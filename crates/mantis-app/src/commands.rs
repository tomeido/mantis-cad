//! Rhino-style command recipes, compiled into ordinary editable graph nodes.
//! A recipe is evaluated before committing and is one undoable document action.

use crate::state::Document;
use crate::util::new_node_id;
use mantis_graph::{Evaluator, GraphOp, NodeId, ParamValue, Value};
use std::collections::{BTreeMap, BTreeSet};

struct CommandSpec {
    name: &'static str,
    component: &'static str,
    example: &'static str,
    help: &'static str,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "MeshBooleanUnion", component: "mesh_boolean_union", example: "MeshBooleanUnion",
        help: "two selected closed meshes; union, tolerance 1e-7",
    },
    CommandSpec {
        name: "MeshBooleanDifference", component: "mesh_boolean_difference", example: "MeshBooleanDifference",
        help: "two selected closed meshes; subtract bottom/right node from top/left node",
    },
    CommandSpec {
        name: "MeshBooleanIntersection", component: "mesh_boolean_intersection", example: "MeshBooleanIntersection",
        help: "two selected closed meshes; shared solid volume",
    },
    CommandSpec {
        name: "MeshTrimPlane", component: "mesh_trim_plane", example: "MeshTrimPlane 0 0 5 0 0 1",
        help: "selected closed meshes: plane origin x y z, normal x y z, optional keep-positive 0/1; capped result",
    },
    CommandSpec {
        name: "MeshSplitPlane", component: "mesh_split_plane", example: "MeshSplitPlane 0 0 5 0 0 1",
        help: "selected closed meshes: plane origin x y z, normal x y z; negative and positive capped outputs",
    },
    CommandSpec {
        name: "Point",
        component: "point_xyz",
        example: "Point 0 0 0",
        help: "x y z",
    },
    CommandSpec {
        name: "Line",
        component: "line",
        example: "Line 0 0 0 10 0 0",
        help: "start x y z, end x y z",
    },
    CommandSpec {
        name: "Polyline",
        component: "polyline",
        example: "Polyline 0 0 0 10 0 0 10 10 0",
        help: "two or more x y z points; open polyline",
    },
    CommandSpec {
        name: "Curve",
        component: "nurbs_curve",
        example: "Curve 0 0 0 5 5 0 10 0 0",
        help: "two or more control points (x y z); NURBS control polygon",
    },
    CommandSpec {
        name: "Circle",
        component: "circle",
        example: "Circle 5",
        help: "radius; world XY at origin",
    },
    CommandSpec {
        name: "Arc",
        component: "arc",
        example: "Arc 5 0 180",
        help: "radius, start and end angles in degrees; XY",
    },
    CommandSpec {
        name: "Rectangle",
        component: "rectangle",
        example: "Rectangle 10 20",
        help: "width height; world XY at origin",
    },
    CommandSpec {
        name: "Box",
        component: "box_mesh",
        example: "Box 10 20 30",
        help: "x y z dimensions; world XY at origin",
    },
    CommandSpec {
        name: "Sphere",
        component: "sphere",
        example: "Sphere 5",
        help: "radius; center at origin",
    },
    CommandSpec {
        name: "Cylinder",
        component: "cylinder",
        example: "Cylinder 5 10",
        help: "radius height; world Z axis",
    },
    CommandSpec {
        name: "Cone",
        component: "cone",
        example: "Cone 5 10",
        help: "radius height; world Z axis",
    },
    CommandSpec {
        name: "Torus",
        component: "torus",
        example: "Torus 5 1",
        help: "major radius, tube radius; world XY",
    },
    CommandSpec {
        name: "ExtrudeCrv",
        component: "extrude",
        example: "ExtrudeCrv 10",
        help: "selected curve(s): height along Z, or dx dy dz; closed planar profiles are capped",
    },
    CommandSpec {
        name: "Revolve",
        component: "revolve",
        example: "Revolve 360",
        help: "selected curve(s): degrees about world Z; default 360",
    },
    CommandSpec {
        name: "Loft",
        component: "loft",
        example: "Loft",
        help: "selected curves, ordered by node position top to bottom, then left to right",
    },
    CommandSpec {
        name: "Pipe",
        component: "pipe",
        example: "Pipe 0.5",
        help: "selected curve(s): tube radius; open rails get flat end caps",
    },
    CommandSpec {
        name: "PlanarSrf",
        component: "planar_srf",
        example: "PlanarSrf",
        help: "selected closed planar curve(s) to mesh",
    },
    CommandSpec {
        name: "Move",
        component: "move",
        example: "Move 10 0 0",
        help: "selected geometry: dx dy dz; source preview hidden",
    },
    CommandSpec {
        name: "Copy",
        component: "move",
        example: "Copy 10 0 0",
        help: "selected geometry: dx dy dz; source preview retained",
    },
    CommandSpec {
        name: "Rotate",
        component: "rotate",
        example: "Rotate 45",
        help: "selected geometry: degrees about world Z at origin",
    },
    CommandSpec {
        name: "Scale",
        component: "scale",
        example: "Scale 2",
        help: "selected geometry: uniform factor about origin",
    },
    CommandSpec {
        name: "Mirror",
        component: "mirror",
        example: "Mirror yz",
        help: "selected geometry: xy, xz or yz plane through origin (default yz)",
    },
    CommandSpec {
        name: "ArrayLinear",
        component: "array_linear",
        example: "ArrayLinear 5 10 0 0",
        help: "selected geometry: count, step dx dy dz; count includes original",
    },
    CommandSpec {
        name: "ArrayPolar",
        component: "array_polar",
        example: "ArrayPolar 6 360",
        help: "selected geometry: count, optional sweep degrees about world Z",
    },
    CommandSpec {
        name: "Divide",
        component: "divide_curve",
        example: "Divide 10",
        help: "selected curve(s): segment count",
    },
    CommandSpec {
        name: "EvaluateCurve",
        component: "eval_curve",
        example: "EvaluateCurve 0.5",
        help: "selected curve(s): normalized parameter 0..1",
    },
    CommandSpec {
        name: "EndPoints",
        component: "end_points",
        example: "EndPoints",
        help: "selected curve(s): start and end points",
    },
    CommandSpec {
        name: "Reverse",
        component: "reverse_curve",
        example: "Reverse",
        help: "selected curve(s): reverse direction",
    },
    CommandSpec {
        name: "Length",
        component: "curve_length",
        example: "Length",
        help: "selected curve(s): length",
    },
    CommandSpec {
        name: "Area",
        component: "area",
        example: "Area",
        help: "selected mesh(es): surface area (use PlanarSrf for closed curves)",
    },
    CommandSpec {
        name: "Volume",
        component: "volume",
        example: "Volume",
        help: "selected mesh(es): signed volume; meaningful for closed meshes",
    },
    CommandSpec {
        name: "BoundingBox",
        component: "bbox",
        example: "BoundingBox",
        help: "selected geometry: minimum and maximum coordinates",
    },
    CommandSpec {
        name: "Series",
        component: "series",
        example: "Series 0 1 10",
        help: "start step count; number list",
    },
    CommandSpec {
        name: "Range",
        component: "range",
        example: "Range 0 1 10",
        help: "start end steps; steps + 1 numbers",
    },
    CommandSpec {
        name: "Random",
        component: "random",
        example: "Random 0 10 10 1",
        help: "min max count seed; deterministic number list",
    },
];

#[derive(Default)]
pub struct CommandPalette {
    pub open: bool,
    input: String,
    error: String,
    focus: bool,
}

impl CommandPalette {
    pub fn show(&mut self) {
        self.open = true;
        self.focus = true;
        self.error.clear();
    }

    pub fn shortcut(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::K,
            ))
        }) {
            self.show();
        }
    }

    /// Return the completed command's output and new selected nodes.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        doc: &mut Document,
        selection: &BTreeSet<NodeId>,
    ) -> Option<CommandResult> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut run = false;
        egui::Window::new("Commands · Ctrl / Cmd + K")
            .id(egui::Id::new("mantis_commands"))
            .open(&mut open).default_width(560.0).resizable(true)
            .show(ctx, |ui| {
                ui.label("Type a command and numbers, then Enter. Click an example to edit it.");
                ui.weak("Coordinates use world axes. Angles are degrees. Numbers accept commas or spaces.");
                ui.horizontal(|ui| {
                    let response = ui.add(egui::TextEdit::singleline(&mut self.input)
                        .desired_width(410.0).hint_text("Circle 5"));
                    if std::mem::take(&mut self.focus) { response.request_focus(); }
                    run |= (response.has_focus() || response.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    run |= ui.add_enabled(doc.editable(), egui::Button::new("Run")).clicked();
                });
                if !doc.editable() { ui.colored_label(egui::Color32::LIGHT_RED, "History is read-only. Return to the latest block to run commands."); }
                ui.weak(format!("{} selected node(s). Operations connect to their first output; Shift-click nodes for multiple selection.", selection.len()));
                if !self.error.is_empty() { ui.colored_label(egui::Color32::LIGHT_RED, &self.error); }
                ui.separator();
                let q = normalize(self.input.split_whitespace().next().unwrap_or(""));
                egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    let mut matches = 0;
                    for spec in COMMANDS.iter().filter(|s| q.is_empty() || normalize(s.name).contains(&q) || normalize(s.component).contains(&q)) {
                        matches += 1;
                        ui.horizontal(|ui| {
                            if ui.selectable_label(false, egui::RichText::new(spec.example).monospace()).clicked() {
                                self.input = spec.example.into();
                                self.focus = true;
                                self.error.clear();
                            }
                        });
                        ui.weak(spec.help);
                        ui.add_space(5.0);
                    }
                    if matches == 0 { ui.weak("No command found. Clear the field to browse commands."); }
                });
                ui.separator();
                ui.weak("Each command builds an editable Grasshopper-style graph and takes one Undo step. Select a node to inspect inputs and outputs. Right-click the graph for all components.");
            });
        self.open = open;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
        }
        if run && self.open {
            match execute(doc, selection, &self.input) {
                Ok(result) => {
                    self.open = false;
                    return Some(result);
                }
                Err(error) => {
                    self.error = error;
                    self.focus = true;
                }
            }
        }
        None
    }
}

pub struct CommandResult {
    pub selection: BTreeSet<NodeId>,
    pub message: String,
}

/// Export the same preview-enabled meshes shown by the viewport. Appending
/// meshes rebases triangle indices, so multiple objects remain valid OBJ.
pub fn visible_mesh_obj(doc: &mut Document) -> Result<String, String> {
    doc.evaluate();
    let mut merged = mantis_kernel::Mesh::new();
    for item in crate::viewport::collect_scene(doc.display_graph(), &doc.last_eval) {
        if let crate::viewport::SceneGeom::Mesh(mesh) = item.geom {
            merged.append(&mesh);
        }
    }
    if merged.triangle_count() == 0 {
        return Err(
            "No visible meshes to export. Create a solid or surface, and enable its preview."
                .into(),
        );
    }
    Ok(format!("# MantisCAD visible meshes\n{}", merged.to_obj()))
}

#[cfg(target_arch = "wasm32")]
pub fn download_obj(payload: &str) -> Result<(), String> {
    use wasm_bindgen::{closure::Closure, JsCast as _, JsValue};
    let window = web_sys::window().ok_or("Browser window unavailable")?;
    let document = window.document().ok_or("Browser document unavailable")?;
    let body = document.body().ok_or("Browser document body unavailable")?;
    let anchor = document
        .create_element("a")
        .map_err(|_| "Cannot create download")?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| "Download unavailable")?;
    anchor.set_download("mantis-model.obj");
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(payload));
    let blob =
        web_sys::Blob::new_with_str_sequence(&parts).map_err(|_| "Cannot create OBJ download")?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|_| "Cannot create download URL")?;
    anchor.set_href(&url);
    if body.append_child(&anchor).is_err() {
        let _ = web_sys::Url::revoke_object_url(&url);
        return Err("Cannot attach download link".into());
    }
    // Give the browser time to consume the Blob, then release its backing
    // memory. once_into_js also releases the captured URL after invocation.
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 30_000)
        .is_err()
    {
        let _ = revoke
            .unchecked_ref::<js_sys::Function>()
            .call0(&JsValue::NULL);
        anchor.remove();
        return Err("Cannot schedule download cleanup".into());
    }
    anchor.click();
    anchor.remove();
    Ok(())
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn parse(input: &str) -> Result<(&'static CommandSpec, Vec<&str>), String> {
    let mut words = input
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty());
    let name = normalize(
        words
            .next()
            .ok_or("Enter a command, for example Circle 5")?,
    );
    let spec = COMMANDS
        .iter()
        .find(|s| normalize(s.name) == name || normalize(s.component) == name)
        .ok_or_else(|| {
            "Unknown command. Clear the field to browse; try Circle 5 or Box 10 20 30.".to_string()
        })?;
    Ok((spec, words.collect()))
}

#[derive(Default)]
struct Recipe {
    ops: Vec<GraphOp>,
    added: Vec<NodeId>,
}

impl Recipe {
    fn add(&mut self, component: &str) -> NodeId {
        let id = new_node_id();
        self.added.push(id);
        self.ops.push(GraphOp::AddNode {
            id,
            type_name: component.into(),
            pos: (0.0, 0.0),
        });
        id
    }
    fn param(&mut self, id: NodeId, key: &str, value: ParamValue) {
        self.ops.push(GraphOp::SetParam {
            id,
            key: key.into(),
            value,
        });
    }
    fn wire(&mut self, from: NodeId, to: NodeId, port: u16) {
        self.ops.push(GraphOp::Connect {
            from: (from, 0),
            to: (to, port),
        });
    }
    fn number(&mut self, to: NodeId, port: u16, value: f64, label: &str) {
        let id = self.add("number_slider");
        let bound = (value.abs() * 2.0).max(10.0);
        self.param(id, "value", ParamValue::Number(value));
        self.param(id, "min", ParamValue::Number(-bound));
        self.param(id, "max", ParamValue::Number(bound));
        self.param(id, "label", ParamValue::Text(label.into()));
        self.wire(id, to, port);
    }
    fn vector(&mut self, to: NodeId, port: u16, v: &[f64], label: &str) {
        let id = self.add("point_xyz");
        self.param(id, "__preview", ParamValue::Bool(false));
        self.param(id, "label", ParamValue::Text(label.into()));
        for (i, value) in v.iter().enumerate() {
            if *value != 0.0 {
                self.number(id, i as u16, *value, ["x", "y", "z"][i]);
            }
        }
        self.wire(id, to, port);
    }
    fn merge(&mut self, ids: &[NodeId]) -> NodeId {
        let mut source = ids[0];
        for id in &ids[1..] {
            let merge = self.add("merge");
            self.wire(source, merge, 0);
            self.wire(*id, merge, 1);
            self.param(merge, "__preview", ParamValue::Bool(false));
            source = merge;
        }
        source
    }
    fn layout(&mut self, doc: &Document) {
        let added: BTreeSet<_> = self.added.iter().copied().collect();
        let mut depths: BTreeMap<NodeId, usize> = self.added.iter().map(|id| (*id, 0)).collect();
        for _ in 0..self.added.len() {
            let mut changed = false;
            for op in &self.ops {
                if let GraphOp::Connect { from, to } = op {
                    if added.contains(&from.0) && added.contains(&to.0) {
                        let next = depths[&from.0] + 1;
                        if next > depths[&to.0] {
                            depths.insert(to.0, next);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let left = doc
            .graph
            .nodes
            .values()
            .map(|n| n.pos.0 + 240.0)
            .fold(0.0_f32, f32::max);
        let mut rows: BTreeMap<usize, usize> = BTreeMap::new();
        for op in &mut self.ops {
            if let GraphOp::AddNode { id, pos, .. } = op {
                let depth = depths[id];
                let row = rows.entry(depth).or_default();
                *pos = (left + depth as f32 * 240.0, *row as f32 * 155.0);
                *row += 1;
            }
        }
    }
}

pub fn execute(
    doc: &mut Document,
    selection: &BTreeSet<NodeId>,
    input: &str,
) -> Result<CommandResult, String> {
    if !doc.editable() {
        return Err("History is read-only; return to the latest block first.".into());
    }
    let (spec, args) = parse(input)?;
    let arity = |allowed: &[usize]| -> Result<(), String> {
        if allowed.contains(&args.len()) {
            Ok(())
        } else {
            Err(format!("Usage: {} — {}", spec.example, spec.help))
        }
    };
    let nums = if spec.name == "Mirror" {
        arity(&[0, 1])?;
        if args
            .first()
            .is_some_and(|v| !["xy", "xz", "yz"].contains(&v.to_ascii_lowercase().as_str()))
        {
            return Err("Usage: Mirror xy, Mirror xz or Mirror yz".into());
        }
        Vec::new()
    } else {
        args.iter()
            .map(|v| {
                v.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && n.abs() <= 1.0e9)
                    .ok_or_else(|| format!("'{v}' is not a finite number within ±1,000,000,000."))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    match spec.name {
        "Point" | "Box" | "Arc" | "Move" | "Copy" | "Series" | "Range" => arity(&[3])?,
        "Line" => arity(&[6])?,
        "MeshTrimPlane" => arity(&[6, 7])?,
        "MeshSplitPlane" => arity(&[6])?,
        "Circle" | "Sphere" | "Pipe" | "Rotate" | "Scale" | "Divide" | "EvaluateCurve" => {
            arity(&[1])?
        }
        "Rectangle" | "Cylinder" | "Cone" | "Torus" => arity(&[2])?,
        "ExtrudeCrv" => arity(&[1, 3])?,
        "Revolve" => arity(&[0, 1])?,
        "ArrayLinear" | "Random" => arity(&[4])?,
        "ArrayPolar" => arity(&[1, 2])?,
        "Polyline" | "Curve" if nums.len() >= 6 && nums.len() % 3 == 0 && nums.len() <= 300 => {}
        "Polyline" | "Curve" => return Err("Supply 2 to 100 points as x y z triples.".into()),
        "Mirror" => {}
        _ => arity(&[0])?,
    }
    let count = match spec.name {
        "Divide" | "ArrayLinear" | "ArrayPolar" => Some(nums[0]),
        "Series" | "Range" | "Random" => Some(nums[2]),
        _ => None,
    };
    if let Some(n) = count {
        let maximum = if ["ArrayLinear", "ArrayPolar"].contains(&spec.name) {
            4096.0
        } else {
            10000.0
        };
        if !(1.0..=maximum).contains(&n) || n.fract() != 0.0 {
            return Err(format!("Count must be a whole number from 1 to {maximum}."));
        }
    }
    if spec.name == "Random" && nums[3].fract() != 0.0 {
        return Err("Seed must be a whole number.".into());
    }
    if spec.name == "EvaluateCurve" && !(0.0..=1.0).contains(&nums[0]) {
        return Err("Curve parameter must be from 0 to 1.".into());
    }
    if [
        "Rectangle",
        "Box",
        "Cylinder",
        "Cone",
        "Torus",
        "Circle",
        "Sphere",
        "Pipe",
    ]
    .contains(&spec.name)
        && nums.iter().any(|v| *v <= 0.0)
    {
        return Err("Dimensions and radii must be greater than zero.".into());
    }
    if spec.name == "Scale" && nums[0] == 0.0 {
        return Err("Scale factor must be nonzero.".into());
    }
    if spec.name == "Revolve"
        && nums
            .first()
            .is_some_and(|angle| angle.to_radians().abs() < 1.0e-9)
    {
        return Err("Revolve sweep must be nonzero.".into());
    }
    let creation = [
        "Point",
        "Line",
        "Polyline",
        "Curve",
        "Circle",
        "Arc",
        "Rectangle",
        "Box",
        "Sphere",
        "Cylinder",
        "Cone",
        "Torus",
        "Series",
        "Range",
        "Random",
    ]
    .contains(&spec.name);
    let mut selected: Vec<_> = selection.iter().copied().collect();
    if !creation {
        if selected.is_empty() {
            return Err(format!(
                "Select geometry nodes in the graph before {}.",
                spec.name
            ));
        }
        if selected.len() > 256 {
            return Err("Select at most 256 nodes for one command.".into());
        }
        for id in &selected {
            if !doc.graph.nodes.contains_key(id) {
                return Err("Selection contains a removed node; select geometry again.".into());
            }
        }
        selected.sort_by(|a, b| {
            let a = &doc.graph.nodes[a];
            let b = &doc.graph.nodes[b];
            a.pos
                .1
                .total_cmp(&b.pos.1)
                .then(a.pos.0.total_cmp(&b.pos.0))
                .then(a.id.cmp(&b.id))
        });
    }
    let mut recipe = Recipe::default();
    let binary = spec.name.starts_with("MeshBoolean");
    if binary && selected.len() != 2 {
        return Err("Select exactly two closed mesh nodes for a mesh Boolean operation.".into());
    }
    if matches!(spec.name, "MeshTrimPlane" | "MeshSplitPlane") {
        if nums[3..6].iter().all(|v| *v == 0.) {
            return Err("Plane normal must be nonzero.".into());
        }
        if nums.get(6).is_some_and(|v| *v != 0. && *v != 1.) {
            return Err("Keep-positive must be 0 or 1.".into());
        }
    }
    let sources: Vec<Option<NodeId>> = if creation {
        vec![None]
    } else if binary {
        vec![Some(selected[0])]
    } else if spec.name == "Loft" {
        vec![Some(recipe.merge(&selected))]
    } else {
        selected.iter().copied().map(Some).collect()
    };
    let mut results = BTreeSet::new();
    for source in sources {
        let id = recipe.add(spec.component);
        results.insert(id);
        if let Some(source) = source {
            recipe.wire(source, id, 0);
        }
        if binary {
            recipe.wire(selected[1], id, 1);
        }
        match spec.name {
            "MeshTrimPlane" | "MeshSplitPlane" => {
                let plane = recipe.add("plane_normal");
                recipe.param(plane, "__preview", ParamValue::Bool(false));
                recipe.vector(plane, 0, &nums[..3], "plane origin");
                recipe.vector(plane, 1, &nums[3..6], "plane normal");
                recipe.wire(plane, id, 1);
                if spec.name == "MeshTrimPlane" {
                    let toggle = recipe.add("bool_toggle");
                    recipe.param(toggle, "value", ParamValue::Bool(nums.get(6) == Some(&1.)));
                    recipe.wire(toggle, id, 2);
                }
            }
            "Point" | "Box" | "Series" | "Range" | "Random" => {
                let offset = u16::from(spec.name == "Box");
                let ports = doc
                    .registry
                    .get(spec.component)
                    .ok_or("Component is unavailable")?
                    .inputs();
                for (i, n) in nums.iter().enumerate() {
                    recipe.number(id, i as u16 + offset, *n, ports[i + offset as usize].name);
                }
            }
            "Line" => {
                recipe.vector(id, 0, &nums[..3], "start");
                recipe.vector(id, 1, &nums[3..], "end");
            }
            "Polyline" | "Curve" => {
                let mut points = Vec::new();
                for xyz in nums.chunks(3) {
                    let p = recipe.add("point_xyz");
                    recipe.param(p, "__preview", ParamValue::Bool(false));
                    for (i, n) in xyz.iter().enumerate() {
                        recipe.number(p, i as u16, *n, ["x", "y", "z"][i]);
                    }
                    points.push(p);
                }
                let merged = recipe.merge(&points);
                recipe.wire(merged, id, 0);
            }
            "Circle" | "Sphere" | "Pipe" => recipe.number(id, 1, nums[0], "radius"),
            "Rectangle" | "Cylinder" | "Cone" | "Torus" => {
                let ports = doc
                    .registry
                    .get(spec.component)
                    .ok_or("Component is unavailable")?
                    .inputs();
                recipe.number(id, 1, nums[0], ports[1].name);
                recipe.number(id, 2, nums[1], ports[2].name);
            }
            "Arc" => {
                recipe.number(id, 1, nums[0], "radius");
                recipe.number(id, 2, nums[1].to_radians(), "start radians");
                recipe.number(id, 3, nums[2].to_radians(), "end radians");
            }
            "ExtrudeCrv" => {
                let v = if nums.len() == 1 {
                    vec![0.0, 0.0, nums[0]]
                } else {
                    nums.clone()
                };
                if v.iter().all(|n| *n == 0.0) {
                    return Err("Extrusion direction must be nonzero.".into());
                }
                recipe.vector(id, 1, &v, "direction");
            }
            "Revolve" => {
                if let Some(n) = nums.first() {
                    recipe.number(id, 3, n.to_radians(), "angle radians");
                }
            }
            "Move" | "Copy" => recipe.vector(id, 1, &nums, "motion"),
            "Rotate" => recipe.number(id, 2, nums[0].to_radians(), "angle radians"),
            "Scale" => recipe.number(id, 2, nums[0], "factor"),
            "Mirror" => {
                let normal = match args
                    .first()
                    .map(|v| v.to_ascii_lowercase())
                    .as_deref()
                    .unwrap_or("yz")
                {
                    "xy" => [0.0, 0.0, 1.0],
                    "xz" => [0.0, 1.0, 0.0],
                    _ => [1.0, 0.0, 0.0],
                };
                let plane = recipe.add("plane_normal");
                recipe.param(plane, "__preview", ParamValue::Bool(false));
                recipe.vector(plane, 1, &normal, "mirror normal");
                recipe.wire(plane, id, 1);
            }
            "ArrayLinear" => {
                recipe.number(id, 2, nums[0], "count");
                recipe.vector(id, 1, &nums[1..], "step");
            }
            "ArrayPolar" => {
                recipe.number(id, 2, nums[0], "count");
                if nums.len() == 2 {
                    recipe.number(id, 3, nums[1].to_radians(), "angle radians");
                }
            }
            "Divide" => recipe.number(id, 1, nums[0], "segments"),
            "EvaluateCurve" => recipe.number(id, 1, nums[0], "parameter"),
            _ => {}
        }
    }
    if [
        "Move",
        "Rotate",
        "Scale",
        "Mirror",
        "Reverse",
        "ArrayLinear",
        "ArrayPolar",
        "MeshBooleanUnion",
        "MeshBooleanDifference",
        "MeshBooleanIntersection",
        "MeshTrimPlane",
        "MeshSplitPlane",
    ]
    .contains(&spec.name)
    {
        for source in &selected {
            recipe.param(*source, "__preview", ParamValue::Bool(false));
        }
    }
    recipe.layout(doc);
    let mut trial = doc.graph.clone();
    trial
        .apply_all(&recipe.ops)
        .map_err(|(i, e)| format!("Command operation {i}: {e}"))?;
    let evaluated = Evaluator::new().evaluate(&trial, &doc.registry);
    for id in &recipe.added {
        if let Some(error) = evaluated.errors.get(id) {
            return Err(format!("{}: {error}", spec.name));
        }
    }
    let descriptions: Vec<_> = results
        .iter()
        .take(3)
        .filter_map(|id| evaluated.outputs.get(id))
        .flat_map(|values| values.iter().map(Value::describe))
        .collect();
    let message = format!("{}: {}", spec.name, descriptions.join(" · "));
    doc.end_gesture();
    doc.apply_ops(recipe.ops)?;
    Ok(CommandResult {
        selection: results,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_kernel::Vec3;

    fn doc() -> Document {
        Document::new(Identity::from_secret_hex("command test", &"19".repeat(32)).unwrap())
    }
    fn run(doc: &mut Document, selection: &BTreeSet<NodeId>, input: &str) -> BTreeSet<NodeId> {
        execute(doc, selection, input).unwrap().selection
    }
    fn first(doc: &mut Document, ids: &BTreeSet<NodeId>) -> Value {
        doc.evaluate();
        doc.last_eval.outputs[ids.first().unwrap()][0].clone()
    }
    #[test]
    fn mesh_boolean_and_plane_commands_are_editable_and_undoable() {
        let mut d = doc();
        let body = run(&mut d, &BTreeSet::new(), "Box 2 2 2");
        let cutter = run(&mut d, &BTreeSet::new(), "Box 1 1 1");
        let cutter = run(&mut d, &cutter, "Move 0.5 0.5 0.5");
        let both = body.union(&cutter).copied().collect();
        let before = d.graph.clone();
        let result = run(&mut d, &both, "MeshBooleanDifference");
        assert!((first(&mut d, &result).as_mesh().unwrap().volume() - 7.).abs() < 1e-8);
        assert!(!d.graph.nodes[body.first().unwrap()].preview());
        d.undo_pending().unwrap();
        assert_eq!(d.graph, before);
        let split = run(&mut d, &body, "MeshSplitPlane 0 0 1 0 0 1");
        d.evaluate();
        let outputs = &d.last_eval.outputs[split.first().unwrap()];
        assert_eq!(outputs.len(), 2);
        for value in outputs {
            assert!((value.as_mesh().unwrap().volume() - 4.).abs() < 1e-8);
        }
        d.undo_pending().unwrap();
        let trim = run(&mut d, &body, "MeshTrimPlane 0 0 1 0 0 1 1");
        let mesh = first(&mut d, &trim).as_mesh().unwrap();
        assert!((mesh.volume() - 4.).abs() < 1e-8);
        assert!((mesh.bbox().min.z - 1.).abs() < 1e-8);
        let before = d.graph.clone();
        assert!(execute(&mut d, &body, "MeshBooleanUnion").is_err());
        assert!(execute(&mut d, &body, "MeshTrimPlane 0 0 1 0 0 0").is_err());
        assert_eq!(d.graph, before);
    }
    #[test]
    fn primitives_use_arguments_and_are_single_undo_steps() {
        let mut d = doc();
        let selected = run(&mut d, &BTreeSet::new(), "_Box 10,20,30");
        let mesh = first(&mut d, &selected).as_mesh().unwrap();
        assert!((mesh.volume().abs() - 6000.0).abs() < 1e-8);
        d.undo_pending().unwrap();
        assert!(d.graph.nodes.is_empty());
        d.redo_pending().unwrap();
        assert!(first(&mut d, &selected).as_mesh().is_some());
    }
    #[test]
    fn operations_preserve_dependency_and_undo_source_preview() {
        let mut d = doc();
        let circle = run(&mut d, &BTreeSet::new(), "Circle 5");
        let before = d.graph.clone();
        let moved = run(&mut d, &circle, "Move 10 0 0");
        assert!(!d.graph.nodes[circle.first().unwrap()].preview());
        let curve = first(&mut d, &moved).as_curve().unwrap();
        assert!((curve.point_at(0.0) - Vec3::new(15.0, 0.0, 0.0)).length() < 1e-8);
        d.undo_pending().unwrap();
        assert_eq!(d.graph, before);
        let length = run(&mut d, &circle, "Length");
        assert!(
            (first(&mut d, &length).as_number().unwrap() - 10.0 * std::f64::consts::PI).abs()
                < 1e-8
        );
    }
    #[test]
    fn rejects_wrong_selection_extra_arguments_and_nonfinite_without_mutation() {
        let mut d = doc();
        for input in [
            "Circle 2 3",
            "Line",
            "Scale 2",
            "Box 1 -2 3",
            "Circle NaN",
            "Circle inf",
            "Circle 1e308",
            "Unknown 3",
            "Series 0 1 2.5",
        ] {
            assert!(execute(&mut d, &BTreeSet::new(), input).is_err(), "{input}");
            assert!(d.graph.nodes.is_empty());
            assert!(d.pending.is_empty());
        }
        let box_ids = run(&mut d, &BTreeSet::new(), "Box 1 2 3");
        let before = d.graph.clone();
        let pending = d.pending.clone();
        assert!(execute(&mut d, &box_ids, "Length").is_err());
        assert_eq!(d.graph, before);
        assert_eq!(d.pending, pending);
    }
    #[test]
    fn loft_merges_selected_curves_and_keeps_atomic_history() {
        let mut d = doc();
        let circle = run(&mut d, &BTreeSet::new(), "Circle 2");
        let elevated = run(&mut d, &circle, "Copy 0 0 5");
        let selection = circle.union(&elevated).copied().collect();
        let before = d.graph.clone();
        let loft = run(&mut d, &selection, "Loft");
        assert!(first(&mut d, &loft).as_mesh().unwrap().triangle_count() > 0);
        d.undo_pending().unwrap();
        assert_eq!(d.graph, before);
    }
    #[test]
    fn all_examples_evaluate_on_appropriate_selection() {
        for spec in COMMANDS {
            let mut d = doc();
            let selection = match spec.name {
                "MeshBooleanUnion" | "MeshBooleanDifference" | "MeshBooleanIntersection" => {
                    let a = run(&mut d, &BTreeSet::new(), "Box 2 2 2");
                    let b = run(&mut d, &BTreeSet::new(), "Box 1 1 1");
                    a.union(&b).copied().collect()
                }
                "MeshTrimPlane" | "MeshSplitPlane" => run(&mut d, &BTreeSet::new(), "Box 10 10 10"),
                "Area" | "Volume" => run(&mut d, &BTreeSet::new(), "Box 1 2 3"),
                "Revolve" => run(&mut d, &BTreeSet::new(), "Line 5 0 0 5 0 10"),
                "ArrayPolar" => {
                    let circle = run(&mut d, &BTreeSet::new(), "Circle 2");
                    run(&mut d, &circle, "Move 5 0 0")
                }
                "Loft" => {
                    let a = run(&mut d, &BTreeSet::new(), "Circle 2");
                    let b = run(&mut d, &a, "Copy 0 0 5");
                    a.union(&b).copied().collect()
                }
                _ => run(&mut d, &BTreeSet::new(), "Circle 2"),
            };
            execute(&mut d, &selection, spec.example)
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            d.evaluate();
            assert!(d.last_eval.errors.is_empty(), "{}", spec.name);
        }
    }
    #[test]
    fn history_view_rejects_commands() {
        let mut d = doc();
        run(&mut d, &BTreeSet::new(), "Circle 2");
        d.commit("circle", 1).unwrap();
        d.set_view(Some(0)).unwrap();
        let graph = d.graph.clone();
        assert!(execute(&mut d, &BTreeSet::new(), "Sphere 3").is_err());
        assert_eq!(d.graph, graph);
        assert!(d.pending.is_empty());
    }

    #[test]
    fn obj_export_rebases_faces_and_honors_preview() {
        let mut d = doc();
        assert!(visible_mesh_obj(&mut d).is_err());
        let box_ids = run(&mut d, &BTreeSet::new(), "Box 1 2 3");
        let _copy = run(&mut d, &box_ids, "Copy 10 0 0");
        let obj = visible_mesh_obj(&mut d).unwrap();
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count(), 48);
        assert_eq!(obj.lines().filter(|l| l.starts_with("f ")).count(), 24);
        let last_face = obj.lines().rfind(|l| l.starts_with("f ")).unwrap();
        assert!(last_face.split_whitespace().skip(1).all(|index| index
            .split('/')
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            > 24));
        d.set_param(
            *box_ids.first().unwrap(),
            "__preview",
            ParamValue::Bool(false),
        )
        .unwrap();
        assert_eq!(
            visible_mesh_obj(&mut d)
                .unwrap()
                .lines()
                .filter(|l| l.starts_with("v "))
                .count(),
            24
        );
    }
}
