//! Persistent parameter contracts shared by document editors and agents.
//!
//! Ports describe wired values; parameters describe the serialized node-local
//! values carried by `GraphOp::SetParam`. A missing parameter uses its component
//! fallback, so validation accepts partial/in-progress documents.

use crate::ParamValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Number,
    Bool,
    Text,
}

impl ParamKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Number => "Number",
            Self::Bool => "Bool",
            Self::Text => "Text",
        }
    }

    pub fn accepts(self, value: &ParamValue) -> bool {
        matches!(
            (self, value),
            (Self::Number, ParamValue::Number(_))
                | (Self::Bool, ParamValue::Bool(_))
                | (Self::Text, ParamValue::Text(_))
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub kind: ParamKind,
    /// `None` means the component supplies no stored fallback value.
    pub default: Option<ParamValue>,
    pub description: &'static str,
}

impl ParamSpec {
    pub fn new(
        name: &'static str,
        kind: ParamKind,
        default: Option<ParamValue>,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            kind,
            default,
            description,
        }
    }
}

/// Presentation parameters are part of every node's persistent contract.
pub fn common_parameters() -> Vec<ParamSpec> {
    vec![
        ParamSpec::new(
            "__preview",
            ParamKind::Bool,
            Some(ParamValue::Bool(true)),
            "Include geometric outputs in viewport/export preview",
        ),
        ParamSpec::new(
            "label",
            ParamKind::Text,
            None,
            "Optional display label; otherwise use the component label",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn every_component_has_unique_well_typed_parameters() {
        let registry = Registry::standard();
        for component in registry.iter() {
            let parameters = component.parameters();
            let mut names = BTreeSet::new();
            for spec in &parameters {
                assert!(
                    names.insert(spec.name),
                    "{}: duplicate {}",
                    component.type_name(),
                    spec.name
                );
                if let Some(default) = &spec.default {
                    assert!(
                        spec.kind.accepts(default),
                        "{}: {}",
                        component.type_name(),
                        spec.name
                    );
                }
            }
            assert!(names.contains("label"));
            assert!(names.contains("__preview"));
        }
    }

    #[test]
    fn validation_accepts_gui_metadata_and_rejects_typos_and_wrong_types() {
        let registry = Registry::standard();
        let params = BTreeMap::from([
            ("label".into(), ParamValue::Text("Radius".into())),
            ("value".into(), ParamValue::Number(5.0)),
        ]);
        registry
            .validate_parameters("number_slider", &params)
            .unwrap();
        let typo = BTreeMap::from([("lable".into(), ParamValue::Text("Radius".into()))]);
        assert_eq!(
            registry.validate_parameters("number_slider", &typo),
            Err("unknown parameter lable".into())
        );
        let wrong_type = BTreeMap::from([("label".into(), ParamValue::Bool(true))]);
        assert_eq!(
            registry.validate_parameters("number_slider", &wrong_type),
            Err("parameter label expects Text".into())
        );
    }
}
