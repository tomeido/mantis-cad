use mantis_graph::{
    geometry::{GeometryData, GeometryRecord, GeometrySource},
    Evaluator, Graph, GraphOp, NodeId, ParamValue, Registry, Value,
};
use mantis_kernel::{Mesh, Plane, Vec3};
use std::collections::BTreeMap;

#[test]
fn persistent_list_roundtrips_graph_and_preserves_typed_pattern_values() {
    let mut graph = Graph::new();
    for (id, name) in [(1, "constant_list"), (2, "constant_list"), (3, "dispatch")] {
        graph
            .apply(&GraphOp::AddNode {
                id: NodeId(id),
                type_name: name.into(),
                pos: (0.0, 0.0),
            })
            .unwrap();
    }
    for (id, json) in [(1, "[1,2,3,4,5]"), (2, "[true,false]")] {
        graph
            .apply(&GraphOp::SetParam {
                id: NodeId(id),
                key: "values".into(),
                value: ParamValue::Text(json.into()),
            })
            .unwrap();
    }
    for (from, port) in [(1, 0), (2, 1)] {
        graph
            .apply(&GraphOp::Connect {
                from: (NodeId(from), 0),
                to: (NodeId(3), port),
            })
            .unwrap();
    }
    let restored: Graph = serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    let result = Evaluator::new().evaluate(&restored, &Registry::standard());
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        result.outputs[&NodeId(3)],
        vec![
            Value::List(vec![
                Value::Number(1.0),
                Value::Number(3.0),
                Value::Number(5.0)
            ]),
            Value::List(vec![Value::Number(2.0), Value::Number(4.0)])
        ]
    );
}

#[test]
fn persistent_list_rejects_nested_or_unsupported_data() {
    let registry = Registry::standard();
    let component = registry.get("constant_list").unwrap();
    for invalid in ["[[1]]", "[null]", "[{\"x\":1}]", "[1e999]", "{}"][..].iter() {
        let params = BTreeMap::from([("values".into(), ParamValue::Text((*invalid).into()))]);
        assert!(component.eval(&[], &params).is_err(), "{invalid}");
    }
    let params = BTreeMap::from([(
        "values".into(),
        ParamValue::Text("[1,true,\"label\"]".into()),
    )]);
    assert_eq!(
        component.eval(&[], &params).unwrap(),
        vec![Value::List(vec![
            Value::Number(1.0),
            Value::Bool(true),
            Value::Text("label".into())
        ])]
    );
}

#[test]
fn imported_mesh_evaluates_after_roundtrip_and_participates_in_native_solid_operation() {
    let record = GeometryRecord {
        name: "box".into(),
        layer: "test".into(),
        geometry: GeometryData::Mesh {
            mesh: Mesh::box_mesh(&Plane::world_xy_at(Vec3::new(0.5, 0.0, 0.0)), 1.0, 1.0, 1.0),
        },
        source: Some(GeometrySource {
            format: "test-brep".into(),
            data: "original CAD data".into(),
            preview_units: None,
        }),
    };
    let mut graph = Graph::new();
    for (id, name) in [
        (1, "imported_geometry"),
        (2, "box_mesh"),
        (3, "mesh_boolean_intersection"),
    ] {
        graph
            .apply(&GraphOp::AddNode {
                id: NodeId(id),
                type_name: name.into(),
                pos: (0.0, 0.0),
            })
            .unwrap();
    }
    graph
        .apply(&GraphOp::SetParam {
            id: NodeId(1),
            key: "data".into(),
            value: ParamValue::Text(record.to_json().unwrap()),
        })
        .unwrap();
    for (from, port) in [(1, 0), (2, 1)] {
        graph
            .apply(&GraphOp::Connect {
                from: (NodeId(from), 0),
                to: (NodeId(3), port),
            })
            .unwrap();
    }
    let restored: Graph = serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    let result = Evaluator::new().evaluate(&restored, &Registry::standard());
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!((result.outputs[&NodeId(3)][0].as_mesh().unwrap().volume() - 0.5).abs() < 1e-7);
    let data = restored.nodes[&NodeId(1)].params["data"].as_text().unwrap();
    assert_eq!(GeometryRecord::from_json(data).unwrap(), record);
}
