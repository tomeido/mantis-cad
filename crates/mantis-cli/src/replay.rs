//! `replay` subcommand: rebuild the graph from the op-log, evaluate it with
//! the standard component registry, and collect the preview meshes.

use mantis_chain::Chain;
use mantis_graph::{Evaluator, Registry, Value};
use mantis_kernel::Mesh;

/// Result of replaying + evaluating a chain.
pub struct ReplayReport {
    /// One line per node, in deterministic topological order.
    pub node_lines: Vec<String>,
    /// All preview meshes merged into one.
    pub mesh: Mesh,
    /// How many individual meshes were collected.
    pub mesh_count: usize,
    /// Number of nodes that errored during evaluation.
    pub error_count: usize,
}

/// Recursively collect every mesh inside a value (lists may nest).
fn collect_meshes(v: &Value, merged: &mut Mesh, count: &mut usize) {
    match v {
        Value::Mesh(m) => {
            merged.append(m);
            *count += 1;
        }
        Value::List(items) => {
            for item in items {
                collect_meshes(item, merged, count);
            }
        }
        _ => {}
    }
}

/// One-line summary of a node's output ports.
fn outputs_summary(vals: &[Value]) -> String {
    if vals.is_empty() {
        return "(no outputs)".to_string();
    }
    vals.iter()
        .map(|v| v.describe())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Replay blocks 0..=upto (None = all), evaluate, and gather preview meshes.
pub fn replay_report(chain: &Chain, upto: Option<usize>) -> Result<ReplayReport, String> {
    let graph = chain
        .replay(upto)
        .map_err(|e| format!("replay failed: {e}"))?;
    let registry = Registry::standard();
    let mut evaluator = Evaluator::new();
    let out = evaluator.evaluate(&graph, &registry);

    let mut node_lines = Vec::with_capacity(graph.nodes.len());
    for id in graph.topo_order() {
        let Some(node) = graph.nodes.get(&id) else { continue };
        let line = match (out.errors.get(&id), out.outputs.get(&id)) {
            (Some(err), _) => format!("{id}  {:<18} ERROR: {err}", node.type_name),
            (None, Some(vals)) => {
                format!("{id}  {:<18} {}", node.type_name, outputs_summary(vals))
            }
            (None, None) => format!("{id}  {:<18} (not evaluated)", node.type_name),
        };
        node_lines.push(line);
    }

    let mut mesh = Mesh::new();
    let mut mesh_count = 0usize;
    for (id, vals) in &out.outputs {
        let preview = graph.nodes.get(id).map(|n| n.preview()).unwrap_or(false);
        if !preview {
            continue;
        }
        for v in vals {
            collect_meshes(v, &mut mesh, &mut mesh_count);
        }
    }
    mesh.recompute_normals();

    Ok(ReplayReport {
        node_lines,
        mesh,
        mesh_count,
        error_count: out.errors.len(),
    })
}
