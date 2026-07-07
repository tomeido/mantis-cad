//! Regression: geometry ops must return an empty mesh (never a NaN-filled one,
//! never a panic) when handed a non-finite direction/axis. `NaN < EPS` is
//! false, so a bare magnitude test would let these through.

use mantis_kernel::ops::{extrude, revolve};
use mantis_kernel::{Curve, Plane, Vec3};

fn unit_circle() -> Curve {
    Curve::Circle { plane: Plane::world_xy(), radius: 1.0 }
}

fn mesh_is_finite(m: &mantis_kernel::Mesh) -> bool {
    m.positions.iter().all(|p| p.is_finite()) && m.normals.iter().all(|n| n.is_finite())
}

#[test]
fn extrude_rejects_nonfinite_direction() {
    for d in [
        Vec3::new(f64::NAN, 0.0, 1.0),
        Vec3::new(0.0, f64::INFINITY, 0.0),
        Vec3::new(0.0, 0.0, f64::NEG_INFINITY),
    ] {
        let m = extrude(&unit_circle(), d, 32);
        assert_eq!(m.triangle_count(), 0, "non-finite dir {d:?} produced geometry");
    }
    // a finite direction still works and is finite
    let ok = extrude(&unit_circle(), Vec3::Z, 32);
    assert!(ok.triangle_count() > 0 && mesh_is_finite(&ok));
}

#[test]
fn revolve_rejects_nonfinite_axis() {
    let profile = Curve::Line { a: Vec3::new(2.0, 0.0, 0.0), b: Vec3::new(2.0, 0.0, 1.0) };
    for axis in [Vec3::new(f64::NAN, 0.0, 1.0), Vec3::new(0.0, 0.0, f64::INFINITY)] {
        let m = revolve(&profile, Vec3::ZERO, axis, std::f64::consts::TAU, 32);
        assert_eq!(m.triangle_count(), 0, "non-finite axis {axis:?} produced geometry");
    }
    let m = revolve(&profile, Vec3::ZERO, Vec3::Z, std::f64::consts::TAU, 32);
    assert!(m.triangle_count() > 0 && mesh_is_finite(&m));
}
