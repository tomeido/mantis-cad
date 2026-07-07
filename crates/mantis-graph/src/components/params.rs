//! Params category: sliders, toggles, panels, primitive constructors.

use super::{util, FnComponent};
use crate::component::{Access, Component, PortSpec};
use crate::value::{ParamValue, Value, ValueKind};
use mantis_kernel::Vec3;
use std::collections::BTreeMap;
use std::sync::Arc;

fn pnum(params: &BTreeMap<String, ParamValue>, key: &str, default: f64) -> f64 {
    params.get(key).and_then(|p| p.as_number()).unwrap_or(default)
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        // Params: min(0), max(10), step(0 = continuous), value(5), label.
        // Output = value clamped to [min,max], snapped to step when step > 0.
        Arc::new(FnComponent {
            type_name: "number_slider",
            label: "Number Slider",
            category: "Params",
            inputs: Vec::new,
            outputs: || vec![PortSpec::item("value", ValueKind::Number)],
            eval: |_, params| {
                let min = pnum(params, "min", 0.0);
                let max = pnum(params, "max", 10.0);
                let step = pnum(params, "step", 0.0);
                let mut v = pnum(params, "value", 5.0);
                let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
                if step > 0.0 && step.is_finite() && v.is_finite() {
                    v = lo + ((v - lo) / step).round() * step;
                }
                // f64::max/min (not clamp): never panics, absorbs NaN.
                v = v.max(lo).min(hi);
                Ok(vec![Value::Number(v)])
            },
        }),
        // Param: value(Bool, default false).
        Arc::new(FnComponent {
            type_name: "bool_toggle",
            label: "Boolean Toggle",
            category: "Params",
            inputs: Vec::new,
            outputs: || vec![PortSpec::item("value", ValueKind::Bool)],
            eval: |_, params| {
                let v = params.get("value").and_then(|p| p.as_bool()).unwrap_or(false);
                Ok(vec![Value::Bool(v)])
            },
        }),
        // Display-only: the UI shows the wired input value, or the "text"
        // param when unwired. No outputs; eval is a no-op.
        Arc::new(FnComponent {
            type_name: "panel",
            label: "Panel",
            category: "Params",
            inputs: || {
                vec![PortSpec {
                    name: "value",
                    ty: ValueKind::Any,
                    access: Access::List,
                    // Null default so an unwired panel is not an error.
                    default: Some(Value::Null),
                }]
            },
            outputs: Vec::new,
            eval: |_, _| Ok(Vec::new()),
        }),
        Arc::new(FnComponent {
            type_name: "point_xyz",
            label: "Point XYZ",
            category: "Params",
            inputs: || {
                vec![
                    PortSpec::item_default("x", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("y", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("z", ValueKind::Number, Value::Number(0.0)),
                ]
            },
            outputs: || vec![PortSpec::item("point", ValueKind::Vector)],
            eval: |inputs, _| {
                let x = util::num(inputs, 0, "x")?;
                let y = util::num(inputs, 1, "y")?;
                let z = util::num(inputs, 2, "z")?;
                Ok(vec![Value::Vector(Vec3::new(x, y, z))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "pi_const",
            label: "Pi",
            category: "Params",
            inputs: Vec::new,
            outputs: || vec![PortSpec::item("value", ValueKind::Number)],
            eval: |_, _| Ok(vec![Value::Number(std::f64::consts::PI)]),
        }),
    ]
}
