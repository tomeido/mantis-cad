//! The graph document + GraphOp — the ONLY thing ever recorded on-chain.
//! Fully implemented; the on-chain JSON format of `GraphOp` is frozen.

use crate::value::ParamValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// 128-bit node id, JSON-serialized as 32-char lowercase hex. Generated at the
/// UI edge (random) and recorded inside ops — replay never generates ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u128);

impl NodeId {
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }
    pub fn from_hex(s: &str) -> Option<NodeId> {
        if s.len() != 32 {
            return None;
        }
        u128::from_str_radix(s, 16).ok().map(NodeId)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // short form for UI/debug
        write!(f, "{}", &self.to_hex()[..8])
    }
}

impl Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        NodeId::from_hex(&s).ok_or_else(|| serde::de::Error::custom("bad NodeId hex"))
    }
}

/// (node, port index) endpoint.
pub type OutPort = (NodeId, u16);
pub type InPort = (NodeId, u16);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub type_name: String,
    pub pos: (f32, f32),
    #[serde(default)]
    pub params: BTreeMap<String, ParamValue>,
}

impl Node {
    /// Preview flag ("__preview" param, default true).
    pub fn preview(&self) -> bool {
        self.params
            .get("__preview")
            .and_then(|p| p.as_bool())
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: OutPort,
    pub to: InPort,
}

/// One graph mutation. THE on-chain record format — field/variant names are
/// frozen. Everything a MantisCAD document is, is a sequence of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum GraphOp {
    AddNode {
        id: NodeId,
        type_name: String,
        pos: (f32, f32),
    },
    RemoveNode {
        id: NodeId,
    },
    Connect {
        from: OutPort,
        to: InPort,
    },
    Disconnect {
        from: OutPort,
        to: InPort,
    },
    SetParam {
        id: NodeId,
        key: String,
        value: ParamValue,
    },
    MoveNode {
        id: NodeId,
        pos: (f32, f32),
    },
}

impl GraphOp {
    /// The node this op is "about" (for UI highlighting).
    pub fn subject(&self) -> NodeId {
        match self {
            GraphOp::AddNode { id, .. }
            | GraphOp::RemoveNode { id }
            | GraphOp::SetParam { id, .. }
            | GraphOp::MoveNode { id, .. } => *id,
            GraphOp::Connect { to, .. } | GraphOp::Disconnect { to, .. } => to.0,
        }
    }
    /// Short human description for the chain panel.
    pub fn describe(&self) -> String {
        match self {
            GraphOp::AddNode { type_name, .. } => format!("+ {type_name}"),
            GraphOp::RemoveNode { id } => format!("- node {id}"),
            GraphOp::Connect { from, to } => format!("{} ▸ {}", from.0, to.0),
            GraphOp::Disconnect { from, to } => format!("{} ✕ {}", from.0, to.0),
            GraphOp::SetParam { id, key, .. } => format!("{id}.{key} ="),
            GraphOp::MoveNode { .. } => "move".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    DuplicateNode(NodeId),
    UnknownNode(NodeId),
    EdgeExists,
    UnknownEdge,
    /// Connecting would create a cycle.
    Cycle,
    SelfLoop,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::DuplicateNode(id) => write!(f, "node {id} already exists"),
            GraphError::UnknownNode(id) => write!(f, "unknown node {id}"),
            GraphError::EdgeExists => write!(f, "edge already exists"),
            GraphError::UnknownEdge => write!(f, "no such edge"),
            GraphError::Cycle => write!(f, "connection would create a cycle"),
            GraphError::SelfLoop => write!(f, "cannot connect a node to itself"),
        }
    }
}
impl std::error::Error for GraphError {}

/// The document. Mutated exclusively through `apply`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: BTreeMap<NodeId, Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn new() -> Graph {
        Graph::default()
    }

    /// Validate + apply one op. This is the single mutation path used by both
    /// live editing and chain replay, so behavior here IS the file format.
    ///
    /// Semantics:
    /// - AddNode: id must be fresh. `type_name` is NOT validated against a
    ///   registry (forward compatibility) — unknown types simply error at eval.
    /// - RemoveNode: also removes all incident edges.
    /// - Connect: both nodes must exist, no self-loops, no cycles. An input
    ///   port holds at most ONE wire — connecting to an occupied input
    ///   replaces the existing wire. Port indices are not range-checked
    ///   (eval reports them).
    /// - Disconnect: exact edge must exist.
    /// - SetParam/MoveNode: node must exist. Unknown param keys are inert.
    pub fn apply(&mut self, op: &GraphOp) -> Result<(), GraphError> {
        match op {
            GraphOp::AddNode { id, type_name, pos } => {
                if self.nodes.contains_key(id) {
                    return Err(GraphError::DuplicateNode(*id));
                }
                self.nodes.insert(
                    *id,
                    Node {
                        id: *id,
                        type_name: type_name.clone(),
                        pos: *pos,
                        params: BTreeMap::new(),
                    },
                );
                Ok(())
            }
            GraphOp::RemoveNode { id } => {
                if self.nodes.remove(id).is_none() {
                    return Err(GraphError::UnknownNode(*id));
                }
                self.edges.retain(|e| e.from.0 != *id && e.to.0 != *id);
                Ok(())
            }
            GraphOp::Connect { from, to } => {
                if !self.nodes.contains_key(&from.0) {
                    return Err(GraphError::UnknownNode(from.0));
                }
                if !self.nodes.contains_key(&to.0) {
                    return Err(GraphError::UnknownNode(to.0));
                }
                if from.0 == to.0 {
                    return Err(GraphError::SelfLoop);
                }
                if self.edges.iter().any(|e| e.from == *from && e.to == *to) {
                    return Err(GraphError::EdgeExists);
                }
                if self.reaches(to.0, from.0) {
                    return Err(GraphError::Cycle);
                }
                // an input port holds one wire: replace
                self.edges.retain(|e| e.to != *to);
                self.edges.push(Edge { from: *from, to: *to });
                Ok(())
            }
            GraphOp::Disconnect { from, to } => {
                let before = self.edges.len();
                self.edges.retain(|e| !(e.from == *from && e.to == *to));
                if self.edges.len() == before {
                    return Err(GraphError::UnknownEdge);
                }
                Ok(())
            }
            GraphOp::SetParam { id, key, value } => {
                let node = self.nodes.get_mut(id).ok_or(GraphError::UnknownNode(*id))?;
                node.params.insert(key.clone(), value.clone());
                Ok(())
            }
            GraphOp::MoveNode { id, pos } => {
                let node = self.nodes.get_mut(id).ok_or(GraphError::UnknownNode(*id))?;
                node.pos = *pos;
                Ok(())
            }
        }
    }

    /// Apply a batch, stopping at the first error (returns index + error).
    pub fn apply_all(&mut self, ops: &[GraphOp]) -> Result<(), (usize, GraphError)> {
        for (i, op) in ops.iter().enumerate() {
            self.apply(op).map_err(|e| (i, e))?;
        }
        Ok(())
    }

    /// Is `to` reachable from `from` following edge direction? (cycle check)
    fn reaches(&self, from: NodeId, to: NodeId) -> bool {
        if from == to {
            return true;
        }
        let mut stack = vec![from];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(n) = stack.pop() {
            for e in self.edges.iter().filter(|e| e.from.0 == n) {
                let next = e.to.0;
                if next == to {
                    return true;
                }
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        false
    }

    /// Incoming edge for an input port, if any.
    pub fn incoming(&self, to: InPort) -> Option<&Edge> {
        self.edges.iter().find(|e| e.to == to)
    }
    /// All edges leaving a node.
    pub fn outgoing(&self, from_node: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.from.0 == from_node)
    }
    /// Nodes downstream of `id` (transitive, excluding id) — dirty propagation.
    pub fn downstream(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(n) = stack.pop() {
            for e in self.edges.iter().filter(|e| e.from.0 == n) {
                if seen.insert(e.to.0) {
                    out.push(e.to.0);
                    stack.push(e.to.0);
                }
            }
        }
        out
    }
    /// Deterministic topological order (Kahn; ties by ascending NodeId).
    /// Nodes in cycles are omitted (cannot happen via `apply`, but be safe).
    pub fn topo_order(&self) -> Vec<NodeId> {
        let mut indeg: BTreeMap<NodeId, usize> =
            self.nodes.keys().map(|id| (*id, 0)).collect();
        for e in &self.edges {
            if let Some(d) = indeg.get_mut(&e.to.0) {
                if self.nodes.contains_key(&e.from.0) {
                    *d += 1;
                }
            }
        }
        let mut ready: std::collections::BTreeSet<NodeId> = indeg
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(&n) = ready.iter().next() {
            ready.remove(&n);
            order.push(n);
            for e in self.edges.iter().filter(|e| e.from.0 == n) {
                if let Some(d) = indeg.get_mut(&e.to.0) {
                    *d -= 1;
                    if *d == 0 {
                        ready.insert(e.to.0);
                    }
                }
            }
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(n: u128) -> NodeId {
        NodeId(n)
    }

    #[test]
    fn op_json_format_is_frozen() {
        let op = GraphOp::AddNode {
            id: nid(0xabc),
            type_name: "circle".into(),
            pos: (10.0, 20.5),
        };
        let j = serde_json::to_string(&op).unwrap();
        assert_eq!(
            j,
            r#"{"op":"AddNode","id":"00000000000000000000000000000abc","type_name":"circle","pos":[10.0,20.5]}"#
        );
        let back: GraphOp = serde_json::from_str(&j).unwrap();
        assert_eq!(back, op);
    }

    #[test]
    fn connect_replaces_input_wire_and_blocks_cycles() {
        let mut g = Graph::new();
        for i in 1..=3u128 {
            g.apply(&GraphOp::AddNode { id: nid(i), type_name: "t".into(), pos: (0.0, 0.0) })
                .unwrap();
        }
        g.apply(&GraphOp::Connect { from: (nid(1), 0), to: (nid(3), 0) }).unwrap();
        // replacing the same input with a wire from node 2
        g.apply(&GraphOp::Connect { from: (nid(2), 0), to: (nid(3), 0) }).unwrap();
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.incoming((nid(3), 0)).unwrap().from.0, nid(2));
        // 3 -> 2 would close a cycle 2 -> 3 -> 2
        assert_eq!(
            g.apply(&GraphOp::Connect { from: (nid(3), 0), to: (nid(2), 0) }),
            Err(GraphError::Cycle)
        );
        // self loop
        assert_eq!(
            g.apply(&GraphOp::Connect { from: (nid(2), 1), to: (nid(2), 0) }),
            Err(GraphError::SelfLoop)
        );
    }

    #[test]
    fn remove_node_drops_incident_edges() {
        let mut g = Graph::new();
        for i in 1..=3u128 {
            g.apply(&GraphOp::AddNode { id: nid(i), type_name: "t".into(), pos: (0.0, 0.0) })
                .unwrap();
        }
        g.apply(&GraphOp::Connect { from: (nid(1), 0), to: (nid(2), 0) }).unwrap();
        g.apply(&GraphOp::Connect { from: (nid(2), 0), to: (nid(3), 0) }).unwrap();
        g.apply(&GraphOp::RemoveNode { id: nid(2) }).unwrap();
        assert!(g.edges.is_empty());
    }

    #[test]
    fn topo_order_deterministic() {
        let mut g = Graph::new();
        for i in [5u128, 3, 9, 1] {
            g.apply(&GraphOp::AddNode { id: nid(i), type_name: "t".into(), pos: (0.0, 0.0) })
                .unwrap();
        }
        g.apply(&GraphOp::Connect { from: (nid(9), 0), to: (nid(1), 0) }).unwrap();
        let order = g.topo_order();
        assert_eq!(order.len(), 4);
        // sources in ascending id order, 1 after 9
        let pos9 = order.iter().position(|n| *n == nid(9)).unwrap();
        let pos1 = order.iter().position(|n| *n == nid(1)).unwrap();
        assert!(pos9 < pos1);
        assert_eq!(order[0], nid(3));
    }
}
