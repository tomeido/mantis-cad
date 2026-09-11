//! Transform category: rigid/affine transforms of geometry values.
//!
//! Geometry variants supported: Vector (treated as a point), Plane, Curve,
//! Mesh. Anything else errors. The engine maps over lists on the `geometry`
//! port automatically (Item access).

use super::{analysis, util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use mantis_kernel::{Mat4, Plane, Vec3};
use std::sync::Arc;

/// Apply `m` to one geometry value, preserving its variant.
fn transform_geo(v: &Value, m: &Mat4, verb: &str) -> Result<Value, String> {
    match v {
        Value::Vector(p) => Ok(Value::Vector(m.transform_point(*p))),
        Value::Plane(p) => Ok(Value::Plane(p.transformed(m))),
        Value::Curve(c) => Ok(Value::Curve(Arc::new(c.transformed(m)))),
        Value::Mesh(mesh) => Ok(Value::Mesh(Arc::new(mesh.transformed(m)))),
        other => Err(format!(
            "{verb}: cannot transform {} (expected Vector/Plane/Curve/Mesh)",
            other.describe()
        )),
    }
}

fn geo_in() -> PortSpec {
    PortSpec::item("geometry", ValueKind::Any)
}

fn geo_out() -> Vec<PortSpec> {
    vec![PortSpec::item("geometry", ValueKind::Any)]
}

fn plane_in() -> PortSpec {
    PortSpec::item_default("plane", ValueKind::Plane, Value::Plane(Plane::world_xy()))
}

// A geometry copy has a much higher cost than a number in a Series. Bound
// both dimensions so an accidental array cannot exhaust a lightweight app.
const MAX_ARRAY_COUNT: usize = 4096;
const MAX_ARRAY_BYTES: usize = 64 * 1024 * 1024;

fn array_count(inputs: &[Value], index: usize, geometry: &Value) -> Result<usize, String> {
    let count = util::count(inputs, index, "count", MAX_ARRAY_COUNT)?;
    util::finite_geometry(geometry)?;
    // Fixed conservative overhead keeps the acceptance threshold identical
    // on wasm32 and native peers (size_of::<Value>() differs by target).
    let bytes = analysis::approx_size(geometry).saturating_add(128);
    if bytes.saturating_mul(count) > MAX_ARRAY_BYTES {
        return Err(
            "array: estimated output exceeds 64 MiB; reduce count or mesh resolution".into(),
        );
    }
    Ok(count)
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "move",
            label: "Move",
            category: "Transform",
            inputs: || {
                vec![
                    geo_in(),
                    PortSpec::item_default("motion", ValueKind::Vector, Value::Vector(Vec3::ZERO)),
                ]
            },
            outputs: geo_out,
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let motion = util::vector(inputs, 1, "motion")?;
                Ok(vec![transform_geo(g, &Mat4::translation(motion), "move")?])
            },
        }),
        // Rotation axis = plane origin + plane normal.
        Arc::new(FnComponent {
            type_name: "rotate",
            label: "Rotate",
            category: "Transform",
            inputs: || {
                vec![
                    geo_in(),
                    plane_in(),
                    PortSpec::item_default("angle", ValueKind::Number, Value::Number(0.0)),
                ]
            },
            outputs: geo_out,
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let plane = util::plane(inputs, 1, "plane")?;
                let angle = util::finite(inputs, 2, "angle")?;
                let m = Mat4::rotation_axis(plane.origin, plane.normal(), angle);
                Ok(vec![transform_geo(g, &m, "rotate")?])
            },
        }),
        Arc::new(FnComponent {
            type_name: "scale",
            label: "Scale",
            category: "Transform",
            inputs: || {
                vec![
                    geo_in(),
                    PortSpec::item_default("center", ValueKind::Vector, Value::Vector(Vec3::ZERO)),
                    PortSpec::item_default("factor", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: geo_out,
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let center = util::vector(inputs, 1, "center")?;
                let factor = util::finite(inputs, 2, "factor")?;
                let m = Mat4::scaling_uniform(center, factor);
                Ok(vec![transform_geo(g, &m, "scale")?])
            },
        }),
        Arc::new(FnComponent {
            type_name: "mirror",
            label: "Mirror",
            category: "Transform",
            inputs: || vec![geo_in(), plane_in()],
            outputs: geo_out,
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let plane = util::plane(inputs, 1, "plane")?;
                Ok(vec![transform_geo(g, &Mat4::mirror(&plane), "mirror")?])
            },
        }),
        Arc::new(FnComponent {
            type_name: "array_linear",
            label: "Linear Array",
            category: "Transform",
            inputs: || {
                vec![
                    geo_in(),
                    PortSpec::item_default("motion", ValueKind::Vector, Value::Vector(Vec3::X)),
                    PortSpec::item_default("count", ValueKind::Number, Value::Number(5.0)),
                ]
            },
            outputs: geo_out,
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let motion = util::vector(inputs, 1, "motion")?;
                let count = array_count(inputs, 2, g)?;
                let mut copies = Vec::with_capacity(count);
                for i in 0..count {
                    // The first item is the original and shares its geometry.
                    let copy = if i == 0 {
                        g.clone()
                    } else {
                        let offset = motion * i as f64;
                        if !offset.is_finite() {
                            return Err("array_linear: translation overflow".into());
                        }
                        transform_geo(g, &Mat4::translation(offset), "array_linear")?
                    };
                    util::finite_geometry(&copy)?;
                    copies.push(copy);
                }
                Ok(vec![Value::List(copies)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "array_polar",
            label: "Polar Array",
            category: "Transform",
            inputs: || {
                vec![
                    geo_in(),
                    plane_in(),
                    PortSpec::item_default("count", ValueKind::Number, Value::Number(6.0)),
                    PortSpec::item_default(
                        "angle",
                        ValueKind::Number,
                        Value::Number(std::f64::consts::TAU),
                    ),
                ]
            },
            outputs: geo_out,
            eval: |inputs, _| {
                let g = util::any(inputs, 0, "geometry")?;
                let plane = util::plane(inputs, 1, "plane")?;
                let count = array_count(inputs, 2, g)?;
                let angle = util::finite(inputs, 3, "angle")?;
                if angle.abs() > std::f64::consts::TAU + 1e-10 {
                    return Err("array_polar: angle must be within -2π..2π radians".into());
                }
                let axis = plane.normal();
                if !axis.is_finite() || axis.length() < 1e-12 {
                    return Err("array_polar: plane must have a nonzero normal".into());
                }
                // A full circle excludes the coincident final copy; a partial
                // sweep includes both ends, matching Rhino's fill-angle mode.
                let full_circle = (angle.abs() - std::f64::consts::TAU).abs() < 1e-10;
                let divisions = if full_circle {
                    count
                } else {
                    count.saturating_sub(1)
                }
                .max(1);
                let mut copies = Vec::with_capacity(count);
                for i in 0..count {
                    let copy = if i == 0 {
                        g.clone()
                    } else {
                        let rotation = angle * (i as f64 / divisions as f64);
                        let matrix = Mat4::rotation_axis(plane.origin, axis, rotation);
                        transform_geo(g, &matrix, "array_polar")?
                    };
                    util::finite_geometry(&copy)?;
                    copies.push(copy);
                }
                Ok(vec![Value::List(copies)])
            },
        }),
    ]
}
