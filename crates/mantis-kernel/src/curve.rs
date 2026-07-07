//! Curve types. All parametrized over normalized t in [0,1].
//! CONTRACT STUB — implement bodies, keep public signatures.

use crate::math::{BBox, Mat4, Plane, Vec3};
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
    pub knots: Vec<f64>,
}

impl NurbsCurve {
    /// Interpolating-ish smooth curve through `points` (uniform clamped knots,
    /// points used as control points — Grasshopper "Nurbs Curve" behavior).
    /// Degree is clamped to points.len()-1. Panics never; returns None if
    /// points.len() < 2.
    pub fn from_points(points: &[Vec3], degree: usize, _closed: bool) -> Option<NurbsCurve> {
        let _ = (points, degree);
        todo!("kernel-agent")
    }
    /// De Boor evaluation at normalized t in [0,1].
    pub fn point_at(&self, t: f64) -> Vec3 {
        let _ = t;
        todo!("kernel-agent")
    }
}

impl Curve {
    /// Point at normalized parameter t in [0,1] (clamped).
    pub fn point_at(&self, t: f64) -> Vec3 {
        let _ = t;
        todo!("kernel-agent")
    }
    /// Unit tangent at t (finite-difference fallback is fine).
    pub fn tangent_at(&self, t: f64) -> Vec3 {
        let _ = t;
        todo!("kernel-agent")
    }
    /// Approximate arc length (adaptive or fixed-sample; document accuracy).
    pub fn length(&self) -> f64 {
        todo!("kernel-agent")
    }
    pub fn is_closed(&self) -> bool {
        todo!("kernel-agent")
    }
    /// Polyline approximation with `segments` >= 1 segments (segments+1 points;
    /// closed curves may return segments points + implicit closure — document).
    pub fn tessellate(&self, segments: usize) -> Vec<Vec3> {
        let _ = segments;
        todo!("kernel-agent")
    }
    /// `n` points spread evenly by parameter (Grasshopper Divide Curve:
    /// n segments -> n+1 points for open, n points for closed).
    pub fn divide(&self, n: usize) -> Vec<Vec3> {
        let _ = n;
        todo!("kernel-agent")
    }
    pub fn transformed(&self, m: &Mat4) -> Curve {
        let _ = m;
        todo!("kernel-agent")
    }
    pub fn bbox(&self) -> BBox {
        todo!("kernel-agent")
    }
}
