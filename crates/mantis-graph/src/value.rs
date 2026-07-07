//! Values flowing along wires, and node parameter values.

use mantis_kernel::{Curve, Mesh, Plane, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A value on a wire. Heavy geometry is Arc'd — clones are cheap.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Number(f64),
    Bool(bool),
    Text(String),
    Vector(Vec3),
    Plane(Plane),
    Curve(Arc<Curve>),
    Mesh(Arc<Mesh>),
    List(Vec<Value>),
}

/// Declared type of a port. `Any` accepts every variant (geometry verbs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueKind {
    Any,
    Number,
    Bool,
    Text,
    Vector,
    Plane,
    Curve,
    Mesh,
}

impl Value {
    pub fn kind_matches(&self, k: ValueKind) -> bool {
        match (self, k) {
            (_, ValueKind::Any) => true,
            (Value::Number(_), ValueKind::Number) => true,
            (Value::Bool(_), ValueKind::Bool) => true,
            (Value::Text(_), ValueKind::Text) => true,
            (Value::Vector(_), ValueKind::Vector) => true,
            (Value::Plane(_), ValueKind::Plane) => true,
            (Value::Curve(_), ValueKind::Curve) => true,
            (Value::Mesh(_), ValueKind::Mesh) => true,
            // Grasshopper-style implicit coercions the `as_*` helpers implement:
            // a point stands in for a world-XY plane at that origin, and a
            // number stands in for a bool (0 = false). Kept in lockstep with
            // `as_plane` / `as_bool` so a wired input is accepted wherever the
            // component can consume it.
            (Value::Vector(_), ValueKind::Plane) => true,
            (Value::Number(_), ValueKind::Bool) => true,
            (Value::Null, _) => false,
            (Value::List(_), _) => false, // engine unwraps lists before checking
            _ => false,
        }
    }
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => Some(*n != 0.0),
            _ => None,
        }
    }
    pub fn as_vector(&self) -> Option<Vec3> {
        match self {
            Value::Vector(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_plane(&self) -> Option<Plane> {
        match self {
            Value::Plane(p) => Some(*p),
            // GH-style convenience: a point makes a world-XY plane there.
            Value::Vector(v) => Some(Plane::world_xy_at(*v)),
            _ => None,
        }
    }
    pub fn as_curve(&self) -> Option<Arc<Curve>> {
        match self {
            Value::Curve(c) => Some(c.clone()),
            _ => None,
        }
    }
    pub fn as_mesh(&self) -> Option<Arc<Mesh>> {
        match self {
            Value::Mesh(m) => Some(m.clone()),
            _ => None,
        }
    }
    /// One-line human description ("Mesh (482 v, 960 f)", "3.14", ...).
    pub fn describe(&self) -> String {
        match self {
            Value::Null => "∅".into(),
            Value::Number(n) => format!("{n:.4}"),
            Value::Bool(b) => b.to_string(),
            Value::Text(s) => s.clone(),
            Value::Vector(v) => format!("({:.3}, {:.3}, {:.3})", v.x, v.y, v.z),
            Value::Plane(_) => "Plane".into(),
            Value::Curve(_) => "Curve".into(),
            Value::Mesh(m) => format!("Mesh ({} v, {} f)", m.vertex_count(), m.triangle_count()),
            Value::List(l) => format!("List [{}]", l.len()),
        }
    }
}

/// Persistent, serializable node parameters (slider values, panel text, the
/// "__preview" flag...). These are what `GraphOp::SetParam` carries — part of
/// the on-chain format, so keep this enum stable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamValue {
    Number(f64),
    Text(String),
    Bool(bool),
}

impl ParamValue {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            ParamValue::Number(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ParamValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ParamValue::Text(t) => Some(t),
            _ => None,
        }
    }
    /// False if this is a non-finite `Number` (NaN / ±Infinity). Such values
    /// cannot survive JSON serialization (serde_json emits `null`), so anything
    /// that gets recorded on-chain must reject them first.
    pub fn is_finite(&self) -> bool {
        match self {
            ParamValue::Number(n) => n.is_finite(),
            _ => true,
        }
    }
}
