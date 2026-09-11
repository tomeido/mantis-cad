//! Bounded polygonal solid operations. These operate on watertight, oriented
//! triangle meshes, not on NURBS B-reps. Curved surfaces retain their input
//! tessellation. Numerically ambiguous or non-manifold results are rejected.
//!
//! The BSP clipping identities follow Evan Wallace's MIT-licensed csg.js
//! (https://github.com/evanw/csg.js); attribution is in THIRD_PARTY_LICENSES.md.
//! Tree traversal is iterative; work, input size and fragment counts are
//! bounded. Final polygon edges are conformed before triangulation so BSP
//! splitting does not leave open T-junctions in the exported mesh.

use crate::{Mesh, Plane, Vec3};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SOLID_TRIANGLES: usize = 4096;
const MAX_FRAGMENTS: usize = 32768;
const MAX_WORK: usize = 40_000_000;
const MAX_COORDINATE: f64 = 1.0e9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOp {
    Union,
    Difference,
    Intersection,
}

struct Budget {
    remaining: usize,
    fragments: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            remaining: MAX_WORK,
            fragments: MAX_FRAGMENTS,
        }
    }
    fn spend(&mut self, work: usize) -> Result<(), String> {
        self.remaining = self
            .remaining
            .checked_sub(work)
            .ok_or("mesh solid operation exceeded work limit; reduce mesh resolution")?;
        Ok(())
    }
}

fn tolerance_valid(tolerance: f64) -> Result<(), String> {
    if !tolerance.is_finite() || !(1e-9..=1.0).contains(&tolerance) {
        return Err("mesh tolerance must be a finite number from 1e-9 to 1".into());
    }
    Ok(())
}

/// Deterministic tolerance weld. Neighbor cells prevent quantization seams.
struct Weld {
    points: Vec<Vec3>,
    cells: BTreeMap<(i64, i64, i64), Vec<u32>>,
    tolerance: f64,
}
impl Weld {
    fn new(tolerance: f64) -> Self {
        Self {
            points: Vec::new(),
            cells: BTreeMap::new(),
            tolerance,
        }
    }
    fn add(&mut self, point: Vec3) -> Result<u32, String> {
        if !point.is_finite()
            || point.x.abs().max(point.y.abs()).max(point.z.abs()) > MAX_COORDINATE
        {
            return Err("mesh solid coordinates must be finite and within ±1e9".into());
        }
        let cell = (
            (point.x / self.tolerance).floor() as i64,
            (point.y / self.tolerance).floor() as i64,
            (point.z / self.tolerance).floor() as i64,
        );
        let mut found = None;
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    if let Some(ids) = self.cells.get(&(cell.0 + x, cell.1 + y, cell.2 + z)) {
                        for id in ids {
                            if self.points[*id as usize].distance(point) <= self.tolerance {
                                found = Some(found.map_or(*id, |old: u32| old.min(*id)));
                            }
                        }
                    }
                }
            }
        }
        if let Some(id) = found {
            return Ok(id);
        }
        if self.points.len() >= MAX_FRAGMENTS * 4 {
            return Err("mesh solid vertex limit exceeded".into());
        }
        let id = self.points.len() as u32;
        self.points.push(point);
        self.cells.entry(cell).or_default().push(id);
        Ok(id)
    }
}

/// Validate geometrically welded closed edges and manifold vertex links.
/// Return a welded mesh; hard face normals on duplicated input vertices are
/// intentionally not part of the topology.
fn checked_mesh(
    mesh: &Mesh,
    tolerance: f64,
    allow_empty: bool,
    limit: usize,
) -> Result<Mesh, String> {
    if mesh.indices.is_empty() {
        return if allow_empty {
            Ok(Mesh::new())
        } else {
            Err("mesh solid input is empty".into())
        };
    }
    if mesh.indices.len() > limit || mesh.positions.len() > limit * 3 {
        return Err(format!(
            "mesh solid input exceeds {limit} triangles; reduce resolution"
        ));
    }
    if mesh
        .positions
        .iter()
        .any(|p| !p.is_finite() || p.x.abs().max(p.y.abs()).max(p.z.abs()) > MAX_COORDINATE)
    {
        return Err("mesh solid coordinates must be finite and within ±1e9".into());
    }
    let mut weld = Weld::new(tolerance);
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for tri in &mesh.indices {
        let mut ids = [0; 3];
        for (i, id) in tri.iter().enumerate() {
            let p = mesh
                .positions
                .get(*id as usize)
                .ok_or("mesh contains an invalid vertex index")?;
            ids[i] = weld.add(*p)?;
        }
        if ids[0] == ids[1] || ids[1] == ids[2] || ids[2] == ids[0] {
            return Err("mesh contains an edge shorter than the tolerance".into());
        }
        let a = weld.points[ids[0] as usize];
        let b = weld.points[ids[1] as usize];
        let c = weld.points[ids[2] as usize];
        if (b - a).cross(c - a).length() <= tolerance * tolerance {
            return Err("mesh contains a degenerate triangle".into());
        }
        indices.push(ids);
    }
    check_topology(&weld.points, &indices, tolerance)?;
    let mut output = Mesh {
        positions: weld.points,
        indices,
        normals: Vec::new(),
    };
    output.recompute_normals();
    Ok(output)
}

fn check_topology(points: &[Vec3], triangles: &[[u32; 3]], tolerance: f64) -> Result<(), String> {
    if triangles.is_empty() {
        return Ok(());
    }
    let mut edges: BTreeMap<(u32, u32), (usize, i32)> = BTreeMap::new();
    let mut links: Vec<Vec<(u32, u32)>> = vec![Vec::new(); points.len()];
    for tri in triangles {
        for i in 0..3 {
            let (a, b, c) = (tri[i], tri[(i + 1) % 3], tri[(i + 2) % 3]);
            let key = (a.min(b), a.max(b));
            let edge = edges.entry(key).or_default();
            edge.0 += 1;
            edge.1 += if a < b { 1 } else { -1 };
            links[a as usize].push((b, c));
        }
    }
    if edges
        .values()
        .any(|(count, direction)| *count != 2 || *direction != 0)
    {
        return Err("mesh must be watertight with consistently oriented manifold edges".into());
    }
    // A pair of shells touching at one vertex has valid edges but a disconnected
    // vertex link. Reject that ambiguity rather than emitting a non-solid.
    for link in &links {
        if link.is_empty() {
            continue;
        }
        let next: BTreeMap<_, _> = link.iter().copied().collect();
        if next.len() != link.len() {
            return Err("mesh has a non-manifold vertex".into());
        }
        let start = link[0].0;
        let mut vertex = start;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(vertex) {
                break;
            }
            vertex = *next.get(&vertex).ok_or("mesh has an open vertex link")?;
        }
        if vertex != start || visited.len() != link.len() {
            return Err("mesh has touching shells or a non-manifold vertex".into());
        }
    }
    // Shift the volume origin near the geometry to avoid cancellation on
    // translated models. Negative winding is rejected, never silently flipped.
    let origin = points[triangles[0][0] as usize];
    let volume = triangles
        .iter()
        .map(|t| {
            let a = points[t[0] as usize] - origin;
            let b = points[t[1] as usize] - origin;
            let c = points[t[2] as usize] - origin;
            a.dot(b.cross(c)) / 6.0
        })
        .sum::<f64>();
    if !volume.is_finite() || volume <= tolerance.powi(3) {
        return Err("mesh must enclose positive volume with outward face winding".into());
    }
    Ok(())
}

fn triangle_contains(
    point: Vec3,
    triangle: [Vec3; 3],
    normal: Vec3,
    tolerance: f64,
    strict: bool,
) -> bool {
    (0..3).all(|i| {
        let edge = triangle[(i + 1) % 3] - triangle[i];
        let side = edge.cross(point - triangle[i]).dot(normal);
        let margin = tolerance * edge.length();
        if strict {
            side > margin
        } else {
            side >= -margin
        }
    })
}

/// Reject crossing and overlapping input shells. Topological edge counts
/// alone cannot detect e.g. two intersecting boxes appended into one mesh.
/// A sweep over triangle X bounds avoids testing spatially separated faces.
fn check_self_intersections(
    mesh: &Mesh,
    tolerance: f64,
    budget: &mut Budget,
) -> Result<(), String> {
    let triangles: Vec<_> = mesh
        .indices
        .iter()
        .map(|t| t.map(|id| mesh.positions[id as usize]))
        .collect();
    let bounds: Vec<_> = triangles.iter().map(crate::BBox::from_points).collect();
    let mut order: Vec<_> = (0..triangles.len()).collect();
    order.sort_by(|a, b| bounds[*a].min.x.total_cmp(&bounds[*b].min.x).then(a.cmp(b)));
    for (position, &a) in order.iter().enumerate() {
        for &b in &order[position + 1..] {
            if bounds[b].min.x > bounds[a].max.x + tolerance {
                break;
            }
            budget.spend(1)?;
            if bounds[a].min.y > bounds[b].max.y + tolerance
                || bounds[b].min.y > bounds[a].max.y + tolerance
                || bounds[a].min.z > bounds[b].max.z + tolerance
                || bounds[b].min.z > bounds[a].max.z + tolerance
            {
                continue;
            }
            let shared: Vec<_> = mesh.indices[a]
                .iter()
                .filter(|id| mesh.indices[b].contains(id))
                .map(|id| mesh.positions[*id as usize])
                .collect();
            let pa = triangles[a];
            let pb = triangles[b];
            let na = (pa[1] - pa[0]).cross(pa[2] - pa[0]);
            let na = na / na.length();
            let nb = (pb[1] - pb[0]).cross(pb[2] - pb[0]);
            let nb = nb / nb.length();
            let coplanar = pa.iter().all(|p| (*p - pb[0]).dot(nb).abs() <= tolerance)
                && pb.iter().all(|p| (*p - pa[0]).dot(na).abs() <= tolerance);
            if coplanar {
                // Strict containment, including centroids, catches coincident
                // or partially overlaid coplanar faces without flagging seams.
                for (left, right, normal) in [(pa, pb, nb), (pb, pa, na)] {
                    let centroid = (left[0] + left[1] + left[2]) / 3.0;
                    if left
                        .iter()
                        .chain(std::iter::once(&centroid))
                        .any(|p| triangle_contains(*p, right, normal, tolerance, true))
                    {
                        return Err(
                            "mesh contains overlapping coplanar faces or self-intersections".into(),
                        );
                    }
                    // Edge crossings can overlap without containing a vertex.
                    for i in 0..3 {
                        for j in 0..3 {
                            let p = left[i];
                            let q = left[(i + 1) % 3];
                            let r = right[j];
                            let s = right[(j + 1) % 3];
                            let d = (q - p).cross(s - r).dot(normal);
                            if d.abs() <= tolerance * tolerance {
                                continue;
                            }
                            let t = (r - p).cross(s - r).dot(normal) / d;
                            let u = (r - p).cross(q - p).dot(normal) / d;
                            let margin_t = tolerance / (q - p).length();
                            let margin_u = tolerance / (s - r).length();
                            if t > margin_t
                                && t < 1.0 - margin_t
                                && u > margin_u
                                && u < 1.0 - margin_u
                            {
                                return Err("mesh contains overlapping coplanar faces or self-intersections".into());
                            }
                        }
                    }
                }
            } else if shared.len() < 2 {
                for (left, right, normal) in [(pa, pb, nb), (pb, pa, na)] {
                    for i in 0..3 {
                        let p = left[i];
                        let q = left[(i + 1) % 3];
                        let d0 = (p - right[0]).dot(normal);
                        let d1 = (q - right[0]).dot(normal);
                        let mut candidates = Vec::new();
                        if d0.abs() <= tolerance {
                            candidates.push(p);
                        }
                        if d1.abs() <= tolerance {
                            candidates.push(q);
                        }
                        if d0.abs() <= tolerance && d1.abs() <= tolerance {
                            candidates.push((p + q) * 0.5);
                        }
                        if (d0 < -tolerance && d1 > tolerance)
                            || (d1 < -tolerance && d0 > tolerance)
                        {
                            candidates.push(p.lerp(q, d0 / (d0 - d1)));
                        }
                        if candidates.into_iter().any(|p| {
                            shared.iter().all(|s| p.distance(*s) > tolerance * 2.0)
                                && triangle_contains(p, right, normal, tolerance, false)
                        }) {
                            return Err(
                                "mesh contains crossing faces or intersecting shells".into()
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Each material boundary must face out of the material: disconnected outer
/// shells point outwards, cavity shells inwards, islands inside cavities out.
/// A positive *total* volume alone cannot establish this for multiple shells.
/// Containment uses triangle solid angles / generalized winding numbers:
/// https://igl.ethz.ch/projects/winding-number/ (Jacobson et al., 2013).
fn check_shell_winding(mesh: &Mesh, tolerance: f64, budget: &mut Budget) -> Result<(), String> {
    if mesh.indices.is_empty() {
        return Ok(());
    }
    fn root(parents: &mut [usize], mut id: usize) -> usize {
        while parents[id] != id {
            parents[id] = parents[parents[id]];
            id = parents[id];
        }
        id
    }
    let mut parents: Vec<usize> = (0..mesh.positions.len()).collect();
    for t in &mesh.indices {
        let a = root(&mut parents, t[0] as usize);
        for &id in &t[1..] {
            let b = root(&mut parents, id as usize);
            parents[b] = a;
        }
    }
    let mut connected: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, t) in mesh.indices.iter().enumerate() {
        connected
            .entry(root(&mut parents, t[0] as usize))
            .or_default()
            .push(index);
    }
    // The existing topology check already verifies the one-shell volume.
    if connected.len() == 1 {
        return Ok(());
    }
    struct Shell {
        triangles: Vec<usize>,
        point: Vec3,
        bounds: crate::BBox,
        volume: f64,
    }
    let mut shells = Vec::with_capacity(connected.len());
    for triangles in connected.into_values() {
        let point = mesh.positions[mesh.indices[triangles[0]][0] as usize];
        let mut bounds = crate::BBox::EMPTY;
        let mut volume = 0.0;
        for &index in &triangles {
            let [a, b, c] = mesh.indices[index].map(|id| mesh.positions[id as usize]);
            for p in [a, b, c] {
                bounds.include(p);
            }
            volume += (a - point).dot((b - point).cross(c - point)) / 6.0;
        }
        if !volume.is_finite() || volume.abs() <= tolerance.powi(3) {
            return Err("mesh shell must enclose nonzero volume".into());
        }
        shells.push(Shell {
            triangles,
            point,
            bounds,
            volume,
        });
    }
    for (index, shell) in shells.iter().enumerate() {
        let mut depth = 0usize;
        for (other_index, other) in shells.iter().enumerate() {
            budget.spend(1)?;
            if index == other_index {
                continue;
            }
            let p = shell.point;
            let b = &other.bounds;
            if p.x < b.min.x - tolerance
                || p.x > b.max.x + tolerance
                || p.y < b.min.y - tolerance
                || p.y > b.max.y + tolerance
                || p.z < b.min.z - tolerance
                || p.z > b.max.z + tolerance
            {
                continue;
            }
            budget.spend(other.triangles.len())?;
            let mut angle = 0.0;
            for &triangle in &other.triangles {
                let [a, b, c] = mesh.indices[triangle].map(|id| mesh.positions[id as usize] - p);
                let (la, lb, lc) = (a.length(), b.length(), c.length());
                if la.min(lb).min(lc) <= tolerance {
                    return Err("mesh shell nesting is ambiguous at this tolerance".into());
                }
                // Normalize first to keep the solid-angle expression stable
                // at very small/large model scales. Orientation affects only
                // the sign; absolute winding determines containment.
                let (a, b, c) = (a / la, b / lb, c / lc);
                angle += 2.0
                    * a.dot(b.cross(c))
                        .atan2(1.0 + a.dot(b) + b.dot(c) + c.dot(a));
            }
            let winding = (angle / (4.0 * std::f64::consts::PI)).abs();
            if !winding.is_finite() {
                return Err("mesh shell nesting is numerically ambiguous".into());
            }
            if (winding - 1.0).abs() <= 1e-5 {
                depth += 1;
            } else if winding > 1e-5 {
                return Err("mesh shells do not define unambiguous nested boundaries".into());
            }
        }
        if (shell.volume > 0.0) != depth.is_multiple_of(2) {
            return Err("mesh shell winding is inconsistent: outer shells must face outward and cavity shells inward".into());
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct CutPlane {
    normal: Vec3,
    distance: f64,
}
#[derive(Clone)]
struct Polygon {
    vertices: Vec<Vec3>,
    plane: CutPlane,
}

impl Polygon {
    fn triangle(a: Vec3, b: Vec3, c: Vec3) -> Self {
        let cross = (b - a).cross(c - a);
        // checked_mesh already validates degeneracy; avoid Vec3::normalized's
        // fixed length epsilon for small but valid model coordinates.
        let normal = cross / cross.length();
        Self {
            vertices: vec![a, b, c],
            plane: CutPlane {
                normal,
                distance: normal.dot(a),
            },
        }
    }
    fn flip(&mut self) {
        self.vertices.reverse();
        self.plane.normal = -self.plane.normal;
        self.plane.distance = -self.plane.distance;
    }
}

struct Partition {
    coplanar_front: Vec<Polygon>,
    coplanar_back: Vec<Polygon>,
    front: Vec<Polygon>,
    back: Vec<Polygon>,
}
impl Partition {
    fn new() -> Self {
        Self {
            coplanar_front: Vec::new(),
            coplanar_back: Vec::new(),
            front: Vec::new(),
            back: Vec::new(),
        }
    }
}

fn partition(
    plane: CutPlane,
    polygon: Polygon,
    tolerance: f64,
    parts: &mut Partition,
    budget: &mut Budget,
) -> Result<(), String> {
    budget.spend(polygon.vertices.len())?;
    let types: Vec<u8> = polygon
        .vertices
        .iter()
        .map(|p| {
            let distance = plane.normal.dot(*p) - plane.distance;
            if distance < -tolerance {
                2
            } else if distance > tolerance {
                1
            } else {
                0
            }
        })
        .collect();
    match types.iter().fold(0, |kind, next| kind | next) {
        0 => {
            if plane.normal.dot(polygon.plane.normal) > 0.0 {
                parts.coplanar_front.push(polygon);
            } else {
                parts.coplanar_back.push(polygon);
            }
        }
        1 => parts.front.push(polygon),
        2 => parts.back.push(polygon),
        _ => {
            budget.fragments = budget
                .fragments
                .checked_sub(2)
                .ok_or("mesh solid fragment budget exceeded; reduce mesh resolution")?;
            let mut front = Vec::new();
            let mut back = Vec::new();
            for i in 0..polygon.vertices.len() {
                let j = (i + 1) % polygon.vertices.len();
                let (a, b) = (polygon.vertices[i], polygon.vertices[j]);
                if types[i] != 2 {
                    front.push(a);
                }
                if types[i] != 1 {
                    back.push(a);
                }
                if types[i] | types[j] == 3 {
                    let t = (plane.distance - plane.normal.dot(a)) / plane.normal.dot(b - a);
                    let intersection = a.lerp(b, t.clamp(0.0, 1.0));
                    front.push(intersection);
                    back.push(intersection);
                }
            }
            // Keep the original polygon plane. Fragment vertices can begin
            // with collinear points and do not reliably define a new normal.
            if front.len() >= 3 {
                parts.front.push(Polygon {
                    vertices: front,
                    plane: polygon.plane,
                });
            }
            if back.len() >= 3 {
                parts.back.push(Polygon {
                    vertices: back,
                    plane: polygon.plane,
                });
            }
        }
    }
    if parts.front.len() + parts.back.len() + parts.coplanar_front.len() + parts.coplanar_back.len()
        > MAX_FRAGMENTS
    {
        return Err("mesh solid polygon limit exceeded; reduce resolution".into());
    }
    Ok(())
}

#[derive(Default)]
struct Node {
    plane: Option<CutPlane>,
    polygons: Vec<Polygon>,
    front: Option<usize>,
    back: Option<usize>,
}
struct Bsp {
    nodes: Vec<Node>,
}
impl Bsp {
    fn from_mesh(mesh: &Mesh, tolerance: f64, budget: &mut Budget) -> Result<Self, String> {
        let polygons = mesh
            .indices
            .iter()
            .map(|t| {
                Polygon::triangle(
                    mesh.positions[t[0] as usize],
                    mesh.positions[t[1] as usize],
                    mesh.positions[t[2] as usize],
                )
            })
            .collect();
        let mut bsp = Self {
            nodes: vec![Node::default()],
        };
        bsp.build(polygons, tolerance, budget)?;
        Ok(bsp)
    }
    fn build(
        &mut self,
        polygons: Vec<Polygon>,
        tolerance: f64,
        budget: &mut Budget,
    ) -> Result<(), String> {
        let mut pending = vec![(0, polygons)];
        while let Some((index, polygons)) = pending.pop() {
            if polygons.is_empty() {
                continue;
            }
            let plane = *self.nodes[index].plane.get_or_insert(polygons[0].plane);
            let mut parts = Partition::new();
            for polygon in polygons {
                partition(plane, polygon, tolerance, &mut parts, budget)?;
            }
            self.nodes[index].polygons.extend(parts.coplanar_front);
            self.nodes[index].polygons.extend(parts.coplanar_back);
            for (front, polygons) in [(true, parts.front), (false, parts.back)] {
                if polygons.is_empty() {
                    continue;
                }
                let child = if front {
                    self.nodes[index].front
                } else {
                    self.nodes[index].back
                };
                let child = if let Some(child) = child {
                    child
                } else {
                    if self.nodes.len() >= MAX_FRAGMENTS {
                        return Err("mesh solid BSP node limit exceeded".into());
                    }
                    let child = self.nodes.len();
                    self.nodes.push(Node::default());
                    if front {
                        self.nodes[index].front = Some(child);
                    } else {
                        self.nodes[index].back = Some(child);
                    }
                    child
                };
                pending.push((child, polygons));
            }
        }
        Ok(())
    }
    fn invert(&mut self) {
        for node in &mut self.nodes {
            if let Some(plane) = &mut node.plane {
                plane.normal = -plane.normal;
                plane.distance = -plane.distance;
            }
            for polygon in &mut node.polygons {
                polygon.flip();
            }
            std::mem::swap(&mut node.front, &mut node.back);
        }
    }
    fn clip(
        &self,
        polygons: Vec<Polygon>,
        tolerance: f64,
        budget: &mut Budget,
    ) -> Result<Vec<Polygon>, String> {
        let mut pending = vec![(0, polygons)];
        let mut output = Vec::new();
        while let Some((index, polygons)) = pending.pop() {
            let node = &self.nodes[index];
            let Some(plane) = node.plane else {
                output.extend(polygons);
                continue;
            };
            let mut parts = Partition::new();
            for polygon in polygons {
                partition(plane, polygon, tolerance, &mut parts, budget)?;
            }
            parts.front.extend(parts.coplanar_front);
            parts.back.extend(parts.coplanar_back);
            if let Some(front) = node.front {
                pending.push((front, parts.front));
            } else {
                output.extend(parts.front);
            }
            if let Some(back) = node.back {
                pending.push((back, parts.back));
            }
            if output.len() > MAX_FRAGMENTS {
                return Err("mesh solid polygon limit exceeded".into());
            }
        }
        Ok(output)
    }
    fn clip_to(&mut self, other: &Self, tolerance: f64, budget: &mut Budget) -> Result<(), String> {
        for node in &mut self.nodes {
            node.polygons = other.clip(std::mem::take(&mut node.polygons), tolerance, budget)?;
        }
        Ok(())
    }
    fn polygons(self) -> Result<Vec<Polygon>, String> {
        let count: usize = self.nodes.iter().map(|n| n.polygons.len()).sum();
        if count > MAX_FRAGMENTS {
            return Err("mesh solid polygon limit exceeded".into());
        }
        Ok(self
            .nodes
            .into_iter()
            .flat_map(|node| node.polygons)
            .collect())
    }
}

fn tessellate(polygons: Vec<Polygon>, tolerance: f64, budget: &mut Budget) -> Result<Mesh, String> {
    if polygons.is_empty() {
        return Ok(Mesh::new());
    }
    let mut weld = Weld::new(tolerance);
    let mut loops = Vec::new();
    for polygon in polygons {
        let mut ids = Vec::new();
        for point in polygon.vertices {
            let id = weld.add(point)?;
            if ids.last() != Some(&id) {
                ids.push(id);
            }
        }
        if ids.len() > 1 && ids.first() == ids.last() {
            ids.pop();
        }
        if ids.len() >= 3 {
            loops.push((ids, polygon.plane));
        }
    }
    let boundary_points = weld.points.len();
    let mut indices = Vec::new();
    for (vertices, plane) in loops {
        let mut boundary = Vec::new();
        for i in 0..vertices.len() {
            let a = vertices[i];
            let b = vertices[(i + 1) % vertices.len()];
            let p = weld.points[a as usize];
            let direction = weld.points[b as usize] - p;
            let length_sq = direction.length_sq();
            let mut along = vec![(0.0, a)];
            budget.spend(boundary_points)?;
            for id in 0..boundary_points {
                if id as u32 == a || id as u32 == b {
                    continue;
                }
                let q = weld.points[id];
                let t = (q - p).dot(direction) / length_sq;
                if t > 0.0 && t < 1.0 && (p + direction * t).distance(q) <= tolerance {
                    along.push((t, id as u32));
                }
            }
            along.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            boundary.extend(along.into_iter().map(|(_, id)| id));
        }
        let center = vertices
            .iter()
            .map(|id| weld.points[*id as usize])
            .fold(Vec3::ZERO, |a, b| a + b)
            / vertices.len() as f64;
        let center_id = weld.add(center)?;
        for i in 0..boundary.len() {
            let a = boundary[i];
            let b = boundary[(i + 1) % boundary.len()];
            let cross = (weld.points[a as usize] - center).cross(weld.points[b as usize] - center);
            if cross.dot(plane.normal) <= tolerance * tolerance {
                continue;
            }
            indices.push([center_id, a, b]);
            if indices.len() > MAX_FRAGMENTS * 4 {
                return Err("mesh solid output triangle limit exceeded".into());
            }
        }
    }
    if indices.is_empty() {
        return Ok(Mesh::new());
    }
    check_topology(&weld.points, &indices, tolerance)
        .map_err(|e| format!("mesh solid result is ambiguous at this tolerance: {e}"))?;
    let mut result = Mesh {
        positions: weld.points,
        indices,
        normals: Vec::new(),
    };
    check_shell_winding(&result, tolerance, budget)
        .map_err(|e| format!("mesh solid result is ambiguous at this tolerance: {e}"))?;
    result.recompute_normals();
    Ok(result)
}

/// Polygonal Boolean; empty output denotes an empty solid. Both inputs must
/// be closed and outward-oriented. No input is modified.
pub fn boolean(a: &Mesh, b: &Mesh, operation: BooleanOp, tolerance: f64) -> Result<Mesh, String> {
    tolerance_valid(tolerance)?;
    let a = checked_mesh(a, tolerance, true, MAX_SOLID_TRIANGLES)?;
    let b = checked_mesh(b, tolerance, true, MAX_SOLID_TRIANGLES)?;
    let mut budget = Budget::new();
    check_self_intersections(&a, tolerance, &mut budget)?;
    check_self_intersections(&b, tolerance, &mut budget)?;
    check_shell_winding(&a, tolerance, &mut budget)?;
    check_shell_winding(&b, tolerance, &mut budget)?;
    if a.indices.is_empty() || b.indices.is_empty() {
        return Ok(match operation {
            BooleanOp::Union => {
                if a.indices.is_empty() {
                    b
                } else {
                    a
                }
            }
            BooleanOp::Difference => a,
            BooleanOp::Intersection => Mesh::new(),
        });
    }
    let mut a = Bsp::from_mesh(&a, tolerance, &mut budget)?;
    let mut b = Bsp::from_mesh(&b, tolerance, &mut budget)?;
    match operation {
        BooleanOp::Union => {
            a.clip_to(&b, tolerance, &mut budget)?;
            b.clip_to(&a, tolerance, &mut budget)?;
            b.invert();
            b.clip_to(&a, tolerance, &mut budget)?;
            b.invert();
            a.build(b.polygons()?, tolerance, &mut budget)?;
        }
        BooleanOp::Difference => {
            a.invert();
            a.clip_to(&b, tolerance, &mut budget)?;
            b.clip_to(&a, tolerance, &mut budget)?;
            b.invert();
            b.clip_to(&a, tolerance, &mut budget)?;
            b.invert();
            a.build(b.polygons()?, tolerance, &mut budget)?;
            a.invert();
        }
        BooleanOp::Intersection => {
            a.invert();
            b.clip_to(&a, tolerance, &mut budget)?;
            b.invert();
            a.clip_to(&b, tolerance, &mut budget)?;
            b.clip_to(&a, tolerance, &mut budget)?;
            a.build(b.polygons()?, tolerance, &mut budget)?;
            a.invert();
        }
    }
    tessellate(a.polygons()?, tolerance, &mut budget)
}

/// Split a closed mesh by an infinite plane, returning (negative, positive)
/// signed-distance halves. Both nonempty results include oriented planar caps.
pub fn split_plane(mesh: &Mesh, plane: &Plane, tolerance: f64) -> Result<(Mesh, Mesh), String> {
    tolerance_valid(tolerance)?;
    let mesh = checked_mesh(mesh, tolerance, false, MAX_SOLID_TRIANGLES)?;
    let mut budget = Budget::new();
    check_self_intersections(&mesh, tolerance, &mut budget)?;
    check_shell_winding(&mesh, tolerance, &mut budget)?;
    if !plane.origin.is_finite() || !plane.x_axis.is_finite() || !plane.y_axis.is_finite() {
        return Err("cutting plane must be finite".into());
    }
    let normal = plane.x_axis.cross(plane.y_axis);
    if normal.length() < 1e-12 || !normal.is_finite() {
        return Err("cutting plane has no normal".into());
    }
    let basis = Plane::from_normal(plane.origin, normal);
    let normal = basis.normal();
    let mut minimum = Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut maximum = -minimum;
    for point in &mesh.positions {
        let relative = *point - basis.origin;
        let p = Vec3::new(
            relative.dot(basis.x_axis),
            relative.dot(basis.y_axis),
            relative.dot(normal),
        );
        minimum = Vec3::new(minimum.x.min(p.x), minimum.y.min(p.y), minimum.z.min(p.z));
        maximum = Vec3::new(maximum.x.max(p.x), maximum.y.max(p.y), maximum.z.max(p.z));
    }
    if minimum.z >= -tolerance {
        return Ok((Mesh::new(), mesh));
    }
    if maximum.z <= tolerance {
        return Ok((mesh, Mesh::new()));
    }
    // Bound an infinite negative half-space beyond all source vertices. The
    // only cutter face that intersects the source is its cap at distance 0.
    // BSP subtraction/intersection constructs caps even for cut loops with holes.
    let margin = (maximum - minimum).length().max(1.0) * 0.1 + tolerance * 16.0;
    let origin = basis.point_at_3(minimum.x - margin, minimum.y - margin, minimum.z - margin);
    let cutter_plane = Plane {
        origin,
        x_axis: basis.x_axis,
        y_axis: basis.y_axis,
    };
    let cutter = Mesh::box_mesh(
        &cutter_plane,
        maximum.x - minimum.x + 2.0 * margin,
        maximum.y - minimum.y + 2.0 * margin,
        -minimum.z + margin,
    );
    let negative = boolean(&mesh, &cutter, BooleanOp::Intersection, tolerance)?;
    let positive = boolean(&mesh, &cutter, BooleanOp::Difference, tolerance)?;
    Ok((negative, positive))
}

/// Validate a closed mesh without performing an operation.
pub fn validate_closed(mesh: &Mesh, tolerance: f64) -> Result<(), String> {
    tolerance_valid(tolerance)?;
    let mesh = checked_mesh(mesh, tolerance, false, MAX_SOLID_TRIANGLES)?;
    let mut budget = Budget::new();
    check_self_intersections(&mesh, tolerance, &mut budget)?;
    check_shell_winding(&mesh, tolerance, &mut budget)
}
