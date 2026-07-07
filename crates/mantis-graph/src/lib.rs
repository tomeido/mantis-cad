//! mantis-graph — Grasshopper-style dataflow engine for MantisCAD.
//!
//! Deterministic: no HashMap in eval/serialization paths, no randomness,
//! no clock. Must compile on wasm32.

pub mod component;
pub mod components;
pub mod eval;
pub mod graph;
pub mod value;

pub use component::{Access, Component, PortSpec, Registry};
pub use eval::{EvalOutput, Evaluator};
pub use graph::{Edge, Graph, GraphError, GraphOp, Node, NodeId};
pub use value::{ParamValue, Value, ValueKind};
