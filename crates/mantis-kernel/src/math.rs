//! Vectors, matrices, planes, bounding boxes. Fully implemented — the
//! foundation every other crate builds on. f64 throughout.

use serde::{Deserialize, Serialize};
use std::ops::{Add, Div, Mul, Neg, Sub};

pub const EPS: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const X: Vec3 = Vec3 { x: 1.0, y: 0.0, z: 0.0 };
    pub const Y: Vec3 = Vec3 { x: 0.0, y: 1.0, z: 0.0 };
    pub const Z: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 1.0 };

    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Vec3 { x, y, z }
    }
    pub fn dot(self, o: Vec3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn length_sq(self) -> f64 {
        self.dot(self)
    }
    pub fn distance(self, o: Vec3) -> f64 {
        (self - o).length()
    }
    /// Unit vector; returns Vec3::ZERO for (near-)zero input.
    pub fn normalized(self) -> Vec3 {
        let l = self.length();
        if l < EPS {
            Vec3::ZERO
        } else {
            self / l
        }
    }
    pub fn lerp(self, o: Vec3, t: f64) -> Vec3 {
        self + (o - self) * t
    }
    /// Any unit vector perpendicular to self (deterministic choice).
    pub fn any_perpendicular(self) -> Vec3 {
        let n = self.normalized();
        let pick = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        n.cross(pick).normalized()
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f64> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Div<f64> for Vec3 {
    type Output = Vec3;
    fn div(self, s: f64) -> Vec3 {
        Vec3::new(self.x / s, self.y / s, self.z / s)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

/// Column-major 4x4 matrix: m[col][row], `transform_point(p) = M * [p,1]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mat4(pub [[f64; 4]; 4]);

impl Mat4 {
    pub fn identity() -> Mat4 {
        let mut m = [[0.0; 4]; 4];
        for (i, col) in m.iter_mut().enumerate() {
            col[i] = 1.0;
        }
        Mat4(m)
    }
    pub fn translation(t: Vec3) -> Mat4 {
        let mut m = Mat4::identity();
        m.0[3][0] = t.x;
        m.0[3][1] = t.y;
        m.0[3][2] = t.z;
        m
    }
    pub fn scaling_uniform(center: Vec3, f: f64) -> Mat4 {
        Mat4::translation(center) * Mat4::scaling(Vec3::new(f, f, f)) * Mat4::translation(-center)
    }
    pub fn scaling(s: Vec3) -> Mat4 {
        let mut m = Mat4::identity();
        m.0[0][0] = s.x;
        m.0[1][1] = s.y;
        m.0[2][2] = s.z;
        m
    }
    /// Rotation about an axis through `origin` by `angle` radians (right-handed).
    pub fn rotation_axis(origin: Vec3, axis: Vec3, angle: f64) -> Mat4 {
        let a = axis.normalized();
        if a == Vec3::ZERO {
            return Mat4::identity();
        }
        let (s, c) = angle.sin_cos();
        let ic = 1.0 - c;
        let (x, y, z) = (a.x, a.y, a.z);
        let mut m = Mat4::identity();
        // column-major: m.0[col][row]
        m.0[0][0] = c + x * x * ic;
        m.0[0][1] = y * x * ic + z * s;
        m.0[0][2] = z * x * ic - y * s;
        m.0[1][0] = x * y * ic - z * s;
        m.0[1][1] = c + y * y * ic;
        m.0[1][2] = z * y * ic + x * s;
        m.0[2][0] = x * z * ic + y * s;
        m.0[2][1] = y * z * ic - x * s;
        m.0[2][2] = c + z * z * ic;
        Mat4::translation(origin) * m * Mat4::translation(-origin)
    }
    /// Mirror across a plane.
    pub fn mirror(plane: &Plane) -> Mat4 {
        let n = plane.normal();
        let o = plane.origin;
        let mut m = Mat4::identity();
        let (x, y, z) = (n.x, n.y, n.z);
        m.0[0][0] = 1.0 - 2.0 * x * x;
        m.0[0][1] = -2.0 * x * y;
        m.0[0][2] = -2.0 * x * z;
        m.0[1][0] = -2.0 * x * y;
        m.0[1][1] = 1.0 - 2.0 * y * y;
        m.0[1][2] = -2.0 * y * z;
        m.0[2][0] = -2.0 * x * z;
        m.0[2][1] = -2.0 * y * z;
        m.0[2][2] = 1.0 - 2.0 * z * z;
        Mat4::translation(o) * m * Mat4::translation(-o)
    }
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let m = &self.0;
        let w = m[0][3] * p.x + m[1][3] * p.y + m[2][3] * p.z + m[3][3];
        let v = Vec3::new(
            m[0][0] * p.x + m[1][0] * p.y + m[2][0] * p.z + m[3][0],
            m[0][1] * p.x + m[1][1] * p.y + m[2][1] * p.z + m[3][1],
            m[0][2] * p.x + m[1][2] * p.y + m[2][2] * p.z + m[3][2],
        );
        if (w - 1.0).abs() > EPS && w.abs() > EPS {
            v / w
        } else {
            v
        }
    }
    /// Transforms a direction (ignores translation). Not normalized.
    pub fn transform_vector(&self, p: Vec3) -> Vec3 {
        let m = &self.0;
        Vec3::new(
            m[0][0] * p.x + m[1][0] * p.y + m[2][0] * p.z,
            m[0][1] * p.x + m[1][1] * p.y + m[2][1] * p.z,
            m[0][2] * p.x + m[1][2] * p.y + m[2][2] * p.z,
        )
    }
}

impl Mul for Mat4 {
    type Output = Mat4;
    fn mul(self, o: Mat4) -> Mat4 {
        let mut r = [[0.0; 4]; 4];
        for c in 0..4 {
            for row in 0..4 {
                let mut acc = 0.0;
                for k in 0..4 {
                    acc += self.0[k][row] * o.0[c][k];
                }
                r[c][row] = acc;
            }
        }
        Mat4(r)
    }
}

/// A construction plane defined by origin + orthonormal x/y axes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Plane {
    pub origin: Vec3,
    pub x_axis: Vec3,
    pub y_axis: Vec3,
}

impl Plane {
    /// World XY plane at `origin`.
    pub fn world_xy_at(origin: Vec3) -> Plane {
        Plane { origin, x_axis: Vec3::X, y_axis: Vec3::Y }
    }
    pub fn world_xy() -> Plane {
        Plane::world_xy_at(Vec3::ZERO)
    }
    /// Plane from origin + normal; x/y axes chosen deterministically.
    pub fn from_normal(origin: Vec3, normal: Vec3) -> Plane {
        let n = normal.normalized();
        if n == Vec3::ZERO {
            return Plane::world_xy_at(origin);
        }
        let x_axis = n.any_perpendicular();
        let y_axis = n.cross(x_axis).normalized();
        Plane { origin, x_axis, y_axis }
    }
    pub fn normal(&self) -> Vec3 {
        self.x_axis.cross(self.y_axis).normalized()
    }
    /// Plane-space (u,v,w) -> world.
    pub fn point_at(&self, u: f64, v: f64) -> Vec3 {
        self.origin + self.x_axis * u + self.y_axis * v
    }
    pub fn point_at_3(&self, u: f64, v: f64, w: f64) -> Vec3 {
        self.point_at(u, v) + self.normal() * w
    }
    pub fn transformed(&self, m: &Mat4) -> Plane {
        Plane {
            origin: m.transform_point(self.origin),
            x_axis: m.transform_vector(self.x_axis).normalized(),
            y_axis: m.transform_vector(self.y_axis).normalized(),
        }
    }
}

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub min: Vec3,
    pub max: Vec3,
}

impl BBox {
    pub const EMPTY: BBox = BBox {
        min: Vec3 { x: f64::INFINITY, y: f64::INFINITY, z: f64::INFINITY },
        max: Vec3 { x: f64::NEG_INFINITY, y: f64::NEG_INFINITY, z: f64::NEG_INFINITY },
    };
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }
    pub fn include(&mut self, p: Vec3) {
        self.min = Vec3::new(self.min.x.min(p.x), self.min.y.min(p.y), self.min.z.min(p.z));
        self.max = Vec3::new(self.max.x.max(p.x), self.max.y.max(p.y), self.max.z.max(p.z));
    }
    pub fn union(mut self, o: BBox) -> BBox {
        if !o.is_empty() {
            self.include(o.min);
            self.include(o.max);
        }
        self
    }
    pub fn from_points<'a>(pts: impl IntoIterator<Item = &'a Vec3>) -> BBox {
        let mut b = BBox::EMPTY;
        for p in pts {
            b.include(*p);
        }
        b
    }
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
    pub fn diagonal(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            (self.max - self.min).length()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_quarter_turn() {
        let m = Mat4::rotation_axis(Vec3::ZERO, Vec3::Z, std::f64::consts::FRAC_PI_2);
        let p = m.transform_point(Vec3::X);
        assert!((p - Vec3::Y).length() < 1e-12);
    }

    #[test]
    fn matmul_translation_order() {
        // M = T * R: rotate first, then translate.
        let m = Mat4::translation(Vec3::new(10.0, 0.0, 0.0))
            * Mat4::rotation_axis(Vec3::ZERO, Vec3::Z, std::f64::consts::PI);
        let p = m.transform_point(Vec3::X);
        assert!((p - Vec3::new(9.0, 0.0, 0.0)).length() < 1e-12);
    }

    #[test]
    fn plane_from_normal_orthonormal() {
        let pl = Plane::from_normal(Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 1.0, 1.0));
        assert!(pl.x_axis.dot(pl.y_axis).abs() < 1e-12);
        assert!((pl.normal().length() - 1.0).abs() < 1e-12);
        assert!((pl.normal() - Vec3::new(1.0, 1.0, 1.0).normalized()).length() < 1e-9);
    }
}
