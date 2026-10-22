//! Machine-oriented commands for CAD agents.
//!
//! The GUI and agents share exactly the same mutation primitive (`GraphOp`).
//! This module adds discovery, validation, signed atomic commits, and compact
//! materialized-result inspection without introducing a second document API.

use crate::{
    document::{working_state_hash, DocumentFile},
    load_chain, now_ms, CliError,
};
use mantis_chain::{Chain, Identity};
use mantis_graph::{
    Access, Evaluator, Graph, GraphOp, NodeId, ParamValue, Registry, Value, ValueKind,
};
use mantis_kernel::{BBox, Curve, Plane, Vec3};
use serde_json::{json, Value as JsonValue};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const LIST_REPORT_LIMIT: usize = 64;
const TEXT_REPORT_LIMIT: usize = 1024;
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Shared JSON helpers
// ---------------------------------------------------------------------------

fn json_number(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

fn vec3_json(v: Vec3) -> JsonValue {
    JsonValue::Array(vec![json_number(v.x), json_number(v.y), json_number(v.z)])
}

fn plane_json(p: Plane) -> JsonValue {
    json!({
        "origin": vec3_json(p.origin),
        "x_axis": vec3_json(p.x_axis),
        "y_axis": vec3_json(p.y_axis),
        "normal": vec3_json(p.normal()),
    })
}

fn bbox_json(b: BBox) -> JsonValue {
    if b.is_empty() {
        JsonValue::Null
    } else {
        json!({ "min": vec3_json(b.min), "max": vec3_json(b.max) })
    }
}

fn curve_kind(curve: &Curve) -> &'static str {
    match curve {
        Curve::Line { .. } => "Line",
        Curve::Polyline { .. } => "Polyline",
        Curve::Circle { .. } => "Circle",
        Curve::Arc { .. } => "Arc",
        Curve::Nurbs(_) => "Nurbs",
    }
}

fn text_summary(text: &str) -> JsonValue {
    let mut end = text.len().min(TEXT_REPORT_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    json!({
        "kind": "Text",
        "bytes": text.len(),
        "value": &text[..end],
        "truncated": text.len() > end,
    })
}

/// Compact, bounded JSON for a runtime value. Geometry is summarized instead
/// of serializing derived vertices, keeping agent responses proportional to
/// the graph rather than the tessellation size.
fn runtime_value_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => json!({ "kind": "Null" }),
        Value::Number(n) => json!({ "kind": "Number", "value": json_number(*n) }),
        Value::Bool(b) => json!({ "kind": "Bool", "value": b }),
        Value::Text(text) if text.len() > TEXT_REPORT_LIMIT => text_summary(text),
        Value::Text(text) => json!({ "kind": "Text", "value": text }),
        Value::Vector(v) => json!({ "kind": "Vector", "value": vec3_json(*v) }),
        Value::Plane(p) => json!({ "kind": "Plane", "value": plane_json(*p) }),
        Value::Curve(curve) => json!({
            "kind": "Curve",
            "curve_type": curve_kind(curve),
            "closed": curve.is_closed(),
            "length": json_number(curve.length()),
            "bbox": bbox_json(curve.bbox()),
        }),
        Value::Mesh(mesh) => json!({
            "kind": "Mesh",
            "vertices": mesh.vertex_count(),
            "triangles": mesh.triangle_count(),
            "area": json_number(mesh.area()),
            "volume": json_number(mesh.volume()),
            "bbox": bbox_json(mesh.bbox()),
        }),
        Value::List(items) => {
            let reported: Vec<JsonValue> = items
                .iter()
                .take(LIST_REPORT_LIMIT)
                .map(runtime_value_json)
                .collect();
            json!({
                "kind": "List",
                "length": items.len(),
                "items": reported,
                "truncated": items.len() > LIST_REPORT_LIMIT,
            })
        }
    }
}

fn default_value_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Number(n) => json_number(*n),
        Value::Bool(b) => json!(b),
        Value::Text(text) => json!(text),
        Value::Vector(v) => vec3_json(*v),
        Value::Plane(p) => plane_json(*p),
        // Built-in port defaults are lightweight values. Keep an explicit
        // fallback should a plug-in ever expose geometry as a default.
        Value::Curve(_) | Value::Mesh(_) | Value::List(_) => runtime_value_json(value),
    }
}

fn kind_name(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Any => "Any",
        ValueKind::Number => "Number",
        ValueKind::Bool => "Bool",
        ValueKind::Text => "Text",
        ValueKind::Vector => "Vector",
        ValueKind::Plane => "Plane",
        ValueKind::Curve => "Curve",
        ValueKind::Mesh => "Mesh",
    }
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Item => "Item",
        Access::List => "List",
    }
}

fn param_json(parameter: &mantis_graph::ParamSpec) -> JsonValue {
    let default = parameter.default.as_ref().map(|value| match value {
        ParamValue::Number(number) => json_number(*number),
        ParamValue::Bool(value) => json!(value),
        ParamValue::Text(value) => json!(value),
    });
    json!({
        "name": parameter.name,
        "kind": parameter.kind.name(),
        "default": default,
        "description": parameter.description,
    })
}

fn port_schema(index: usize, port: &mantis_graph::PortSpec, is_input: bool) -> JsonValue {
    json!({
        "index": index,
        "name": port.name,
        "kind": kind_name(port.ty),
        "access": access_name(port.access),
        "required": is_input && port.default.is_none(),
        "default": port.default.as_ref().map(default_value_json),
    })
}

fn catalog_json() -> JsonValue {
    let registry = Registry::standard();
    let components: Vec<JsonValue> = registry
        .iter()
        .map(|component| {
            let inputs: Vec<JsonValue> = component
                .inputs()
                .iter()
                .enumerate()
                .map(|(index, port)| port_schema(index, port, true))
                .collect();
            let outputs: Vec<JsonValue> = component
                .outputs()
                .iter()
                .enumerate()
                .map(|(index, port)| port_schema(index, port, false))
                .collect();
            json!({
                "type_name": component.type_name(),
                "label": component.label(),
                "category": component.category(),
                "inputs": inputs,
                "outputs": outputs,
                "parameters": component.parameters().iter().map(param_json).collect::<Vec<_>>(),
            })
        })
        .collect();

    json!({
        "protocol": "mantis-agent",
        "version": 1,
        "node_id": "32 lowercase hex characters",
        "angle_unit": "radians",
        "graph_op_examples": [
            {"op":"AddNode","id":"00000000000000000000000000000001","type_name":"sphere","pos":[0.0,0.0]},
            {"op":"SetParam","id":"00000000000000000000000000000001","key":"__preview","value":{"Bool":true}},
            {"op":"Connect","from":["00000000000000000000000000000001",0],"to":["00000000000000000000000000000002",0]},
            {"op":"Disconnect","from":["00000000000000000000000000000001",0],"to":["00000000000000000000000000000002",0]},
            {"op":"MoveNode","id":"00000000000000000000000000000001","pos":[120.0,80.0]},
            {"op":"RemoveNode","id":"00000000000000000000000000000001"}
        ],
        "components": components,
    })
}

// ---------------------------------------------------------------------------
// init / catalog
// ---------------------------------------------------------------------------

/// Write via a sibling temporary file then rename, so a killed agent cannot
/// leave a half-written chain at the destination.
pub(crate) fn write_chain_atomic(path: &Path, chain: &Chain) -> Result<(), CliError> {
    write_json_atomic(path, format!("{}\n", chain.to_json()).as_bytes())
}

pub(crate) fn write_json_atomic(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    // Locks resolve existing aliases to the target. Replace the same target
    // so saving through a symbolic link preserves the link and its contents.
    let resolved = if path.exists() || path.is_symlink() {
        path.canonicalize().map_err(|error| {
            CliError::runtime(format!("cannot resolve output {}: {error}", path.display()))
        })?
    } else {
        path.to_path_buf()
    };
    let path = resolved.as_path();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CliError::runtime("output path has no valid file name"))?;
    let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{file_name}.mantis-tmp-{}-{sequence}",
        std::process::id()
    ));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(CliError::runtime(format!(
            "cannot durably write temporary {}: {error}",
            temp.display()
        )));
    }
    if let Err(error) = replace_atomic(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(CliError::runtime(format!(
            "cannot atomically replace {}: {error}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                CliError::runtime(format!(
                    "cannot sync directory {} after replacing {}: {error}",
                    parent.display(),
                    path.display()
                ))
            })?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_atomic(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_atomic(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both pointers reference live, NUL-terminated UTF-16 buffers and
    // the flags are valid for MoveFileExW.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn cmd_init(args: &[String]) -> Result<String, CliError> {
    let [path] = args else {
        return Err(CliError::usage("usage: mantis-cli init FILE"));
    };
    let path = Path::new(path);
    let _locks = crate::local_io::DocumentLocks::acquire(&[path])?;
    if path.exists() {
        return Err(CliError::runtime(format!(
            "refusing to overwrite existing {}",
            path.display()
        )));
    }
    let chain = Chain::new();
    write_chain_atomic(path, &chain)?;
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "path": path,
            "blocks": chain.len(),
            "head": chain.head().hash,
        }))
        .unwrap_or_default()
    ))
}

pub(crate) fn cmd_catalog(args: &[String]) -> Result<String, CliError> {
    let as_json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(CliError::usage("usage: mantis-cli catalog [--json]")),
    };
    let catalog = catalog_json();
    if as_json {
        return Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&catalog).unwrap_or_default()
        ));
    }

    let mut text = String::from("category     type_name             inputs outputs  label\n");
    if let Some(components) = catalog["components"].as_array() {
        for component in components {
            text.push_str(&format!(
                "{:<12} {:<21} {:>6} {:>7}  {}\n",
                component["category"].as_str().unwrap_or("?"),
                component["type_name"].as_str().unwrap_or("?"),
                component["inputs"].as_array().map_or(0, Vec::len),
                component["outputs"].as_array().map_or(0, Vec::len),
                component["label"].as_str().unwrap_or("?"),
            ));
        }
    }
    Ok(text)
}

/// Validated provenance plus a compact head commitment that can be published
/// to an external/public ledger without publishing derived geometry.
pub(crate) fn cmd_audit(args: &[String]) -> Result<String, CliError> {
    let [path] = args else {
        return Err(CliError::usage("usage: mantis-cli audit FILE"));
    };
    let chain = load_chain(path)?;
    let audit = chain
        .audit()
        .map_err(|error| CliError::runtime(format!("[{}] {error}", error.code())))?;
    let json = serde_json::to_string_pretty(&audit)
        .map_err(|error| CliError::runtime(format!("cannot serialize audit: {error}")))?;
    Ok(format!("{json}\n"))
}

// ---------------------------------------------------------------------------
// Validation shared by apply
// ---------------------------------------------------------------------------

fn kinds_compatible(from: ValueKind, to: ValueKind) -> bool {
    from == ValueKind::Any
        || to == ValueKind::Any
        || from == to
        || (from == ValueKind::Vector && to == ValueKind::Plane)
        || (from == ValueKind::Number && to == ValueKind::Bool)
}

/// Stricter than the chain's forward-compatible structural replay: an agent
/// commit may not contain invisible wires, misspelled parameters, or known
/// incompatible port kinds.
fn validate_agent_graph(graph: &Graph, registry: &Registry) -> Result<(), String> {
    for node in graph.nodes.values() {
        if registry.get(&node.type_name).is_none() {
            return Err(format!(
                "node {} uses unknown component {}; run `mantis-cli catalog --json`",
                node.id.to_hex(),
                node.type_name
            ));
        }
        registry
            .validate_parameters(&node.type_name, &node.params)
            .map_err(|error| format!("node {} ({}): {error}", node.id.to_hex(), node.type_name))?;
    }

    for edge in &graph.edges {
        let source = graph
            .nodes
            .get(&edge.from.0)
            .ok_or_else(|| format!("wire source {} does not exist", edge.from.0.to_hex()))?;
        let target = graph
            .nodes
            .get(&edge.to.0)
            .ok_or_else(|| format!("wire target {} does not exist", edge.to.0.to_hex()))?;
        let source_component = registry
            .get(&source.type_name)
            .ok_or_else(|| format!("unknown source component {}", source.type_name))?;
        let target_component = registry
            .get(&target.type_name)
            .ok_or_else(|| format!("unknown target component {}", target.type_name))?;
        let outputs = source_component.outputs();
        let inputs = target_component.inputs();
        let output = outputs.get(edge.from.1 as usize).ok_or_else(|| {
            format!(
                "wire {}:{} -> {}:{}: source port is out of range ({} outputs)",
                edge.from.0.to_hex(),
                edge.from.1,
                edge.to.0.to_hex(),
                edge.to.1,
                outputs.len()
            )
        })?;
        let input = inputs.get(edge.to.1 as usize).ok_or_else(|| {
            format!(
                "wire {}:{} -> {}:{}: target port is out of range ({} inputs)",
                edge.from.0.to_hex(),
                edge.from.1,
                edge.to.0.to_hex(),
                edge.to.1,
                inputs.len()
            )
        })?;
        if !kinds_compatible(output.ty, input.ty) {
            return Err(format!(
                "wire {}:{} ({}) -> {}:{} ({}): incompatible port kinds",
                edge.from.0.to_hex(),
                edge.from.1,
                kind_name(output.ty),
                edge.to.0.to_hex(),
                edge.to.1,
                kind_name(input.ty)
            ));
        }
    }
    Ok(())
}

fn load_identity(path: &Path) -> Result<Identity, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::runtime(format!("cannot read identity {}: {e}", path.display())))?;
    let json: JsonValue = serde_json::from_str(&text)
        .map_err(|e| CliError::runtime(format!("invalid identity JSON: {e}")))?;
    let name = json["name"]
        .as_str()
        .ok_or_else(|| CliError::runtime("identity needs string field `name`"))?;
    let secret = json["secret"]
        .as_str()
        .ok_or_else(|| CliError::runtime("identity needs string field `secret`"))?;
    let identity = Identity::from_secret_hex(name, secret)
        .map_err(|e| CliError::runtime(format!("invalid identity secret: {e}")))?;
    if let Some(expected_public) = json["public"].as_str() {
        if identity.public_hex() != expected_public {
            return Err(CliError::runtime(
                "identity public key does not match its secret",
            ));
        }
    }
    Ok(identity)
}

fn load_ops(path: &Path) -> Result<Vec<GraphOp>, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::runtime(format!("cannot read ops {}: {e}", path.display())))?;
    let json: JsonValue = serde_json::from_str(&text)
        .map_err(|e| CliError::runtime(format!("invalid ops JSON: {e}")))?;
    let ops_json = if json.is_array() {
        json
    } else if let Some(ops) = json.get("ops") {
        ops.clone()
    } else {
        return Err(CliError::runtime(
            "ops JSON must be an array or an object with an `ops` array",
        ));
    };
    let ops: Vec<GraphOp> = serde_json::from_value(ops_json)
        .map_err(|e| CliError::runtime(format!("invalid GraphOp at {e}")))?;
    if ops.is_empty() {
        return Err(CliError::runtime("ops batch must not be empty"));
    }
    Ok(ops)
}

#[derive(Default)]
struct ApplyArgs {
    chain: Option<PathBuf>,
    ops: Option<PathBuf>,
    identity: Option<PathBuf>,
    message: Option<String>,
    output: Option<PathBuf>,
    timestamp_ms: Option<u64>,
    dry_run: bool,
    allow_errors: bool,
    expected_head: Option<String>,
    expected_state: Option<String>,
    commit_pending: bool,
}

fn parse_apply_args(args: &[String]) -> Result<ApplyArgs, CliError> {
    let mut parsed = ApplyArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let next = |name: &str, index: &mut usize| -> Result<&String, CliError> {
            *index += 1;
            args.get(*index)
                .ok_or_else(|| CliError::usage(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--ops" => parsed.ops = Some(PathBuf::from(next("--ops", &mut index)?)),
            "--identity" => parsed.identity = Some(PathBuf::from(next("--identity", &mut index)?)),
            "--message" => parsed.message = Some(next("--message", &mut index)?.clone()),
            "--out" => parsed.output = Some(PathBuf::from(next("--out", &mut index)?)),
            "--expected-head" | "--expected-state" => {
                let value = next(arg, &mut index)?;
                if value.len() != 64
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(CliError::usage(format!(
                        "{arg} needs 64 lowercase hex characters"
                    )));
                }
                if arg == "--expected-head" {
                    parsed.expected_head = Some(value.clone());
                } else {
                    parsed.expected_state = Some(value.clone());
                }
            }
            "--timestamp" => {
                let value = next("--timestamp", &mut index)?;
                parsed.timestamp_ms = Some(value.parse::<u64>().map_err(|_| {
                    CliError::usage(format!("invalid --timestamp milliseconds: {value}"))
                })?);
            }
            "--dry-run" => parsed.dry_run = true,
            "--allow-errors" => parsed.allow_errors = true,
            "--commit-pending" => parsed.commit_pending = true,
            value if !value.starts_with("--") && parsed.chain.is_none() => {
                parsed.chain = Some(PathBuf::from(value));
            }
            other => return Err(CliError::usage(format!("apply: unknown argument {other}"))),
        }
        index += 1;
    }
    Ok(parsed)
}

fn eval_error_json(errors: &std::collections::BTreeMap<NodeId, String>) -> Vec<JsonValue> {
    errors
        .iter()
        .map(|(id, message)| json!({ "id": id.to_hex(), "message": message }))
        .collect()
}

pub(crate) fn cmd_apply(args: &[String]) -> Result<String, CliError> {
    let parsed = parse_apply_args(args)?;
    let chain_path = parsed.chain.ok_or_else(|| {
        CliError::usage(
            "usage: mantis-cli apply FILE --ops OPS.json --identity ID.json --message TEXT",
        )
    })?;
    let ops_path = parsed
        .ops
        .ok_or_else(|| CliError::usage("apply requires --ops OPS.json"))?;
    let identity_path = parsed
        .identity
        .ok_or_else(|| CliError::usage("apply requires --identity ID.json"))?;
    let message = parsed
        .message
        .ok_or_else(|| CliError::usage("apply requires --message TEXT"))?;
    if message.trim().is_empty() {
        return Err(CliError::usage("apply message must not be empty"));
    }

    let destination = parsed.output.as_ref().unwrap_or(&chain_path);
    let _locks = if parsed.dry_run {
        None
    } else {
        Some(crate::local_io::DocumentLocks::acquire(&[
            &chain_path,
            destination,
        ])?)
    };
    let mut document = DocumentFile::load(&chain_path)?;
    if let Some(expected) = &parsed.expected_head {
        if expected != &document.chain().head().hash {
            return Err(CliError::coded(
                "head_mismatch",
                "document changed since observation; read graph again and re-plan the edit",
                json!({"expected_head": expected, "actual_head": document.chain().head().hash, "retryable": false}),
            ));
        }
    }
    let previous_state_hash = document.state_hash();
    if let Some(expected) = &parsed.expected_state {
        if expected != &previous_state_hash {
            return Err(CliError::coded(
                "state_mismatch",
                "working definition changed since observation; read graph again and re-plan the edit",
                json!({"expected_state": expected, "actual_state": previous_state_hash, "retryable": false}),
            ));
        }
    }
    let pending_count = document.pending().len();
    if pending_count > 0 && !parsed.commit_pending {
        return Err(CliError::coded(
            "pending_work_requires_commit",
            "workspace contains unsigned working edits; Commit in the GUI first, or use --commit-pending to explicitly sign the pending edits together with this batch",
            json!({"pending_ops": pending_count, "path": chain_path}),
        ));
    }
    let identity = load_identity(&identity_path)?;
    let mut ops = document.pending().to_vec();
    ops.extend(load_ops(&ops_path)?);

    // Trial the complete batch before signing it. `apply_all` mutates only the
    // clone, so any failure leaves both memory and disk untouched.
    let mut graph = document
        .chain()
        .replay(None)
        .map_err(|e| CliError::runtime(format!("cannot materialize chain: {e}")))?;
    graph.apply_all(&ops).map_err(|(index, error)| {
        CliError::coded(
            "graph_op_rejected",
            format!("GraphOp {index} rejected: {error}"),
            json!({"op": index, "message": error.to_string()}),
        )
    })?;
    let registry = Registry::standard();
    validate_agent_graph(&graph, &registry)
        .map_err(|message| CliError::coded("graph_validation_failed", message, json!({})))?;
    let evaluation = Evaluator::new().evaluate(&graph, &registry);
    if !parsed.allow_errors && !evaluation.errors.is_empty() {
        let summary = evaluation
            .errors
            .iter()
            .take(8)
            .map(|(id, error)| format!("{}: {error}", id.to_hex()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(CliError::coded("evaluation_failed", format!(
            "evaluation failed ({summary}); use --allow-errors only for an intentional incomplete graph"
        ), json!({"nodes": eval_error_json(&evaluation.errors)})));
    }

    let timestamp_ms = parsed.timestamp_ms.unwrap_or_else(now_ms);
    let op_count = ops.len();
    document
        .chain_mut()
        .append(ops, &message, &identity, timestamp_ms)
        .map_err(|e| CliError::runtime(format!("cannot seal block: {e}")))?;
    // Exercise the exact load-time contract before touching the destination.
    document
        .chain()
        .validate()
        .map_err(|e| CliError::runtime(format!("sealed chain failed validation: {e}")))?;
    document.committed(timestamp_ms);
    let serialized = document.to_bytes()?;
    let block = document.chain().head();
    let receipt = json!({
        "ok": true,
        "dry_run": parsed.dry_run,
        "path": parsed.output.as_ref().unwrap_or(&chain_path),
        "committed_pending_ops": pending_count,
        "previous_state_hash": previous_state_hash,
        "state_hash": document.state_hash(),
        "block": {
            "index": block.index,
            "hash": block.hash,
            "prev_hash": block.prev_hash,
            "timestamp_ms": block.timestamp_ms,
            "author": block.author,
            "author_pk": block.author_pk,
            "message": block.message,
            "ops": op_count,
        },
        "graph": {
            "nodes": graph.nodes.len(),
            "edges": graph.edges.len(),
            "evaluation_errors": eval_error_json(&evaluation.errors),
        }
    });

    if !parsed.dry_run {
        let destination = parsed.output.as_ref().unwrap_or(&chain_path);
        write_json_atomic(destination, &serialized)?;
    }
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&receipt).unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// graph — bounded materialized result for agent perception
// ---------------------------------------------------------------------------

#[cfg(test)]
fn graph_json(chain: &Chain, upto: Option<usize>) -> Result<JsonValue, CliError> {
    graph_json_with_options(chain, &[], upto, None, false)
}

fn parameter_values_json(
    parameters: &std::collections::BTreeMap<String, ParamValue>,
    full: bool,
) -> JsonValue {
    let entries = parameters.iter().map(|(key, value)| {
        let value = match value {
            ParamValue::Text(text) if !full && text.len() > TEXT_REPORT_LIMIT => {
                let mut summary = text_summary(text);
                summary["read_full"] = json!("graph FILE --json --node NODE_ID --full-parameters");
                summary
            }
            _ => serde_json::to_value(value).unwrap_or(JsonValue::Null),
        };
        (key.clone(), value)
    });
    JsonValue::Object(entries.collect())
}

fn graph_json_with_options(
    chain: &Chain,
    pending: &[GraphOp],
    upto: Option<usize>,
    only_node: Option<NodeId>,
    full_parameters: bool,
) -> Result<JsonValue, CliError> {
    let mut graph = chain
        .replay(upto)
        .map_err(|e| CliError::runtime(format!("cannot replay graph: {e}")))?;
    let pending = if upto.is_none() { pending } else { &[] };
    graph.apply_all(pending).map_err(|(index, error)| {
        CliError::coded(
            "invalid_workspace_pending",
            format!("pending operation {index} cannot be replayed: {error}"),
            json!({"op": index}),
        )
    })?;
    let registry = Registry::standard();
    if let Some(id) = only_node {
        if !graph.nodes.contains_key(&id) {
            return Err(CliError::runtime(format!(
                "node {} does not exist at the requested revision",
                id.to_hex()
            )));
        }
    }
    let evaluation = Evaluator::new().evaluate(&graph, &registry);
    let nodes: Vec<JsonValue> = graph
        .topo_order()
        .into_iter()
        .filter_map(|id| graph.nodes.get(&id))
        .filter(|node| only_node.is_none_or(|id| node.id == id))
        .map(|node| {
            let output_names: Vec<String> = registry
                .get(&node.type_name)
                .map(|component| {
                    component
                        .outputs()
                        .iter()
                        .map(|port| port.name.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let outputs: Vec<JsonValue> = evaluation
                .outputs
                .get(&node.id)
                .map(|values| {
                    values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            json!({
                                "index": index,
                                "name": output_names.get(index).map(String::as_str),
                                "value": runtime_value_json(value),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "id": node.id.to_hex(),
                "type_name": node.type_name,
                "position": [node.pos.0, node.pos.1],
                "parameters": parameter_values_json(&node.params, full_parameters),
                "preview": node.preview(),
                "status": if evaluation.errors.contains_key(&node.id) { "error" } else { "ok" },
                "error": evaluation.errors.get(&node.id),
                "outputs": outputs,
            })
        })
        .collect();
    let edges: Vec<JsonValue> = graph
        .edges
        .iter()
        .filter(|edge| only_node.is_none_or(|id| edge.from.0 == id || edge.to.0 == id))
        .map(|edge| {
            json!({
                "from": [edge.from.0.to_hex(), edge.from.1],
                "to": [edge.to.0.to_hex(), edge.to.1],
            })
        })
        .collect();
    let last_index = upto
        .unwrap_or_else(|| chain.len().saturating_sub(1))
        .min(chain.len().saturating_sub(1));
    let block = &chain.blocks[last_index];
    Ok(json!({
        "protocol": "mantis-agent",
        "version": 1,
        "materialized_at": { "index": block.index, "hash": block.hash },
        "state_hash": working_state_hash(&block.hash, pending),
        "pending_ops": pending.len(),
        "parameter_text_limit_bytes": if full_parameters { JsonValue::Null } else { json!(TEXT_REPORT_LIMIT) },
        "nodes": nodes,
        "edges": edges,
        "evaluation_errors": eval_error_json(&evaluation.errors),
    }))
}

pub(crate) fn cmd_graph(args: &[String]) -> Result<String, CliError> {
    let mut path: Option<String> = None;
    let mut upto: Option<usize> = None;
    let mut as_json = false;
    let mut only_node = None;
    let mut full_parameters = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => as_json = true,
            "--full-parameters" => full_parameters = true,
            "--node" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| CliError::usage("--node needs a node id"))?;
                only_node = Some(
                    NodeId::from_hex(value)
                        .ok_or_else(|| CliError::usage(format!("invalid --node id: {value}")))?,
                );
            }
            "--upto" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| CliError::usage("--upto needs a value"))?;
                upto = Some(value.parse::<usize>().map_err(|_| {
                    CliError::usage(format!("invalid --upto block index: {value}"))
                })?);
            }
            value if !value.starts_with("--") && path.is_none() => path = Some(value.into()),
            other => return Err(CliError::usage(format!("graph: unknown argument {other}"))),
        }
        index += 1;
    }
    let path = path.ok_or_else(|| {
        CliError::usage(
            "usage: mantis-cli graph FILE [--upto N] [--json] [--node ID [--full-parameters]]",
        )
    })?;
    if full_parameters && (only_node.is_none() || !as_json) {
        return Err(CliError::usage(
            "--full-parameters requires --json and --node ID",
        ));
    }
    let document = DocumentFile::load(Path::new(&path))?;
    let report = graph_json_with_options(
        document.chain(),
        document.pending(),
        upto,
        only_node,
        full_parameters,
    )?;
    if as_json {
        return Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        ));
    }
    let mut text = format!(
        "materialized block {} {} ({} pending ops)\n",
        report["materialized_at"]["index"].as_u64().unwrap_or(0),
        report["materialized_at"]["hash"].as_str().unwrap_or(""),
        report["pending_ops"].as_u64().unwrap_or(0)
    );
    if let Some(nodes) = report["nodes"].as_array() {
        for node in nodes {
            let status = node["status"].as_str().unwrap_or("?");
            let id = node["id"].as_str().unwrap_or("");
            let short_id = id.get(..8).unwrap_or(id);
            let detail = node["error"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "{} output(s)",
                        node["outputs"].as_array().map_or(0, Vec::len)
                    )
                });
            text.push_str(&format!(
                "{}  {:<18} {:<5} {}\n",
                short_id,
                node["type_name"].as_str().unwrap_or("?"),
                status,
                detail
            ));
        }
    }
    text.push_str(&format!(
        "totals: {} nodes, {} edges, {} evaluation errors\n",
        report["nodes"].as_array().map_or(0, Vec::len),
        report["edges"].as_array().map_or(0, Vec::len),
        report["evaluation_errors"].as_array().map_or(0, Vec::len),
    ));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "mantis-agent-test-{}-{n}-{tag}",
            std::process::id()
        ))
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn write_identity(path: &Path, identity: &Identity) {
        std::fs::write(
            path,
            serde_json::to_string(&json!({
                "name": identity.name,
                "secret": identity.secret_hex(),
                "public": identity.public_hex(),
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn sphere_ops(id: NodeId) -> Vec<GraphOp> {
        vec![GraphOp::AddNode {
            id,
            type_name: "sphere".into(),
            pos: (20.0, 30.0),
        }]
    }

    #[test]
    fn catalog_exposes_machine_port_and_param_schema() {
        let output = cmd_catalog(&strings(&["--json"])).unwrap();
        let json: JsonValue = serde_json::from_str(&output).unwrap();
        assert_eq!(json["protocol"], "mantis-agent");
        let components = json["components"].as_array().unwrap();
        let circle = components
            .iter()
            .find(|component| component["type_name"] == "circle")
            .unwrap();
        assert_eq!(circle["inputs"][0]["name"], "plane");
        assert_eq!(circle["inputs"][1]["kind"], "Number");
        let slider = components
            .iter()
            .find(|component| component["type_name"] == "number_slider")
            .unwrap();
        assert!(slider["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|parameter| parameter["name"] == "value"));
        for component in Registry::standard().iter() {
            let catalog_component = components
                .iter()
                .find(|entry| entry["type_name"] == component.type_name())
                .unwrap();
            let expected: Vec<_> = component.parameters().iter().map(param_json).collect();
            assert_eq!(catalog_component["parameters"], json!(expected));
        }
    }

    #[test]
    fn init_refuses_to_overwrite() {
        let chain_path = temp_path("init.json");
        cmd_init(&strings(&[chain_path.to_str().unwrap()])).unwrap();
        let chain = load_chain(chain_path.to_str().unwrap()).unwrap();
        assert_eq!(chain.len(), 1);
        assert!(matches!(
            cmd_init(&strings(&[chain_path.to_str().unwrap()])),
            Err(CliError::Runtime(_))
        ));
        let _ = std::fs::remove_file(chain_path);
    }

    #[test]
    fn apply_validates_signs_and_graph_reports_geometry() {
        let chain_path = temp_path("model.json");
        let identity_path = temp_path("identity.json");
        let ops_path = temp_path("ops.json");
        write_chain_atomic(&chain_path, &Chain::new()).unwrap();
        let identity = Identity::generate("agent-1");
        write_identity(&identity_path, &identity);
        std::fs::write(
            &ops_path,
            serde_json::to_string(&sphere_ops(NodeId(1))).unwrap(),
        )
        .unwrap();

        let output = cmd_apply(&strings(&[
            chain_path.to_str().unwrap(),
            "--ops",
            ops_path.to_str().unwrap(),
            "--identity",
            identity_path.to_str().unwrap(),
            "--message",
            "add a sphere",
            "--timestamp",
            "1234",
        ]))
        .unwrap();
        let receipt: JsonValue = serde_json::from_str(&output).unwrap();
        assert_eq!(receipt["block"]["index"], 1);
        assert_eq!(receipt["block"]["author"], "agent-1");
        assert_eq!(receipt["block"]["timestamp_ms"], 1234);
        let chain = load_chain(chain_path.to_str().unwrap()).unwrap();
        assert_eq!(chain.len(), 2);
        let audit: JsonValue =
            serde_json::from_str(&cmd_audit(&strings(&[chain_path.to_str().unwrap()])).unwrap())
                .unwrap();
        assert_eq!(audit["head_hash"], chain.head().hash);
        assert_eq!(audit["signed_block_count"], 1);
        assert_eq!(audit["authors"][0]["names"][0], "agent-1");

        let report = graph_json(&chain, None).unwrap();
        assert_eq!(report["nodes"][0]["type_name"], "sphere");
        assert_eq!(report["nodes"][0]["outputs"][0]["value"]["kind"], "Mesh");
        assert!(
            report["nodes"][0]["outputs"][0]["value"]["vertices"]
                .as_u64()
                .unwrap()
                > 0
        );

        for path in [chain_path, identity_path, ops_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn apply_rejects_invisible_port_without_modifying_chain() {
        let chain_path = temp_path("bad-port-model.json");
        let identity_path = temp_path("bad-port-identity.json");
        let ops_path = temp_path("bad-port-ops.json");
        write_chain_atomic(&chain_path, &Chain::new()).unwrap();
        let identity = Identity::generate("agent-2");
        write_identity(&identity_path, &identity);
        let a = NodeId(1);
        let b = NodeId(2);
        let ops = vec![
            GraphOp::AddNode {
                id: a,
                type_name: "sphere".into(),
                pos: (0.0, 0.0),
            },
            GraphOp::AddNode {
                id: b,
                type_name: "move".into(),
                pos: (200.0, 0.0),
            },
            GraphOp::Connect {
                from: (a, 99),
                to: (b, 0),
            },
        ];
        std::fs::write(&ops_path, serde_json::to_string(&ops).unwrap()).unwrap();
        let result = cmd_apply(&strings(&[
            chain_path.to_str().unwrap(),
            "--ops",
            ops_path.to_str().unwrap(),
            "--identity",
            identity_path.to_str().unwrap(),
            "--message",
            "bad port",
        ]));
        let error = result.unwrap_err();
        assert!(error.message().contains("out of range"), "{error:?}");
        assert_eq!(load_chain(chain_path.to_str().unwrap()).unwrap().len(), 1);

        for path in [chain_path, identity_path, ops_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn dry_run_and_eval_failure_never_write() {
        let chain_path = temp_path("dry-model.json");
        let identity_path = temp_path("dry-identity.json");
        let ops_path = temp_path("dry-ops.json");
        write_chain_atomic(&chain_path, &Chain::new()).unwrap();
        let identity = Identity::generate("agent-3");
        write_identity(&identity_path, &identity);
        let incomplete = vec![GraphOp::AddNode {
            id: NodeId(1),
            type_name: "add".into(),
            pos: (0.0, 0.0),
        }];
        std::fs::write(&ops_path, serde_json::to_string(&incomplete).unwrap()).unwrap();
        let base = [
            chain_path.to_str().unwrap(),
            "--ops",
            ops_path.to_str().unwrap(),
            "--identity",
            identity_path.to_str().unwrap(),
            "--message",
            "incomplete",
        ];
        assert!(cmd_apply(&strings(&base))
            .unwrap_err()
            .message()
            .contains("evaluation failed"));
        assert_eq!(load_chain(chain_path.to_str().unwrap()).unwrap().len(), 1);

        let mut dry_args = base.to_vec();
        dry_args.extend(["--allow-errors", "--dry-run"]);
        let receipt: JsonValue =
            serde_json::from_str(&cmd_apply(&strings(&dry_args)).unwrap()).unwrap();
        assert_eq!(receipt["dry_run"], true);
        assert_eq!(load_chain(chain_path.to_str().unwrap()).unwrap().len(), 1);

        for path in [chain_path, identity_path, ops_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn agent_can_edit_documents_with_gui_command_cad_and_grasshopper_parameters() {
        use mantis_graph::geometry::{GeometryData, GeometryRecord};

        let chain_path = temp_path("handoff-model.json");
        let identity_path = temp_path("handoff-identity.json");
        let ops_path = temp_path("handoff-ops.json");
        let human = Identity::generate("human-designer");
        let agent = Identity::generate("cad-agent");
        write_identity(&identity_path, &agent);

        let mut gui_ops = Vec::new();
        for (id, ty) in [
            (1, "circle"),
            (2, "number_slider"),
            (3, "point_xyz"),
            (4, "panel"),
            (5, "imported_geometry"),
            (6, "constant_list"),
        ] {
            gui_ops.push(GraphOp::AddNode {
                id: NodeId(id),
                type_name: ty.into(),
                pos: (0.0, 0.0),
            });
        }
        let record = GeometryRecord {
            name: "Imported point".into(),
            layer: "Layer 1".into(),
            geometry: GeometryData::Point {
                point: Vec3::new(1.0, 2.0, 3.0),
            },
            source: None,
        };
        // Same persistent fields written by Recipe::number/vector,
        // cad_io::insert_records and grasshopper::Plan::literals.
        for (id, key, value) in [
            (2, "label", ParamValue::Text("radius".into())),
            (2, "value", ParamValue::Number(5.0)),
            (2, "min", ParamValue::Number(-10.0)),
            (2, "max", ParamValue::Number(10.0)),
            (3, "label", ParamValue::Text("center".into())),
            (3, "__preview", ParamValue::Bool(false)),
            (
                4,
                "text",
                ParamValue::Text("CAD source document (shared metadata)".into()),
            ),
            (
                4,
                "__cad_document",
                ParamValue::Text("{\"units\":2}".into()),
            ),
            (4, "__preview", ParamValue::Bool(false)),
            (5, "data", ParamValue::Text(record.to_json().unwrap())),
            (5, "label", ParamValue::Text(record.name.clone())),
            (
                5,
                "__cad_document_node",
                ParamValue::Text(NodeId(4).to_hex()),
            ),
            (
                6,
                "values",
                ParamValue::Text("[1,2,true,\"Grasshopper text\"]".into()),
            ),
            (6, "label", ParamValue::Text("Imported GH panel".into())),
            (6, "__preview", ParamValue::Bool(false)),
        ] {
            gui_ops.push(GraphOp::SetParam {
                id: NodeId(id),
                key: key.into(),
                value,
            });
        }
        gui_ops.push(GraphOp::Connect {
            from: (NodeId(2), 0),
            to: (NodeId(1), 1),
        });
        gui_ops.push(GraphOp::Connect {
            from: (NodeId(3), 0),
            to: (NodeId(1), 0),
        });
        let mut chain = Chain::new();
        chain
            .append(gui_ops, "GUI commands and CAD/GHX import", &human, 100)
            .unwrap();
        write_chain_atomic(&chain_path, &chain).unwrap();
        let edits: Vec<_> = (1..=6)
            .map(|id| GraphOp::MoveNode {
                id: NodeId(id),
                pos: (120.0, 80.0),
            })
            .collect();
        std::fs::write(&ops_path, serde_json::to_string(&edits).unwrap()).unwrap();
        let receipt: JsonValue = serde_json::from_str(
            &cmd_apply(&strings(&[
                chain_path.to_str().unwrap(),
                "--ops",
                ops_path.to_str().unwrap(),
                "--identity",
                identity_path.to_str().unwrap(),
                "--message",
                "continue human design",
                "--timestamp",
                "200",
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["graph"]["evaluation_errors"], json!([]));
        let edited = load_chain(chain_path.to_str().unwrap()).unwrap();
        edited.validate().unwrap();
        assert_eq!(edited.len(), 3);
        assert_eq!(edited.head().author, "cad-agent");
        let graph = edited.replay(None).unwrap();
        assert_eq!(
            graph.nodes[&NodeId(5)].params["data"],
            ParamValue::Text(record.to_json().unwrap())
        );
        assert_eq!(graph.nodes[&NodeId(6)].pos, (120.0, 80.0));
        for path in [chain_path, identity_path, ops_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn shared_contract_keeps_misspelled_and_wrongly_typed_parameters_invalid() {
        let registry = Registry::standard();
        for (key, value, expected) in [
            (
                "lable",
                ParamValue::Text("radius".into()),
                "unknown parameter lable",
            ),
            (
                "label",
                ParamValue::Bool(true),
                "parameter label expects Text",
            ),
            (
                "value",
                ParamValue::Text("five".into()),
                "parameter value expects Number",
            ),
        ] {
            let mut graph = Graph::new();
            graph
                .apply_all(&[
                    GraphOp::AddNode {
                        id: NodeId(1),
                        type_name: "number_slider".into(),
                        pos: (0.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: NodeId(1),
                        key: key.into(),
                        value,
                    },
                ])
                .unwrap();
            assert!(validate_agent_graph(&graph, &registry)
                .unwrap_err()
                .contains(expected));
        }
    }

    #[test]
    fn graph_bounds_large_text_and_allows_explicit_full_parameter_for_one_node() {
        use mantis_graph::geometry::{GeometryData, GeometryRecord, GeometrySource};

        let path = temp_path("bounded-graph.json");
        let text = "🪲".repeat(8_000);
        let record = GeometryRecord {
            name: "Preserved CAD".into(),
            layer: String::new(),
            geometry: GeometryData::Point {
                point: Vec3::new(1.0, 2.0, 3.0),
            },
            source: Some(GeometrySource {
                format: "test".into(),
                data: text.clone(),
                preview_units: None,
            }),
        };
        let data = record.to_json().unwrap();
        let mut chain = Chain::new();
        chain
            .append(
                vec![
                    GraphOp::AddNode {
                        id: NodeId(1),
                        type_name: "imported_geometry".into(),
                        pos: (0.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: NodeId(1),
                        key: "data".into(),
                        value: ParamValue::Text(data.clone()),
                    },
                    GraphOp::AddNode {
                        id: NodeId(2),
                        type_name: "constant_list".into(),
                        pos: (0.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: NodeId(2),
                        key: "values".into(),
                        value: ParamValue::Text(serde_json::to_string(&vec![&text]).unwrap()),
                    },
                ],
                "large imported source",
                &Identity::generate("human"),
                100,
            )
            .unwrap();
        write_chain_atomic(&path, &chain).unwrap();
        let compact = cmd_graph(&strings(&[path.to_str().unwrap(), "--json"])).unwrap();
        assert!(
            compact.len() < 8_000,
            "unexpectedly large report: {} bytes",
            compact.len()
        );
        let report: JsonValue = serde_json::from_str(&compact).unwrap();
        assert_eq!(report["nodes"][0]["parameters"]["data"]["truncated"], true);
        assert_eq!(
            report["nodes"][0]["parameters"]["data"]["bytes"],
            data.len()
        );
        assert_eq!(
            report["nodes"][1]["outputs"][0]["value"]["items"][0]["truncated"],
            true
        );
        let node = NodeId(1).to_hex();
        let full: JsonValue = serde_json::from_str(
            &cmd_graph(&strings(&[
                path.to_str().unwrap(),
                "--json",
                "--node",
                &node,
                "--full-parameters",
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(full["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(full["nodes"][0]["parameters"]["data"]["Text"], data);
        assert!(cmd_graph(&strings(&[
            path.to_str().unwrap(),
            "--json",
            "--full-parameters"
        ]))
        .is_err());
        assert!(cmd_graph(&strings(&[
            path.to_str().unwrap(),
            "--json",
            "--node",
            &NodeId(99).to_hex()
        ]))
        .is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn portable_workspace_observation_and_explicit_signed_handoff_preserve_work() {
        use mantis_protocol::{PortableWorkspaceV1, PORTABLE_WORKSPACE_VERSION};

        let path = temp_path("portable.mantis");
        let identity_path = temp_path("portable-identity.json");
        let ops_path = temp_path("portable-ops.json");
        let human = Identity::generate("human-designer");
        let agent = Identity::generate("agent-finisher");
        write_identity(&identity_path, &agent);
        let mut chain = Chain::new();
        chain
            .append(
                vec![
                    GraphOp::AddNode {
                        id: NodeId(1),
                        type_name: "number_slider".into(),
                        pos: (0.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: NodeId(1),
                        key: "value".into(),
                        value: ParamValue::Number(2.0),
                    },
                ],
                "human committed model",
                &human,
                100,
            )
            .unwrap();
        let pending = vec![
            GraphOp::SetParam {
                id: NodeId(1),
                key: "value".into(),
                value: ParamValue::Number(7.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "label".into(),
                value: ParamValue::Text("Human radius".into()),
            },
            GraphOp::AddNode {
                id: NodeId(2),
                type_name: "point_xyz".into(),
                pos: (40.0, 20.0),
            },
        ];
        let remote = serde_json::from_value(json!({
            "base_url": "https://cad.example", "project_id": "demo",
            "chain_format_version": chain.format_version().unwrap(), "chain_id": chain.chain_id().unwrap(),
            "genesis_hash": chain.blocks[0].hash, "last_synced_len": chain.len(),
            "last_synced_head": chain.head().hash, "access": "writer",
        })).unwrap();
        let workspace = PortableWorkspaceV1 {
            format_version: PORTABLE_WORKSPACE_VERSION,
            id: "human-workspace".into(),
            name: "Shared design".into(),
            updated_ms: 100,
            chain,
            pending: pending.clone(),
            recovery_ops: vec![GraphOp::RemoveNode { id: NodeId(99) }],
            remote: Some(remote),
        };
        let original = serde_json::to_vec_pretty(&workspace).unwrap();
        write_json_atomic(&path, &original).unwrap();
        let observed: JsonValue = serde_json::from_str(
            &cmd_graph(&strings(&[path.to_str().unwrap(), "--json"])).unwrap(),
        )
        .unwrap();
        assert_eq!(observed["pending_ops"], 3);
        assert_eq!(
            observed["materialized_at"]["hash"],
            workspace.chain.head().hash
        );
        assert_eq!(observed["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(observed["nodes"][0]["outputs"][0]["value"]["value"], 7.0);
        let historic: JsonValue = serde_json::from_str(
            &cmd_graph(&strings(&[path.to_str().unwrap(), "--json", "--upto", "1"])).unwrap(),
        )
        .unwrap();
        assert_eq!(historic["pending_ops"], 0);
        assert_eq!(historic["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(historic["nodes"][0]["outputs"][0]["value"]["value"], 2.0);
        // Committed-history commands can read the same file, without sealing WIP.
        assert_eq!(load_chain(path.to_str().unwrap()).unwrap(), workspace.chain);
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let edits = vec![
            GraphOp::MoveNode {
                id: NodeId(2),
                pos: (120.0, 80.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "value".into(),
                value: ParamValue::Number(9.0),
            },
        ];
        std::fs::write(&ops_path, serde_json::to_vec(&edits).unwrap()).unwrap();
        let base = strings(&[
            path.to_str().unwrap(),
            "--ops",
            ops_path.to_str().unwrap(),
            "--identity",
            identity_path.to_str().unwrap(),
            "--message",
            "explicitly continue human WIP",
            "--timestamp",
            "200",
        ]);
        assert!(
            matches!(cmd_apply(&base), Err(CliError::Detailed { code, .. }) if code == "pending_work_requires_commit")
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut explicit = base.clone();
        explicit.push("--commit-pending".into());
        let mut dry_run = explicit.clone();
        dry_run.push("--dry-run".into());
        cmd_apply(&dry_run).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let receipt: JsonValue = serde_json::from_str(&cmd_apply(&explicit).unwrap()).unwrap();
        assert_eq!(receipt["committed_pending_ops"], 3);
        let updated: PortableWorkspaceV1 =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        updated.validate().unwrap();
        assert_eq!(updated.id, workspace.id);
        assert_eq!(updated.name, workspace.name);
        assert_eq!(updated.remote, workspace.remote);
        assert_eq!(updated.recovery_ops, workspace.recovery_ops);
        assert_eq!(updated.updated_ms, 200);
        assert!(updated.pending.is_empty());
        assert_eq!(updated.chain.head().author, "agent-finisher");
        let mut expected_ops = pending;
        expected_ops.extend(edits);
        assert_eq!(updated.chain.head().ops, expected_ops);
        let graph = updated.chain.replay(None).unwrap();
        assert_eq!(
            graph.nodes[&NodeId(1)].params["value"],
            ParamValue::Number(9.0)
        );
        assert_eq!(graph.nodes[&NodeId(2)].pos, (120.0, 80.0));
        for path in [path, identity_path, ops_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn expected_state_detects_pending_changes_with_an_unchanged_committed_head() {
        use mantis_protocol::{PortableWorkspaceV1, PORTABLE_WORKSPACE_VERSION};
        let path = temp_path("working-state.mantis");
        let identity_path = temp_path("working-state-identity.json");
        let ops_path = temp_path("working-state-ops.json");
        write_identity(&identity_path, &Identity::generate("agent"));
        let mut workspace = PortableWorkspaceV1 {
            format_version: PORTABLE_WORKSPACE_VERSION,
            id: "working-state".into(),
            name: "Working state".into(),
            updated_ms: 100,
            chain: Chain::new(),
            recovery_ops: Vec::new(),
            remote: None,
            pending: vec![GraphOp::AddNode {
                id: NodeId(1),
                type_name: "number_slider".into(),
                pos: (0.0, 0.0),
            }],
        };
        write_json_atomic(&path, &serde_json::to_vec(&workspace).unwrap()).unwrap();
        let observed: JsonValue = serde_json::from_str(
            &cmd_graph(&strings(&[path.to_str().unwrap(), "--json"])).unwrap(),
        )
        .unwrap();
        let observed_state = observed["state_hash"].as_str().unwrap();
        workspace.pending.push(GraphOp::SetParam {
            id: NodeId(1),
            key: "value".into(),
            value: ParamValue::Number(3.0),
        });
        let changed = serde_json::to_vec(&workspace).unwrap();
        write_json_atomic(&path, &changed).unwrap();
        let actual_state = working_state_hash(&workspace.chain.head().hash, &workspace.pending);
        assert_ne!(observed_state, actual_state);
        assert_eq!(
            observed["materialized_at"]["hash"],
            workspace.chain.head().hash
        );
        std::fs::write(
            &ops_path,
            serde_json::to_vec(&vec![GraphOp::MoveNode {
                id: NodeId(1),
                pos: (20.0, 30.0),
            }])
            .unwrap(),
        )
        .unwrap();
        let mut args = strings(&[
            path.to_str().unwrap(),
            "--ops",
            ops_path.to_str().unwrap(),
            "--identity",
            identity_path.to_str().unwrap(),
            "--message",
            "continue observed work",
            "--commit-pending",
            "--expected-head",
            &workspace.chain.head().hash,
            "--expected-state",
            observed_state,
        ]);
        match cmd_apply(&args) {
            Err(CliError::Detailed { code, context, .. }) => {
                assert_eq!(code, "state_mismatch");
                assert_eq!(context["actual_state"], actual_state);
            }
            other => panic!("expected pending state conflict, got {other:?}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        *args.last_mut().unwrap() = actual_state.clone();
        let receipt: JsonValue = serde_json::from_str(&cmd_apply(&args).unwrap()).unwrap();
        assert_eq!(receipt["previous_state_hash"], actual_state);
        let updated = DocumentFile::load(&path).unwrap();
        assert!(updated.pending().is_empty());
        assert_eq!(receipt["state_hash"], updated.state_hash());
        assert_eq!(
            updated.chain().replay(None).unwrap().nodes[&NodeId(1)].pos,
            (20.0, 30.0)
        );
        for path in [path, identity_path, ops_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn atomic_chain_write_through_symlink_replaces_target_and_keeps_alias() {
        let target = temp_path("symlink-target.json");
        let alias = temp_path("symlink-alias.json");
        let mut chain = Chain::new();
        write_chain_atomic(&target, &chain).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        chain
            .append(
                sphere_ops(NodeId(1)),
                "signed update",
                &Identity::generate("agent"),
                100,
            )
            .unwrap();
        write_chain_atomic(&alias, &chain).unwrap();
        assert!(alias.is_symlink());
        assert_eq!(load_chain(target.to_str().unwrap()).unwrap(), chain);
        assert_eq!(load_chain(alias.to_str().unwrap()).unwrap(), chain);
        for path in [alias, target] {
            let _ = std::fs::remove_file(path);
        }
    }
}
