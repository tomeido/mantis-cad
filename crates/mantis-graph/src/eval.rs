//! Deterministic dataflow evaluation with dirty-tracking cache.
//! CONTRACT STUB — implement bodies, keep signatures.
//!
//! Semantics (graph-agent must implement exactly):
//! - Nodes evaluate in `Graph::topo_order()`.
//! - Unknown `type_name` -> error entry "unknown component: X".
//! - Input gathering, per input port i of the component:
//!     wired  -> the upstream node's output value at that port
//!               (upstream errored -> this node errors "upstream error");
//!     unwired-> port default if Some, else Value::Null.
//! - Longest-list matching: if any Access::Item port holds a Value::List, the
//!   component runs N times (N = max len over those ports; empty list -> node
//!   evaluates to empty lists). Run i takes list[min(i, len-1)] (last repeats),
//!   non-list values broadcast. Each output port then becomes a List of the
//!   per-run values. Access::List ports receive lists whole (scalars wrapped
//!   in a 1-list) and do NOT trigger mapping.
//! - Type check AFTER unwrap: a scalar on an Item port that fails
//!   `kind_matches` -> node error naming the port. Null on a defaultless port
//!   -> error "input <name> missing".
//! - Component eval Err(msg) -> node error; outputs absent for errored nodes.
//! - Cache: a node re-evaluates iff marked dirty (invalidate/invalidate_all,
//!   including downstream propagation) or its inputs changed; otherwise cached
//!   outputs are reused. Structure changes (connect/disconnect/remove) must
//!   dirty affected nodes + downstream (the app calls `invalidate`).

use crate::component::Registry;
use crate::graph::{Graph, NodeId};
use crate::value::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvalOutput {
    /// Per node: one Value per output port. Errored nodes absent.
    pub outputs: BTreeMap<NodeId, Vec<Value>>,
    pub errors: BTreeMap<NodeId, String>,
}

/// Reusable evaluator holding the cache across frames.
#[derive(Default)]
pub struct Evaluator {
    cache: BTreeMap<NodeId, Vec<Value>>,
    dirty_all: bool,
}

impl Evaluator {
    pub fn new() -> Evaluator {
        Evaluator { cache: BTreeMap::new(), dirty_all: true }
    }
    /// Mark `id` and everything downstream dirty.
    pub fn invalidate(&mut self, graph: &Graph, id: NodeId) {
        let _ = (graph, id);
        todo!("graph-agent")
    }
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
        self.dirty_all = true;
    }
    /// Evaluate the whole graph (cached nodes reused).
    pub fn evaluate(&mut self, graph: &Graph, reg: &Registry) -> EvalOutput {
        let _ = (graph, reg);
        todo!("graph-agent")
    }
}
