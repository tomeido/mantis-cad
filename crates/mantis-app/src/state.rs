//! Document state: chain + working graph + pending ops + evaluator.
//!
//! EVERY structural edit goes through [`Document::apply_op`]; interactive
//! gestures (slider drags, node drags) go through the live/gesture helpers so
//! the pending op list contains exactly one coalesced op per gesture.

use mantis_chain::{Block, Chain, ChainError, Identity};
use mantis_graph::{
    EvalOutput, Evaluator, Graph, GraphError, GraphOp, NodeId, ParamValue, Registry,
};
use mantis_protocol::ChainId;
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
    /// Pending operations that could not be replayed after a remote update.
    /// Kept separately so a conflict is recoverable instead of silently lost.
    pub recovery_ops: Vec<GraphOp>,
    /// Pending-op snapshots before each user edit. History deliberately stops
    /// at a commit: immutable chain blocks are never rewritten by Undo.
    undo_history: Vec<Vec<GraphOp>>,
    /// Pending-op snapshots removed by Undo and available to Redo.
    redo_history: Vec<Vec<GraphOp>>,
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
    #[cfg(test)]
    pub fn new(identity: Identity) -> Document {
        Self::with_chain(identity, Chain::new())
    }

    /// Create a new personal workspace with an isolated v2 genesis. Legacy
    /// snapshots still restore through `restore`; only newly authored work
    /// uses scoped chains.
    pub fn new_scoped(identity: Identity) -> Result<Document, String> {
        let chain_id = ChainId::generate().map_err(|error| error.to_string())?;
        let chain = Chain::new_scoped(chain_id.as_str())
            .map_err(|error| format!("cannot create scoped chain: {error}"))?;
        Ok(Self::with_chain(identity, chain))
    }

    fn with_chain(identity: Identity, chain: Chain) -> Document {
        Document {
            chain,
            graph: Graph::new(),
            pending: Vec::new(),
            recovery_ops: Vec::new(),
            undo_history: Vec::new(),
            redo_history: Vec::new(),
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

    /// Restore and fully validate a durable workspace snapshot. Pending ops
    /// are all-or-nothing: a corrupt snapshot cannot partially mutate a doc.
    pub fn restore(
        identity: Identity,
        chain: Chain,
        pending: Vec<GraphOp>,
        recovery_ops: Vec<GraphOp>,
        view_index: Option<usize>,
    ) -> Result<Document, String> {
        chain
            .validate()
            .map_err(|e| format!("saved chain validation failed: {e}"))?;
        let mut graph = chain
            .replay(None)
            .map_err(|e| format!("saved chain replay failed: {e}"))?;
        graph.apply_all(&pending).map_err(|(i, e)| {
            format!("saved pending operation {} cannot be replayed: {e}", i + 1)
        })?;
        let mut doc = Document {
            chain,
            graph,
            pending,
            recovery_ops,
            undo_history: Vec::new(),
            redo_history: Vec::new(),
            evaluator: Evaluator::new(),
            registry: Registry::standard(),
            identity,
            last_eval: EvalOutput::default(),
            view_index: None,
            view_graph: None,
            gesture: None,
            scene_dirty: true,
        };
        doc.set_view(view_index)?;
        Ok(doc)
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

    pub fn view_index(&self) -> Option<usize> {
        self.view_index
    }

    /// Whether this document has no authored work and may safely adopt the
    /// genesis/history of a remote project on its first explicit Pull.
    pub fn is_pristine(&self) -> bool {
        self.chain.len() == 1 && self.chain.total_ops() == 0 && self.pending.is_empty()
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

    /// Force a viewport rebuild (kept for callers that change display state
    /// outside `apply_op`, e.g. future display options).
    #[allow(dead_code)]
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
        self.record_history_boundary();
        self.invalidate_for(&op);
        self.pending.push(op);
        Ok(())
    }

    /// Apply several ops as one user action (and therefore one Undo step).
    /// The batch is validated on a clone first so failures cannot leave a
    /// half-applied edit in the working graph.
    pub fn apply_ops(&mut self, ops: Vec<GraphOp>) -> Result<usize, String> {
        if !self.editable() {
            return Err("read-only: viewing chain history".into());
        }
        if ops.is_empty() {
            return Ok(0);
        }
        let mut next = self.graph.clone();
        next.apply_all(&ops)
            .map_err(|(i, e)| format!("operation {} failed: {e}", i + 1))?;
        self.record_history_boundary();
        self.graph = next;
        for op in &ops {
            self.invalidate_for(op);
        }
        let count = ops.len();
        self.pending.extend(ops);
        Ok(count)
    }

    fn record_history_boundary(&mut self) {
        self.undo_history.push(self.pending.clone());
        self.redo_history.clear();
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
    // undo / redo (pending edits only)
    // ------------------------------------------------------------------

    /// Whether the current, uncommitted edit history can move backward.
    pub fn can_undo(&self) -> bool {
        self.editable() && (self.gesture.is_some() || !self.undo_history.is_empty())
    }

    /// Whether an undone, uncommitted edit can be restored.
    pub fn can_redo(&self) -> bool {
        self.editable() && !self.redo_history.is_empty()
    }

    /// Number of user-level pending edit steps available to undo.
    pub fn undo_depth(&self) -> usize {
        self.undo_history.len()
    }

    /// Number of user-level pending edit steps available to redo.
    pub fn redo_depth(&self) -> usize {
        self.redo_history.len()
    }

    /// Undo one uncommitted user edit. Returns the number of GraphOps removed
    /// from the pending ledger. Committed blocks are an intentional hard stop.
    pub fn undo_pending(&mut self) -> Result<usize, String> {
        if !self.editable() {
            return Err("read-only: viewing chain history".into());
        }
        self.end_gesture();
        let previous = self
            .undo_history
            .pop()
            .ok_or_else(|| "nothing uncommitted to undo".to_string())?;
        let current = self.pending.clone();
        let removed = current.len().saturating_sub(previous.len());
        let graph = match self.replay_pending(&previous) {
            Ok(graph) => graph,
            Err(e) => {
                self.undo_history.push(previous);
                return Err(e);
            }
        };
        self.pending = previous;
        self.graph = graph;
        self.redo_history.push(current);
        self.invalidate_after_history_move();
        Ok(removed)
    }

    /// Redo one uncommitted user edit. Returns the number of GraphOps restored.
    pub fn redo_pending(&mut self) -> Result<usize, String> {
        if !self.editable() {
            return Err("read-only: viewing chain history".into());
        }
        self.end_gesture();
        let next = self
            .redo_history
            .pop()
            .ok_or_else(|| "nothing to redo".to_string())?;
        let current = self.pending.clone();
        let restored = next.len().saturating_sub(current.len());
        let graph = match self.replay_pending(&next) {
            Ok(graph) => graph,
            Err(e) => {
                self.redo_history.push(next);
                return Err(e);
            }
        };
        self.undo_history.push(current);
        self.pending = next;
        self.graph = graph;
        self.invalidate_after_history_move();
        Ok(restored)
    }

    fn replay_pending(&self, pending: &[GraphOp]) -> Result<Graph, String> {
        let mut graph = self
            .chain
            .replay(None)
            .map_err(|e| format!("history replay failed: {e}"))?;
        graph
            .apply_all(pending)
            .map_err(|(i, e)| format!("pending operation {} failed during replay: {e}", i + 1))?;
        Ok(graph)
    }

    fn invalidate_after_history_move(&mut self) {
        self.evaluator.invalidate_all();
        self.scene_dirty = true;
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
        if let Some(Gesture::Param {
            id,
            key,
            start,
            last,
        }) = self.gesture.take()
        {
            if !self.graph.nodes.contains_key(&id) {
                return; // node deleted mid-gesture: nothing valid to record
            }
            if start.as_ref() == Some(&last) {
                return; // no net change
            }
            // The graph already holds `last` (applied live) — just record it.
            self.record_history_boundary();
            self.pending.push(GraphOp::SetParam {
                id,
                key,
                value: last,
            });
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
            let mut ops = Vec::new();
            for (id, start_pos) in start {
                let Some(node) = self.graph.nodes.get(&id) else {
                    continue;
                };
                let pos = node.pos;
                if pos != start_pos {
                    ops.push(GraphOp::MoveNode { id, pos });
                }
            }
            if !ops.is_empty() {
                self.record_history_boundary();
                self.pending.extend(ops);
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
        self.undo_history.clear();
        self.redo_history.clear();
        Ok(count)
    }

    /// Merge blocks pulled from the server: extend the chain, rebuild the
    /// working graph from a full replay, then re-apply pending ops one by
    /// one. Conflicts are moved into `recovery_ops`, never silently lost.
    pub fn merge_remote(&mut self, blocks: &[Block]) -> Result<MergeReport, ChainError> {
        self.end_gesture();
        let appended = self.chain.try_extend(blocks)?;
        if appended == 0 {
            return Ok(MergeReport {
                appended: 0,
                dropped: 0,
            });
        }
        let mut graph = self.chain.replay(None)?;
        let mut kept = Vec::with_capacity(self.pending.len());
        let mut conflicts = Vec::new();
        for op in self.pending.drain(..) {
            match graph.apply(&op) {
                Ok(()) => kept.push(op),
                Err(_) => conflicts.push(op),
            }
        }
        let dropped = conflicts.len();
        self.recovery_ops.extend(conflicts);
        self.pending = kept;
        self.graph = graph;
        // A changed chain is a new replay base. Stale local snapshots could
        // otherwise resurrect edits against a different remote history.
        self.undo_history.clear();
        self.redo_history.clear();
        // Leave any time-travel view in place (indices are still valid: the
        // chain only grew), but refresh everything.
        self.evaluator.invalidate_all();
        self.scene_dirty = true;
        Ok(MergeReport { appended, dropped })
    }

    /// Replace a pristine local placeholder chain with a complete remote
    /// history (including its project-scoped genesis).
    pub fn adopt_remote(&mut self, blocks: Vec<Block>) -> Result<usize, String> {
        self.end_gesture();
        if !self.is_pristine() {
            return Err("remote history can only be adopted by a pristine workspace".into());
        }
        let chain = Chain { blocks };
        chain
            .validate()
            .map_err(|e| format!("remote chain validation failed: {e}"))?;
        let graph = chain
            .replay(None)
            .map_err(|e| format!("remote chain replay failed: {e}"))?;
        let len = chain.len();
        self.chain = chain;
        self.graph = graph;
        self.pending.clear();
        self.undo_history.clear();
        self.redo_history.clear();
        self.view_index = None;
        self.view_graph = None;
        self.evaluator.invalidate_all();
        self.scene_dirty = true;
        Ok(len)
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
    fn new_personal_workspace_uses_unique_scoped_v2_genesis() {
        let first = Document::new_scoped(Identity::generate("a")).unwrap();
        let second = Document::new_scoped(Identity::generate("b")).unwrap();
        assert_eq!(first.chain.format_version().unwrap(), 2);
        assert!(first.chain.chain_id().unwrap().is_some());
        assert_ne!(first.chain.blocks[0].hash, second.chain.blocks[0].hash);
    }

    #[test]
    fn undo_redo_round_trips_pending_graph_exactly() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.set_param(nid(1), "value", ParamValue::Number(7.5))
            .unwrap();
        let edited = d.graph.clone();
        let edited_pending = d.pending.clone();

        assert_eq!(d.undo_depth(), 2);
        assert_eq!(d.undo_pending().unwrap(), 1);
        assert_eq!(d.pending.len(), 1);
        assert!(!d.graph.nodes[&nid(1)].params.contains_key("value"));
        assert!(d.can_redo());

        assert_eq!(d.redo_pending().unwrap(), 1);
        assert_eq!(d.graph, edited);
        assert_eq!(d.pending, edited_pending);
        assert!(!d.can_redo());
    }

    #[test]
    fn new_edit_clears_redo_history() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.set_param(nid(1), "value", ParamValue::Number(2.0))
            .unwrap();
        d.undo_pending().unwrap();
        assert!(d.can_redo());

        d.apply_op(GraphOp::MoveNode {
            id: nid(1),
            pos: (20.0, 30.0),
        })
        .unwrap();
        assert!(!d.can_redo());
    }

    #[test]
    fn batch_edit_is_atomic_and_one_undo_step() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        add(&mut d, 2, "panel");
        let before_depth = d.undo_depth();
        let before_pending = d.pending.len();

        let count = d
            .apply_ops(vec![
                GraphOp::RemoveNode { id: nid(1) },
                GraphOp::RemoveNode { id: nid(2) },
            ])
            .unwrap();
        assert_eq!(count, 2);
        assert!(d.graph.nodes.is_empty());
        assert_eq!(d.undo_depth(), before_depth + 1);

        assert_eq!(d.undo_pending().unwrap(), 2);
        assert_eq!(d.pending.len(), before_pending);
        assert!(d.graph.nodes.contains_key(&nid(1)));
        assert!(d.graph.nodes.contains_key(&nid(2)));

        let pending = d.pending.clone();
        let graph = d.graph.clone();
        assert!(d
            .apply_ops(vec![
                GraphOp::RemoveNode { id: nid(1) },
                GraphOp::RemoveNode { id: nid(99) },
            ])
            .is_err());
        assert_eq!(d.pending, pending, "failed batch records no partial ops");
        assert_eq!(d.graph, graph, "failed batch leaves graph untouched");
    }

    #[test]
    fn commit_is_an_immutable_undo_boundary() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.commit("checkpoint", 1).unwrap();

        assert!(!d.can_undo());
        assert!(d.undo_pending().is_err());
        assert!(d.graph.nodes.contains_key(&nid(1)));
        assert_eq!(d.chain.len(), 2);
        d.chain.validate().unwrap();
    }

    #[test]
    fn undo_finishes_and_reverses_a_live_gesture() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.param_drag(nid(1), "value", ParamValue::Number(9.0));
        assert!(d.gesture_active());

        assert_eq!(d.undo_pending().unwrap(), 1);
        assert!(!d.gesture_active());
        assert!(!d.graph.nodes[&nid(1)].params.contains_key("value"));
        assert!(d.can_redo());
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
        d.set_param(nid(1), "value", ParamValue::Number(4.0))
            .unwrap();
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
        d.set_param(nid(1), "value", ParamValue::Number(3.0))
            .unwrap();
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
        bob.set_param(nid(1), "value", ParamValue::Number(4.0))
            .unwrap();

        let report = bob.merge_remote(&alice.chain.blocks).unwrap();
        assert_eq!(report.appended, 1);
        assert_eq!(report.dropped, 1, "duplicate AddNode dropped");
        assert_eq!(bob.chain.len(), 2);
        assert_eq!(bob.pending.len(), 2, "panel add + set_param kept");
        assert_eq!(
            bob.recovery_ops.len(),
            1,
            "conflict is preserved for recovery"
        );
        assert_eq!(
            bob.recovery_ops[0],
            GraphOp::AddNode {
                id: nid(1),
                type_name: "number_slider".into(),
                pos: (0.0, 0.0),
            }
        );
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
        assert_eq!(
            report,
            MergeReport {
                appended: 0,
                dropped: 0
            }
        );
        assert_eq!(a.pending.len(), 1);
    }

    #[test]
    fn durable_restore_replays_chain_and_pending_without_dropping_ops() {
        let mut original = doc("alice");
        add(&mut original, 1, "number_slider");
        original.commit("base", 1).unwrap();
        original
            .set_param(nid(1), "value", ParamValue::Number(12.0))
            .unwrap();
        let restored = Document::restore(
            Identity::generate("bob"),
            original.chain.clone(),
            original.pending.clone(),
            vec![GraphOp::MoveNode {
                id: nid(1),
                pos: (5.0, 6.0),
            }],
            None,
        )
        .unwrap();
        assert_eq!(restored.graph, original.graph);
        assert_eq!(restored.pending, original.pending);
        assert_eq!(restored.recovery_ops.len(), 1);
    }

    #[test]
    fn durable_restore_rejects_invalid_pending_atomically() {
        let result = Document::restore(
            Identity::generate("bob"),
            Chain::new(),
            vec![GraphOp::RemoveNode { id: nid(99) }],
            Vec::new(),
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn pristine_workspace_can_adopt_scoped_remote_history() {
        let mut remote_chain = Chain::new_scoped(&"ab".repeat(32)).unwrap();
        remote_chain
            .append(
                vec![GraphOp::AddNode {
                    id: nid(1),
                    type_name: "panel".into(),
                    pos: (0.0, 0.0),
                }],
                "remote",
                &Identity::generate("remote"),
                1,
            )
            .unwrap();
        let mut local = doc("local");
        assert_eq!(local.adopt_remote(remote_chain.blocks.clone()).unwrap(), 2);
        assert_eq!(local.chain, remote_chain);
        assert!(local.graph.nodes.contains_key(&nid(1)));
        assert!(local.adopt_remote(remote_chain.blocks).is_err());
    }

    #[test]
    fn eval_runs_on_display_graph() {
        let mut d = doc("a");
        add(&mut d, 1, "number_slider");
        d.set_param(nid(1), "value", ParamValue::Number(7.0))
            .unwrap();
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
