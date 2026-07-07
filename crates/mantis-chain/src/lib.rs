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
//! Genesis: index 0, prev_hash = 64*'0', timestamp 0, author "genesis",
//! author_pk "", message "MantisCAD genesis", ops [], sig "" (exempt from
//! signature verification; hash still verified).

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use mantis_graph::{Graph, GraphOp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `prev_hash` of the genesis block: 64 ASCII zeros.
const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const GENESIS_AUTHOR: &str = "genesis";
const GENESIS_MESSAGE: &str = "MantisCAD genesis";

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
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(out)
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
fn genesis_block() -> Block {
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
    BadIndex { at: usize },
    BadPrevHash { at: usize },
    BadHash { at: usize },
    BadSignature { at: usize },
    /// Replay failed: block index, op index, message.
    BadOps { block: usize, op: usize, msg: String },
    /// Foreign blocks don't chain onto our head.
    Diverged { at_index: u64 },
    EmptyOps,
    BadKey,
    Json(String),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ChainError {}

// ---------------------------------------------------------------------------
// Chain
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    pub blocks: Vec<Block>,
}

/// Verifies a block's signature against its own `author_pk` and `hash`.
/// `at` is the position used in error reporting.
fn verify_sig(block: &Block, at: usize) -> Result<(), ChainError> {
    let pk_bytes = hex_decode(&block.author_pk).ok_or(ChainError::BadKey)?;
    let pk_arr: [u8; 32] = pk_bytes.try_into().map_err(|_| ChainError::BadKey)?;
    let vk = VerifyingKey::from_bytes(&pk_arr).map_err(|_| ChainError::BadKey)?;
    let sig_bytes = hex_decode(&block.sig).ok_or(ChainError::BadSignature { at })?;
    let sig =
        Signature::from_slice(&sig_bytes).map_err(|_| ChainError::BadSignature { at })?;
    let raw_hash = hex_decode(&block.hash).ok_or(ChainError::BadHash { at })?;
    vk.verify(&raw_hash, &sig)
        .map_err(|_| ChainError::BadSignature { at })
}

/// Structural verification of a non-genesis block against its predecessor:
/// sequential index, prev_hash link, hash recomputes, signature verifies.
fn verify_linked_block(block: &Block, prev: &Block, at: usize) -> Result<(), ChainError> {
    if block.index != prev.index + 1 {
        return Err(ChainError::BadIndex { at });
    }
    if block.prev_hash != prev.hash {
        return Err(ChainError::BadPrevHash { at });
    }
    if block.hash != block.compute_hash() {
        return Err(ChainError::BadHash { at });
    }
    verify_sig(block, at)
}

impl Chain {
    /// New chain containing only the genesis block.
    pub fn new() -> Chain {
        Chain {
            blocks: vec![genesis_block()],
        }
    }

    pub fn head(&self) -> &Block {
        self.blocks.last().expect("chain never empty")
    }
    pub fn len(&self) -> usize {
        self.blocks.len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn total_ops(&self) -> usize {
        self.blocks.iter().map(|b| b.ops.len()).sum()
    }
    /// Whole-chain JSON size in bytes (the number that stays tiny).
    pub fn byte_size(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }

    /// Seal `ops` into a new signed block on the head. Rejects empty ops.
    /// Ops are NOT re-validated against a replayed graph here — callers keep
    /// the invariant that pending ops were applied to a graph built from this
    /// chain (validate() / replay() are the safety net).
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
        let genesis = self.blocks.first().ok_or(ChainError::Empty)?;
        let canon = genesis_block();
        if *genesis != canon {
            return Err(if genesis.index != 0 {
                ChainError::BadIndex { at: 0 }
            } else if genesis.prev_hash != canon.prev_hash {
                ChainError::BadPrevHash { at: 0 }
            } else if !genesis.sig.is_empty() {
                ChainError::BadSignature { at: 0 }
            } else {
                // any other field mismatch necessarily changes (or breaks)
                // the hash relative to the canonical genesis
                ChainError::BadHash { at: 0 }
            });
        }
        for at in 1..self.blocks.len() {
            verify_linked_block(&self.blocks[at], &self.blocks[at - 1], at)?;
        }
        // Replay the whole op log; any failure surfaces as BadOps.
        self.replay(None)?;
        Ok(())
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
        // Materialize our current document once; candidates replay on top.
        let mut graph = self.replay(None)?;
        let mut appended: Vec<Block> = Vec::new();
        for block in blocks {
            let (head_index, head_hash) = {
                let head = appended.last().unwrap_or_else(|| self.head());
                (head.index, head.hash.clone())
            };
            if block.index <= head_index {
                // Already-known territory: must match our block exactly by
                // (index, hash), otherwise the peer has forked history.
                let pos = block.index as usize;
                let known = if pos < self.blocks.len() {
                    &self.blocks[pos]
                } else {
                    &appended[pos - self.blocks.len()]
                };
                if known.index != block.index || known.hash != block.hash {
                    return Err(ChainError::Diverged {
                        at_index: block.index,
                    });
                }
                continue;
            }
            if block.index != head_index + 1 || block.prev_hash != head_hash {
                return Err(ChainError::Diverged {
                    at_index: block.index,
                });
            }
            let at = block.index as usize;
            if block.hash != block.compute_hash() {
                return Err(ChainError::BadHash { at });
            }
            verify_sig(block, at)?;
            // Trial replay before committing: a hash/sig-valid block whose
            // ops don't apply is rejected and nothing is mutated.
            let mut trial = graph.clone();
            for (oi, op) in block.ops.iter().enumerate() {
                trial.apply(op).map_err(|e| ChainError::BadOps {
                    block: at,
                    op: oi,
                    msg: e.to_string(),
                })?;
            }
            graph = trial;
            appended.push(block.clone());
        }
        let count = appended.len();
        self.blocks.extend(appended);
        Ok(count)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parses AND validates.
    pub fn from_json(s: &str) -> Result<Chain, ChainError> {
        let chain: Chain =
            serde_json::from_str(s).map_err(|e| ChainError::Json(e.to_string()))?;
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
    const GENESIS_HASH: &str =
        "6647ae8b4509faf6518cdfc11e2f778c856e3c0fe82a557e745f675a7cab0bee";

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
        fork.append(
            vec![GraphOp::RemoveNode { id: NodeId(2) }],
            "fork!",
            &id,
            7,
        )
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
        assert_eq!(
            Chain::from_json(&json),
            Err(ChainError::BadHash { at: 1 })
        );
        // syntactically broken JSON -> Json error
        match Chain::from_json("{not json") {
            Err(ChainError::Json(_)) => {}
            other => panic!("expected Json error, got {other:?}"),
        }
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
