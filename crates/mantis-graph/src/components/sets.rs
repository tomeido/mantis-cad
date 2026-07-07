//! Sets category: list construction and access.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use std::sync::Arc;

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        // Arithmetic series: start, start+step, ... (count values).
        Arc::new(FnComponent {
            type_name: "series",
            label: "Series",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::item_default("start", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("step", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("count", ValueKind::Number, Value::Number(10.0)),
                ]
            },
            outputs: || vec![PortSpec::item("series", ValueKind::Number)],
            eval: |inputs, _| {
                let start = util::finite(inputs, 0, "start")?;
                let step = util::finite(inputs, 1, "step")?;
                let n = util::count(inputs, 2, "count", util::MAX_COUNT)?;
                let vals = (0..n)
                    .map(|i| Value::Number(start + step * i as f64))
                    .collect();
                Ok(vec![Value::List(vals)])
            },
        }),
        // steps+1 numbers evenly spanning [a, b].
        Arc::new(FnComponent {
            type_name: "range",
            label: "Range",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::item_default("a", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("b", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("steps", ValueKind::Number, Value::Number(10.0)),
                ]
            },
            outputs: || vec![PortSpec::item("range", ValueKind::Number)],
            eval: |inputs, _| {
                let a = util::finite(inputs, 0, "a")?;
                let b = util::finite(inputs, 1, "b")?;
                let steps = util::count(inputs, 2, "steps", util::MAX_COUNT)?.max(1);
                let vals = (0..=steps)
                    .map(|i| Value::Number(a + (b - a) * (i as f64 / steps as f64)))
                    .collect();
                Ok(vec![Value::List(vals)])
            },
        }),
        // list[index]; `wrap` treats the index modulo the list length
        // (negative indices allowed when wrapping).
        Arc::new(FnComponent {
            type_name: "list_item",
            label: "List Item",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::list("list", ValueKind::Any),
                    PortSpec::item_default("index", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("wrap", ValueKind::Bool, Value::Bool(false)),
                ]
            },
            outputs: || vec![PortSpec::item("item", ValueKind::Any)],
            eval: |inputs, _| {
                let l = util::list(inputs, 0, "list")?;
                let idx = util::finite(inputs, 1, "index")?;
                let wrap = util::boolean(inputs, 2, "wrap")?;
                if l.is_empty() {
                    return Err("list_item: list is empty".into());
                }
                let i = idx.floor() as i64; // saturating cast
                let len = l.len() as i64;
                let i = if wrap {
                    ((i % len) + len) % len
                } else if i < 0 || i >= len {
                    return Err(format!(
                        "list_item: index {i} out of range 0..{len} (enable wrap?)"
                    ));
                } else {
                    i
                };
                Ok(vec![l[i as usize].clone()])
            },
        }),
        Arc::new(FnComponent {
            type_name: "list_length",
            label: "List Length",
            category: "Sets",
            inputs: || vec![PortSpec::list("list", ValueKind::Any)],
            outputs: || vec![PortSpec::item("length", ValueKind::Number)],
            eval: |inputs, _| {
                let l = util::list(inputs, 0, "list")?;
                Ok(vec![Value::Number(l.len() as f64)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "repeat",
            label: "Repeat",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::item("item", ValueKind::Any),
                    PortSpec::item_default("count", ValueKind::Number, Value::Number(10.0)),
                ]
            },
            outputs: || vec![PortSpec::item("list", ValueKind::Any)],
            eval: |inputs, _| {
                let item = util::any(inputs, 0, "item")?;
                let n = util::count(inputs, 1, "count", util::MAX_COUNT)?;
                Ok(vec![Value::List(vec![item.clone(); n])])
            },
        }),
    ]
}
