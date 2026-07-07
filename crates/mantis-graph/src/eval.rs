//! Deterministic dataflow evaluation with dirty-tracking cache.
//!
//! Semantics:
//! - Nodes evaluate in `Graph::topo_order()`.
//! - Unknown `type_name` -> error entry "unknown component: X".
//! - Input gathering, per input port i of the component:
//!     wired  -> the upstream node's output value at that port
//!               (upstream errored -> this node errors "upstream error");
//!     unwired-> port default if Some, else Value::Null.
//! - Longest-list matching: if any Access::Item port holds a Value::List, the
//!   component runs N times (N = max len over those ports; empty list -> node
//!   evaluates to empty lists). Run i takes list[min(i, len-1)] (last repeats),
//!   non-list values broadcast. Each output port then becomes a List of the
//!   per-run values. Access::List ports receive lists whole (scalars wrapped
//!   in a 1-list) and do NOT trigger mapping. Nested lists do not trigger a
//!   second mapping level — inner lists pass through as-is (the component may
//!   then type-error).
//! - Type check AFTER unwrap: a scalar on an Item port that fails
//!   `kind_matches` -> node error naming the port. Null on a defaultless port
//!   -> error "input <name> missing".
//! - Component eval Err(msg) -> node error; outputs absent for errored nodes.
//! - Cache: a node re-evaluates iff marked dirty (invalidate/invalidate_all,
//!   including downstream propagation) or its inputs changed; otherwise cached
//!   outputs are reused. Structure changes (connect/disconnect/remove) must
//!   dirty affected nodes + downstream (the app calls `invalidate`).

use crate::component::{Access, Component, PortSpec, Registry};
use crate::graph::{Graph, NodeId};
use crate::value::{ParamValue, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvalOutput {
    /// Per node: one Value per output port. Errored nodes absent.
    pub outputs: BTreeMap<NodeId, Vec<Value>>,
    pub errors: BTreeMap<NodeId, String>,
}

/// Reusable evaluator holding the cache across frames.
///
/// A node is "dirty" when it has no cache entry; `invalidate` removes the
/// entry for a node and everything downstream of it. During `evaluate`, a
/// node additionally re-evaluates when any wired upstream node produced a
/// different output than the previous pass (or errored).
#[derive(Default)]
pub struct Evaluator {
    cache: BTreeMap<NodeId, Vec<Value>>,
    dirty_all: bool,
}

impl Evaluator {
    pub fn new() -> Evaluator {
        Evaluator { cache: BTreeMap::new(), dirty_all: true }
    }

    /// Mark `id` and everything downstream dirty.
    pub fn invalidate(&mut self, graph: &Graph, id: NodeId) {
        self.cache.remove(&id);
        for d in graph.downstream(id) {
            self.cache.remove(&d);
        }
    }

    pub fn invalidate_all(&mut self) {
        self.cache.clear();
        self.dirty_all = true;
    }

    /// Evaluate the whole graph (cached nodes reused).
    pub fn evaluate(&mut self, graph: &Graph, reg: &Registry) -> EvalOutput {
        if self.dirty_all {
            self.cache.clear();
            self.dirty_all = false;
        }
        // Nodes removed from the graph must not linger in the cache (their id
        // could in principle be re-used by a later AddNode).
        let stale: Vec<NodeId> = self
            .cache
            .keys()
            .copied()
            .filter(|id| !graph.nodes.contains_key(id))
            .collect();
        for id in stale {
            self.cache.remove(&id);
        }

        let mut out = EvalOutput::default();
        // Nodes whose outputs this pass differ from the previous pass (or
        // which errored) — anything wired to them must re-evaluate.
        let mut changed: BTreeSet<NodeId> = BTreeSet::new();

        for id in graph.topo_order() {
            let Some(node) = graph.nodes.get(&id) else { continue };
            let Some(comp) = reg.get(&node.type_name).cloned() else {
                self.cache.remove(&id);
                changed.insert(id);
                out.errors
                    .insert(id, format!("unknown component: {}", node.type_name));
                continue;
            };
            let specs = comp.inputs();

            let mut needs = !self.cache.contains_key(&id);
            let mut upstream_err = false;
            for i in 0..specs.len() {
                if let Some(e) = graph.incoming((id, i as u16)) {
                    if changed.contains(&e.from.0) {
                        needs = true;
                    }
                    if !out.outputs.contains_key(&e.from.0) {
                        // Upstream errored (or was skipped) this pass.
                        needs = true;
                        upstream_err = true;
                    }
                }
            }

            if !needs {
                let vals = self.cache.get(&id).cloned().unwrap_or_default();
                out.outputs.insert(id, vals);
                continue;
            }
            if upstream_err {
                self.cache.remove(&id);
                changed.insert(id);
                out.errors.insert(id, "upstream error".into());
                continue;
            }

            match run_node(graph, id, comp.as_ref(), &specs, &node.params, &out.outputs) {
                Ok(vals) => {
                    if self.cache.get(&id) != Some(&vals) {
                        changed.insert(id);
                    }
                    out.outputs.insert(id, vals.clone());
                    self.cache.insert(id, vals);
                }
                Err(msg) => {
                    self.cache.remove(&id);
                    changed.insert(id);
                    out.errors.insert(id, msg);
                }
            }
        }
        out
    }
}

/// Gather inputs for one node, apply longest-list matching, run the component.
fn run_node(
    graph: &Graph,
    id: NodeId,
    comp: &dyn Component,
    specs: &[PortSpec],
    params: &BTreeMap<String, ParamValue>,
    ready: &BTreeMap<NodeId, Vec<Value>>,
) -> Result<Vec<Value>, String> {
    // 1. Gather one raw value per input port.
    let mut raw: Vec<Value> = Vec::with_capacity(specs.len());
    for (i, spec) in specs.iter().enumerate() {
        let mut v = match graph.incoming((id, i as u16)) {
            Some(e) => {
                let outs = ready
                    .get(&e.from.0)
                    .ok_or_else(|| "upstream error".to_string())?;
                outs.get(e.from.1 as usize).cloned().ok_or_else(|| {
                    format!(
                        "input {}: upstream output port {} out of range",
                        spec.name, e.from.1
                    )
                })?
            }
            None => spec.default.clone().unwrap_or(Value::Null),
        };
        if matches!(v, Value::Null) {
            match &spec.default {
                Some(d) => v = d.clone(),
                None => return Err(format!("input {} missing", spec.name)),
            }
        }
        raw.push(v);
    }

    // 2. Longest-list analysis over Item ports.
    let mut mapping = false;
    let mut any_empty = false;
    let mut n_runs = 0usize;
    for (spec, v) in specs.iter().zip(&raw) {
        if spec.access == Access::Item {
            if let Value::List(l) = v {
                mapping = true;
                if l.is_empty() {
                    any_empty = true;
                }
                n_runs = n_runs.max(l.len());
            }
        }
    }
    let out_count = comp.outputs().len();
    if mapping && (any_empty || n_runs == 0) {
        // An empty list on any mapped port -> empty output lists.
        return Ok(vec![Value::List(Vec::new()); out_count]);
    }

    // Per-run scalar for one port (type-checked AFTER unwrap for Item ports).
    let prep = |spec: &PortSpec, v: &Value, i: usize| -> Result<Value, String> {
        match spec.access {
            Access::List => Ok(match v {
                Value::List(_) => v.clone(),
                other => Value::List(vec![other.clone()]),
            }),
            Access::Item => {
                let s = match v {
                    Value::List(l) => l[i.min(l.len() - 1)].clone(),
                    other => other.clone(),
                };
                if !s.kind_matches(spec.ty) {
                    return Err(match s {
                        Value::Null => format!("input {} missing", spec.name),
                        _ => format!(
                            "input {}: expected {:?}, got {}",
                            spec.name,
                            spec.ty,
                            s.describe()
                        ),
                    });
                }
                Ok(s)
            }
        }
    };

    if !mapping {
        let inputs: Vec<Value> = specs
            .iter()
            .zip(&raw)
            .map(|(s, v)| prep(s, v, 0))
            .collect::<Result<_, _>>()?;
        return comp.eval(&inputs, params);
    }

    // 3. Mapped runs; each output port becomes the List of per-run values.
    let mut runs: Vec<Vec<Value>> = Vec::with_capacity(n_runs);
    for i in 0..n_runs {
        let inputs: Vec<Value> = specs
            .iter()
            .zip(&raw)
            .map(|(s, v)| prep(s, v, i))
            .collect::<Result<_, _>>()?;
        runs.push(comp.eval(&inputs, params)?);
    }
    let mut outs = Vec::with_capacity(out_count);
    for j in 0..out_count {
        outs.push(Value::List(
            runs.iter()
                .map(|r| r.get(j).cloned().unwrap_or(Value::Null))
                .collect(),
        ));
    }
    Ok(outs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{PortSpec, Registry};
    use crate::graph::GraphOp;
    use crate::value::ValueKind;
    use mantis_kernel::Vec3;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn nid(n: u128) -> NodeId {
        NodeId(n)
    }
    fn add_node(g: &mut Graph, id: u128, ty: &str) {
        g.apply(&GraphOp::AddNode { id: nid(id), type_name: ty.into(), pos: (0.0, 0.0) })
            .unwrap();
    }
    fn connect(g: &mut Graph, from: (u128, u16), to: (u128, u16)) {
        g.apply(&GraphOp::Connect { from: (nid(from.0), from.1), to: (nid(to.0), to.1) })
            .unwrap();
    }
    fn set_num(g: &mut Graph, id: u128, key: &str, v: f64) {
        g.apply(&GraphOp::SetParam {
            id: nid(id),
            key: key.into(),
            value: ParamValue::Number(v),
        })
        .unwrap();
    }
    fn set_bool(g: &mut Graph, id: u128, key: &str, v: bool) {
        g.apply(&GraphOp::SetParam { id: nid(id), key: key.into(), value: ParamValue::Bool(v) })
            .unwrap();
    }
    fn slider(g: &mut Graph, id: u128, value: f64) {
        add_node(g, id, "number_slider");
        set_num(g, id, "value", value);
    }
    fn eval(g: &Graph) -> EvalOutput {
        Evaluator::new().evaluate(g, &Registry::standard())
    }
    fn nums(v: &Value) -> Vec<f64> {
        match v {
            Value::List(l) => l.iter().map(|e| e.as_number().expect("number")).collect(),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn two_sliders_add() {
        let mut g = Graph::new();
        slider(&mut g, 1, 2.0);
        slider(&mut g, 2, 3.0);
        add_node(&mut g, 3, "add");
        connect(&mut g, (1, 0), (3, 0));
        connect(&mut g, (2, 0), (3, 1));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(3)][0], Value::Number(5.0));
    }

    #[test]
    fn series_sin_maps_over_list() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "series"); // defaults: 0, 1, count 10
        add_node(&mut g, 2, "sin");
        connect(&mut g, (1, 0), (2, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let got = nums(&out.outputs[&nid(2)][0]);
        assert_eq!(got.len(), 10);
        for (i, v) in got.iter().enumerate() {
            assert!((v - (i as f64).sin()).abs() < 1e-12);
        }
    }

    #[test]
    fn list_scalar_broadcast() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "series"); // [0..9]
        slider(&mut g, 2, 5.0);
        add_node(&mut g, 3, "add");
        connect(&mut g, (1, 0), (3, 0));
        connect(&mut g, (2, 0), (3, 1));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let got = nums(&out.outputs[&nid(3)][0]);
        assert_eq!(got, (0..10).map(|i| i as f64 + 5.0).collect::<Vec<_>>());
    }

    #[test]
    fn longest_list_repeats_last() {
        let mut g = Graph::new();
        // s1 = [0,1,2]
        slider(&mut g, 1, 3.0);
        add_node(&mut g, 2, "series");
        connect(&mut g, (1, 0), (2, 2));
        // s2 = [10, 20]
        slider(&mut g, 3, 10.0); // start
        slider(&mut g, 4, 10.0); // step
        slider(&mut g, 5, 2.0); // count
        add_node(&mut g, 6, "series");
        connect(&mut g, (3, 0), (6, 0));
        connect(&mut g, (4, 0), (6, 1));
        connect(&mut g, (5, 0), (6, 2));
        add_node(&mut g, 7, "add");
        connect(&mut g, (2, 0), (7, 0));
        connect(&mut g, (6, 0), (7, 1));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        // last element of the shorter list repeats: [0+10, 1+20, 2+20]
        assert_eq!(nums(&out.outputs[&nid(7)][0]), vec![10.0, 21.0, 22.0]);
    }

    #[test]
    fn empty_list_yields_empty_outputs() {
        let mut g = Graph::new();
        slider(&mut g, 1, 0.0); // count 0
        add_node(&mut g, 2, "series");
        connect(&mut g, (1, 0), (2, 2));
        add_node(&mut g, 3, "sin");
        connect(&mut g, (2, 0), (3, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(2)][0], Value::List(vec![]));
        assert_eq!(out.outputs[&nid(3)][0], Value::List(vec![]));
    }

    #[test]
    fn list_port_wraps_scalar() {
        let mut g = Graph::new();
        slider(&mut g, 1, 4.0);
        add_node(&mut g, 2, "list_length");
        connect(&mut g, (1, 0), (2, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(2)][0], Value::Number(1.0));
    }

    #[test]
    fn type_mismatch_names_port() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "bool_toggle");
        add_node(&mut g, 2, "sin");
        connect(&mut g, (1, 0), (2, 0));
        let out = eval(&g);
        let err = &out.errors[&nid(2)];
        assert!(err.contains("input x"), "{err}");
        assert!(err.contains("Number"), "{err}");
        assert!(!out.outputs.contains_key(&nid(2)));
    }

    #[test]
    fn missing_defaultless_input_errors() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "add");
        let out = eval(&g);
        assert_eq!(out.errors[&nid(1)], "input a missing");
    }

    #[test]
    fn unknown_component_errors() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "warp_drive");
        let out = eval(&g);
        assert_eq!(out.errors[&nid(1)], "unknown component: warp_drive");
    }

    #[test]
    fn upstream_error_propagates() {
        let mut g = Graph::new();
        slider(&mut g, 1, 1.0);
        slider(&mut g, 2, 0.0);
        add_node(&mut g, 3, "divide");
        connect(&mut g, (1, 0), (3, 0));
        connect(&mut g, (2, 0), (3, 1));
        add_node(&mut g, 4, "negate");
        connect(&mut g, (3, 0), (4, 0));
        let out = eval(&g);
        assert!(out.errors[&nid(3)].contains("zero"), "{:?}", out.errors);
        assert_eq!(out.errors[&nid(4)], "upstream error");
        assert!(!out.outputs.contains_key(&nid(3)));
        assert!(!out.outputs.contains_key(&nid(4)));
    }

    #[test]
    fn panel_unwired_is_not_an_error() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "panel");
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.outputs[&nid(1)].is_empty());
    }

    /// Test-only component that counts how many times it is evaluated.
    struct Counter(Arc<AtomicUsize>);
    impl Component for Counter {
        fn type_name(&self) -> &'static str {
            "test_counter"
        }
        fn label(&self) -> &'static str {
            "Counter"
        }
        fn category(&self) -> &'static str {
            "Test"
        }
        fn inputs(&self) -> Vec<PortSpec> {
            vec![PortSpec::item("x", ValueKind::Number)]
        }
        fn outputs(&self) -> Vec<PortSpec> {
            vec![PortSpec::item("x", ValueKind::Number)]
        }
        fn eval(
            &self,
            inputs: &[Value],
            _params: &BTreeMap<String, ParamValue>,
        ) -> Result<Vec<Value>, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![inputs[0].clone()])
        }
    }

    #[test]
    fn cache_skips_clean_branches() {
        let evals = Arc::new(AtomicUsize::new(0));
        let mut reg = Registry::standard();
        reg.register(Arc::new(Counter(evals.clone())));

        let mut g = Graph::new();
        slider(&mut g, 1, 1.0);
        add_node(&mut g, 2, "test_counter"); // branch A
        connect(&mut g, (1, 0), (2, 0));
        slider(&mut g, 3, 2.0);
        add_node(&mut g, 4, "test_counter"); // branch B
        connect(&mut g, (3, 0), (4, 0));

        let mut ev = Evaluator::new();
        let out = ev.evaluate(&g, &reg);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(2)][0], Value::Number(1.0));
        assert_eq!(out.outputs[&nid(4)][0], Value::Number(2.0));
        assert_eq!(evals.load(Ordering::SeqCst), 2);

        // Change branch A's slider through the graph, invalidate it.
        set_num(&mut g, 1, "value", 7.0);
        ev.invalidate(&g, nid(1));
        let out = ev.evaluate(&g, &reg);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(2)][0], Value::Number(7.0));
        // Branch B's counter was NOT re-evaluated; only branch A's was.
        assert_eq!(evals.load(Ordering::SeqCst), 3);
        // Untouched branch still has its cached output available.
        assert_eq!(out.outputs[&nid(4)][0], Value::Number(2.0));

        // A no-op pass re-evaluates nothing.
        let out = ev.evaluate(&g, &reg);
        assert_eq!(evals.load(Ordering::SeqCst), 3);
        assert_eq!(out.outputs.len(), 4);

        // invalidate_all recomputes both.
        ev.invalidate_all();
        ev.evaluate(&g, &reg);
        assert_eq!(evals.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn structure_change_with_invalidate_recomputes() {
        let mut g = Graph::new();
        slider(&mut g, 1, 2.0);
        slider(&mut g, 2, 9.0);
        add_node(&mut g, 3, "negate");
        connect(&mut g, (1, 0), (3, 0));
        let mut ev = Evaluator::new();
        let reg = Registry::standard();
        let out = ev.evaluate(&g, &reg);
        assert_eq!(out.outputs[&nid(3)][0], Value::Number(-2.0));
        // Rewire negate to the other slider.
        g.apply(&GraphOp::Connect { from: (nid(2), 0), to: (nid(3), 0) }).unwrap();
        ev.invalidate(&g, nid(3));
        let out = ev.evaluate(&g, &reg);
        assert_eq!(out.outputs[&nid(3)][0], Value::Number(-9.0));
    }

    #[test]
    fn removed_node_leaves_no_stale_cache() {
        let mut g = Graph::new();
        slider(&mut g, 1, 2.0);
        add_node(&mut g, 2, "negate");
        connect(&mut g, (1, 0), (2, 0));
        let mut ev = Evaluator::new();
        let reg = Registry::standard();
        ev.evaluate(&g, &reg);
        g.apply(&GraphOp::RemoveNode { id: nid(2) }).unwrap();
        let out = ev.evaluate(&g, &reg);
        assert!(!out.outputs.contains_key(&nid(2)));
        assert!(!out.errors.contains_key(&nid(2)));
    }

    #[test]
    fn list_item_wrap_through_graph() {
        let mut g = Graph::new();
        slider(&mut g, 1, 5.0); // count 5 -> [0,1,2,3,4]
        add_node(&mut g, 2, "series");
        connect(&mut g, (1, 0), (2, 2));
        slider(&mut g, 3, 7.0); // index 7
        add_node(&mut g, 4, "bool_toggle");
        set_bool(&mut g, 4, "value", true);
        add_node(&mut g, 5, "list_item");
        connect(&mut g, (2, 0), (5, 0));
        connect(&mut g, (3, 0), (5, 1));
        connect(&mut g, (4, 0), (5, 2));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(5)][0], Value::Number(2.0)); // 7 mod 5
    }

    #[test]
    fn remap_through_graph_defaults() {
        let mut g = Graph::new();
        slider(&mut g, 1, 0.5);
        add_node(&mut g, 2, "remap"); // defaults s0=0,s1=1,t0=0,t1=10
        connect(&mut g, (1, 0), (2, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let v = out.outputs[&nid(2)][0].as_number().unwrap();
        assert!((v - 5.0).abs() < 1e-12);
    }

    #[test]
    fn deconstruct_construct_round_trip() {
        let mut g = Graph::new();
        slider(&mut g, 1, 1.0);
        slider(&mut g, 2, 2.0);
        slider(&mut g, 3, 3.0);
        add_node(&mut g, 4, "vector_xyz");
        connect(&mut g, (1, 0), (4, 0));
        connect(&mut g, (2, 0), (4, 1));
        connect(&mut g, (3, 0), (4, 2));
        add_node(&mut g, 5, "deconstruct_vector");
        connect(&mut g, (4, 0), (5, 0));
        add_node(&mut g, 6, "vector_xyz");
        connect(&mut g, (5, 0), (6, 0));
        connect(&mut g, (5, 1), (6, 1));
        connect(&mut g, (5, 2), (6, 2));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.outputs[&nid(6)][0], Value::Vector(Vec3::new(1.0, 2.0, 3.0)));
    }

    #[test]
    fn rotate_vector_90deg_through_graph() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "unit_x");
        slider(&mut g, 2, std::f64::consts::FRAC_PI_2);
        add_node(&mut g, 3, "rotate_vector");
        connect(&mut g, (1, 0), (3, 0));
        connect(&mut g, (2, 0), (3, 2));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let v = out.outputs[&nid(3)][0].as_vector().unwrap();
        assert!((v - Vec3::Y).length() < 1e-9, "{v:?}");
    }

    #[test]
    fn upstream_output_port_out_of_range_errors() {
        let mut g = Graph::new();
        slider(&mut g, 1, 1.0);
        add_node(&mut g, 2, "negate");
        // Slider only has output port 0; wire from port 5.
        g.apply(&GraphOp::Connect { from: (nid(1), 5), to: (nid(2), 0) }).unwrap();
        let out = eval(&g);
        assert!(out.errors[&nid(2)].contains("out of range"), "{:?}", out.errors);
    }

    // ---- pipelines through mantis-kernel geometry ----

    #[test]
    #[ignore = "pending kernel: Curve::divide/length still todo!() while mantis-kernel is implemented concurrently"]
    fn kernel_line_divide_and_length() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "point_xyz"); // (0,0,0)
        slider(&mut g, 2, 10.0);
        add_node(&mut g, 3, "point_xyz"); // (10,0,0)
        connect(&mut g, (2, 0), (3, 0));
        add_node(&mut g, 4, "line");
        connect(&mut g, (1, 0), (4, 0));
        connect(&mut g, (3, 0), (4, 1));
        add_node(&mut g, 5, "divide_curve"); // n default 10
        connect(&mut g, (4, 0), (5, 0));
        add_node(&mut g, 6, "curve_length");
        connect(&mut g, (4, 0), (6, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let pts = match &out.outputs[&nid(5)][0] {
            Value::List(l) => l.clone(),
            other => panic!("expected list, got {other:?}"),
        };
        assert_eq!(pts.len(), 11);
        let mid = pts[5].as_vector().unwrap();
        assert!((mid - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-9, "{mid:?}");
        let len = out.outputs[&nid(6)][0].as_number().unwrap();
        assert!((len - 10.0).abs() < 1e-6, "{len}");
    }

    #[test]
    #[ignore = "pending kernel: ops::extrude/Mesh::volume still todo!() while mantis-kernel is implemented concurrently"]
    fn kernel_circle_extrude_volume() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "circle"); // defaults: world XY, r=1
        add_node(&mut g, 2, "extrude"); // defaults: dir (0,0,1), segments 32
        connect(&mut g, (1, 0), (2, 0));
        add_node(&mut g, 3, "volume");
        connect(&mut g, (2, 0), (3, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let v = out.outputs[&nid(3)][0].as_number().unwrap();
        // 32-gon prism of height 1: area = 16*sin(pi/16) ~ 3.1214
        assert!((v.abs() - std::f64::consts::PI).abs() < 0.1, "volume {v}");
    }

    #[test]
    #[ignore = "pending kernel: Mesh::sphere/volume still todo!() while mantis-kernel is implemented concurrently"]
    fn kernel_sphere_volume() {
        let mut g = Graph::new();
        add_node(&mut g, 1, "sphere"); // defaults r=1, segs 24
        add_node(&mut g, 2, "volume");
        connect(&mut g, (1, 0), (2, 0));
        let out = eval(&g);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let v = out.outputs[&nid(2)][0].as_number().unwrap();
        let exact = 4.0 / 3.0 * std::f64::consts::PI;
        assert!((v.abs() - exact).abs() < 0.25, "volume {v} vs {exact}");
    }
}
