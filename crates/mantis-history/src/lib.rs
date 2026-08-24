//! Deterministic comparison of two committed MantisCAD revisions.
//!
//! The signed chain remains the authority. Geometry in this crate is derived
//! by replaying and evaluating both revisions with the current component
//! registry; it is never written back to the chain.

use mantis_chain::{Chain, ChainError};
use mantis_graph::{Edge, Evaluator, Graph, GraphOp, Node, NodeId, ParamValue, Registry, Value};
use mantis_kernel::{BBox, Curve, Mesh, Plane, Vec3};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Version of the machine-readable comparison report.
pub const HISTORY_DIFF_SCHEMA_VERSION: u32 = 1;

/// Match the viewport's protection against pathologically nested lists.
const MAX_GEOMETRY_LIST_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub enum CompareError {
    InvalidChain(ChainError),
    RevisionOutOfRange { requested: usize, head: usize },
    ReversedRange { from: usize, to: usize },
}

impl fmt::Display for CompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompareError::InvalidChain(error) => write!(f, "invalid chain: {error}"),
            CompareError::RevisionOutOfRange { requested, head } => write!(
                f,
                "revision {requested} is out of range (head revision is {head})"
            ),
            CompareError::ReversedRange { from, to } => {
                write!(f, "from revision {from} is newer than to revision {to}")
            }
        }
    }
}

impl std::error::Error for CompareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CompareError::InvalidChain(error) => Some(error),
            CompareError::RevisionOutOfRange { .. } | CompareError::ReversedRange { .. } => None,
        }
    }
}

impl From<ChainError> for CompareError {
    fn from(value: ChainError) -> Self {
        CompareError::InvalidChain(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionRef {
    pub index: usize,
    pub hash: String,
    pub timestamp_ms: u64,
    pub author: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitRecord {
    pub revision: RevisionRef,
    pub operations: Vec<GraphOp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionChange {
    pub before: (f32, f32),
    pub after: (f32, f32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParameterChange {
    pub key: String,
    pub before: Option<ParamValue>,
    pub after: Option<ParamValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeChange {
    pub id: NodeId,
    pub before_type: String,
    pub after_type: String,
    pub position: Option<PositionChange>,
    pub parameters: Vec<ParameterChange>,
}

/// Net graph-state difference between the two revisions.
///
/// `commits` in [`HistoryDiff`] separately preserves transient operations that
/// cancel out before the final revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefinitionDiff {
    pub added_nodes: Vec<Node>,
    pub removed_nodes: Vec<Node>,
    pub modified_nodes: Vec<NodeChange>,
    pub added_connections: Vec<Edge>,
    pub removed_connections: Vec<Edge>,
}

impl DefinitionDiff {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.modified_nodes.is_empty()
            && self.added_connections.is_empty()
            && self.removed_connections.is_empty()
    }

    /// True when the definition changed in a way other than canvas layout.
    pub fn has_semantic_changes(&self) -> bool {
        !self.added_nodes.is_empty()
            || !self.removed_nodes.is_empty()
            || !self.added_connections.is_empty()
            || !self.removed_connections.is_empty()
            || self
                .modified_nodes
                .iter()
                .any(|node| node.before_type != node.after_type || !node.parameters.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds3 {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Bounds3 {
    fn from_bbox(bounds: BBox) -> Option<Bounds3> {
        if bounds.is_empty() {
            return None;
        }
        let min = vec3_array(bounds.min)?;
        let max = vec3_array(bounds.max)?;
        Some(Bounds3 { min, max })
    }

    fn union(self, other: Bounds3) -> Bounds3 {
        Bounds3 {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }
}

/// Stable address of a drawable value in an evaluator result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GeometryObjectId {
    pub node_id: NodeId,
    pub output_port: usize,
    /// Indices followed through nested `Value::List` values.
    pub list_path: Vec<usize>,
}

/// Bounded explanatory metrics. Exact change detection uses the object's
/// fingerprint, not these metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeometryDetails {
    Point {
        position: Option<[f64; 3]>,
    },
    Curve {
        curve_type: String,
        closed: bool,
        length: Option<f64>,
        bounds: Option<Bounds3>,
    },
    Mesh {
        vertices: usize,
        triangles: usize,
        surface_area: Option<f64>,
        signed_volume: Option<f64>,
        bounds: Option<Bounds3>,
    },
}

impl GeometryDetails {
    fn kind_name(&self) -> &'static str {
        match self {
            GeometryDetails::Point { .. } => "point",
            GeometryDetails::Curve { .. } => "curve",
            GeometryDetails::Mesh { .. } => "mesh",
        }
    }

    fn bounds(&self) -> Option<Bounds3> {
        match self {
            GeometryDetails::Point { position } => position.map(|point| Bounds3 {
                min: point,
                max: point,
            }),
            GeometryDetails::Curve { bounds, .. } | GeometryDetails::Mesh { bounds, .. } => *bounds,
        }
    }

    fn metrics_are_finite(&self) -> bool {
        match self {
            GeometryDetails::Point { position } => position.is_some(),
            GeometryDetails::Curve { length, bounds, .. } => length.is_some() && bounds.is_some(),
            GeometryDetails::Mesh {
                vertices,
                surface_area,
                signed_volume,
                bounds,
                ..
            } => {
                surface_area.is_some()
                    && signed_volume.is_some()
                    && (*vertices == 0 || bounds.is_some())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryObject {
    pub id: GeometryObjectId,
    pub node_type: String,
    /// SHA-256 over the exact point/curve definition or mesh positions and
    /// triangle indices. Mesh normals are deliberately excluded.
    pub fingerprint: String,
    pub details: GeometryDetails,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationError {
    pub node_id: NodeId,
    pub node_type: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometrySummary {
    pub object_count: usize,
    pub point_count: usize,
    pub curve_count: usize,
    pub mesh_count: usize,
    pub mesh_vertices: usize,
    pub mesh_triangles: usize,
    pub curve_length: Option<f64>,
    pub mesh_surface_area: Option<f64>,
    pub mesh_signed_volume: Option<f64>,
    pub bounds: Option<Bounds3>,
    /// Identity-independent multiset hash of all drawable object fingerprints.
    pub scene_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionGeometry {
    pub summary: GeometrySummary,
    pub objects: Vec<GeometryObject>,
    pub evaluation_errors: Vec<EvaluationError>,
    pub truncated_list_count: usize,
    pub invalid_geometry_count: usize,
}

impl RevisionGeometry {
    pub fn is_complete(&self) -> bool {
        self.evaluation_errors.is_empty()
            && self.truncated_list_count == 0
            && self.invalid_geometry_count == 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryObjectChange {
    pub before: GeometryObject,
    pub after: GeometryObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeometryDiffStatus {
    Unchanged,
    Changed,
    /// At least one revision had evaluation errors, over-deep lists, or
    /// non-finite derived geometry, so absence of a shape change is uncertain.
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryDiff {
    /// Visible-scene result. A changed fingerprint is reported as `Changed`
    /// even if one side is incomplete; inspect `before/after.is_complete()`
    /// to determine whether the reported object sets are exhaustive.
    pub status: GeometryDiffStatus,
    pub before: RevisionGeometry,
    pub after: RevisionGeometry,
    pub added_objects: Vec<GeometryObject>,
    pub removed_objects: Vec<GeometryObject>,
    pub modified_objects: Vec<GeometryObjectChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClassification {
    NoEffect,
    LayoutOnly,
    DefinitionOnly,
    GeometryChanged,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryDiff {
    pub schema_version: u32,
    /// Version of the Mantis evaluator/kernel used to derive both snapshots.
    pub engine_version: String,
    pub from: RevisionRef,
    pub to: RevisionRef,
    /// Every signed commit in `(from, to]`, including operations that have no
    /// net effect on the final graph snapshot.
    pub commits: Vec<CommitRecord>,
    pub definition: DefinitionDiff,
    pub geometry: GeometryDiff,
    pub classification: ChangeClassification,
}

/// Validate a chain, replay two committed revisions, and compare their net
/// definition and currently-derived preview geometry.
pub fn compare_revisions(
    chain: &Chain,
    from: usize,
    to: usize,
) -> Result<HistoryDiff, CompareError> {
    chain.validate()?;
    let head = chain.len().checked_sub(1).ok_or(ChainError::Empty)?;
    if from > head {
        return Err(CompareError::RevisionOutOfRange {
            requested: from,
            head,
        });
    }
    if to > head {
        return Err(CompareError::RevisionOutOfRange {
            requested: to,
            head,
        });
    }
    if from > to {
        return Err(CompareError::ReversedRange { from, to });
    }

    let before_graph = chain.replay(Some(from))?;
    let after_graph = chain.replay(Some(to))?;
    let definition = diff_graphs(&before_graph, &after_graph);
    let geometry = diff_geometry(&before_graph, &after_graph);
    let classification = classify(&definition, &geometry);
    let commits = chain
        .blocks
        .iter()
        .enumerate()
        .skip(from.saturating_add(1))
        .take(to - from)
        .map(|(index, block)| CommitRecord {
            revision: revision_ref(block, index),
            operations: block.ops.clone(),
        })
        .collect();

    Ok(HistoryDiff {
        schema_version: HISTORY_DIFF_SCHEMA_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        from: revision_ref(&chain.blocks[from], from),
        to: revision_ref(&chain.blocks[to], to),
        commits,
        definition,
        geometry,
        classification,
    })
}

fn classify(definition: &DefinitionDiff, geometry: &GeometryDiff) -> ChangeClassification {
    match geometry.status {
        GeometryDiffStatus::Incomplete => ChangeClassification::Incomplete,
        GeometryDiffStatus::Changed => ChangeClassification::GeometryChanged,
        GeometryDiffStatus::Unchanged if definition.has_semantic_changes() => {
            ChangeClassification::DefinitionOnly
        }
        GeometryDiffStatus::Unchanged if !definition.is_empty() => ChangeClassification::LayoutOnly,
        GeometryDiffStatus::Unchanged => ChangeClassification::NoEffect,
    }
}

fn revision_ref(block: &mantis_chain::Block, index: usize) -> RevisionRef {
    RevisionRef {
        index,
        hash: block.hash.clone(),
        timestamp_ms: block.timestamp_ms,
        author: block.author.clone(),
        message: block.message.clone(),
    }
}

type EdgeKey = (NodeId, u16, NodeId, u16);

fn edge_key(edge: &Edge) -> EdgeKey {
    (edge.from.0, edge.from.1, edge.to.0, edge.to.1)
}

fn edge_from_key(key: EdgeKey) -> Edge {
    Edge {
        from: (key.0, key.1),
        to: (key.2, key.3),
    }
}

fn diff_graphs(before: &Graph, after: &Graph) -> DefinitionDiff {
    let added_nodes = after
        .nodes
        .iter()
        .filter(|(id, _)| !before.nodes.contains_key(id))
        .map(|(_, node)| node.clone())
        .collect();
    let removed_nodes = before
        .nodes
        .iter()
        .filter(|(id, _)| !after.nodes.contains_key(id))
        .map(|(_, node)| node.clone())
        .collect();

    let mut modified_nodes = Vec::new();
    for (id, before_node) in &before.nodes {
        let Some(after_node) = after.nodes.get(id) else {
            continue;
        };
        let position = (before_node.pos != after_node.pos).then_some(PositionChange {
            before: before_node.pos,
            after: after_node.pos,
        });
        let keys: BTreeSet<&String> = before_node
            .params
            .keys()
            .chain(after_node.params.keys())
            .collect();
        let parameters = keys
            .into_iter()
            .filter_map(|key| {
                let before_value = before_node.params.get(key);
                let after_value = after_node.params.get(key);
                (before_value != after_value).then(|| ParameterChange {
                    key: key.clone(),
                    before: before_value.cloned(),
                    after: after_value.cloned(),
                })
            })
            .collect::<Vec<_>>();
        if before_node.type_name != after_node.type_name
            || position.is_some()
            || !parameters.is_empty()
        {
            modified_nodes.push(NodeChange {
                id: *id,
                before_type: before_node.type_name.clone(),
                after_type: after_node.type_name.clone(),
                position,
                parameters,
            });
        }
    }

    let before_edges: BTreeSet<EdgeKey> = before.edges.iter().map(edge_key).collect();
    let after_edges: BTreeSet<EdgeKey> = after.edges.iter().map(edge_key).collect();
    let added_connections = after_edges
        .difference(&before_edges)
        .copied()
        .map(edge_from_key)
        .collect();
    let removed_connections = before_edges
        .difference(&after_edges)
        .copied()
        .map(edge_from_key)
        .collect();

    DefinitionDiff {
        added_nodes,
        removed_nodes,
        modified_nodes,
        added_connections,
        removed_connections,
    }
}

fn diff_geometry(before: &Graph, after: &Graph) -> GeometryDiff {
    let before = evaluate_geometry(before);
    let after = evaluate_geometry(after);
    let before_by_id: BTreeMap<_, _> = before
        .objects
        .iter()
        .cloned()
        .map(|object| (object.id.clone(), object))
        .collect();
    let after_by_id: BTreeMap<_, _> = after
        .objects
        .iter()
        .cloned()
        .map(|object| (object.id.clone(), object))
        .collect();

    let added_objects = after_by_id
        .iter()
        .filter(|(id, _)| !before_by_id.contains_key(id))
        .map(|(_, object)| object.clone())
        .collect();
    let removed_objects = before_by_id
        .iter()
        .filter(|(id, _)| !after_by_id.contains_key(id))
        .map(|(_, object)| object.clone())
        .collect();
    let modified_objects = before_by_id
        .iter()
        .filter_map(|(id, before_object)| {
            let after_object = after_by_id.get(id)?;
            (before_object.fingerprint != after_object.fingerprint
                || before_object.details.kind_name() != after_object.details.kind_name())
            .then(|| GeometryObjectChange {
                before: before_object.clone(),
                after: after_object.clone(),
            })
        })
        .collect();

    let status = if before.summary.scene_fingerprint != after.summary.scene_fingerprint {
        GeometryDiffStatus::Changed
    } else if !before.is_complete() || !after.is_complete() {
        GeometryDiffStatus::Incomplete
    } else {
        GeometryDiffStatus::Unchanged
    };

    GeometryDiff {
        status,
        before,
        after,
        added_objects,
        removed_objects,
        modified_objects,
    }
}

fn evaluate_geometry(graph: &Graph) -> RevisionGeometry {
    let registry = Registry::standard();
    let output = Evaluator::new().evaluate(graph, &registry);
    let evaluation_errors = output
        .errors
        .iter()
        // Geometry comparison follows the viewport: hidden nodes do not make
        // the visible scene incomplete. A hidden upstream failure still
        // propagates to any preview-enabled downstream node and is retained.
        .filter(|(id, _)| graph.nodes.get(id).is_some_and(Node::preview))
        .map(|(id, message)| EvaluationError {
            node_id: *id,
            node_type: graph
                .nodes
                .get(id)
                .map(|node| node.type_name.clone())
                .unwrap_or_default(),
            message: message.clone(),
        })
        .collect();
    let mut objects = Vec::new();
    let mut truncated_list_count = 0;
    for (id, node) in &graph.nodes {
        if !node.preview() {
            continue;
        }
        let Some(values) = output.outputs.get(id) else {
            continue;
        };
        for (output_port, value) in values.iter().enumerate() {
            collect_geometry_value(
                *id,
                &node.type_name,
                output_port,
                value,
                &mut Vec::new(),
                0,
                &mut objects,
                &mut truncated_list_count,
            );
        }
    }
    objects.sort_by(|left, right| left.id.cmp(&right.id));
    let invalid_geometry_count = objects
        .iter()
        .filter(|object| !object.details.metrics_are_finite())
        .count();
    let summary = summarize_geometry(&objects);
    RevisionGeometry {
        summary,
        objects,
        evaluation_errors,
        truncated_list_count,
        invalid_geometry_count,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_geometry_value(
    node_id: NodeId,
    node_type: &str,
    output_port: usize,
    value: &Value,
    path: &mut Vec<usize>,
    depth: usize,
    objects: &mut Vec<GeometryObject>,
    truncated_list_count: &mut usize,
) {
    let id = || GeometryObjectId {
        node_id,
        output_port,
        list_path: path.clone(),
    };
    let object = match value {
        Value::Vector(point) => Some(GeometryObject {
            id: id(),
            node_type: node_type.to_string(),
            fingerprint: point_fingerprint(*point),
            details: GeometryDetails::Point {
                position: vec3_array(*point),
            },
        }),
        Value::Curve(curve) => {
            let length = finite(curve.length());
            let bounds = Bounds3::from_bbox(curve.bbox());
            Some(GeometryObject {
                id: id(),
                node_type: node_type.to_string(),
                fingerprint: curve_fingerprint(curve),
                details: GeometryDetails::Curve {
                    curve_type: curve_kind(curve).to_string(),
                    closed: curve.is_closed(),
                    length,
                    bounds,
                },
            })
        }
        Value::Mesh(mesh) => Some(GeometryObject {
            id: id(),
            node_type: node_type.to_string(),
            fingerprint: mesh_fingerprint(mesh),
            details: GeometryDetails::Mesh {
                vertices: mesh.vertex_count(),
                triangles: mesh.triangle_count(),
                surface_area: finite(mesh.area()),
                signed_volume: finite(mesh.volume()),
                bounds: Bounds3::from_bbox(mesh.bbox()),
            },
        }),
        Value::List(values) if depth < MAX_GEOMETRY_LIST_DEPTH => {
            for (index, nested) in values.iter().enumerate() {
                path.push(index);
                collect_geometry_value(
                    node_id,
                    node_type,
                    output_port,
                    nested,
                    path,
                    depth + 1,
                    objects,
                    truncated_list_count,
                );
                path.pop();
            }
            None
        }
        Value::List(_) => {
            *truncated_list_count += 1;
            None
        }
        Value::Null | Value::Number(_) | Value::Bool(_) | Value::Text(_) | Value::Plane(_) => None,
    };
    if let Some(object) = object {
        objects.push(object);
    }
}

fn summarize_geometry(objects: &[GeometryObject]) -> GeometrySummary {
    let mut point_count = 0;
    let mut curve_count = 0;
    let mut mesh_count = 0;
    let mut mesh_vertices = 0usize;
    let mut mesh_triangles = 0usize;
    let mut curve_length = Some(0.0);
    let mut mesh_surface_area = Some(0.0);
    let mut mesh_signed_volume = Some(0.0);
    let mut bounds: Option<Bounds3> = None;
    let mut fingerprints = Vec::with_capacity(objects.len());

    for object in objects {
        fingerprints.push(format!(
            "{}:{}",
            object.details.kind_name(),
            object.fingerprint
        ));
        if let Some(object_bounds) = object.details.bounds() {
            bounds = Some(match bounds {
                Some(current) => current.union(object_bounds),
                None => object_bounds,
            });
        }
        match &object.details {
            GeometryDetails::Point { .. } => point_count += 1,
            GeometryDetails::Curve { length, .. } => {
                curve_count += 1;
                accumulate_optional(&mut curve_length, *length);
            }
            GeometryDetails::Mesh {
                vertices,
                triangles,
                surface_area,
                signed_volume,
                ..
            } => {
                mesh_count += 1;
                mesh_vertices = mesh_vertices.saturating_add(*vertices);
                mesh_triangles = mesh_triangles.saturating_add(*triangles);
                accumulate_optional(&mut mesh_surface_area, *surface_area);
                accumulate_optional(&mut mesh_signed_volume, *signed_volume);
            }
        }
    }
    fingerprints.sort();
    let scene_fingerprint = (!fingerprints.is_empty()).then(|| {
        let mut hasher = Sha256::new();
        hasher.update(b"mantis-scene-v1");
        for fingerprint in fingerprints {
            hash_len(&mut hasher, fingerprint.len());
            hasher.update(fingerprint.as_bytes());
        }
        finish_hex(hasher)
    });

    GeometrySummary {
        object_count: objects.len(),
        point_count,
        curve_count,
        mesh_count,
        mesh_vertices,
        mesh_triangles,
        curve_length,
        mesh_surface_area,
        mesh_signed_volume,
        bounds,
        scene_fingerprint,
    }
}

fn accumulate_optional(total: &mut Option<f64>, value: Option<f64>) {
    *total = match (*total, value) {
        (Some(total), Some(value)) => finite(total + value),
        _ => None,
    };
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn vec3_array(vector: Vec3) -> Option<[f64; 3]> {
    (vector.x.is_finite() && vector.y.is_finite() && vector.z.is_finite())
        .then_some([vector.x, vector.y, vector.z])
}

fn curve_kind(curve: &Curve) -> &'static str {
    match curve {
        Curve::Line { .. } => "line",
        Curve::Polyline { .. } => "polyline",
        Curve::Circle { .. } => "circle",
        Curve::Arc { .. } => "arc",
        Curve::Nurbs(_) => "nurbs",
    }
}

fn point_fingerprint(point: Vec3) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mantis-point-v1");
    hash_vec3(&mut hasher, point);
    finish_hex(hasher)
}

fn curve_fingerprint(curve: &Curve) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mantis-curve-v1");
    match curve {
        Curve::Line { a, b } => {
            hasher.update([0]);
            hash_vec3(&mut hasher, *a);
            hash_vec3(&mut hasher, *b);
        }
        Curve::Polyline { points, closed } => {
            hasher.update([1, u8::from(*closed)]);
            hash_len(&mut hasher, points.len());
            for point in points {
                hash_vec3(&mut hasher, *point);
            }
        }
        Curve::Circle { plane, radius } => {
            hasher.update([2]);
            hash_plane(&mut hasher, *plane);
            hash_f64(&mut hasher, *radius);
        }
        Curve::Arc {
            plane,
            radius,
            start_angle,
            end_angle,
        } => {
            hasher.update([3]);
            hash_plane(&mut hasher, *plane);
            hash_f64(&mut hasher, *radius);
            hash_f64(&mut hasher, *start_angle);
            hash_f64(&mut hasher, *end_angle);
        }
        Curve::Nurbs(curve) => {
            hasher.update([4]);
            hash_len(&mut hasher, curve.degree);
            hash_len(&mut hasher, curve.control_points.len());
            for point in &curve.control_points {
                hash_vec3(&mut hasher, *point);
            }
            hash_len(&mut hasher, curve.weights.len());
            for weight in &curve.weights {
                hash_f64(&mut hasher, *weight);
            }
            hash_len(&mut hasher, curve.knots.len());
            for knot in &curve.knots {
                hash_f64(&mut hasher, *knot);
            }
        }
    }
    finish_hex(hasher)
}

fn mesh_fingerprint(mesh: &Mesh) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mantis-mesh-v1");
    hash_len(&mut hasher, mesh.positions.len());
    for position in &mesh.positions {
        hash_vec3(&mut hasher, *position);
    }
    hash_len(&mut hasher, mesh.indices.len());
    for triangle in &mesh.indices {
        for index in triangle {
            hasher.update(index.to_le_bytes());
        }
    }
    finish_hex(hasher)
}

fn hash_plane(hasher: &mut Sha256, plane: Plane) {
    hash_vec3(hasher, plane.origin);
    hash_vec3(hasher, plane.x_axis);
    hash_vec3(hasher, plane.y_axis);
}

fn hash_vec3(hasher: &mut Sha256, vector: Vec3) {
    hash_f64(hasher, vector.x);
    hash_f64(hasher, vector.y);
    hash_f64(hasher, vector.z);
}

fn hash_f64(hasher: &mut Sha256, value: f64) {
    // Treat -0 and +0 as the same geometric coordinate.
    let value = if value == 0.0 { 0.0 } else { value };
    hasher.update(value.to_bits().to_le_bytes());
}

fn hash_len(hasher: &mut Sha256, value: usize) {
    hasher.update((value as u64).to_le_bytes());
}

fn finish_hex(hasher: Sha256) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = hasher.finalize();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;

    fn node(value: u128) -> NodeId {
        NodeId(value)
    }

    fn identity() -> Identity {
        Identity::from_secret_hex(
            "history-test",
            "0101010101010101010101010101010101010101010101010101010101010101",
        )
        .expect("fixed test identity is valid")
    }

    fn assert_approx(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("metric should be finite");
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    fn sample_chain() -> Chain {
        let mut chain = Chain::new();
        let author = identity();
        chain
            .append(
                vec![
                    GraphOp::AddNode {
                        id: node(1),
                        type_name: "number_slider".into(),
                        pos: (10.0, 20.0),
                    },
                    GraphOp::SetParam {
                        id: node(1),
                        key: "value".into(),
                        value: ParamValue::Number(1.0),
                    },
                    GraphOp::AddNode {
                        id: node(2),
                        type_name: "box_mesh".into(),
                        pos: (30.0, 20.0),
                    },
                    GraphOp::Connect {
                        from: (node(1), 0),
                        to: (node(2), 1),
                    },
                ],
                "unit box",
                &author,
                1,
            )
            .unwrap();
        chain
            .append(
                vec![GraphOp::SetParam {
                    id: node(1),
                    key: "value".into(),
                    value: ParamValue::Number(2.0),
                }],
                "double width",
                &author,
                2,
            )
            .unwrap();
        chain
            .append(
                vec![GraphOp::MoveNode {
                    id: node(2),
                    pos: (80.0, 40.0),
                }],
                "layout only",
                &author,
                3,
            )
            .unwrap();
        chain
    }

    #[test]
    fn graph_and_mesh_change_are_reported_at_stable_output_identity() {
        let report = compare_revisions(&sample_chain(), 1, 2).unwrap();
        assert_eq!(report.schema_version, HISTORY_DIFF_SCHEMA_VERSION);
        assert_eq!(report.classification, ChangeClassification::GeometryChanged);
        assert_eq!(report.commits.len(), 1);
        assert_eq!(report.commits[0].operations.len(), 1);
        assert!(report.definition.has_semantic_changes());
        assert_eq!(report.definition.modified_nodes.len(), 1);
        assert_eq!(report.definition.modified_nodes[0].id, node(1));
        assert_eq!(report.definition.modified_nodes[0].parameters.len(), 1);

        assert_eq!(report.geometry.status, GeometryDiffStatus::Changed);
        assert!(report.geometry.before.is_complete());
        assert!(report.geometry.after.is_complete());
        assert_eq!(report.geometry.modified_objects.len(), 1);
        assert_eq!(
            report.geometry.modified_objects[0].before.id,
            GeometryObjectId {
                node_id: node(2),
                output_port: 0,
                list_path: vec![],
            }
        );
        assert_eq!(report.geometry.before.summary.mesh_vertices, 24);
        assert_eq!(report.geometry.after.summary.mesh_vertices, 24);
        assert_approx(report.geometry.before.summary.mesh_surface_area, 6.0);
        assert_approx(report.geometry.after.summary.mesh_surface_area, 10.0);
        assert_approx(report.geometry.before.summary.mesh_signed_volume, 1.0);
        assert_approx(report.geometry.after.summary.mesh_signed_volume, 2.0);
    }

    #[test]
    fn layout_only_change_does_not_claim_geometry_changed() {
        let report = compare_revisions(&sample_chain(), 2, 3).unwrap();
        assert!(!report.definition.is_empty());
        assert!(!report.definition.has_semantic_changes());
        assert!(report.definition.modified_nodes[0].position.is_some());
        assert_eq!(report.geometry.status, GeometryDiffStatus::Unchanged);
        assert_eq!(report.classification, ChangeClassification::LayoutOnly);
        assert!(report.geometry.added_objects.is_empty());
        assert!(report.geometry.removed_objects.is_empty());
        assert!(report.geometry.modified_objects.is_empty());
    }

    #[test]
    fn genesis_to_first_commit_includes_net_graph_and_raw_history() {
        let report = compare_revisions(&sample_chain(), 0, 1).unwrap();
        assert_eq!(report.definition.added_nodes.len(), 2);
        assert_eq!(report.definition.added_connections.len(), 1);
        assert_eq!(report.commits.len(), 1);
        assert_eq!(report.commits[0].operations.len(), 4);
        assert_eq!(report.geometry.added_objects.len(), 1);
        assert_eq!(report.geometry.status, GeometryDiffStatus::Changed);
    }

    #[test]
    fn evaluation_failure_makes_geometry_status_incomplete() {
        let mut chain = sample_chain();
        chain
            .append(
                vec![GraphOp::AddNode {
                    id: node(9),
                    type_name: "future_component".into(),
                    pos: (0.0, 0.0),
                }],
                "unknown component",
                &identity(),
                4,
            )
            .unwrap();
        let report = compare_revisions(&chain, 3, 4).unwrap();
        assert_eq!(report.geometry.status, GeometryDiffStatus::Incomplete);
        assert!(report.geometry.before.evaluation_errors.is_empty());
        assert_eq!(report.geometry.after.evaluation_errors.len(), 1);
        assert_eq!(report.geometry.after.evaluation_errors[0].node_id, node(9));
    }

    #[test]
    fn definite_scene_change_wins_over_partial_evaluation() {
        let mut chain = sample_chain();
        chain
            .append(
                vec![
                    GraphOp::SetParam {
                        id: node(1),
                        key: "value".into(),
                        value: ParamValue::Number(3.0),
                    },
                    GraphOp::AddNode {
                        id: node(9),
                        type_name: "future_component".into(),
                        pos: (0.0, 0.0),
                    },
                ],
                "change with partial evaluation",
                &identity(),
                4,
            )
            .unwrap();
        let report = compare_revisions(&chain, 3, 4).unwrap();
        assert_eq!(report.geometry.status, GeometryDiffStatus::Changed);
        assert_eq!(report.classification, ChangeClassification::GeometryChanged);
        assert!(!report.geometry.after.is_complete());
        assert_eq!(report.geometry.modified_objects.len(), 1);
    }

    #[test]
    fn hidden_node_error_does_not_make_visible_scene_incomplete() {
        let mut chain = sample_chain();
        chain
            .append(
                vec![
                    GraphOp::AddNode {
                        id: node(9),
                        type_name: "future_component".into(),
                        pos: (0.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: node(9),
                        key: "__preview".into(),
                        value: ParamValue::Bool(false),
                    },
                ],
                "hidden unknown component",
                &identity(),
                4,
            )
            .unwrap();
        let report = compare_revisions(&chain, 3, 4).unwrap();
        assert_eq!(report.geometry.status, GeometryDiffStatus::Unchanged);
        assert!(report.geometry.after.is_complete());
        assert!(report.geometry.after.evaluation_errors.is_empty());
    }

    #[test]
    fn invalid_revision_ranges_are_rejected_instead_of_clamped() {
        let chain = sample_chain();
        assert_eq!(
            compare_revisions(&chain, 0, 99),
            Err(CompareError::RevisionOutOfRange {
                requested: 99,
                head: 3,
            })
        );
        assert_eq!(
            compare_revisions(&chain, 3, 2),
            Err(CompareError::ReversedRange { from: 3, to: 2 })
        );
    }

    #[test]
    fn same_revision_has_no_net_effect_and_no_commits() {
        let report = compare_revisions(&sample_chain(), 2, 2).unwrap();
        assert!(report.commits.is_empty());
        assert!(report.definition.is_empty());
        assert_eq!(report.geometry.status, GeometryDiffStatus::Unchanged);
        assert_eq!(report.classification, ChangeClassification::NoEffect);
    }

    #[test]
    fn report_is_stable_machine_readable_json() {
        let report = compare_revisions(&sample_chain(), 1, 2).unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["engine_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["classification"], "geometry_changed");
        assert_eq!(value["geometry"]["status"], "changed");
        assert_eq!(
            value["geometry"]["modified_objects"][0]["before"]["details"]["kind"],
            "mesh"
        );
    }
}
