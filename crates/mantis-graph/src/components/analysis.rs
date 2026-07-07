//! Analysis category: measuring geometry and data.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use mantis_kernel::{BBox, Curve};
use std::sync::Arc;

/// Grow `b` by the extent of one value (recurses into lists).
fn include_bbox(v: &Value, b: &mut BBox) -> Result<(), String> {
    match v {
        Value::Vector(p) => {
            b.include(*p);
            Ok(())
        }
        Value::Plane(p) => {
            b.include(p.origin);
            Ok(())
        }
        Value::Curve(c) => {
            *b = b.union(c.bbox());
            Ok(())
        }
        Value::Mesh(m) => {
            *b = b.union(m.bbox());
            Ok(())
        }
        Value::List(l) => {
            for e in l {
                include_bbox(e, b)?;
            }
            Ok(())
        }
        other => Err(format!("bbox: unsupported input {}", other.describe())),
    }
}

/// Approximate serialized byte size of a value (see `data_size`).
fn approx_size(v: &Value) -> usize {
    match v {
        Value::Null => 0,
        Value::Number(_) => 8,
        Value::Bool(_) => 1,
        Value::Text(s) => s.len(),
        Value::Vector(_) => 24,
        Value::Plane(_) => 72,
        Value::Curve(c) => curve_size(c),
        Value::Mesh(m) => m.approx_byte_size(),
        Value::List(l) => l.iter().map(approx_size).sum(),
    }
}

fn curve_size(c: &Curve) -> usize {
    match c {
        Curve::Line { .. } => 48,
        Curve::Polyline { points, .. } => points.len() * 24 + 1,
        Curve::Circle { .. } => 80,
        Curve::Arc { .. } => 96,
        Curve::Nurbs(n) => {
            n.control_points.len() * 24 + n.weights.len() * 8 + n.knots.len() * 8 + 8
        }
    }
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "bbox",
            label: "Bounding Box",
            category: "Analysis",
            inputs: || vec![PortSpec::item("geometry", ValueKind::Any)],
            outputs: || {
                vec![
                    PortSpec::item("min", ValueKind::Vector),
                    PortSpec::item("max", ValueKind::Vector),
                ]
            },
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let mut b = BBox::EMPTY;
                include_bbox(g, &mut b)?;
                if b.is_empty() {
                    return Err("bbox: geometry has no extent".into());
                }
                Ok(vec![Value::Vector(b.min), Value::Vector(b.max)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "area",
            label: "Area",
            category: "Analysis",
            inputs: || vec![PortSpec::item("mesh", ValueKind::Mesh)],
            outputs: || vec![PortSpec::item("area", ValueKind::Number)],
            eval: |inputs, _| {
                let m = util::mesh(inputs, 0, "mesh")?;
                Ok(vec![Value::Number(m.area())])
            },
        }),
        // Signed volume; meaningful for closed meshes.
        Arc::new(FnComponent {
            type_name: "volume",
            label: "Volume",
            category: "Analysis",
            inputs: || vec![PortSpec::item("mesh", ValueKind::Mesh)],
            outputs: || vec![PortSpec::item("volume", ValueKind::Number)],
            eval: |inputs, _| {
                let m = util::mesh(inputs, 0, "mesh")?;
                Ok(vec![Value::Number(m.volume())])
            },
        }),
        Arc::new(FnComponent {
            type_name: "mesh_info",
            label: "Mesh Info",
            category: "Analysis",
            inputs: || vec![PortSpec::item("mesh", ValueKind::Mesh)],
            outputs: || {
                vec![
                    PortSpec::item("vertices", ValueKind::Number),
                    PortSpec::item("faces", ValueKind::Number),
                    PortSpec::item("bytes", ValueKind::Number),
                ]
            },
            eval: |inputs, _| {
                let m = util::mesh(inputs, 0, "mesh")?;
                Ok(vec![
                    Value::Number(m.vertex_count() as f64),
                    Value::Number(m.triangle_count() as f64),
                    Value::Number(m.approx_byte_size() as f64),
                ])
            },
        }),
        // Approximate serialized byte size of anything (numbers/vectors count
        // small fixed sizes; meshes use Mesh::approx_byte_size; lists sum).
        Arc::new(FnComponent {
            type_name: "data_size",
            label: "Data Size",
            category: "Analysis",
            inputs: || vec![PortSpec::list("data", ValueKind::Any)],
            outputs: || vec![PortSpec::item("bytes", ValueKind::Number)],
            eval: |inputs, _| {
                let l = util::list(inputs, 0, "data")?;
                let total: usize = l.iter().map(approx_size).sum();
                Ok(vec![Value::Number(total as f64)])
            },
        }),
    ]
}
