//! Geometric and graph-level regressions for Rhino/GH-inspired components.

use mantis_graph::{Evaluator, Graph, GraphOp, NodeId, ParamValue, Registry, Value};
use mantis_kernel::{Curve, Mesh, NurbsCurve, Plane, Vec3};
use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::sync::Arc;

fn eval(name: &str, inputs: &[Value]) -> Result<Vec<Value>, String> {
    Registry::standard()
        .get(name)
        .unwrap()
        .eval(inputs, &BTreeMap::new())
}

fn n(number: f64) -> Value {
    Value::Number(number)
}
fn list(numbers: &[f64]) -> Value {
    Value::List(numbers.iter().copied().map(n).collect())
}
fn values(value: &Value) -> &[Value] {
    match value {
        Value::List(list) => list,
        _ => panic!("expected list: {value:?}"),
    }
}
fn near(a: Vec3, b: Vec3) {
    assert!((a - b).length() < 1e-8, "{a:?} != {b:?}");
}

#[test]
fn linear_array_includes_original_and_translates_meshes() {
    let source = Arc::new(Mesh::box_mesh(&Plane::world_xy(), 2.0, 3.0, 4.0));
    let out = eval(
        "array_linear",
        &[
            Value::Mesh(source.clone()),
            Value::Vector(Vec3::new(5.0, -2.0, 1.0)),
            n(3.0),
        ],
    )
    .unwrap();
    let copies = values(&out[0]);
    assert_eq!(copies.len(), 3);
    assert!(Arc::ptr_eq(&source, &copies[0].as_mesh().unwrap()));
    for (index, copy) in copies.iter().enumerate() {
        let mesh = copy.as_mesh().unwrap();
        near(
            mesh.bbox().min,
            source.bbox().min + Vec3::new(5.0, -2.0, 1.0) * index as f64,
        );
        assert!((mesh.volume() - source.volume()).abs() < 1e-8);
        assert_eq!(mesh.indices, source.indices);
    }
}

#[test]
fn arrays_handle_zero_one_and_reject_nonfinite_overflow_or_excessive_allocations() {
    let point = Value::Vector(Vec3::X);
    for count in [0.0, 1.0] {
        let out = eval(
            "array_linear",
            &[point.clone(), Value::Vector(Vec3::Y), n(count)],
        )
        .unwrap();
        assert_eq!(values(&out[0]).len(), count as usize);
    }
    assert!(eval(
        "array_linear",
        &[point.clone(), Value::Vector(Vec3::X), n(4097.0)]
    )
    .is_err());
    assert!(eval(
        "array_linear",
        &[point.clone(), Value::Vector(Vec3::X), n(f64::NAN)]
    )
    .is_err());
    assert!(eval(
        "array_linear",
        &[point, Value::Vector(Vec3::new(f64::MAX, 0.0, 0.0)), n(3.0)]
    )
    .is_err());
    let heavy = Value::Mesh(Arc::new(Mesh {
        positions: vec![Vec3::ZERO; 1000],
        normals: vec![Vec3::Z; 1000],
        indices: Vec::new(),
    }));
    assert!(
        eval("array_linear", &[heavy, Value::Vector(Vec3::X), n(4096.0)])
            .unwrap_err()
            .contains("64 MiB")
    );
    assert!(eval("array_linear", &[n(3.0), Value::Vector(Vec3::X), n(0.0)]).is_err());
}

#[test]
fn polar_array_full_circle_has_no_duplicate_and_partial_sweep_has_endpoints() {
    let plane = Value::Plane(Plane::world_xy_at(Vec3::new(4.0, 5.0, 0.0)));
    let point = Value::Vector(Vec3::new(5.0, 5.0, 0.0));
    let full = eval(
        "array_polar",
        &[point.clone(), plane.clone(), n(4.0), n(TAU)],
    )
    .unwrap();
    let expected = [
        Vec3::new(5.0, 5.0, 0.0),
        Vec3::new(4.0, 6.0, 0.0),
        Vec3::new(3.0, 5.0, 0.0),
        Vec3::new(4.0, 4.0, 0.0),
    ];
    for (copy, expected) in values(&full[0]).iter().zip(expected) {
        near(copy.as_vector().unwrap(), expected);
    }
    let half = eval(
        "array_polar",
        &[point.clone(), plane.clone(), n(3.0), n(PI)],
    )
    .unwrap();
    near(
        values(&half[0])[2].as_vector().unwrap(),
        Vec3::new(3.0, 5.0, 0.0),
    );
    let clockwise = eval(
        "array_polar",
        &[point.clone(), plane.clone(), n(2.0), n(-FRAC_PI_2)],
    )
    .unwrap();
    near(
        values(&clockwise[0])[1].as_vector().unwrap(),
        Vec3::new(4.0, 4.0, 0.0),
    );
    assert!(eval("array_polar", &[point, plane, n(2.0), n(2.0 * TAU)]).is_err());
}

#[test]
fn polar_array_uses_the_plane_normal() {
    let plane = Plane {
        origin: Vec3::ZERO,
        x_axis: Vec3::Y,
        y_axis: Vec3::Z,
    };
    let out = eval(
        "array_polar",
        &[
            Value::Vector(Vec3::Y),
            Value::Plane(plane),
            n(2.0),
            n(FRAC_PI_2),
        ],
    )
    .unwrap();
    near(values(&out[0])[1].as_vector().unwrap(), Vec3::Z);
}

#[test]
fn reversal_preserves_locus_and_reverses_parameters_for_every_curve_variant() {
    let points = vec![
        Vec3::ZERO,
        Vec3::new(2.0, 0.0, 0.0),
        Vec3::new(3.0, 2.0, 0.0),
        Vec3::new(0.0, 4.0, 1.0),
    ];
    let mut weighted = NurbsCurve::from_points(&points, 2, false).unwrap();
    weighted.weights = vec![1.0, 0.5, 2.0, 1.0];
    weighted.knots = vec![2.0, 2.0, 2.0, 3.0, 6.0, 6.0, 6.0];
    let mut explicit_closed = points.clone();
    explicit_closed.push(points[0]);
    let curves = vec![
        Curve::Line {
            a: points[0],
            b: points[2],
        },
        Curve::Polyline {
            points: points.clone(),
            closed: false,
        },
        Curve::Polyline {
            points: points.clone(),
            closed: true,
        },
        Curve::Polyline {
            points: explicit_closed,
            closed: true,
        },
        Curve::Circle {
            plane: Plane::world_xy_at(Vec3::Z),
            radius: 3.0,
        },
        Curve::Arc {
            plane: Plane::world_xy(),
            radius: 2.0,
            start_angle: 0.3,
            end_angle: 2.7,
        },
        Curve::Nurbs(weighted),
        Curve::Nurbs(NurbsCurve::from_points(&points, 3, true).unwrap()),
    ];
    for curve in curves {
        let reversed = eval("reverse_curve", &[Value::Curve(Arc::new(curve.clone()))]).unwrap()[0]
            .as_curve()
            .unwrap();
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            near(reversed.point_at(t), curve.point_at(1.0 - t));
        }
        let ends = eval("end_points", &[Value::Curve(reversed)]).unwrap();
        near(ends[0].as_vector().unwrap(), curve.point_at(1.0));
        near(ends[1].as_vector().unwrap(), curve.point_at(0.0));
    }
}

#[test]
fn rectangle_respects_plane_and_has_a_closed_seam() {
    let plane = Plane {
        origin: Vec3::new(1.0, 2.0, 3.0),
        x_axis: Vec3::Y,
        y_axis: Vec3::Z,
    };
    let curve = eval("rectangle", &[Value::Plane(plane), n(2.0), n(3.0)]).unwrap()[0]
        .as_curve()
        .unwrap();
    assert!(curve.is_closed());
    assert!((curve.length() - 10.0).abs() < 1e-8);
    near(curve.point_at(0.0), plane.origin);
    near(curve.point_at(1.0), plane.origin);
    near(curve.bbox().max, Vec3::new(1.0, 4.0, 6.0));
    assert!(eval("rectangle", &[Value::Plane(plane), n(-2.0), n(3.0)]).is_err());
}

#[test]
fn reversing_nearly_coincident_polyline_endpoints_uses_kernel_closure_tolerance() {
    let curve = Curve::Polyline {
        points: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(1e-10, 0.0, 0.0)],
        closed: true,
    };
    let reversed = eval("reverse_curve", &[Value::Curve(Arc::new(curve.clone()))]).unwrap()[0]
        .as_curve()
        .unwrap();
    assert!((reversed.point_at(0.0) - curve.point_at(1.0)).length() < 1e-12);
    for i in 0..=20 {
        let t = i as f64 / 20.0;
        assert!((reversed.point_at(t) - curve.point_at(1.0 - t)).length() < 1e-12);
    }
}

#[test]
fn reverse_sort_and_bounds_preserve_expected_order_and_stable_indices() {
    let source = list(&[3.0, -2.0, 3.0, 1.0]);
    assert_eq!(
        eval("reverse_list", std::slice::from_ref(&source)).unwrap(),
        vec![list(&[1.0, 3.0, -2.0, 3.0])]
    );
    assert_eq!(
        eval("sort_list", std::slice::from_ref(&source)).unwrap(),
        vec![list(&[-2.0, 1.0, 3.0, 3.0]), list(&[1.0, 3.0, 0.0, 2.0])]
    );
    assert_eq!(eval("bounds", &[source]).unwrap(), vec![n(-2.0), n(3.0)]);
    assert!(eval("bounds", &[list(&[])]).is_err());
    assert!(eval("bounds", &[list(&[f64::INFINITY])]).is_err());
    assert!(eval("sort_list", &[list(&[1.0, f64::NAN])]).is_err());
    assert_eq!(
        eval("sort_list", &[list(&[])]).unwrap(),
        vec![list(&[]), list(&[])]
    );
}

#[test]
fn shifts_wrap_or_remove_items_in_both_directions() {
    let source = list(&[0.0, 1.0, 2.0, 3.0]);
    for (shift, wrap, expected) in [
        (1.0, true, vec![1.0, 2.0, 3.0, 0.0]),
        (-1.0, true, vec![3.0, 0.0, 1.0, 2.0]),
        (5.0, true, vec![1.0, 2.0, 3.0, 0.0]),
        (1.0, false, vec![1.0, 2.0, 3.0]),
        (-1.0, false, vec![0.0, 1.0, 2.0]),
        (100.0, false, vec![]),
        (4294967296.0, false, vec![]),
        (f64::MIN, false, vec![]),
    ] {
        assert_eq!(
            eval("shift_list", &[source.clone(), n(shift), Value::Bool(wrap)]).unwrap(),
            vec![list(&expected)]
        );
    }
    assert_eq!(
        eval("shift_list", &[list(&[]), n(1.0), Value::Bool(true)]).unwrap(),
        vec![list(&[])]
    );
}

#[test]
fn patterns_repeat_and_dispatch_partitions_without_losing_items() {
    let source = list(&[0.0, 1.0, 2.0, 3.0, 4.0]);
    let pattern = Value::List(vec![Value::Bool(true), Value::Bool(false)]);
    let output = eval("dispatch", &[source.clone(), pattern.clone()]).unwrap();
    assert_eq!(output, vec![list(&[0.0, 2.0, 4.0]), list(&[1.0, 3.0])]);
    assert_eq!(
        eval("cull_pattern", &[source.clone(), pattern]).unwrap(),
        vec![output[1].clone()]
    );
    assert_eq!(
        eval("cull_pattern", &[source.clone(), list(&[0.0, 1.0])]).unwrap(),
        vec![output[0].clone()]
    );
    assert!(eval("dispatch", &[source.clone(), list(&[])]).is_err());
    assert!(eval("cull_pattern", &[source, list(&[f64::NAN])]).is_err());
    assert_eq!(
        eval("merge", &output).unwrap(),
        vec![list(&[0.0, 2.0, 4.0, 1.0, 3.0])]
    );
}

#[test]
fn random_is_seeded_bounded_and_accepts_finite_wide_domains() {
    let inputs = [n(0.0), n(1.0), n(100.0), n(1.0)];
    let output = eval("random", &inputs).unwrap();
    assert_eq!(output, eval("random", &inputs).unwrap());
    let numbers = values(&output[0]);
    assert_eq!(numbers.len(), 100);
    assert!((numbers[0].as_number().unwrap() - 0.5665615751722809).abs() < 1e-15);
    assert!(numbers
        .iter()
        .all(|v| (0.0..1.0).contains(&v.as_number().unwrap())));
    assert_ne!(
        output,
        eval("random", &[n(0.0), n(1.0), n(100.0), n(2.0)]).unwrap()
    );
    assert!(eval("random", &[n(1.0), n(0.0), n(5.0), n(1.0)]).is_err());
    assert!(eval("random", &[n(0.0), n(1.0), n(1e12), n(1.0)]).is_err());
    assert!(eval("random", &[n(-f64::MAX), n(f64::MAX), n(100.0), n(1.0)]).is_ok());
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
fn parameter(graph: &mut Graph, id: u128, key: &str, value: f64) {
    graph
        .apply(&GraphOp::SetParam {
            id: NodeId(id),
            key: key.into(),
            value: ParamValue::Number(value),
        })
        .unwrap();
}

#[test]
fn graph_random_sort_bounds_remap_and_geometry_recompute_after_seed_edit() {
    let mut graph = Graph::new();
    for (id, name) in [
        (1, "number_slider"),
        (2, "random"),
        (3, "sort_list"),
        (4, "bounds"),
        (5, "remap"),
        (6, "point_xyz"),
        (7, "polyline"),
    ] {
        add(&mut graph, id, name);
    }
    parameter(&mut graph, 1, "value", 1.0);
    for (from, to) in [
        ((1, 0), (2, 3)),
        ((2, 0), (3, 0)),
        ((3, 0), (4, 0)),
        ((3, 0), (5, 0)),
        ((4, 0), (5, 1)),
        ((4, 1), (5, 2)),
        ((5, 0), (6, 0)),
        ((6, 0), (7, 0)),
    ] {
        wire(&mut graph, from, to);
    }
    let mut evaluator = Evaluator::new();
    let registry = Registry::standard();
    let before = evaluator.evaluate(&graph, &registry);
    assert!(before.errors.is_empty(), "{:?}", before.errors);
    let curve = before.outputs[&NodeId(7)][0].as_curve().unwrap();
    near(curve.point_at(0.0), Vec3::ZERO);
    near(curve.point_at(1.0), Vec3::X * 10.0);
    parameter(&mut graph, 1, "value", 2.0);
    let after = evaluator.evaluate(&graph, &registry);
    assert!(after.errors.is_empty(), "{:?}", after.errors);
    assert_ne!(before.outputs[&NodeId(2)], after.outputs[&NodeId(2)]);
    assert_ne!(before.outputs[&NodeId(7)], after.outputs[&NodeId(7)]);
    assert_eq!(
        after.outputs,
        Evaluator::new().evaluate(&graph, &registry).outputs
    );
}

#[test]
fn graph_array_dispatch_merge_and_list_item_use_whole_lists() {
    let mut graph = Graph::new();
    for (id, name) in [
        (1, "point_xyz"),
        (2, "array_linear"),
        (3, "dispatch"),
        (4, "merge"),
        (5, "list_length"),
        (6, "list_item"),
    ] {
        add(&mut graph, id, name);
    }
    for (from, to) in [
        ((1, 0), (2, 0)),
        ((2, 0), (3, 0)),
        ((3, 0), (4, 0)),
        ((3, 1), (4, 1)),
        ((4, 0), (5, 0)),
        ((4, 0), (6, 0)),
    ] {
        wire(&mut graph, from, to);
    }
    let result = Evaluator::new().evaluate(&graph, &Registry::standard());
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.outputs[&NodeId(5)], vec![n(5.0)]);
    assert_eq!(result.outputs[&NodeId(6)], vec![Value::Vector(Vec3::ZERO)]);
    assert_eq!(values(&result.outputs[&NodeId(3)][0]).len(), 3);
    assert_eq!(values(&result.outputs[&NodeId(3)][1]).len(), 2);
}
