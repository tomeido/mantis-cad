//! Component trait + registry.

use crate::parameters::{common_parameters, ParamSpec};
use crate::value::{ParamValue, Value, ValueKind};
use std::collections::BTreeMap;
use std::sync::Arc;

/// How a port consumes values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Engine auto-maps lists over this port (longest-list matching).
    Item,
    /// Port receives the whole list (non-list values are wrapped in [v]).
    List,
}

#[derive(Debug, Clone)]
pub struct PortSpec {
    pub name: &'static str,
    pub ty: ValueKind,
    pub access: Access,
    /// Used when the port has no wire. None -> Value::Null is passed
    /// (component may then error or have its own fallback).
    pub default: Option<Value>,
}

impl PortSpec {
    pub fn item(name: &'static str, ty: ValueKind) -> PortSpec {
        PortSpec {
            name,
            ty,
            access: Access::Item,
            default: None,
        }
    }
    pub fn item_default(name: &'static str, ty: ValueKind, default: Value) -> PortSpec {
        PortSpec {
            name,
            ty,
            access: Access::Item,
            default: Some(default),
        }
    }
    pub fn list(name: &'static str, ty: ValueKind) -> PortSpec {
        PortSpec {
            name,
            ty,
            access: Access::List,
            default: None,
        }
    }
}

/// A stateless node type. `type_name` is recorded on-chain — NEVER rename.
pub trait Component: Send + Sync {
    /// Stable id, lowercase snake ("number_slider", "extrude"...).
    fn type_name(&self) -> &'static str;
    /// Display name for the UI ("Number Slider").
    fn label(&self) -> &'static str;
    /// Palette category ("Params", "Maths", "Vector", "Curve", "Surface",
    /// "Transform", "Sets", "Analysis").
    fn category(&self) -> &'static str;
    fn inputs(&self) -> Vec<PortSpec>;
    fn outputs(&self) -> Vec<PortSpec>;
    /// Persistent node-local values, including shared presentation metadata.
    /// Custom components can extend `common_parameters()` with their own keys.
    fn parameters(&self) -> Vec<ParamSpec> {
        common_parameters()
    }
    /// Evaluate. `inputs.len() == self.inputs().len()` (engine fills defaults /
    /// Null). For Item ports the engine has already unwrapped lists — eval sees
    /// scalars there. Must be pure & deterministic.
    fn eval(
        &self,
        inputs: &[Value],
        params: &BTreeMap<String, ParamValue>,
    ) -> Result<Vec<Value>, String>;
}

#[derive(Clone)]
pub struct Registry {
    map: BTreeMap<&'static str, Arc<dyn Component>>,
}

impl Registry {
    pub fn empty() -> Registry {
        Registry {
            map: BTreeMap::new(),
        }
    }
    /// All built-in components (see components/).
    pub fn standard() -> Registry {
        let mut r = Registry::empty();
        for c in crate::components::all() {
            r.register(c);
        }
        r
    }
    pub fn register(&mut self, c: Arc<dyn Component>) {
        self.map.insert(c.type_name(), c);
    }
    pub fn get(&self, type_name: &str) -> Option<&Arc<dyn Component>> {
        self.map.get(type_name)
    }
    pub fn parameter_schema(&self, type_name: &str) -> Option<Vec<ParamSpec>> {
        self.get(type_name).map(|component| component.parameters())
    }
    /// Strict key/type checks use the same contract exposed by component
    /// discovery. Geometry/list payload contents are validated on evaluation.
    pub fn validate_parameters(
        &self,
        type_name: &str,
        params: &BTreeMap<String, ParamValue>,
    ) -> Result<(), String> {
        let schema = self
            .parameter_schema(type_name)
            .ok_or_else(|| format!("unknown component {type_name}"))?;
        for (key, value) in params {
            let spec = schema
                .iter()
                .find(|spec| spec.name == key)
                .ok_or_else(|| format!("unknown parameter {key}"))?;
            if !spec.kind.accepts(value) {
                return Err(format!("parameter {key} expects {}", spec.kind.name()));
            }
        }
        Ok(())
    }
    /// Deterministic (name-sorted) iteration — used by the palette UI.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Component>> {
        self.map.values()
    }
}
