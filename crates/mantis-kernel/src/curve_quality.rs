//! Local curve diagnostics and cubic tangent blends.
//!
//! Curvature is the geometric vector dT/ds (inverse model units). These are
//! local differential checks, not a surface/Class-A certification. Uniform
//! parameter samples can miss detail between samples. Undefined curvature at
//! corners or stationary points is reported explicitly, never as a NaN.

use crate::{Curve, NurbsCurve, Vec3};

pub const MAX_COMB_SAMPLES: usize = 2048;
const MAX_CONTROL_POINTS: usize = 16_384;
const MAX_DEGREE: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvatureSample {
    pub parameter: f64,
    pub point: Vec3,
    pub curvature: Vec3,
    /// False at stationary points and nonsmooth interior joins. In that case
    /// curvature is the finite placeholder ZERO, not a measurement.
    pub valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContinuityTolerance {
    pub distance: f64,
    /// Radians, in [0, pi]. Tangents follow increasing curve parameters.
    pub angle: f64,
    /// Difference of curvature vectors, in inverse model units.
    pub curvature: f64,
}

impl Default for ContinuityTolerance {
    fn default() -> Self {
        Self {
            distance: 0.001,
            angle: 0.01,
            curvature: 0.001,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContinuityReport {
    pub gap: f64,
    pub angle: f64,
    pub curvature_delta: f64,
    pub g0: bool,
    pub g1: bool,
    pub g2: bool,
}

fn norm(v: Vec3) -> f64 {
    v.x.hypot(v.y).hypot(v.z)
}

#[derive(Clone, Copy)]
struct Homogeneous {
    p: Vec3,
    w: f64,
}

impl Homogeneous {
    fn mix(self, rhs: Self, t: f64) -> Self {
        Self {
            p: self.p * (1.0 - t) + rhs.p * t,
            w: self.w * (1.0 - t) + rhs.w * t,
        }
    }
}

struct Rational {
    origin: Vec3,
    nets: Vec<Vec<Homogeneous>>,
}

impl Rational {
    fn evaluate(&self, n: &NurbsCurve, u: f64, from_left: bool) -> (Vec3, Vec3, Vec3) {
        let mut evaluated = [Homogeneous {
            p: Vec3::ZERO,
            w: 0.0,
        }; 3];
        for (order, net) in self.nets.iter().enumerate() {
            evaluated[order] = de_boor(
                net,
                &n.knots[order..n.knots.len() - order],
                n.degree - order,
                u,
                from_left,
            );
        }
        let [h, first, second] = evaluated;
        let local = h.p / h.w;
        let velocity = (first.p - local * first.w) / h.w;
        let acceleration = (second.p - velocity * (2.0 * first.w) - local * second.w) / h.w;
        (local, velocity, acceleration)
    }
}

fn geometric_derivatives(first: Vec3, second: Vec3) -> Option<(Vec3, Vec3)> {
    let speed = norm(first);
    if speed == 0.0 || !speed.is_finite() {
        return None;
    }
    let tangent = first / speed;
    let curvature = ((second - tangent * tangent.dot(second)) / speed) / speed;
    (curvature.is_finite() && norm(curvature).is_finite()).then_some((tangent, curvature))
}

struct Prepared<'a> {
    curve: &'a Curve,
    rational: Option<Rational>,
    polyline: Vec<(Vec3, f64)>,
}

impl<'a> Prepared<'a> {
    fn is_closed(&self) -> bool {
        match self.curve {
            Curve::Line { .. } => false,
            Curve::Circle { .. } => true,
            Curve::Arc {
                start_angle,
                end_angle,
                ..
            } => {
                let sweep = end_angle - start_angle;
                let turns = (sweep / std::f64::consts::TAU).round();
                turns != 0.0 && (sweep - turns * std::f64::consts::TAU).abs() < 1e-10
            }
            Curve::Polyline { closed, .. } => {
                *closed
                    || (self.polyline.len() > 2 && {
                        let first = self.polyline[0].0;
                        let (last, length) = self.polyline[self.polyline.len() - 1];
                        norm(first - last) <= 1e-10 * length
                    })
            }
            Curve::Nurbs(n) => {
                let scale = n
                    .control_points
                    .iter()
                    .map(|p| norm(*p - n.control_points[0]))
                    .fold(0.0, f64::max);
                norm(self.derivatives(0.0).0 - self.derivatives(1.0).0) <= 1e-10 * scale
            }
        }
    }

    fn new(curve: &'a Curve) -> Result<Self, String> {
        let mut prepared = Self {
            curve,
            rational: None,
            polyline: Vec::new(),
        };
        let plane_valid =
            |p: &crate::Plane| p.origin.is_finite() && p.x_axis.is_finite() && p.y_axis.is_finite();
        let valid = match curve {
            Curve::Line { a, b } => a.is_finite() && b.is_finite(),
            Curve::Circle { plane, radius } => plane_valid(plane) && radius.is_finite(),
            Curve::Arc {
                plane,
                radius,
                start_angle,
                end_angle,
            } => {
                plane_valid(plane)
                    && radius.is_finite()
                    && start_angle.is_finite()
                    && end_angle.is_finite()
                    && (end_angle - start_angle).is_finite()
            }
            Curve::Polyline { points, closed } => {
                if points.len() > MAX_CONTROL_POINTS {
                    return Err("curve quality: too many polyline vertices (maximum 16384)".into());
                }
                let mut length = 0.0;
                for &point in points {
                    if let Some(&(last, _)) = prepared.polyline.last() {
                        let segment = norm(point - last);
                        if segment == 0.0 {
                            continue;
                        }
                        length += segment;
                    }
                    prepared.polyline.push((point, length));
                }
                if *closed && prepared.polyline.len() > 1 {
                    let first = prepared.polyline[0].0;
                    let last = prepared.polyline.last().unwrap().0;
                    let segment = norm(first - last);
                    if segment > 0.0 {
                        prepared.polyline.push((first, length + segment));
                    }
                }
                points.iter().all(|p| p.is_finite())
                    && prepared.polyline.iter().all(|(_, d)| d.is_finite())
            }
            Curve::Nurbs(n) => {
                let count = n.control_points.len();
                if count > MAX_CONTROL_POINTS || n.degree > MAX_DEGREE {
                    return Err(
                        "curve quality: NURBS limit is 16384 control points and degree 25".into(),
                    );
                }
                if count < 2
                    || n.degree == 0
                    || n.degree >= count
                    || n.knots.len() != count + n.degree + 1
                    || n.weights.len() != count
                    || !n.control_points.iter().all(|p| p.is_finite())
                    || !n.weights.iter().all(|w| w.is_finite() && *w > 0.0)
                    || !n.knots.iter().all(|k| k.is_finite())
                    || !n.knots.windows(2).all(|pair| pair[0] <= pair[1])
                {
                    return Err(
                        "curve quality: malformed NURBS control points, weights or knots".into(),
                    );
                }
                let extent = n.knots[count] - n.knots[n.degree];
                if !extent.is_finite() || extent <= 0.0 {
                    return Err("curve quality: empty NURBS knot domain".into());
                }
                let origin = n.control_points[0];
                let weight_scale = n.weights.iter().copied().fold(0.0, f64::max);
                let net: Vec<_> = n
                    .control_points
                    .iter()
                    .zip(&n.weights)
                    .map(|(&p, &w)| {
                        let w = w / weight_scale;
                        Homogeneous {
                            p: (p - origin) * w,
                            w,
                        }
                    })
                    .collect();
                let mut nets = vec![net];
                for order in 1..=n.degree.min(2) {
                    let previous = nets.last().unwrap();
                    let degree = n.degree + 1 - order;
                    let derivative = previous
                        .windows(2)
                        .enumerate()
                        .map(|(i, pair)| {
                            let denominator = n.knots[i + n.degree + 1] - n.knots[i + order];
                            // A repeated zero knot span has no contribution on its
                            // adjacent open spans (standard derivative convention).
                            let factor = if denominator == 0.0 {
                                0.0
                            } else {
                                degree as f64 / denominator
                            };
                            Homogeneous {
                                p: (pair[1].p - pair[0].p) * factor,
                                w: (pair[1].w - pair[0].w) * factor,
                            }
                        })
                        .collect();
                    nets.push(derivative);
                }
                let valid = nets
                    .iter()
                    .flatten()
                    .all(|h| h.p.is_finite() && h.w.is_finite());
                prepared.rational = Some(Rational { origin, nets });
                valid
            }
        };
        if !valid {
            return Err("curve quality: non-finite geometry or arithmetic overflow".into());
        }
        Ok(prepared)
    }

    /// Returns point, first/second derivatives, and local smoothness. NURBS
    /// derivatives use the actual knot coordinate; geometric curvature is
    /// invariant under its affine conversion to normalized t.
    fn derivatives(&self, t: f64) -> (Vec3, Vec3, Vec3, bool) {
        match self.curve {
            Curve::Line { a, b } => (a.lerp(*b, t), *b - *a, Vec3::ZERO, true),
            Curve::Polyline { .. } => {
                let points = &self.polyline;
                if points.len() < 2 {
                    return (
                        points.first().map_or(Vec3::ZERO, |v| v.0),
                        Vec3::ZERO,
                        Vec3::ZERO,
                        false,
                    );
                }
                let target = points.last().unwrap().1 * t;
                let i = points
                    .partition_point(|(_, d)| *d <= target)
                    .saturating_sub(1)
                    .min(points.len() - 2);
                let (a, d0) = points[i];
                let (b, d1) = points[i + 1];
                let direction = (b - a) / (d1 - d0);
                let mut smooth = true;
                if i > 0
                    && t > 0.0
                    && t < 1.0
                    && (target - d0).abs() <= 1e-12 * points.last().unwrap().1
                {
                    let incoming = (a - points[i - 1].0) / (d0 - points[i - 1].1);
                    smooth = norm(incoming - direction) < 1e-10;
                }
                (
                    a.lerp(b, (target - d0) / (d1 - d0)),
                    direction,
                    Vec3::ZERO,
                    smooth,
                )
            }
            Curve::Circle { plane, radius } => {
                let angle = std::f64::consts::TAU * t;
                let radial = (plane.x_axis * angle.cos() + plane.y_axis * angle.sin()) * *radius;
                let first = (plane.x_axis * -angle.sin() + plane.y_axis * angle.cos()) * *radius;
                (plane.origin + radial, first, -radial, true)
            }
            Curve::Arc {
                plane,
                radius,
                start_angle,
                end_angle,
            } => {
                let sweep = end_angle - start_angle;
                let angle = start_angle + sweep * t;
                let radial = (plane.x_axis * angle.cos() + plane.y_axis * angle.sin()) * *radius;
                // Use angle coordinate, retaining orientation but avoiding
                // needless overflow from a large angular sweep.
                let first = (plane.x_axis * -angle.sin() + plane.y_axis * angle.cos())
                    * (*radius * sweep.signum());
                (plane.origin + radial, first, -radial, sweep != 0.0)
            }
            Curve::Nurbs(n) => {
                let rational = self.rational.as_ref().unwrap();
                let count = n.control_points.len();
                let u = n.knots[n.degree] + (n.knots[count] - n.knots[n.degree]) * t;
                let (local, velocity, acceleration) = rational.evaluate(n, u, false);
                let multiplicity = if t > 0.0 && t < 1.0 {
                    n.knots.partition_point(|k| *k <= u) - n.knots.partition_point(|k| *k < u)
                } else {
                    0
                };
                let mut smooth = multiplicity == 0 || n.degree >= multiplicity + 2;
                if !smooth {
                    // Low parametric continuity does not imply low geometric
                    // continuity: multi-span rational circles are a common
                    // example. Compare exact one-sided differentials at knots.
                    let (left, first, second) = rational.evaluate(n, u, true);
                    if let (Some((ta, ca)), Some((tb, cb))) = (
                        geometric_derivatives(first, second),
                        geometric_derivatives(velocity, acceleration),
                    ) {
                        smooth = norm(local - left)
                            <= 1e-10 * norm(local).max(norm(left)).max(1e-12)
                            && norm(ta - tb) <= 1e-9
                            && norm(ca - cb) <= 1e-9 * norm(ca).max(norm(cb)).max(1.0);
                    }
                }
                (rational.origin + local, velocity, acceleration, smooth)
            }
        }
    }

    fn sample(&self, t: f64) -> Result<(CurvatureSample, Vec3), String> {
        let (point, first, second, smooth) = self.derivatives(t);
        if !point.is_finite() || !first.is_finite() || !second.is_finite() {
            return Err("curve quality: derivative evaluation overflow".into());
        }
        let speed = norm(first);
        let mut sample = CurvatureSample {
            parameter: t,
            point,
            curvature: Vec3::ZERO,
            valid: false,
        };
        if speed == 0.0 || !smooth {
            return Ok((sample, Vec3::ZERO));
        }
        if !speed.is_finite() {
            return Err("curve quality: derivative magnitude overflow".into());
        }
        let tangent = first / speed;
        // Divide twice instead of squaring speed (which can overflow).
        let curvature = ((second - tangent * tangent.dot(second)) / speed) / speed;
        if !curvature.is_finite() || !norm(curvature).is_finite() {
            return Err("curve quality: curvature overflow".into());
        }
        sample.curvature = curvature;
        sample.valid = true;
        Ok((sample, tangent))
    }
}

fn de_boor(
    net: &[Homogeneous],
    knots: &[f64],
    degree: usize,
    u: f64,
    from_left: bool,
) -> Homogeneous {
    let count = net.len();
    let k = (degree
        + knots[degree..count].partition_point(|&k| if from_left { k < u } else { k <= u }))
    .saturating_sub(1)
    .clamp(degree, count - 1);
    // Degree is validated before evaluation, keeping each evaluation bounded.
    let mut work = [Homogeneous {
        p: Vec3::ZERO,
        w: 0.0,
    }; MAX_DEGREE + 1];
    work[..=degree].copy_from_slice(&net[k - degree..=k]);
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = j + k - degree;
            let denominator = knots[i + degree - r + 1] - knots[i];
            let alpha = if denominator == 0.0 {
                0.0
            } else {
                (u - knots[i]) / denominator
            };
            work[j] = work[j - 1].mix(work[j], alpha);
        }
    }
    work[degree]
}

impl Curve {
    /// Uniform normalized-parameter samples, including both ends for open
    /// curves and excluding the duplicated seam for closed curves.
    pub fn curvature_comb(&self, samples: usize) -> Result<Vec<CurvatureSample>, String> {
        if !(2..=MAX_COMB_SAMPLES).contains(&samples) {
            return Err("curve_comb: samples must be an integer in 2..2048".into());
        }
        let prepared = Prepared::new(self)?;
        let closed = prepared.is_closed();
        let denominator = if closed { samples } else { samples - 1 };
        let mut result: Vec<CurvatureSample> = (0..samples)
            .map(|i| {
                prepared
                    .sample(i as f64 / denominator as f64)
                    .map(|(sample, _)| sample)
            })
            .collect::<Result<_, _>>()?;
        // Both sides of a closed seam matter for combs; endpoint continuity
        // and blending intentionally use only the endpoint's interior side.
        if closed {
            let (start, ta) = prepared.sample(0.0)?;
            let (end, tb) = prepared.sample(1.0)?;
            if !start.valid
                || !end.valid
                || norm(ta - tb) > 1e-9
                || norm(start.curvature - end.curvature)
                    > 1e-9 * norm(start.curvature).max(norm(end.curvature)).max(1.0)
            {
                result[0].valid = false;
                result[0].curvature = Vec3::ZERO;
            }
        }
        Ok(result)
    }

    /// Inspect this curve's end against the next curve's start. G1 requires
    /// matching directed tangents; G2 additionally compares curvature vectors.
    /// All levels include the preceding levels. Undefined endpoint derivatives
    /// are errors so a collapsed curve can never be certified as G1/G2.
    pub fn continuity_to(
        &self,
        next: &Curve,
        tolerance: ContinuityTolerance,
    ) -> Result<ContinuityReport, String> {
        if ![tolerance.distance, tolerance.angle, tolerance.curvature]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
            || tolerance.angle > std::f64::consts::PI
        {
            return Err(
                "curve_continuity: tolerances must be finite and nonnegative; angle must be <= pi"
                    .into(),
            );
        }
        let (a, ta) = Prepared::new(self)?.sample(1.0)?;
        let (b, tb) = Prepared::new(next)?.sample(0.0)?;
        if !a.valid || !b.valid {
            return Err("curve_continuity: endpoint tangent or curvature is undefined".into());
        }
        let gap = norm(b.point - a.point);
        let angle = norm(ta.cross(tb)).atan2(ta.dot(tb).clamp(-1.0, 1.0));
        let curvature_delta = norm(b.curvature - a.curvature);
        if !gap.is_finite() || !curvature_delta.is_finite() {
            return Err("curve_continuity: measurement overflow".into());
        }
        let g0 = gap <= tolerance.distance;
        let g1 = g0 && angle <= tolerance.angle;
        let g2 = g1 && curvature_delta <= tolerance.curvature;
        Ok(ContinuityReport {
            gap,
            angle,
            curvature_delta,
            g0,
            g1,
            g2,
        })
    }

    /// Cubic Bezier/NURBS from this end to the next start, matching directed
    /// endpoint tangents. Each handle length is endpoint gap * tension / 3.
    /// Positive tension controls shape; this guarantees G1, not G2 or fairness.
    pub fn tangent_blend_to(&self, next: &Curve, tension: f64) -> Result<Curve, String> {
        if !tension.is_finite() || tension <= 0.0 || tension > 10.0 {
            return Err("blend_curve: tension must be in (0, 10]".into());
        }
        let (a, ta) = Prepared::new(self)?.sample(1.0)?;
        let (b, tb) = Prepared::new(next)?.sample(0.0)?;
        if !a.valid || !b.valid {
            return Err("blend_curve: endpoint tangent is undefined".into());
        }
        let gap = norm(b.point - a.point);
        if gap <= 1e-12 || !gap.is_finite() {
            return Err("blend_curve: endpoints must have a finite nonzero gap".into());
        }
        let handle = gap * (tension / 3.0);
        let points = [
            a.point,
            a.point + ta * handle,
            b.point - tb * handle,
            b.point,
        ];
        if !points.iter().all(|p| p.is_finite()) || points[0] == points[1] || points[2] == points[3]
        {
            return Err("blend_curve: handle overflow or below coordinate precision".into());
        }
        Ok(Curve::Nurbs(
            NurbsCurve::from_points(&points, 3, false).unwrap(),
        ))
    }
}
