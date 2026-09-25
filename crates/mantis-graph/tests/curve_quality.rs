use mantis_graph::{Access, Registry, Value};
use mantis_kernel::{Curve, Plane, Vec3};
use std::{collections::BTreeMap, sync::Arc};

fn c(curve: Curve) -> Value {
    Value::Curve(Arc::new(curve))
}
fn line(a: Vec3, b: Vec3) -> Value {
    c(Curve::Line { a, b })
}
fn eval(name: &str, supplied: &[Value]) -> Result<Vec<Value>, String> {
    let registry = Registry::standard();
    let component = registry.get(name).unwrap();
    let mut inputs = supplied.to_vec();
    for port in component.inputs().into_iter().skip(inputs.len()) {
        inputs.push(port.default.unwrap_or(Value::Null));
    }
    component.eval(&inputs, &BTreeMap::new())
}
fn list(value: &Value) -> &[Value] {
    let Value::List(values) = value else {
        panic!("expected list")
    };
    values
}

#[test]
fn curve_quality_ports_defaults_and_comb_geometry() {
    let registry = Registry::standard();
    let component = registry.get("curve_comb").unwrap();
    assert_eq!(
        component
            .inputs()
            .iter()
            .map(|p| p.name)
            .collect::<Vec<_>>(),
        ["curve", "samples", "scale"]
    );
    assert!(component.outputs().iter().all(|p| p.access == Access::List));
    let circle = c(Curve::Circle {
        plane: Plane::world_xy(),
        radius: 2.0,
    });
    let out = eval("curve_comb", std::slice::from_ref(&circle)).unwrap();
    assert_eq!(list(&out[0]).len(), 32);
    assert_eq!(list(&out[1]), vec![Value::Number(0.5); 32].as_slice());
    assert_eq!(list(&out[2]), vec![Value::Bool(true); 32].as_slice());
    let tooth = list(&out[0])[0].as_curve().unwrap();
    assert_eq!(tooth.point_at(0.0), Vec3::new(2.0, 0.0, 0.0));
    assert_eq!(tooth.point_at(1.0), Vec3::new(1.5, 0.0, 0.0));
    let scaled = eval(
        "curve_comb",
        &[circle, Value::Number(8.0), Value::Number(4.0)],
    )
    .unwrap();
    assert_eq!(
        list(&scaled[0])[0].as_curve().unwrap().point_at(1.0),
        Vec3::ZERO
    );
}

#[test]
fn continuity_and_blend_reports_are_usable_with_defaults() {
    let a = line(-Vec3::X, Vec3::ZERO);
    let b = line(Vec3::ZERO, Vec3::X);
    let out = eval("curve_continuity", &[a.clone(), b]).unwrap();
    assert_eq!(
        out,
        [
            Value::Number(0.0),
            Value::Number(0.0),
            Value::Number(0.0),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true)
        ]
    );
    let b = line(Vec3::new(2.0, 2.0, 0.0), Vec3::new(2.0, 3.0, 0.0));
    let blend = eval("blend_curve", &[a.clone(), b.clone()])
        .unwrap()
        .remove(0);
    assert_eq!(
        eval("curve_continuity", &[a, blend.clone()]).unwrap()[4],
        Value::Bool(true)
    );
    assert_eq!(
        eval("curve_continuity", &[blend, b]).unwrap()[4],
        Value::Bool(true)
    );
}

#[test]
fn diagnostics_fail_cleanly_without_silently_accepting_invalid_samples() {
    let a = line(Vec3::ZERO, Vec3::X);
    for count in [f64::NAN, f64::INFINITY, 0.0, 1.0, 2.5, 2049.0, 1e300] {
        assert!(eval("curve_comb", &[a.clone(), Value::Number(count)]).is_err());
    }
    for scale in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(eval(
            "curve_comb",
            &[a.clone(), Value::Number(4.0), Value::Number(scale)]
        )
        .is_err());
    }
    let tiny_circle = c(Curve::Circle {
        plane: Plane::world_xy(),
        radius: 1e-6,
    });
    assert!(eval(
        "curve_comb",
        &[tiny_circle, Value::Number(4.0), Value::Number(f64::MAX)]
    )
    .is_err());
    let collapsed = line(Vec3::ZERO, Vec3::ZERO);
    let out = eval("curve_comb", &[collapsed.clone(), Value::Number(2.0)]).unwrap();
    assert_eq!(list(&out[1]), [Value::Number(0.0), Value::Number(0.0)]);
    assert_eq!(list(&out[2]), [Value::Bool(false), Value::Bool(false)]);
    assert!(eval("curve_continuity", &[collapsed.clone(), a.clone()]).is_err());
    assert!(eval("blend_curve", &[collapsed, a.clone()]).is_err());
    assert!(eval(
        "curve_continuity",
        &[a.clone(), a.clone(), Value::Number(-1.0)]
    )
    .is_err());
    assert!(eval("blend_curve", &[a.clone(), a, Value::Number(0.0)]).is_err());
}
