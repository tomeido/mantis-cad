//! Maths category. add/subtract/multiply are polymorphic over
//! Number/Vector; everything else is plain f64 arithmetic.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use std::sync::Arc;

fn n2() -> Vec<PortSpec> {
    vec![
        PortSpec::item("a", ValueKind::Number),
        PortSpec::item("b", ValueKind::Number),
    ]
}

fn any2() -> Vec<PortSpec> {
    vec![
        PortSpec::item("a", ValueKind::Any),
        PortSpec::item("b", ValueKind::Any),
    ]
}

fn num_out() -> Vec<PortSpec> {
    vec![PortSpec::item("result", ValueKind::Number)]
}

fn any_out() -> Vec<PortSpec> {
    vec![PortSpec::item("result", ValueKind::Any)]
}

fn x_in() -> Vec<PortSpec> {
    vec![PortSpec::item("x", ValueKind::Number)]
}

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "add",
            label: "Add",
            category: "Maths",
            inputs: any2,
            outputs: any_out,
            eval: |inputs, _| {
                let a = util::any(inputs, 0, "a")?;
                let b = util::any(inputs, 1, "b")?;
                match (a, b) {
                    (Value::Number(x), Value::Number(y)) => Ok(vec![Value::Number(x + y)]),
                    (Value::Vector(x), Value::Vector(y)) => Ok(vec![Value::Vector(*x + *y)]),
                    _ => Err(format!(
                        "add: cannot add {} and {}",
                        a.describe(),
                        b.describe()
                    )),
                }
            },
        }),
        Arc::new(FnComponent {
            type_name: "subtract",
            label: "Subtract",
            category: "Maths",
            inputs: any2,
            outputs: any_out,
            eval: |inputs, _| {
                let a = util::any(inputs, 0, "a")?;
                let b = util::any(inputs, 1, "b")?;
                match (a, b) {
                    (Value::Number(x), Value::Number(y)) => Ok(vec![Value::Number(x - y)]),
                    (Value::Vector(x), Value::Vector(y)) => Ok(vec![Value::Vector(*x - *y)]),
                    _ => Err(format!(
                        "subtract: cannot subtract {} from {}",
                        b.describe(),
                        a.describe()
                    )),
                }
            },
        }),
        Arc::new(FnComponent {
            type_name: "multiply",
            label: "Multiply",
            category: "Maths",
            inputs: any2,
            outputs: any_out,
            eval: |inputs, _| {
                let a = util::any(inputs, 0, "a")?;
                let b = util::any(inputs, 1, "b")?;
                match (a, b) {
                    (Value::Number(x), Value::Number(y)) => Ok(vec![Value::Number(x * y)]),
                    (Value::Vector(v), Value::Number(s)) => Ok(vec![Value::Vector(*v * *s)]),
                    (Value::Number(s), Value::Vector(v)) => Ok(vec![Value::Vector(*v * *s)]),
                    _ => Err(format!(
                        "multiply: cannot multiply {} by {} (use dot/cross for two vectors)",
                        a.describe(),
                        b.describe()
                    )),
                }
            },
        }),
        Arc::new(FnComponent {
            type_name: "divide",
            label: "Divide",
            category: "Maths",
            inputs: n2,
            outputs: num_out,
            eval: |inputs, _| {
                let a = util::num(inputs, 0, "a")?;
                let b = util::num(inputs, 1, "b")?;
                if b == 0.0 {
                    return Err("divide: division by zero".into());
                }
                Ok(vec![Value::Number(a / b)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "power",
            label: "Power",
            category: "Maths",
            inputs: n2,
            outputs: num_out,
            eval: |inputs, _| {
                let a = util::num(inputs, 0, "a")?;
                let b = util::num(inputs, 1, "b")?;
                let r = a.powf(b);
                if r.is_finite() {
                    Ok(vec![Value::Number(r)])
                } else {
                    Err(format!("power: {a}^{b} is not a finite number"))
                }
            },
        }),
        // Floored (Euclidean) modulo: result is always in [0, |b|).
        Arc::new(FnComponent {
            type_name: "modulo",
            label: "Modulo",
            category: "Maths",
            inputs: n2,
            outputs: num_out,
            eval: |inputs, _| {
                let a = util::num(inputs, 0, "a")?;
                let b = util::num(inputs, 1, "b")?;
                if b == 0.0 {
                    return Err("modulo: division by zero".into());
                }
                Ok(vec![Value::Number(a.rem_euclid(b))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "negate",
            label: "Negate",
            category: "Maths",
            inputs: x_in,
            outputs: num_out,
            eval: |inputs, _| Ok(vec![Value::Number(-util::num(inputs, 0, "x")?)]),
        }),
        Arc::new(FnComponent {
            type_name: "sin",
            label: "Sine",
            category: "Maths",
            inputs: x_in,
            outputs: num_out,
            eval: |inputs, _| Ok(vec![Value::Number(util::num(inputs, 0, "x")?.sin())]),
        }),
        Arc::new(FnComponent {
            type_name: "cos",
            label: "Cosine",
            category: "Maths",
            inputs: x_in,
            outputs: num_out,
            eval: |inputs, _| Ok(vec![Value::Number(util::num(inputs, 0, "x")?.cos())]),
        }),
        Arc::new(FnComponent {
            type_name: "sqrt",
            label: "Square Root",
            category: "Maths",
            inputs: x_in,
            outputs: num_out,
            eval: |inputs, _| {
                let x = util::num(inputs, 0, "x")?;
                if x < 0.0 {
                    return Err(format!("sqrt: negative input {x}"));
                }
                Ok(vec![Value::Number(x.sqrt())])
            },
        }),
        Arc::new(FnComponent {
            type_name: "abs",
            label: "Absolute",
            category: "Maths",
            inputs: x_in,
            outputs: num_out,
            eval: |inputs, _| Ok(vec![Value::Number(util::num(inputs, 0, "x")?.abs())]),
        }),
        Arc::new(FnComponent {
            type_name: "min",
            label: "Minimum",
            category: "Maths",
            inputs: n2,
            outputs: num_out,
            eval: |inputs, _| {
                let a = util::num(inputs, 0, "a")?;
                let b = util::num(inputs, 1, "b")?;
                Ok(vec![Value::Number(a.min(b))])
            },
        }),
        Arc::new(FnComponent {
            type_name: "max",
            label: "Maximum",
            category: "Maths",
            inputs: n2,
            outputs: num_out,
            eval: |inputs, _| {
                let a = util::num(inputs, 0, "a")?;
                let b = util::num(inputs, 1, "b")?;
                Ok(vec![Value::Number(a.max(b))])
            },
        }),
        // Linear remap of `value` from source domain [s0,s1] to target [t0,t1].
        Arc::new(FnComponent {
            type_name: "remap",
            label: "Remap Numbers",
            category: "Maths",
            inputs: || {
                vec![
                    PortSpec::item("value", ValueKind::Number),
                    PortSpec::item_default("s0", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("s1", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("t0", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("t1", ValueKind::Number, Value::Number(10.0)),
                ]
            },
            outputs: num_out,
            eval: |inputs, _| {
                let v = util::num(inputs, 0, "value")?;
                let s0 = util::num(inputs, 1, "s0")?;
                let s1 = util::num(inputs, 2, "s1")?;
                let t0 = util::num(inputs, 3, "t0")?;
                let t1 = util::num(inputs, 4, "t1")?;
                let d = s1 - s0;
                if d.abs() < 1e-12 {
                    return Err("remap: source domain is degenerate (s0 == s1)".into());
                }
                Ok(vec![Value::Number(t0 + (v - s0) / d * (t1 - t0))])
            },
        }),
    ]
}
