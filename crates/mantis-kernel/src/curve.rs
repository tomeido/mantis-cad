//! Curve types. All parametrized over normalized t in [0,1].
//!
//! Parametrization notes:
//! * `Line`: linear in t.
//! * `Polyline`: chord-length parametrized (t proportional to distance along
//!   the polyline), so `divide` yields evenly spaced points.
//! * `Circle`/`Arc`: angular (t linear in angle) — evaluation is exact.
//! * `Nurbs`: t mapped linearly onto the active knot domain
//!   `[knots[degree], knots[n]]`, evaluated with rational de Boor.

use crate::math::{BBox, Mat4, Plane, Vec3, EPS};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    Line { a: Vec3, b: Vec3 },
    /// `closed` implies an implicit segment from last back to first point.
    Polyline { points: Vec<Vec3>, closed: bool },
    Circle { plane: Plane, radius: f64 },
    /// Angles in radians measured in the plane from x_axis toward y_axis.
    Arc { plane: Plane, radius: f64, start_angle: f64, end_angle: f64 },
    Nurbs(NurbsCurve),
}

/// Non-rational-capable NURBS curve (weights supported).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NurbsCurve {
    pub degree: usize,
    pub control_points: Vec<Vec3>,
    pub weights: Vec<f64>,
    /// Clamped knot vector, length = control_points.len() + degree + 1.
    /// (Curves built with `from_points(.., closed=true)` instead carry a
    /// uniform *unclamped/periodic* knot vector of the same length.)
    pub knots: Vec<f64>,
}

/// Clamped uniform knot vector for `n` control points of `degree`:
/// `degree+1` zeros, uniformly increasing interior knots, `degree+1` ones.
fn clamped_uniform_knots(n: usize, degree: usize) -> Vec<f64> {
    let mut knots = Vec::with_capacity(n + degree + 1);
    let interior = n - degree; // number of spans; n > degree assumed
    for _ in 0..=degree {
        knots.push(0.0);
    }
    for i in 1..interior {
        knots.push(i as f64 / interior as f64);
    }
    for _ in 0..=degree {
        knots.push(1.0);
    }
    knots
}

impl NurbsCurve {
    /// Interpolating-ish smooth curve through `points` (uniform clamped knots,
    /// points used as control points — Grasshopper "Nurbs Curve" behavior).
    /// Degree is clamped to points.len()-1. Panics never; returns None if
    /// points.len() < 2.
    ///
    /// `closed == true` builds a *periodic* curve: the first `degree` control
    /// points are appended again at the end (`n + degree` control points
    /// total) and the knot vector is uniform and unclamped
    /// (`0, 1, 2, …, n + 2*degree`), giving an exactly closed curve with
    /// C^(degree-1) continuity across the seam.
    pub fn from_points(points: &[Vec3], degree: usize, closed: bool) -> Option<NurbsCurve> {
        if points.len() < 2 {
            return None;
        }
        let n = points.len();
        let degree = degree.clamp(1, n - 1);
        if closed {
            let mut cps = points.to_vec();
            cps.extend_from_slice(&points[..degree]);
            let m = cps.len(); // n + degree
            let knots = (0..m + degree + 1).map(|i| i as f64).collect();
            Some(NurbsCurve { degree, control_points: cps, weights: vec![1.0; m], knots })
        } else {
            Some(NurbsCurve {
                degree,
                control_points: points.to_vec(),
                weights: vec![1.0; n],
                knots: clamped_uniform_knots(n, degree),
            })
        }
    }

    /// De Boor evaluation at normalized t in [0,1].
    ///
    /// Robust against malformed data: a wrong-length knot vector is replaced
    /// (locally, without mutating self) by a clamped uniform one; missing
    /// weights default to 1; non-positive weights are clamped to a tiny
    /// positive value. Never panics.
    pub fn point_at(&self, t: f64) -> Vec3 {
        let n = self.control_points.len();
        if n == 0 {
            return Vec3::ZERO;
        }
        if n == 1 {
            return self.control_points[0];
        }
        let p = self.degree.min(n - 1);
        // Validate/repair the knot vector.
        let repaired;
        let knots: &[f64] = if self.knots.len() == n + p + 1
            && self.knots.windows(2).all(|w| w[0] <= w[1])
            && self.knots.iter().all(|k| k.is_finite())
        {
            &self.knots
        } else {
            repaired = clamped_uniform_knots(n, p.max(1));
            if repaired.len() != n + p + 1 {
                // p == 0 fallback: piecewise-constant; just return nearest cp.
                let i = ((t.clamp(0.0, 1.0) * (n - 1) as f64).round() as usize).min(n - 1);
                return self.control_points[i];
            }
            &repaired
        };
        let w = |i: usize| self.weights.get(i).copied().unwrap_or(1.0).max(1e-12);

        let lo = knots[p];
        let hi = knots[n];
        if !(hi - lo).is_finite() || hi - lo < 1e-12 {
            return self.control_points[0];
        }
        let u = lo + (hi - lo) * t.clamp(0.0, 1.0);
        // Knot span k in [p, n-1] with knots[k] <= u (< knots[k+1] except at end).
        let mut k = p;
        while k + 1 < n && u >= knots[k + 1] {
            k += 1;
        }
        // Rational de Boor in homogeneous coordinates.
        let mut dx: Vec<Vec3> = (0..=p).map(|j| self.control_points[j + k - p] * w(j + k - p)).collect();
        let mut dw: Vec<f64> = (0..=p).map(|j| w(j + k - p)).collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = j + k - p;
                let den = knots[i + p - r + 1] - knots[i];
                let a = if den.abs() < 1e-12 { 0.0 } else { (u - knots[i]) / den };
                dx[j] = dx[j - 1] * (1.0 - a) + dx[j] * a;
                dw[j] = dw[j - 1] * (1.0 - a) + dw[j] * a;
            }
        }
        if dw[p].abs() < 1e-12 {
            dx[p]
        } else {
            dx[p] / dw[p]
        }
    }
}

/// Effective vertex list of a polyline for evaluation: if `closed` and the
/// endpoints do not already coincide, the first point is appended.
fn polyline_effective(points: &[Vec3], closed: bool) -> Vec<Vec3> {
    let mut pts = points.to_vec();
    if closed {
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            if first.distance(*last) > EPS {
                pts.push(*first);
            }
        }
    }
    pts
}

/// Cumulative chord lengths (len = pts.len()); returns (cumulative, total).
fn cumulative_lengths(pts: &[Vec3]) -> (Vec<f64>, f64) {
    let mut cum = Vec::with_capacity(pts.len());
    let mut acc = 0.0;
    cum.push(0.0);
    for i in 1..pts.len() {
        acc += pts[i - 1].distance(pts[i]);
        cum.push(acc);
    }
    (cum, acc)
}

/// Point on a chord-length parametrized polyline at t in [0,1].
fn polyline_point(pts: &[Vec3], t: f64) -> Vec3 {
    match pts.len() {
        0 => Vec3::ZERO,
        1 => pts[0],
        _ => {
            let (cum, total) = cumulative_lengths(pts);
            if total < EPS {
                return pts[0];
            }
            let target = total * t.clamp(0.0, 1.0);
            // Find segment containing target.
            let mut i = 0;
            while i + 2 < pts.len() && cum[i + 1] < target {
                i += 1;
            }
            let seg = cum[i + 1] - cum[i];
            let local = if seg < EPS { 0.0 } else { (target - cum[i]) / seg };
            pts[i].lerp(pts[i + 1], local)
        }
    }
}

/// Unit direction of the polyline segment containing t (chord-length param).
fn polyline_tangent(pts: &[Vec3], t: f64) -> Vec3 {
    if pts.len() < 2 {
        return Vec3::ZERO;
    }
    let (cum, total) = cumulative_lengths(pts);
    if total < EPS {
        return Vec3::ZERO;
    }
    let target = total * t.clamp(0.0, 1.0);
    let mut i = 0;
    while i + 2 < pts.len() && cum[i + 1] <= target {
        i += 1;
    }
    // Skip zero-length segments deterministically (prefer forward).
    let mut j = i;
    while j + 1 < pts.len() && pts[j].distance(pts[j + 1]) < EPS {
        j += 1;
    }
    if j + 1 < pts.len() {
        (pts[j + 1] - pts[j]).normalized()
    } else {
        (pts[i + 1] - pts[i]).normalized()
    }
}

/// Exact rational-quadratic NURBS representation of a circular arc from
/// `a0` to `a1` (radians, in-plane). Splits the sweep into <= 90° pieces;
/// weights are cos(dθ/2) at the tangent-intersection control points.
/// Affine transforms of its control points reproduce the transformed conic
/// exactly, which is how ellipses (non-uniformly scaled circles) are handled.
fn arc_to_nurbs(plane: &Plane, radius: f64, a0: f64, a1: f64) -> NurbsCurve {
    let sweep = a1 - a0;
    let on = |ang: f64, r: f64| plane.point_at(r * ang.cos(), r * ang.sin());
    if sweep.abs() < EPS || radius.abs() < EPS {
        // Degenerate: a two-point "curve" at the same location.
        let p = on(a0, radius);
        return NurbsCurve {
            degree: 1,
            control_points: vec![p, p],
            weights: vec![1.0, 1.0],
            knots: vec![0.0, 0.0, 1.0, 1.0],
        };
    }
    let narcs = ((sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).max(1);
    let dth = sweep / narcs as f64;
    let w1 = (dth / 2.0).cos();
    let mut cps = Vec::with_capacity(2 * narcs + 1);
    let mut wts = Vec::with_capacity(2 * narcs + 1);
    cps.push(on(a0, radius));
    wts.push(1.0);
    for i in 1..=narcs {
        let ta = a0 + dth * (i - 1) as f64;
        let tb = a0 + dth * i as f64;
        let mid = 0.5 * (ta + tb);
        let rr = radius / w1; // tangent-line intersection radius
        cps.push(plane.point_at(rr * mid.cos(), rr * mid.sin()));
        wts.push(w1);
        cps.push(on(tb, radius));
        wts.push(1.0);
    }
    let mut knots = vec![0.0, 0.0, 0.0];
    for i in 1..narcs {
        let v = i as f64 / narcs as f64;
        knots.push(v);
        knots.push(v);
    }
    knots.extend_from_slice(&[1.0, 1.0, 1.0]);
    NurbsCurve { degree: 2, control_points: cps, weights: wts, knots }
}

/// True if a linear map is conformal on the given plane (its x/y axes stay
/// orthogonal and equally scaled), returning the mapped orthonormal axes and
/// scale factor.
fn conformal_in_plane(m: &Mat4, plane: &Plane) -> Option<(Vec3, Vec3, f64)> {
    let u = m.transform_vector(plane.x_axis);
    let v = m.transform_vector(plane.y_axis);
    let (lu, lv) = (u.length(), v.length());
    if lu < EPS || lv < EPS {
        return None;
    }
    let scale_tol = 1e-9 * lu.max(lv);
    if (lu - lv).abs() > scale_tol || u.dot(v).abs() > 1e-9 * lu * lv {
        return None;
    }
    Some((u / lu, v / lv, 0.5 * (lu + lv)))
}

impl Curve {
    /// Point at normalized parameter t in [0,1] (clamped).
    pub fn point_at(&self, t: f64) -> Vec3 {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        match self {
            Curve::Line { a, b } => a.lerp(*b, t),
            Curve::Polyline { points, closed } => {
                polyline_point(&polyline_effective(points, *closed), t)
            }
            Curve::Circle { plane, radius } => {
                let th = std::f64::consts::TAU * t;
                plane.point_at(radius * th.cos(), radius * th.sin())
            }
            Curve::Arc { plane, radius, start_angle, end_angle } => {
                let th = start_angle + (end_angle - start_angle) * t;
                plane.point_at(radius * th.cos(), radius * th.sin())
            }
            Curve::Nurbs(nc) => nc.point_at(t),
        }
    }

    /// Unit tangent at t (direction of increasing t). Analytic for
    /// line/polyline/circle/arc, central finite differences (h = 1e-5) for
    /// NURBS. Returns Vec3::ZERO for degenerate curves.
    pub fn tangent_at(&self, t: f64) -> Vec3 {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        match self {
            Curve::Line { a, b } => (*b - *a).normalized(),
            Curve::Polyline { points, closed } => {
                polyline_tangent(&polyline_effective(points, *closed), t)
            }
            Curve::Circle { plane, radius } => {
                let th = std::f64::consts::TAU * t;
                ((plane.x_axis * (-th.sin()) + plane.y_axis * th.cos()) * *radius).normalized()
            }
            Curve::Arc { plane, radius, start_angle, end_angle } => {
                let sweep = end_angle - start_angle;
                let th = start_angle + sweep * t;
                ((plane.x_axis * (-th.sin()) + plane.y_axis * th.cos()) * (*radius * sweep))
                    .normalized()
            }
            Curve::Nurbs(_) => {
                let h = 1e-5;
                let a = self.point_at((t - h).max(0.0));
                let b = self.point_at((t + h).min(1.0));
                (b - a).normalized()
            }
        }
    }

    /// Approximate arc length. Exact for line/polyline/circle/arc; NURBS use
    /// a 256-segment chord sum (relative error O(1/256²) ≈ 1e-5 for smooth
    /// curves; underestimates for very wiggly curves).
    pub fn length(&self) -> f64 {
        match self {
            Curve::Line { a, b } => a.distance(*b),
            Curve::Polyline { points, closed } => {
                cumulative_lengths(&polyline_effective(points, *closed)).1
            }
            Curve::Circle { radius, .. } => std::f64::consts::TAU * radius.abs(),
            Curve::Arc { radius, start_angle, end_angle, .. } => {
                (end_angle - start_angle).abs() * radius.abs()
            }
            Curve::Nurbs(_) => {
                let n = 256;
                let mut acc = 0.0;
                let mut prev = self.point_at(0.0);
                for i in 1..=n {
                    let p = self.point_at(i as f64 / n as f64);
                    acc += prev.distance(p);
                    prev = p;
                }
                acc
            }
        }
    }

    /// Whether the curve forms a closed loop. Circle: always. Polyline: the
    /// `closed` flag, or first == last (3+ points). Arc: |sweep| >= 2π - eps.
    /// Nurbs: endpoints coincide (relative tolerance). Line: never.
    pub fn is_closed(&self) -> bool {
        match self {
            Curve::Line { .. } => false,
            Curve::Polyline { points, closed } => {
                *closed
                    || (points.len() > 2 && {
                        let scale = BBox::from_points(points).diagonal().max(1.0);
                        points[0].distance(points[points.len() - 1]) < 1e-9 * scale
                    })
            }
            Curve::Circle { .. } => true,
            Curve::Arc { start_angle, end_angle, .. } => {
                (end_angle - start_angle).abs() >= std::f64::consts::TAU - 1e-9
            }
            Curve::Nurbs(nc) => {
                let a = nc.point_at(0.0);
                let b = nc.point_at(1.0);
                let scale = BBox::from_points(&nc.control_points).diagonal().max(1.0);
                a.distance(b) < 1e-9 * scale
            }
        }
    }

    /// Polyline approximation with `segments` >= 1 segments.
    ///
    /// Seam rule: **open** curves return `segments + 1` points (t = 0..=1);
    /// **closed** curves return exactly `segments` points at t = i/segments,
    /// i in 0..segments, WITHOUT a duplicated seam point (closure is implicit
    /// by wrapping back to the first point). Points lie exactly on the
    /// analytic curve for circles/arcs (and on the segments of polylines).
    pub fn tessellate(&self, segments: usize) -> Vec<Vec3> {
        let segs = segments.max(1);
        if self.is_closed() {
            (0..segs).map(|i| self.point_at(i as f64 / segs as f64)).collect()
        } else {
            (0..=segs).map(|i| self.point_at(i as f64 / segs as f64)).collect()
        }
    }

    /// `n` points spread evenly by parameter (Grasshopper Divide Curve:
    /// n segments -> n+1 points for open, n points for closed).
    /// n == 0 returns an empty Vec.
    pub fn divide(&self, n: usize) -> Vec<Vec3> {
        if n == 0 {
            return Vec::new();
        }
        self.tessellate(n)
    }

    /// Transform the curve. Defining points are transformed directly.
    /// Circles/arcs stay circles/arcs when the map is conformal in their
    /// plane (rotation/translation/mirror/uniform scale); otherwise they are
    /// converted to an exact rational-quadratic NURBS (the transform of a
    /// circle under any affine map is an ellipse, represented exactly).
    /// Perspective components of `m` are not supported (affine assumed).
    pub fn transformed(&self, m: &Mat4) -> Curve {
        match self {
            Curve::Line { a, b } => {
                Curve::Line { a: m.transform_point(*a), b: m.transform_point(*b) }
            }
            Curve::Polyline { points, closed } => Curve::Polyline {
                points: points.iter().map(|p| m.transform_point(*p)).collect(),
                closed: *closed,
            },
            Curve::Circle { plane, radius } => match conformal_in_plane(m, plane) {
                Some((x_axis, y_axis, s)) => Curve::Circle {
                    plane: Plane { origin: m.transform_point(plane.origin), x_axis, y_axis },
                    radius: radius * s,
                },
                None => {
                    let nc = arc_to_nurbs(plane, *radius, 0.0, std::f64::consts::TAU);
                    Curve::Nurbs(transform_nurbs(&nc, m))
                }
            },
            Curve::Arc { plane, radius, start_angle, end_angle } => {
                match conformal_in_plane(m, plane) {
                    Some((x_axis, y_axis, s)) => Curve::Arc {
                        plane: Plane { origin: m.transform_point(plane.origin), x_axis, y_axis },
                        radius: radius * s,
                        start_angle: *start_angle,
                        end_angle: *end_angle,
                    },
                    None => {
                        let nc = arc_to_nurbs(plane, *radius, *start_angle, *end_angle);
                        Curve::Nurbs(transform_nurbs(&nc, m))
                    }
                }
            }
            Curve::Nurbs(nc) => Curve::Nurbs(transform_nurbs(nc, m)),
        }
    }

    /// Axis-aligned bounding box. Exact for line/polyline/circle; arcs and
    /// NURBS are sampled (128 segments, endpoints included), so the box can
    /// be very slightly small for extremely curvy NURBS.
    pub fn bbox(&self) -> BBox {
        match self {
            Curve::Line { a, b } => BBox::from_points([a, b]),
            Curve::Polyline { points, .. } => BBox::from_points(points),
            Curve::Circle { plane, radius } => {
                let r = radius.abs();
                let e = Vec3::new(
                    r * (plane.x_axis.x.powi(2) + plane.y_axis.x.powi(2)).sqrt(),
                    r * (plane.x_axis.y.powi(2) + plane.y_axis.y.powi(2)).sqrt(),
                    r * (plane.x_axis.z.powi(2) + plane.y_axis.z.powi(2)).sqrt(),
                );
                let mut b = BBox::EMPTY;
                b.include(plane.origin - e);
                b.include(plane.origin + e);
                b
            }
            Curve::Arc { .. } | Curve::Nurbs(_) => {
                let mut b = BBox::EMPTY;
                for i in 0..=128 {
                    b.include(self.point_at(i as f64 / 128.0));
                }
                b
            }
        }
    }
}

/// Transform a NURBS curve's control points (weights/knots unchanged) —
/// correct for affine transforms of rational B-splines.
fn transform_nurbs(nc: &NurbsCurve, m: &Mat4) -> NurbsCurve {
    NurbsCurve {
        degree: nc.degree,
        control_points: nc.control_points.iter().map(|p| m.transform_point(*p)).collect(),
        weights: nc.weights.clone(),
        knots: nc.knots.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI, TAU};

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    #[test]
    fn nurbs_from_points_open_endpoints() {
        let pts = [v(0.0, 0.0, 0.0), v(1.0, 2.0, 0.0), v(3.0, -1.0, 1.0), v(4.0, 0.0, 0.0)];
        let nc = NurbsCurve::from_points(&pts, 3, false).unwrap();
        assert_eq!(nc.degree, 3);
        assert_eq!(nc.knots.len(), nc.control_points.len() + nc.degree + 1);
        assert!(nc.point_at(0.0).distance(pts[0]) < 1e-9);
        assert!(nc.point_at(1.0).distance(pts[3]) < 1e-9);
    }

    #[test]
    fn nurbs_degree_clamped() {
        let pts = [v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(2.0, 1.0, 0.0)];
        let nc = NurbsCurve::from_points(&pts, 7, false).unwrap();
        assert_eq!(nc.degree, 2);
        let nc = NurbsCurve::from_points(&pts, 0, false).unwrap();
        assert_eq!(nc.degree, 1);
        assert!(NurbsCurve::from_points(&pts[..1], 3, false).is_none());
        assert!(NurbsCurve::from_points(&[], 3, false).is_none());
    }

    #[test]
    fn nurbs_closed_seam_coincides() {
        let pts = [
            v(1.0, 0.0, 0.0),
            v(0.0, 1.0, 0.0),
            v(-1.0, 0.0, 0.0),
            v(0.0, -1.0, 0.0),
            v(0.5, -0.5, 0.5),
        ];
        let nc = NurbsCurve::from_points(&pts, 3, true).unwrap();
        assert_eq!(nc.knots.len(), nc.control_points.len() + nc.degree + 1);
        let a = nc.point_at(0.0);
        let b = nc.point_at(1.0);
        assert!(a.distance(b) < 1e-9, "seam gap {}", a.distance(b));
        let c = Curve::Nurbs(nc);
        assert!(c.is_closed());
        // Smooth across the seam: tangents match too (C^{degree-1}).
        let t0 = c.tangent_at(0.0);
        let t1 = c.tangent_at(1.0);
        assert!(t0.distance(t1) < 1e-3, "tangent seam {:?} {:?}", t0, t1);
    }

    #[test]
    fn nurbs_degree_one_is_polyline() {
        let pts = [v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(1.0, 1.0, 0.0)];
        let nc = NurbsCurve::from_points(&pts, 1, false).unwrap();
        // Degree 1: curve passes through all control points.
        assert!(nc.point_at(0.5).distance(v(1.0, 0.0, 0.0)) < 1e-9);
        assert!(nc.point_at(0.25).distance(v(0.5, 0.0, 0.0)) < 1e-9);
    }

    #[test]
    fn nurbs_malformed_knots_no_panic() {
        let nc = NurbsCurve {
            degree: 3,
            control_points: vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0)],
            weights: vec![],
            knots: vec![0.0],
        };
        let p = nc.point_at(0.5);
        assert!(p.is_finite());
        assert!(nc.point_at(0.0).distance(v(0.0, 0.0, 0.0)) < 1e-9);
        assert!(nc.point_at(1.0).distance(v(1.0, 0.0, 0.0)) < 1e-9);
    }

    #[test]
    fn rational_circle_exact() {
        let plane = Plane::world_xy();
        let nc = arc_to_nurbs(&plane, 2.0, 0.0, TAU);
        for i in 0..=100 {
            let p = nc.point_at(i as f64 / 100.0);
            let r = (p.x * p.x + p.y * p.y).sqrt();
            assert!((r - 2.0).abs() < 1e-9, "radius {} at {}", r, i);
        }
        assert!(nc.point_at(0.0).distance(v(2.0, 0.0, 0.0)) < 1e-9);
    }

    #[test]
    fn line_eval() {
        let c = Curve::Line { a: v(1.0, 0.0, 0.0), b: v(3.0, 0.0, 0.0) };
        assert!(c.point_at(0.5).distance(v(2.0, 0.0, 0.0)) < 1e-12);
        assert!(c.tangent_at(0.3).distance(Vec3::X) < 1e-12);
        assert!((c.length() - 2.0).abs() < 1e-12);
        assert!(!c.is_closed());
        // Clamping.
        assert!(c.point_at(2.0).distance(v(3.0, 0.0, 0.0)) < 1e-12);
        assert!(c.point_at(-1.0).distance(v(1.0, 0.0, 0.0)) < 1e-12);
    }

    #[test]
    fn polyline_chord_length_param() {
        // Segments of length 3 and 1: midpoint of param is at distance 2.
        let c = Curve::Polyline {
            points: vec![v(0.0, 0.0, 0.0), v(3.0, 0.0, 0.0), v(3.0, 1.0, 0.0)],
            closed: false,
        };
        assert!((c.length() - 4.0).abs() < 1e-12);
        assert!(c.point_at(0.5).distance(v(2.0, 0.0, 0.0)) < 1e-12);
        assert!(c.point_at(1.0).distance(v(3.0, 1.0, 0.0)) < 1e-12);
        assert!(c.tangent_at(0.9).distance(Vec3::Y) < 1e-12);
    }

    #[test]
    fn polyline_closed_semantics() {
        let sq = vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(1.0, 1.0, 0.0), v(0.0, 1.0, 0.0)];
        let c = Curve::Polyline { points: sq.clone(), closed: true };
        assert!(c.is_closed());
        assert!((c.length() - 4.0).abs() < 1e-12);
        assert!(c.point_at(1.0).distance(v(0.0, 0.0, 0.0)) < 1e-12);
        // Unflagged but first == last also reads as closed.
        let mut loopy = sq.clone();
        loopy.push(sq[0]);
        let c2 = Curve::Polyline { points: loopy, closed: false };
        assert!(c2.is_closed());
        assert!((c2.length() - 4.0).abs() < 1e-12);
        // Open square is open.
        let c3 = Curve::Polyline { points: sq, closed: false };
        assert!(!c3.is_closed());
        assert!((c3.length() - 3.0).abs() < 1e-12);
    }

    #[test]
    fn polyline_degenerate_no_panic() {
        let c = Curve::Polyline { points: vec![], closed: true };
        assert_eq!(c.point_at(0.5), Vec3::ZERO);
        assert_eq!(c.length(), 0.0);
        let c = Curve::Polyline { points: vec![v(1.0, 1.0, 1.0); 4], closed: true };
        assert!(c.point_at(0.7).distance(v(1.0, 1.0, 1.0)) < 1e-12);
        assert_eq!(c.tangent_at(0.5), Vec3::ZERO);
    }

    #[test]
    fn circle_eval_exact() {
        let c = Curve::Circle { plane: Plane::world_xy(), radius: 2.0 };
        assert!(c.is_closed());
        assert!((c.length() - TAU * 2.0).abs() < 1e-12);
        assert!(c.point_at(0.25).distance(v(0.0, 2.0, 0.0)) < 1e-12);
        assert!(c.tangent_at(0.0).distance(Vec3::Y) < 1e-12);
    }

    #[test]
    fn arc_eval_and_closure() {
        let c = Curve::Arc {
            plane: Plane::world_xy(),
            radius: 1.0,
            start_angle: 0.0,
            end_angle: PI,
        };
        assert!(!c.is_closed());
        assert!((c.length() - PI).abs() < 1e-12);
        assert!(c.point_at(1.0).distance(v(-1.0, 0.0, 0.0)) < 1e-12);
        assert!(c.point_at(0.5).distance(v(0.0, 1.0, 0.0)) < 1e-12);
        // Reversed sweep tangent points the other way.
        let r = Curve::Arc {
            plane: Plane::world_xy(),
            radius: 1.0,
            start_angle: PI,
            end_angle: 0.0,
        };
        assert!(r.tangent_at(0.5).distance(-c.tangent_at(0.5)) < 1e-9);
        let full = Curve::Arc {
            plane: Plane::world_xy(),
            radius: 1.0,
            start_angle: FRAC_PI_2,
            end_angle: FRAC_PI_2 + TAU,
        };
        assert!(full.is_closed());
    }

    #[test]
    fn tessellate_seam_rules() {
        let circle = Curve::Circle { plane: Plane::world_xy(), radius: 1.0 };
        let pts = circle.tessellate(8);
        assert_eq!(pts.len(), 8); // closed: no seam duplicate
        for p in &pts {
            assert!((p.length() - 1.0).abs() < 1e-12); // exactly on circle
        }
        assert!(pts[0].distance(pts[7]) > 1e-6);

        let line = Curve::Line { a: Vec3::ZERO, b: Vec3::X };
        assert_eq!(line.tessellate(8).len(), 9); // open: segments+1
        assert_eq!(line.tessellate(0).len(), 2); // clamped to >= 1 segment

        let arc = Curve::Arc {
            plane: Plane::world_xy(),
            radius: 1.0,
            start_angle: 0.0,
            end_angle: PI,
        };
        let apts = arc.tessellate(4);
        assert_eq!(apts.len(), 5);
        for p in &apts {
            assert!((p.length() - 1.0).abs() < 1e-12); // exactly on arc
        }
    }

    #[test]
    fn divide_counts() {
        let circle = Curve::Circle { plane: Plane::world_xy(), radius: 1.0 };
        assert_eq!(circle.divide(6).len(), 6);
        let line = Curve::Line { a: Vec3::ZERO, b: Vec3::X };
        assert_eq!(line.divide(6).len(), 7);
        assert!(line.divide(0).is_empty());
        // Evenly spaced by parameter.
        let d = line.divide(4);
        assert!(d[1].distance(v(0.25, 0.0, 0.0)) < 1e-12);
    }

    #[test]
    fn transform_circle_rigid_stays_circle() {
        let c = Curve::Circle { plane: Plane::world_xy(), radius: 1.5 };
        let m = Mat4::translation(v(1.0, 2.0, 3.0))
            * Mat4::rotation_axis(Vec3::ZERO, v(1.0, 1.0, 0.0), 0.7);
        match c.transformed(&m) {
            Curve::Circle { plane, radius } => {
                assert!((radius - 1.5).abs() < 1e-9);
                assert!(plane.origin.distance(v(1.0, 2.0, 3.0)) < 1e-9);
            }
            other => panic!("expected Circle, got {:?}", other),
        }
        // Uniform scale keeps circle, scales radius.
        let s = Mat4::scaling_uniform(Vec3::ZERO, 2.0);
        match c.transformed(&s) {
            Curve::Circle { radius, .. } => assert!((radius - 3.0).abs() < 1e-9),
            other => panic!("expected Circle, got {:?}", other),
        }
    }

    #[test]
    fn transform_circle_nonuniform_becomes_exact_ellipse() {
        let c = Curve::Circle { plane: Plane::world_xy(), radius: 1.0 };
        let m = Mat4::scaling(v(3.0, 1.0, 1.0));
        let e = c.transformed(&m);
        match &e {
            Curve::Nurbs(_) => {}
            other => panic!("expected Nurbs, got {:?}", other),
        }
        // Every point satisfies the ellipse equation (x/3)^2 + y^2 = 1 exactly.
        for i in 0..=64 {
            let p = e.point_at(i as f64 / 64.0);
            let val = (p.x / 3.0).powi(2) + p.y * p.y;
            assert!((val - 1.0).abs() < 1e-9, "ellipse deviation {} at {}", val, i);
        }
        assert!(e.is_closed());
    }

    #[test]
    fn transform_polyline_and_line() {
        let m = Mat4::translation(v(0.0, 0.0, 5.0));
        let c = Curve::Polyline {
            points: vec![v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0)],
            closed: false,
        };
        match c.transformed(&m) {
            Curve::Polyline { points, closed } => {
                assert!(!closed);
                assert!(points[1].distance(v(1.0, 0.0, 5.0)) < 1e-12);
            }
            other => panic!("expected Polyline, got {:?}", other),
        }
    }

    #[test]
    fn bbox_circle_exact_tilted() {
        let plane = Plane::from_normal(v(0.0, 0.0, 1.0), v(0.0, 1.0, 1.0));
        let c = Curve::Circle { plane, radius: 2.0 };
        let bb = c.bbox();
        // Compare against dense sampling.
        let mut sampled = BBox::EMPTY;
        for i in 0..4096 {
            sampled.include(c.point_at(i as f64 / 4096.0));
        }
        assert!(bb.min.distance(sampled.min) < 1e-4);
        assert!(bb.max.distance(sampled.max) < 1e-4);
    }

    #[test]
    fn nurbs_length_accuracy() {
        // Half circle as NURBS-ish: use arc_to_nurbs and compare lengths.
        let nc = arc_to_nurbs(&Plane::world_xy(), 1.0, 0.0, PI);
        let c = Curve::Nurbs(nc);
        assert!((c.length() - PI).abs() / PI < 1e-3);
    }

    #[test]
    fn serde_roundtrip() {
        let c = Curve::Nurbs(
            NurbsCurve::from_points(
                &[v(0.0, 0.0, 0.0), v(1.0, 1.0, 0.0), v(2.0, 0.0, 0.0)],
                2,
                false,
            )
            .unwrap(),
        );
        let s = serde_json::to_string(&c).unwrap();
        let back: Curve = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
