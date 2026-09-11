use mantis_kernel::{
    solid::{self, BooleanOp},
    Mat4, Mesh, Plane, Vec3,
};
use std::collections::BTreeMap;

const TOLERANCE: f64 = 1e-7;
fn cube(origin: Vec3, size: f64) -> Mesh {
    Mesh::box_mesh(&Plane::world_xy_at(origin), size, size, size)
}
fn volume(mesh: &Mesh, expected: f64) {
    assert!(
        (mesh.volume() - expected).abs() < 1e-7,
        "volume {} != {expected}",
        mesh.volume()
    );
}
/// Check actual output indices, not a tolerant re-weld: export must be closed.
fn closed(mesh: &Mesh) {
    let mut edges: BTreeMap<(u32, u32), (usize, i32)> = BTreeMap::new();
    for triangle in &mesh.indices {
        for i in 0..3 {
            let (a, b) = (triangle[i], triangle[(i + 1) % 3]);
            let count = edges.entry((a.min(b), a.max(b))).or_default();
            count.0 += 1;
            count.1 += if a < b { 1 } else { -1 };
        }
    }
    assert!(
        edges.values().all(|count| *count == (2, 0)),
        "non-manifold output edges: {:?}",
        edges
            .iter()
            .filter(|(_, count)| **count != (2, 0))
            .collect::<Vec<_>>()
    );
    assert!(mesh.positions.iter().all(|p| p.is_finite()));
    assert_eq!(mesh.normals.len(), mesh.positions.len());
}

#[test]
fn overlapping_boxes_have_correct_boolean_volumes_and_closed_indices() {
    let a = cube(Vec3::ZERO, 2.0);
    let b = cube(Vec3::new(1.0, 0.0, 0.0), 2.0);
    for (op, expected) in [
        (BooleanOp::Union, 12.0),
        (BooleanOp::Difference, 4.0),
        (BooleanOp::Intersection, 4.0),
    ] {
        let result = solid::boolean(&a, &b, op, TOLERANCE).unwrap();
        volume(&result, expected);
        closed(&result);
    }
    volume(&a, 8.0);
    volume(&b, 8.0);
}

#[test]
fn disjoint_contained_identical_and_empty_booleans() {
    let a = cube(Vec3::ZERO, 2.0);
    for (b, expected) in [
        (cube(Vec3::new(3.0, 0.0, 0.0), 1.0), [9.0, 8.0, 0.0]),
        (cube(Vec3::new(0.5, 0.5, 0.5), 1.0), [8.0, 7.0, 1.0]),
        (a.clone(), [8.0, 0.0, 8.0]),
        (Mesh::new(), [8.0, 8.0, 0.0]),
    ] {
        for (op, expected) in [
            BooleanOp::Union,
            BooleanOp::Difference,
            BooleanOp::Intersection,
        ]
        .into_iter()
        .zip(expected)
        {
            let result = solid::boolean(&a, &b, op, TOLERANCE).unwrap();
            volume(&result, expected);
            closed(&result);
        }
    }
}

#[test]
fn face_touching_boxes_union_removes_internal_face() {
    let a = cube(Vec3::ZERO, 1.0);
    let b = cube(Vec3::X, 1.0);
    let union = solid::boolean(&a, &b, BooleanOp::Union, TOLERANCE).unwrap();
    volume(&union, 2.0);
    closed(&union);
    assert!((union.area() - 10.0).abs() < 1e-7);
    assert!(solid::boolean(&a, &b, BooleanOp::Intersection, TOLERANCE)
        .unwrap()
        .indices
        .is_empty());
    volume(
        &solid::boolean(&a, &b, BooleanOp::Difference, TOLERANCE).unwrap(),
        1.0,
    );
}

#[test]
fn edge_or_point_touching_union_rejects_nonmanifold_result() {
    let a = cube(Vec3::ZERO, 1.0);
    for origin in [Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 1.0)] {
        let b = cube(origin, 1.0);
        assert!(solid::boolean(&a, &b, BooleanOp::Union, TOLERANCE).is_err());
        assert!(solid::boolean(&a, &b, BooleanOp::Intersection, TOLERANCE)
            .unwrap()
            .indices
            .is_empty());
    }
}

#[test]
fn plane_split_caps_are_closed_and_oriented_outwards() {
    let input = cube(Vec3::ZERO, 2.0);
    let plane = Plane::world_xy_at(Vec3::Z);
    let (negative, positive) = solid::split_plane(&input, &plane, TOLERANCE).unwrap();
    for mesh in [&negative, &positive] {
        volume(mesh, 4.0);
        closed(mesh);
    }
    for (mesh, sign) in [(&negative, 1.0), (&positive, -1.0)] {
        let mut caps = 0;
        for t in &mesh.indices {
            let [a, b, c] = t.map(|id| mesh.positions[id as usize]);
            if [a, b, c].iter().all(|p| (p.z - 1.0).abs() < TOLERANCE) {
                assert!((b - a).cross(c - a).z * sign > 0.0);
                caps += 1;
            }
        }
        assert!(caps > 0);
    }
}

#[test]
fn plane_split_oblique_hollow_and_outside_cases() {
    let input = cube(Vec3::ZERO, 2.0);
    let plane = Plane::from_normal(Vec3::new(1.0, 1.0, 1.0), Vec3::new(1.0, 2.0, 3.0));
    let (a, b) = solid::split_plane(&input, &plane, TOLERANCE).unwrap();
    volume(&a, 4.0);
    volume(&b, 4.0);
    closed(&a);
    closed(&b);
    let inner = cube(Vec3::new(0.5, 0.5, 0.5), 1.0);
    let hollow = solid::boolean(&input, &inner, BooleanOp::Difference, TOLERANCE).unwrap();
    let (a, b) = solid::split_plane(&hollow, &Plane::world_xy_at(Vec3::Z), TOLERANCE).unwrap();
    volume(&a, 3.5);
    volume(&b, 3.5);
    closed(&a);
    closed(&b);
    let (a, b) = solid::split_plane(
        &input,
        &Plane::world_xy_at(Vec3::new(0.0, 0.0, 3.0)),
        TOLERANCE,
    )
    .unwrap();
    volume(&a, 8.0);
    assert!(b.indices.is_empty());
}

#[test]
fn rotated_boxes_and_curved_meshes_work_without_axis_assumptions() {
    let a = cube(Vec3::new(-1.0, -1.0, -1.0), 2.0);
    let b = a.transformed(&Mat4::rotation_axis(
        Vec3::ZERO,
        Vec3::Z,
        std::f64::consts::FRAC_PI_4,
    ));
    let intersection = solid::boolean(&a, &b, BooleanOp::Intersection, TOLERANCE).unwrap();
    let expected = 16.0 * (2.0_f64.sqrt() - 1.0);
    volume(&intersection, expected);
    closed(&intersection);
    let sphere = Mesh::sphere(Vec3::ZERO, 0.75, 12, 6);
    let difference = solid::boolean(&a, &sphere, BooleanOp::Difference, TOLERANCE).unwrap();
    volume(&difference, a.volume() - sphere.volume());
    closed(&difference);
}

#[test]
fn invalid_open_inverted_degenerate_and_oversize_meshes_fail_cleanly() {
    let b = cube(Vec3::ZERO, 1.0);
    let mut open = b.clone();
    open.indices.pop();
    let mut inverted = b.clone();
    for t in &mut inverted.indices {
        t.swap(1, 2);
    }
    let mut bad_index = b.clone();
    bad_index.indices[0][0] = u32::MAX;
    let mut degenerate = b.clone();
    degenerate.indices[0][1] = degenerate.indices[0][0];
    let mut nonfinite = b.clone();
    nonfinite.positions[0].x = f64::NAN;
    let mut too_large = b.clone();
    too_large
        .indices
        .resize(solid::MAX_SOLID_TRIANGLES + 1, [0, 1, 2]);
    for invalid in [open, inverted, bad_index, degenerate, nonfinite, too_large] {
        assert!(solid::boolean(&invalid, &b, BooleanOp::Union, TOLERANCE).is_err());
    }
    for tolerance in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(solid::boolean(&b, &b, BooleanOp::Union, tolerance).is_err());
    }
}

#[test]
fn appended_intersecting_shells_are_rejected_even_when_each_shell_is_closed() {
    let a = cube(Vec3::ZERO, 2.0);
    for offset in [Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 0.5, 0.5)] {
        let mut overlapping = a.clone();
        overlapping.append(&cube(offset, 2.0));
        assert!(solid::validate_closed(&overlapping, TOLERANCE).is_err());
        assert!(solid::boolean(&overlapping, &a, BooleanOp::Difference, TOLERANCE).is_err());
    }
}

#[test]
fn separate_inverted_and_nested_outward_shells_are_rejected_before_boolean_shortcuts() {
    let outer = cube(Vec3::ZERO, 4.0);
    let mut inverted = cube(Vec3::new(6.0, 0.0, 0.0), 1.0);
    for triangle in &mut inverted.indices {
        triangle.swap(1, 2);
    }
    let mut separate = outer.clone();
    separate.append(&inverted);
    let mut nested = outer.clone();
    nested.append(&cube(Vec3::new(1.0, 1.0, 1.0), 2.0));
    for invalid in [separate, nested] {
        assert!(invalid.volume() > 0.0); // The old total-volume check accepted both.
        assert!(solid::validate_closed(&invalid, TOLERANCE).is_err());
        for operation in [
            BooleanOp::Union,
            BooleanOp::Difference,
            BooleanOp::Intersection,
        ] {
            assert!(solid::boolean(&invalid, &Mesh::new(), operation, TOLERANCE).is_err());
            assert!(solid::boolean(&outer, &invalid, operation, TOLERANCE).is_err());
        }
        assert!(solid::split_plane(&invalid, &Plane::world_xy(), TOLERANCE).is_err());
    }
}

#[test]
fn cavity_and_nested_material_island_winding_preserve_volumes_and_caps() {
    let outer = cube(Vec3::ZERO, 4.0);
    let mut cavity = cube(Vec3::new(1.0, 1.0, 1.0), 2.0);
    for triangle in &mut cavity.indices {
        triangle.swap(1, 2);
    }
    // Put the inward cavity first to avoid relying on shell traversal order.
    let mut hollow = cavity;
    hollow.append(&outer);
    solid::validate_closed(&hollow, TOLERANCE).unwrap();
    volume(
        &solid::boolean(&hollow, &Mesh::new(), BooleanOp::Union, TOLERANCE).unwrap(),
        56.0,
    );
    hollow.append(&cube(Vec3::new(1.75, 1.75, 1.75), 0.5));
    hollow.indices.reverse();
    solid::validate_closed(&hollow, TOLERANCE).unwrap();
    let enclosing = cube(Vec3::new(-1.0, -1.0, -1.0), 6.0);
    let intersection =
        solid::boolean(&hollow, &enclosing, BooleanOp::Intersection, TOLERANCE).unwrap();
    volume(&intersection, 56.125);
    closed(&intersection);
    let (negative, positive) = solid::split_plane(
        &hollow,
        &Plane::world_xy_at(Vec3::new(0.0, 0.0, 2.0)),
        TOLERANCE,
    )
    .unwrap();
    for half in [negative, positive] {
        volume(&half, 28.0625);
        closed(&half);
        solid::validate_closed(&half, TOLERANCE).unwrap();
    }
    let mut separate_outward = hollow;
    separate_outward.append(&cube(Vec3::new(6.0, 0.0, 0.0), 1.0));
    solid::validate_closed(&separate_outward, TOLERANCE).unwrap();
    volume(&separate_outward, 57.125);
}

#[test]
fn intersecting_sphere_and_box_conserve_partition_volume() {
    let a = cube(Vec3::ZERO, 2.0);
    let b = Mesh::sphere(Vec3::new(1.8, 1.0, 1.0), 0.8, 16, 8);
    let union = solid::boolean(&a, &b, BooleanOp::Union, TOLERANCE).unwrap();
    let difference = solid::boolean(&a, &b, BooleanOp::Difference, TOLERANCE).unwrap();
    let intersection = solid::boolean(&a, &b, BooleanOp::Intersection, TOLERANCE).unwrap();
    for mesh in [&union, &difference, &intersection] {
        closed(mesh);
    }
    volume(&union, a.volume() + b.volume() - intersection.volume());
    volume(&difference, a.volume() - intersection.volume());
    assert!(intersection.volume() > 0.0 && intersection.volume() < b.volume());
    // Reusing the same inputs must preserve vertex and triangle ordering.
    assert_eq!(
        union,
        solid::boolean(&a, &b, BooleanOp::Union, TOLERANCE).unwrap()
    );
}
