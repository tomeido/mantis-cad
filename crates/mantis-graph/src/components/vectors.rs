//! Vector category: vector construction, arithmetic and planes.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use mantis_kernel::{Mat4, Plane, Vec3};
use std::sync::Arc;

fn xyz_in() -> Vec<PortSpec> {
    vec![
        PortSpec::item_default("x", ValueKind::Number, Value::Number(0.0)),
        PortSpec::item_default("y", ValueKind::Number, Value::Number(0.0)),
        PortSpec::item_default("z", ValueKind::Number, Value::Number(0.0)),
    ]
}

fn vec_out() -> Vec<PortSpec> {
    vec![PortSpec::item("vector", ValueKind::Vector)]
}

fn factor_in() -> Vec<PortSpec> {
    vec![PortSpec::item_default(
        "factor",
        ValueKind::Number,
        Value::Number(1.0),
    )]
}

fn ab_vec_in() -> Vec<PortSpec> {
    vec![
        PortSpec::item("a", ValueKind::Vector),
        PortSpec::item("b", ValueKind::Vector),
    ]
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "vector_xyz",
            label: "Vector XYZ",
            category: "Vector",
            inputs: xyz_in,
            outputs: vec_out,
            eval: |inputs, _| {
                let x = util::num(inputs, 0, "x")?;
                let y = util::num(inputs, 1, "y")?;
                let z = util::num(inputs, 2, "z")?;
                Ok(vec![Value::Vector(Vec3::new(x, y, z))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "deconstruct_vector",
            label: "Deconstruct Vector",
            category: "Vector",
            inputs: || vec![PortSpec::item("vector", ValueKind::Vector)],
            outputs: || {
                vec![
                    PortSpec::item("x", ValueKind::Number),
                    PortSpec::item("y", ValueKind::Number),
                    PortSpec::item("z", ValueKind::Number),
                ]
            },
            eval: |inputs, _| {
                let v = util::vector(inputs, 0, "vector")?;
                Ok(vec![
                    Value::Number(v.x),
                    Value::Number(v.y),
                    Value::Number(v.z),
                ])
            },
        }),
        Arc::new(FnComponent {
            type_name: "unit_x",
            label: "Unit X",
            category: "Vector",
            inputs: factor_in,
            outputs: vec_out,
            eval: |inputs, _| {
                let f = util::num(inputs, 0, "factor")?;
                Ok(vec![Value::Vector(Vec3::X * f)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "unit_y",
            label: "Unit Y",
            category: "Vector",
            inputs: factor_in,
            outputs: vec_out,
            eval: |inputs, _| {
                let f = util::num(inputs, 0, "factor")?;
                Ok(vec![Value::Vector(Vec3::Y * f)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "unit_z",
            label: "Unit Z",
            category: "Vector",
            inputs: factor_in,
            outputs: vec_out,
            eval: |inputs, _| {
                let f = util::num(inputs, 0, "factor")?;
                Ok(vec![Value::Vector(Vec3::Z * f)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "distance",
            label: "Distance",
            category: "Vector",
            inputs: ab_vec_in,
            outputs: || vec![PortSpec::item("distance", ValueKind::Number)],
            eval: |inputs, _| {
                let a = util::vector(inputs, 0, "a")?;
                let b = util::vector(inputs, 1, "b")?;
                Ok(vec![Value::Number(a.distance(b))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "dot",
            label: "Dot Product",
            category: "Vector",
            inputs: ab_vec_in,
            outputs: || vec![PortSpec::item("dot", ValueKind::Number)],
            eval: |inputs, _| {
                let a = util::vector(inputs, 0, "a")?;
                let b = util::vector(inputs, 1, "b")?;
                Ok(vec![Value::Number(a.dot(b))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "cross",
            label: "Cross Product",
            category: "Vector",
            inputs: ab_vec_in,
            outputs: || vec![PortSpec::item("cross", ValueKind::Vector)],
            eval: |inputs, _| {
                let a = util::vector(inputs, 0, "a")?;
                let b = util::vector(inputs, 1, "b")?;
                Ok(vec![Value::Vector(a.cross(b))])
            },
        }),
        // Set the length of a vector, keeping its direction.
        Arc::new(FnComponent {
            type_name: "amplitude",
            label: "Amplitude",
            category: "Vector",
            inputs: || {
                vec![
                    PortSpec::item("vector", ValueKind::Vector),
                    PortSpec::item_default("length", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: vec_out,
            eval: |inputs, _| {
                let v = util::vector(inputs, 0, "vector")?;
                let l = util::finite(inputs, 1, "length")?;
                let n = v.normalized();
                if n == Vec3::ZERO {
                    return Err("amplitude: zero-length vector has no direction".into());
                }
                Ok(vec![Value::Vector(n * l)])
            },
        }),
        // Rotate a vector around an axis through the origin (right-handed).
        Arc::new(FnComponent {
            type_name: "rotate_vector",
            label: "Rotate Vector",
            category: "Vector",
            inputs: || {
                vec![
                    PortSpec::item("vector", ValueKind::Vector),
                    PortSpec::item_default("axis", ValueKind::Vector, Value::Vector(Vec3::Z)),
                    PortSpec::item_default("angle", ValueKind::Number, Value::Number(0.0)),
                ]
            },
            outputs: vec_out,
            eval: |inputs, _| {
                let v = util::vector(inputs, 0, "vector")?;
                let axis = util::vector(inputs, 1, "axis")?;
                let angle = util::finite(inputs, 2, "angle")?;
                let m = Mat4::rotation_axis(Vec3::ZERO, axis, angle);
                Ok(vec![Value::Vector(m.transform_vector(v))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "xy_plane",
            label: "XY Plane",
            category: "Vector",
            inputs: || {
                vec![PortSpec::item_default(
                    "origin",
                    ValueKind::Vector,
                    Value::Vector(Vec3::ZERO),
                )]
            },
            outputs: || vec![PortSpec::item("plane", ValueKind::Plane)],
            eval: |inputs, _| {
                let o = util::vector(inputs, 0, "origin")?;
                Ok(vec![Value::Plane(Plane::world_xy_at(o))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "plane_normal",
            label: "Plane Normal",
            category: "Vector",
            inputs: || {
                vec![
                    PortSpec::item_default("origin", ValueKind::Vector, Value::Vector(Vec3::ZERO)),
                    PortSpec::item_default("normal", ValueKind::Vector, Value::Vector(Vec3::Z)),
                ]
            },
            outputs: || vec![PortSpec::item("plane", ValueKind::Plane)],
            eval: |inputs, _| {
                let o = util::vector(inputs, 0, "origin")?;
                let n = util::vector(inputs, 1, "normal")?;
                Ok(vec![Value::Plane(Plane::from_normal(o, n))])
            },
        }),
    ]
}
