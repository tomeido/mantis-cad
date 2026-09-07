//! Sets category: list construction and access.

use super::{util, FnComponent};
use crate::component::{Component, PortSpec};
use crate::value::{Value, ValueKind};
use std::sync::Arc;

fn list_default(name: &'static str, ty: ValueKind, values: Vec<Value>) -> PortSpec {
    let mut port = PortSpec::list(name, ty);
    port.default = Some(Value::List(values));
    port
}

fn numbers(inputs: &[Value], index: usize) -> Result<Vec<f64>, String> {
    util::list(inputs, index, "list")?
        .iter()
        .enumerate()
        .map(|(i, value)| {
            value
                .as_number()
                .filter(|n| n.is_finite())
                .ok_or_else(|| format!("list[{i}]: expected a finite number"))
        })
        .collect()
}

fn pattern(inputs: &[Value], index: usize) -> Result<Vec<bool>, String> {
    let list = util::list(inputs, index, "pattern")?;
    if list.is_empty() {
        return Err("pattern must not be empty".into());
    }
    list.iter()
        .enumerate()
        .map(|(i, value)| match value {
            Value::Bool(b) => Ok(*b),
            Value::Number(n) if n.is_finite() => Ok(*n != 0.0),
            _ => Err(format!("pattern[{i}]: expected Bool or a finite number")),
        })
        .collect()
}

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
        Arc::new(FnComponent {
            type_name: "reverse_list",
            label: "Reverse List",
            category: "Sets",
            inputs: || vec![PortSpec::list("list", ValueKind::Any)],
            outputs: || vec![PortSpec::item("list", ValueKind::Any)],
            eval: |inputs, _| {
                Ok(vec![Value::List(
                    util::list(inputs, 0, "list")?
                        .iter()
                        .rev()
                        .cloned()
                        .collect(),
                )])
            },
        }),
        Arc::new(FnComponent {
            type_name: "sort_list",
            label: "Sort List",
            category: "Sets",
            inputs: || vec![PortSpec::list("list", ValueKind::Number)],
            outputs: || {
                vec![
                    PortSpec::item("list", ValueKind::Number),
                    PortSpec::item("indices", ValueKind::Number),
                ]
            },
            eval: |inputs, _| {
                let numbers = numbers(inputs, 0)?;
                let mut indices: Vec<usize> = (0..numbers.len()).collect();
                // Stable sort preserves the original order of equal keys,
                // including -0/+0. Indices can reorder a companion list.
                indices.sort_by(|a, b| numbers[*a].partial_cmp(&numbers[*b]).unwrap());
                Ok(vec![
                    Value::List(indices.iter().map(|i| Value::Number(numbers[*i])).collect()),
                    Value::List(
                        indices
                            .into_iter()
                            .map(|i| Value::Number(i as f64))
                            .collect(),
                    ),
                ])
            },
        }),
        Arc::new(FnComponent {
            type_name: "shift_list",
            label: "Shift List",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::list("list", ValueKind::Any),
                    PortSpec::item_default("shift", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("wrap", ValueKind::Bool, Value::Bool(true)),
                ]
            },
            outputs: || vec![PortSpec::item("list", ValueKind::Any)],
            eval: |inputs, _| {
                let list = util::list(inputs, 0, "list")?;
                let shift = util::finite(inputs, 1, "shift")?.floor() as i64;
                let wrap = util::boolean(inputs, 2, "wrap")?;
                if list.is_empty() {
                    return Ok(vec![Value::List(Vec::new())]);
                }
                // Positive shift moves the first items to the end (left).
                let result = if wrap {
                    let n = shift.rem_euclid(list.len() as i64) as usize;
                    list[n..].iter().chain(&list[..n]).cloned().collect()
                } else if shift >= 0 {
                    list[(shift as u64).min(list.len() as u64) as usize..].to_vec()
                } else {
                    list[..list
                        .len()
                        .saturating_sub(shift.unsigned_abs().min(list.len() as u64) as usize)]
                        .to_vec()
                };
                Ok(vec![Value::List(result)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "cull_pattern",
            label: "Cull Pattern",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::list("list", ValueKind::Any),
                    list_default(
                        "pattern",
                        ValueKind::Bool,
                        vec![Value::Bool(false), Value::Bool(true)],
                    ),
                ]
            },
            outputs: || vec![PortSpec::item("list", ValueKind::Any)],
            eval: |inputs, _| {
                let list = util::list(inputs, 0, "list")?;
                let mask = pattern(inputs, 1)?;
                Ok(vec![Value::List(
                    list.iter()
                        .enumerate()
                        .filter(|(i, _)| !mask[i % mask.len()])
                        .map(|(_, value)| value.clone())
                        .collect(),
                )])
            },
        }),
        Arc::new(FnComponent {
            type_name: "dispatch",
            label: "Dispatch",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::list("list", ValueKind::Any),
                    list_default(
                        "pattern",
                        ValueKind::Bool,
                        vec![Value::Bool(true), Value::Bool(false)],
                    ),
                ]
            },
            outputs: || {
                vec![
                    PortSpec::item("a", ValueKind::Any),
                    PortSpec::item("b", ValueKind::Any),
                ]
            },
            eval: |inputs, _| {
                let list = util::list(inputs, 0, "list")?;
                let mask = pattern(inputs, 1)?;
                let mut a = Vec::new();
                let mut b = Vec::new();
                for (i, value) in list.iter().enumerate() {
                    if mask[i % mask.len()] {
                        a.push(value.clone());
                    } else {
                        b.push(value.clone());
                    }
                }
                Ok(vec![Value::List(a), Value::List(b)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "merge",
            label: "Merge",
            category: "Sets",
            inputs: || {
                vec![
                    list_default("a", ValueKind::Any, Vec::new()),
                    list_default("b", ValueKind::Any, Vec::new()),
                ]
            },
            outputs: || vec![PortSpec::item("list", ValueKind::Any)],
            eval: |inputs, _| {
                let a = util::list(inputs, 0, "a")?;
                let b = util::list(inputs, 1, "b")?;
                if a.len().saturating_add(b.len()) > util::MAX_COUNT {
                    return Err(format!("merge: output exceeds {} items", util::MAX_COUNT));
                }
                Ok(vec![Value::List(a.iter().chain(b).cloned().collect())])
            },
        }),
        Arc::new(FnComponent {
            type_name: "bounds",
            label: "Bounds",
            category: "Sets",
            inputs: || vec![PortSpec::list("list", ValueKind::Number)],
            outputs: || {
                vec![
                    PortSpec::item("min", ValueKind::Number),
                    PortSpec::item("max", ValueKind::Number),
                ]
            },
            eval: |inputs, _| {
                let numbers = numbers(inputs, 0)?;
                let Some(&first) = numbers.first() else {
                    return Err("bounds: list is empty".into());
                };
                let (min, max) = numbers
                    .iter()
                    .fold((first, first), |(lo, hi), n| (lo.min(*n), hi.max(*n)));
                Ok(vec![Value::Number(min), Value::Number(max)])
            },
        }),
        Arc::new(FnComponent {
            type_name: "random",
            label: "Random",
            category: "Sets",
            inputs: || {
                vec![
                    PortSpec::item_default("a", ValueKind::Number, Value::Number(0.0)),
                    PortSpec::item_default("b", ValueKind::Number, Value::Number(1.0)),
                    PortSpec::item_default("count", ValueKind::Number, Value::Number(10.0)),
                    PortSpec::item_default("seed", ValueKind::Number, Value::Number(1.0)),
                ]
            },
            outputs: || vec![PortSpec::item("numbers", ValueKind::Number)],
            eval: |inputs, _| {
                let a = util::finite(inputs, 0, "a")?;
                let b = util::finite(inputs, 1, "b")?;
                if a > b {
                    return Err("random: a must be <= b".into());
                }
                let count = util::count(inputs, 2, "count", util::MAX_COUNT)?;
                let mut state = util::finite(inputs, 3, "seed")?.floor() as i64 as u64;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    // SplitMix64: platform-independent, seeded and dependency-free.
                    state = state.wrapping_add(0x9e3779b97f4a7c15);
                    let mut bits = state;
                    bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
                    bits ^= bits >> 31;
                    let t = (bits >> 11) as f64 / ((1_u64 << 53) as f64);
                    let value = a * (1.0 - t) + b * t;
                    if !value.is_finite() {
                        return Err("random: domain arithmetic overflow".into());
                    }
                    values.push(Value::Number(value));
                }
                Ok(vec![Value::List(values)])
            },
        }),
    ]
}
