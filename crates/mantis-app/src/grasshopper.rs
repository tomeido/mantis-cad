//! A bounded reader/writer for actual Grasshopper 1 GHX archives.
//! Component identities and port layouts are verified against McNeel's
//! rhinocodetests fixtures. Unknown components and affected dependents are
//! reported, never executed or silently replaced by default values.

use mantis_graph::{
    geometry::{GeometryData, GeometryRecord},
    Graph, GraphOp, NodeId, ParamValue, Registry, Value,
};
use mantis_kernel::{Plane, Vec3};
use quick_xml::{events::Event, Reader};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const SLIDER: &str = "57da07bd-ecab-415d-9d86-af36d7073abc";
const TOGGLE: &str = "2e78987b-9dfb-42a2-8b76-3923ac8bd91a";
const PANEL: &str = "59e0b89a-e487-49f8-bab8-b5bab16be14c";
const NUMBER: &str = "3e8ca6be-fda8-4aaf-b5c0-3c54c8bb7312";
const DATA: &str = "8ec86459-bf01-4409-baee-174d0d2b13d0";

pub struct ImportReport {
    pub ops: Vec<GraphOp>,
    pub warnings: Vec<String>,
    pub node_ids: Vec<NodeId>,
}

#[derive(Clone, Default)]
struct Xml {
    tag: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Xml>,
}
impl Xml {
    fn name(&self) -> &str {
        self.attrs.get("name").map(String::as_str).unwrap_or("")
    }
    fn index(&self) -> usize {
        self.attrs
            .get("index")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
    fn child(&self, tag: &str) -> Option<&Xml> {
        self.children.iter().find(|n| n.tag == tag)
    }
    fn chunks(&self) -> impl Iterator<Item = &Xml> {
        self.child("chunks")
            .into_iter()
            .flat_map(|n| n.children.iter())
            .filter(|n| n.tag == "chunk")
    }
    fn chunk(&self, name: &str) -> Option<&Xml> {
        self.chunks().find(|n| n.name() == name)
    }
    fn items(&self) -> impl Iterator<Item = &Xml> {
        self.child("items")
            .into_iter()
            .flat_map(|n| n.children.iter())
            .filter(|n| n.tag == "item")
    }
    fn item(&self, name: &str) -> Option<&Xml> {
        self.items().find(|n| n.name() == name)
    }
    fn string(&self, name: &str) -> Option<&str> {
        self.item(name).map(|n| n.text.as_str())
    }
    fn number(&self, name: &str, default: f64) -> Result<f64, String> {
        self.string(name).map(number).unwrap_or(Ok(default))
    }
    fn boolean(&self, name: &str, default: bool) -> Result<bool, String> {
        self.string(name).map(boolean).unwrap_or(Ok(default))
    }
    fn coord(&self, name: &str) -> Result<f64, String> {
        number(
            &self
                .child(name)
                .ok_or_else(|| format!("Missing coordinate {name}"))?
                .text,
        )
    }
}

fn number(s: &str) -> Result<f64, String> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && n.abs() <= 1.0e12)
        .ok_or_else(|| format!("Invalid or out-of-range number: {s}"))
}
fn boolean(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!("Invalid boolean: {s}")),
    }
}
fn guid(s: &str) -> Result<NodeId, String> {
    let hex: String = s
        .trim()
        .trim_matches(|c| c == '{' || c == '}')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    NodeId::from_hex(&hex)
        .filter(|id| id.0 != 0)
        .ok_or_else(|| format!("Invalid Grasshopper GUID: {s}"))
}
fn guid_text(id: NodeId) -> String {
    let s = id.to_hex();
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}

fn xml(text: &str) -> Result<Xml, String> {
    if text.len() > MAX_BYTES {
        return Err("GHX archive exceeds 64 MiB.".into());
    }
    let mut reader = Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut stack = vec![Xml::default()];
    let mut count = 0;
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("Invalid GHX XML: {e}"))?
        {
            Event::Start(e) | Event::Empty(e) => {
                // The reader distinguishes Empty below from the input bytes.
                let empty = reader.buffer_position() > 1
                    && text
                        .trim_start_matches('\u{feff}')
                        .as_bytes()
                        .get(reader.buffer_position() as usize - 2)
                        == Some(&b'/');
                count += 1;
                if count > 250_000 || stack.len() > 96 {
                    return Err("GHX XML structure exceeds limits.".into());
                }
                let mut node = Xml {
                    tag: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
                    ..Default::default()
                };
                for a in e.attributes() {
                    let a = a.map_err(|e| e.to_string())?;
                    node.attrs.insert(
                        String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                        a.decode_and_unescape_value(reader.decoder())
                            .map_err(|e| e.to_string())?
                            .into_owned(),
                    );
                }
                if node
                    .attrs
                    .get("index")
                    .is_some_and(|s| s.parse::<u32>().is_err())
                {
                    return Err("Invalid negative or noninteger GHX chunk/item index".into());
                }
                if empty {
                    stack.last_mut().unwrap().children.push(node);
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err("Unbalanced GHX XML".into());
                }
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(e) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&e.xml_content().map_err(|e| e.to_string())?),
            Event::CData(e) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&e.decode().map_err(|e| e.to_string())?),
            Event::GeneralRef(e) => {
                if let Some(c) = e.resolve_char_ref().map_err(|e| e.to_string())? {
                    stack.last_mut().unwrap().text.push(c);
                } else {
                    let name = e.decode().map_err(|e| e.to_string())?;
                    let value = quick_xml::escape::resolve_predefined_entity(&name)
                        .ok_or("Custom XML entities are unsupported")?;
                    stack.last_mut().unwrap().text.push_str(value);
                }
            }
            Event::DocType(_) => {
                return Err("DTD and external entities are not accepted in GHX.".into())
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 || stack[0].children.len() != 1 {
        return Err("GHX must have one complete XML root.".into());
    }
    let root = stack.pop().unwrap().children.remove(0);
    if root.tag != "Archive" {
        return Err("This is not a Grasshopper GHX archive.".into());
    }
    Ok(root)
}

#[derive(Clone, Copy)]
struct Mapping {
    gh: &'static str,
    mantis: &'static str,
    name: &'static str,
    inputs: usize,
    outputs: usize,
    variable: bool,
}
const MAP: &[Mapping] = &[
    Mapping {
        gh: "3581f42a-9592-4549-bd6b-1c0fc39d067b",
        mantis: "point_xyz",
        name: "Construct Point",
        inputs: 3,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "56b92eab-d121-43f7-94d3-6cd8f0ddead8",
        mantis: "vector_xyz",
        name: "Vector XYZ",
        inputs: 3,
        outputs: 2,
        variable: false,
    },
    Mapping {
        gh: "4c4e56eb-2f04-43f9-95a3-cc46a14f495a",
        mantis: "line",
        name: "Line",
        inputs: 2,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "807b86e3-be8d-4970-92b5-f8cdcb45b06b",
        mantis: "circle",
        name: "Circle",
        inputs: 2,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "71b5b089-500a-4ea6-81c5-2f960441a0e8",
        mantis: "polyline",
        name: "PolyLine",
        inputs: 2,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "dde71aef-d6ed-40a6-af98-6b0673983c82",
        mantis: "nurbs_curve",
        name: "Nurbs Curve",
        inputs: 3,
        outputs: 3,
        variable: false,
    },
    Mapping {
        gh: "2162e72e-72fc-4bf8-9459-d4d82fa8aa14",
        mantis: "divide_curve",
        name: "Divide Curve",
        inputs: 3,
        outputs: 3,
        variable: false,
    },
    Mapping {
        gh: "e9eb1dcf-92f6-4d4d-84ae-96222d60f56b",
        mantis: "move",
        name: "Move",
        inputs: 2,
        outputs: 2,
        variable: false,
    },
    Mapping {
        gh: "e64c5fb1-845c-4ab1-8911-5f338516ba67",
        mantis: "series",
        name: "Series",
        inputs: 3,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "a0d62394-a118-422d-abb3-6af115c75b25",
        mantis: "add",
        name: "Addition",
        inputs: 2,
        outputs: 1,
        variable: true,
    },
    Mapping {
        gh: "ce46b74e-00c9-43c4-805a-193b69ea4a11",
        mantis: "multiply",
        name: "Multiplication",
        inputs: 2,
        outputs: 1,
        variable: true,
    },
    Mapping {
        gh: "3cadddef-1e2b-4c09-9390-0e8f78f7609f",
        mantis: "merge",
        name: "Merge",
        inputs: 2,
        outputs: 1,
        variable: true,
    },
    Mapping {
        gh: "1817fd29-20ae-4503-b542-f0fb651e67d7",
        mantis: "list_length",
        name: "List Length",
        inputs: 1,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "59daf374-bc21-4a5e-8282-5504fb7ae9ae",
        mantis: "list_item",
        name: "List Item",
        inputs: 3,
        outputs: 1,
        variable: true,
    },
    Mapping {
        gh: "79f9fbb3-8f1d-4d9a-88a9-f7961b1012cd",
        mantis: "unit_x",
        name: "Unit X",
        inputs: 1,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "d3d195ea-2d59-4ffa-90b1-8b7ff3369f69",
        mantis: "unit_y",
        name: "Unit Y",
        inputs: 1,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "9103c240-a6a9-4223-9b42-dbd19bf38e2b",
        mantis: "unit_z",
        name: "Unit Z",
        inputs: 1,
        outputs: 1,
        variable: false,
    },
    Mapping {
        gh: "93b8e93d-f932-402c-b435-84be04d87666",
        mantis: "distance",
        name: "Distance",
        inputs: 2,
        outputs: 1,
        variable: false,
    },
];

fn parameter_type(gh: &str) -> bool {
    [
        NUMBER,
        DATA,
        PANEL,
        "2e3ab970-8545-46bb-836c-1c11e5610bce",
        "cb95db89-6165-43b6-9c41-5702bc5bf137",
        "3ede854e-c753-40eb-84cb-b48008f14fd4",
        "fbac3e32-f100-4292-8692-77240a42fd1a",
        "16ef3e75-e315-4899-b531-d3166b42dac9",
        "4f8984c4-7c7a-4d69-b0a2-183cbb330d20",
        "d5967b9f-e8ee-436b-a8ad-29fdcecf32d5",
        "ac2bc2cb-70fb-4dd5-9c78-7e1ea97fe278",
        "d1028c72-ff86-4057-9eb0-36c687a4d98c",
        "abf9c670-5462-4cd8-acb3-f1ab0256dbf3",
        "b6236720-8d88-4289-93c3-ac4c99f9b97b",
    ]
    .contains(&gh)
}

fn ports(container: &Xml, input: bool) -> Vec<&Xml> {
    let mut result: Vec<_> = if let Some(data) = container.chunk("ParameterData") {
        data.chunks()
            .filter(|n| n.name() == if input { "InputParam" } else { "OutputParam" })
            .collect()
    } else {
        container
            .chunks()
            .filter(|n| n.name() == if input { "param_input" } else { "param_output" })
            .collect()
    };
    result.sort_by_key(|n| n.index());
    result
}

fn no_modifiers(node: &Xml) -> Result<(), String> {
    for key in ["Reverse", "Simplify", "Graft", "Flatten", "Locked"] {
        if node.boolean(key, false)? {
            return Err(format!("{key} processing is unsupported"));
        }
    }
    if node.number("DataMapping", 0.0)? != 0.0 {
        return Err("Data-tree mapping is unsupported".into());
    }
    if node
        .string("Expression")
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err("Parameter expressions are unsupported".into());
    }
    Ok(())
}

#[derive(Clone)]
enum Literal {
    Scalar(serde_json::Value),
    Geometry(GeometryData),
}
fn persistent(node: &Xml) -> Result<Vec<Literal>, String> {
    let Some(data) = node.chunk("PersistentData") else {
        return Ok(Vec::new());
    };
    let branches: Vec<_> = data.chunks().filter(|n| n.name() == "Branch").collect();
    if branches.len() > 1 {
        return Err("Multiple data-tree branches cannot be represented as a flat list".into());
    }
    let mut result = Vec::new();
    for branch in branches {
        if branch.string("Path").is_some_and(|s| s != "{0}") {
            return Err("Non-default data-tree paths are unsupported".into());
        }
        let mut items: Vec<_> = branch.chunks().filter(|n| n.name() == "Item").collect();
        items.sort_by_key(|n| n.index());
        if items.len() > 10_000 {
            return Err("Persistent list exceeds 10,000 items".into());
        }
        for item in items {
            if item.boolean("null_string", false)? {
                return Err("Null string values are unsupported".into());
            }
            let value = item
                .items()
                .find(|n| {
                    [
                        "number",
                        "boolean",
                        "string",
                        "Coordinate",
                        "vector",
                        "plane",
                    ]
                    .contains(&n.name())
                })
                .ok_or("Referenced Rhino geometry or empty persistent item is unsupported")?;
            let ty = value
                .attrs
                .get("type_name")
                .map(String::as_str)
                .unwrap_or("");
            let literal = match ty {
                "gh_double" | "gh_single" | "gh_int32" | "gh_int64" => {
                    Literal::Scalar(serde_json::json!(number(&value.text)?))
                }
                "gh_bool" => Literal::Scalar(serde_json::json!(boolean(&value.text)?)),
                "gh_string" => Literal::Scalar(serde_json::json!(&value.text)),
                "gh_point3d" | "gh_vector3d" => Literal::Geometry(GeometryData::Point {
                    point: Vec3::new(value.coord("X")?, value.coord("Y")?, value.coord("Z")?),
                }),
                "gh_plane" => Literal::Geometry(GeometryData::Plane {
                    plane: Plane {
                        origin: Vec3::new(
                            value.coord("Ox")?,
                            value.coord("Oy")?,
                            value.coord("Oz")?,
                        ),
                        x_axis: Vec3::new(
                            value.coord("Xx")?,
                            value.coord("Xy")?,
                            value.coord("Xz")?,
                        ),
                        y_axis: Vec3::new(
                            value.coord("Yx")?,
                            value.coord("Yy")?,
                            value.coord("Yz")?,
                        ),
                    },
                }),
                _ => {
                    return Err(format!(
                        "Persistent {ty} data requires unsupported Rhino serialization"
                    ))
                }
            };
            result.push(literal);
        }
    }
    Ok(result)
}

struct Plan {
    root: NodeId,
    name: String,
    ops: Vec<GraphOp>,
    added: Vec<NodeId>,
    wires: Vec<(NodeId, (NodeId, u16))>,
    pos: (f32, f32),
}
impl Plan {
    fn add(&mut self, id: NodeId, ty: &str) {
        self.added.push(id);
        self.ops.push(GraphOp::AddNode {
            id,
            type_name: ty.into(),
            pos: self.pos,
        });
    }
    fn param(&mut self, id: NodeId, key: &str, value: ParamValue) {
        self.ops.push(GraphOp::SetParam {
            id,
            key: key.into(),
            value,
        });
    }
    fn helper(&mut self, ty: &str) -> NodeId {
        let id = crate::util::new_node_id();
        self.add(id, ty);
        self.param(id, "__preview", ParamValue::Bool(false));
        id
    }
    fn connect(&mut self, from: (NodeId, u16), to: (NodeId, u16)) {
        self.ops.push(GraphOp::Connect { from, to });
    }
    fn literals(&mut self, values: &[Literal]) -> Result<NodeId, String> {
        if values.iter().all(|v| matches!(v, Literal::Scalar(_))) {
            let values: Vec<_> = values
                .iter()
                .filter_map(|v| {
                    if let Literal::Scalar(v) = v {
                        Some(v)
                    } else {
                        None
                    }
                })
                .collect();
            let id = self.helper("constant_list");
            self.param(
                id,
                "values",
                ParamValue::Text(serde_json::to_string(&values).map_err(|e| e.to_string())?),
            );
            return Ok(id);
        }
        let mut ids = Vec::new();
        for value in values {
            match value {
                Literal::Scalar(_) => ids.push(self.literals(std::slice::from_ref(value))?),
                Literal::Geometry(geometry) => {
                    let record = GeometryRecord {
                        name: String::new(),
                        layer: String::new(),
                        geometry: geometry.clone(),
                        source: None,
                    };
                    let id = self.helper("imported_geometry");
                    self.param(id, "data", ParamValue::Text(record.to_json()?));
                    ids.push(id);
                }
            }
        }
        let mut result = *ids.first().ok_or("No persistent data")?;
        for id in ids.into_iter().skip(1) {
            let merged = self.helper("merge");
            self.connect((result, 0), (merged, 0));
            self.connect((id, 0), (merged, 1));
            result = merged;
        }
        Ok(result)
    }
    fn input(&mut self, node: &Xml, target: (NodeId, u16)) -> Result<(), String> {
        no_modifiers(node)?;
        let mut sources: Vec<_> = node.items().filter(|n| n.name() == "Source").collect();
        sources.sort_by_key(|n| n.index());
        if sources.iter().enumerate().any(|(i, n)| n.index() != i) {
            return Err("Duplicate or missing wire source indices".into());
        }
        let declared = node.number("SourceCount", sources.len() as f64)?;
        if declared != sources.len() as f64 {
            return Err("Wire source count does not match archive".into());
        }
        if sources.is_empty() {
            let values = persistent(node)?;
            // A cleared GH input stays empty; a native component default must
            // not invent values that were absent from the source document.
            let id = self.literals(&values)?;
            self.connect((id, 0), target);
        } else if sources.len() == 1 {
            self.wires.push((guid(&sources[0].text)?, target));
        } else {
            let mut current = None;
            for source in sources {
                let merged = self.helper("merge");
                if let Some(previous) = current {
                    self.connect((previous, 0), (merged, 0));
                }
                self.wires.push((guid(&source.text)?, (merged, 1)));
                current = Some(merged);
            }
            self.connect((current.unwrap(), 0), target);
        }
        Ok(())
    }
}

pub fn import_ghx(text: &str) -> Result<ImportReport, String> {
    let root = xml(text)?;
    let objects = root
        .chunk("Definition")
        .and_then(|n| n.chunk("DefinitionObjects"))
        .ok_or("GHX contains no DefinitionObjects chunk")?;
    let objects: Vec<_> = objects.chunks().filter(|n| n.name() == "Object").collect();
    if objects.len() > 10_000 {
        return Err("GHX contains more than 10,000 objects".into());
    }
    let mut warnings = Vec::new();
    let mut plans = Vec::new();
    let mut endpoints = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for object in objects {
        let gh = object.string("GUID").unwrap_or("").to_ascii_lowercase();
        let container = object
            .chunk("Container")
            .ok_or("Object has no Container chunk")?;
        let id = guid(
            container
                .string("InstanceGuid")
                .ok_or("Object has no InstanceGuid")?,
        )?;
        if !seen.insert(id) {
            return Err("Duplicate Grasshopper object InstanceGuid".into());
        }
        let name = container
            .string("NickName")
            .filter(|n| !n.is_empty())
            .or_else(|| object.string("Name"))
            .unwrap_or("Unnamed")
            .to_string();
        let pos_item = container
            .chunk("Attributes")
            .and_then(|n| n.item("Pivot").or_else(|| n.item("Bounds")));
        let pos = match pos_item {
            Some(p) => (p.coord("X")? as f32, p.coord("Y")? as f32),
            None => (0.0, 0.0),
        };
        let mut plan = Plan {
            root: id,
            name: name.clone(),
            ops: Vec::new(),
            added: Vec::new(),
            wires: Vec::new(),
            pos,
        };
        let result = (|| -> Result<(), String> {
            no_modifiers(container)?;
            if gh == SLIDER {
                let slider = container
                    .chunk("Slider")
                    .ok_or("Slider settings are missing")?;
                let interval = slider.number("Interval", 1.0)?;
                if interval != 0.0 && interval != 1.0 {
                    return Err("Even/odd slider modes are unsupported".into());
                }
                plan.add(id, "number_slider");
                for (key, gh_key, default) in [
                    ("value", "Value", 0.0),
                    ("min", "Min", 0.0),
                    ("max", "Max", 10.0),
                ] {
                    plan.param(id, key, ParamValue::Number(slider.number(gh_key, default)?));
                }
                if interval == 1.0 && slider.number("Min", 0.0)?.fract() != 0.0 {
                    return Err("Integer sliders with fractional bounds are unsupported".into());
                }
                plan.param(
                    id,
                    "step",
                    ParamValue::Number(if interval == 1.0 { 1.0 } else { 0.0 }),
                );
                endpoints.insert(id, (id, 0));
            } else if gh == TOGGLE {
                plan.add(id, "bool_toggle");
                plan.param(
                    id,
                    "value",
                    ParamValue::Bool(container.boolean("ToggleValue", false)?),
                );
                endpoints.insert(id, (id, 0));
            } else if parameter_type(&gh) {
                let sources = container.number("SourceCount", 0.0)?;
                if sources > 0.0
                    && ![
                        DATA,
                        "ac2bc2cb-70fb-4dd5-9c78-7e1ea97fe278",
                        "b6236720-8d88-4289-93c3-ac4c99f9b97b",
                    ]
                    .contains(&gh.as_str())
                {
                    return Err("Wired typed parameters require Grasshopper coercion that is not yet supported".into());
                }
                if sources == 0.0 && gh == PANEL {
                    let text = container.string("UserText").unwrap_or("");
                    let values: Vec<serde_json::Value> = text
                        .lines()
                        .map(|v| {
                            number(v)
                                .map(serde_json::Value::from)
                                .unwrap_or_else(|_| serde_json::Value::String(v.into()))
                        })
                        .collect();
                    plan.add(id, "constant_list");
                    plan.param(
                        id,
                        "values",
                        ParamValue::Text(
                            serde_json::to_string(&values).map_err(|e| e.to_string())?,
                        ),
                    );
                } else {
                    let values = persistent(container)?;
                    if sources == 0.0
                        && values.is_empty()
                        && container.chunk("PersistentData").is_none()
                    {
                        return Err(
                            "Empty or externally referenced parameter has no self-contained values"
                                .into(),
                        );
                    }
                    plan.add(id, "merge");
                    plan.input(container, (id, 0))?;
                }
                endpoints.insert(id, (id, 0));
            } else if let Some(mapping) = MAP.iter().find(|m| m.gh == gh) {
                plan.add(id, mapping.mantis);
                let input_ports = ports(container, true);
                if input_ports.len() != mapping.inputs {
                    return Err(
                        "Input port count differs from the supported component layout".into(),
                    );
                }
                for (index, port) in input_ports.into_iter().enumerate() {
                    if port.index() != index {
                        return Err("Duplicate or missing component input indices".into());
                    }
                    if port.index() >= mapping.inputs {
                        return Err("Input port index exceeds supported component ports".into());
                    }
                    if mapping.mantis == "divide_curve" && port.index() == 2 {
                        if port.number("SourceCount", 0.0)? != 0.0
                            || persistent(port)?.iter().any(|v| {
                                !matches!(v, Literal::Scalar(serde_json::Value::Bool(false)))
                            })
                        {
                            return Err("Divide Curve Kinks=true/wired is unsupported".into());
                        }
                        continue;
                    }
                    if mapping.mantis == "nurbs_curve"
                        && port.index() == 2
                        && (port.number("SourceCount", 0.0)? != 0.0
                            || persistent(port)?.iter().any(|v| {
                                !matches!(v, Literal::Scalar(serde_json::Value::Bool(false)))
                            }))
                    {
                        return Err("Periodic NURBS cannot be mapped to this kernel without changing the curve".into());
                    }
                    plan.input(port, (id, port.index() as u16))?;
                }
                let count = Registry::standard()
                    .get(mapping.mantis)
                    .unwrap()
                    .outputs()
                    .len();
                for port in ports(container, false) {
                    if port.index() < count {
                        endpoints.insert(
                            guid(
                                port.string("InstanceGuid")
                                    .ok_or("Output has no InstanceGuid")?,
                            )?,
                            (id, port.index() as u16),
                        );
                    }
                }
            } else {
                return Err(format!(
                    "Unsupported component GUID {gh}; scripts/plugins are not executed"
                ));
            }
            plan.param(id, "label", ParamValue::Text(name.clone()));
            plan.param(
                id,
                "__preview",
                ParamValue::Bool(!container.boolean("Hidden", false)?),
            );
            Ok(())
        })();
        match result {
            Ok(()) => plans.push(plan),
            Err(error) => warnings.push(format!("{name} ({id}): {error}")),
        }
    }
    // A missing upstream object must never be replaced with an input default.
    let mut excluded = BTreeSet::new();
    loop {
        let mut changed = false;
        let available: BTreeSet<_> = plans
            .iter()
            .filter(|p| !excluded.contains(&p.root))
            .flat_map(|p| p.added.iter().copied())
            .collect();
        for plan in &plans {
            if excluded.contains(&plan.root) {
                continue;
            }
            if plan.wires.iter().any(|(source, _)| {
                endpoints
                    .get(source)
                    .is_none_or(|(id, _)| !available.contains(id))
            }) {
                excluded.insert(plan.root);
                changed = true;
                warnings.push(format!(
                    "{}: omitted because an upstream component/output is unsupported or missing",
                    plan.name
                ));
            }
        }
        if !changed {
            break;
        }
    }
    let mut ops = Vec::new();
    let mut wires = Vec::new();
    let mut node_ids = Vec::new();
    for mut plan in plans.into_iter().filter(|p| !excluded.contains(&p.root)) {
        node_ids.push(plan.root);
        for (source, to) in plan.wires {
            wires.push(GraphOp::Connect {
                from: endpoints[&source],
                to,
            });
        }
        ops.append(&mut plan.ops);
    }
    ops.extend(wires);
    if node_ids.is_empty() {
        return Err(format!(
            "No supported, self-contained Grasshopper objects found. {}",
            warnings.join("\n")
        ));
    }
    let mut graph = Graph::new();
    graph
        .apply_all(&ops)
        .map_err(|(_, e)| format!("Invalid/cyclic Grasshopper wiring: {e}"))?;
    warnings.extend(integer_wire_warnings(&graph));
    Ok(ImportReport {
        ops,
        warnings,
        node_ids,
    })
}

fn integer_wire_warnings(graph: &Graph) -> Vec<String> {
    fn integer_source(graph: &Graph, id: NodeId, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        let Some(node) = graph.nodes.get(&id) else {
            return false;
        };
        match node.type_name.as_str() {
            "number_slider" => node.params.get("step").and_then(ParamValue::as_number) == Some(1.0),
            "list_length" => true,
            "constant_list" => node
                .params
                .get("values")
                .and_then(ParamValue::as_text)
                .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
                .is_some_and(|values| {
                    values.iter().all(|v| {
                        v.as_f64()
                            .is_some_and(|n| n.is_finite() && n.fract() == 0.0)
                    })
                }),
            "merge" => (0..2).all(|port| {
                graph
                    .incoming((id, port))
                    .is_none_or(|edge| integer_source(graph, edge.from.0, depth + 1))
            }),
            _ => false,
        }
    }
    graph.nodes.values().filter_map(|node| {
        let port=match node.type_name.as_str() {
            "series" => 2,
            "divide_curve" | "nurbs_curve" | "list_item" => 1,
            _ => return None,
        };
        graph.incoming((node.id,port)).filter(|edge| !integer_source(graph,edge.from.0,0)).map(|_|format!(
            "{} ({}): integer input comes from a source that may be fractional. Grasshopper rounds integer inputs; native count/index conversion can differ. Use integer sliders or stored integer values for matching results.",
            node.type_name,node.id))
    }).collect()
}

fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}
fn item(name: &str, ty: &str, code: u16, value: &str) -> String {
    format!(
        "<item name=\"{}\" type_name=\"{ty}\" type_code=\"{code}\">{}</item>",
        esc(name),
        esc(value)
    )
}
fn chunk(name: &str, index: Option<usize>, items: Vec<String>, children: Vec<String>) -> String {
    let index = index.map(|i| format!(" index=\"{i}\"")).unwrap_or_default();
    format!("<chunk name=\"{}\"{index}><items count=\"{}\">{}</items><chunks count=\"{}\">{}</chunks></chunk>",esc(name),items.len(),items.concat(),children.len(),children.concat())
}
fn attrs(pos: (f32, f32)) -> String {
    chunk("Attributes",None,vec![format!("<item name=\"Pivot\" type_name=\"gh_drawing_pointf\" type_code=\"31\"><X>{}</X><Y>{}</Y></item>",pos.0,pos.1)],vec![])
}

fn persistent_xml(values: &[Value]) -> Result<String, String> {
    persistent_xml_as(values, "point")
}
fn persistent_xml_as(values: &[Value], kind: &str) -> Result<String, String> {
    let mut children = Vec::new();
    for (i, value) in values.iter().enumerate() {
        let data = match value {
            Value::Number(n) if n.is_finite() && kind == "integer" && n.fract() == 0.0 && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 => item("number","gh_int32",3,&n.to_string()),
            Value::Number(_) if kind == "integer" => return Err("Grasshopper integer input cannot represent this value.".into()),
            Value::Number(n) if n.is_finite() => item("number","gh_double",6,&n.to_string()),
            Value::Bool(b) => item("boolean","gh_bool",1,&b.to_string()),
            Value::Text(s) => item("string","gh_string",10,s),
            Value::Vector(p) => format!("<item name=\"{}\" type_name=\"gh_point3d\" type_code=\"51\"><X>{}</X><Y>{}</Y><Z>{}</Z></item>",if kind == "vector" {"vector"} else {"Coordinate"},p.x,p.y,p.z),
            Value::Plane(p) => format!("<item name=\"plane\" type_name=\"gh_plane\" type_code=\"72\"><Ox>{}</Ox><Oy>{}</Oy><Oz>{}</Oz><Xx>{}</Xx><Xy>{}</Xy><Xz>{}</Xz><Yx>{}</Yx><Yy>{}</Yy><Yz>{}</Yz></item>",p.origin.x,p.origin.y,p.origin.z,p.x_axis.x,p.x_axis.y,p.x_axis.z,p.y_axis.x,p.y_axis.y,p.y_axis.z),
            _ => return Err("This persistent geometry cannot be exported to GHX.".into()),
        };
        let items = if matches!(value, Value::Text(_)) {
            vec![item("null_string", "gh_bool", 1, "false"), data]
        } else {
            vec![data]
        };
        children.push(chunk("Item", Some(i), items, vec![]));
    }
    Ok(chunk(
        "PersistentData",
        None,
        vec![item("Count", "gh_int32", 3, "1")],
        vec![chunk(
            "Branch",
            Some(0),
            vec![
                item("Count", "gh_int32", 3, &values.len().to_string()),
                item("Path", "gh_string", 10, "{0}"),
            ],
            children,
        )],
    ))
}

pub fn export_ghx(graph: &Graph) -> Result<String, String> {
    let registry = Registry::standard();
    for edge in &graph.edges {
        let from = graph
            .nodes
            .get(&edge.from.0)
            .and_then(|n| registry.get(&n.type_name));
        let to = graph
            .nodes
            .get(&edge.to.0)
            .and_then(|n| registry.get(&n.type_name));
        if from.is_none_or(|c| edge.from.1 as usize >= c.outputs().len())
            || to.is_none_or(|c| edge.to.1 as usize >= c.inputs().len())
        {
            return Err("Cannot export a graph containing an invalid wire endpoint.".into());
        }
    }
    let mut output_ids = BTreeMap::new();
    for node in graph.nodes.values() {
        let component = registry
            .get(&node.type_name)
            .ok_or_else(|| format!("Cannot export unknown component {}", node.type_name))?;
        for i in 0..component.outputs().len() {
            let id = if [
                "number_slider",
                "bool_toggle",
                "constant_list",
                "imported_geometry",
            ]
            .contains(&node.type_name.as_str())
            {
                node.id
            } else {
                crate::util::new_node_id()
            };
            output_ids.insert((node.id, i as u16), id);
        }
    }
    let mut objects = Vec::new();
    for (index, node) in graph.nodes.values().enumerate() {
        let special = [
            "number_slider",
            "bool_toggle",
            "constant_list",
            "imported_geometry",
            "panel",
        ]
        .contains(&node.type_name.as_str());
        let mapping = MAP.iter().find(|m| m.mantis == node.type_name);
        if !special && mapping.is_none() {
            return Err(format!(
                "GHX export does not support {} (node {}). No file was written.",
                node.type_name, node.id
            ));
        }
        let component = registry.get(&node.type_name).unwrap();
        let (gh, name) = match node.type_name.as_str() {
            "number_slider" => (SLIDER, "Number Slider"),
            "bool_toggle" => (TOGGLE, "Boolean Toggle"),
            "constant_list" => {
                let values = component.eval(&[], &node.params)?;
                let Value::List(values) = &values[0] else {
                    unreachable!()
                };
                if values.iter().all(|v| matches!(v, Value::Number(_))) {
                    (NUMBER, "Number")
                } else if values.iter().all(|v| matches!(v, Value::Bool(_))) {
                    ("cb95db89-6165-43b6-9c41-5702bc5bf137", "Boolean")
                } else if values.iter().all(|v| matches!(v, Value::Text(_))) {
                    ("3ede854e-c753-40eb-84cb-b48008f14fd4", "Text")
                } else {
                    return Err("GHX stored-list export requires one scalar type per list.".into());
                }
            }
            "panel" => (PANEL, "Panel"),
            "imported_geometry" => {
                let record = GeometryRecord::from_json(
                    node.params
                        .get("data")
                        .and_then(ParamValue::as_text)
                        .ok_or("CAD Geometry has no data")?,
                )?;
                match record.geometry { GeometryData::Point {..} => ("fbac3e32-f100-4292-8692-77240a42fd1a","Point"), GeometryData::Plane {..} => ("4f8984c4-7c7a-4d69-b0a2-183cbb330d20","Plane"), _ => return Err("Baked meshes/curves require .3dm export; GHX export cannot encode their Rhino archives yet.".into()) }
            }
            _ => {
                let m = mapping.unwrap();
                (m.gh, m.name)
            }
        };
        let mut items = vec![
            item("InstanceGuid", "gh_guid", 9, &guid_text(node.id)),
            item("Name", "gh_string", 10, name),
            item(
                "NickName",
                "gh_string",
                10,
                node.params
                    .get("label")
                    .and_then(ParamValue::as_text)
                    .unwrap_or(name),
            ),
            item("Description", "gh_string", 10, name),
            item("Hidden", "gh_bool", 1, &(!node.preview()).to_string()),
        ];
        let mut children = vec![attrs(node.pos)];
        if node.type_name == "number_slider" {
            let p = |key: &str, default: f64| {
                node.params
                    .get(key)
                    .and_then(ParamValue::as_number)
                    .unwrap_or(default)
            };
            let step = p("step", 0.0);
            if step != 0.0 && step != 1.0 {
                return Err("GHX export supports continuous or integer sliders; arbitrary step sizes cannot be represented.".into());
            }
            if step == 1.0 && p("min", 0.0).min(p("max", 10.0)).fract() != 0.0 {
                return Err("Integer sliders with fractional bounds cannot be exported without changing their values.".into());
            }
            let settings = [
                ("Value", p("value", 5.0)),
                ("Min", p("min", 0.0)),
                ("Max", p("max", 10.0)),
            ]
            .into_iter()
            .map(|(k, v)| item(k, "gh_double", 6, &v.to_string()))
            .chain([
                item("Digits", "gh_int32", 3, "12"),
                item(
                    "Interval",
                    "gh_int32",
                    3,
                    if step == 1.0 { "1" } else { "0" },
                ),
                item("GripDisplay", "gh_int32", 3, "1"),
                item("SnapCount", "gh_int32", 3, "0"),
            ])
            .collect();
            children.push(chunk("Slider", None, settings, vec![]));
            items.push(item("SourceCount", "gh_int32", 3, "0"));
        } else if node.type_name == "bool_toggle" {
            items.push(item(
                "ToggleValue",
                "gh_bool",
                1,
                &node
                    .params
                    .get("value")
                    .and_then(ParamValue::as_bool)
                    .unwrap_or(false)
                    .to_string(),
            ));
            items.push(item("SourceCount", "gh_int32", 3, "0"));
        } else if node.type_name == "constant_list" {
            let values = component.eval(&[], &node.params)?;
            let Value::List(values) = &values[0] else {
                unreachable!()
            };
            children.push(persistent_xml(values)?);
            items.push(item("SourceCount", "gh_int32", 3, "0"));
        } else if node.type_name == "imported_geometry" {
            let record = GeometryRecord::from_json(
                node.params["data"]
                    .as_text()
                    .ok_or("Invalid geometry data")?,
            )?;
            children.push(persistent_xml(&[record.value()])?);
            items.push(item("SourceCount", "gh_int32", 3, "0"));
        } else if node.type_name == "panel" {
            items.push(item(
                "UserText",
                "gh_string",
                10,
                node.params
                    .get("text")
                    .and_then(ParamValue::as_text)
                    .unwrap_or(""),
            ));
            let edge = graph.incoming((node.id, 0));
            items.push(item(
                "SourceCount",
                "gh_int32",
                3,
                if edge.is_some() { "1" } else { "0" },
            ));
            if let Some(edge) = edge {
                items.push(format!("<item name=\"Source\" index=\"0\" type_name=\"gh_guid\" type_code=\"9\">{}</item>",guid_text(*output_ids.get(&edge.from).ok_or("Unknown source output")?)));
            }
        } else {
            let mapping = mapping.unwrap();
            let inputs = component.inputs();
            let outputs = component.outputs();
            let mut parameters = Vec::new();
            for i in 0..mapping.inputs {
                let name = inputs.get(i).map(|p| p.name).unwrap_or("Kinks");
                let mut pi = vec![
                    item(
                        "InstanceGuid",
                        "gh_guid",
                        9,
                        &guid_text(crate::util::new_node_id()),
                    ),
                    item("Name", "gh_string", 10, name),
                    item("NickName", "gh_string", 10, name),
                    item("Optional", "gh_bool", 1, "false"),
                ];
                let mut pc = vec![attrs(node.pos)];
                if let Some(edge) = graph.incoming((node.id, i as u16)) {
                    if node.type_name == "nurbs_curve" && i == 2 {
                        return Err(
                            "Wired periodic/closed NURBS input is not portable to Grasshopper."
                                .into(),
                        );
                    }
                    pi.push(item("SourceCount", "gh_int32", 3, "1"));
                    pi.push(format!("<item name=\"Source\" index=\"0\" type_name=\"gh_guid\" type_code=\"9\">{}</item>",guid_text(*output_ids.get(&edge.from).ok_or("Unknown source output")?)));
                } else {
                    pi.push(item("SourceCount", "gh_int32", 3, "0"));
                    let default = inputs
                        .get(i)
                        .and_then(|p| p.default.as_ref())
                        .cloned()
                        .unwrap_or(Value::Bool(false));
                    if inputs.get(i).is_none_or(|p| p.default.is_some()) {
                        let kind = match (node.type_name.as_str(), i) {
                            ("series", 2)
                            | ("divide_curve", 1)
                            | ("nurbs_curve", 1)
                            | ("list_item", 1) => "integer",
                            ("move", 1) => "vector",
                            _ => "point",
                        };
                        pc.push(match default {
                            Value::List(values) => persistent_xml_as(&values, kind)?,
                            value => persistent_xml_as(&[value], kind)?,
                        });
                    }
                }
                parameters.push(chunk(
                    if mapping.variable {
                        "InputParam"
                    } else {
                        "param_input"
                    },
                    Some(i),
                    pi,
                    pc,
                ));
            }
            for i in 0..mapping.outputs {
                let name = outputs
                    .get(i)
                    .map(|p| p.name)
                    .unwrap_or("Additional output");
                let id = output_ids
                    .get(&(node.id, i as u16))
                    .copied()
                    .unwrap_or_else(crate::util::new_node_id);
                parameters.push(chunk(
                    if mapping.variable {
                        "OutputParam"
                    } else {
                        "param_output"
                    },
                    Some(i),
                    vec![
                        item("InstanceGuid", "gh_guid", 9, &guid_text(id)),
                        item("Name", "gh_string", 10, name),
                        item("NickName", "gh_string", 10, name),
                        item("Optional", "gh_bool", 1, "false"),
                        item("SourceCount", "gh_int32", 3, "0"),
                    ],
                    vec![attrs(node.pos)],
                ));
            }
            if mapping.variable {
                let mut counts = vec![
                    item("InputCount", "gh_int32", 3, &mapping.inputs.to_string()),
                    item("OutputCount", "gh_int32", 3, &mapping.outputs.to_string()),
                ];
                for (direction, count) in
                    [("InputId", mapping.inputs), ("OutputId", mapping.outputs)]
                {
                    for i in 0..count {
                        let parameter_guid =
                            if node.type_name == "list_item" && direction == "InputId" {
                                match i {
                                    1 => "2e3ab970-8545-46bb-836c-1c11e5610bce",
                                    2 => "cb95db89-6165-43b6-9c41-5702bc5bf137",
                                    _ => DATA,
                                }
                            } else {
                                DATA
                            };
                        counts.push(format!("<item name=\"{direction}\" index=\"{i}\" type_name=\"gh_guid\" type_code=\"9\">{parameter_guid}</item>"));
                    }
                }
                children.push(chunk("ParameterData", None, counts, parameters));
            } else {
                children.extend(parameters);
            }
        }
        objects.push(chunk(
            "Object",
            Some(index),
            vec![
                item("GUID", "gh_guid", 9, gh),
                item("Name", "gh_string", 10, name),
            ],
            vec![chunk("Container", None, items, children)],
        ));
    }
    let definition = chunk("Definition",None,vec!["<item name=\"plugin_version\" type_name=\"gh_version\" type_code=\"80\"><Major>1</Major><Minor>0</Minor><Revision>8</Revision></item>".into()],vec![chunk("DocumentHeader",None,vec![item("DocumentID","gh_guid",9,&guid_text(crate::util::new_node_id()))],vec![]),chunk("DefinitionProperties",None,vec![item("Name","gh_string",10,"MantisCAD.ghx"),item("Description","gh_string",10,"Supported MantisCAD graph components")],vec![]),chunk("DefinitionObjects",None,vec![item("ObjectCount","gh_int32",3,&objects.len().to_string())],objects)]);
    Ok(format!("<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?><Archive name=\"Root\"><items count=\"1\"><item name=\"ArchiveVersion\" type_name=\"gh_version\" type_code=\"80\"><Major>0</Major><Minor>2</Minor><Revision>2</Revision></item></items><chunks count=\"1\">{definition}</chunks></Archive>"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_graph::Evaluator;
    const SERIES: &str = include_str!("../tests/fixtures/grasshopper/mcneel_sliders_series.ghx");
    const CIRCLE: &str = include_str!("../tests/fixtures/grasshopper/mcneel_circle_unit_z.ghx");
    const TEXT: &str = include_str!("../tests/fixtures/grasshopper/mcneel_text.ghx");
    fn graph(report: &ImportReport) -> Graph {
        let mut g = Graph::new();
        g.apply_all(&report.ops).unwrap();
        g
    }
    fn render(node: &Xml) -> String {
        let attrs = node
            .attrs
            .iter()
            .map(|(k, v)| format!(" {k}=\"{}\"", esc(v)))
            .collect::<String>();
        format!(
            "<{}{attrs}>{}{}</{}>",
            node.tag,
            esc(&node.text),
            node.children.iter().map(render).collect::<String>(),
            node.tag
        )
    }
    fn find_mut<'a>(node: &'a mut Xml, name: &str) -> Option<&'a mut Xml> {
        if node.name() == name {
            return Some(node);
        }
        node.children.iter_mut().find_map(|n| find_mut(n, name))
    }
    fn list_graph(values: &str) -> Graph {
        let mut g = Graph::new();
        g.apply_all(&[
            GraphOp::AddNode {
                id: NodeId(1),
                type_name: "constant_list".into(),
                pos: (0.0, 0.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "values".into(),
                value: ParamValue::Text(values.into()),
            },
        ])
        .unwrap();
        g
    }
    #[test]
    fn imports_real_mcneel_slider_values_positions_and_instance_guid_wires() {
        let report = import_ghx(SERIES).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.node_ids.len(), 4);
        let g = graph(&report);
        assert_eq!(g.edges.len(), 3);
        assert!(g.nodes.values().any(|n| n.pos.0 != 0.0));
        let eval = Evaluator::new().evaluate(&g, &Registry::standard());
        assert!(eval.errors.is_empty(), "{:?}", eval.errors);
        let series = g.nodes.values().find(|n| n.type_name == "series").unwrap();
        let Value::List(values) = &eval.outputs[&series.id][0] else {
            panic!()
        };
        assert!(!values.is_empty());
        let input = |i| {
            eval.outputs[&g.incoming((series.id, i)).unwrap().from.0][0]
                .as_number()
                .unwrap()
        };
        assert_eq!(values.len(), input(2) as usize);
        assert_eq!(values[0].as_number().unwrap(), input(0));
        if values.len() > 1 {
            assert_eq!(values[1].as_number().unwrap(), input(0) + input(1));
        }
    }
    #[test]
    fn fractional_integer_wire_semantics_are_reported_without_removing_nodes() {
        let continuous = SERIES.replace(
            "name=\"Interval\" type_name=\"gh_int32\" type_code=\"3\">1",
            "name=\"Interval\" type_name=\"gh_int32\" type_code=\"3\">0",
        );
        let report = import_ghx(&continuous).unwrap();
        assert_eq!(report.node_ids.len(), 4);
        assert!(
            report
                .warnings
                .iter()
                .any(|s| s.contains("Grasshopper rounds integer inputs")),
            "{:?}",
            report.warnings
        );
    }
    #[test]
    fn real_persistent_plane_and_numbers_construct_geometry() {
        let report = import_ghx(CIRCLE).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let g = graph(&report);
        let eval = Evaluator::new().evaluate(&g, &Registry::standard());
        assert!(eval.errors.is_empty(), "{:?}", eval.errors);
        assert!(eval.outputs.values().flatten().any(|v| match v {
            Value::Curve(_) => true,
            Value::List(l) => l.iter().any(|v| matches!(v, Value::Curve(_))),
            _ => false,
        }));
    }
    #[test]
    fn unsupported_source_omits_dependents_instead_of_using_defaults() {
        let mut source = xml(SERIES).unwrap();
        let definition = source
            .children
            .iter_mut()
            .find(|n| n.tag == "chunks")
            .unwrap()
            .children
            .iter_mut()
            .find(|n| n.name() == "Definition")
            .unwrap();
        let objects = definition
            .children
            .iter_mut()
            .find(|n| n.tag == "chunks")
            .unwrap()
            .children
            .iter_mut()
            .find(|n| n.name() == "DefinitionObjects")
            .unwrap();
        let slider = objects
            .chunks()
            .find(|n| n.string("GUID") == Some(SLIDER))
            .unwrap();
        let first_guid = slider
            .chunk("Container")
            .unwrap()
            .string("InstanceGuid")
            .unwrap();
        // Change only the first slider's type identity, preserving its wires.
        let original = SERIES.find(&format!(">{SLIDER}</item>")).unwrap();
        let mut invalid = SERIES.to_string();
        invalid.replace_range(
            original + 1..original + 1 + SLIDER.len(),
            "11111111-2222-3333-4444-555555555555",
        );
        let report = import_ghx(&invalid).unwrap();
        let g = graph(&report);
        assert!(!g.nodes.contains_key(&guid(first_guid).unwrap()));
        assert!(!g.nodes.values().any(|n| n.type_name == "series"));
        assert!(report.warnings.iter().any(|s| s.contains("upstream")));
    }
    #[test]
    fn rejects_malformed_entities_nonfinite_and_duplicate_id() {
        for input in [
            "<Archive>",
            "<!DOCTYPE Archive [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><Archive>&x;</Archive>",
            "<not-gh/>",
        ] {
            assert!(import_ghx(input).is_err());
        }
        assert!(import_ghx(&SERIES.replace("<X>", "<X>NaN")).is_err());
    }
    #[test]
    fn ghx_export_has_real_component_guids_and_roundtrips_wire_results() {
        let original = graph(&import_ghx(SERIES).unwrap());
        let output = export_ghx(&original).unwrap();
        assert!(output.contains("e64c5fb1-845c-4ab1-8911-5f338516ba67"));
        let rebuilt = graph(&import_ghx(&output).unwrap());
        let reg = Registry::standard();
        let a = Evaluator::new().evaluate(&original, &reg);
        let b = Evaluator::new().evaluate(&rebuilt, &reg);
        assert_eq!(a.outputs, b.outputs);
        if let Ok(path) = std::env::var("MANTIS_GHX_TEST_EXPORT") {
            std::fs::write(path, output).unwrap();
        }
    }
    #[test]
    fn unsupported_export_is_explicit_and_leaves_graph_unchanged() {
        let mut g = Graph::new();
        g.apply(&GraphOp::AddNode {
            id: NodeId(1),
            type_name: "box_mesh".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        let before = g.clone();
        assert!(export_ghx(&g).unwrap_err().contains("box_mesh"));
        assert_eq!(g, before);
    }
    #[test]
    fn arithmetic_wires_export_without_mutating_source() {
        for ty in ["add", "multiply"] {
            let mut g = Graph::new();
            g.apply(&GraphOp::AddNode {
                id: NodeId(1),
                type_name: ty.into(),
                pos: (0.0, 0.0),
            })
            .unwrap();
            g.apply_all(&[
                GraphOp::AddNode {
                    id: NodeId(2),
                    type_name: "number_slider".into(),
                    pos: (-160.0, 0.0),
                },
                GraphOp::AddNode {
                    id: NodeId(3),
                    type_name: "number_slider".into(),
                    pos: (-160.0, 80.0),
                },
                GraphOp::Connect {
                    from: (NodeId(2), 0),
                    to: (NodeId(1), 0),
                },
                GraphOp::Connect {
                    from: (NodeId(3), 0),
                    to: (NodeId(1), 1),
                },
            ])
            .unwrap();
            let before = g.clone();
            let exported = export_ghx(&g).unwrap();
            assert_eq!(g, before);
            let restored = graph(&import_ghx(&exported).unwrap());
            assert!(restored.incoming((NodeId(1), 0)).is_some());
            assert!(restored.incoming((NodeId(1), 1)).is_some());
            let reg = Registry::standard();
            let a = Evaluator::new().evaluate(&g, &reg);
            let b = Evaluator::new().evaluate(&restored, &reg);
            let b = &b.outputs[&NodeId(1)][0];
            let number = match b {
                Value::List(v) => v[0].as_number().unwrap(),
                _ => b.as_number().unwrap(),
            };
            assert_eq!(a.outputs[&NodeId(1)][0].as_number().unwrap(), number);
        }
    }
    #[test]
    fn cleared_gh_input_produces_no_geometry_instead_of_native_default() {
        for explicit_empty in [true, false] {
            let mut source = xml(CIRCLE).unwrap();
            let objects = find_mut(&mut source, "DefinitionObjects").unwrap();
            let circle = objects.child("chunks").unwrap().children[0].clone();
            let mut circle = circle;
            let container = find_mut(&mut circle, "Container").unwrap();
            let ports = container
                .children
                .iter_mut()
                .find(|n| n.tag == "chunks")
                .unwrap();
            let radius = ports
                .children
                .iter_mut()
                .find(|n| n.name() == "param_input" && n.index() == 1)
                .unwrap();
            let chunks = radius
                .children
                .iter_mut()
                .find(|n| n.tag == "chunks")
                .unwrap();
            chunks.children.retain(|n| n.name() != "PersistentData");
            if explicit_empty {
                chunks.children.push(Xml {
                    tag: "chunk".into(),
                    attrs: BTreeMap::from([("name".into(), "PersistentData".into())]),
                    ..Default::default()
                });
            }
            objects
                .children
                .iter_mut()
                .find(|n| n.tag == "chunks")
                .unwrap()
                .children[0] = circle;
            let report = import_ghx(&render(&source)).unwrap();
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            let g = graph(&report);
            let circle = g.nodes.values().find(|n| n.type_name == "circle").unwrap();
            let eval = Evaluator::new().evaluate(&g, &Registry::standard());
            assert_eq!(eval.outputs[&circle.id], vec![Value::List(Vec::new())]);
        }
    }
    #[test]
    fn stored_lists_keep_scalar_types_and_empty_values() {
        for values in [
            "[]",
            "[1,2.5,-3]",
            "[true,false]",
            "[\"2\",\"hello & <world>\"]",
        ] {
            let original = list_graph(values);
            let ghx = export_ghx(&original).unwrap();
            let report = import_ghx(&ghx).unwrap();
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            let reg = Registry::standard();
            let a = Evaluator::new().evaluate(&original, &reg);
            let b = Evaluator::new().evaluate(&graph(&report), &reg);
            assert_eq!(a.outputs[&NodeId(1)], b.outputs[&NodeId(1)]);
        }
        assert!(export_ghx(&list_graph("[1,true]"))
            .unwrap_err()
            .contains("one scalar type"));
    }
    #[test]
    fn real_gh_string_metadata_is_not_mistaken_for_a_boolean_value() {
        let source = xml(TEXT).unwrap();
        let container = source
            .chunk("Definition")
            .unwrap()
            .chunk("DefinitionObjects")
            .unwrap()
            .chunk("Object")
            .unwrap()
            .chunk("Container")
            .unwrap();
        let data = container
            .chunk("PersistentData")
            .unwrap()
            .chunk("Branch")
            .unwrap()
            .chunk("Item")
            .unwrap();
        assert_eq!(data.items().next().unwrap().name(), "null_string");
        let expected = data.string("string").unwrap();
        let report = import_ghx(TEXT).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let eval = Evaluator::new().evaluate(&graph(&report), &Registry::standard());
        assert_eq!(
            eval.outputs[&report.node_ids[0]],
            vec![Value::List(vec![Value::Text(expected.into())])]
        );
        let out = xml(&export_ghx(&list_graph("[\"Apple\"]")).unwrap()).unwrap();
        let item = out
            .chunk("Definition")
            .unwrap()
            .chunk("DefinitionObjects")
            .unwrap()
            .chunk("Object")
            .unwrap()
            .chunk("Container")
            .unwrap()
            .chunk("PersistentData")
            .unwrap()
            .chunk("Branch")
            .unwrap()
            .chunk("Item")
            .unwrap();
        assert_eq!(item.string("string"), Some("Apple"));
        assert_eq!(item.string("null_string"), Some("false"));
        assert_eq!(item.child("items").unwrap().attrs["count"], "2");
    }
    #[test]
    fn wired_typed_parameter_coercions_are_explicitly_rejected() {
        let output = export_ghx(&list_graph("[1]")).unwrap();
        let mut source = xml(&output).unwrap();
        let container = find_mut(&mut source, "Container").unwrap();
        find_mut(container, "SourceCount").unwrap().text = "1".into();
        let error = import_ghx(&render(&source)).err().unwrap();
        assert!(error.contains("coercion"), "{error}");
    }
    #[test]
    fn export_rejects_fractional_integer_slider_origin_and_uses_typed_list_item_ports() {
        let mut g = Graph::new();
        g.apply_all(&[
            GraphOp::AddNode {
                id: NodeId(1),
                type_name: "number_slider".into(),
                pos: (0.0, 0.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "step".into(),
                value: ParamValue::Number(1.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "min".into(),
                value: ParamValue::Number(0.5),
            },
        ])
        .unwrap();
        assert!(export_ghx(&g).unwrap_err().contains("fractional bounds"));
        let mut g = Graph::new();
        g.apply(&GraphOp::AddNode {
            id: NodeId(2),
            type_name: "list_item".into(),
            pos: (0.0, 0.0),
        })
        .unwrap();
        let root = xml(&export_ghx(&g).unwrap()).unwrap();
        let object = root
            .chunk("Definition")
            .unwrap()
            .chunk("DefinitionObjects")
            .unwrap()
            .chunk("Object")
            .unwrap();
        let params = object
            .chunk("Container")
            .unwrap()
            .chunk("ParameterData")
            .unwrap();
        let ids: Vec<_> = params
            .items()
            .filter(|n| n.name() == "InputId")
            .map(|n| n.text.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                DATA,
                "2e3ab970-8545-46bb-836c-1c11e5610bce",
                "cb95db89-6165-43b6-9c41-5702bc5bf137"
            ]
        );
        let index = ports(object.chunk("Container").unwrap(), true)[1];
        let value = index
            .chunk("PersistentData")
            .unwrap()
            .chunk("Branch")
            .unwrap()
            .chunk("Item")
            .unwrap()
            .item("number")
            .unwrap();
        assert_eq!(value.attrs["type_name"], "gh_int32");
        let vector =
            persistent_xml_as(&[Value::Vector(Vec3::new(1.0, 2.0, 3.0))], "vector").unwrap();
        assert!(vector.contains("name=\"vector\" type_name=\"gh_point3d\""));
    }
}
