//! Native CAD exchange and optional exact B-rep engine. Workers never mutate
//! the document: a successful response is validated and inserted as one edit.

use crate::{grasshopper, state::Document, util::new_node_id};
use mantis_graph::{geometry::GeometryRecord, Graph, GraphOp, NodeId, ParamValue, Value};
use serde::Deserialize;
use serde_json::{json, Value as Json};
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::Hasher,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

const MAX_IO: usize = 64 * 1024 * 1024;

#[derive(Default, Deserialize)]
struct BridgeResponse {
    #[serde(default)]
    objects: Vec<GeometryRecord>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    document: Option<Json>,
    #[serde(default)]
    written: usize,
    #[serde(default)]
    metrics: Option<Json>,
}

enum Outcome {
    Geometry(BridgeResponse),
    Graph(grasshopper::ImportReport),
    Export(String, Vec<String>),
}

struct Job {
    receiver: mpsc::Receiver<Result<Outcome, String>>,
    cancel: Arc<AtomicBool>,
    workspace: String,
    hide: Vec<NodeId>,
    context: JobContext,
}

enum JobContext {
    Import,
    Export(PathBuf),
    Brep(u64),
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct CadIo {
    pub open: bool,
    tab: usize,
    import_path: String,
    export_path: String,
    selected_only: bool,
    operation: usize,
    origin: [f64; 3],
    size: [f64; 3],
    normal: [f64; 3],
    radius: f64,
    height: f64,
    keep_positive: bool,
    edges: String,
    status: String,
    warnings: Vec<String>,
    job: Option<Job>,
}

impl Default for CadIo {
    fn default() -> Self {
        Self {
            open: false,
            tab: 0,
            import_path: String::new(),
            export_path: default_export_path(),
            selected_only: true,
            operation: 0,
            origin: [0.; 3],
            size: [10.; 3],
            normal: [0., 0., 1.],
            radius: 1.,
            height: 10.,
            keep_positive: false,
            edges: String::new(),
            status: String::new(),
            warnings: Vec::new(),
            job: None,
        }
    }
}

impl CadIo {
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        selection: &BTreeSet<NodeId>,
        workspace: &str,
    ) -> Option<BTreeSet<NodeId>> {
        let mut new_selection = self.poll(doc, workspace);
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
            if matches!(
                extension(&path).as_str(),
                "3dm" | "gh" | "ghx" | "step" | "stp"
            ) {
                self.import_path = path.to_string_lossy().into();
                self.tab = 0;
                self.open = true;
            }
        }
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.open {
            return new_selection;
        }
        let mut open = self.open;
        egui::Window::new("CAD files & B-rep")
            .open(&mut open).default_width(570.).resizable(true).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, 0, "Import");
                    ui.selectable_value(&mut self.tab, 1, "Export");
                    ui.selectable_value(&mut self.tab, 2, "Solids & fillets");
                });
                ui.separator();
                let idle = self.job.is_none();
                ui.add_enabled_ui(idle, |ui| match self.tab {
                    0 => {
                        ui.label("Open Rhino .3dm, Grasshopper .gh/.ghx or STEP files.");
                        ui.label("File path (you can also drop a file onto the window)");
                        ui.add(egui::TextEdit::singleline(&mut self.import_path).desired_width(f32::INFINITY));
                        ui.weak("Supported objects and connections are added to this workspace. Import is one Undo step.");
                        if ui.add_enabled(doc.editable(), egui::Button::new("Import file")).clicked() {
                            let path = PathBuf::from(self.import_path.trim());
                            self.start(workspace, Vec::new(), JobContext::Import, move |cancel| import_file(&path, cancel));
                        }
                    }
                    1 => {
                        ui.label("Save .3dm, .ghx, .gh or .step by entering the matching extension.");
                        ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(f32::INFINITY));
                        ui.checkbox(&mut self.selected_only, "Selected geometry only (.3dm / STEP)");
                        ui.weak("Grasshopper export includes the graph. Unsupported nodes are reported. Existing files are protected.");
                        if ui.button("Export file").clicked() {
                            let path = PathBuf::from(self.export_path.trim());
                            let ext = extension(&path);
                            let prepared = if matches!(ext.as_str(), "gh" | "ghx") {
                                grasshopper::export_ghx(doc.display_graph()).map(ExportPayload::Ghx)
                            } else {
                                collect_records(doc, if self.selected_only { Some(selection) } else { None })
                                    .map(|(objects, document)| ExportPayload::Geometry { objects, document })
                            };
                            match prepared {
                                Ok(payload) => self.start(workspace, Vec::new(), JobContext::Export(path.clone()), move |cancel| export_file(&path, payload, cancel)),
                                Err(error) => self.status = error,
                            }
                        }
                    }
                    _ => {
                        const OPS: &[(&str, &str)] = &[("box", "B-rep Box"), ("sphere", "B-rep Sphere"),
                            ("cylinder", "B-rep Cylinder"), ("union", "Boolean Union"),
                            ("difference", "Boolean Difference"), ("intersection", "Boolean Intersection"),
                            ("trim", "Trim with plane"), ("fillet", "Fillet edges")];
                        egui::ComboBox::from_id_salt("brep_operation").selected_text(OPS[self.operation].1)
                            .show_ui(ui, |ui| { for (index, (_, label)) in OPS.iter().enumerate() {
                                ui.selectable_value(&mut self.operation, index, *label);
                            }});
                        let operation = OPS[self.operation].0;
                        ui.weak("B-rep dimensions and fillet radii use millimeters.");
                        if self.operation <= 2 || operation == "trim" { vector_ui(ui, "Origin / center", &mut self.origin); }
                        if operation == "box" { vector_ui(ui, "Size X / Y / Z", &mut self.size); }
                        if matches!(operation, "sphere" | "cylinder" | "fillet") {
                            ui.horizontal(|ui| { ui.label("Radius"); ui.add(egui::DragValue::new(&mut self.radius).speed(0.1).range(0.00001..=1e6)); });
                        }
                        if operation == "cylinder" {
                            ui.horizontal(|ui| { ui.label("Height"); ui.add(egui::DragValue::new(&mut self.height).speed(0.1).range(0.00001..=1e6)); });
                            vector_ui(ui, "Axis direction", &mut self.normal);
                        }
                        if operation == "trim" {
                            vector_ui(ui, "Plane normal", &mut self.normal);
                            ui.checkbox(&mut self.keep_positive, "Keep the positive side of the plane");
                        }
                        if operation == "fillet" {
                            ui.label("Edge indices (zero-based, comma-separated; empty = all edges)");
                            ui.text_edit_singleline(&mut self.edges);
                        }
                        if self.operation >= 3 {
                            ui.label(format!("{} selected node(s)", selection.len()));
                            ui.weak("Difference uses the topmost, then leftmost selected node as the body. The other nodes are cutters.");
                        }
                        ui.weak("Creates an independent result with an exact B-rep and a mesh preview. Mesh inputs become faceted B-reps.");
                        if ui.add_enabled(doc.editable(), egui::Button::new("Run B-rep operation")).clicked() {
                            let records = if self.operation <= 2 { Ok((Vec::new(), None)) }
                                else { collect_records(doc, Some(selection)) };
                            let edges = parse_edges(&self.edges);
                            match records.and_then(|r| edges.map(|e| (r, e))).and_then(|(r,e)| graph_revision(&doc.graph).map(|rev| (r,e,rev))) {
                                Ok(((objects, document), edges, revision)) => {
                                    let parameters = json!({"origin": xyz(self.origin), "center":xyz(self.origin),
                                        "size":xyz(self.size), "direction":xyz(self.normal), "normal":xyz(self.normal),
                                        "radius":self.radius, "height":self.height,
                                        "keep":if self.keep_positive {"positive"} else {"negative"}, "edges":edges});
                                    let request = json!({"operation":operation,"objects":objects,"document":document,"parameters":parameters});
                                    let hide = if self.operation >= 3 { selection.iter().copied().collect() } else { Vec::new() };
                                    self.start(workspace, hide, JobContext::Brep(revision), move |cancel| bridge("brep", request, cancel).map(Outcome::Geometry));
                                }
                                Err(error) => self.status = error,
                            }
                        }
                    }
                });
                if let Some(job) = &self.job {
                    ui.horizontal(|ui| {
                        ui.spinner(); ui.label("Working…");
                        if ui.button("Cancel").clicked() { job.cancel.store(true, Ordering::Relaxed); }
                    });
                }
                if !self.status.is_empty() { ui.separator(); ui.label(&self.status); }
                if !self.warnings.is_empty() {
                    egui::ScrollArea::vertical().max_height(160.).show(ui, |ui| {
                        for warning in &self.warnings { ui.colored_label(egui::Color32::from_rgb(230,190,95), warning); }
                    });
                }
                ui.separator();
                ui.weak("GHX works in the base app. Rhino files and exact solids use the optional compatibility pack; binary GH uses the GH archive pack.");
            });
        self.open = open;
        new_selection.take()
    }

    fn start(
        &mut self,
        workspace: &str,
        hide: Vec<NodeId>,
        context: JobContext,
        work: impl FnOnce(Arc<AtomicBool>) -> Result<Outcome, String> + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work(worker_cancel));
        });
        self.job = Some(Job {
            receiver,
            cancel,
            workspace: workspace.into(),
            hide,
            context,
        });
        self.status = "Working…".into();
        self.warnings.clear();
    }

    fn poll(&mut self, doc: &mut Document, workspace: &str) -> Option<BTreeSet<NodeId>> {
        let result = match self.job.as_ref()?.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("CAD worker stopped unexpectedly.".into()),
        };
        let job = self.job.take()?;
        if let Ok(Outcome::Export(message, warnings)) = &result {
            self.status = message.clone();
            self.warnings = warnings.clone();
            return None;
        }
        if job.cancel.load(Ordering::Relaxed) {
            self.status = match &job.context {
                JobContext::Export(path) => format!("Cancellation requested. Check {}: an export may have completed before cancellation.", path.display()),
                _ => "Cancelled.".into(),
            };
            return None;
        }
        if let JobContext::Brep(revision) = job.context {
            if graph_revision(&doc.graph).ok() != Some(revision) {
                self.status = "Graph changed during the B-rep operation. Run again to use the current geometry.".into();
                return None;
            }
        }
        match result {
            Ok(Outcome::Export(message, warnings)) => {
                self.status = message;
                self.warnings = warnings;
                None
            }
            Ok(_) if job.workspace != workspace => {
                self.status = "Workspace changed during import. Return to the intended workspace and import again.".into();
                None
            }
            Ok(Outcome::Geometry(response)) => {
                self.warnings = response.warnings;
                let summary = response
                    .metrics
                    .as_ref()
                    .map(|m| {
                        format!(
                            " Exact volume: {:.6}; {} B-rep edges.",
                            m.get("volume").and_then(Json::as_f64).unwrap_or(0.),
                            m.get("edges").and_then(Json::as_u64).unwrap_or(0)
                        )
                    })
                    .unwrap_or_default();
                match insert_records(doc, response.objects, response.document, &job.hide) {
                    Ok(ids) => {
                        self.status = format!("Added {} CAD object(s).{summary}", ids.len());
                        Some(ids)
                    }
                    Err(error) => {
                        self.status = error;
                        None
                    }
                }
            }
            Ok(Outcome::Graph(report)) => {
                let imported_count = report.node_ids.len();
                self.warnings = report.warnings;
                match insert_graph(doc, report.ops) {
                    Ok(ids) => {
                        self.status = format!("Imported {imported_count} Grasshopper object(s), {} Mantis nodes including inputs.", ids.len());
                        Some(ids)
                    }
                    Err(error) => {
                        self.status = error;
                        None
                    }
                }
            }
            Err(error) => {
                self.status = error;
                None
            }
        }
    }
}

fn graph_revision(graph: &Graph) -> Result<u64, String> {
    struct Digest(std::collections::hash_map::DefaultHasher);
    impl Write for Digest {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.write(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut digest = Digest(std::collections::hash_map::DefaultHasher::new());
    serde_json::to_writer(&mut digest, graph).map_err(|e| e.to_string())?;
    Ok(digest.0.finish())
}

fn vector_ui(ui: &mut egui::Ui, label: &str, vector: &mut [f64; 3]) {
    ui.horizontal(|ui| {
        ui.label(label);
        for v in vector {
            ui.add(egui::DragValue::new(v).speed(0.1).range(-1e9..=1e9));
        }
    });
}
fn xyz(v: [f64; 3]) -> Json {
    json!({"x":v[0],"y":v[1],"z":v[2]})
}
fn parse_edges(text: &str) -> Result<Vec<usize>, String> {
    let values = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| format!("Invalid edge index: {s}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() > 10_000 {
        return Err("Select at most 10,000 edges.".into());
    }
    Ok(values)
}
fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}
fn default_export_path() -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let base = home.map(PathBuf::from).unwrap_or_default();
    let downloads = base.join("Downloads");
    (if downloads.is_dir() { downloads } else { base })
        .join("mantis-model.3dm")
        .to_string_lossy()
        .into()
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    if !path.is_file() {
        return Err(format!("File does not exist: {}", path.display()));
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    read_bounded(file, MAX_IO)
}
fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > limit {
        Err(format!(
            "CAD data exceeds the {} MiB limit.",
            limit / 1024 / 1024
        ))
    } else {
        Ok(data)
    }
}

fn import_file(path: &Path, cancel: Arc<AtomicBool>) -> Result<Outcome, String> {
    let ext = extension(path);
    match ext.as_str() {
        "ghx" => {
            let xml = String::from_utf8(read_file(path)?).map_err(|_| "GHX must be UTF-8 XML.")?;
            grasshopper::import_ghx(&xml).map(Outcome::Graph)
        }
        "gh" => {
            let mut command = Command::new(gh_converter()?);
            command.arg("decode").arg(path);
            let xml = run_process(command, Vec::new(), cancel)?;
            let xml =
                String::from_utf8(xml).map_err(|_| "GH archive converter returned invalid XML.")?;
            grasshopper::import_ghx(&xml).map(Outcome::Graph)
        }
        "3dm" | "step" | "stp" => bridge(
            if ext == "3dm" {
                "import_3dm"
            } else {
                "import_step"
            },
            json!({"path":path}),
            cancel,
        )
        .map(Outcome::Geometry),
        _ => Err("Choose a .3dm, .gh, .ghx, .step or .stp file.".into()),
    }
}

enum ExportPayload {
    Ghx(String),
    Geometry {
        objects: Vec<GeometryRecord>,
        document: Option<Json>,
    },
}
fn export_file(
    path: &Path,
    payload: ExportPayload,
    cancel: Arc<AtomicBool>,
) -> Result<Outcome, String> {
    if path.exists() {
        return Err("A file already exists at that path. Choose a new filename.".into());
    }
    match (extension(path).as_str(), payload) {
        ("ghx", ExportPayload::Ghx(xml)) => {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            write_new(path, xml.as_bytes())?;
            Ok(Outcome::Export(
                format!("Saved {}", path.display()),
                Vec::new(),
            ))
        }
        ("gh", ExportPayload::Ghx(xml)) => {
            let mut command = Command::new(gh_converter()?);
            command.arg("encode").arg(path);
            run_process(command, xml.into_bytes(), cancel)?;
            Ok(Outcome::Export(
                format!("Saved {}", path.display()),
                Vec::new(),
            ))
        }
        (ext @ ("3dm" | "step" | "stp"), ExportPayload::Geometry { objects, document }) => {
            let r = bridge(
                if ext == "3dm" {
                    "export_3dm"
                } else {
                    "export_step"
                },
                json!({"path":path,"objects":objects,"document":document,"overwrite":false}),
                cancel,
            )?;
            Ok(Outcome::Export(
                format!("Saved {} object(s) to {}", r.written, path.display()),
                r.warnings,
            ))
        }
        _ => Err("Use a .3dm, .ghx, .gh or .step filename.".into()),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error.to_string());
    }
    Ok(())
}

fn addon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = std::env::var_os("MANTIS_COMPAT_DIR") {
        roots.push(path.into());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            roots.push(parent.join("compat"));
            roots.push(parent.join("interop"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("MantisCAD").join("compat"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/mantis-cad/compat"));
    }
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../interop"));
    roots
}

fn bridge(action: &str, request: Json, cancel: Arc<AtomicBool>) -> Result<BridgeResponse, String> {
    let root = addon_roots().into_iter().find(|root| root.join("compat.py").is_file())
        .ok_or("Install the optional CAD compatibility pack to open .3dm/STEP files and run B-rep operations. See docs/INTEROP.md.")?;
    let python = std::env::var_os("MANTIS_COMPAT_PYTHON").map(PathBuf::from).or_else(|| {
        ["python/python.exe", ".venv/bin/python", ".venv/Scripts/python.exe"]
            .iter().map(|p| root.join(p)).find(|p| p.is_file())
    }).ok_or("The CAD compatibility pack needs its Python runtime. Run the pack's installer; see docs/INTEROP.md.")?;
    let mut command = Command::new(python);
    command.arg("-I").arg(root.join("compat.py")).arg(action);
    let input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let output = run_process(command, input, cancel)?;
    serde_json::from_slice(&output)
        .map_err(|e| format!("CAD engine returned an invalid response: {e}"))
}

fn gh_converter() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("MANTIS_GH_CONVERTER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err("MANTIS_GH_CONVERTER does not point to a file.".into());
    }
    let name = if cfg!(windows) {
        "mantis-gh-io.exe"
    } else {
        "mantis-gh-io"
    };
    for root in addon_roots() {
        for path in [root.join(name), root.join("grasshopper").join(name)] {
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err("Binary .gh files need the optional GH archive pack. GHX XML files open directly. See docs/INTEROP.md.".into())
}

fn run_process(
    mut command: Command,
    input: Vec<u8>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    if input.len() > MAX_IO {
        return Err("CAD request exceeds 64 MiB.".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW, keeping native UX quiet.
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start CAD engine: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("CAD input pipe unavailable")?;
    let stdout = child.stdout.take().ok_or("CAD output pipe unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("CAD diagnostic pipe unavailable")?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output_reader = std::thread::spawn(move || read_bounded(stdout, MAX_IO));
    let error_reader = std::thread::spawn(move || read_bounded(stderr, 1024 * 1024));
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            let _ = output_reader.join();
            let _ = error_reader.join();
            return Err(if cancel.load(Ordering::Relaxed) {
                "Cancelled."
            } else {
                "CAD operation exceeded two minutes."
            }
            .into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    };
    let written = writer.join().map_err(|_| "CAD writer stopped.")?;
    let output = output_reader.join().map_err(|_| "CAD reader stopped.")??;
    let error = error_reader
        .join()
        .map_err(|_| "CAD diagnostic reader stopped.")??;
    if !status.success() {
        let detail = String::from_utf8_lossy(&error);
        return Err(format!(
            "CAD engine failed: {}",
            if detail.trim().is_empty() {
                status.to_string()
            } else {
                detail.trim().into()
            }
        ));
    }
    written.map_err(|e| format!("Cannot send CAD request: {e}"))?;
    Ok(output)
}

fn collect_records(
    doc: &mut Document,
    selected: Option<&BTreeSet<NodeId>>,
) -> Result<(Vec<GeometryRecord>, Option<Json>), String> {
    doc.evaluate();
    if selected.is_some_and(BTreeSet::is_empty) {
        return Err("Select geometry nodes first, or turn off Selected geometry only.".into());
    }
    let graph = doc.display_graph();
    let mut nodes: Vec<_> = graph
        .nodes
        .values()
        .filter(|n| {
            selected
                .map(|s| s.contains(&n.id))
                .unwrap_or_else(|| n.preview())
        })
        .collect();
    nodes.sort_by(|a, b| {
        a.pos
            .1
            .total_cmp(&b.pos.1)
            .then(a.pos.0.total_cmp(&b.pos.0))
            .then(a.id.cmp(&b.id))
    });
    let mut objects = Vec::new();
    let mut budget = MAX_IO;
    let document_text = inherited_document(graph, nodes.iter().map(|n| n.id))?;
    let document = document_text
        .map(|text| {
            spend_geometry(&mut budget, text.len())?;
            serde_json::from_str(text).map_err(|e| format!("Invalid CAD document metadata: {e}"))
        })
        .transpose()?;
    for node in nodes {
        if let Some(error) = doc.last_eval.errors.get(&node.id) {
            if selected.is_some() {
                return Err(format!(
                    "Selected {} cannot be evaluated: {error}",
                    node.type_name
                ));
            }
            continue;
        }
        if node.type_name == "imported_geometry" {
            if let Some(data) = node.params.get("data").and_then(ParamValue::as_text) {
                spend_geometry(&mut budget, data.len())?;
                objects.push(GeometryRecord::from_json(data)?);
                continue;
            }
        }
        if let Some(values) = doc.last_eval.outputs.get(&node.id) {
            for value in values {
                collect_value(value, &node.type_name, &mut objects, &mut budget, 0)?;
            }
        }
    }
    if objects.is_empty() {
        return Err("No geometry to export or operate on.".into());
    }
    let mut bytes = 0usize;
    for record in &objects {
        bytes += record.to_json()?.len();
        if bytes > MAX_IO {
            return Err("Selected geometry exceeds 64 MiB.".into());
        }
    }
    Ok((objects, document))
}

/// Units and definition tables follow geometry through ordinary graph
/// transforms; the exact geometry source itself is never reused after a transform.
fn inherited_document(
    graph: &Graph,
    nodes: impl Iterator<Item = NodeId>,
) -> Result<Option<&str>, String> {
    let mut incoming = BTreeMap::<NodeId, Vec<NodeId>>::new();
    for edge in &graph.edges {
        incoming.entry(edge.to.0).or_default().push(edge.from.0);
    }
    let mut stack: Vec<_> = nodes.collect();
    let mut seen = BTreeSet::new();
    let mut document: Option<&str> = None;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = graph.nodes.get(&id) else {
            continue;
        };
        let metadata_node = node
            .params
            .get("__cad_document_node")
            .and_then(ParamValue::as_text)
            .and_then(NodeId::from_hex)
            .and_then(|id| graph.nodes.get(&id));
        let text = metadata_node
            .unwrap_or(node)
            .params
            .get("__cad_document")
            .and_then(ParamValue::as_text);
        if node.params.contains_key("__cad_document_node") && text.is_none() {
            return Err("The shared CAD source document was removed. Undo its removal before exporting preserved objects or running B-rep operations.".into());
        }
        if let Some(text) = text {
            if document.is_some_and(|first| first != text) {
                return Err("The selection contains different imported document settings. Export one source document at a time to preserve units, materials and block definitions.".into());
            }
            document = Some(text);
        }
        if let Some(sources) = incoming.get(&id) {
            stack.extend(sources);
        }
    }
    Ok(document)
}

fn spend_geometry(budget: &mut usize, bytes: usize) -> Result<(), String> {
    *budget = budget
        .checked_sub(bytes)
        .ok_or("Geometry selection exceeds 64 MiB. Select fewer objects.")?;
    Ok(())
}
fn collect_value(
    value: &Value,
    name: &str,
    out: &mut Vec<GeometryRecord>,
    budget: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 || out.len() >= 10_000 {
        return Err("Geometry selection exceeds the depth or 10,000-object limit.".into());
    }
    if let Value::List(values) = value {
        for value in values {
            collect_value(value, name, out, budget, depth + 1)?;
        }
    } else {
        // Reserve a conservative serialized geometry estimate BEFORE turning
        // shared Arc meshes into owned records. Repeat must not multiply a
        // large shared mesh into unbounded allocations before the limit check.
        let bytes = match value {
            Value::Mesh(mesh) => mesh
                .positions
                .len()
                .saturating_add(mesh.normals.len())
                .saturating_mul(128)
                .saturating_add(mesh.indices.len().saturating_mul(48)),
            Value::Curve(curve) => match &**curve {
                mantis_kernel::Curve::Polyline { points, .. } => points.len().saturating_mul(128),
                mantis_kernel::Curve::Nurbs(n) => n
                    .control_points
                    .len()
                    .saturating_mul(192)
                    .saturating_add(n.knots.len().saturating_mul(32)),
                _ => 1024,
            },
            Value::Vector(_) => 128,
            _ => 0,
        };
        spend_geometry(budget, bytes.saturating_add(name.len()).saturating_add(256))?;
        if let Some(record) = GeometryRecord::from_value(value, name.into()) {
            out.push(record);
        }
    }
    Ok(())
}

fn insert_records(
    doc: &mut Document,
    records: Vec<GeometryRecord>,
    document: Option<Json>,
    hide: &[NodeId],
) -> Result<BTreeSet<NodeId>, String> {
    if records.is_empty() {
        return Err("No supported geometry was returned.".into());
    }
    if records.len() > 10_000 {
        return Err("Import exceeds 10,000 objects.".into());
    }
    let metadata = document
        .map(|d| serde_json::to_string(&d))
        .transpose()
        .map_err(|e| e.to_string())?;
    if metadata.as_ref().is_some_and(|m| m.len() > MAX_IO) {
        return Err("CAD document metadata exceeds 64 MiB.".into());
    }
    let left = doc
        .graph
        .nodes
        .values()
        .map(|n| n.pos.0 + 260.)
        .fold(0., f32::max);
    let mut ops = Vec::new();
    let mut selection = BTreeSet::new();
    let mut total = metadata.as_ref().map_or(0, String::len);
    // One ordinary metadata panel retains original document tables and bytes.
    // Geometry nodes refer to it, so thousands of objects share one source.
    let metadata_id = metadata.map(|metadata| {
        let id = new_node_id();
        ops.push(GraphOp::AddNode {
            id,
            type_name: "panel".into(),
            pos: (left, -150.),
        });
        ops.push(GraphOp::SetParam {
            id,
            key: "text".into(),
            value: ParamValue::Text("CAD source document (shared metadata)".into()),
        });
        ops.push(GraphOp::SetParam {
            id,
            key: "__cad_document".into(),
            value: ParamValue::Text(metadata),
        });
        ops.push(GraphOp::SetParam {
            id,
            key: "__preview".into(),
            value: ParamValue::Bool(false),
        });
        id
    });
    for (i, record) in records.into_iter().enumerate() {
        let json = record.to_json()?;
        total += json.len() + record.name.len() + 256;
        if total > MAX_IO {
            return Err("Import exceeds 64 MiB of geometry.".into());
        }
        let id = new_node_id();
        selection.insert(id);
        ops.push(GraphOp::AddNode {
            id,
            type_name: "imported_geometry".into(),
            pos: (left + (i / 20) as f32 * 260., (i % 20) as f32 * 130.),
        });
        ops.push(GraphOp::SetParam {
            id,
            key: "data".into(),
            value: ParamValue::Text(json),
        });
        if !record.name.is_empty() {
            ops.push(GraphOp::SetParam {
                id,
                key: "label".into(),
                value: ParamValue::Text(record.name),
            });
        }
        if let Some(metadata_id) = metadata_id {
            ops.push(GraphOp::SetParam {
                id,
                key: "__cad_document_node".into(),
                value: ParamValue::Text(metadata_id.to_hex()),
            });
        }
    }
    for id in hide {
        if doc.graph.nodes.contains_key(id) {
            ops.push(GraphOp::SetParam {
                id: *id,
                key: "__preview".into(),
                value: ParamValue::Bool(false),
            });
        }
    }
    doc.end_gesture();
    doc.apply_ops(ops)?;
    Ok(selection)
}

fn insert_graph(doc: &mut Document, ops: Vec<GraphOp>) -> Result<BTreeSet<NodeId>, String> {
    if ops.len() > 100_000 {
        return Err("Imported graph exceeds 100,000 operations.".into());
    }
    let ids: BTreeMap<_, _> = ops
        .iter()
        .filter_map(|op| match op {
            GraphOp::AddNode { id, .. } => Some((*id, new_node_id())),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return Err("No supported Grasshopper nodes to import.".into());
    }
    let left = doc
        .graph
        .nodes
        .values()
        .map(|n| n.pos.0 + 260.)
        .fold(0., f32::max);
    let positions = import_positions(&ops, &doc.registry)?;
    let map = |id: NodeId| {
        ids.get(&id)
            .copied()
            .ok_or_else(|| "Import references an unknown node.".to_string())
    };
    let mut remapped = Vec::with_capacity(ops.len());
    for op in ops {
        let op = match op {
            GraphOp::AddNode { id, type_name, .. } => {
                let pos = positions[&id];
                GraphOp::AddNode {
                    id: map(id)?,
                    type_name,
                    pos: (pos.0 + left, pos.1),
                }
            }
            GraphOp::SetParam { id, key, value } => GraphOp::SetParam {
                id: map(id)?,
                key,
                value,
            },
            GraphOp::Connect { from, to } => GraphOp::Connect {
                from: (map(from.0)?, from.1),
                to: (map(to.0)?, to.1),
            },
            _ => {
                return Err("Imported definitions may only add nodes, parameters and wires.".into())
            }
        };
        if !op.is_finite() {
            return Err("Import contains a non-finite value.".into());
        }
        remapped.push(op);
    }
    // Validate internally too: repeated node IDs are not silently overwritten.
    let mut isolated = Graph::new();
    isolated
        .apply_all(&remapped)
        .map_err(|(_, e)| e.to_string())?;
    doc.end_gesture();
    doc.apply_ops(remapped)?;
    Ok(ids.into_values().collect())
}

/// GH sliders are much shorter than Mantis' embedded controls. Preserve the
/// relative columns/order while reserving enough space for readable widgets.
fn import_positions(
    ops: &[GraphOp],
    registry: &mantis_graph::Registry,
) -> Result<BTreeMap<NodeId, (f32, f32)>, String> {
    let mut nodes: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            GraphOp::AddNode { id, type_name, pos } => Some((*id, type_name, *pos)),
            _ => None,
        })
        .collect();
    if nodes
        .iter()
        .any(|(_, _, p)| !p.0.is_finite() || !p.1.is_finite() || p.0.abs() > 1e9 || p.1.abs() > 1e9)
    {
        return Err("Imported node position is outside the supported range.".into());
    }
    nodes.sort_by(|a, b| {
        a.2 .1
            .total_cmp(&b.2 .1)
            .then(a.2 .0.total_cmp(&b.2 .0))
            .then(a.0.cmp(&b.0))
    });
    let min_x = nodes.iter().map(|n| n.2 .0).fold(f32::INFINITY, f32::min);
    let min_y = nodes.iter().map(|n| n.2 .1).fold(f32::INFINITY, f32::min);
    let mut skyline = BTreeMap::<i64, f32>::new();
    let mut result = BTreeMap::new();
    for (id, ty, pos) in nodes {
        let x = (pos.0 - min_x) * 1.5;
        let mut y = (pos.1 - min_y) * 1.5;
        let start = (x / 120.).floor() as i64;
        let end = ((x + 210.) / 120.).floor() as i64;
        for column in start..=end {
            y = y.max(skyline.get(&column).copied().unwrap_or(0.));
        }
        let ports = registry
            .get(ty)
            .map(|c| c.inputs().len().max(c.outputs().len()))
            .unwrap_or(1);
        let height =
            (60. + ports as f32 * 20. + if ty == "number_slider" { 42. } else { 0. }).max(110.);
        for column in start..=end {
            skyline.insert(column, y + height + 24.);
        }
        result.insert(id, (x, y));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_graph::geometry::{GeometryData, GeometrySource};
    use mantis_kernel::{Mesh, Plane};

    fn doc() -> Document {
        Document::new(Identity::from_secret_hex("CAD test", &"21".repeat(32)).unwrap())
    }
    fn record() -> GeometryRecord {
        GeometryRecord {
            name: "solid".into(),
            layer: "parts".into(),
            geometry: GeometryData::Mesh {
                mesh: Mesh::box_mesh(&Plane::world_xy(), 2., 3., 4.),
            },
            source: Some(GeometrySource {
                format: "ocp-brep".into(),
                data: "preserved".into(),
                preview_units: None,
            }),
        }
    }
    #[test]
    fn cad_import_is_atomic_undoable_and_preserves_exact_source() {
        let mut d = doc();
        let source = record();
        let ids = insert_records(
            &mut d,
            vec![source.clone()],
            Some(json!({"units":"Millimeters"})),
            &[],
        )
        .unwrap();
        let (export, metadata) = collect_records(&mut d, Some(&ids)).unwrap();
        assert_eq!(export, vec![source]);
        assert_eq!(metadata.unwrap()["units"], "Millimeters");
        d.undo_pending().unwrap();
        assert!(d.graph.nodes.is_empty());
        d.redo_pending().unwrap();
        assert_eq!(collect_records(&mut d, Some(&ids)).unwrap().0.len(), 1);
        let before = d.graph.clone();
        let mut bad = record();
        if let GeometryData::Mesh { mesh } = &mut bad.geometry {
            mesh.indices[0][0] = u32::MAX;
        }
        assert!(insert_records(&mut d, vec![record(), bad], None, &[]).is_err());
        assert_eq!(d.graph, before);
    }
    #[test]
    fn repeated_graph_import_remaps_ids_and_keeps_existing_graph() {
        let mut d = doc();
        let original = NodeId(1);
        let ops = vec![
            GraphOp::AddNode {
                id: original,
                type_name: "number_slider".into(),
                pos: (0., 0.),
            },
            GraphOp::SetParam {
                id: original,
                key: "value".into(),
                value: ParamValue::Number(4.),
            },
        ];
        let first = insert_graph(&mut d, ops.clone()).unwrap();
        let second = insert_graph(&mut d, ops).unwrap();
        assert!(first.is_disjoint(&second));
        assert_eq!(d.graph.nodes.len(), 2);
        d.undo_pending().unwrap();
        assert_eq!(d.graph.nodes.len(), 1);
    }
    #[test]
    fn changed_geometry_does_not_reuse_stale_cad_source() {
        let mut d = doc();
        let original = insert_records(&mut d, vec![record()], None, &[]).unwrap();
        let moved = crate::commands::execute(&mut d, &original, "Move 10 0 0")
            .unwrap()
            .selection;
        let records = collect_records(&mut d, Some(&moved)).unwrap().0;
        assert!(records[0].source.is_none());
        let GeometryData::Mesh { mesh } = &records[0].geometry else {
            panic!()
        };
        assert_eq!(mesh.bbox().min.x, 10.);
    }
    #[test]
    fn data_limits_and_new_file_writes_protect_input() {
        assert!(read_bounded(&b"12345"[..], 4).is_err());
        assert_eq!(parse_edges("1, 2 3").unwrap(), vec![1, 2, 3]);
        assert!(parse_edges("1 -2").is_err());
        let path = std::env::temp_dir().join(format!("mantis-cad-test-{}", new_node_id().to_hex()));
        write_new(&path, b"original").unwrap();
        assert!(write_new(&path, b"changed").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stale_brep_results_and_completed_export_cancellation_are_handled() {
        let mut d = doc();
        let revision = graph_revision(&d.graph).unwrap();
        insert_records(&mut d, vec![record()], None, &[]).unwrap();
        let before = d.graph.clone();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(Outcome::Geometry(BridgeResponse {
                objects: vec![record()],
                ..Default::default()
            })))
            .unwrap();
        let mut ui = CadIo {
            job: Some(Job {
                receiver,
                cancel: Arc::new(AtomicBool::new(false)),
                workspace: "one".into(),
                hide: vec![],
                context: JobContext::Brep(revision),
            }),
            ..Default::default()
        };
        assert!(ui.poll(&mut d, "one").is_none());
        assert_eq!(d.graph, before);
        assert!(ui.status.contains("Graph changed"));
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(Outcome::Export("Saved file.3dm".into(), vec![])))
            .unwrap();
        ui.job = Some(Job {
            receiver,
            cancel: Arc::new(AtomicBool::new(true)),
            workspace: "one".into(),
            hide: vec![],
            context: JobContext::Export("file.3dm".into()),
        });
        ui.poll(&mut d, "one");
        assert_eq!(ui.status, "Saved file.3dm");
    }
    #[test]
    fn metadata_is_shared_and_geometry_budget_precedes_deep_copy() {
        let mut d = doc();
        let metadata = json!({"units":2,"source_3dm_base64":"A".repeat(100_000)});
        let ids = insert_records(&mut d, vec![record(); 100], Some(metadata.clone()), &[]).unwrap();
        assert_eq!(
            d.graph
                .nodes
                .values()
                .filter(|n| n.params.contains_key("__cad_document"))
                .count(),
            1
        );
        let one = BTreeSet::from([*ids.iter().next_back().unwrap()]);
        assert_eq!(
            collect_records(&mut d, Some(&one)).unwrap().1,
            Some(metadata)
        );
        let mut out = Vec::new();
        let mut budget = 1024;
        assert!(collect_value(&record().value(), "box", &mut out, &mut budget, 0).is_err());
        assert!(out.is_empty());
    }
    #[test]
    fn imported_short_grasshopper_sliders_get_readable_spacing() {
        let ops = (1..=3)
            .map(|i| GraphOp::AddNode {
                id: NodeId(i),
                type_name: "number_slider".into(),
                pos: (0., i as f32 * 25.),
            })
            .collect::<Vec<_>>();
        let positions = import_positions(&ops, &mantis_graph::Registry::standard()).unwrap();
        assert!(positions[&NodeId(2)].1 - positions[&NodeId(1)].1 >= 140.);
        assert!(positions[&NodeId(3)].1 - positions[&NodeId(2)].1 >= 140.);
    }
    #[test]
    fn missing_document_tables_are_reported_instead_of_losing_units() {
        let mut d = doc();
        let ids = insert_records(&mut d, vec![record()], Some(json!({"units":8})), &[]).unwrap();
        let metadata = d
            .graph
            .nodes
            .values()
            .find(|n| n.params.contains_key("__cad_document"))
            .unwrap()
            .id;
        d.apply_op(GraphOp::RemoveNode { id: metadata }).unwrap();
        assert!(collect_records(&mut d, Some(&ids))
            .unwrap_err()
            .contains("source document was removed"));
        d.undo_pending().unwrap();
        assert!(collect_records(&mut d, Some(&ids)).is_ok());
    }
    #[test]
    fn graph_transforms_keep_document_units_without_reusing_exact_shape() {
        let mut d = doc();
        let ids = insert_records(&mut d, vec![record()], Some(json!({"units":8})), &[]).unwrap();
        let moved = crate::commands::execute(&mut d, &ids, "Move 10 0 0")
            .unwrap()
            .selection;
        let (objects, metadata) = collect_records(&mut d, Some(&moved)).unwrap();
        assert_eq!(metadata.unwrap()["units"], 8);
        assert!(objects[0].source.is_none());
    }
}
