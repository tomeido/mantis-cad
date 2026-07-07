//! Transform category: rigid/affine transforms of geometry values.
//!
//! Geometry variants supported: Vector (treated as a point), Plane, Curve,
//! Mesh. Anything else errors. The engine maps over lists on the `geometry`
//! port automatically (Item access).

use super::{util, FnComponent};
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
    ]
}
