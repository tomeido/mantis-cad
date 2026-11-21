//! Fingerprint the declared registry contract, independently of its UI labels.
//!
//! Encoding v1 uses explicit one-byte enum tags, little-endian u64 lengths and
//! degree, length-prefixed UTF-8 strings, u32 mesh indices, and exact f64 bits.
//! Components are name-sorted; ports and all value arrays retain their order.
//! Changes to this encoding require a new domain/version, not new enum ordinals.

use mantis_graph::{Access, PortSpec, Registry, Value, ValueKind};
use mantis_kernel::{Curve, Plane, Vec3};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Provenance of the current evaluator used for a derived comparison report.
/// This is not part of the signed chain or an archival replay guarantee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatorProvenance {
    /// SHA-256 of type names and ordered port names, types, access and defaults.
    /// Excludes UI labels/categories, evaluation code and parameter semantics.
    pub registry_contract_hash: String,
    /// Compile-time MANTIS_GIT_SHA, or "unknown" when unavailable or empty.
    /// The registry hash alone cannot identify kernel/evaluator implementation.
    pub build_id: String,
}

impl EvaluatorProvenance {
    pub(crate) fn for_registry(registry: &Registry) -> Self {
        Self {
            registry_contract_hash: registry_contract_hash(registry),
            build_id: build_id(option_env!("MANTIS_GIT_SHA")),
        }
    }
}

fn build_id(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn registry_contract_hash(registry: &Registry) -> String {
    let mut hash = ContractHash(Sha256::new());
    hash.0.update(b"mantis-registry-contract-v1");
    hash.len(registry.iter().count());
    for component in registry.iter() {
        hash.text(component.type_name());
        hash.ports(&component.inputs());
        hash.ports(&component.outputs());
    }
    super::finish_hex(hash.0)
}

struct ContractHash(Sha256);

impl ContractHash {
    fn tag(&mut self, tag: u8) {
        self.0.update([tag]);
    }

    fn len(&mut self, value: usize) {
        self.0.update((value as u64).to_le_bytes());
    }

    fn text(&mut self, value: &str) {
        self.len(value.len());
        self.0.update(value.as_bytes());
    }

    fn number(&mut self, value: f64) {
        // Defaults are a contract: preserve signed zero and non-finite bits.
        self.0.update(value.to_bits().to_le_bytes());
    }

    fn vector(&mut self, value: Vec3) {
        self.number(value.x);
        self.number(value.y);
        self.number(value.z);
    }

    fn vectors(&mut self, values: &[Vec3]) {
        self.len(values.len());
        for value in values {
            self.vector(*value);
        }
    }

    fn plane(&mut self, value: Plane) {
        self.vector(value.origin);
        self.vector(value.x_axis);
        self.vector(value.y_axis);
    }

    fn ports(&mut self, ports: &[PortSpec]) {
        self.len(ports.len());
        for port in ports {
            self.text(port.name);
            self.tag(match port.ty {
                ValueKind::Any => 0,
                ValueKind::Number => 1,
                ValueKind::Bool => 2,
                ValueKind::Text => 3,
                ValueKind::Vector => 4,
                ValueKind::Plane => 5,
                ValueKind::Curve => 6,
                ValueKind::Mesh => 7,
            });
            self.tag(match port.access {
                Access::Item => 0,
                Access::List => 1,
            });
            match &port.default {
                None => self.tag(0),
                Some(value) => {
                    self.tag(1);
                    self.value(value);
                }
            }
        }
    }

    fn value(&mut self, value: &Value) {
        match value {
            Value::Null => self.tag(0),
            Value::Number(value) => {
                self.tag(1);
                self.number(*value);
            }
            Value::Bool(value) => {
                self.tag(2);
                self.tag(u8::from(*value));
            }
            Value::Text(value) => {
                self.tag(3);
                self.text(value);
            }
            Value::Vector(value) => {
                self.tag(4);
                self.vector(*value);
            }
            Value::Plane(value) => {
                self.tag(5);
                self.plane(*value);
            }
            Value::Curve(value) => {
                self.tag(6);
                self.curve(value);
            }
            Value::Mesh(value) => {
                self.tag(7);
                self.vectors(&value.positions);
                self.vectors(&value.normals);
                self.len(value.indices.len());
                for triangle in &value.indices {
                    for index in triangle {
                        self.0.update(index.to_le_bytes());
                    }
                }
            }
            Value::List(values) => {
                self.tag(8);
                self.len(values.len());
                for value in values {
                    self.value(value);
                }
            }
        }
    }

    fn curve(&mut self, curve: &Curve) {
        match curve {
            Curve::Line { a, b } => {
                self.tag(0);
                self.vector(*a);
                self.vector(*b);
            }
            Curve::Polyline { points, closed } => {
                self.tag(1);
                self.vectors(points);
                self.tag(u8::from(*closed));
            }
            Curve::Circle { plane, radius } => {
                self.tag(2);
                self.plane(*plane);
                self.number(*radius);
            }
            Curve::Arc {
                plane,
                radius,
                start_angle,
                end_angle,
            } => {
                self.tag(3);
                self.plane(*plane);
                self.number(*radius);
                self.number(*start_angle);
                self.number(*end_angle);
            }
            Curve::Nurbs(value) => {
                self.tag(4);
                self.len(value.degree);
                self.vectors(&value.control_points);
                self.len(value.weights.len());
                for weight in &value.weights {
                    self.number(*weight);
                }
                self.len(value.knots.len());
                for knot in &value.knots {
                    self.number(*knot);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_graph::{Component, ParamValue};
    use mantis_kernel::Mesh;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[derive(Clone)]
    struct Fixture {
        name: &'static str,
        label: &'static str,
        category: &'static str,
        inputs: Vec<PortSpec>,
        outputs: Vec<PortSpec>,
    }

    impl Component for Fixture {
        fn type_name(&self) -> &'static str {
            self.name
        }
        fn label(&self) -> &'static str {
            self.label
        }
        fn category(&self) -> &'static str {
            self.category
        }
        fn inputs(&self) -> Vec<PortSpec> {
            self.inputs.clone()
        }
        fn outputs(&self) -> Vec<PortSpec> {
            self.outputs.clone()
        }
        fn eval(
            &self,
            _: &[Value],
            _: &BTreeMap<String, ParamValue>,
        ) -> Result<Vec<Value>, String> {
            Ok(Vec::new())
        }
    }

    fn fixture() -> Fixture {
        Fixture {
            name: "fixture",
            label: "Fixture",
            category: "Tests",
            inputs: vec![PortSpec::item_default(
                "input",
                ValueKind::Number,
                Value::Number(1.25),
            )],
            outputs: vec![PortSpec::item("output", ValueKind::Vector)],
        }
    }

    fn hash(components: Vec<Fixture>) -> String {
        let mut registry = Registry::empty();
        for component in components {
            registry.register(Arc::new(component));
        }
        registry_contract_hash(&registry)
    }

    #[test]
    fn contract_hash_is_stable_and_registration_order_independent() {
        let first = fixture();
        let mut second = fixture();
        second.name = "second";
        assert_eq!(
            hash(vec![first.clone(), second.clone()]),
            hash(vec![second, first.clone()])
        );
        assert_eq!(
            hash(vec![first]),
            "dcbf09c08dae12a0f00bee7b668aaa4c0c3f3927c2208930ff94fa22af0d54fd"
        );
        assert_eq!(
            registry_contract_hash(&Registry::standard()),
            registry_contract_hash(&Registry::standard())
        );
    }

    #[test]
    fn contract_hash_tracks_ports_but_excludes_ui_metadata() {
        let base = fixture();
        let expected = hash(vec![base.clone()]);
        let mut presentation = base.clone();
        presentation.label = "Translated label";
        presentation.category = "Another category";
        assert_eq!(expected, hash(vec![presentation]));

        let mut variants = Vec::new();
        let mut changed = base.clone();
        changed.name = "renamed";
        variants.push(changed);
        let mut changed = base.clone();
        changed.inputs[0].name = "renamed";
        variants.push(changed);
        let mut changed = base.clone();
        changed.inputs[0].ty = ValueKind::Any;
        variants.push(changed);
        let mut changed = base.clone();
        changed.inputs[0].access = Access::List;
        variants.push(changed);
        let mut changed = base.clone();
        changed.inputs[0].default = None;
        variants.push(changed);
        let mut changed = base.clone();
        changed.inputs[0].default = Some(Value::Number(2.0));
        variants.push(changed);
        let mut changed = base.clone();
        changed.outputs[0].ty = ValueKind::Mesh;
        variants.push(changed);
        let mut changed = base.clone();
        changed
            .outputs
            .push(PortSpec::item("another", ValueKind::Vector));
        variants.push(changed);
        for changed in variants {
            assert_ne!(expected, hash(vec![changed]));
        }

        let mut ordered = base;
        ordered
            .inputs
            .push(PortSpec::item("second", ValueKind::Number));
        let expected = hash(vec![ordered.clone()]);
        ordered.inputs.swap(0, 1);
        assert_ne!(expected, hash(vec![ordered]));

        let mut ordered = fixture();
        ordered
            .outputs
            .push(PortSpec::item("second", ValueKind::Number));
        let expected = hash(vec![ordered.clone()]);
        ordered.outputs.swap(0, 1);
        assert_ne!(expected, hash(vec![ordered]));
    }

    fn default_hash(value: Value) -> String {
        let mut component = fixture();
        component.inputs[0].default = Some(value);
        hash(vec![component])
    }

    #[test]
    fn defaults_preserve_float_bits_value_tags_and_array_boundaries() {
        let mut absent = fixture();
        absent.inputs[0].default = None;
        assert_ne!(hash(vec![absent]), default_hash(Value::Null));
        assert_ne!(
            default_hash(Value::Number(0.0)),
            default_hash(Value::Number(-0.0))
        );
        assert_ne!(
            default_hash(Value::Number(f64::from_bits(0x7ff8_0000_0000_0001))),
            default_hash(Value::Number(f64::from_bits(0x7ff8_0000_0000_0002)))
        );
        assert_ne!(
            default_hash(Value::Null),
            default_hash(Value::List(Vec::new()))
        );
        assert_ne!(
            default_hash(Value::Bool(true)),
            default_hash(Value::Number(1.0))
        );
        assert_ne!(
            default_hash(Value::List(vec![
                Value::Text("ab".into()),
                Value::Text("c".into())
            ])),
            default_hash(Value::List(vec![
                Value::Text("a".into()),
                Value::Text("bc".into())
            ]))
        );
        assert_ne!(
            default_hash(Value::List(vec![Value::Number(1.0), Value::Number(2.0)])),
            default_hash(Value::List(vec![Value::Number(2.0), Value::Number(1.0)]))
        );
    }

    #[test]
    fn defaults_include_mesh_normals_and_curve_parameters() {
        let mesh = Mesh {
            positions: vec![Vec3::ZERO],
            normals: vec![Vec3::Z],
            indices: vec![[0, 0, 0]],
        };
        let mut changed = mesh.clone();
        changed.normals[0] = Vec3::X;
        assert_ne!(
            default_hash(Value::Mesh(Arc::new(mesh))),
            default_hash(Value::Mesh(Arc::new(changed)))
        );
        let circle = |radius| {
            Value::Curve(Arc::new(Curve::Circle {
                plane: Plane::world_xy_at(Vec3::ZERO),
                radius,
            }))
        };
        assert_ne!(default_hash(circle(1.0)), default_hash(circle(2.0)));
    }

    #[test]
    fn unavailable_build_ids_are_explicitly_unknown() {
        assert_eq!(build_id(None), "unknown");
        assert_eq!(build_id(Some(" \n\t")), "unknown");
        assert_eq!(build_id(Some(" revision-123 \n")), "revision-123");
    }
}
