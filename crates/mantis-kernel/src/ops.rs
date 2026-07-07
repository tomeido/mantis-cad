//! Surface-generating operations (the Grasshopper verbs).
//!
//! All operations recompute normals before returning. Degenerate inputs
//! (zero-length extrusion direction, profiles with fewer than 3 points, …)
//! return an empty `Mesh`, or `Err` where the signature allows.

use crate::curve::Curve;
use crate::math::{BBox, Mat4, Plane, Vec3, EPS};
use crate::mesh::Mesh;

/// Profile points for sweep-style ops. Polylines contribute their exact
/// vertices (so rectangles extrude with sharp corners instead of being
/// resampled); every other curve is tessellated to `segments`.
/// Returns (points, closed); closed profiles carry NO duplicated seam point.
fn profile_points(curve: &Curve, segments: usize) -> (Vec<Vec3>, bool) {
    let closed = curve.is_closed();
    if let Curve::Line { a, b } = curve {
        return if a.distance(*b) > EPS { (vec![*a, *b], false) } else { (vec![*a], false) };
    }
    if let Curve::Polyline { points, .. } = curve {
        // Drop consecutive duplicates (and the seam duplicate when closed).
        let mut pts: Vec<Vec3> = Vec::with_capacity(points.len());
        for p in points {
            if pts.last().map_or(true, |q| q.distance(*p) > EPS) {
                pts.push(*p);
            }
        }
        if closed && pts.len() > 1 && pts[0].distance(pts[pts.len() - 1]) <= EPS {
            pts.pop();
        }
        (pts, closed)
    } else {
        (curve.tessellate(segments), closed)
    }
}

/// Newell normal of a polygon ring (not normalized; magnitude = 2 * area).
fn newell_normal(pts: &[Vec3]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

/// Ear-clip a (roughly planar, simple) polygon given in 3D. Points are
/// projected onto the best-fit plane derived from the Newell normal; emitted
/// triangles are wound CCW around that normal (i.e. their geometric normal
/// points along the polygon's Newell normal). Returns indices into `pts`.
/// Returns an empty Vec for degenerate rings (< 3 points or ~zero area).
/// Never panics and always terminates: if no strict ear is found (numerical
/// trouble / self-intersection), the most convex vertex is clipped anyway.
fn ear_clip(pts: &[Vec3]) -> Vec<[usize; 3]> {
    if pts.len() < 3 {
        return Vec::new();
    }
    let nn = newell_normal(pts);
    if nn.length() < EPS {
        return Vec::new();
    }
    let plane = Plane::from_normal(pts[0], nn);
    let p2: Vec<[f64; 2]> = pts
        .iter()
        .map(|p| {
            let d = *p - plane.origin;
            [d.dot(plane.x_axis), d.dot(plane.y_axis)]
        })
        .collect();
    let cross2 = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| -> f64 {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    // Signed area in the projection (positive when CCW around the normal).
    let mut area2 = 0.0;
    for i in 0..p2.len() {
        let a = p2[i];
        let b = p2[(i + 1) % p2.len()];
        area2 += a[0] * b[1] - b[0] * a[1];
    }
    let scale = BBox::from_points(pts).diagonal().max(EPS);
    let eps_area = 1e-12 * scale * scale;
    if area2.abs() < eps_area {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (0..pts.len()).collect();
    if area2 < 0.0 {
        idx.reverse(); // clip as CCW; emitted triangles stay CCW in projection
    }
    let inside = |a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]| -> bool {
        cross2(a, b, p) >= -eps_area
            && cross2(b, c, p) >= -eps_area
            && cross2(c, a, p) >= -eps_area
    };
    let mut tris = Vec::with_capacity(pts.len().saturating_sub(2));
    while idx.len() > 3 {
        let m = idx.len();
        let mut clipped = false;
        let mut best = 0usize;
        let mut best_cross = f64::NEG_INFINITY;
        for i in 0..m {
            let (ip, ic, inx) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (p2[ip], p2[ic], p2[inx]);
            let cr = cross2(a, b, c);
            if cr > best_cross {
                best_cross = cr;
                best = i;
            }
            if cr <= eps_area {
                continue; // reflex or degenerate corner
            }
            // No other remaining vertex inside the candidate ear.
            let blocked = idx.iter().enumerate().any(|(j, &vj)| {
                j != (i + m - 1) % m && j != i && j != (i + 1) % m && inside(a, b, c, p2[vj])
            });
            if !blocked {
                tris.push([ip, ic, inx]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            // Degenerate/self-intersecting input: clip the most convex vertex
            // anyway so we always terminate.
            let m = idx.len();
            let (ip, ic, inx) = (idx[(best + m - 1) % m], idx[best], idx[(best + 1) % m]);
            tris.push([ip, ic, inx]);
            idx.remove(best);
        }
    }
    tris.push([idx[0], idx[1], idx[2]]);
    // Drop degenerate output triangles (can appear via the fallback path).
    tris.retain(|t| {
        let cr = cross2(p2[t[0]], p2[t[1]], p2[t[2]]);
        cr.abs() > eps_area
    });
    tris
}

/// Extrude a curve along `dir`. `segments` controls the profile tessellation
/// (polylines keep their exact vertices). Closed profiles are re-oriented so
/// their Newell normal has a positive component along `dir`, get top and
/// bottom caps (ear clipping in the best-fit plane), and produce a closed
/// mesh with positive volume. Open profiles produce an uncapped wall.
/// Zero-length `dir` or fewer than 2 profile points give an empty mesh.
pub fn extrude(curve: &Curve, dir: Vec3, segments: usize) -> Mesh {
    // `NaN < EPS` is false, so a plain magnitude test would let a non-finite
    // direction through and seed a NaN mesh — reject it explicitly.
    if !dir.is_finite() || dir.length() < EPS {
        return Mesh::new();
    }
    let (mut pts, closed) = profile_points(curve, segments.max(1));
    if pts.len() < 2 || (closed && pts.len() < 3) {
        return Mesh::new();
    }
    let nn = if closed { newell_normal(&pts) } else { Vec3::ZERO };
    if closed && nn.dot(dir) < 0.0 {
        pts.reverse(); // make the profile CCW as seen along +dir
    }
    let n = pts.len();
    let mut mesh = Mesh::new();
    mesh.positions.extend(pts.iter().copied()); // bottom ring: 0..n
    mesh.positions.extend(pts.iter().map(|p| *p + dir)); // top ring: n..2n
    let quads = if closed { n } else { n - 1 };
    for i in 0..quads {
        let a = i as u32;
        let b = ((i + 1) % n) as u32;
        let c = (n + (i + 1) % n) as u32;
        let d = (n + i) as u32;
        mesh.indices.push([a, b, c]);
        mesh.indices.push([a, c, d]);
    }
    if closed {
        let tris = ear_clip(&pts);
        // Cap triangles are CCW around the (re-oriented) profile normal,
        // which points along +dir: use directly for the top, flipped for the
        // bottom, so both face outward.
        for t in &tris {
            mesh.indices.push([
                (n + t[0]) as u32,
                (n + t[1]) as u32,
                (n + t[2]) as u32,
            ]);
            mesh.indices.push([t[0] as u32, t[2] as u32, t[1] as u32]);
        }
    }
    mesh.recompute_normals();
    mesh
}

/// Revolve a profile curve around axis (origin+dir) by `angle` radians in
/// `segments` steps (2π = full revolution). A full turn welds the seam by
/// index wrap; partial sweeps are open (no end caps). Smooth profiles are
/// tessellated to 32 segments; polyline profiles keep their exact vertices.
/// Zero axis or ~zero angle gives an empty mesh.
pub fn revolve(
    profile: &Curve,
    axis_origin: Vec3,
    axis_dir: Vec3,
    angle: f64,
    segments: usize,
) -> Mesh {
    if !axis_dir.is_finite()
        || !axis_origin.is_finite()
        || axis_dir.length() < EPS
        || angle.abs() < EPS
        || !angle.is_finite()
    {
        return Mesh::new();
    }
    let (pts, profile_closed) = profile_points(profile, 32);
    if pts.len() < 2 {
        return Mesh::new();
    }
    let full = (angle.abs() - std::f64::consts::TAU).abs() < 1e-7;
    let steps = if full { segments.max(3) } else { segments.max(1) };
    let rows = if full { steps } else { steps + 1 };
    let n = pts.len();
    let mut mesh = Mesh::new();
    for s in 0..rows {
        let m = Mat4::rotation_axis(axis_origin, axis_dir, angle * s as f64 / steps as f64);
        mesh.positions.extend(pts.iter().map(|p| m.transform_point(*p)));
    }
    let row_quads = if full { rows } else { rows - 1 };
    let col_quads = if profile_closed { n } else { n - 1 };
    for s in 0..row_quads {
        let s1 = (s + 1) % rows;
        for j in 0..col_quads {
            let j1 = (j + 1) % n;
            let a = (s * n + j) as u32;
            let b = (s1 * n + j) as u32;
            let c = (s1 * n + j1) as u32;
            let d = (s * n + j1) as u32;
            mesh.indices.push([a, b, c]);
            mesh.indices.push([a, c, d]);
        }
    }
    mesh.recompute_normals();
    mesh
}

/// Skin through 2+ section curves. Each section is tessellated to the same
/// point count: if ALL sections are closed the tube is closed (rings of
/// `segments_per_section` points, welded by index wrap); otherwise every
/// row uses `segments_per_section + 1` points (closed sections get their
/// seam point duplicated to match). Errors if fewer than 2 sections.
pub fn loft(sections: &[Curve], segments_per_section: usize) -> Result<Mesh, String> {
    if sections.len() < 2 {
        return Err(format!("loft needs at least 2 sections, got {}", sections.len()));
    }
    let segs = segments_per_section.max(1);
    let all_closed = sections.iter().all(|s| s.is_closed());
    let mut rows: Vec<Vec<Vec3>> = Vec::with_capacity(sections.len());
    for s in sections {
        let mut pts = s.tessellate(segs);
        if !all_closed && s.is_closed() {
            // Duplicate the seam so every row has segs+1 points.
            let first = pts[0];
            pts.push(first);
        }
        rows.push(pts);
    }
    let n = rows[0].len();
    if n < 2 {
        return Err("loft sections tessellated to fewer than 2 points".into());
    }
    debug_assert!(rows.iter().all(|r| r.len() == n));
    let mut mesh = Mesh::new();
    for row in &rows {
        mesh.positions.extend(row.iter().copied());
    }
    let col_quads = if all_closed { n } else { n - 1 };
    for s in 0..rows.len() - 1 {
        for j in 0..col_quads {
            let j1 = (j + 1) % n;
            let a = (s * n + j) as u32;
            let b = (s * n + j1) as u32;
            let c = ((s + 1) * n + j1) as u32;
            let d = ((s + 1) * n + j) as u32;
            mesh.indices.push([a, b, c]);
            mesh.indices.push([a, c, d]);
        }
    }
    mesh.recompute_normals();
    Ok(mesh)
}

/// Circular tube swept along a rail curve using parallel-transport frames
/// (rotation-minimizing; chord-based tangents). Closed rails weld the seam
/// by index wrap and distribute the transport holonomy evenly along the rail
/// so the seam does not twist; open rails get flat end caps. Degenerate
/// rails (zero length) or non-positive radius give an empty mesh.
pub fn pipe(rail: &Curve, radius: f64, rail_segments: usize, ring_segments: usize) -> Mesh {
    if !(radius > EPS) {
        return Mesh::new();
    }
    let closed = rail.is_closed();
    let pts = rail.tessellate(rail_segments.max(if closed { 3 } else { 1 }));
    let m = pts.len();
    if m < 2 {
        return Mesh::new();
    }
    let ring_n = ring_segments.max(3);
    // Chord-based tangents.
    let tangent = |i: usize| -> Vec3 {
        let (prev, next) = if closed {
            (pts[(i + m - 1) % m], pts[(i + 1) % m])
        } else {
            (pts[i.saturating_sub(1)], pts[(i + 1).min(m - 1)])
        };
        (next - prev).normalized()
    };
    let t0 = tangent(0);
    if t0 == Vec3::ZERO {
        return Mesh::new(); // rail has no extent
    }
    // Parallel transport of the initial normal along the tangent sequence.
    let transport = |n_prev: Vec3, t_prev: Vec3, t_new: Vec3| -> Vec3 {
        let axis = t_prev.cross(t_new);
        let s = axis.length();
        if s < EPS || t_new == Vec3::ZERO {
            return n_prev;
        }
        let ang = s.min(1.0).asin().max(0.0);
        let ang = if t_prev.dot(t_new) < 0.0 { std::f64::consts::PI - ang } else { ang };
        let rot = Mat4::rotation_axis(Vec3::ZERO, axis, ang);
        rot.transform_vector(n_prev)
    };
    let mut tangents = Vec::with_capacity(m);
    let mut frames_n = Vec::with_capacity(m);
    let mut n_cur = t0.any_perpendicular();
    if n_cur == Vec3::ZERO {
        return Mesh::new();
    }
    let mut t_prev = t0;
    for i in 0..m {
        let t_i = {
            let t = tangent(i);
            if t == Vec3::ZERO {
                t_prev
            } else {
                t
            }
        };
        if i > 0 {
            n_cur = transport(n_cur, t_prev, t_i);
        }
        // Re-orthonormalize against drift.
        n_cur = (n_cur - t_i * n_cur.dot(t_i)).normalized();
        if n_cur == Vec3::ZERO {
            n_cur = t_i.any_perpendicular();
        }
        tangents.push(t_i);
        frames_n.push(n_cur);
        t_prev = t_i;
    }
    // Closed rails: measure the holonomy (angle between the frame carried
    // once around the loop and the starting frame) and untwist gradually.
    if closed {
        let n_back = {
            let n = transport(frames_n[m - 1], tangents[m - 1], t0);
            (n - t0 * n.dot(t0)).normalized()
        };
        if n_back != Vec3::ZERO {
            let b0 = t0.cross(frames_n[0]);
            let defect = n_back.dot(b0).atan2(n_back.dot(frames_n[0]));
            for (i, (nf, tf)) in frames_n.iter_mut().zip(&tangents).enumerate() {
                let corr = -defect * i as f64 / m as f64;
                let rot = Mat4::rotation_axis(Vec3::ZERO, *tf, corr);
                *nf = rot.transform_vector(*nf).normalized();
            }
        }
    }
    // Build rings.
    let mut mesh = Mesh::new();
    for i in 0..m {
        let (t_i, n_i) = (tangents[i], frames_n[i]);
        let b_i = t_i.cross(n_i).normalized();
        for k in 0..ring_n {
            let phi = std::f64::consts::TAU * k as f64 / ring_n as f64;
            mesh.positions.push(pts[i] + (n_i * phi.cos() + b_i * phi.sin()) * radius);
        }
    }
    let ring_rows = if closed { m } else { m - 1 };
    for i in 0..ring_rows {
        let i1 = (i + 1) % m;
        for k in 0..ring_n {
            let k1 = (k + 1) % ring_n;
            let a = (i * ring_n + k) as u32;
            let b = (i * ring_n + k1) as u32;
            let c = (i1 * ring_n + k1) as u32;
            let d = (i1 * ring_n + k) as u32;
            mesh.indices.push([a, b, c]);
            mesh.indices.push([a, c, d]);
        }
    }
    if !closed {
        // End caps: triangle fans around the rail endpoints.
        let start_center = mesh.positions.len() as u32;
        mesh.positions.push(pts[0]);
        let end_center = mesh.positions.len() as u32;
        mesh.positions.push(pts[m - 1]);
        let last = ((m - 1) * ring_n) as u32;
        for k in 0..ring_n as u32 {
            let k1 = (k + 1) % ring_n as u32;
            mesh.indices.push([start_center, k1, k]); // outward -t0
            mesh.indices.push([end_center, last + k, last + k1]); // outward +t_end
        }
    }
    mesh.recompute_normals();
    mesh
}

/// Fill a closed (approximately planar) curve with triangles (ear clipping
/// in the best-fit Newell plane). `segments` controls tessellation of smooth
/// curves; polyline boundaries keep their exact vertices. The surface normal
/// follows the curve orientation (right-hand rule). Errors on open curves
/// and on degenerate (zero-area / collinear) boundaries.
pub fn planar_surface(boundary: &Curve, segments: usize) -> Result<Mesh, String> {
    if !boundary.is_closed() {
        return Err("planar_surface: boundary curve is not closed".into());
    }
    let (pts, _) = profile_points(boundary, segments.max(3));
    if pts.len() < 3 {
        return Err("planar_surface: boundary has fewer than 3 distinct points".into());
    }
    let tris = ear_clip(&pts);
    if tris.is_empty() {
        return Err("planar_surface: degenerate (zero-area) boundary".into());
    }
    let mut mesh = Mesh::new();
    mesh.positions = pts;
    mesh.indices = tris.iter().map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).collect();
    mesh.recompute_normals();
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::NurbsCurve;
    use std::f64::consts::{PI, TAU};

    fn unit_circle(r: f64) -> Curve {
        Curve::Circle { plane: Plane::world_xy(), radius: r }
    }

    fn rect(w: f64, h: f64) -> Curve {
        Curve::Polyline {
            points: vec![
                Vec3::ZERO,
                Vec3::new(w, 0.0, 0.0),
                Vec3::new(w, h, 0.0),
                Vec3::new(0.0, h, 0.0),
            ],
            closed: true,
        }
    }

    /// Concave L-shape (6 vertices), area = 3.
    fn l_shape() -> Curve {
        Curve::Polyline {
            points: vec![
                Vec3::ZERO,
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 1.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(1.0, 2.0, 0.0),
                Vec3::new(0.0, 2.0, 0.0),
            ],
            closed: true,
        }
    }

    #[test]
    fn extrude_circle_volume_positive() {
        let m = extrude(&unit_circle(1.0), Vec3::Z * 2.0, 64);
        let exact = PI * 2.0;
        assert!(m.volume() > 0.0);
        assert!((m.volume() - exact).abs() / exact < 0.02, "vol {}", m.volume());
        // Lateral + 2 caps.
        let exact_a = TAU * 2.0 + 2.0 * PI;
        assert!((m.area() - exact_a).abs() / exact_a < 0.02);
    }

    #[test]
    fn extrude_against_normal_still_positive_volume() {
        // Extruding DOWN must flip the profile so volume stays positive.
        let m = extrude(&unit_circle(1.0), Vec3::Z * -2.0, 64);
        assert!(m.volume() > 0.0, "vol {}", m.volume());
    }

    #[test]
    fn extrude_rect_exact() {
        let m = extrude(&rect(2.0, 3.0), Vec3::Z * 4.0, 16);
        assert!((m.volume() - 24.0).abs() < 1e-9, "vol {}", m.volume());
        assert!((m.area() - 2.0 * (6.0 + 12.0 + 8.0)).abs() < 1e-9);
    }

    #[test]
    fn extrude_concave_polyline_ear_clipping() {
        let m = extrude(&l_shape(), Vec3::Z * 2.0, 16);
        assert!((m.volume() - 6.0).abs() < 1e-9, "vol {}", m.volume());
    }

    #[test]
    fn extrude_open_curve_is_uncapped_wall() {
        let line = Curve::Line { a: Vec3::ZERO, b: Vec3::X * 3.0 };
        let m = extrude(&line, Vec3::Z * 2.0, 1);
        assert!((m.area() - 6.0).abs() < 1e-9);
        assert!(m.volume().abs() < 1e-9);
    }

    #[test]
    fn extrude_degenerate_inputs_empty() {
        assert_eq!(extrude(&unit_circle(1.0), Vec3::ZERO, 32).vertex_count(), 0);
        let pt = Curve::Polyline { points: vec![Vec3::ZERO], closed: false };
        assert_eq!(extrude(&pt, Vec3::Z, 8).vertex_count(), 0);
        let two = Curve::Polyline { points: vec![Vec3::ZERO, Vec3::X, Vec3::ZERO], closed: true };
        assert_eq!(extrude(&two, Vec3::Z, 8).vertex_count(), 0);
    }

    #[test]
    fn revolve_full_turn_torus_volume() {
        // Circle of radius 1 in the xz-plane, centered at (3,0,0), revolved
        // around the world Z axis -> torus R=3 r=1.
        let profile = Curve::Circle {
            plane: Plane {
                origin: Vec3::new(3.0, 0.0, 0.0),
                x_axis: Vec3::X,
                y_axis: Vec3::Z,
            },
            radius: 1.0,
        };
        let m = revolve(&profile, Vec3::ZERO, Vec3::Z, TAU, 64);
        let exact = 2.0 * PI * PI * 3.0;
        assert!(m.volume() > 0.0, "vol {}", m.volume());
        assert!((m.volume() - exact).abs() / exact < 0.04, "vol {}", m.volume());
        // Full turn welds the seam: rows * profile points, no extra row.
        assert_eq!(m.vertex_count(), 64 * 32);
    }

    #[test]
    fn revolve_partial_open() {
        let profile = Curve::Line { a: Vec3::new(1.0, 0.0, 0.0), b: Vec3::new(1.0, 0.0, 2.0) };
        let m = revolve(&profile, Vec3::ZERO, Vec3::Z, PI, 32);
        // Half a cylinder shell of radius 1, height 2: area = π * 1 * 2 * ... = 2π.
        let exact = PI * 2.0;
        assert!((m.area() - exact).abs() / exact < 0.01, "area {}", m.area());
        assert_eq!(m.vertex_count(), 33 * 2);
    }

    #[test]
    fn revolve_degenerate_empty() {
        let profile = Curve::Line { a: Vec3::X, b: Vec3::X * 2.0 };
        assert_eq!(revolve(&profile, Vec3::ZERO, Vec3::ZERO, TAU, 16).vertex_count(), 0);
        assert_eq!(revolve(&profile, Vec3::ZERO, Vec3::Z, 0.0, 16).vertex_count(), 0);
    }

    #[test]
    fn loft_two_circles_is_cylinder_side() {
        let bottom = unit_circle(1.0);
        let top = Curve::Circle { plane: Plane::world_xy_at(Vec3::Z * 3.0), radius: 1.0 };
        let m = loft(&[bottom, top], 64).unwrap();
        let exact = TAU * 3.0; // lateral area 2π r h
        assert!((m.area() - exact).abs() / exact < 0.01, "area {}", m.area());
        assert_eq!(m.vertex_count(), 2 * 64); // closed rings, welded seams
        assert_eq!(m.triangle_count(), 2 * 64);
    }

    #[test]
    fn loft_capped_by_planar_surfaces_matches_cylinder_volume() {
        let bottom = unit_circle(1.0);
        let top = Curve::Circle { plane: Plane::world_xy_at(Vec3::Z * 3.0), radius: 1.0 };
        let mut m = loft(&[bottom.clone(), top.clone()], 64).unwrap();
        // Cap it: bottom disc flipped (normal -Z), top disc as-is (+Z).
        let mut bot_cap = planar_surface(&bottom, 64).unwrap();
        for t in &mut bot_cap.indices {
            t.swap(1, 2);
        }
        let top_cap = planar_surface(&top, 64).unwrap();
        m.append(&bot_cap);
        m.append(&top_cap);
        let exact = PI * 3.0;
        assert!((m.volume() - exact).abs() / exact < 0.01, "vol {}", m.volume());
    }

    #[test]
    fn loft_errors_on_too_few_sections() {
        assert!(loft(&[], 16).is_err());
        assert!(loft(&[unit_circle(1.0)], 16).is_err());
    }

    #[test]
    fn loft_mixed_open_closed_counts_match() {
        let open = Curve::Line { a: Vec3::ZERO, b: Vec3::X };
        let closed = Curve::Circle { plane: Plane::world_xy_at(Vec3::Z), radius: 0.5 };
        let m = loft(&[open, closed], 12).unwrap();
        assert_eq!(m.vertex_count(), 2 * 13);
        assert!(m.triangle_count() > 0);
    }

    #[test]
    fn pipe_straight_rail_is_cylinder() {
        let rail = Curve::Line { a: Vec3::ZERO, b: Vec3::Z * 5.0 };
        let m = pipe(&rail, 0.5, 8, 48);
        let exact = PI * 0.25 * 5.0;
        assert!(m.volume() > 0.0);
        assert!((m.volume() - exact).abs() / exact < 0.01, "vol {}", m.volume());
    }

    #[test]
    fn pipe_closed_rail_is_torus() {
        let rail = unit_circle(3.0);
        let m = pipe(&rail, 1.0, 96, 32);
        let exact = 2.0 * PI * PI * 3.0;
        assert!((m.volume().abs() - exact).abs() / exact < 0.04, "vol {}", m.volume());
        // Welded: rails*rings vertices, no caps.
        assert_eq!(m.vertex_count(), 96 * 32);
    }

    #[test]
    fn pipe_degenerate_empty() {
        let rail = Curve::Line { a: Vec3::ZERO, b: Vec3::ZERO };
        assert_eq!(pipe(&rail, 0.5, 8, 8).vertex_count(), 0);
        let rail = Curve::Line { a: Vec3::ZERO, b: Vec3::Z };
        assert_eq!(pipe(&rail, 0.0, 8, 8).vertex_count(), 0);
        assert_eq!(pipe(&rail, -1.0, 8, 8).vertex_count(), 0);
    }

    #[test]
    fn planar_surface_circle_area() {
        let m = planar_surface(&unit_circle(2.0), 128).unwrap();
        let exact = PI * 4.0;
        assert!((m.area() - exact).abs() / exact < 0.01, "area {}", m.area());
        // Normal follows curve orientation: CCW circle in XY -> +Z.
        assert!(m.normals.iter().all(|n| n.distance(Vec3::Z) < 1e-9));
    }

    #[test]
    fn planar_surface_concave_exact() {
        let m = planar_surface(&l_shape(), 16).unwrap();
        assert!((m.area() - 3.0).abs() < 1e-9, "area {}", m.area());
        assert_eq!(m.triangle_count(), 4); // n-2 triangles for n=6
        // All triangles must lie inside the L (concavity respected):
        // centroid of each triangle is inside the polygon.
        for t in &m.indices {
            let c = (m.positions[t[0] as usize]
                + m.positions[t[1] as usize]
                + m.positions[t[2] as usize])
                / 3.0;
            let inside_l = (c.x >= 0.0 && c.x <= 2.0 && c.y >= 0.0 && c.y <= 1.0)
                || (c.x >= 0.0 && c.x <= 1.0 && c.y >= 0.0 && c.y <= 2.0);
            assert!(inside_l, "triangle centroid {:?} escaped the L", c);
        }
    }

    #[test]
    fn planar_surface_clockwise_polygon_normal_follows_orientation() {
        // Clockwise square (as seen from +Z): normal must be -Z.
        let cw = Curve::Polyline {
            points: vec![
                Vec3::ZERO,
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
            ],
            closed: true,
        };
        let m = planar_surface(&cw, 4).unwrap();
        assert!((m.area() - 1.0).abs() < 1e-9);
        assert!(m.normals.iter().all(|n| n.distance(-Vec3::Z) < 1e-9));
    }

    #[test]
    fn planar_surface_errors() {
        let open = Curve::Line { a: Vec3::ZERO, b: Vec3::X };
        assert!(planar_surface(&open, 8).is_err());
        // Closed but zero-area (back-and-forth) polyline.
        let flat = Curve::Polyline {
            points: vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0, Vec3::X],
            closed: true,
        };
        assert!(planar_surface(&flat, 8).is_err());
    }

    #[test]
    fn nurbs_profile_ops_smoke() {
        // Closed NURBS profile through 6 points, extruded: positive volume.
        let pts: Vec<Vec3> = (0..6)
            .map(|i| {
                let a = TAU * i as f64 / 6.0;
                Vec3::new(a.cos() * 2.0, a.sin() * 2.0, 0.0)
            })
            .collect();
        let nc = NurbsCurve::from_points(&pts, 3, true).unwrap();
        let c = Curve::Nurbs(nc);
        assert!(c.is_closed());
        let m = extrude(&c, Vec3::Z, 48);
        assert!(m.volume() > 0.0);
        assert!(m.triangle_count() > 0);
    }

    #[test]
    fn all_ops_have_normals() {
        let m = extrude(&unit_circle(1.0), Vec3::Z, 16);
        assert_eq!(m.normals.len(), m.positions.len());
        let m = pipe(&Curve::Line { a: Vec3::ZERO, b: Vec3::Z }, 0.3, 4, 8);
        assert_eq!(m.normals.len(), m.positions.len());
        let m = loft(
            &[unit_circle(1.0), Curve::Circle { plane: Plane::world_xy_at(Vec3::Z), radius: 1.0 }],
            16,
        )
        .unwrap();
        assert_eq!(m.normals.len(), m.positions.len());
    }
}
