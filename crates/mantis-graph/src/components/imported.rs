use super::FnComponent;
use crate::{
    component::{Component, PortSpec},
    geometry::GeometryRecord,
    Value, ValueKind,
};
use std::sync::Arc;

pub(crate) fn all() -> Vec<Arc<dyn Component>> {
    vec![
        Arc::new(FnComponent {
            type_name: "imported_geometry",
            label: "CAD Geometry",
            category: "Params",
            inputs: Vec::new,
            outputs: || vec![PortSpec::item("geometry", ValueKind::Any)],
            eval: |_, params| {
                let data = params
                    .get("data")
                    .and_then(|v| v.as_text())
                    .ok_or("Import a CAD file or create a B-rep object first.")?;
                Ok(vec![GeometryRecord::from_json(data)?.value()])
            },
        }),
        Arc::new(FnComponent {
            type_name: "constant_list",
            label: "Stored List",
            category: "Params",
            inputs: Vec::new,
            outputs: || vec![PortSpec::list("values", ValueKind::Any)],
            eval: |_, params| {
                let text = params
                    .get("values")
                    .and_then(|p| p.as_text())
                    .unwrap_or("[]");
                if text.len() > 1024 * 1024 {
                    return Err("Stored list exceeds 1 MiB.".into());
                }
                let data: Vec<serde_json::Value> =
                    serde_json::from_str(text).map_err(|e| e.to_string())?;
                if data.len() > 10_000 {
                    return Err("Stored list exceeds 10,000 items.".into());
                }
                let values = data
                    .into_iter()
                    .map(|v| match v {
                        serde_json::Value::Number(n) => n
                            .as_f64()
                            .filter(|n| n.is_finite())
                            .map(Value::Number)
                            .ok_or_else(|| "Stored list number must be finite.".into()),
                        serde_json::Value::Bool(v) => Ok(Value::Bool(v)),
                        serde_json::Value::String(v) => Ok(Value::Text(v)),
                        _ => Err("Stored lists support only numbers, booleans and text.".into()),
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(vec![Value::List(values)])
            },
        }),
    ]
}
