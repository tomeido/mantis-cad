//! Curve category: curve construction and interrogation.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use mantis_kernel::{Curve, NurbsCurve, Plane};
use std::sync::Arc;

fn world_xy_default() -> PortSpec {
    PortSpec::item_default("plane", ValueKind::Plane, Value::Plane(Plane::world_xy()))
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "line",
            label: "Line",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::item("a", ValueKind::Vector),
                    PortSpec::item("b", ValueKind::Vector),
                ]
            },
            outputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            eval: |inputs, _| {
                let a = util::vector(inputs, 0, "a")?;
                let b = util::vector(inputs, 1, "b")?;
                Ok(vec![Value::Curve(Arc::new(Curve::Line { a, b }))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "polyline",
            label: "Polyline",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::list("points", ValueKind::Vector),
                    PortSpec::item_default("closed", ValueKind::Bool, Value::Bool(false)),
                ]
            },
            outputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            eval: |inputs, _| {
                let points = util::vectors(inputs, 0, "points")?;
                let closed = util::boolean(inputs, 1, "closed")?;
                if points.len() < 2 {
                    return Err(format!(
                        "polyline: need at least 2 points (got {})",
                        points.len()
                    ));
                }
                Ok(vec![Value::Curve(Arc::new(Curve::Polyline {
                    points,
                    closed,
                }))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "circle",
            label: "Circle",
            category: "Curve",
            inputs: || {
                vec![
                    world_xy_default(),
                    PortSpec::item_default("radius", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            eval: |inputs, _| {
                let plane = util::plane(inputs, 0, "plane")?;
                let radius = util::positive(inputs, 1, "radius")?;
                Ok(vec![Value::Curve(Arc::new(Curve::Circle { plane, radius }))])
            },
        }),
        // Angles in radians, measured in-plane from x_axis toward y_axis.
        Arc::new(FnComponent {
            type_name: "arc",
            label: "Arc",
            category: "Curve",
            inputs: || {
                vec![
                    world_xy_default(),
                    PortSpec::item_default("radius", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("a0", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default(
                        "a1",
                        ValueKind::Number,
                        Value::Number(std::f64::consts::PI),
                    ),
                ]
            },
            outputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            eval: |inputs, _| {
                let plane = util::plane(inputs, 0, "plane")?;
                let radius = util::positive(inputs, 1, "radius")?;
                let a0 = util::finite(inputs, 2, "a0")?;
                let a1 = util::finite(inputs, 3, "a1")?;
                Ok(vec![Value::Curve(Arc::new(Curve::Arc {
                    plane,
                    radius,
                    start_angle: a0,
                    end_angle: a1,
                }))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "nurbs_curve",
            label: "Nurbs Curve",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::list("points", ValueKind::Vector),
                    PortSpec::item_default("degree", ValueKind::Number, Value::Number(3.0)),
                    PortSpec::item_default("closed", ValueKind::Bool, Value::Bool(false)),
                ]
            },
            outputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            eval: |inputs, _| {
                let points = util::vectors(inputs, 0, "points")?;
                let degree = util::count(inputs, 1, "degree", 25)?.max(1);
                let closed = util::boolean(inputs, 2, "closed")?;
                match NurbsCurve::from_points(&points, degree, closed) {
                    Some(n) => Ok(vec![Value::Curve(Arc::new(Curve::Nurbs(n)))]),
                    None => Err(format!(
                        "nurbs_curve: need at least 2 points (got {})",
                        points.len()
                    )),
                }
            },
        }),
        // n segments -> n+1 points for open curves, n points for closed.
        Arc::new(FnComponent {
            type_name: "divide_curve",
            label: "Divide Curve",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    PortSpec::item_default("n", ValueKind::Number, Value::Number(10.0)),
                ]
            },
            outputs: || vec![PortSpec::item("points", ValueKind::Vector)],
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                let n = util::count(inputs, 1, "n", util::MAX_COUNT)?.max(1);
                let pts = c.divide(n).into_iter().map(Value::Vector).collect();
                Ok(vec![Value::List(pts)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "eval_curve",
            label: "Evaluate Curve",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    PortSpec::item_default("t", ValueKind::Number, Value::Number(0.5)),
                ]
            },
            outputs: || {
                vec![
                    PortSpec::item("point", ValueKind::Vector),
                    PortSpec::item("tangent", ValueKind::Vector),
                ]
            },
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                let t = util::finite(inputs, 1, "t")?.max(0.0).min(1.0);
                Ok(vec![
                    Value::Vector(c.point_at(t)),
                    Value::Vector(c.tangent_at(t)),
                ])
            },
        }),
        Arc::new(FnComponent {
            type_name: "curve_length",
            label: "Curve Length",
            category: "Curve",
            inputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            outputs: || vec![PortSpec::item("length", ValueKind::Number)],
            eval: |inputs, _| {
                let c = util::curve(inputs, 0, "curve")?;
                Ok(vec![Value::Number(c.length())])
            },
        }),
    ]
}
