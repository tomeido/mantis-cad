//! Triangle mesh + primitives.
//!
//! Conventions: triangles are CCW seen from outside (right-hand rule gives
//! the outward normal); all primitives are geometrically closed (watertight)
//! and wound so that `volume()` is positive.

use crate::math::{BBox, Mat4, Plane, Vec3, EPS};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    /// Per-vertex normals; same length as positions (recompute_normals ensures).
    pub normals: Vec<Vec3>,
    /// CCW triangles (right-hand rule -> outward normal).
    pub indices: Vec<[u32; 3]>,
}

impl Mesh {
    pub fn new() -> Mesh {
        Mesh::default()
    }

    /// Area-weighted per-vertex normals. Degenerate (zero-area) triangles and
    /// out-of-range indices are skipped; vertices that end up with a zero
    /// normal get Vec3::Z as a deterministic fallback.
    pub fn recompute_normals(&mut self) {
        let n = self.positions.len();
        let mut acc = vec![Vec3::ZERO; n];
        for tri in &self.indices {
            let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if ia >= n || ib >= n || ic >= n {
                continue;
            }
            let (a, b, c) = (self.positions[ia], self.positions[ib], self.positions[ic]);
            // Cross product magnitude = 2 * area -> already area-weighted.
            let cr = (b - a).cross(c - a);
            if cr.length_sq() < EPS * EPS {
                continue;
            }
            acc[ia] = acc[ia] + cr;
            acc[ib] = acc[ib] + cr;
            acc[ic] = acc[ic] + cr;
        }
        self.normals = acc
            .into_iter()
            .map(|v| {
                let u = v.normalized();
                if u == Vec3::ZERO {
                    Vec3::Z
                } else {
                    u
                }
            })
            .collect();
    }

    /// Transform positions as points and normals as directions
    /// (renormalized afterwards).
    pub fn transform(&mut self, m: &Mat4) {
        for p in &mut self.positions {
            *p = m.transform_point(*p);
        }
        for nrm in &mut self.normals {
            *nrm = m.transform_vector(*nrm).normalized();
        }
    }

    pub fn transformed(&self, m: &Mat4) -> Mesh {
        let mut c = self.clone();
        c.transform(m);
        c
    }

    /// Append `other`, reindexing its triangles. Normal arrays are padded
    /// with Vec3::ZERO if either mesh lacks per-vertex normals, so the
    /// `normals.len() == positions.len()` invariant is preserved (call
    /// `recompute_normals` afterwards for correct shading).
    pub fn append(&mut self, other: &Mesh) {
        let base = self.positions.len() as u32;
        // Keep normals in sync with positions on both sides.
        if self.normals.len() != self.positions.len() {
            self.normals.resize(self.positions.len(), Vec3::ZERO);
        }
        self.positions.extend_from_slice(&other.positions);
        self.normals.extend_from_slice(&other.normals);
        if self.normals.len() != self.positions.len() {
            self.normals.resize(self.positions.len(), Vec3::ZERO);
        }
        self.indices
            .extend(other.indices.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }

    pub fn bbox(&self) -> BBox {
        BBox::from_points(&self.positions)
    }
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }
    pub fn triangle_count(&self) -> usize {
        self.indices.len()
    }

    /// Total surface area (sum of triangle areas; invalid indices skipped).
    pub fn area(&self) -> f64 {
        let n = self.positions.len();
        let mut acc = 0.0;
        for tri in &self.indices {
            let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if ia >= n || ib >= n || ic >= n {
                continue;
            }
            let (a, b, c) = (self.positions[ia], self.positions[ib], self.positions[ic]);
            acc += 0.5 * (b - a).cross(c - a).length();
        }
        acc
    }

    /// Signed volume via the divergence theorem (sum of signed tetrahedra
    /// against the origin); meaningful for closed meshes, positive when
    /// triangles are CCW seen from outside.
    pub fn volume(&self) -> f64 {
        let n = self.positions.len();
        let mut acc = 0.0;
        for tri in &self.indices {
            let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if ia >= n || ib >= n || ic >= n {
                continue;
            }
            let (a, b, c) = (self.positions[ia], self.positions[ib], self.positions[ic]);
            acc += a.dot(b.cross(c)) / 6.0;
        }
        acc
    }

    /// Wavefront OBJ export: one `v` line per position, one `vn` line per
    /// normal, one `f a//a b//b c//c` line per triangle (1-based indices).
    /// Floats use Rust's default `Display` formatting, which is exact and
    /// deterministic (shortest representation that round-trips).
    pub fn to_obj(&self) -> String {
        let mut s = String::with_capacity(self.positions.len() * 32 + self.indices.len() * 16);
        for p in &self.positions {
            s.push_str(&format!("v {} {} {}\n", p.x, p.y, p.z));
        }
        for nrm in &self.normals {
            s.push_str(&format!("vn {} {} {}\n", nrm.x, nrm.y, nrm.z));
        }
        for t in &self.indices {
            let (a, b, c) = (t[0] + 1, t[1] + 1, t[2] + 1);
            s.push_str(&format!("f {}//{} {}//{} {}//{}\n", a, a, b, b, c, c));
        }
        s
    }

    /// Approximate serialized size in bytes (for the "op-log vs geometry"
    /// size comparison in the UI): 12 floats-ish per vertex + indices.
    pub fn approx_byte_size(&self) -> usize {
        self.positions.len() * 24 + self.normals.len() * 24 + self.indices.len() * 12
    }

    /// Push a quad as two triangles with its own 4 vertices (flat shading).
    /// Corners must be given CCW seen from outside.
    fn push_quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
        let i = self.positions.len() as u32;
        self.positions.extend_from_slice(&[a, b, c, d]);
        self.indices.push([i, i + 1, i + 2]);
        self.indices.push([i, i + 2, i + 3]);
    }

    // ---- primitives (normals recomputed & outward) ----

    /// Axis-aligned-in-plane box: extents x,y along plane axes, z along the
    /// plane normal. One corner sits at the plane origin and the box spans
    /// the positive u/v/w octant. Faces carry their own vertices (24 total)
    /// so recomputed normals are flat per-face. Positive extents give
    /// positive volume; zero/negative extents never panic.
    pub fn box_mesh(plane: &Plane, x: f64, y: f64, z: f64) -> Mesh {
        let mut m = Mesh::new();
        let c = |u: f64, v: f64, w: f64| plane.point_at_3(u, v, w);
        // bottom (outward -normal)
        m.push_quad(c(0.0, 0.0, 0.0), c(0.0, y, 0.0), c(x, y, 0.0), c(x, 0.0, 0.0));
        // top (outward +normal)
        m.push_quad(c(0.0, 0.0, z), c(x, 0.0, z), c(x, y, z), c(0.0, y, z));
        // v = 0 (outward -y)
        m.push_quad(c(0.0, 0.0, 0.0), c(x, 0.0, 0.0), c(x, 0.0, z), c(0.0, 0.0, z));
        // u = x (outward +x)
        m.push_quad(c(x, 0.0, 0.0), c(x, y, 0.0), c(x, y, z), c(x, 0.0, z));
        // v = y (outward +y)
        m.push_quad(c(x, y, 0.0), c(0.0, y, 0.0), c(0.0, y, z), c(x, y, z));
        // u = 0 (outward -x)
        m.push_quad(c(0.0, y, 0.0), c(0.0, 0.0, 0.0), c(0.0, 0.0, z), c(0.0, y, z));
        m.recompute_normals();
        m
    }

    /// UV sphere. u_segs clamped >= 3 around, v_segs clamped >= 2 stacks.
    /// Welded seam (index wrap), single pole vertices; watertight.
    pub fn sphere(center: Vec3, radius: f64, u_segs: usize, v_segs: usize) -> Mesh {
        let us = u_segs.max(3);
        let vs = v_segs.max(2);
        let mut m = Mesh::new();
        let north = center + Vec3::Z * radius;
        let south = center - Vec3::Z * radius;
        m.positions.push(north); // index 0
        // Interior rings: v = 1..vs-1, ring r has us vertices.
        for r in 1..vs {
            let phi = std::f64::consts::PI * r as f64 / vs as f64;
            let (sp, cp) = phi.sin_cos();
            for u in 0..us {
                let th = std::f64::consts::TAU * u as f64 / us as f64;
                m.positions.push(
                    center + Vec3::new(sp * th.cos(), sp * th.sin(), cp) * radius,
                );
            }
        }
        let south_i = m.positions.len() as u32;
        m.positions.push(south);
        let ring = |r: usize, u: usize| -> u32 { 1 + (r * us + u % us) as u32 };
        // Top fan.
        for u in 0..us {
            m.indices.push([0, ring(0, u), ring(0, u + 1)]);
        }
        // Bands.
        for r in 0..vs.saturating_sub(2) {
            for u in 0..us {
                let a = ring(r, u);
                let b = ring(r, u + 1);
                let c = ring(r + 1, u + 1);
                let d = ring(r + 1, u);
                m.indices.push([a, d, c]);
                m.indices.push([a, c, b]);
            }
        }
        // Bottom fan.
        let last = vs - 2;
        for u in 0..us {
            m.indices.push([south_i, ring(last, u + 1), ring(last, u)]);
        }
        m.recompute_normals();
        m
    }

    /// Capped cylinder sitting on `plane`, height along the plane normal.
    /// segs clamped >= 3. Side and cap rings use separate vertices so
    /// recomputed normals stay crisp; seam is welded by index wrap.
    pub fn cylinder(plane: &Plane, radius: f64, height: f64, segs: usize) -> Mesh {
        let n = segs.max(3);
        let mut m = Mesh::new();
        let ring_pt = |u: usize, w: f64| -> Vec3 {
            let th = std::f64::consts::TAU * (u % n) as f64 / n as f64;
            plane.point_at_3(radius * th.cos(), radius * th.sin(), w)
        };
        // Side rings: bottom 0..n, top n..2n.
        for u in 0..n {
            m.positions.push(ring_pt(u, 0.0));
        }
        for u in 0..n {
            m.positions.push(ring_pt(u, height));
        }
        for u in 0..n {
            let a = u as u32;
            let b = ((u + 1) % n) as u32;
            let c = (n + (u + 1) % n) as u32;
            let d = (n + u) as u32;
            m.indices.push([a, b, c]);
            m.indices.push([a, c, d]);
        }
        // Caps (duplicated rings + center vertices).
        let bot_start = m.positions.len() as u32;
        for u in 0..n {
            m.positions.push(ring_pt(u, 0.0));
        }
        let bot_center = m.positions.len() as u32;
        m.positions.push(plane.point_at_3(0.0, 0.0, 0.0));
        let top_start = m.positions.len() as u32;
        for u in 0..n {
            m.positions.push(ring_pt(u, height));
        }
        let top_center = m.positions.len() as u32;
        m.positions.push(plane.point_at_3(0.0, 0.0, height));
        for u in 0..n as u32 {
            let un = (u + 1) % n as u32;
            m.indices.push([bot_center, bot_start + un, bot_start + u]); // outward -normal
            m.indices.push([top_center, top_start + u, top_start + un]); // outward +normal
        }
        m.recompute_normals();
        m
    }

    /// Capped cone: base circle on `plane`, apex at height along the plane
    /// normal. segs clamped >= 3.
    pub fn cone(plane: &Plane, radius: f64, height: f64, segs: usize) -> Mesh {
        let n = segs.max(3);
        let mut m = Mesh::new();
        let ring_pt = |u: usize| -> Vec3 {
            let th = std::f64::consts::TAU * (u % n) as f64 / n as f64;
            plane.point_at_3(radius * th.cos(), radius * th.sin(), 0.0)
        };
        // Side ring + apex.
        for u in 0..n {
            m.positions.push(ring_pt(u));
        }
        let apex = m.positions.len() as u32;
        m.positions.push(plane.point_at_3(0.0, 0.0, height));
        for u in 0..n as u32 {
            let un = (u + 1) % n as u32;
            m.indices.push([u, un, apex]);
        }
        // Base cap.
        let base_start = m.positions.len() as u32;
        for u in 0..n {
            m.positions.push(ring_pt(u));
        }
        let base_center = m.positions.len() as u32;
        m.positions.push(plane.point_at_3(0.0, 0.0, 0.0));
        for u in 0..n as u32 {
            let un = (u + 1) % n as u32;
            m.indices.push([base_center, base_start + un, base_start + u]);
        }
        m.recompute_normals();
        m
    }

    /// Torus around the plane normal. major = ring radius, minor = tube
    /// radius. u_segs (around axis) and v_segs (around tube) clamped >= 3.
    /// Fully welded (index wrap in both directions); watertight.
    pub fn torus(plane: &Plane, major: f64, minor: f64, u_segs: usize, v_segs: usize) -> Mesh {
        let us = u_segs.max(3);
        let vs = v_segs.max(3);
        let mut m = Mesh::new();
        for u in 0..us {
            let th = std::f64::consts::TAU * u as f64 / us as f64;
            for v in 0..vs {
                let ph = std::f64::consts::TAU * v as f64 / vs as f64;
                let r = major + minor * ph.cos();
                m.positions.push(plane.point_at_3(r * th.cos(), r * th.sin(), minor * ph.sin()));
            }
        }
        let at = |u: usize, v: usize| -> u32 { ((u % us) * vs + v % vs) as u32 };
        for u in 0..us {
            for v in 0..vs {
                let a = at(u, v);
                let b = at(u + 1, v);
                let c = at(u + 1, v + 1);
                let d = at(u, v + 1);
                m.indices.push([a, b, c]);
                m.indices.push([a, c, d]);
            }
        }
        m.recompute_normals();
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn assert_normals_unit(m: &Mesh) {
        assert_eq!(m.normals.len(), m.positions.len());
        for n in &m.normals {
            assert!((n.length() - 1.0).abs() < 1e-9, "normal not unit: {:?}", n);
        }
    }

    /// Outward normals <=> positive volume; also sanity that recompute ran.
    fn assert_closed_outward(m: &Mesh) {
        assert!(m.volume() > 0.0, "volume {} not positive", m.volume());
        assert_normals_unit(m);
    }

    #[test]
    fn box_exact_volume_and_area() {
        let m = Mesh::box_mesh(&Plane::world_xy(), 2.0, 3.0, 4.0);
        assert_eq!(m.vertex_count(), 24);
        assert_eq!(m.triangle_count(), 12);
        assert!((m.volume() - 24.0).abs() < 1e-9);
        let area = 2.0 * (2.0 * 3.0 + 3.0 * 4.0 + 2.0 * 4.0);
        assert!((m.area() - area).abs() < 1e-9);
        assert_closed_outward(&m);
    }

    #[test]
    fn box_on_tilted_plane() {
        let plane = Plane::from_normal(Vec3::new(5.0, -2.0, 1.0), Vec3::new(1.0, 2.0, 3.0));
        let m = Mesh::box_mesh(&plane, 1.0, 2.0, 3.0);
        assert!((m.volume() - 6.0).abs() < 1e-9);
        assert!((m.area() - 22.0).abs() < 1e-9);
    }

    #[test]
    fn sphere_volume_area() {
        let m = Mesh::sphere(Vec3::new(1.0, 2.0, 3.0), 1.5, 48, 24);
        let exact_v = 4.0 / 3.0 * PI * 1.5f64.powi(3);
        let exact_a = 4.0 * PI * 1.5f64 * 1.5;
        assert!((m.volume() - exact_v).abs() / exact_v < 0.02, "vol {}", m.volume());
        assert!((m.area() - exact_a).abs() / exact_a < 0.02, "area {}", m.area());
        assert_closed_outward(&m);
        // Center offset respected.
        assert!(m.bbox().center().distance(Vec3::new(1.0, 2.0, 3.0)) < 1e-9);
    }

    #[test]
    fn sphere_clamps_degenerate_segs() {
        let m = Mesh::sphere(Vec3::ZERO, 1.0, 0, 0);
        assert!(m.volume() > 0.0);
    }

    #[test]
    fn cylinder_volume() {
        let m = Mesh::cylinder(&Plane::world_xy(), 1.0, 3.0, 64);
        let exact = PI * 3.0;
        assert!((m.volume() - exact).abs() / exact < 0.01, "vol {}", m.volume());
        let exact_a = 2.0 * PI * 3.0 + 2.0 * PI;
        assert!((m.area() - exact_a).abs() / exact_a < 0.01);
        assert_closed_outward(&m);
    }

    #[test]
    fn cone_volume() {
        let m = Mesh::cone(&Plane::world_xy(), 1.0, 3.0, 64);
        let exact = PI * 3.0 / 3.0;
        assert!((m.volume() - exact).abs() / exact < 0.01, "vol {}", m.volume());
        assert_closed_outward(&m);
    }

    #[test]
    fn torus_volume() {
        let m = Mesh::torus(&Plane::world_xy(), 3.0, 1.0, 64, 32);
        let exact = 2.0 * PI * PI * 3.0; // 2 π² R r²
        assert!((m.volume() - exact).abs() / exact < 0.02, "vol {}", m.volume());
        let exact_a = 4.0 * PI * PI * 3.0;
        assert!((m.area() - exact_a).abs() / exact_a < 0.02);
        assert_closed_outward(&m);
    }

    #[test]
    fn transform_rigid_preserves_volume_and_area() {
        let m = Mesh::box_mesh(&Plane::world_xy(), 2.0, 3.0, 4.0);
        let rigid = Mat4::translation(Vec3::new(-4.0, 2.0, 9.0))
            * Mat4::rotation_axis(Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 2.0, 3.0), 1.234);
        let t = m.transformed(&rigid);
        assert!((t.volume() - m.volume()).abs() < 1e-9);
        assert!((t.area() - m.area()).abs() < 1e-9);
        assert_normals_unit(&t);
    }

    #[test]
    fn transform_uniform_scale_volume() {
        let m = Mesh::box_mesh(&Plane::world_xy(), 1.0, 1.0, 1.0);
        let t = m.transformed(&Mat4::scaling_uniform(Vec3::ZERO, 2.0));
        assert!((t.volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn append_reindexes() {
        let mut a = Mesh::box_mesh(&Plane::world_xy(), 1.0, 1.0, 1.0);
        let b = Mesh::box_mesh(&Plane::world_xy_at(Vec3::new(5.0, 0.0, 0.0)), 1.0, 1.0, 1.0);
        let va = a.volume();
        a.append(&b);
        assert_eq!(a.vertex_count(), 48);
        assert_eq!(a.triangle_count(), 24);
        assert!((a.volume() - 2.0 * va).abs() < 1e-9);
        assert_eq!(a.normals.len(), a.positions.len());
        let max_index = a.indices.iter().flatten().copied().max().unwrap();
        assert!((max_index as usize) < a.vertex_count());
    }

    #[test]
    fn recompute_normals_flat_box_faces() {
        let m = Mesh::box_mesh(&Plane::world_xy(), 1.0, 1.0, 1.0);
        // The top face is the second quad (verts 4..8): normal must be +Z.
        for i in 4..8 {
            assert!(m.normals[i].distance(Vec3::Z) < 1e-12);
        }
    }

    #[test]
    fn recompute_normals_skips_degenerate() {
        let mut m = Mesh {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0, Vec3::Y],
            normals: vec![],
            indices: vec![[0, 1, 2], [0, 1, 3]], // first is degenerate (collinear)
        };
        m.recompute_normals();
        assert_eq!(m.normals.len(), 4);
        assert!(m.normals[3].distance(Vec3::Z) < 1e-12);
        // Out-of-range indices must not panic.
        m.indices.push([0, 1, 99]);
        m.recompute_normals();
        assert!((m.area() - 0.5).abs() < 1e-12);
        let _ = m.volume();
    }

    #[test]
    fn obj_output_shape() {
        let m = Mesh::box_mesh(&Plane::world_xy(), 1.0, 1.0, 1.0);
        let obj = m.to_obj();
        let v_lines = obj.lines().filter(|l| l.starts_with("v ")).count();
        let vn_lines = obj.lines().filter(|l| l.starts_with("vn ")).count();
        let f_lines = obj.lines().filter(|l| l.starts_with("f ")).count();
        assert_eq!(v_lines, m.vertex_count());
        assert_eq!(vn_lines, m.vertex_count());
        assert_eq!(f_lines, m.triangle_count());
        assert_eq!(obj.lines().count(), v_lines + vn_lines + f_lines);
        // 1-based face indices, v//vn syntax.
        assert!(obj.contains("f 1//1 2//2 3//3"));
        // Deterministic: same mesh -> same string.
        assert_eq!(obj, m.to_obj());
    }

    #[test]
    fn empty_mesh_safe() {
        let mut m = Mesh::new();
        m.recompute_normals();
        assert_eq!(m.volume(), 0.0);
        assert_eq!(m.area(), 0.0);
        assert_eq!(m.to_obj(), "");
        assert!(m.bbox().is_empty());
    }
}
