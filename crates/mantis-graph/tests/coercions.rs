//! Regression: documented Grasshopper-style implicit coercions must be
//! accepted on a *wired* input, not just as a port default. The eval type gate
//! (`Value::kind_matches`) must stay in lockstep with the `as_*` helpers.

use mantis_graph::{Evaluator, Graph, GraphOp, NodeId, ParamValue, Registry, Value};

fn nid(n: u128) -> NodeId {
    NodeId(n)
}

#[test]
fn point_wired_into_plane_port_is_accepted() {
    let reg = Registry::standard();
    let mut g = Graph::new();
    g.apply(&GraphOp::AddNode { id: nid(1), type_name: "point_xyz".into(), pos: (0.0, 0.0) })
        .unwrap();
    g.apply(&GraphOp::AddNode { id: nid(2), type_name: "circle".into(), pos: (0.0, 0.0) })
        .unwrap();
    g.apply(&GraphOp::Connect { from: (nid(1), 0), to: (nid(2), 0) }).unwrap();

    let mut ev = Evaluator::new();
    let out = ev.evaluate(&g, &reg);
    assert!(out.errors.get(&nid(2)).is_none(), "circle errored: {:?}", out.errors.get(&nid(2)));
    assert!(matches!(out.outputs.get(&nid(2)).and_then(|v| v.first()), Some(Value::Curve(_))));
}

#[test]
fn kind_matches_allows_point_as_plane_and_number_as_bool() {
    use mantis_graph::ValueKind;
    assert!(Value::Vector(Default::default()).kind_matches(ValueKind::Plane));
    assert!(Value::Number(1.0).kind_matches(ValueKind::Bool));
    // and does NOT over-accept
    assert!(!Value::Number(1.0).kind_matches(ValueKind::Vector));
    assert!(!Value::Null.kind_matches(ValueKind::Plane));
}

#[test]
fn nonfinite_vector_becomes_a_clean_node_error() {
    // A finite-but-huge upstream product overflows f64 to +inf; `multiply`
    // does not (and need not) guard that. The infinite number then reaches a
    // geometry node's vector port, which must fail cleanly rather than seed a
    // NaN curve. slider*slider -> vector_xyz.x -> line.b.
    let reg = Registry::standard();
    let mut g = Graph::new();
    // slider with value = 1e308
    g.apply(&GraphOp::AddNode { id: nid(1), type_name: "number_slider".into(), pos: (0.0, 0.0) })
        .unwrap();
    for (k, v) in [("min", 0.0), ("max", f64::MAX), ("value", f64::MAX)] {
        g.apply(&GraphOp::SetParam { id: nid(1), key: k.into(), value: ParamValue::Number(v) }).unwrap();
    }
    g.apply(&GraphOp::AddNode { id: nid(2), type_name: "multiply".into(), pos: (0.0, 0.0) })
        .unwrap();
    g.apply(&GraphOp::Connect { from: (nid(1), 0), to: (nid(2), 0) }).unwrap();
    g.apply(&GraphOp::Connect { from: (nid(1), 0), to: (nid(2), 1) }).unwrap(); // MAX*MAX = +inf
    g.apply(&GraphOp::AddNode { id: nid(3), type_name: "vector_xyz".into(), pos: (0.0, 0.0) })
        .unwrap();
    g.apply(&GraphOp::Connect { from: (nid(2), 0), to: (nid(3), 0) }).unwrap(); // x = +inf
    g.apply(&GraphOp::AddNode { id: nid(4), type_name: "line".into(), pos: (0.0, 0.0) })
        .unwrap();
    g.apply(&GraphOp::Connect { from: (nid(3), 0), to: (nid(4), 1) }).unwrap(); // line.b = (inf,0,0)

    let mut ev = Evaluator::new();
    let out = ev.evaluate(&g, &reg);
    // multiply really did overflow to a non-finite number:
    assert_eq!(
        out.outputs.get(&nid(2)).and_then(|v| v.first()),
        Some(&Value::Number(f64::INFINITY))
    );
    // ...and the line node refuses it instead of producing a curve.
    assert!(
        out.errors.contains_key(&nid(4)) && !out.outputs.contains_key(&nid(4)),
        "infinite vector leaked into geometry: err={:?} out={:?}",
        out.errors.get(&nid(4)),
        out.outputs.get(&nid(4)),
    );
}
