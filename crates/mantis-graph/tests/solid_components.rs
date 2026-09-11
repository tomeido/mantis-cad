use mantis_graph::{Evaluator, Graph, GraphOp, NodeId, ParamValue, Registry, Value};
use mantis_kernel::{Mesh, Plane, Vec3};
use std::collections::BTreeMap;
use std::sync::Arc;

fn mesh(origin: Vec3) -> Value {
    Value::Mesh(Arc::new(Mesh::box_mesh(
        &Plane::world_xy_at(origin),
        2.0,
        2.0,
        2.0,
    )))
}
fn eval(name: &str, inputs: &[Value]) -> Result<Vec<Value>, String> {
    Registry::standard()
        .get(name)
        .unwrap()
        .eval(inputs, &BTreeMap::new())
}
fn volume(value: &Value, expected: f64) {
    assert!((value.as_mesh().unwrap().volume() - expected).abs() < 1e-7);
}

#[test]
fn mesh_boolean_and_plane_nodes_expose_solid_results() {
    let a = mesh(Vec3::ZERO);
    let b = mesh(Vec3::X);
    let tolerance = Value::Number(1e-7);
    for (name, expected) in [
        ("mesh_boolean_union", 12.0),
        ("mesh_boolean_difference", 4.0),
        ("mesh_boolean_intersection", 4.0),
    ] {
        volume(
            &eval(name, &[a.clone(), b.clone(), tolerance.clone()]).unwrap()[0],
            expected,
        );
    }
    let plane = Value::Plane(Plane::world_xy_at(Vec3::Z));
    let split = eval(
        "mesh_split_plane",
        &[a.clone(), plane.clone(), tolerance.clone()],
    )
    .unwrap();
    volume(&split[0], 4.0);
    volume(&split[1], 4.0);
    for positive in [false, true] {
        let trim = eval(
            "mesh_trim_plane",
            &[
                a.clone(),
                plane.clone(),
                Value::Bool(positive),
                tolerance.clone(),
            ],
        )
        .unwrap();
        volume(&trim[0], 4.0);
        let bounds = trim[0].as_mesh().unwrap().bbox();
        assert!(
            (if positive {
                bounds.min.z - 1.0
            } else {
                bounds.max.z - 1.0
            })
            .abs()
                < 1e-7
        );
    }
    assert!(eval("mesh_boolean_union", &[Value::Number(1.0), b, tolerance]).is_err());
}

fn add(graph: &mut Graph, id: u128, name: &str) {
    graph
        .apply(&GraphOp::AddNode {
            id: NodeId(id),
            type_name: name.into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
}
fn wire(graph: &mut Graph, from: (u128, u16), to: (u128, u16)) {
    graph
        .apply(&GraphOp::Connect {
            from: (NodeId(from.0), from.1),
            to: (NodeId(to.0), to.1),
        })
        .unwrap();
}

#[test]
fn graph_boolean_updates_from_shared_slider_and_matches_fresh_evaluation() {
    let mut graph = Graph::new();
    for (id, name) in [
        (1, "box_mesh"),
        (2, "box_mesh"),
        (3, "point_xyz"),
        (4, "number_slider"),
        (5, "mesh_boolean_union"),
        (6, "volume"),
    ] {
        add(&mut graph, id, name);
    }
    graph
        .apply(&GraphOp::SetParam {
            id: NodeId(4),
            key: "value".into(),
            value: ParamValue::Number(0.5),
        })
        .unwrap();
    for (from, to) in [
        ((4, 0), (3, 0)),
        ((3, 0), (2, 0)),
        ((1, 0), (5, 0)),
        ((2, 0), (5, 1)),
        ((5, 0), (6, 0)),
    ] {
        wire(&mut graph, from, to);
    }
    let registry = Registry::standard();
    let mut evaluator = Evaluator::new();
    let before = evaluator.evaluate(&graph, &registry);
    assert!(before.errors.is_empty(), "{:?}", before.errors);
    assert!((before.outputs[&NodeId(6)][0].as_number().unwrap() - 1.5).abs() < 1e-7);
    graph
        .apply(&GraphOp::SetParam {
            id: NodeId(4),
            key: "value".into(),
            value: ParamValue::Number(1.5),
        })
        .unwrap();
    let after = evaluator.evaluate(&graph, &registry);
    assert!(after.errors.is_empty(), "{:?}", after.errors);
    assert!((after.outputs[&NodeId(6)][0].as_number().unwrap() - 2.0).abs() < 1e-7);
    assert_eq!(
        after.outputs,
        Evaluator::new().evaluate(&graph, &registry).outputs
    );
}

#[test]
fn invalid_open_mesh_becomes_a_clean_graph_error() {
    let mut graph = Graph::new();
    for (id, name) in [
        (1, "rectangle"),
        (2, "planar_srf"),
        (3, "box_mesh"),
        (4, "mesh_boolean_difference"),
        (5, "volume"),
    ] {
        add(&mut graph, id, name);
    }
    for (from, to) in [
        ((1, 0), (2, 0)),
        ((2, 0), (4, 0)),
        ((3, 0), (4, 1)),
        ((4, 0), (5, 0)),
    ] {
        wire(&mut graph, from, to);
    }
    let result = Evaluator::new().evaluate(&graph, &Registry::standard());
    assert!(result.errors[&NodeId(4)].contains("watertight"));
    assert!(result.errors[&NodeId(5)].contains("upstream"));
    assert!(!result.outputs.contains_key(&NodeId(4)));
}
