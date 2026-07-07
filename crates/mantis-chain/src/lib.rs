//! mantis-chain — the op-log blockchain.
//!
//! Blocks carry ONLY `GraphOp`s (component insert/wire/param ops) — never
//! geometry. Replaying the chain through `Graph::apply` reconstructs the
//! document identically on every peer; meshes are re-derived locally. A
//! building that would be tens of MB as mesh data syncs as a few KB of ops.
//!
//! CONTRACT STUB — implement bodies, keep public signatures.
//! No clock reads (timestamps passed in), no I/O in this crate.
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

use mantis_graph::{Graph, GraphOp};
use serde::{Deserialize, Serialize};

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

impl Block {
    /// Canonical bytes covered by hash (see module docs).
    pub fn signable_json(&self) -> String {
        todo!("chain-agent")
    }
    pub fn compute_hash(&self) -> String {
        todo!("chain-agent")
    }
    /// Serialized size in bytes of this block's JSON (UI size display).
    pub fn byte_size(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }
}

/// A signing identity (author). Secret key never leaves the client.
pub struct Identity {
    pub name: String,
    signing: ed25519_dalek::SigningKey,
}

impl Identity {
    /// Fresh random identity (OsRng). Only called at the UI/CLI edge.
    pub fn generate(name: &str) -> Identity {
        let _ = name;
        todo!("chain-agent")
    }
    pub fn from_secret_hex(name: &str, secret_hex: &str) -> Result<Identity, ChainError> {
        let _ = (name, secret_hex);
        todo!("chain-agent")
    }
    pub fn secret_hex(&self) -> String {
        todo!("chain-agent")
    }
    pub fn public_hex(&self) -> String {
        todo!("chain-agent")
    }
    pub fn sign_hash_hex(&self, hash_hex: &str) -> String {
        let _ = hash_hex;
        todo!("chain-agent")
    }
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    pub blocks: Vec<Block>,
}

impl Chain {
    /// New chain containing only the genesis block.
    pub fn new() -> Chain {
        todo!("chain-agent")
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
        let _ = (ops, message, identity, timestamp_ms);
        todo!("chain-agent")
    }

    /// Full validation: genesis exact, indices sequential, prev_hash links,
    /// hashes recompute, signatures verify (non-genesis), and the whole op
    /// log replays cleanly through `Graph::apply`.
    pub fn validate(&self) -> Result<(), ChainError> {
        todo!("chain-agent")
    }

    /// Rebuild the document by replaying blocks 0..=upto (None = all).
    /// This is THE way a peer materializes the model.
    pub fn replay(&self, upto: Option<usize>) -> Result<Graph, ChainError> {
        let _ = upto;
        todo!("chain-agent")
    }

    /// Fast-forward with foreign blocks (already-known blocks skipped by
    /// (index,hash); each new block fully verified + replay-checked).
    /// Returns number of blocks appended. Diverged if prev_hash mismatch.
    pub fn try_extend(&mut self, blocks: &[Block]) -> Result<usize, ChainError> {
        let _ = blocks;
        todo!("chain-agent")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
    /// Parses AND validates.
    pub fn from_json(s: &str) -> Result<Chain, ChainError> {
        let _ = s;
        todo!("chain-agent")
    }
}

impl Default for Chain {
    fn default() -> Self {
        Chain::new()
    }
}
