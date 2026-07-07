//! Surface category: mesh-generating verbs and primitives.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use mantis_kernel::{ops, Mesh, Plane, Vec3};
use std::sync::Arc;

fn world_xy_default() -> PortSpec {
    PortSpec::item_default("plane", ValueKind::Plane, Value::Plane(Plane::world_xy()))
}

fn segments_default(v: f64) -> PortSpec {
    PortSpec::item_default("segments", ValueKind::Number, Value::Number(v))
}

fn mesh_out() -> Vec<PortSpec> {
    vec![PortSpec::item("mesh", ValueKind::Mesh)]
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "extrude",
            label: "Extrude",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    PortSpec::item_default(
                        "direction",
                        ValueKind::Vector,
                        Value::Vector(Vec3::new(0.0, 0.0, 1.0)),
                    ),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                let dir = util::vector(inputs, 1, "direction")?;
                let segs = util::segments(inputs, 2, "segments", 1)?;
                Ok(vec![Value::Mesh(Arc::new(ops::extrude(&c, dir, segs)))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "revolve",
            label: "Revolve",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    PortSpec::item_default(
                        "axis_origin",
                        ValueKind::Vector,
                        Value::Vector(Vec3::ZERO),
                    ),
                    PortSpec::item_default("axis_dir", ValueKind::Vector, Value::Vector(Vec3::Z)),
                    PortSpec::item_default(
                        "angle",
                        ValueKind::Number,
                        Value::Number(std::f64::consts::TAU),
                    ),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                let origin = util::vector(inputs, 1, "axis_origin")?;
                let dir = util::vector(inputs, 2, "axis_dir")?;
                let angle = util::finite(inputs, 3, "angle")?;
                let segs = util::segments(inputs, 4, "segments", 3)?;
                Ok(vec![Value::Mesh(Arc::new(ops::revolve(
                    &c, origin, dir, angle, segs,
                )))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "loft",
            label: "Loft",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::list("curves", ValueKind::Curve),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let sections = util::curves(inputs, 0, "curves")?;
                let segs = util::segments(inputs, 1, "segments", 3)?;
                let m = ops::loft(&sections, segs)?;
                Ok(vec![Value::Mesh(Arc::new(m))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "pipe",
            label: "Pipe",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    PortSpec::item_default("radius", ValueKind::Number, Value::Number(0.2)),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                let r = util::positive(inputs, 1, "radius")?;
                // One segment count drives both the rail sampling and the
                // tube ring (ring needs >= 3 to be a surface).
                let segs = util::segments(inputs, 2, "segments", 3)?;
                Ok(vec![Value::Mesh(Arc::new(ops::pipe(&c, r, segs, segs)))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "planar_srf",
            label: "Planar Surface",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                let segs = util::segments(inputs, 1, "segments", 3)?;
                let m = ops::planar_surface(&c, segs)?;
                Ok(vec![Value::Mesh(Arc::new(m))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "box_mesh",
            label: "Box",
            category: "Surface",
            inputs: || {
                vec![
                    world_xy_default(),
                    PortSpec::item_default("x", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("y", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("z", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let plane = util::plane(inputs, 0, "plane")?;
                let x = util::finite(inputs, 1, "x")?;
                let y = util::finite(inputs, 2, "y")?;
                let z = util::finite(inputs, 3, "z")?;
                Ok(vec![Value::Mesh(Arc::new(Mesh::box_mesh(&plane, x, y, z)))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "sphere",
            label: "Sphere",
            category: "Surface",
            inputs: || {
                vec![
                    PortSpec::item_default("center", ValueKind::Vector, Value::Vector(Vec3::ZERO)),
                    PortSpec::item_default("radius", ValueKind::Number, Value::Number(1.0)),
                    segments_default(24.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let center = util::vector(inputs, 0, "center")?;
                let r = util::positive(inputs, 1, "radius")?;
                let segs = util::segments(inputs, 2, "segments", 3)?;
                let v_segs = (segs / 2).max(2);
                Ok(vec![Value::Mesh(Arc::new(Mesh::sphere(
                    center, r, segs, v_segs,
                )))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "cylinder",
            label: "Cylinder",
            category: "Surface",
            inputs: || {
                vec![
                    world_xy_default(),
                    PortSpec::item_default("radius", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("height", ValueKind::Number, Value::Number(2.0)),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let plane = util::plane(inputs, 0, "plane")?;
                let r = util::positive(inputs, 1, "radius")?;
                let h = util::finite(inputs, 2, "height")?;
                let segs = util::segments(inputs, 3, "segments", 3)?;
                Ok(vec![Value::Mesh(Arc::new(Mesh::cylinder(
                    &plane, r, h, segs,
                )))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "cone",
            label: "Cone",
            category: "Surface",
            inputs: || {
                vec![
                    world_xy_default(),
                    PortSpec::item_default("radius", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("height", ValueKind::Number, Value::Number(2.0)),
                    segments_default(32.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let plane = util::plane(inputs, 0, "plane")?;
                let r = util::positive(inputs, 1, "radius")?;
                let h = util::finite(inputs, 2, "height")?;
                let segs = util::segments(inputs, 3, "segments", 3)?;
                Ok(vec![Value::Mesh(Arc::new(Mesh::cone(&plane, r, h, segs)))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "torus",
            label: "Torus",
            category: "Surface",
            inputs: || {
                vec![
                    world_xy_default(),
                    PortSpec::item_default("major", ValueKind::Number, Value::Number(2.0)),
                    PortSpec::item_default("minor", ValueKind::Number, Value::Number(0.5)),
                    segments_default(24.0),
                ]
            },
            outputs: mesh_out,
            eval: |inputs, _| {
                let plane = util::plane(inputs, 0, "plane")?;
                let major = util::positive(inputs, 1, "major")?;
                let minor = util::positive(inputs, 2, "minor")?;
                let segs = util::segments(inputs, 3, "segments", 3)?;
                Ok(vec![Value::Mesh(Arc::new(Mesh::torus(
                    &plane, major, minor, segs, segs,
                )))])
            },
        }),
    ]
}
