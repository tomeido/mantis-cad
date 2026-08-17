//! mantis-chain — the op-log blockchain.
//!
//! Blocks carry ONLY `GraphOp`s (component insert/wire/param ops) — never
//! geometry. Replaying the chain through `Graph::apply` reconstructs the
//! document identically on every peer; meshes are re-derived locally. A
//! building that would be tens of MB as mesh data syncs as a few KB of ops.
//!
//! No clock reads (timestamps passed in), no I/O in this crate. The only
//! sanctioned randomness is `Identity::generate` (OsRng), called at the
//! UI/CLI edge.
//!
//! Hashing/signing (frozen format):
//!   signable = serde_json of {"index":..,"prev_hash":..,"timestamp_ms":..,
//!              "author":..,"author_pk":..,"message":..,"ops":[..]}
//!              (exact field order as the struct declares)
//!   hash     = lowercase hex sha256(signable bytes)
//!   sig      = lowercase hex ed25519 signature over the RAW 32 hash bytes
//! Legacy-v1 genesis: index 0, prev_hash = 64*'0', timestamp 0, author
//! "genesis", author_pk "", message "MantisCAD genesis", ops [], sig "".
//! Scoped-v2 uses the same frozen fields but domain-separates the message as
//! "MantisCAD genesis v2:<64-lowerhex-chain-id>". Both genesis variants are
//! exempt from signature verification; their exact hashes are still checked.

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use mantis_graph::{Graph, GraphOp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// `prev_hash` of the genesis block: 64 ASCII zeros.
const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const GENESIS_AUTHOR: &str = "genesis";
const GENESIS_MESSAGE: &str = "MantisCAD genesis";
const SCOPED_GENESIS_MESSAGE_PREFIX: &str = "MantisCAD genesis v2:";

/// Version of the original, globally fixed genesis format.
pub const LEGACY_CHAIN_FORMAT_VERSION: u32 = 1;

/// Version of the project-scoped, domain-separated genesis format.
pub const SCOPED_CHAIN_FORMAT_VERSION: u32 = 2;

/// Latest chain format produced for newly provisioned projects.
///
/// `Chain::new()` intentionally remains legacy-compatible. New projects
/// should use `Chain::new_scoped` and persist its chain id in their manifest.
pub const CHAIN_FORMAT_VERSION: u32 = SCOPED_CHAIN_FORMAT_VERSION;

// ---------------------------------------------------------------------------
// hex helpers (local, dependency-free)
// ---------------------------------------------------------------------------

/// Lowercase hex encoding of arbitrary bytes.
fn hex_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

/// Strict hex decoding (accepts upper/lower case, rejects everything else,
/// including signs/whitespace that `from_str_radix` would tolerate).
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(out)
}

fn is_lower_hex(s: &str, byte_len: usize) -> bool {
    s.len() == byte_len * 2
        && s.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// ---------------------------------------------------------------------------
// Block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub index: u64,
    pub prev_hash: String,
    pub timestamp_ms: u64,
    pub author: String,
    /// hex ed25519 verifying key (64 hex chars).
    pub author_pk: String,
    pub message: String,
    pub ops: Vec<GraphOp>,
    pub hash: String,
    pub sig: String,
}

/// Deterministic provenance summary for one signing key.
///
/// Names are informational claims signed by the key, not identities bestowed
/// by the server. Keeping every claimed name makes key reuse/renames visible
/// instead of silently collapsing them into the latest display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorActivity {
    pub public_key: String,
    pub names: Vec<String>,
    pub block_count: usize,
    pub operation_count: usize,
    pub first_block: u64,
    pub last_block: u64,
}

/// A validated, compact checkpoint suitable for audit UIs and automation.
///
/// `Chain::audit` only produces this value after verifying every hash, link,
/// signature, and graph operation. The `head_hash` can be stored or anchored
/// externally as a compact commitment to the complete ordered history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainAudit {
    pub format_version: u32,
    /// Present for scoped-v2 chains, absent for legacy-v1 chains.
    pub chain_id: Option<String>,
    pub genesis_hash: String,
    pub head_hash: String,
    pub block_count: usize,
    pub signed_block_count: usize,
    pub operation_count: usize,
    pub byte_size: usize,
    pub authors: Vec<AuthorActivity>,
}

/// The exact byte layout covered by a block hash. serde_json emits struct
/// fields in declaration order, so this pins the frozen field order.
#[derive(Serialize)]
struct Signable<'a> {
    index: u64,
    prev_hash: &'a str,
    timestamp_ms: u64,
    author: &'a str,
    author_pk: &'a str,
    message: &'a str,
    ops: &'a [GraphOp],
}

impl Block {
    /// Canonical bytes covered by hash (see module docs).
    pub fn signable_json(&self) -> String {
        let signable = Signable {
            index: self.index,
            prev_hash: &self.prev_hash,
            timestamp_ms: self.timestamp_ms,
            author: &self.author,
            author_pk: &self.author_pk,
            message: &self.message,
            ops: &self.ops,
        };
        // Cannot fail for this shape (string keys, no fallible Serialize);
        // never panic in library code regardless.
        serde_json::to_string(&signable).unwrap_or_default()
    }

    /// Lowercase hex sha256 of `signable_json` bytes.
    pub fn compute_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.signable_json().as_bytes());
        hex_encode(&hasher.finalize())
    }

    /// Serialized size in bytes of this block's JSON (UI size display).
    pub fn byte_size(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }
}

/// Builds the canonical genesis block (hash filled in, sig empty).
fn legacy_genesis_block() -> Block {
    let mut b = Block {
        index: 0,
        prev_hash: GENESIS_PREV_HASH.to_string(),
        timestamp_ms: 0,
        author: GENESIS_AUTHOR.to_string(),
        author_pk: String::new(),
        message: GENESIS_MESSAGE.to_string(),
        ops: Vec::new(),
        hash: String::new(),
        sig: String::new(),
    };
    b.hash = b.compute_hash();
    b
}

/// Builds a project-scoped genesis without adding fields to the frozen block
/// signable. The domain and chain id live in the existing signed `message`
/// field, so every scoped project receives a distinct genesis hash.
fn scoped_genesis_block(chain_id: &str) -> Block {
    let mut block = Block {
        index: 0,
        prev_hash: GENESIS_PREV_HASH.to_string(),
        timestamp_ms: 0,
        author: GENESIS_AUTHOR.to_string(),
        author_pk: String::new(),
        message: format!("{SCOPED_GENESIS_MESSAGE_PREFIX}{chain_id}"),
        ops: Vec::new(),
        hash: String::new(),
        sig: String::new(),
    };
    block.hash = block.compute_hash();
    block
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// A signing identity (author). Secret key never leaves the client.
pub struct Identity {
    pub name: String,
    signing: ed25519_dalek::SigningKey,
}

impl Identity {
    /// Fresh random identity (OsRng). Only called at the UI/CLI edge.
    pub fn generate(name: &str) -> Identity {
        let signing = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        Identity {
            name: name.to_string(),
            signing,
        }
    }

    /// Restores an identity from a 64-hex-char (32-byte) ed25519 secret key.
    pub fn from_secret_hex(name: &str, secret_hex: &str) -> Result<Identity, ChainError> {
        let bytes = hex_decode(secret_hex).ok_or(ChainError::BadKey)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| ChainError::BadKey)?;
        Ok(Identity {
            name: name.to_string(),
            signing: ed25519_dalek::SigningKey::from_bytes(&arr),
        })
    }

    /// Lowercase hex of the 32-byte secret key.
    pub fn secret_hex(&self) -> String {
        hex_encode(&self.signing.to_bytes())
    }

    /// Lowercase hex of the 32-byte public (verifying) key.
    pub fn public_hex(&self) -> String {
        hex_encode(&self.signing.verifying_key().to_bytes())
    }

    /// Signs the RAW bytes decoded from `hash_hex`, returns the signature as
    /// lowercase hex. Returns an empty string if `hash_hex` is not valid hex
    /// (such a "signature" can never verify).
    pub fn sign_hash_hex(&self, hash_hex: &str) -> String {
        match hex_decode(hash_hex) {
            Some(raw) => hex_encode(&self.signing.sign(&raw).to_bytes()),
            None => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ChainError {
    Empty,
    BadIndex {
        at: usize,
    },
    BadPrevHash {
        at: usize,
    },
    BadHash {
        at: usize,
    },
    BadSignature {
        at: usize,
    },
    /// Replay failed: block index, op index, message.
    BadOps {
        block: usize,
        op: usize,
        msg: String,
    },
    /// Foreign blocks don't chain onto our head.
    Diverged {
        at_index: u64,
    },
    EmptyOps,
    BadKey,
    /// A scoped genesis carries a chain id that is not 32 bytes of canonical
    /// lowercase hexadecimal.
    BadChainId,
    /// An op carries a non-finite float (NaN / ±Infinity). serde_json cannot
    /// represent those (it emits `null`), so they would corrupt the block hash
    /// and make the chain un-reloadable. `block` is the block position the op
    /// occupies (or would occupy during `append`), `op` its offset in the block.
    NonFinite {
        block: usize,
        op: usize,
    },
    Json(String),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::Empty => write!(f, "chain has no genesis block"),
            ChainError::BadIndex { at } => {
                write!(f, "block at position {at} has a non-sequential index")
            }
            ChainError::BadPrevHash { at } => {
                write!(f, "block at position {at} does not link to its predecessor")
            }
            ChainError::BadHash { at } => {
                write!(f, "block at position {at} does not match its content hash")
            }
            ChainError::BadSignature { at } => {
                write!(f, "block at position {at} has an invalid signature")
            }
            ChainError::BadOps { block, op, msg } => {
                write!(f, "block {block} operation {op} cannot be replayed: {msg}")
            }
            ChainError::Diverged { at_index } => {
                write!(f, "history diverges at block index {at_index}")
            }
            ChainError::EmptyOps => write!(f, "cannot append an empty operation list"),
            ChainError::BadKey => write!(f, "invalid Ed25519 public or secret key"),
            ChainError::BadChainId => write!(
                f,
                "invalid chain id (expected 64 lowercase hexadecimal characters)"
            ),
            ChainError::NonFinite { block, op } => write!(
                f,
                "block {block} operation {op} contains a non-finite number"
            ),
            ChainError::Json(msg) => write!(f, "invalid chain JSON: {msg}"),
        }
    }
}
impl std::error::Error for ChainError {}

impl ChainError {
    /// Stable, machine-readable category for API clients.
    ///
    /// Human-facing `Display` text may become more descriptive; automation
    /// should branch on this code instead.
    pub const fn code(&self) -> &'static str {
        match self {
            ChainError::Empty => "empty_chain",
            ChainError::BadIndex { .. } => "bad_index",
            ChainError::BadPrevHash { .. } => "bad_prev_hash",
            ChainError::BadHash { .. } => "bad_hash",
            ChainError::BadSignature { .. } => "bad_signature",
            ChainError::BadOps { .. } => "bad_ops",
            ChainError::Diverged { .. } => "diverged",
            ChainError::EmptyOps => "empty_ops",
            ChainError::BadKey => "bad_key",
            ChainError::BadChainId => "bad_chain_id",
            ChainError::NonFinite { .. } => "non_finite",
            ChainError::Json(_) => "invalid_json",
        }
    }

    /// Block position/index associated with the error, when available.
    pub fn block_index(&self) -> Option<u64> {
        match self {
            ChainError::BadIndex { at }
            | ChainError::BadPrevHash { at }
            | ChainError::BadHash { at }
            | ChainError::BadSignature { at } => u64::try_from(*at).ok(),
            ChainError::BadOps { block, .. } | ChainError::NonFinite { block, .. } => {
                u64::try_from(*block).ok()
            }
            ChainError::Diverged { at_index } => Some(*at_index),
            _ => None,
        }
    }

    /// Operation offset associated with an op-level error, when available.
    pub const fn operation_index(&self) -> Option<usize> {
        match self {
            ChainError::BadOps { op, .. } | ChainError::NonFinite { op, .. } => Some(*op),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Chain
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    pub blocks: Vec<Block>,
}

/// Verifies a block's signature against its own `author_pk` and `hash`.
/// `at` is the position used in error reporting.
fn verify_sig(block: &Block, at: usize, strict_hex: bool) -> Result<(), ChainError> {
    if strict_hex && !is_lower_hex(&block.author_pk, 32) {
        return Err(ChainError::BadKey);
    }
    let pk_bytes = hex_decode(&block.author_pk).ok_or(ChainError::BadKey)?;
    let pk_arr: [u8; 32] = pk_bytes.try_into().map_err(|_| ChainError::BadKey)?;
    let vk = VerifyingKey::from_bytes(&pk_arr).map_err(|_| ChainError::BadKey)?;
    if strict_hex && !is_lower_hex(&block.sig, 64) {
        return Err(ChainError::BadSignature { at });
    }
    let sig_bytes = hex_decode(&block.sig).ok_or(ChainError::BadSignature { at })?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| ChainError::BadSignature { at })?;
    let raw_hash = hex_decode(&block.hash).ok_or(ChainError::BadHash { at })?;
    vk.verify(&raw_hash, &sig)
        .map_err(|_| ChainError::BadSignature { at })
}

/// Rejects a block whose ops carry any non-finite float. Must run BEFORE the
/// hash/signature are trusted, since serde_json turns NaN/±Inf into `null`:
/// the hash would then commit to `null` (colliding NaN, +Inf and -Inf into one
/// hash) and the block could never be reloaded from JSON.
fn verify_finite_ops(block: &Block, at: usize) -> Result<(), ChainError> {
    for (oi, op) in block.ops.iter().enumerate() {
        if !op.is_finite() {
            return Err(ChainError::NonFinite { block: at, op: oi });
        }
    }
    Ok(())
}

/// Structural verification of a non-genesis block against its predecessor:
/// sequential index, prev_hash link, hash recomputes, signature verifies.
fn verify_linked_block(
    block: &Block,
    prev: &Block,
    at: usize,
    strict_hex: bool,
) -> Result<(), ChainError> {
    if prev.index.checked_add(1) != Some(block.index) {
        return Err(ChainError::BadIndex { at });
    }
    if block.prev_hash != prev.hash {
        return Err(ChainError::BadPrevHash { at });
    }
    if block.ops.is_empty() {
        return Err(ChainError::EmptyOps);
    }
    // Reject non-finite ops before trusting the hash: a `null`-serialized float
    // hashes consistently, so BadHash would NOT catch it.
    verify_finite_ops(block, at)?;
    if block.hash != block.compute_hash() {
        return Err(ChainError::BadHash { at });
    }
    verify_sig(block, at, strict_hex)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenesisKind<'a> {
    Legacy,
    Scoped(&'a str),
}

/// Classify and validate the exact canonical genesis shape. Field-specific
/// legacy errors are preserved for compatibility with existing callers.
fn validate_genesis(genesis: &Block) -> Result<GenesisKind<'_>, ChainError> {
    if genesis.index != 0 {
        return Err(ChainError::BadIndex { at: 0 });
    }
    if genesis.prev_hash != GENESIS_PREV_HASH {
        return Err(ChainError::BadPrevHash { at: 0 });
    }
    if !genesis.sig.is_empty() {
        return Err(ChainError::BadSignature { at: 0 });
    }

    let kind = if genesis.message == GENESIS_MESSAGE {
        GenesisKind::Legacy
    } else if let Some(chain_id) = genesis.message.strip_prefix(SCOPED_GENESIS_MESSAGE_PREFIX) {
        if !is_lower_hex(chain_id, 32) {
            return Err(ChainError::BadChainId);
        }
        GenesisKind::Scoped(chain_id)
    } else {
        return Err(ChainError::BadHash { at: 0 });
    };

    let canonical = match kind {
        GenesisKind::Legacy => legacy_genesis_block(),
        GenesisKind::Scoped(chain_id) => scoped_genesis_block(chain_id),
    };
    if *genesis != canonical {
        return Err(ChainError::BadHash { at: 0 });
    }
    Ok(kind)
}

impl Chain {
    /// New legacy-v1 chain containing the globally fixed genesis block.
    ///
    /// Kept deliberately stable so existing local documents and fixtures do
    /// not silently change identity. New collaborative projects should call
    /// [`Chain::new_scoped`].
    pub fn new() -> Chain {
        Chain {
            blocks: vec![legacy_genesis_block()],
        }
    }

    /// New v2 chain with a project-scoped genesis.
    ///
    /// `chain_id` is exactly 32 random bytes represented as 64 lowercase hex
    /// characters. Randomness is supplied by the caller so this crate remains
    /// deterministic and usable in replay/test environments.
    pub fn new_scoped(chain_id: &str) -> Result<Chain, ChainError> {
        if !is_lower_hex(chain_id, 32) {
            return Err(ChainError::BadChainId);
        }
        Ok(Chain {
            blocks: vec![scoped_genesis_block(chain_id)],
        })
    }

    /// The validated format version encoded by this chain's genesis.
    pub fn format_version(&self) -> Result<u32, ChainError> {
        let genesis = self.blocks.first().ok_or(ChainError::Empty)?;
        match validate_genesis(genesis)? {
            GenesisKind::Legacy => Ok(LEGACY_CHAIN_FORMAT_VERSION),
            GenesisKind::Scoped(_) => Ok(SCOPED_CHAIN_FORMAT_VERSION),
        }
    }

    /// The validated scoped chain id, or `None` for a legacy-v1 chain.
    pub fn chain_id(&self) -> Result<Option<&str>, ChainError> {
        let genesis = self.blocks.first().ok_or(ChainError::Empty)?;
        match validate_genesis(genesis)? {
            GenesisKind::Legacy => Ok(None),
            GenesisKind::Scoped(chain_id) => Ok(Some(chain_id)),
        }
    }

    pub fn head(&self) -> &Block {
        self.blocks.last().expect("chain never empty")
    }
    pub fn len(&self) -> usize {
        self.blocks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
    pub fn total_ops(&self) -> usize {
        self.blocks.iter().map(|b| b.ops.len()).sum()
    }
    /// Whole-chain JSON size in bytes (the number that stays tiny).
    pub fn byte_size(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }

    /// Seal `ops` into a new signed block on the head. Rejects empty or
    /// unreplayable ops and refuses to build on an invalid local history.
    ///
    /// All checks happen before mutation, so an error leaves the chain exactly
    /// as it was. This matters for non-UI callers (including agents), which may
    /// construct `GraphOp`s directly instead of going through a live `Graph`.
    pub fn append(
        &mut self,
        ops: Vec<GraphOp>,
        message: &str,
        identity: &Identity,
        timestamp_ms: u64,
    ) -> Result<&Block, ChainError> {
        if ops.is_empty() {
            return Err(ChainError::EmptyOps);
        }
        let new_block = self.blocks.len();
        for (oi, op) in ops.iter().enumerate() {
            if !op.is_finite() {
                return Err(ChainError::NonFinite {
                    block: new_block,
                    op: oi,
                });
            }
        }
        let mut graph = self.validate_and_replay()?;
        for (oi, op) in ops.iter().enumerate() {
            graph.apply(op).map_err(|e| ChainError::BadOps {
                block: new_block,
                op: oi,
                msg: e.to_string(),
            })?;
        }
        let head = self.head();
        let mut block = Block {
            index: head.index + 1,
            prev_hash: head.hash.clone(),
            timestamp_ms,
            author: identity.name.clone(),
            author_pk: identity.public_hex(),
            message: message.to_string(),
            ops,
            hash: String::new(),
            sig: String::new(),
        };
        block.hash = block.compute_hash();
        block.sig = identity.sign_hash_hex(&block.hash);
        self.blocks.push(block);
        Ok(self.head())
    }

    /// Full validation: genesis exact, indices sequential, prev_hash links,
    /// hashes recompute, signatures verify (non-genesis), and the whole op
    /// log replays cleanly through `Graph::apply`.
    pub fn validate(&self) -> Result<(), ChainError> {
        self.validate_and_replay().map(|_| ())
    }

    /// Validate the complete history once and return its materialized graph.
    /// Keeping these operations together prevents append/extend from checking
    /// semantics while accidentally building on broken hashes or signatures.
    fn validate_and_replay(&self) -> Result<Graph, ChainError> {
        let genesis = self.blocks.first().ok_or(ChainError::Empty)?;
        let strict_hex = matches!(validate_genesis(genesis)?, GenesisKind::Scoped(_));
        for at in 1..self.blocks.len() {
            verify_linked_block(&self.blocks[at], &self.blocks[at - 1], at, strict_hex)?;
        }
        // Replay the whole op log; any failure surfaces as BadOps.
        self.replay(None)
    }

    /// Verify a candidate extension against the trusted current head without
    /// re-validating or replaying the already accepted prefix.
    ///
    /// This is intended for servers that validate a persisted chain once at
    /// startup and retain its materialized [`Graph`]. It checks every new
    /// block's index/link, finite values, hash, and signature, and checks
    /// already-known blocks by `(index, hash)`. It deliberately does not scan
    /// the existing history or apply graph semantics. Callers must preserve
    /// the invariant that `self` was fully validated before it became trusted.
    pub fn verify_extension_crypto(&self, blocks: &[Block]) -> Result<usize, ChainError> {
        let strict_hex = self.format_version()? == SCOPED_CHAIN_FORMAT_VERSION;
        let mut appended: Vec<&Block> = Vec::new();
        for block in blocks {
            let head = appended.last().copied().unwrap_or_else(|| self.head());
            if block.index <= head.index {
                let pos = usize::try_from(block.index).map_err(|_| ChainError::Diverged {
                    at_index: block.index,
                })?;
                let known = if pos < self.blocks.len() {
                    &self.blocks[pos]
                } else {
                    let appended_pos = pos.saturating_sub(self.blocks.len());
                    appended
                        .get(appended_pos)
                        .copied()
                        .ok_or(ChainError::Diverged {
                            at_index: block.index,
                        })?
                };
                if known.index != block.index || known.hash != block.hash {
                    return Err(ChainError::Diverged {
                        at_index: block.index,
                    });
                }
                continue;
            }
            if head.index.checked_add(1) != Some(block.index) || block.prev_hash != head.hash {
                return Err(ChainError::Diverged {
                    at_index: block.index,
                });
            }
            let at = usize::try_from(block.index)
                .map_err(|_| ChainError::BadIndex { at: usize::MAX })?;
            verify_linked_block(block, head, at, strict_hex)?;
            appended.push(block);
        }
        Ok(appended.len())
    }

    /// Atomically apply a cryptographically verified extension to a trusted
    /// materialized graph without replaying the accepted prefix.
    ///
    /// `materialized` must be the graph produced by this exact chain. Servers
    /// should first call [`Chain::verify_extension_crypto`], then rate-limit
    /// the proven signing keys, and only then call this method. Verification is
    /// repeated here so the method remains safe as a standalone atomic update.
    pub fn try_extend_trusted(
        &mut self,
        materialized: &mut Graph,
        blocks: &[Block],
    ) -> Result<usize, ChainError> {
        let appended = self.verify_extension_crypto(blocks)?;
        let original_len = self.blocks.len();
        let mut next_index =
            u64::try_from(original_len).map_err(|_| ChainError::BadIndex { at: usize::MAX })?;
        let mut candidate_graph = materialized.clone();
        let mut tail = Vec::with_capacity(appended);
        for block in blocks {
            let Ok(index) = usize::try_from(block.index) else {
                return Err(ChainError::BadIndex { at: usize::MAX });
            };
            if block.index < next_index {
                continue;
            }
            debug_assert_eq!(block.index, next_index);
            for (op, graph_op) in block.ops.iter().enumerate() {
                candidate_graph
                    .apply(graph_op)
                    .map_err(|error| ChainError::BadOps {
                        block: index,
                        op,
                        msg: error.to_string(),
                    })?;
            }
            tail.push(block.clone());
            next_index = next_index
                .checked_add(1)
                .ok_or(ChainError::BadIndex { at: usize::MAX })?;
        }
        self.blocks.extend(tail);
        *materialized = candidate_graph;
        Ok(appended)
    }

    /// Rebuild the document by replaying blocks 0..=upto (None = all).
    /// This is THE way a peer materializes the model. `upto` beyond the last
    /// block is clamped to the full chain.
    pub fn replay(&self, upto: Option<usize>) -> Result<Graph, ChainError> {
        let mut graph = Graph::new();
        let count = match upto {
            Some(u) => u.saturating_add(1).min(self.blocks.len()),
            None => self.blocks.len(),
        };
        for (bi, block) in self.blocks.iter().enumerate().take(count) {
            for (oi, op) in block.ops.iter().enumerate() {
                graph.apply(op).map_err(|e| ChainError::BadOps {
                    block: bi,
                    op: oi,
                    msg: e.to_string(),
                })?;
            }
        }
        Ok(graph)
    }

    /// Fast-forward with foreign blocks (already-known blocks skipped by
    /// (index,hash); each new block fully verified + replay-checked).
    /// Returns number of blocks appended. Diverged if prev_hash mismatch.
    ///
    /// All-or-nothing: on any error `self` is left unmodified.
    pub fn try_extend(&mut self, blocks: &[Block]) -> Result<usize, ChainError> {
        let mut graph = self.validate_and_replay()?;
        self.try_extend_trusted(&mut graph, blocks)
    }

    /// Validate the full chain and return a deterministic provenance/checkpoint
    /// summary. Persisting just `head_hash` is enough to later detect any
    /// rewrite when the full chain is audited again.
    pub fn audit(&self) -> Result<ChainAudit, ChainError> {
        self.validate_and_replay()?;

        #[derive(Default)]
        struct Activity {
            names: BTreeSet<String>,
            block_count: usize,
            operation_count: usize,
            first_block: Option<u64>,
            last_block: u64,
        }

        let mut by_key: BTreeMap<String, Activity> = BTreeMap::new();
        for block in self.blocks.iter().skip(1) {
            let activity = by_key.entry(block.author_pk.clone()).or_default();
            activity.names.insert(block.author.clone());
            activity.block_count += 1;
            activity.operation_count += block.ops.len();
            activity.first_block.get_or_insert(block.index);
            activity.last_block = block.index;
        }
        let authors = by_key
            .into_iter()
            .map(|(public_key, activity)| AuthorActivity {
                public_key,
                names: activity.names.into_iter().collect(),
                block_count: activity.block_count,
                operation_count: activity.operation_count,
                first_block: activity.first_block.unwrap_or(0),
                last_block: activity.last_block,
            })
            .collect();

        Ok(ChainAudit {
            format_version: self.format_version()?,
            chain_id: self.chain_id()?.map(str::to_owned),
            genesis_hash: self.blocks[0].hash.clone(),
            head_hash: self.head().hash.clone(),
            block_count: self.len(),
            signed_block_count: self.len().saturating_sub(1),
            operation_count: self.total_ops(),
            byte_size: self.byte_size(),
            authors,
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parses AND validates.
    pub fn from_json(s: &str) -> Result<Chain, ChainError> {
        let chain: Chain = serde_json::from_str(s).map_err(|e| ChainError::Json(e.to_string()))?;
        chain.validate()?;
        Ok(chain)
    }
}

impl Default for Chain {
    fn default() -> Self {
        Chain::new()
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_graph::{GraphOp, NodeId, ParamValue};

    /// sha256 of the canonical genesis signable JSON, precomputed externally.
    /// Pins the frozen hash format across refactors.
    const GENESIS_HASH: &str = "6647ae8b4509faf6518cdfc11e2f778c856e3c0fe82a557e745f675a7cab0bee";

    fn ident(name: &str) -> Identity {
        Identity::generate(name)
    }

    fn ops_block_a() -> Vec<GraphOp> {
        vec![
            GraphOp::AddNode {
                id: NodeId(1),
                type_name: "Number".into(),
                pos: (10.0, 20.0),
            },
            GraphOp::AddNode {
                id: NodeId(2),
                type_name: "Extrude".into(),
                pos: (100.0, 20.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "value".into(),
                value: ParamValue::Number(2.5),
            },
        ]
    }

    fn ops_block_b() -> Vec<GraphOp> {
        vec![GraphOp::Connect {
            from: (NodeId(1), 0),
            to: (NodeId(2), 0),
        }]
    }

    /// Hand-seals a structurally perfect block (valid hash + sig) with
    /// arbitrary ops — the tool of choice for adversarial tests.
    fn seal(prev: &Block, ops: Vec<GraphOp>, id: &Identity, ts: u64) -> Block {
        let mut b = Block {
            index: prev.index + 1,
            prev_hash: prev.hash.clone(),
            timestamp_ms: ts,
            author: id.name.clone(),
            author_pk: id.public_hex(),
            message: "hand-sealed".into(),
            ops,
            hash: String::new(),
            sig: String::new(),
        };
        b.hash = b.compute_hash();
        b.sig = id.sign_hash_hex(&b.hash);
        b
    }

    /// A 2-block chain (genesis + a + b) plus the identity that signed it.
    fn sample_chain() -> (Chain, Identity) {
        let id = ident("alice");
        let mut chain = Chain::new();
        chain.append(ops_block_a(), "add nodes", &id, 1000).unwrap();
        chain.append(ops_block_b(), "wire", &id, 2000).unwrap();
        (chain, id)
    }

    // -- genesis ------------------------------------------------------------

    #[test]
    fn genesis_is_stable_and_exact() {
        let a = Chain::new();
        let b = Chain::new();
        assert_eq!(a, b);
        let g = &a.blocks[0];
        assert_eq!(g.index, 0);
        assert_eq!(g.prev_hash, "0".repeat(64));
        assert_eq!(g.timestamp_ms, 0);
        assert_eq!(g.author, "genesis");
        assert_eq!(g.author_pk, "");
        assert_eq!(g.message, "MantisCAD genesis");
        assert!(g.ops.is_empty());
        assert_eq!(g.sig, "");
        assert_eq!(g.hash, GENESIS_HASH);
        a.validate().unwrap();
        assert_eq!(a.format_version(), Ok(LEGACY_CHAIN_FORMAT_VERSION));
        assert_eq!(a.chain_id(), Ok(None));
    }

    #[test]
    fn scoped_genesis_is_unique_domain_separated_and_stable() {
        let chain_id_a = "01".repeat(32);
        let chain_id_b = "02".repeat(32);
        let a = Chain::new_scoped(&chain_id_a).unwrap();
        let same = Chain::new_scoped(&chain_id_a).unwrap();
        let b = Chain::new_scoped(&chain_id_b).unwrap();

        assert_eq!(a, same);
        assert_ne!(a.head().hash, b.head().hash);
        assert_ne!(a.head().hash, Chain::new().head().hash);
        assert_eq!(
            a.head().message,
            format!("MantisCAD genesis v2:{chain_id_a}")
        );
        assert_eq!(a.format_version(), Ok(SCOPED_CHAIN_FORMAT_VERSION));
        assert_eq!(a.chain_id(), Ok(Some(chain_id_a.as_str())));
        a.validate().unwrap();
    }

    #[test]
    fn scoped_chain_id_is_strict_lowercase_hex() {
        for invalid in [
            "",
            "01",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64),
            &"gg".repeat(32),
        ] {
            assert_eq!(Chain::new_scoped(invalid), Err(ChainError::BadChainId));
        }
    }

    #[test]
    fn scoped_chain_rejects_noncanonical_key_and_signature_hex() {
        let id = ident("alice");
        let mut chain = Chain::new_scoped(&"ab".repeat(32)).unwrap();
        chain.append(ops_block_a(), "add", &id, 1).unwrap();

        let mut uppercase_key = chain.clone();
        uppercase_key.blocks[1].author_pk = uppercase_key.blocks[1].author_pk.to_uppercase();
        uppercase_key.blocks[1].hash = uppercase_key.blocks[1].compute_hash();
        uppercase_key.blocks[1].sig = id.sign_hash_hex(&uppercase_key.blocks[1].hash);
        assert_eq!(uppercase_key.validate(), Err(ChainError::BadKey));

        let mut uppercase_sig = chain;
        uppercase_sig.blocks[1].sig = uppercase_sig.blocks[1].sig.to_uppercase();
        assert_eq!(
            uppercase_sig.validate(),
            Err(ChainError::BadSignature { at: 1 })
        );
    }

    #[test]
    fn genesis_signable_json_frozen_format() {
        let g = &Chain::new().blocks[0];
        assert_eq!(
            g.signable_json(),
            format!(
                "{{\"index\":0,\"prev_hash\":\"{}\",\"timestamp_ms\":0,\
                 \"author\":\"genesis\",\"author_pk\":\"\",\
                 \"message\":\"MantisCAD genesis\",\"ops\":[]}}",
                "0".repeat(64)
            )
        );
    }

    #[test]
    fn tampered_genesis_rejected() {
        let (mut chain, _) = sample_chain();
        chain.blocks[0].message = "EVIL genesis".into();
        chain.blocks[0].hash = chain.blocks[0].compute_hash();
        // hash recomputes, but it is not THE canonical genesis
        assert_eq!(chain.validate(), Err(ChainError::BadHash { at: 0 }));

        let (mut chain, _) = sample_chain();
        chain.blocks[0].sig = "ab".repeat(32);
        assert_eq!(chain.validate(), Err(ChainError::BadSignature { at: 0 }));

        let (mut chain, _) = sample_chain();
        chain.blocks[0].prev_hash = "1".repeat(64);
        assert_eq!(chain.validate(), Err(ChainError::BadPrevHash { at: 0 }));
    }

    #[test]
    fn empty_chain_is_invalid() {
        let chain = Chain { blocks: vec![] };
        assert_eq!(chain.validate(), Err(ChainError::Empty));
    }

    // -- append + validate ---------------------------------------------------

    #[test]
    fn append_then_validate_ok() {
        let (chain, _) = sample_chain();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.total_ops(), 4);
        chain.validate().unwrap();
        // links are correct
        assert_eq!(chain.blocks[1].prev_hash, chain.blocks[0].hash);
        assert_eq!(chain.blocks[2].prev_hash, chain.blocks[1].hash);
        assert_eq!(chain.blocks[2].index, 2);
    }

    #[test]
    fn append_empty_ops_rejected() {
        let id = ident("alice");
        let mut chain = Chain::new();
        assert_eq!(
            chain.append(vec![], "nothing", &id, 1),
            Err(ChainError::EmptyOps)
        );
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn handcrafted_signed_empty_blocks_are_rejected_everywhere() {
        let id = ident("spammer");
        let empty = seal(Chain::new().head(), vec![], &id, 1);

        let mut loaded = Chain::new();
        loaded.blocks.push(empty.clone());
        assert_eq!(loaded.validate(), Err(ChainError::EmptyOps));

        let mut extended = Chain::new();
        assert_eq!(
            extended.try_extend(std::slice::from_ref(&empty)),
            Err(ChainError::EmptyOps)
        );
        assert_eq!(extended, Chain::new());
    }

    #[test]
    fn append_rejects_unreplayable_ops_atomically() {
        let id = ident("agent");
        let mut chain = Chain::new();
        let before = chain.clone();
        let result = chain.append(
            vec![GraphOp::SetParam {
                id: NodeId(0xdead),
                key: "value".into(),
                value: ParamValue::Number(2.0),
            }],
            "invalid direct op",
            &id,
            1,
        );
        assert!(matches!(
            result,
            Err(ChainError::BadOps {
                block: 1,
                op: 0,
                ..
            })
        ));
        assert_eq!(chain, before);
    }

    #[test]
    fn append_refuses_to_build_on_tampered_history() {
        let (mut chain, id) = sample_chain();
        chain.blocks[1].message = "rewritten history".into();
        let len = chain.len();
        assert_eq!(
            chain
                .append(
                    vec![GraphOp::MoveNode {
                        id: NodeId(1),
                        pos: (1.0, 1.0),
                    }],
                    "must not seal",
                    &id,
                    3,
                )
                .err(),
            Some(ChainError::BadHash { at: 1 })
        );
        assert_eq!(chain.len(), len);
    }

    // -- tamper detection ------------------------------------------------------

    #[test]
    fn tampered_op_breaks_hash() {
        let (mut chain, _) = sample_chain();
        chain.blocks[1].ops[2] = GraphOp::SetParam {
            id: NodeId(1),
            key: "value".into(),
            value: ParamValue::Number(999.0),
        };
        assert_eq!(chain.validate(), Err(ChainError::BadHash { at: 1 }));
    }

    #[test]
    fn tampered_metadata_breaks_hash() {
        let (mut chain, _) = sample_chain();
        chain.blocks[2].message = "innocent-looking".into();
        assert_eq!(chain.validate(), Err(ChainError::BadHash { at: 2 }));
    }

    #[test]
    fn rehash_without_resign_breaks_signature() {
        let (mut chain, _) = sample_chain();
        chain.blocks[1].ops[2] = GraphOp::SetParam {
            id: NodeId(1),
            key: "value".into(),
            value: ParamValue::Number(999.0),
        };
        chain.blocks[1].hash = chain.blocks[1].compute_hash();
        // old sig no longer covers the new hash, and block 2's prev link is
        // also broken — signature check happens per-block first
        assert_eq!(chain.validate(), Err(ChainError::BadSignature { at: 1 }));
    }

    #[test]
    fn forged_author_pk_rejected() {
        // Block claims alice's key but is signed by mallory.
        let alice = ident("alice");
        let mallory = ident("mallory");
        let mut chain = Chain::new();
        chain.append(ops_block_a(), "legit", &alice, 1).unwrap();
        let head = chain.head().clone();
        let mut forged = Block {
            index: head.index + 1,
            prev_hash: head.hash.clone(),
            timestamp_ms: 2,
            author: "alice".into(),
            author_pk: alice.public_hex(),
            message: "totally alice".into(),
            ops: ops_block_b(),
            hash: String::new(),
            sig: String::new(),
        };
        forged.hash = forged.compute_hash();
        forged.sig = mallory.sign_hash_hex(&forged.hash);
        chain.blocks.push(forged);
        assert_eq!(chain.validate(), Err(ChainError::BadSignature { at: 2 }));
    }

    #[test]
    fn wrong_prev_hash_rejected() {
        let (mut chain, id) = sample_chain();
        let mut fake_prev = chain.head().clone();
        fake_prev.hash = "1".repeat(64); // sealed against a hash that isn't our head's
        let block = seal(&fake_prev, ops_block_b(), &id, 3);
        chain.blocks.push(block);
        assert_eq!(chain.validate(), Err(ChainError::BadPrevHash { at: 3 }));
    }

    #[test]
    fn non_sequential_index_rejected() {
        let (mut chain, id) = sample_chain();
        let mut fake_prev = chain.head().clone();
        fake_prev.index += 1; // skip an index; keep the real head hash
        let block = seal(&fake_prev, ops_block_b(), &id, 3);
        chain.blocks.push(block);
        assert_eq!(chain.validate(), Err(ChainError::BadIndex { at: 3 }));
    }

    #[test]
    fn bad_author_pk_hex_is_bad_key() {
        let (mut chain, id) = sample_chain();
        let head = chain.head().clone();
        let mut b = Block {
            index: head.index + 1,
            prev_hash: head.hash.clone(),
            timestamp_ms: 3,
            author: "eve".into(),
            author_pk: "zz".repeat(32), // not hex
            message: "bad key".into(),
            ops: ops_block_b(),
            hash: String::new(),
            sig: String::new(),
        };
        b.hash = b.compute_hash();
        b.sig = id.sign_hash_hex(&b.hash);
        chain.blocks.push(b);
        assert_eq!(chain.validate(), Err(ChainError::BadKey));
    }

    #[test]
    fn garbage_sig_hex_is_bad_signature() {
        let (mut chain, _) = sample_chain();
        chain.blocks[2].sig = "nothex".into();
        assert_eq!(chain.validate(), Err(ChainError::BadSignature { at: 2 }));
        chain.blocks[2].sig = "ab".repeat(10); // valid hex, wrong length
        assert_eq!(chain.validate(), Err(ChainError::BadSignature { at: 2 }));
    }

    // -- BadOps: structurally valid block, semantically invalid ops -----------

    #[test]
    fn smuggled_invalid_ops_rejected_by_validate() {
        let (mut chain, id) = sample_chain();
        // Connect to a node that never existed — hash+sig are perfectly valid.
        let evil_ops = vec![GraphOp::Connect {
            from: (NodeId(0xdead), 0),
            to: (NodeId(0xbeef), 0),
        }];
        let block = seal(chain.head(), evil_ops, &id, 5);
        chain.blocks.push(block);
        match chain.validate() {
            Err(ChainError::BadOps { block, op, .. }) => {
                assert_eq!(block, 3);
                assert_eq!(op, 0);
            }
            other => panic!("expected BadOps, got {other:?}"),
        }
    }

    // -- replay ---------------------------------------------------------------

    #[test]
    fn replay_all_matches_direct_apply() {
        let (chain, _) = sample_chain();
        let replayed = chain.replay(None).unwrap();
        let mut direct = Graph::new();
        for op in ops_block_a().iter().chain(ops_block_b().iter()) {
            direct.apply(op).unwrap();
        }
        assert_eq!(replayed, direct);
        assert_eq!(replayed.nodes.len(), 2);
        assert_eq!(replayed.edges.len(), 1);
    }

    #[test]
    fn replay_prefixes() {
        let (chain, _) = sample_chain();
        // genesis only -> empty graph
        assert_eq!(chain.replay(Some(0)).unwrap(), Graph::new());
        // through block 1 -> nodes but no wire
        let g1 = chain.replay(Some(1)).unwrap();
        assert_eq!(g1.nodes.len(), 2);
        assert!(g1.edges.is_empty());
        // upto beyond end clamps to all
        assert_eq!(chain.replay(Some(99)).unwrap(), chain.replay(None).unwrap());
    }

    // -- try_extend -------------------------------------------------------------

    #[test]
    fn try_extend_fast_forwards_and_skips_known() {
        let (full, _) = sample_chain();
        let mut behind = Chain {
            blocks: full.blocks[..2].to_vec(), // genesis + block 1
        };
        // feed the WHOLE foreign chain: known blocks skipped, tail appended
        let n = behind.try_extend(&full.blocks).unwrap();
        assert_eq!(n, 1);
        assert_eq!(behind, full);
        behind.validate().unwrap();

        // extending with the same blocks again is a no-op
        let n = behind.try_extend(&full.blocks).unwrap();
        assert_eq!(n, 0);
        assert_eq!(behind, full);
    }

    #[test]
    fn try_extend_multiple_new_blocks() {
        let (full, id) = sample_chain();
        let mut extended = full.clone();
        extended
            .append(
                vec![GraphOp::MoveNode {
                    id: NodeId(1),
                    pos: (5.0, 5.0),
                }],
                "nudge",
                &id,
                9,
            )
            .unwrap();
        let mut fresh = Chain::new();
        let n = fresh.try_extend(&extended.blocks).unwrap();
        assert_eq!(n, 3);
        assert_eq!(fresh, extended);
    }

    #[test]
    fn try_extend_detects_fork() {
        let (chain, id) = sample_chain();
        // fork: same parent as our block 2, different content
        let mut fork = Chain {
            blocks: chain.blocks[..2].to_vec(),
        };
        fork.append(vec![GraphOp::RemoveNode { id: NodeId(2) }], "fork!", &id, 7)
            .unwrap();
        let mut ours = chain.clone();
        assert_eq!(
            ours.try_extend(&fork.blocks),
            Err(ChainError::Diverged { at_index: 2 })
        );
        assert_eq!(ours, chain); // untouched on error
    }

    #[test]
    fn try_extend_rejects_gap_and_bad_link() {
        let (chain, id) = sample_chain();

        // gap: foreign block skips an index
        let mut fake_prev = chain.head().clone();
        fake_prev.index += 1;
        let gap_block = seal(&fake_prev, ops_block_b(), &id, 8);
        let mut ours = chain.clone();
        assert_eq!(
            ours.try_extend(std::slice::from_ref(&gap_block)),
            Err(ChainError::Diverged {
                at_index: gap_block.index
            })
        );

        // right index, wrong prev_hash
        let mut fake_prev = chain.head().clone();
        fake_prev.hash = "2".repeat(64);
        let bad_link = seal(&fake_prev, ops_block_b(), &id, 8);
        let mut ours = chain.clone();
        assert_eq!(
            ours.try_extend(std::slice::from_ref(&bad_link)),
            Err(ChainError::Diverged { at_index: 3 })
        );
        assert_eq!(ours, chain);
    }

    #[test]
    fn try_extend_rejects_valid_block_with_bad_ops() {
        let (chain, id) = sample_chain();
        // hash+sig valid, but ops reference an unknown node
        let evil = seal(
            chain.head(),
            vec![GraphOp::SetParam {
                id: NodeId(0xdead),
                key: "x".into(),
                value: ParamValue::Bool(true),
            }],
            &id,
            9,
        );
        let mut ours = chain.clone();
        match ours.try_extend(std::slice::from_ref(&evil)) {
            Err(ChainError::BadOps { block, op, .. }) => {
                assert_eq!(block, 3);
                assert_eq!(op, 0);
            }
            other => panic!("expected BadOps, got {other:?}"),
        }
        assert_eq!(ours, chain); // nothing committed
    }

    #[test]
    fn try_extend_rejects_tampered_foreign_block() {
        let (chain, id) = sample_chain();
        let mut block = seal(chain.head(), ops_block_b(), &id, 9);
        block.message = "tampered".into();
        let mut ours = chain.clone();
        assert_eq!(
            ours.try_extend(std::slice::from_ref(&block)),
            Err(ChainError::BadHash { at: 3 })
        );

        // re-hashed but signed by nobody we can verify against the claim
        let mallory = ident("mallory");
        let mut forged = seal(chain.head(), ops_block_b(), &id, 9);
        forged.sig = mallory.sign_hash_hex(&forged.hash);
        let mut ours = chain.clone();
        assert_eq!(
            ours.try_extend(std::slice::from_ref(&forged)),
            Err(ChainError::BadSignature { at: 3 })
        );
    }

    #[test]
    fn try_extend_refuses_tampered_local_history_even_for_noop() {
        let (mut chain, _) = sample_chain();
        chain.blocks[1].message = "rewritten history".into();
        assert_eq!(chain.try_extend(&[]), Err(ChainError::BadHash { at: 1 }));
    }

    #[test]
    fn trusted_extension_checks_only_the_new_tail_and_applies_incrementally() {
        let (clean, identity) = sample_chain();
        let mut extended = clean.clone();
        extended
            .append(
                vec![GraphOp::MoveNode {
                    id: NodeId(1),
                    pos: (42.0, 24.0),
                }],
                "trusted incremental update",
                &identity,
                10,
            )
            .unwrap();

        let mut trusted = clean.clone();
        let mut materialized = trusted.replay(None).unwrap();
        // A server is required to protect this trusted-prefix invariant. The
        // deliberate corruption proves the extension API does not rescan it.
        trusted.blocks[1].hash = "ff".repeat(32);
        assert!(trusted.validate().is_err());

        let tail = &extended.blocks[clean.len()..];
        let duplicate_tail = vec![tail[0].clone(), tail[0].clone()];
        assert_eq!(trusted.verify_extension_crypto(&duplicate_tail).unwrap(), 1);
        assert_eq!(
            trusted
                .try_extend_trusted(&mut materialized, &duplicate_tail)
                .unwrap(),
            1
        );
        assert_eq!(trusted.len(), extended.len());
        assert_eq!(materialized, extended.replay(None).unwrap());
    }

    // -- json -------------------------------------------------------------------

    #[test]
    fn from_json_round_trip() {
        let (chain, _) = sample_chain();
        let json = chain.to_json();
        let parsed = Chain::from_json(&json).unwrap();
        assert_eq!(parsed, chain);
        assert!(chain.byte_size() > 0);
        assert!(chain.head().byte_size() > 0);
    }

    #[test]
    fn from_json_tampered_fails() {
        let (chain, _) = sample_chain();
        // flip a message deep inside the JSON -> hash mismatch on validate
        let json = chain.to_json().replace("add nodes", "ADD NODES");
        assert_eq!(Chain::from_json(&json), Err(ChainError::BadHash { at: 1 }));
        // syntactically broken JSON -> Json error
        match Chain::from_json("{not json") {
            Err(ChainError::Json(_)) => {}
            other => panic!("expected Json error, got {other:?}"),
        }
    }

    #[test]
    fn audit_is_validated_deterministic_provenance_checkpoint() {
        let (mut chain, mut id) = sample_chain();
        id.name = "alice-renamed".into();
        chain
            .append(
                vec![GraphOp::MoveNode {
                    id: NodeId(1),
                    pos: (7.0, 8.0),
                }],
                "rename is visible by key",
                &id,
                3000,
            )
            .unwrap();

        let audit = chain.audit().unwrap();
        assert_eq!(audit.format_version, LEGACY_CHAIN_FORMAT_VERSION);
        assert_eq!(audit.chain_id, None);
        assert_eq!(audit.genesis_hash, GENESIS_HASH);
        assert_eq!(audit.head_hash, chain.head().hash);
        assert_eq!(audit.block_count, 4);
        assert_eq!(audit.signed_block_count, 3);
        assert_eq!(audit.operation_count, 5);
        assert_eq!(audit.byte_size, chain.byte_size());
        assert_eq!(audit.authors.len(), 1);
        assert_eq!(audit.authors[0].public_key, id.public_hex());
        assert_eq!(audit.authors[0].names, ["alice", "alice-renamed"]);
        assert_eq!(audit.authors[0].block_count, 3);
        assert_eq!(audit.authors[0].operation_count, 5);
        assert_eq!(audit.authors[0].first_block, 1);
        assert_eq!(audit.authors[0].last_block, 3);

        let json = serde_json::to_string(&audit).unwrap();
        assert_eq!(serde_json::from_str::<ChainAudit>(&json).unwrap(), audit);

        chain.blocks[2].sig = "00".repeat(64);
        assert_eq!(chain.audit(), Err(ChainError::BadSignature { at: 2 }));
    }

    #[test]
    fn chain_error_exposes_stable_machine_context() {
        let error = ChainError::BadOps {
            block: 7,
            op: 3,
            msg: "unknown node".into(),
        };
        assert_eq!(error.code(), "bad_ops");
        assert_eq!(error.block_index(), Some(7));
        assert_eq!(error.operation_index(), Some(3));
        assert!(error.to_string().contains("unknown node"));
        assert_eq!(ChainError::BadKey.code(), "bad_key");
        assert_eq!(ChainError::BadKey.block_index(), None);
    }

    // -- identity ------------------------------------------------------------------

    #[test]
    fn identity_secret_hex_round_trip() {
        let a = ident("alice");
        let restored = Identity::from_secret_hex("alice2", &a.secret_hex()).unwrap();
        assert_eq!(a.public_hex(), restored.public_hex());
        assert_eq!(a.secret_hex(), restored.secret_hex());
        // ed25519 is deterministic: same key + same hash -> same signature
        let hash = Chain::new().blocks[0].hash.clone();
        assert_eq!(a.sign_hash_hex(&hash), restored.sign_hash_hex(&hash));
        assert_eq!(a.public_hex().len(), 64);
        assert_eq!(a.secret_hex().len(), 64);
    }

    #[test]
    fn identity_bad_secret_hex_rejected() {
        assert_eq!(
            Identity::from_secret_hex("x", "nothex").err(),
            Some(ChainError::BadKey)
        );
        assert_eq!(
            Identity::from_secret_hex("x", &"ab".repeat(16)).err(),
            Some(ChainError::BadKey) // 32 hex chars = 16 bytes, wrong length
        );
        // valid 64-char hex works
        Identity::from_secret_hex("x", &"ab".repeat(32)).unwrap();
    }

    #[test]
    fn two_generated_identities_differ() {
        let a = ident("a");
        let b = ident("b");
        assert_ne!(a.public_hex(), b.public_hex());
    }

    #[test]
    fn sign_hash_hex_invalid_input_yields_empty() {
        let a = ident("a");
        assert_eq!(a.sign_hash_hex("not-hex!"), "");
    }

    // -- hex helpers ------------------------------------------------------------

    #[test]
    fn hex_helpers() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x1a]), "00ff1a");
        assert_eq!(hex_decode("00ff1a"), Some(vec![0x00, 0xff, 0x1a]));
        assert_eq!(hex_decode("00FF1A"), Some(vec![0x00, 0xff, 0x1a]));
        assert_eq!(hex_decode("0"), None); // odd length
        assert_eq!(hex_decode("+f"), None); // from_str_radix would accept this
        assert_eq!(hex_decode("g0"), None);
        assert_eq!(hex_decode(""), Some(vec![]));
    }

    // -- signature over RAW hash bytes (format pin) --------------------------------

    #[test]
    fn signature_covers_raw_hash_bytes() {
        let (chain, _) = sample_chain();
        let b = &chain.blocks[1];
        let pk: [u8; 32] = hex_decode(&b.author_pk).unwrap().try_into().unwrap();
        let vk = VerifyingKey::from_bytes(&pk).unwrap();
        let sig = Signature::from_slice(&hex_decode(&b.sig).unwrap()).unwrap();
        let raw = hex_decode(&b.hash).unwrap();
        assert_eq!(raw.len(), 32);
        vk.verify(&raw, &sig).unwrap(); // raw bytes verify...
        assert!(vk.verify(b.hash.as_bytes(), &sig).is_err()); // ...hex string does not
    }
}
