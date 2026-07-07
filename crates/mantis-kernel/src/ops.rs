//! Surface-generating operations (the Grasshopper verbs).
//! CONTRACT STUB — implement bodies, keep signatures.

use crate::curve::Curve;
use crate::math::Vec3;
use crate::mesh::Mesh;

/// Extrude a curve along `dir`. Closed planar curves get caps
/// (ear-clipping triangulation of the profile).
pub fn extrude(curve: &Curve, dir: Vec3, segments: usize) -> Mesh {
    let _ = (curve, dir, segments);
    todo!("kernel-agent")
}

/// Revolve a profile curve around axis (origin+dir) by `angle` radians
/// (2π = full revolution, welded seam not required).
pub fn revolve(profile: &Curve, axis_origin: Vec3, axis_dir: Vec3, angle: f64, segments: usize) -> Mesh {
    let _ = (profile, axis_origin, axis_dir, angle, segments);
    todo!("kernel-agent")
}

/// Skin through 2+ section curves (each tessellated to the same count).
/// Closed sections -> closed tube; caps not required.
pub fn loft(sections: &[Curve], segments_per_section: usize) -> Result<Mesh, String> {
    let _ = (sections, segments_per_section);
    todo!("kernel-agent")
}

/// Circular tube swept along a rail curve using parallel-transport frames.
pub fn pipe(rail: &Curve, radius: f64, rail_segments: usize, ring_segments: usize) -> Mesh {
    let _ = (rail, radius, rail_segments, ring_segments);
    todo!("kernel-agent")
}

/// Fill a closed (approximately) planar curve with triangles (ear clipping).
/// Errors on open curves or degenerate input.
pub fn planar_surface(boundary: &Curve, segments: usize) -> Result<Mesh, String> {
    let _ = (boundary, segments);
    todo!("kernel-agent")
}
