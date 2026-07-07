//! Document state: chain + working graph + pending ops + evaluator.
//!
//! EVERY structural edit goes through [`Document::apply_op`]; interactive
//! gestures (slider drags, node drags) go through the live/gesture helpers so
//! the pending op list contains exactly one coalesced op per gesture.

use mantis_chain::{Block, Chain, ChainError, Identity};
use mantis_graph::{
    EvalOutput, Evaluator, Graph, GraphError, GraphOp, NodeId, ParamValue, Registry,
};
use std::collections::BTreeMap;

/// An in-flight interactive gesture whose intermediate states must NOT be
/// recorded as ops.
enum Gesture {
    /// A slider/dragvalue/textedit gesture on one (node, param key).
    Param {
        id: NodeId,
        key: String,
        start: Option<ParamValue>,
        last: ParamValue,
    },
    /// Dragging one or more nodes; start positions keyed by node.
    Move { start: BTreeMap<NodeId, (f32, f32)> },
}

/// Result of merging remote blocks into the local document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeReport {
    /// Blocks appended to the local chain.
    pub appended: usize,
    /// Pending ops dropped because they no longer applied after the merge.
    pub dropped: usize,
}

/// The whole mutable state of one MantisCAD session.
pub struct Document {
    /// The op-log. Source of truth; starts as `Chain::new()` (genesis only).
    pub chain: Chain,
    /// Working graph = replay of chain + pending ops (+ live gesture state).
    pub graph: Graph,
    /// Uncommitted ops, in order. `chain.replay() + pending` == `graph`
    /// (modulo an in-flight gesture, which is finalized before commit).
    pub pending: Vec<GraphOp>,
    pub evaluator: Evaluator,
    pub registry: Registry,
    pub identity: Identity,
    /// Output of the most recent evaluation of the *displayed* graph.
    pub last_eval: EvalOutput,
    /// Time travel: `Some(i)` = read-only view of the chain replayed through
    /// block `i`. `None` = head (editable).
    view_index: Option<usize>,
    view_graph: Option<Graph>,
    gesture: Option<Gesture>,
    /// Set whenever displayed geometry may have changed; the viewport drains
    /// it to rebuild GPU batches.
    scene_dirty: bool,
}

impl Document {
    pub fn new(identity: Identity) -> Document {
        Document {
            chain: Chain::new(),
            graph: Graph::new(),
            pending: Vec::new(),
            evaluator: Evaluator::new(),
            registry: Registry::standard(),
            identity,
            last_eval: EvalOutput::default(),
            view_index: None,
            view_graph: None,
            gesture: None,
            scene_dirty: true,
        }
    }

    // ------------------------------------------------------------------
    // display / read-only view
    // ------------------------------------------------------------------

    /// The graph currently shown in the UI (time-travel view or working).
    pub fn display_graph(&self) -> &Graph {
        self.view_graph.as_ref().unwrap_or(&self.graph)
    }

    /// True when editing is allowed (not time traveling).
    pub fn editable(&self) -> bool {
        self.view_graph.is_none()
    }

    /// Block index currently viewed (head index when at head).
    pub fn viewed_block(&self) -> usize {
        self.view_index.unwrap_or(self.chain.len() - 1)
    }

    pub fn is_time_traveling(&self) -> bool {
        self.view_index.is_some()
    }

    /// Enter/leave time travel. `Some(i)` with `i >= head` returns to head.
    pub fn set_view(&mut self, idx: Option<usize>) -> Result<(), String> {
        let head = self.chain.len() - 1;
        let target = match idx {
            Some(i) if i < head => Some(i),
            _ => None,
        };
        if target == self.view_index {
            return Ok(());
        }
        match target {
            Some(i) => {
                let g = self
                    .chain
                    .replay(Some(i))
                    .map_err(|e| format!("replay failed: {e}"))?;
                self.view_index = Some(i);
                self.view_graph = Some(g);
            }
            None => {
                self.view_index = None;
                self.view_graph = None;
            }
        }
        self.evaluator.invalidate_all();
        self.scene_dirty = true;
        Ok(())
    }

    // ------------------------------------------------------------------
    // evaluation / scene dirtiness
    // ------------------------------------------------------------------

    /// Evaluate the displayed graph (cached — cheap when nothing changed).
    pub fn evaluate(&mut self) {
        let graph = self.view_graph.as_ref().unwrap_or(&self.graph);
        self.last_eval = self.evaluator.evaluate(graph, &self.registry);
    }

    /// True once if displayed geometry may have changed since the last call.
    pub fn take_scene_dirty(&mut self) -> bool {
        std::mem::take(&mut self.scene_dirty)
    }

    pub fn mark_scene_dirty(&mut self) {
        self.scene_dirty = true;
    }

    // ------------------------------------------------------------------
    // the single mutation path
    // ------------------------------------------------------------------

    /// Apply an op to the working graph and record it as pending.
    /// Rejected while time traveling.
    pub fn apply_op(&mut self, op: GraphOp) -> Result<(), String> {
        if !self.editable() {
            return Err("read-only: viewing chain history".into());
        }
        self.graph.apply(&op).map_err(|e| e.to_string())?;
        self.invalidate_for(&op);
        self.pending.push(op);
        Ok(())
    }

    /// Apply an op to the working graph WITHOUT recording it (intermediate
    /// gesture frames). The caller is responsible for recording one final op.
    fn apply_live(&mut self, op: &GraphOp) -> Result<(), GraphError> {
        self.graph.apply(op)?;
        self.invalidate_for(op);
        Ok(())
    }

    fn invalidate_for(&mut self, op: &GraphOp) {
        match op {
            GraphOp::MoveNode { .. } => {} // layout only: no eval, no scene
            GraphOp::RemoveNode { .. } => {
                // Downstream info is gone after removal: safe blanket refresh.
                self.evaluator.invalidate_all();
                self.scene_dirty = true;
            }
            GraphOp::AddNode { id, .. } | GraphOp::SetParam { id, .. } => {
                self.evaluator.invalidate(&self.graph, *id);
                self.scene_dirty = true;
            }
            GraphOp::Connect { to, .. } | GraphOp::Disconnect { to, .. } => {
                self.evaluator.invalidate(&self.graph, to.0);
                self.scene_dirty = true;
            }
        }
    }

    // ------------------------------------------------------------------
    // gesture coalescing: params
    // ------------------------------------------------------------------

    /// One frame of a param drag: applies the value live; records nothing.
    /// The first call of a gesture snapshots the pre-drag value.
    pub fn param_drag(&mut self, id: NodeId, key: &str, value: ParamValue) {
        if !self.editable() {
            return;
        }
        // A different (node, key) target ends the previous gesture first.
        let matches = matches!(
            &self.gesture,
            Some(Gesture::Param { id: gid, key: gkey, .. }) if *gid == id && gkey == key
        );
        if !matches {
            self.end_gesture();
            let start = self
                .graph
                .nodes
                .get(&id)
                .and_then(|n| n.params.get(key))
                .cloned();
            self.gesture = Some(Gesture::Param {
                id,
                key: key.to_string(),
                start,
                last: value.clone(),
            });
        } else if let Some(Gesture::Param { last, .. }) = &mut self.gesture {
            *last = value.clone();
        }
        let _ = self.apply_live(&GraphOp::SetParam {
            id,
            key: key.to_string(),
            value,
        });
    }

    /// Finish the active param gesture: exactly one `SetParam` op is recorded
    /// (none if the value ended where it started, or the node vanished).
    pub fn end_param_drag(&mut self) {
        if let Some(Gesture::Param { id, key, start, last }) = self.gesture.take() {
            if !self.graph.nodes.contains_key(&id) {
                return; // node deleted mid-gesture: nothing valid to record
            }
            if start.as_ref() == Some(&last) {
                return; // no net change
            }
            // The graph already holds `last` (applied live) — just record it.
            self.pending.push(GraphOp::SetParam { id, key, value: last });
        }
    }

    /// Convenience: a one-shot param change (checkbox toggle, text commit).
    pub fn set_param(&mut self, id: NodeId, key: &str, value: ParamValue) -> Result<(), String> {
        self.apply_op(GraphOp::SetParam {
            id,
            key: key.to_string(),
            value,
        })
    }

    // ------------------------------------------------------------------
    // gesture coalescing: node moves
    // ------------------------------------------------------------------

    /// Start a node-drag gesture over `ids`, snapshotting start positions.
    pub fn begin_move(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        if !self.editable() {
            return;
        }
        self.end_gesture();
        let mut start = BTreeMap::new();
        for id in ids {
            if let Some(n) = self.graph.nodes.get(&id) {
                start.insert(id, n.pos);
            }
        }
        if !start.is_empty() {
            self.gesture = Some(Gesture::Move { start });
        }
    }

    /// One frame of a node drag (live position update, nothing recorded).
    pub fn move_live(&mut self, id: NodeId, pos: (f32, f32)) {
        if !self.editable() {
            return;
        }
        if matches!(&self.gesture, Some(Gesture::Move { start }) if start.contains_key(&id)) {
            let _ = self.apply_live(&GraphOp::MoveNode { id, pos });
        }
    }

    /// Finish the node-drag gesture: one `MoveNode` per node that moved.
    pub fn end_move(&mut self) {
        if let Some(Gesture::Move { start }) = self.gesture.take() {
            for (id, start_pos) in start {
                let Some(node) = self.graph.nodes.get(&id) else { continue };
                let pos = node.pos;
                if pos != start_pos {
                    self.pending.push(GraphOp::MoveNode { id, pos });
                }
            }
        }
    }

    /// Finish whatever gesture is active (called before commits and merges).
    pub fn end_gesture(&mut self) {
        match self.gesture {
            Some(Gesture::Param { .. }) => self.end_param_drag(),
            Some(Gesture::Move { .. }) => self.end_move(),
            None => {}
        }
    }

    /// True while a param/move gesture is in flight.
    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    // ------------------------------------------------------------------
    // commit / merge
    // ------------------------------------------------------------------

    /// Seal pending ops into a signed block. Returns the op count sealed.
    pub fn commit(&mut self, message: &str, now_ms: u64) -> Result<usize, String> {
        self.end_gesture();
        if !self.editable() {
            return Err("read-only: viewing chain history".into());
        }
        if self.pending.is_empty() {
            return Err("nothing to commit".into());
        }
        let ops = self.pending.clone();
        let count = ops.len();
        let msg = if message.trim().is_empty() {
            "(no message)"
        } else {
            message
        };
        self.chain
            .append(ops, msg, &self.identity, now_ms)
            .map_err(|e| format!("commit failed: {e}"))?;
        self.pending.clear();
        Ok(count)
    }

    /// Merge blocks pulled from the server: extend the chain, rebuild the
    /// working graph from a full replay, then re-apply pending ops one by
    /// one, dropping any that no longer apply.
    pub fn merge_remote(&mut self, blocks: &[Block]) -> Result<MergeReport, ChainError> {
        self.end_gesture();
        let appended = self.chain.try_extend(blocks)?;
        if appended == 0 {
            return Ok(MergeReport { appended: 0, dropped: 0 });
        }
        let mut graph = self.chain.replay(None)?;
        let mut kept = Vec::with_capacity(self.pending.len());
        let mut dropped = 0usize;
        for op in self.pending.drain(..) {
            match graph.apply(&op) {
                Ok(()) => kept.push(op),
                Err(_) => dropped += 1,
            }
        }
        self.pending = kept;
        self.graph = graph;
        // Leave any time-travel view in place (indices are still valid: the
        // chain only grew), but refresh everything.
        self.evaluator.invalidate_all();
        self.scene_dirty = true;
        Ok(MergeReport { appended, dropped })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::now_ms;

    fn doc(name: &str) -> Document {
        Document::new(Identity::generate(name))
    }

    fn nid(n: u128) -> NodeId {
        NodeId(n)
    }

    fn add(doc: &mut Document, id: u128, ty: &str) {
        doc.apply_op(GraphOp::AddNode {
            id: nid(id),
            type_name: ty.into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
    }

    #[test]
    fn apply_op_records_pending_and_mutates_graph() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        assert_eq!(d.pending.len(), 1);
        assert!(d.graph.nodes.contains_key(&nid(1)));
        // A bad op is rejected and NOT recorded.
        let err = d.apply_op(GraphOp::RemoveNode { id: nid(99) });
        assert!(err.is_err());
        assert_eq!(d.pending.len(), 1);
    }

    #[test]
    fn slider_drag_coalesces_to_one_op() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        let before = d.pending.len();
        // Simulate a drag: many frames, one release.
        for v in [1.0, 2.0, 3.5, 7.25] {
            d.param_drag(nid(1), "value", ParamValue::Number(v));
        }
        assert_eq!(d.pending.len(), before, "no ops recorded mid-drag");
        // Live value is visible in the graph during the drag.
        assert_eq!(
            d.graph.nodes[&nid(1)].params.get("value"),
            Some(&ParamValue::Number(7.25))
        );
        d.end_param_drag();
        assert_eq!(d.pending.len(), before + 1, "exactly one op per gesture");
        assert_eq!(
            d.pending.last(),
            Some(&GraphOp::SetParam {
                id: nid(1),
                key: "value".into(),
                value: ParamValue::Number(7.25)
            })
        );
        // Releasing again is a no-op.
        d.end_param_drag();
        assert_eq!(d.pending.len(), before + 1);
    }

    #[test]
    fn slider_drag_back_to_start_records_nothing() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.set_param(nid(1), "value", ParamValue::Number(4.0)).unwrap();
        let before = d.pending.len();
        d.param_drag(nid(1), "value", ParamValue::Number(9.0));
        d.param_drag(nid(1), "value", ParamValue::Number(4.0));
        d.end_param_drag();
        assert_eq!(d.pending.len(), before, "round trip drag records no op");
    }

    #[test]
    fn switching_param_target_ends_previous_gesture() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        add(&mut d, 2, "number_slider");
        let before = d.pending.len();
        d.param_drag(nid(1), "value", ParamValue::Number(1.0));
        d.param_drag(nid(2), "value", ParamValue::Number(2.0)); // implicit end of #1
        d.end_param_drag();
        assert_eq!(d.pending.len(), before + 2);
    }

    #[test]
    fn node_move_coalesces_one_op_per_node() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        add(&mut d, 2, "panel");
        let before = d.pending.len();
        d.begin_move([nid(1), nid(2)]);
        for i in 1..=5 {
            let f = i as f32;
            d.move_live(nid(1), (f, f));
            d.move_live(nid(2), (f * 2.0, f));
        }
        assert_eq!(d.pending.len(), before);
        d.end_move();
        assert_eq!(d.pending.len(), before + 2);
        assert_eq!(d.graph.nodes[&nid(1)].pos, (5.0, 5.0));
        assert_eq!(d.graph.nodes[&nid(2)].pos, (10.0, 5.0));
        // Unmoved gesture records nothing.
        d.begin_move([nid(1)]);
        d.end_move();
        assert_eq!(d.pending.len(), before + 2);
    }

    #[test]
    fn pending_replays_cleanly_on_committed_graph() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.param_drag(nid(1), "value", ParamValue::Number(2.0));
        d.param_drag(nid(1), "value", ParamValue::Number(8.0));
        d.end_param_drag();
        d.begin_move([nid(1)]);
        d.move_live(nid(1), (50.0, 60.0));
        d.end_move();
        // Invariant: committed replay + pending == working graph.
        let mut g = d.chain.replay(None).unwrap();
        g.apply_all(&d.pending).unwrap();
        assert_eq!(g, d.graph);
    }

    #[test]
    fn commit_seals_and_clears_pending() {
        let mut d = doc("alice");
        add(&mut d, 1, "number_slider");
        d.set_param(nid(1), "value", ParamValue::Number(3.0)).unwrap();
        let n = d.commit("first", now_ms()).unwrap();
        assert_eq!(n, 2);
        assert!(d.pending.is_empty());
        assert_eq!(d.chain.len(), 2);
        d.chain.validate().unwrap();
        assert_eq!(d.chain.replay(None).unwrap(), d.graph);
        // Empty commit rejected.
        assert!(d.commit("empty", now_ms()).is_err());
    }

    #[test]
    fn time_travel_is_read_only_and_reversible() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.commit("one", 1).unwrap();
        add(&mut d, 2, "panel");
        d.commit("two", 2).unwrap();
        add(&mut d, 3, "pi_const"); // pending on top of head

        d.set_view(Some(1)).unwrap();
        assert!(d.is_time_traveling());
        assert!(!d.editable());
        assert_eq!(d.display_graph().nodes.len(), 1);
        assert!(d.apply_op(GraphOp::RemoveNode { id: nid(1) }).is_err());
        // Gestures are ignored while read-only.
        d.param_drag(nid(1), "value", ParamValue::Number(9.0));
        d.end_param_drag();
        assert!(d.pending.len() == 1); // only the pi_const AddNode

        d.set_view(None).unwrap();
        assert!(d.editable());
        assert_eq!(d.display_graph().nodes.len(), 3);
        // Viewing the head index is the same as None.
        d.set_view(Some(d.chain.len() - 1)).unwrap();
        assert!(!d.is_time_traveling());
    }

    #[test]
    fn merge_remote_reapplies_pending_and_drops_conflicts() {
        // Alice commits a slider (id 1) and pushes.
        let mut alice = doc("alice");
        add(&mut alice, 1, "number_slider");
        alice.commit("slider", 1).unwrap();

        // Bob (fresh chain) has pending work: a panel (id 2, fine) and an
        // AddNode with the SAME id 1 (conflicts after merge) plus a param op
        // on it (also dropped once its AddNode is gone... but note id 1 DOES
        // exist post-merge from alice's block, so the SetParam survives).
        let mut bob = doc("bob");
        add(&mut bob, 2, "panel");
        add(&mut bob, 1, "number_slider"); // will collide with alice's node
        bob.set_param(nid(1), "value", ParamValue::Number(4.0)).unwrap();

        let report = bob.merge_remote(&alice.chain.blocks).unwrap();
        assert_eq!(report.appended, 1);
        assert_eq!(report.dropped, 1, "duplicate AddNode dropped");
        assert_eq!(bob.chain.len(), 2);
        assert_eq!(bob.pending.len(), 2, "panel add + set_param kept");
        // Working graph = replay + surviving pending.
        assert!(bob.graph.nodes.contains_key(&nid(1)));
        assert!(bob.graph.nodes.contains_key(&nid(2)));
        assert_eq!(
            bob.graph.nodes[&nid(1)].params.get("value"),
            Some(&ParamValue::Number(4.0))
        );
        // Invariant still holds.
        let mut g = bob.chain.replay(None).unwrap();
        g.apply_all(&bob.pending).unwrap();
        assert_eq!(g, bob.graph);
    }

    #[test]
    fn merge_remote_no_new_blocks_is_noop() {
        let mut a = doc("a");
        add(&mut a, 1, "panel");
        let blocks = a.chain.blocks.clone(); // genesis only
        let report = a.merge_remote(&blocks).unwrap();
        assert_eq!(report, MergeReport { appended: 0, dropped: 0 });
        assert_eq!(a.pending.len(), 1);
    }

    #[test]
    fn eval_runs_on_display_graph() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.set_param(nid(1), "value", ParamValue::Number(7.0)).unwrap();
        d.evaluate();
        assert_eq!(
            d.last_eval.outputs[&nid(1)][0],
            mantis_graph::Value::Number(7.0)
        );
        d.commit("c", 1).unwrap();
        // At view 0 (genesis) nothing exists.
        d.set_view(Some(0)).unwrap();
        d.evaluate();
        assert!(d.last_eval.outputs.is_empty());
        d.set_view(None).unwrap();
        d.evaluate();
        assert_eq!(d.last_eval.outputs.len(), 1);
    }

    #[test]
    fn deleting_dragged_node_mid_gesture_records_nothing() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.begin_move([nid(1)]);
        d.move_live(nid(1), (9.0, 9.0));
        // Gesture interrupted by deletion (via direct pending edit path).
        let n = d.pending.len();
        d.graph.apply(&GraphOp::RemoveNode { id: nid(1) }).unwrap();
        d.end_move();
        assert_eq!(d.pending.len(), n, "no MoveNode for a removed node");
    }
}
