//! Regression: non-finite floats (NaN / ±Infinity) must never enter the chain.
//!
//! serde_json serializes non-finite floats as `null`, which (a) makes every
//! block hash collide NaN with ±Infinity and (b) makes the chain impossible to
//! reload from JSON. The chain must reject such ops at every entry point.

use mantis_chain::{Block, Chain, ChainError, Identity};
use mantis_graph::{GraphOp, NodeId, ParamValue};

#[test]
fn append_rejects_nonfinite_param() {
    let mut chain = Chain::new();
    let id = Identity::generate("mallory");
    let r = chain.append(
        vec![GraphOp::SetParam {
            id: NodeId(1),
            key: "value".into(),
            value: ParamValue::Number(f64::NAN),
        }],
        "poison",
        &id,
        1,
    );
    assert!(matches!(r, Err(ChainError::NonFinite { .. })), "got {r:?}");
    assert_eq!(chain.len(), 1, "poison op must not have been sealed");
}

#[test]
fn append_rejects_nonfinite_pos() {
    let mut chain = Chain::new();
    let id = Identity::generate("mallory");
    for bad in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        let r = chain.append(
            vec![GraphOp::AddNode {
                id: NodeId(1),
                type_name: "circle".into(),
                pos: (bad, 0.0),
            }],
            "poison",
            &id,
            1,
        );
        assert!(matches!(r, Err(ChainError::NonFinite { .. })), "pos {bad} -> {r:?}");
    }
}

#[test]
fn try_extend_rejects_handcrafted_nonfinite_block() {
    // A malicious peer hand-builds a block with a NaN param, correctly hashed
    // and signed over the `null`-serialized bytes. It must still be refused —
    // before its (self-consistent) hash/sig are ever trusted.
    let id = Identity::generate("mallory");
    let mut block = Block {
        index: 1,
        prev_hash: Chain::new().head().hash.clone(),
        timestamp_ms: 1,
        author: id.name.clone(),
        author_pk: id.public_hex(),
        message: "poison".into(),
        ops: vec![GraphOp::SetParam {
            id: NodeId(1),
            key: "value".into(),
            value: ParamValue::Number(f64::NAN),
        }],
        hash: String::new(),
        sig: String::new(),
    };
    block.hash = block.compute_hash();
    block.sig = id.sign_hash_hex(&block.hash);

    let mut chain = Chain::new();
    let r = chain.try_extend(&[block]);
    assert!(matches!(r, Err(ChainError::NonFinite { .. })), "got {r:?}");
    assert_eq!(chain.len(), 1);
}

#[test]
fn clean_chain_still_round_trips() {
    let mut chain = Chain::new();
    let id = Identity::generate("alice");
    chain
        .append(
            vec![GraphOp::AddNode {
                id: NodeId(1),
                type_name: "circle".into(),
                pos: (10.0, 20.0),
            }],
            "ok",
            &id,
            1,
        )
        .unwrap();
    assert!(chain.validate().is_ok());
    let json = chain.to_json();
    assert!(Chain::from_json(&json).is_ok());
}
