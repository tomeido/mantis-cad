//! Direct (engine-less) tests of the built-in components. Everything here
//! avoids kernel geometry bodies that may not be implemented yet — only pure
//! math and data plumbing.

use crate::component::Registry;
use crate::value::{ParamValue, Value};
use mantis_kernel::{Curve, Mesh, Plane, Vec3};
use std::collections::BTreeMap;
use std::sync::Arc;

fn params() -> BTreeMap<String, ParamValue> {
    BTreeMap::new()
}

fn eval(type_name: &str, inputs: &[Value]) -> Result<Vec<Value>, String> {
    let reg = Registry::standard();
    let c = reg.get(type_name).unwrap_or_else(|| panic!("missing component {type_name}"));
    c.eval(inputs, &params())
}

fn num(v: &Value) -> f64 {
    v.as_number().expect("number")
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
}

fn approx_vec(a: Vec3, b: Vec3) {
    assert!((a - b).length() < 1e-9, "{a:?} !~ {b:?}");
}

const FROZEN_TYPE_NAMES: &[&str] = &[
    // Params
    "number_slider", "bool_toggle", "panel", "point_xyz", "pi_const",
    // Maths
    "add", "subtract", "multiply", "divide", "power", "modulo", "negate",
    "sin", "cos", "sqrt", "abs", "min", "max", "remap",
    // Sets
    "series", "range", "list_item", "list_length", "repeat",
    // Vector
    "vector_xyz", "deconstruct_vector", "unit_x", "unit_y", "unit_z",
    "distance", "dot", "cross", "amplitude", "rotate_vector",
    "xy_plane", "plane_normal",
    // Curve
    "line", "polyline", "circle", "arc", "nurbs_curve", "divide_curve",
    "eval_curve", "curve_length",
    // Surface
    "extrude", "revolve", "loft", "pipe", "planar_srf", "box_mesh",
    "sphere", "cylinder", "cone", "torus",
    // Transform
    "move", "rotate", "scale", "mirror",
    // Analysis
    "bbox", "area", "volume", "mesh_info", "data_size",
];

#[test]
fn registry_contains_all_frozen_type_names() {
    let reg = Registry::standard();
    for name in FROZEN_TYPE_NAMES {
        let c = reg.get(name).unwrap_or_else(|| panic!("missing component {name}"));
        assert_eq!(c.type_name(), *name);
        assert!(!c.label().is_empty());
        assert!(!c.category().is_empty());
        // Port specs must be constructible without panicking.
        let _ = (c.inputs(), c.outputs());
    }
    assert_eq!(FROZEN_TYPE_NAMES.len(), 63);
}

#[test]
fn slider_defaults_clamp_and_snap() {
    // Default params -> value 5.
    let out = eval("number_slider", &[]).unwrap();
    assert_eq!(out, vec![Value::Number(5.0)]);

    let mut p = params();
    p.insert("min".into(), ParamValue::Number(0.0));
    p.insert("max".into(), ParamValue::Number(10.0));
    p.insert("step".into(), ParamValue::Number(0.5));
    p.insert("value".into(), ParamValue::Number(3.3));
    let reg = Registry::standard();
    let c = reg.get("number_slider").unwrap();
    assert_eq!(c.eval(&[], &p).unwrap(), vec![Value::Number(3.5)]);

    p.insert("value".into(), ParamValue::Number(42.0));
    assert_eq!(c.eval(&[], &p).unwrap(), vec![Value::Number(10.0)]);
    p.insert("value".into(), ParamValue::Number(-3.0));
    assert_eq!(c.eval(&[], &p).unwrap(), vec![Value::Number(0.0)]);
}

#[test]
fn bool_toggle_and_pi() {
    assert_eq!(eval("bool_toggle", &[]).unwrap(), vec![Value::Bool(false)]);
    let mut p = params();
    p.insert("value".into(), ParamValue::Bool(true));
    let reg = Registry::standard();
    assert_eq!(
        reg.get("bool_toggle").unwrap().eval(&[], &p).unwrap(),
        vec![Value::Bool(true)]
    );
    approx(num(&eval("pi_const", &[]).unwrap()[0]), std::f64::consts::PI);
}

#[test]
fn panel_is_display_only() {
    let out = eval("panel", &[Value::List(vec![Value::Number(1.0)])]).unwrap();
    assert!(out.is_empty());
}

#[test]
fn maths_polymorphic() {
    let n = |x: f64| Value::Number(x);
    let v = |x: f64, y: f64, z: f64| Value::Vector(Vec3::new(x, y, z));
    assert_eq!(eval("add", &[n(2.0), n(3.0)]).unwrap(), vec![n(5.0)]);
    assert_eq!(
        eval("add", &[v(1.0, 0.0, 0.0), v(0.0, 2.0, 0.0)]).unwrap(),
        vec![v(1.0, 2.0, 0.0)]
    );
    assert_eq!(
        eval("subtract", &[v(1.0, 2.0, 3.0), v(1.0, 1.0, 1.0)]).unwrap(),
        vec![v(0.0, 1.0, 2.0)]
    );
    assert_eq!(
        eval("multiply", &[v(1.0, 2.0, 0.0), n(2.0)]).unwrap(),
        vec![v(2.0, 4.0, 0.0)]
    );
    assert_eq!(
        eval("multiply", &[n(2.0), v(1.0, 2.0, 0.0)]).unwrap(),
        vec![v(2.0, 4.0, 0.0)]
    );
    // Vector * Vector is an error suggesting dot/cross.
    let err = eval("multiply", &[v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)]).unwrap_err();
    assert!(err.contains("multiply"), "{err}");
    // Number + Vector is an error.
    assert!(eval("add", &[n(1.0), v(1.0, 0.0, 0.0)]).is_err());
}

#[test]
fn maths_edge_cases() {
    let n = |x: f64| Value::Number(x);
    assert!(eval("divide", &[n(1.0), n(0.0)]).unwrap_err().contains("zero"));
    assert_eq!(eval("divide", &[n(9.0), n(3.0)]).unwrap(), vec![n(3.0)]);
    assert!(eval("modulo", &[n(1.0), n(0.0)]).is_err());
    // Floored modulo: -1 mod 3 == 2.
    assert_eq!(eval("modulo", &[n(-1.0), n(3.0)]).unwrap(), vec![n(2.0)]);
    assert!(eval("sqrt", &[n(-4.0)]).is_err());
    assert_eq!(eval("sqrt", &[n(9.0)]).unwrap(), vec![n(3.0)]);
    assert_eq!(eval("negate", &[n(2.5)]).unwrap(), vec![n(-2.5)]);
    assert_eq!(eval("abs", &[n(-2.5)]).unwrap(), vec![n(2.5)]);
    assert_eq!(eval("min", &[n(1.0), n(2.0)]).unwrap(), vec![n(1.0)]);
    assert_eq!(eval("max", &[n(1.0), n(2.0)]).unwrap(), vec![n(2.0)]);
    assert_eq!(eval("power", &[n(2.0), n(10.0)]).unwrap(), vec![n(1024.0)]);
    approx(num(&eval("sin", &[n(std::f64::consts::FRAC_PI_2)]).unwrap()[0]), 1.0);
    approx(num(&eval("cos", &[n(0.0)]).unwrap()[0]), 1.0);
}

#[test]
fn remap_maps_domains() {
    let n = |x: f64| Value::Number(x);
    let out = eval("remap", &[n(5.0), n(0.0), n(10.0), n(0.0), n(100.0)]).unwrap();
    approx(num(&out[0]), 50.0);
    // Degenerate source domain errors.
    assert!(eval("remap", &[n(5.0), n(1.0), n(1.0), n(0.0), n(1.0)]).is_err());
}

#[test]
fn series_range_repeat() {
    let n = |x: f64| Value::Number(x);
    let out = eval("series", &[n(2.0), n(3.0), n(4.0)]).unwrap();
    assert_eq!(
        out[0],
        Value::List(vec![n(2.0), n(5.0), n(8.0), n(11.0)])
    );
    // Negative count clamps to empty.
    assert_eq!(eval("series", &[n(0.0), n(1.0), n(-5.0)]).unwrap()[0], Value::List(vec![]));
    // Excessive count errors instead of allocating gigabytes.
    assert!(eval("series", &[n(0.0), n(1.0), n(1e12)]).is_err());

    let out = eval("range", &[n(0.0), n(1.0), n(4.0)]).unwrap();
    assert_eq!(
        out[0],
        Value::List(vec![n(0.0), n(0.25), n(0.5), n(0.75), n(1.0)])
    );

    let out = eval("repeat", &[Value::Bool(true), n(3.0)]).unwrap();
    assert_eq!(
        out[0],
        Value::List(vec![Value::Bool(true); 3])
    );
}

#[test]
fn list_item_and_length() {
    let n = |x: f64| Value::Number(x);
    let l = Value::List(vec![n(10.0), n(20.0), n(30.0)]);
    assert_eq!(
        eval("list_item", &[l.clone(), n(1.0), Value::Bool(false)]).unwrap(),
        vec![n(20.0)]
    );
    // wrap: 4 mod 3 == 1; -1 mod 3 == 2.
    assert_eq!(
        eval("list_item", &[l.clone(), n(4.0), Value::Bool(true)]).unwrap(),
        vec![n(20.0)]
    );
    assert_eq!(
        eval("list_item", &[l.clone(), n(-1.0), Value::Bool(true)]).unwrap(),
        vec![n(30.0)]
    );
    // out of range without wrap errors
    assert!(eval("list_item", &[l.clone(), n(5.0), Value::Bool(false)]).is_err());
    assert!(eval("list_item", &[Value::List(vec![]), n(0.0), Value::Bool(true)]).is_err());
    assert_eq!(eval("list_length", &[l]).unwrap(), vec![n(3.0)]);
}

#[test]
fn vector_components() {
    let n = |x: f64| Value::Number(x);
    let out = eval("vector_xyz", &[n(1.0), n(2.0), n(3.0)]).unwrap();
    assert_eq!(out, vec![Value::Vector(Vec3::new(1.0, 2.0, 3.0))]);

    let out = eval("deconstruct_vector", &[Value::Vector(Vec3::new(1.0, 2.0, 3.0))]).unwrap();
    assert_eq!(out, vec![n(1.0), n(2.0), n(3.0)]);

    assert_eq!(eval("unit_x", &[n(2.0)]).unwrap(), vec![Value::Vector(Vec3::new(2.0, 0.0, 0.0))]);
    assert_eq!(eval("unit_y", &[n(1.0)]).unwrap(), vec![Value::Vector(Vec3::Y)]);
    assert_eq!(eval("unit_z", &[n(1.0)]).unwrap(), vec![Value::Vector(Vec3::Z)]);

    let a = Value::Vector(Vec3::new(0.0, 0.0, 0.0));
    let b = Value::Vector(Vec3::new(3.0, 4.0, 0.0));
    approx(num(&eval("distance", &[a, b]).unwrap()[0]), 5.0);

    let x = Value::Vector(Vec3::X);
    let y = Value::Vector(Vec3::Y);
    approx(num(&eval("dot", &[x.clone(), y.clone()]).unwrap()[0]), 0.0);
    let out = eval("cross", &[x, y]).unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::Z);

    let out = eval("amplitude", &[Value::Vector(Vec3::new(0.0, 3.0, 0.0)), n(2.0)]).unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::new(0.0, 2.0, 0.0));
    assert!(eval("amplitude", &[Value::Vector(Vec3::ZERO), n(2.0)]).is_err());
}

#[test]
fn rotate_vector_quarter_turn() {
    let out = eval(
        "rotate_vector",
        &[
            Value::Vector(Vec3::X),
            Value::Vector(Vec3::Z),
            Value::Number(std::f64::consts::FRAC_PI_2),
        ],
    )
    .unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::Y);
}

#[test]
fn planes() {
    let o = Vec3::new(1.0, 2.0, 3.0);
    let out = eval("xy_plane", &[Value::Vector(o)]).unwrap();
    match &out[0] {
        Value::Plane(p) => {
            assert_eq!(p.origin, o);
            approx_vec(p.normal(), Vec3::Z);
        }
        other => panic!("expected plane, got {other:?}"),
    }
    let out = eval(
        "plane_normal",
        &[Value::Vector(o), Value::Vector(Vec3::new(1.0, 1.0, 1.0))],
    )
    .unwrap();
    match &out[0] {
        Value::Plane(p) => {
            approx_vec(p.normal(), Vec3::new(1.0, 1.0, 1.0).normalized());
        }
        other => panic!("expected plane, got {other:?}"),
    }
}

#[test]
fn curve_constructors_are_pure_data() {
    let a = Vec3::ZERO;
    let b = Vec3::new(1.0, 0.0, 0.0);
    let out = eval("line", &[Value::Vector(a), Value::Vector(b)]).unwrap();
    assert!(matches!(&*out[0].as_curve().unwrap(), Curve::Line { .. }));

    let pts = Value::List(vec![
        Value::Vector(Vec3::ZERO),
        Value::Vector(Vec3::X),
        Value::Vector(Vec3::Y),
    ]);
    let out = eval("polyline", &[pts, Value::Bool(true)]).unwrap();
    match &*out[0].as_curve().unwrap() {
        Curve::Polyline { points, closed } => {
            assert_eq!(points.len(), 3);
            assert!(*closed);
        }
        other => panic!("expected polyline, got {other:?}"),
    }
    // Too few points errors.
    assert!(eval(
        "polyline",
        &[Value::List(vec![Value::Vector(Vec3::ZERO)]), Value::Bool(false)]
    )
    .is_err());
    // Non-vector element errors with an indexed port name.
    let err = eval(
        "polyline",
        &[
            Value::List(vec![Value::Vector(Vec3::ZERO), Value::Number(3.0)]),
            Value::Bool(false),
        ],
    )
    .unwrap_err();
    assert!(err.contains("points[1]"), "{err}");

    // circle accepts a bare point as its plane (Value::as_plane convenience).
    let out = eval("circle", &[Value::Vector(Vec3::new(0.0, 0.0, 2.0)), Value::Number(3.0)]).unwrap();
    match &*out[0].as_curve().unwrap() {
        Curve::Circle { plane, radius } => {
            assert_eq!(*radius, 3.0);
            assert_eq!(plane.origin, Vec3::new(0.0, 0.0, 2.0));
        }
        other => panic!("expected circle, got {other:?}"),
    }
    assert!(eval("circle", &[Value::Plane(Plane::world_xy()), Value::Number(0.0)]).is_err());

    let out = eval(
        "arc",
        &[
            Value::Plane(Plane::world_xy()),
            Value::Number(2.0),
            Value::Number(0.0),
            Value::Number(1.0),
        ],
    )
    .unwrap();
    assert!(matches!(&*out[0].as_curve().unwrap(), Curve::Arc { .. }));
}

#[test]
fn transforms_on_points_and_planes() {
    let p = Value::Vector(Vec3::new(1.0, 2.0, 3.0));
    let out = eval("move", &[p.clone(), Value::Vector(Vec3::new(1.0, 0.0, 0.0))]).unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::new(2.0, 2.0, 3.0));

    let out = eval(
        "rotate",
        &[
            Value::Vector(Vec3::X),
            Value::Plane(Plane::world_xy()),
            Value::Number(std::f64::consts::FRAC_PI_2),
        ],
    )
    .unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::Y);

    let out = eval(
        "scale",
        &[p.clone(), Value::Vector(Vec3::ZERO), Value::Number(2.0)],
    )
    .unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::new(2.0, 4.0, 6.0));

    let out = eval("mirror", &[p.clone(), Value::Plane(Plane::world_xy())]).unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::new(1.0, 2.0, -3.0));

    // Planes transform too.
    let out = eval(
        "move",
        &[
            Value::Plane(Plane::world_xy()),
            Value::Vector(Vec3::new(0.0, 0.0, 5.0)),
        ],
    )
    .unwrap();
    match &out[0] {
        Value::Plane(pl) => approx_vec(pl.origin, Vec3::new(0.0, 0.0, 5.0)),
        other => panic!("expected plane, got {other:?}"),
    }

    // Non-geometry input errors.
    assert!(eval("move", &[Value::Text("hi".into()), Value::Vector(Vec3::ZERO)]).is_err());
}

fn test_mesh() -> Mesh {
    Mesh {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z, Vec3::Z, Vec3::Z],
        indices: vec![[0, 1, 2]],
    }
}

#[test]
fn mesh_info_and_bbox() {
    let m = Arc::new(test_mesh());
    let out = eval("mesh_info", &[Value::Mesh(m.clone())]).unwrap();
    assert_eq!(out[0], Value::Number(3.0));
    assert_eq!(out[1], Value::Number(1.0));
    assert_eq!(out[2], Value::Number((3 * 24 + 3 * 24 + 12) as f64));

    let out = eval("bbox", &[Value::Mesh(m)]).unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::ZERO);
    approx_vec(out[1].as_vector().unwrap(), Vec3::new(1.0, 1.0, 0.0));

    // bbox of a list of points unions them.
    let l = Value::List(vec![
        Value::Vector(Vec3::new(-1.0, 0.0, 0.0)),
        Value::Vector(Vec3::new(2.0, 3.0, -4.0)),
    ]);
    let out = eval("bbox", &[l]).unwrap();
    approx_vec(out[0].as_vector().unwrap(), Vec3::new(-1.0, 0.0, -4.0));
    approx_vec(out[1].as_vector().unwrap(), Vec3::new(2.0, 3.0, 0.0));

    assert!(eval("bbox", &[Value::Number(1.0)]).is_err());
}

#[test]
fn data_size_sums() {
    let m = Arc::new(test_mesh());
    let l = Value::List(vec![
        Value::Number(1.0),
        Value::Vector(Vec3::ZERO),
        Value::Mesh(m.clone()),
        Value::List(vec![Value::Bool(true), Value::Text("abc".into())]),
    ]);
    let out = eval("data_size", &[l]).unwrap();
    let expected = 8 + 24 + m.approx_byte_size() + 1 + 3;
    assert_eq!(out[0], Value::Number(expected as f64));
}
