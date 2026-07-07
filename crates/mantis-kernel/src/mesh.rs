//! Triangle mesh + primitives. CONTRACT STUB — implement bodies, keep signatures.

use crate::math::{BBox, Mat4, Plane, Vec3};
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
    /// Area-weighted per-vertex normals.
    pub fn recompute_normals(&mut self) {
        todo!("kernel-agent")
    }
    pub fn transform(&mut self, m: &Mat4) {
        let _ = m;
        todo!("kernel-agent")
    }
    pub fn transformed(&self, m: &Mat4) -> Mesh {
        let mut c = self.clone();
        c.transform(m);
        c
    }
    /// Append `other`, reindexing.
    pub fn append(&mut self, other: &Mesh) {
        let _ = other;
        todo!("kernel-agent")
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
    /// Total surface area.
    pub fn area(&self) -> f64 {
        todo!("kernel-agent")
    }
    /// Signed volume (divergence theorem); meaningful for closed meshes.
    pub fn volume(&self) -> f64 {
        todo!("kernel-agent")
    }
    /// Wavefront OBJ (positions + normals + faces), deterministic output.
    pub fn to_obj(&self) -> String {
        todo!("kernel-agent")
    }
    /// Approximate serialized size in bytes (for the "op-log vs geometry"
    /// size comparison in the UI): 12 floats-ish per vertex + indices.
    pub fn approx_byte_size(&self) -> usize {
        self.positions.len() * 24 + self.normals.len() * 24 + self.indices.len() * 12
    }

    // ---- primitives (normals must be recomputed & outward) ----

    /// Axis-aligned-in-plane box: extents x,y along plane axes, z along normal.
    pub fn box_mesh(plane: &Plane, x: f64, y: f64, z: f64) -> Mesh {
        let _ = (plane, x, y, z);
        todo!("kernel-agent")
    }
    /// UV sphere. u_segs >= 3 around, v_segs >= 2 stacks.
    pub fn sphere(center: Vec3, radius: f64, u_segs: usize, v_segs: usize) -> Mesh {
        let _ = (center, radius, u_segs, v_segs);
        todo!("kernel-agent")
    }
    /// Capped cylinder sitting on `plane`, height along plane normal.
    pub fn cylinder(plane: &Plane, radius: f64, height: f64, segs: usize) -> Mesh {
        let _ = (plane, radius, height, segs);
        todo!("kernel-agent")
    }
    /// Capped cone (apex up the plane normal).
    pub fn cone(plane: &Plane, radius: f64, height: f64, segs: usize) -> Mesh {
        let _ = (plane, radius, height, segs);
        todo!("kernel-agent")
    }
    /// Torus around plane normal. major = ring radius, minor = tube radius.
    pub fn torus(plane: &Plane, major: f64, minor: f64, u_segs: usize, v_segs: usize) -> Mesh {
        let _ = (plane, major, minor, u_segs, v_segs);
        todo!("kernel-agent")
    }
}
