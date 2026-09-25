//! Bounded local curve analysis and tangent-controlled cubic blending.

use super::{util, FnComponent};
use crate::{Component, PortSpec, Value, ValueKind};
use mantis_kernel::{ContinuityTolerance, Curve};
use std::sync::Arc;

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "curve_comb",
            label: "Curvature Comb",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::item("curve", ValueKind::Curve),
                    PortSpec::item_default("samples", ValueKind::Number, Value::Number(32.0)),
                    PortSpec::item_default("scale", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: || {
                vec![
                    PortSpec::list("comb", ValueKind::Curve),
                    PortSpec::list("curvature", ValueKind::Number),
                    PortSpec::list("valid", ValueKind::Bool),
                ]
            },
            eval: |inputs, _| {
                let curve = util::curve(inputs, 0, "curve")?;
                let count = util::finite(inputs, 1, "samples")?;
                if !(2.0..=mantis_kernel::curve_quality::MAX_COMB_SAMPLES as f64).contains(&count)
                    || count.fract() != 0.0
                {
                    return Err("curve_comb: samples must be an integer in 2..2048".into());
                }
                let scale = util::finite(inputs, 2, "scale")?;
                if scale < 0.0 {
                    return Err("curve_comb: scale must be nonnegative".into());
                }
                let samples = curve.curvature_comb(count as usize)?;
                let mut comb = Vec::with_capacity(samples.len());
                let mut curvature = Vec::with_capacity(samples.len());
                let mut valid = Vec::with_capacity(samples.len());
                for sample in samples {
                    let end = sample.point + sample.curvature * scale;
                    if !end.is_finite() {
                        return Err("curve_comb: comb scale overflows coordinates".into());
                    }
                    comb.push(Value::Curve(Arc::new(Curve::Line {
                        a: sample.point,
                        b: end,
                    })));
                    curvature.push(Value::Number(
                        sample
                            .curvature
                            .x
                            .hypot(sample.curvature.y)
                            .hypot(sample.curvature.z),
                    ));
                    valid.push(Value::Bool(sample.valid));
                }
                Ok(vec![
                    Value::List(comb),
                    Value::List(curvature),
                    Value::List(valid),
                ])
            },
        }),
        Arc::new(FnComponent {
            type_name: "curve_continuity",
            label: "Curve Continuity",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::item("a", ValueKind::Curve),
                    PortSpec::item("b", ValueKind::Curve),
                    PortSpec::item_default("gap_tol", ValueKind::Number, Value::Number(0.001)),
                    PortSpec::item_default("angle_tol", ValueKind::Number, Value::Number(0.01)),
                    PortSpec::item_default(
                        "curvature_tol",
                        ValueKind::Number,
                        Value::Number(0.001),
                    ),
                ]
            },
            outputs: || {
                vec![
                    PortSpec::item("gap", ValueKind::Number),
                    PortSpec::item("angle", ValueKind::Number),
                    PortSpec::item("curvature_delta", ValueKind::Number),
                    PortSpec::item("g0", ValueKind::Bool),
                    PortSpec::item("g1", ValueKind::Bool),
                    PortSpec::item("g2", ValueKind::Bool),
                ]
            },
            eval: |inputs, _| {
                let a = util::curve(inputs, 0, "a")?;
                let b = util::curve(inputs, 1, "b")?;
                let report = a.continuity_to(
                    &b,
                    ContinuityTolerance {
                        distance: util::finite(inputs, 2, "distance_tolerance")?,
                        angle: util::finite(inputs, 3, "angle_tolerance")?,
                        curvature: util::finite(inputs, 4, "curvature_tolerance")?,
                    },
                )?;
                Ok(vec![
                    Value::Number(report.gap),
                    Value::Number(report.angle),
                    Value::Number(report.curvature_delta),
                    Value::Bool(report.g0),
                    Value::Bool(report.g1),
                    Value::Bool(report.g2),
                ])
            },
        }),
        Arc::new(FnComponent {
            type_name: "blend_curve",
            label: "Tangent Blend Curve",
            category: "Curve",
            inputs: || {
                vec![
                    PortSpec::item("a", ValueKind::Curve),
                    PortSpec::item("b", ValueKind::Curve),
                    PortSpec::item_default("tension", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: || vec![PortSpec::item("curve", ValueKind::Curve)],
            eval: |inputs, _| {
                let a = util::curve(inputs, 0, "a")?;
                let b = util::curve(inputs, 1, "b")?;
                let tension = util::finite(inputs, 2, "tension")?;
                Ok(vec![Value::Curve(Arc::new(
                    a.tangent_blend_to(&b, tension)?,
                ))])
            },
        }),
    ]
}
