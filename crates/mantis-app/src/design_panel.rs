//! A compact feature navigator and shared design parameters for the graph.
//!
//! Keep only topology and small presentation fields here: imported CAD data
//! and evaluated geometry belong to the document, never to a per-frame clone.

use crate::state::Document;
use mantis_graph::{Edge, Graph, Node, NodeId, ParamValue};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct DesignPanel {
    filter: String,
    topology: Topology,
}

#[derive(Default)]
struct Topology {
    nodes: Vec<NodeId>,
    edges: Vec<Edge>,
    order: Vec<NodeId>,
    depth: BTreeMap<NodeId, usize>,
    upstream: BTreeMap<NodeId, BTreeSet<NodeId>>,
    downstream: BTreeMap<NodeId, BTreeSet<NodeId>>,
}

impl Topology {
    fn refresh(&mut self, graph: &Graph) {
        if self.nodes.iter().copied().eq(graph.nodes.keys().copied()) && self.edges == graph.edges {
            return;
        }
        self.nodes = graph.nodes.keys().copied().collect();
        self.edges.clone_from(&graph.edges);
        self.order.clear();
        self.depth.clear();
        self.upstream = self.nodes.iter().map(|id| (*id, BTreeSet::new())).collect();
        self.downstream = self.upstream.clone();
        for edge in &self.edges {
            if graph.nodes.contains_key(&edge.from.0) && graph.nodes.contains_key(&edge.to.0) {
                self.upstream
                    .get_mut(&edge.to.0)
                    .unwrap()
                    .insert(edge.from.0);
                self.downstream
                    .get_mut(&edge.from.0)
                    .unwrap()
                    .insert(edge.to.0);
            }
        }
        // Count dependencies by feature, so a shared source connected to two
        // ports still appears once and releases its consumer exactly once.
        let mut remaining: BTreeMap<_, _> = self
            .upstream
            .iter()
            .map(|(id, sources)| (*id, sources.len()))
            .collect();
        let mut ready: BTreeSet<_> = remaining
            .iter()
            .filter_map(|(id, count)| (*count == 0).then_some(*id))
            .collect();
        while let Some(id) = ready.pop_first() {
            self.order.push(id);
            let depth = self.depth.get(&id).copied().unwrap_or(0);
            for next in &self.downstream[&id] {
                let next_depth = self.depth.entry(*next).or_default();
                *next_depth = (*next_depth).max(depth + 1);
                let count = remaining.get_mut(next).unwrap();
                *count -= 1;
                if *count == 0 {
                    ready.insert(*next);
                }
            }
        }
        // A malformed imported graph must still expose its problematic nodes.
        let visited: BTreeSet<_> = self.order.iter().copied().collect();
        self.order.extend(
            self.nodes
                .iter()
                .filter(|id| !visited.contains(id))
                .copied(),
        );
    }
}

struct Feature {
    id: NodeId,
    label: String,
    type_name: String,
    preview: bool,
    error: Option<String>,
}

fn label<'a>(node: &'a Node, doc: &'a Document) -> &'a str {
    node.params
        .get("label")
        .and_then(ParamValue::as_text)
        .filter(|label| !label.trim().is_empty())
        .or_else(|| {
            doc.registry
                .get(&node.type_name)
                .map(|component| component.label())
        })
        .unwrap_or(&node.type_name)
}

fn feature(doc: &Document, id: NodeId) -> Option<Feature> {
    let node = doc.display_graph().nodes.get(&id)?;
    Some(Feature {
        id,
        label: label(node, doc).chars().take(64).collect(),
        type_name: node.type_name.clone(),
        preview: node.preview(),
        error: doc.last_eval.errors.get(&id).cloned(),
    })
}

fn matches_filter(doc: &Document, id: NodeId, query: &str) -> bool {
    let Some(node) = doc.display_graph().nodes.get(&id) else {
        return false;
    };
    query.is_empty()
        || label(node, doc).to_lowercase().contains(query)
        || node.type_name.to_lowercase().contains(query)
}

fn select(selection: &mut BTreeSet<NodeId>, id: NodeId, additive: bool) {
    if additive {
        if !selection.insert(id) {
            selection.remove(&id);
        }
    } else {
        selection.clear();
        selection.insert(id);
    }
}

fn set_preview(doc: &mut Document, id: NodeId, preview: bool) -> Result<(), String> {
    if !doc.editable() {
        return Err("read-only: viewing chain history".into());
    }
    doc.end_gesture();
    doc.set_param(id, "__preview", ParamValue::Bool(preview))
}

struct Parameter {
    label: String,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
}

impl Parameter {
    fn from_node(node: &Node) -> Option<Self> {
        if node.type_name != "number_slider" {
            return None;
        }
        let number = |key: &str, fallback: f64| {
            node.params
                .get(key)
                .and_then(ParamValue::as_number)
                .filter(|value| value.is_finite())
                .unwrap_or(fallback)
        };
        let min = number("min", 0.0);
        let max = number("max", 10.0);
        Some(Self {
            label: node
                .params
                .get("label")
                .and_then(ParamValue::as_text)
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect(),
            value: number("value", 5.0),
            min: min.min(max),
            max: min.max(max),
            step: number("step", 0.0),
        })
    }

    fn edited_value(&self, value: f64) -> Option<f64> {
        if !value.is_finite() {
            return None;
        }
        // Match the evaluator's step origin (the lower bound), including
        // reversed min/max ranges imported from other graph tools.
        let value = if self.step > 0.0 {
            self.min + ((value - self.min) / self.step).round() * self.step
        } else {
            value
        };
        Some(value.max(self.min).min(self.max))
    }
}

impl DesignPanel {
    /// Returns true when a feature was chosen and the graph should locate it.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        selection: &mut BTreeSet<NodeId>,
        errors: &mut Vec<String>,
    ) -> bool {
        self.topology.refresh(doc.display_graph());
        let mut focus = false;
        ui.heading("Model");
        ui.weak("Features and shared design parameters");
        if !doc.editable() {
            ui.label(
                egui::RichText::new("History view · read only").color(ui.visuals().warn_fg_color),
            );
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter by name or type…")
                .desired_width(f32::INFINITY),
        );
        let query = self.filter.trim().to_lowercase();
        let filtered: Vec<_> = self
            .topology
            .order
            .iter()
            .copied()
            .filter(|id| matches_filter(doc, *id, &query))
            .collect();
        ui.separator();

        egui::ScrollArea::vertical().id_salt("model_navigator_scroll").show(ui, |ui| {
            egui::CollapsingHeader::new("Design parameters").default_open(true).show(ui, |ui| {
                let mut count = 0;
                for id in &filtered {
                    let Some(mut parameter) = doc.display_graph().nodes.get(id)
                        .and_then(Parameter::from_node) else { continue };
                    count += 1;
                    ui.push_id(("design_parameter", id.0), |ui| {
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selection.contains(id), "↗")
                                .on_hover_text("Select parameter and locate it in the graph").clicked()
                            {
                                select(selection, *id, ui.input(|i| i.modifiers.shift || i.modifiers.command));
                                focus = true;
                            }
                            let response = ui.add_enabled(doc.editable(),
                                egui::TextEdit::singleline(&mut parameter.label)
                                    .hint_text("Name this parameter…").char_limit(120)
                                    .desired_width(ui.available_width()));
                            if response.changed() {
                                doc.param_drag(*id, "label", ParamValue::Text(parameter.label.clone()));
                            }
                            if response.lost_focus() {
                                doc.end_param_drag();
                            }
                        });
                        let mut value = parameter.value;
                        let response = ui.add_enabled(doc.editable(),
                            egui::Slider::new(&mut value, parameter.min..=parameter.max)
                                .clamping(egui::SliderClamping::Edits));
                        if response.changed() {
                            if let Some(value) = parameter.edited_value(value) {
                                doc.param_drag(*id, "value", ParamValue::Number(value));
                            }
                        }
                        if response.drag_stopped() || response.lost_focus() {
                            doc.end_param_drag();
                        }
                        response.on_hover_text(format!("Range: {} to {} · Step: {}\nClick the number for precise entry; edit limits in the inspector.", parameter.min, parameter.max, parameter.step.max(0.0)));
                        ui.add_space(3.0);
                    });
                }
                if count == 0 {
                    ui.weak(if query.is_empty() { "Number sliders appear here." } else { "No matching parameters." });
                }
            });
            ui.separator();
            egui::CollapsingHeader::new(format!("Features ({}/{})", filtered.len(), self.topology.nodes.len()))
                .id_salt("model_features").default_open(true).show(ui, |ui| {
                    ui.weak("Dependency order · checkbox controls preview");
                    if filtered.is_empty() {
                        ui.weak(if query.is_empty() { "Create geometry to start your model." } else { "No matching features." });
                    }
                    for id in &filtered {
                        let Some(item) = feature(doc, *id) else { continue };
                        ui.push_id(("model_feature", id.0), |ui| {
                            ui.horizontal(|ui| {
                                let depth = self.topology.depth.get(id).copied().unwrap_or(0);
                                ui.add_space(depth.min(4) as f32 * 7.0);
                                let mut preview = item.preview;
                                if ui.add_enabled(doc.editable(), egui::Checkbox::without_text(&mut preview))
                                    .on_hover_text("Show or hide this feature's geometry preview").changed()
                                {
                                    if let Err(error) = set_preview(doc, *id, preview) {
                                        errors.push(error);
                                    }
                                }
                                let title = if item.error.is_some() {
                                    egui::RichText::new(format!("! {}", item.label)).color(ui.visuals().error_fg_color)
                                } else {
                                    egui::RichText::new(&item.label)
                                };
                                let response = ui.add(egui::Button::new(title)
                                    .selected(selection.contains(id)).frame(false).truncate());
                                if response.clicked() {
                                    select(selection, *id, ui.input(|i| i.modifiers.shift || i.modifiers.command));
                                    focus = true;
                                }
                                response.on_hover_ui(|ui| {
                                    ui.label(&item.type_name);
                                    ui.weak(item.id.to_hex());
                                    if let Some(error) = &item.error {
                                        ui.colored_label(ui.visuals().error_fg_color, error);
                                    }
                                    ui.weak("Click to locate · Shift / Cmd / Ctrl to add or remove");
                                });
                            });
                        });
                    }
                });
            ui.separator();
            if let Some(id) = selection.iter().find(|id| doc.display_graph().nodes.contains_key(id)).copied() {
                if let Some(item) = feature(doc, id) {
                    ui.strong(&item.label);
                    if selection.len() > 1 {
                        ui.weak(format!("{} selected · showing one feature", selection.len()));
                    }
                    if let Some(error) = item.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    for (heading, neighbors) in [("Depends on", self.topology.upstream.get(&id)),
                        ("Used by", self.topology.downstream.get(&id))]
                    {
                        let count = neighbors.map_or(0, BTreeSet::len);
                        ui.weak(format!("{heading} ({count})"));
                        if let Some(neighbors) = neighbors {
                            for neighbor in neighbors {
                                if let Some(item) = feature(doc, *neighbor) {
                                    if ui.link(&item.label).on_hover_text(&item.type_name).clicked() {
                                        select(selection, *neighbor, false);
                                        focus = true;
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                ui.weak("Select a feature to follow its dependencies.");
            }
        });
        focus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_graph::GraphOp;

    fn document() -> Document {
        let mut doc = Document::new(Identity::generate("design-panel-test"));
        for (id, kind) in [
            (1, "add"),
            (2, "number_slider"),
            (3, "number_slider"),
            (4, "multiply"),
        ] {
            doc.apply_op(GraphOp::AddNode {
                id: NodeId(id),
                type_name: kind.into(),
                pos: (0.0, 0.0),
            })
            .unwrap();
        }
        for (from, to, port) in [(2, 1, 0), (3, 1, 1), (1, 4, 0), (2, 4, 1)] {
            doc.apply_op(GraphOp::Connect {
                from: (NodeId(from), 0),
                to: (NodeId(to), port),
            })
            .unwrap();
        }
        doc
    }

    #[test]
    fn dependency_order_lists_shared_sources_once_and_tracks_both_directions() {
        let mut doc = document();
        // Another port from the same source is one feature dependency.
        doc.apply_op(GraphOp::Connect {
            from: (NodeId(2), 0),
            to: (NodeId(4), 2),
        })
        .unwrap();
        let mut topology = Topology::default();
        topology.refresh(doc.display_graph());
        assert_eq!(
            topology.order,
            vec![NodeId(2), NodeId(3), NodeId(1), NodeId(4)]
        );
        assert_eq!(
            topology.upstream[&NodeId(4)],
            BTreeSet::from([NodeId(1), NodeId(2)])
        );
        assert_eq!(
            topology.downstream[&NodeId(2)],
            BTreeSet::from([NodeId(1), NodeId(4)])
        );
        assert_eq!(topology.depth[&NodeId(4)], 2);
        doc.apply_op(GraphOp::RemoveNode { id: NodeId(1) }).unwrap();
        topology.refresh(doc.display_graph());
        assert_eq!(topology.order, vec![NodeId(2), NodeId(3), NodeId(4)]);
        assert_eq!(topology.depth[&NodeId(4)], 1);
    }

    #[test]
    fn labels_are_searchable_and_selection_keeps_existing_editor_semantics() {
        let mut doc = document();
        doc.set_param(NodeId(2), "label", ParamValue::Text("Wing Span".into()))
            .unwrap();
        assert!(matches_filter(&doc, NodeId(2), "wing"));
        assert!(matches_filter(&doc, NodeId(2), "number_slider"));
        assert!(!matches_filter(&doc, NodeId(3), "wing"));
        let mut selection = BTreeSet::from([NodeId(1)]);
        select(&mut selection, NodeId(2), false);
        select(&mut selection, NodeId(3), true);
        assert_eq!(selection, BTreeSet::from([NodeId(2), NodeId(3)]));
        select(&mut selection, NodeId(2), true);
        assert_eq!(selection, BTreeSet::from([NodeId(3)]));
    }

    #[test]
    fn parameter_edits_match_evaluation_and_coalesce_into_one_undo_step() {
        let mut doc = document();
        for (key, value) in [("min", 9.0), ("max", 1.0), ("step", 2.0), ("value", 3.0)] {
            doc.set_param(NodeId(2), key, ParamValue::Number(value))
                .unwrap();
        }
        let parameter = Parameter::from_node(&doc.graph.nodes[&NodeId(2)]).unwrap();
        let before = doc.pending.len();
        for value in [4.2, 7.2, 8.8] {
            doc.param_drag(
                NodeId(2),
                "value",
                ParamValue::Number(parameter.edited_value(value).unwrap()),
            );
        }
        doc.end_param_drag();
        assert_eq!(doc.pending.len(), before + 1);
        assert_eq!(
            doc.graph.nodes[&NodeId(2)].params["value"],
            ParamValue::Number(9.0)
        );
        doc.evaluate();
        assert_eq!(doc.last_eval.outputs[&NodeId(2)][0].as_number(), Some(9.0));
        assert!(parameter.edited_value(f64::NAN).is_none());
        doc.undo_pending().unwrap();
        assert_eq!(
            doc.graph.nodes[&NodeId(2)].params["value"],
            ParamValue::Number(3.0)
        );
        doc.redo_pending().unwrap();
        let mut replay = doc.chain.replay(None).unwrap();
        replay.apply_all(&doc.pending).unwrap();
        assert_eq!(replay.nodes, doc.graph.nodes);
    }

    #[test]
    fn preview_finishes_parameter_edit_and_history_stays_read_only() {
        let mut doc = document();
        doc.param_drag(NodeId(2), "label", ParamValue::Text("Span".into()));
        set_preview(&mut doc, NodeId(2), false).unwrap();
        assert!(!doc.graph.nodes[&NodeId(2)].preview());
        doc.undo_pending().unwrap();
        assert!(doc.graph.nodes[&NodeId(2)].preview());
        assert_eq!(
            doc.graph.nodes[&NodeId(2)].params["label"],
            ParamValue::Text("Span".into())
        );
        doc.commit("model", 1).unwrap();
        doc.set_param(NodeId(2), "value", ParamValue::Number(7.0))
            .unwrap();
        doc.commit("parameter", 2).unwrap();
        doc.set_view(Some(1)).unwrap();
        assert!(set_preview(&mut doc, NodeId(2), false).is_err());
        doc.param_drag(NodeId(2), "value", ParamValue::Number(8.0));
        assert!(doc.pending.is_empty());
        assert_eq!(
            Parameter::from_node(&doc.display_graph().nodes[&NodeId(2)])
                .unwrap()
                .value,
            5.0
        );
    }
}
