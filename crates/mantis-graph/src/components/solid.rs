//! Polygonal mesh solid operations. These deliberately expose Mesh ports so
//! they cannot be mistaken for exact trimmed NURBS/B-rep modeling.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use mantis_kernel::{
    solid::{self, BooleanOp},
    Plane,
};
use std::sync::Arc;

fn tolerance() -> PortSpec {
    PortSpec::item_default("tolerance", ValueKind::Number, Value::Number(1e-7))
}
fn plane() -> PortSpec {
    PortSpec::item_default("plane", ValueKind::Plane, Value::Plane(Plane::world_xy()))
}
fn boolean_inputs() -> Vec<PortSpec> {
    vec![
        PortSpec::item("a", ValueKind::Mesh),
        PortSpec::item("b", ValueKind::Mesh),
        tolerance(),
    ]
}
fn mesh_output() -> Vec<PortSpec> {
    vec![PortSpec::item("mesh", ValueKind::Mesh)]
}
fn boolean(inputs: &[Value], operation: BooleanOp) -> Result<Vec<Value>, String> {
    let a = util::mesh(inputs, 0, "a")?;
    let b = util::mesh(inputs, 1, "b")?;
    let tolerance = util::finite(inputs, 2, "tolerance")?;
    Ok(vec![Value::Mesh(Arc::new(solid::boolean(
        &a, &b, operation, tolerance,
    )?))])
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "mesh_boolean_union",
            label: "Mesh Boolean Union",
            category: "Surface",
            inputs: boolean_inputs,
            outputs: mesh_output,
            eval: |inputs, _| boolean(inputs, BooleanOp::Union),
        }),
        Arc::new(FnComponent {
            type_name: "mesh_boolean_difference",
            label: "Mesh Boolean Difference",
            category: "Surface",
            inputs: boolean_inputs,
            outputs: mesh_output,
            eval: |inputs, _| boolean(inputs, BooleanOp::Difference),
        }),
        Arc::new(FnComponent {
            type_name: "mesh_boolean_intersection",
            label: "Mesh Boolean Intersection",
            category: "Surface",
            inputs: boolean_inputs,
            outputs: mesh_output,
            eval: |inputs, _| boolean(inputs, BooleanOp::Intersection),
        }),
        Arc::new(FnComponent {
            type_name: "mesh_split_plane",
            label: "Mesh Split Plane",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item("mesh", ValueKind::Mesh),
                    plane(),
                    tolerance(),
                ]
            },
            outputs: || {
                vec![
                    PortSpec::item("negative", ValueKind::Mesh),
                    PortSpec::item("positive", ValueKind::Mesh),
                ]
            },
            eval: |inputs, _| {
                let mesh = util::mesh(inputs, 0, "mesh")?;
                let plane = util::plane(inputs, 1, "plane")?;
                let tolerance = util::finite(inputs, 2, "tolerance")?;
                let (negative, positive) = solid::split_plane(&mesh, &plane, tolerance)?;
                Ok(vec![
                    Value::Mesh(Arc::new(negative)),
                    Value::Mesh(Arc::new(positive)),
                ])
            },
        }),
        Arc::new(FnComponent {
            type_name: "mesh_trim_plane",
            label: "Mesh Trim Plane",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item("mesh", ValueKind::Mesh),
                    plane(),
                    PortSpec::item_default("keep_positive", ValueKind::Bool, Value::Bool(false)),
                    tolerance(),
                ]
            },
            outputs: mesh_output,
            eval: |inputs, _| {
                let mesh = util::mesh(inputs, 0, "mesh")?;
                let plane = util::plane(inputs, 1, "plane")?;
                let keep_positive = util::boolean(inputs, 2, "keep_positive")?;
                let tolerance = util::finite(inputs, 3, "tolerance")?;
                let (negative, positive) = solid::split_plane(&mesh, &plane, tolerance)?;
                Ok(vec![Value::Mesh(Arc::new(if keep_positive {
                    positive
                } else {
                    negative
                }))])
            },
        }),
    ]
}
