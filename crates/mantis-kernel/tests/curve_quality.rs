use mantis_kernel::{ContinuityTolerance, Curve, NurbsCurve, Plane, Vec3};

fn v(x: f64, y: f64) -> Vec3 {
    Vec3::new(x, y, 0.0)
}
fn line(a: Vec3, b: Vec3) -> Curve {
    Curve::Line { a, b }
}
fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}
fn quarter() -> Curve {
    Curve::Nurbs(NurbsCurve {
        degree: 2,
        control_points: vec![Vec3::X, v(1.0, 1.0), Vec3::Y],
        weights: vec![1.0, std::f64::consts::FRAC_1_SQRT_2, 1.0],
        knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
    })
}

#[test]
fn exact_circle_line_and_rational_curvatures() {
    let circle = Curve::Circle {
        plane: Plane::world_xy(),
        radius: 2.0,
    };
    let samples = circle.curvature_comb(16).unwrap();
    assert_eq!(samples.len(), 16);
    assert!(samples.last().unwrap().parameter < 1.0);
    for sample in samples {
        assert!(sample.valid);
        near(sample.curvature.length(), 0.5);
        assert!((sample.curvature + sample.point / 4.0).length() < 1e-12);
    }
    for sample in line(Vec3::ZERO, v(3.0, 4.0)).curvature_comb(8).unwrap() {
        assert!(sample.valid);
        assert_eq!(sample.curvature, Vec3::ZERO);
    }
    for sample in quarter().curvature_comb(17).unwrap() {
        assert!(sample.valid);
        near(sample.curvature.length(), 1.0);
        assert!((sample.curvature + sample.point).length() < 1e-12);
    }
}

#[test]
fn rational_curvature_is_translation_invariant_and_parametric_speed_independent() {
    let offset = v(1e8, -1e8);
    let Curve::Nurbs(mut q) = quarter() else {
        unreachable!()
    };
    q.control_points.iter_mut().for_each(|p| *p = *p + offset);
    for sample in Curve::Nurbs(q).curvature_comb(13).unwrap() {
        near(sample.curvature.length(), 1.0);
    }
    let rational_line = Curve::Nurbs(NurbsCurve {
        degree: 1,
        control_points: vec![Vec3::ZERO, Vec3::X],
        weights: vec![0.1, 50.0],
        knots: vec![0.0, 0.0, 1.0, 1.0],
    });
    for sample in rational_line.curvature_comb(13).unwrap() {
        assert!(sample.valid);
        near(sample.curvature.length(), 0.0);
    }
}

#[test]
fn continuity_distinguishes_position_tangent_and_curvature_failures() {
    let a = line(v(-1.0, 0.0), Vec3::ZERO);
    let tolerance = ContinuityTolerance::default();
    let smooth = a
        .continuity_to(&line(Vec3::ZERO, Vec3::X), tolerance)
        .unwrap();
    assert!(smooth.g0 && smooth.g1 && smooth.g2);
    assert_eq!(
        (smooth.gap, smooth.angle, smooth.curvature_delta),
        (0.0, 0.0, 0.0)
    );
    let gap = a
        .continuity_to(&line(Vec3::Y, v(1.0, 1.0)), tolerance)
        .unwrap();
    assert!(!gap.g0 && !gap.g1 && !gap.g2);
    near(gap.gap, 1.0);
    let corner = a
        .continuity_to(&line(Vec3::ZERO, Vec3::Y), tolerance)
        .unwrap();
    assert!(corner.g0 && !corner.g1 && !corner.g2);
    near(corner.angle, std::f64::consts::FRAC_PI_2);
    let reversed = a
        .continuity_to(&line(Vec3::ZERO, -Vec3::X), tolerance)
        .unwrap();
    near(reversed.angle, std::f64::consts::PI);
    assert!(!reversed.g1);
    let arc = Curve::Arc {
        plane: Plane::world_xy(),
        radius: 1.0,
        start_angle: 0.0,
        end_angle: std::f64::consts::FRAC_PI_2,
    };
    let tangent = line(Vec3::Y, v(-1.0, 1.0));
    let curvature_jump = arc.continuity_to(&tangent, tolerance).unwrap();
    assert!(curvature_jump.g0 && curvature_jump.g1 && !curvature_jump.g2);
    near(curvature_jump.curvature_delta, 1.0);
    let arc_next = Curve::Arc {
        plane: Plane::world_xy(),
        radius: 1.0,
        start_angle: std::f64::consts::FRAC_PI_2,
        end_angle: std::f64::consts::PI,
    };
    let matched = quarter().continuity_to(&arc_next, tolerance).unwrap();
    assert!(matched.g2);
}

#[test]
fn cubic_blend_matches_both_directed_tangents_and_tension_changes_shape() {
    let a = line(v(-2.0, 0.0), Vec3::ZERO);
    let b = line(v(2.0, 2.0), v(2.0, 4.0));
    let blend = a.tangent_blend_to(&b, 1.0).unwrap();
    assert!(blend.point_at(0.0).distance(a.point_at(1.0)) < 1e-12);
    assert!(blend.point_at(1.0).distance(b.point_at(0.0)) < 1e-12);
    assert!(
        a.continuity_to(&blend, ContinuityTolerance::default())
            .unwrap()
            .g1
    );
    assert!(
        blend
            .continuity_to(&b, ContinuityTolerance::default())
            .unwrap()
            .g1
    );
    let tighter = a.tangent_blend_to(&b, 0.25).unwrap();
    assert!(blend.point_at(0.5).distance(tighter.point_at(0.5)) > 0.1);
    assert_eq!(blend, a.tangent_blend_to(&b, 1.0).unwrap());
}

#[test]
fn undefined_curvature_is_flagged_at_corners_and_stationary_points() {
    let corner = Curve::Polyline {
        points: vec![Vec3::ZERO, Vec3::X, v(1.0, 1.0)],
        closed: false,
    };
    let samples = corner.curvature_comb(3).unwrap();
    assert!(samples[0].valid && !samples[1].valid && samples[2].valid);
    assert_eq!(samples[1].curvature, Vec3::ZERO);
    let closed = Curve::Polyline {
        points: vec![Vec3::ZERO, Vec3::X, v(1.0, 1.0)],
        closed: true,
    };
    assert!(!closed.curvature_comb(4).unwrap()[0].valid);
    let repeated = Curve::Polyline {
        points: vec![Vec3::ZERO, Vec3::ZERO, Vec3::X, Vec3::X],
        closed: false,
    };
    assert!(repeated.curvature_comb(3).unwrap().iter().all(|s| s.valid));
    let nurbs_corner = Curve::Nurbs(
        NurbsCurve::from_points(&[Vec3::ZERO, Vec3::X, v(1.0, 1.0)], 1, false).unwrap(),
    );
    assert!(!nurbs_corner.curvature_comb(3).unwrap()[1].valid);
    for degenerate in [
        line(Vec3::ZERO, Vec3::ZERO),
        Curve::Circle {
            plane: Plane::world_xy(),
            radius: 0.0,
        },
        Curve::Nurbs(NurbsCurve::from_points(&[Vec3::X; 4], 3, false).unwrap()),
    ] {
        for sample in degenerate.curvature_comb(5).unwrap() {
            assert!(!sample.valid);
            assert!(sample.point.is_finite() && sample.curvature.is_finite());
        }
        assert!(degenerate
            .continuity_to(&line(Vec3::ZERO, Vec3::X), ContinuityTolerance::default())
            .is_err());
        assert!(degenerate
            .tangent_blend_to(&line(Vec3::ZERO, Vec3::X), 1.0)
            .is_err());
    }
}

#[test]
fn rejects_invalid_parameters_malformed_curves_and_excess_work() {
    let a = line(Vec3::ZERO, Vec3::X);
    for count in [0, 1, 2049, usize::MAX] {
        assert!(a.curvature_comb(count).is_err());
    }
    assert_eq!(a.curvature_comb(2048).unwrap().len(), 2048);
    for tension in [f64::NAN, f64::INFINITY, -1.0, 0.0, 10.1] {
        assert!(a.tangent_blend_to(&a, tension).is_err());
    }
    assert!(a
        .tangent_blend_to(&line(Vec3::X, v(2.0, 0.0)), 1.0)
        .is_err());
    assert!(a
        .continuity_to(
            &a,
            ContinuityTolerance {
                distance: -1.0,
                ..Default::default()
            }
        )
        .is_err());
    assert!(a
        .continuity_to(
            &a,
            ContinuityTolerance {
                angle: 4.0,
                ..Default::default()
            }
        )
        .is_err());
    assert!(a
        .continuity_to(
            &a,
            ContinuityTolerance {
                curvature: f64::NAN,
                ..Default::default()
            }
        )
        .is_err());
    let malformed = Curve::Nurbs(NurbsCurve {
        degree: 2,
        control_points: vec![Vec3::ZERO, Vec3::X],
        weights: vec![1.0; 2],
        knots: vec![],
    });
    assert!(malformed.curvature_comb(2).is_err());
    let huge = Curve::Nurbs(NurbsCurve::from_points(&[Vec3::ZERO; 27], 26, false).unwrap());
    assert!(huge.curvature_comb(2).is_err());
    let dense = Curve::Polyline {
        points: vec![Vec3::ZERO; 16_385],
        closed: false,
    };
    assert!(dense.curvature_comb(2).is_err());
    assert!(line(Vec3::ZERO, v(f64::INFINITY, 0.0))
        .curvature_comb(2)
        .is_err());
    assert!(line(v(-f64::MAX, 0.0), v(f64::MAX, 0.0))
        .curvature_comb(2)
        .is_err());
}

#[test]
fn rational_circle_repeated_knots_keep_geometric_curvature_valid() {
    let w = std::f64::consts::FRAC_1_SQRT_2;
    let circle = Curve::Nurbs(NurbsCurve {
        degree: 2,
        control_points: vec![
            Vec3::X,
            v(1.0, 1.0),
            Vec3::Y,
            v(-1.0, 1.0),
            -Vec3::X,
            v(-1.0, -1.0),
            -Vec3::Y,
            v(1.0, -1.0),
            Vec3::X,
        ],
        weights: vec![1.0, w, 1.0, w, 1.0, w, 1.0, w, 1.0],
        knots: vec![
            0.0, 0.0, 0.0, 0.25, 0.25, 0.5, 0.5, 0.75, 0.75, 1.0, 1.0, 1.0,
        ],
    });
    let samples = circle.curvature_comb(16).unwrap();
    for sample in samples {
        assert!(sample.valid, "rational circle at {}", sample.parameter);
        near(sample.curvature.length(), 1.0);
    }
    // A degree-one knot with different speed, but no geometric corner.
    let straight = Curve::Nurbs(
        NurbsCurve::from_points(&[Vec3::ZERO, Vec3::X, v(3.0, 0.0)], 1, false).unwrap(),
    );
    assert!(straight.curvature_comb(3).unwrap().iter().all(|s| s.valid));
}

#[test]
fn arcs_past_a_full_turn_and_tiny_open_curves_keep_their_endpoints() {
    let arc = Curve::Arc {
        plane: Plane::world_xy(),
        radius: 1.0,
        start_angle: 0.0,
        end_angle: 2.5 * std::f64::consts::PI,
    };
    let samples = arc.curvature_comb(6).unwrap();
    assert_eq!(samples.last().unwrap().parameter, 1.0);
    let tiny = Curve::Polyline {
        points: vec![Vec3::ZERO, v(1e-15, 0.0), v(1e-15, 1e-15)],
        closed: false,
    };
    let samples = tiny.curvature_comb(5).unwrap();
    assert_eq!(
        samples.iter().map(|s| s.valid).collect::<Vec<_>>(),
        [true, true, false, true, true]
    );
    assert_eq!(samples[4].parameter, 1.0);
    let tiny_nurbs =
        Curve::Nurbs(NurbsCurve::from_points(&[Vec3::ZERO, v(1e-15, 0.0)], 1, false).unwrap());
    assert_eq!(tiny_nurbs.curvature_comb(2).unwrap()[1].parameter, 1.0);
}
