//! Hand-rolled Grasshopper-style node editor drawn on an egui canvas.
//!
//! World space = graph coordinates (`Node::pos`); the view transform
//! (pan + zoom) maps world to screen. All edits go through
//! [`Document::apply_op`] / the gesture helpers so pending ops stay coalesced
//! (one `SetParam` per slider drag, one `MoveNode` per node drag).

use crate::state::Document;
use crate::util::new_node_id;
use mantis_graph::{Edge, GraphOp, NodeId, ParamValue, ValueKind};
use std::collections::{BTreeMap, BTreeSet};

/// Zoom limits.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 2.5;

// World-space node metrics.
const NODE_W: f32 = 180.0;
const TITLE_H: f32 = 24.0;
const ROW_H: f32 = 20.0;
const PORT_R: f32 = 5.0;

// Colors.
const BG: egui::Color32 = egui::Color32::from_rgb(0x20, 0x23, 0x2a);
const GRID: egui::Color32 = egui::Color32::from_rgb(0x28, 0x2c, 0x35);
const NODE_BODY: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x2e, 0x37);
const NODE_BODY_HOVER: egui::Color32 = egui::Color32::from_rgb(0x31, 0x36, 0x40);
const NODE_TITLE: egui::Color32 = egui::Color32::from_rgb(0x23, 0x27, 0x2e);
const NODE_OUTLINE: egui::Color32 = egui::Color32::from_rgb(0x14, 0x16, 0x1a);
const SELECT: egui::Color32 = egui::Color32::from_rgb(0xe8, 0x96, 0x3c);
const ERROR: egui::Color32 = egui::Color32::from_rgb(0xe8, 0x5a, 0x50);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xd8, 0xdc, 0xe4);
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(0xa0, 0xa6, 0xb0);
const BANNER: egui::Color32 = egui::Color32::from_rgb(0xe8, 0xc0, 0x6a);

/// Wire/port color for a value kind (Any = gray).
pub fn kind_color(kind: ValueKind) -> egui::Color32 {
    match kind {
        ValueKind::Any => egui::Color32::from_rgb(0x96, 0x98, 0xa0),
        ValueKind::Number => egui::Color32::from_rgb(0x60, 0xaa, 0xff),
        ValueKind::Bool => egui::Color32::from_rgb(0xc4, 0x82, 0xff),
        ValueKind::Text => egui::Color32::from_rgb(0xe8, 0xd2, 0x6e),
        ValueKind::Vector => egui::Color32::from_rgb(0x78, 0xd6, 0x82),
        ValueKind::Plane => egui::Color32::from_rgb(0x5a, 0xd2, 0xc8),
        ValueKind::Curve => egui::Color32::from_rgb(0xf0, 0xf0, 0xf5),
        ValueKind::Mesh => egui::Color32::from_rgb(0xe8, 0x96, 0x3c),
    }
}

/// Category strip color for the palette / node title bar.
pub fn category_color(category: &str) -> egui::Color32 {
    match category {
        "Params" => egui::Color32::from_rgb(0xe8, 0x96, 0x3c),
        "Maths" => egui::Color32::from_rgb(0x60, 0xaa, 0xff),
        "Vector" => egui::Color32::from_rgb(0x78, 0xd6, 0x82),
        "Curve" => egui::Color32::from_rgb(0x5a, 0xd2, 0xc8),
        "Surface" => egui::Color32::from_rgb(0xc4, 0x82, 0xff),
        "Transform" => egui::Color32::from_rgb(0xe8, 0xd2, 0x6e),
        "Sets" => egui::Color32::from_rgb(0xf0, 0x8a, 0xb4),
        "Analysis" => egui::Color32::from_rgb(0xe8, 0x5a, 0x50),
        _ => egui::Color32::from_rgb(0x96, 0x98, 0xa0),
    }
}

/// Can an output of kind `from` plug into an input of kind `to`?
/// Mirrors the engine's implicit coercions (`Value::kind_matches` /
/// `as_plane` / `as_bool`): a point plugs into a `Plane` port, a number into
/// a `Bool` port.
pub fn kinds_compatible(from: ValueKind, to: ValueKind) -> bool {
    from == ValueKind::Any
        || to == ValueKind::Any
        || from == to
        || (to == ValueKind::Plane && from == ValueKind::Vector)
        || (to == ValueKind::Bool && from == ValueKind::Number)
}

// ---------------------------------------------------------------------------
// view transform (pure — unit tested)
// ---------------------------------------------------------------------------

/// Pan/zoom transform between world (graph) and screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewXf {
    pub origin: egui::Pos2,
    pub pan: egui::Vec2,
    pub zoom: f32,
}

impl ViewXf {
    pub fn to_screen(&self, world: egui::Pos2) -> egui::Pos2 {
        self.origin + self.pan + world.to_vec2() * self.zoom
    }
    pub fn to_world(&self, screen: egui::Pos2) -> egui::Pos2 {
        (((screen - self.origin) - self.pan) / self.zoom).to_pos2()
    }
}

/// Zoom by `factor` keeping the world point under `cursor` fixed on screen.
/// Returns the new (pan, zoom).
pub fn zoom_at(
    pan: egui::Vec2,
    zoom: f32,
    origin: egui::Pos2,
    cursor: egui::Pos2,
    factor: f32,
) -> (egui::Vec2, f32) {
    let new_zoom = (zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let world = ((cursor - origin) - pan) / zoom;
    let new_pan = (cursor - origin) - world * new_zoom;
    (new_pan, new_zoom)
}

// ---------------------------------------------------------------------------
// per-frame node layout
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum BodyWidget {
    None,
    Slider { value: f64, min: f64, max: f64, step: f64 },
    Toggle { value: bool },
    Panel { display: Option<String>, text: String },
}

fn widget_height(w: &BodyWidget) -> f32 {
    match w {
        BodyWidget::None => 0.0,
        BodyWidget::Slider { .. } => 42.0,
        BodyWidget::Toggle { .. } => 22.0,
        BodyWidget::Panel { .. } => 24.0,
    }
}

struct PortPin {
    pos: egui::Pos2,
    name: String,
    kind: ValueKind,
}

struct Layout {
    id: NodeId,
    rect: egui::Rect,
    ins: Vec<PortPin>,
    outs: Vec<PortPin>,
    label: String,
    category: String,
    widget: BodyWidget,
    widget_rect: egui::Rect,
    error: Option<String>,
    preview: bool,
}

// ---------------------------------------------------------------------------
// editor state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct WireDrag {
    from: (NodeId, u16),
    kind: ValueKind,
}

struct AddMenu {
    screen: egui::Pos2,
    world: (f32, f32),
    search: String,
    focus_search: bool,
    just_opened: bool,
}

/// The node editor panel: view transform, selection and in-flight gestures.
pub struct NodeEditor {
    pub pan: egui::Vec2,
    pub zoom: f32,
    pub selection: BTreeSet<NodeId>,
    wire_drag: Option<WireDrag>,
    add_menu: Option<AddMenu>,
}

impl Default for NodeEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeEditor {
    pub fn new() -> NodeEditor {
        NodeEditor {
            pan: egui::vec2(60.0, 40.0),
            zoom: 1.0,
            selection: BTreeSet::new(),
            wire_drag: None,
            add_menu: None,
        }
    }

    /// Draw + interact. Any op errors are pushed into `errors` (toasted by
    /// the app). Editing is disabled while `doc` is time traveling.
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Document, errors: &mut Vec<String>) {
        let rect = ui.available_rect_before_wrap();
        if rect.width() < 20.0 || rect.height() < 20.0 {
            return;
        }
        let canvas = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let editable = doc.editable();

        // ---- view navigation --------------------------------------------
        if canvas.dragged_by(egui::PointerButton::Primary) && self.wire_drag.is_none() {
            self.pan += canvas.drag_delta();
        }
        let (middle_down, pointer_delta) =
            ui.input(|i| (i.pointer.middle_down(), i.pointer.delta()));
        if middle_down && ui.rect_contains_pointer(rect) {
            self.pan += pointer_delta;
        }
        if ui.rect_contains_pointer(rect) && self.add_menu.is_none() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.1 {
                if let Some(cursor) = ui.input(|i| i.pointer.hover_pos()) {
                    let (p, z) =
                        zoom_at(self.pan, self.zoom, rect.min, cursor, (scroll * 0.0035).exp());
                    self.pan = p;
                    self.zoom = z;
                }
            }
        }
        let xf = ViewXf { origin: rect.min, pan: self.pan, zoom: self.zoom };
        let z = self.zoom;

        // ---- background ---------------------------------------------------
        painter.rect_filled(rect, egui::CornerRadius::ZERO, BG);
        self.draw_grid(&painter, rect, &xf);

        // ---- canvas click / context menu -----------------------------------
        if canvas.clicked() {
            self.selection.clear();
            self.add_menu = None;
        }
        if canvas.secondary_clicked() && editable {
            if let Some(p) = canvas.interact_pointer_pos() {
                let w = xf.to_world(p);
                self.add_menu = Some(AddMenu {
                    screen: p,
                    world: (w.x, w.y),
                    search: String::new(),
                    focus_search: true,
                    just_opened: true,
                });
            }
        }

        // ---- layouts + wires ----------------------------------------------
        let layouts = build_layouts(doc, &xf);
        let index: BTreeMap<NodeId, usize> =
            layouts.iter().enumerate().map(|(i, l)| (l.id, i)).collect();
        let edges: Vec<Edge> = doc.display_graph().edges.clone();

        let wire_w = (2.0 * z).clamp(1.0, 3.5);
        for e in &edges {
            let (Some(&fi), Some(&ti)) = (index.get(&e.from.0), index.get(&e.to.0)) else {
                continue;
            };
            let (p0, kind) = match layouts[fi].outs.get(e.from.1 as usize) {
                Some(pin) => (pin.pos, pin.kind),
                None => (layouts[fi].rect.right_center(), ValueKind::Any),
            };
            let p1 = match layouts[ti].ins.get(e.to.1 as usize) {
                Some(pin) => pin.pos,
                None => layouts[ti].rect.left_center(),
            };
            draw_wire(&painter, p0, p1, kind_color(kind), wire_w);
        }

        // ---- nodes ----------------------------------------------------------
        let shift = ui.input(|i| i.modifiers.shift);
        for l in &layouts {
            self.node_ui(ui, &painter, doc, l, &layouts, &index, &edges, shift, editable, errors);
        }

        // ---- ghost wire / wire release ---------------------------------------
        self.wire_drag_ui(ui, &painter, doc, &layouts, &index, wire_w, editable, errors);

        // ---- keyboard -----------------------------------------------------
        self.keyboard(ui, doc, errors, editable);

        // ---- add-node popup -------------------------------------------------
        self.add_menu_ui(ui, doc, errors);

        // ---- overlays -------------------------------------------------------
        if !editable {
            painter.text(
                rect.center_top() + egui::vec2(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                "viewing chain history — read-only",
                egui::FontId::proportional(13.0),
                BANNER,
            );
        } else if layouts.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "right-click to add a component",
                egui::FontId::proportional(14.0),
                TEXT_DIM,
            );
        }
        painter.text(
            rect.left_bottom() + egui::vec2(8.0, -6.0),
            egui::Align2::LEFT_BOTTOM,
            "right-click add · drag ports to wire · Del remove · Esc deselect · scroll zoom",
            egui::FontId::proportional(10.5),
            egui::Color32::from_rgb(0x6e, 0x76, 0x82),
        );
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: egui::Rect, xf: &ViewXf) {
        let step_world = 50.0;
        let step = step_world * self.zoom;
        if step < 9.0 {
            return;
        }
        let stroke = egui::Stroke::new(1.0, GRID);
        let w0 = xf.to_world(rect.min);
        let w1 = xf.to_world(rect.max);
        let mut x = (w0.x / step_world).floor() * step_world;
        let mut guard = 0;
        while x <= w1.x && guard < 400 {
            let sx = xf.to_screen(egui::pos2(x, 0.0)).x;
            painter.vline(sx, rect.y_range(), stroke);
            x += step_world;
            guard += 1;
        }
        let mut y = (w0.y / step_world).floor() * step_world;
        while y <= w1.y && guard < 800 {
            let sy = xf.to_screen(egui::pos2(0.0, y)).y;
            painter.hline(rect.x_range(), sy, stroke);
            y += step_world;
            guard += 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        doc: &mut Document,
        l: &Layout,
        layouts: &[Layout],
        index: &BTreeMap<NodeId, usize>,
        edges: &[Edge],
        shift: bool,
        editable: bool,
        errors: &mut Vec<String>,
    ) {
        let z = self.zoom;
        let sel = self.selection.contains(&l.id);
        let base_id = ui.id().with(("node", l.id.0));
        let mut resp = ui.interact(l.rect, base_id, egui::Sense::click_and_drag());
        if let Some(err) = &l.error {
            resp = resp.on_hover_text(egui::RichText::new(err).color(ERROR));
        }

        // -- selection & node dragging --
        if resp.clicked() {
            if shift {
                if !self.selection.remove(&l.id) {
                    self.selection.insert(l.id);
                }
            } else {
                self.selection.clear();
                self.selection.insert(l.id);
            }
        }
        if editable && resp.drag_started_by(egui::PointerButton::Primary) {
            if !self.selection.contains(&l.id) {
                if !shift {
                    self.selection.clear();
                }
                self.selection.insert(l.id);
            }
            doc.begin_move(self.selection.iter().copied());
        }
        if editable && resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta() / z;
            if d != egui::Vec2::ZERO {
                let moves: Vec<(NodeId, (f32, f32))> = self
                    .selection
                    .iter()
                    .filter_map(|id| {
                        doc.graph
                            .nodes
                            .get(id)
                            .map(|n| (*id, (n.pos.0 + d.x, n.pos.1 + d.y)))
                    })
                    .collect();
                for (id, pos) in moves {
                    doc.move_live(id, pos);
                }
            }
        }
        if editable && resp.drag_stopped_by(egui::PointerButton::Primary) {
            doc.end_move();
        }

        // -- body / title chrome --
        let rounding = egui::CornerRadius::same((6.0 * z).clamp(2.0, 8.0) as u8);
        painter.rect_filled(l.rect, rounding, if resp.hovered() { NODE_BODY_HOVER } else { NODE_BODY });
        let title_rect =
            egui::Rect::from_min_max(l.rect.min, egui::pos2(l.rect.max.x, l.rect.min.y + TITLE_H * z));
        painter.rect_filled(title_rect, rounding, NODE_TITLE);
        let strip = egui::Rect::from_min_max(
            title_rect.min,
            egui::pos2(title_rect.min.x + 4.0 * z, title_rect.max.y),
        );
        painter.rect_filled(strip, egui::CornerRadius::ZERO, category_color(&l.category));
        let (oc, ow) = if l.error.is_some() {
            (ERROR, 2.0)
        } else if sel {
            (SELECT, 2.0)
        } else {
            (NODE_OUTLINE, 1.0)
        };
        painter.rect_stroke(l.rect, rounding, egui::Stroke::new(ow, oc), egui::StrokeKind::Outside);
        if z >= 0.4 {
            painter.text(
                egui::pos2(strip.max.x + 5.0 * z, title_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &l.label,
                egui::FontId::proportional((12.0 * z).clamp(8.0, 18.0)),
                TEXT,
            );
        }
        if l.error.is_some() {
            painter.text(
                egui::pos2(title_rect.max.x - 11.0 * z, title_rect.center().y),
                egui::Align2::CENTER_CENTER,
                "⚠",
                egui::FontId::proportional((12.0 * z).clamp(9.0, 18.0)),
                ERROR,
            );
        }

        // -- preview toggle (small dot in the title bar) --
        let eye_x = title_rect.max.x - (if l.error.is_some() { 26.0 } else { 12.0 }) * z;
        let eye_c = egui::pos2(eye_x, title_rect.center().y);
        let eye_rect = egui::Rect::from_center_size(eye_c, egui::vec2(13.0, 13.0) * z.max(0.7));
        let er = ui
            .interact(eye_rect, base_id.with("eye"), egui::Sense::click())
            .on_hover_text(if l.preview { "preview: on" } else { "preview: off" });
        if er.clicked() && editable {
            if let Err(e) = doc.set_param(l.id, "__preview", ParamValue::Bool(!l.preview)) {
                errors.push(e);
            }
        }
        let eye_col = if l.preview {
            egui::Color32::from_rgb(0x7d, 0x9f, 0xc4)
        } else {
            egui::Color32::from_rgb(0x4a, 0x4f, 0x58)
        };
        painter.circle(
            eye_c,
            (3.5 * z).max(2.0),
            if l.preview { eye_col } else { egui::Color32::TRANSPARENT },
            egui::Stroke::new(1.0, eye_col),
        );

        // -- ports --
        let show_names = z >= 0.6;
        let font = egui::FontId::proportional((10.5 * z).clamp(8.0, 16.0));
        let hit = egui::vec2(14.0, 14.0) * z.max(0.7);
        for (i, pin) in l.ins.iter().enumerate() {
            let pr = ui
                .interact(
                    egui::Rect::from_center_size(pin.pos, hit),
                    base_id.with(("in", i)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_text(format!("{} · {:?}", pin.name, pin.kind));
            let r = if pr.hovered() { PORT_R * z * 1.4 } else { PORT_R * z };
            painter.circle(pin.pos, r.max(2.5), kind_color(pin.kind), egui::Stroke::new(1.0, NODE_OUTLINE));
            if show_names {
                painter.text(
                    pin.pos + egui::vec2(9.0 * z, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &pin.name,
                    font.clone(),
                    TEXT_DIM,
                );
            }
            // Grabbing a wired input detaches its wire and re-drags it.
            if editable && pr.drag_started_by(egui::PointerButton::Primary) {
                if let Some(edge) = edges.iter().find(|e| e.to == (l.id, i as u16)).copied() {
                    match doc.apply_op(GraphOp::Disconnect { from: edge.from, to: edge.to }) {
                        Ok(()) => {
                            let kind = index
                                .get(&edge.from.0)
                                .and_then(|&fi| layouts[fi].outs.get(edge.from.1 as usize))
                                .map(|p| p.kind)
                                .unwrap_or(ValueKind::Any);
                            self.wire_drag = Some(WireDrag { from: edge.from, kind });
                        }
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
        for (j, pin) in l.outs.iter().enumerate() {
            let pr = ui
                .interact(
                    egui::Rect::from_center_size(pin.pos, hit),
                    base_id.with(("out", j)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_text(format!("{} · {:?}", pin.name, pin.kind));
            let r = if pr.hovered() { PORT_R * z * 1.4 } else { PORT_R * z };
            painter.circle(pin.pos, r.max(2.5), kind_color(pin.kind), egui::Stroke::new(1.0, NODE_OUTLINE));
            if show_names {
                painter.text(
                    pin.pos - egui::vec2(9.0 * z, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    &pin.name,
                    font.clone(),
                    TEXT_DIM,
                );
            }
            if editable && pr.drag_started_by(egui::PointerButton::Primary) {
                self.wire_drag = Some(WireDrag { from: (l.id, j as u16), kind: pin.kind });
            }
        }

        // -- body widgets --
        if z >= 0.5 && !matches!(l.widget, BodyWidget::None) {
            self.widget_ui(ui, painter, doc, l, editable, errors);
        }
    }

    fn widget_ui(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        doc: &mut Document,
        l: &Layout,
        editable: bool,
        errors: &mut Vec<String>,
    ) {
        let wr = l.widget_rect;
        let font = egui::FontId::proportional((11.0 * self.zoom).clamp(8.0, 16.0));
        // Scope per node: the scope restores the parent's auto-id counter, so
        // how many widgets an arm creates cannot shift the auto-derived ids of
        // later nodes' widgets (breaking their in-flight drags/focus). The
        // explicit max_rect pins the scope's min_rect inside the canvas — with
        // the default max_rect an empty scope reports its min_rect at the
        // parent cursor below the canvas, growing the resizable panel each
        // repaint.
        let scope = egui::UiBuilder::new()
            .id_salt(("widget", l.id.0))
            .max_rect(painter.clip_rect());
        ui.scope_builder(scope, |ui| match l.widget.clone() {
            BodyWidget::None => {}
            BodyWidget::Slider { mut value, mut min, mut max, step } => {
                if !editable {
                    painter.text(
                        wr.left_center(),
                        egui::Align2::LEFT_CENTER,
                        format!("{value:.3}"),
                        font,
                        TEXT_DIM,
                    );
                    return;
                }
                let sh = wr.height() * 0.52;
                let s_rect = egui::Rect::from_min_size(wr.min, egui::vec2(wr.width(), sh));
                let hi = if max > min { max } else { min + 1.0 };
                let mut slider = egui::Slider::new(&mut value, min..=hi);
                if step > 0.0 && step.is_finite() {
                    slider = slider.step_by(step);
                }
                let sr = put_at(ui, (l.id.0, "value"), s_rect, slider);
                if sr.changed() {
                    doc.param_drag(l.id, "value", ParamValue::Number(value));
                }
                if sr.drag_stopped() || sr.lost_focus() {
                    doc.end_param_drag();
                }
                let m_rect = egui::Rect::from_min_max(
                    egui::pos2(wr.min.x, wr.min.y + sh + 2.0),
                    wr.max,
                );
                let half = m_rect.width() * 0.5;
                let mn_rect =
                    egui::Rect::from_min_size(m_rect.min, egui::vec2(half - 2.0, m_rect.height()));
                let mx_rect = egui::Rect::from_min_size(
                    egui::pos2(m_rect.min.x + half + 2.0, m_rect.min.y),
                    egui::vec2(half - 2.0, m_rect.height()),
                );
                let mr = put_at(
                    ui,
                    (l.id.0, "min"),
                    mn_rect,
                    egui::DragValue::new(&mut min).speed(0.1).prefix("min "),
                );
                if mr.changed() {
                    doc.param_drag(l.id, "min", ParamValue::Number(min));
                }
                if mr.drag_stopped() || mr.lost_focus() {
                    doc.end_param_drag();
                }
                let xr = put_at(
                    ui,
                    (l.id.0, "max"),
                    mx_rect,
                    egui::DragValue::new(&mut max).speed(0.1).prefix("max "),
                );
                if xr.changed() {
                    doc.param_drag(l.id, "max", ParamValue::Number(max));
                }
                if xr.drag_stopped() || xr.lost_focus() {
                    doc.end_param_drag();
                }
            }
            BodyWidget::Toggle { mut value } => {
                if !editable {
                    painter.text(
                        wr.left_center(),
                        egui::Align2::LEFT_CENTER,
                        value.to_string(),
                        font,
                        TEXT_DIM,
                    );
                    return;
                }
                let label = if value { "true" } else { "false" };
                let cr = put_at(ui, (l.id.0, "toggle"), wr, egui::Checkbox::new(&mut value, label));
                if cr.changed() {
                    if let Err(e) = doc.set_param(l.id, "value", ParamValue::Bool(value)) {
                        errors.push(e);
                    }
                }
            }
            BodyWidget::Panel { display, mut text } => {
                match display {
                    Some(s) => {
                        // Wired: show the (truncated) description of the value.
                        let shown: String = s.chars().take(30).collect();
                        painter.text(
                            wr.left_center(),
                            egui::Align2::LEFT_CENTER,
                            shown,
                            font,
                            TEXT,
                        );
                    }
                    None => {
                        if editable {
                            let tr =
                                put_at(ui, (l.id.0, "text"), wr, egui::TextEdit::singleline(&mut text));
                            if tr.changed() {
                                doc.param_drag(l.id, "text", ParamValue::Text(text));
                            }
                            if tr.lost_focus() {
                                doc.end_param_drag();
                            }
                        } else {
                            painter.text(
                                wr.left_center(),
                                egui::Align2::LEFT_CENTER,
                                text,
                                font,
                                TEXT_DIM,
                            );
                        }
                    }
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn wire_drag_ui(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        doc: &mut Document,
        layouts: &[Layout],
        index: &BTreeMap<NodeId, usize>,
        wire_w: f32,
        editable: bool,
        errors: &mut Vec<String>,
    ) {
        let Some(wd) = self.wire_drag else { return };
        let snap = 14.0 * self.zoom.max(0.7);
        let Some(&fi) = index.get(&wd.from.0) else {
            self.wire_drag = None; // source node vanished mid-drag
            return;
        };
        let p0 = layouts[fi]
            .outs
            .get(wd.from.1 as usize)
            .map(|p| p.pos)
            .unwrap_or_else(|| layouts[fi].rect.right_center());
        if let Some(ptr) = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos())) {
            draw_wire(painter, p0, ptr, kind_color(wd.kind), wire_w);
            if let Some((_, _, k, pos)) = nearest_in_port(layouts, ptr, snap) {
                let ok = kinds_compatible(wd.kind, k);
                painter.circle_stroke(
                    pos,
                    PORT_R * self.zoom * 2.0,
                    egui::Stroke::new(2.0, if ok { kind_color(wd.kind) } else { ERROR }),
                );
            }
        }
        if ui.input(|i| i.pointer.any_released()) {
            self.wire_drag = None;
            if !editable {
                return;
            }
            if let Some(ptr) = ui.input(|i| i.pointer.interact_pos()) {
                if let Some((nid, pi, k, _)) = nearest_in_port(layouts, ptr, snap) {
                    if kinds_compatible(wd.kind, k) {
                        if let Err(e) =
                            doc.apply_op(GraphOp::Connect { from: wd.from, to: (nid, pi) })
                        {
                            errors.push(e);
                        }
                    }
                }
            }
        }
    }

    fn keyboard(
        &mut self,
        ui: &mut egui::Ui,
        doc: &mut Document,
        errors: &mut Vec<String>,
        editable: bool,
    ) {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.add_menu.is_some() {
                self.add_menu = None;
            } else if self.wire_drag.is_some() {
                self.wire_drag = None;
            } else {
                self.selection.clear();
            }
        }
        if editable && !ui.ctx().wants_keyboard_input() && !self.selection.is_empty() {
            let del = ui.input(|i| {
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
            });
            if del {
                let ids: Vec<NodeId> = self.selection.iter().copied().collect();
                self.selection.clear();
                for id in ids {
                    if let Err(e) = doc.apply_op(GraphOp::RemoveNode { id }) {
                        errors.push(e);
                    }
                }
            }
        }
    }

    fn add_menu_ui(&mut self, ui: &mut egui::Ui, doc: &mut Document, errors: &mut Vec<String>) {
        let mut add: Option<String> = None;
        let mut close = false;
        if let Some(menu) = &mut self.add_menu {
            let just_opened = std::mem::take(&mut menu.just_opened);
            let area = egui::Area::new(ui.id().with("add_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(menu.screen)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_width(220.0);
                        let sr = ui.add(
                            egui::TextEdit::singleline(&mut menu.search)
                                .hint_text("search components…"),
                        );
                        if std::mem::take(&mut menu.focus_search) {
                            sr.request_focus();
                        }
                        let q = menu.search.trim().to_lowercase();
                        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            let mut by_cat: BTreeMap<&'static str, Vec<(&'static str, &'static str)>> =
                                BTreeMap::new();
                            for c in doc.registry.iter() {
                                let hit = q.is_empty()
                                    || c.label().to_lowercase().contains(&q)
                                    || c.type_name().contains(&q)
                                    || c.category().to_lowercase().contains(&q);
                                if hit {
                                    by_cat
                                        .entry(c.category())
                                        .or_default()
                                        .push((c.type_name(), c.label()));
                                }
                            }
                            if by_cat.is_empty() {
                                ui.weak("no matching component");
                            }
                            for (cat, comps) in by_cat {
                                ui.label(
                                    egui::RichText::new(cat).small().color(category_color(cat)),
                                );
                                for (type_name, label) in comps {
                                    if ui.selectable_label(false, label).clicked() {
                                        add = Some(type_name.to_string());
                                    }
                                }
                                ui.add_space(4.0);
                            }
                        });
                    });
                });
            if !just_opened
                && ui.input(|i| i.pointer.any_pressed())
                && !area.response.contains_pointer()
            {
                close = true;
            }
        }
        if let Some(type_name) = add {
            if let Some(menu) = &self.add_menu {
                let id = new_node_id();
                match doc.apply_op(GraphOp::AddNode { id, type_name, pos: menu.world }) {
                    Ok(()) => {
                        self.selection.clear();
                        self.selection.insert(id);
                    }
                    Err(e) => errors.push(e),
                }
            }
            close = true;
        }
        if close {
            self.add_menu = None;
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Place a widget at an exact screen rect *without* allocating space in the
/// parent `Ui` (unlike `Ui::put`).
///
/// Node-body widgets sit at pan/zoom-dependent canvas positions, so their
/// rects can stick out of the panel. `Ui::put` grows the parent's `min_rect`
/// to include the widget, and a resizable panel stores its content `min_rect`
/// as next frame's size — a node outside the canvas would then inflate the
/// node-editor panel on every repaint, squeezing the 3D viewport away.
fn put_at(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    rect: egui::Rect,
    widget: impl egui::Widget,
) -> egui::Response {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id_salt)
            .max_rect(rect)
            .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown)),
    );
    child.add(widget)
}

/// Cubic bezier with horizontal tangents between two port positions.
fn draw_wire(painter: &egui::Painter, a: egui::Pos2, b: egui::Pos2, color: egui::Color32, width: f32) {
    let dx = ((b.x - a.x).abs() * 0.5).max(30.0);
    let shape = egui::epaint::CubicBezierShape::from_points_stroke(
        [a, a + egui::vec2(dx, 0.0), b - egui::vec2(dx, 0.0), b],
        false,
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(width, color),
    );
    painter.add(shape);
}

/// Closest input port within `max_dist` of `p`.
fn nearest_in_port(
    layouts: &[Layout],
    p: egui::Pos2,
    max_dist: f32,
) -> Option<(NodeId, u16, ValueKind, egui::Pos2)> {
    let mut best: Option<(f32, NodeId, u16, ValueKind, egui::Pos2)> = None;
    for l in layouts {
        for (i, pin) in l.ins.iter().enumerate() {
            let d = pin.pos.distance(p);
            if d <= max_dist && best.map_or(true, |(bd, ..)| d < bd) {
                best = Some((d, l.id, i as u16, pin.kind, pin.pos));
            }
        }
    }
    best.map(|(_, id, i, k, pos)| (id, i, k, pos))
}

/// Compute the per-node screen layouts for the displayed graph.
fn build_layouts(doc: &Document, xf: &ViewXf) -> Vec<Layout> {
    let graph = doc.display_graph();
    let eval = &doc.last_eval;
    let mut out = Vec::with_capacity(graph.nodes.len());
    for (id, node) in &graph.nodes {
        let comp = doc.registry.get(&node.type_name);
        let (label, category, in_specs, out_specs) = match comp {
            Some(c) => (
                c.label().to_string(),
                c.category().to_string(),
                c.inputs(),
                c.outputs(),
            ),
            None => (format!("? {}", node.type_name), "?".to_string(), Vec::new(), Vec::new()),
        };
        let pnum = |key: &str, default: f64| {
            node.params.get(key).and_then(|p| p.as_number()).unwrap_or(default)
        };
        let widget = match node.type_name.as_str() {
            "number_slider" => BodyWidget::Slider {
                value: pnum("value", 5.0),
                min: pnum("min", 0.0),
                max: pnum("max", 10.0),
                step: pnum("step", 0.0),
            },
            "bool_toggle" => BodyWidget::Toggle {
                value: node.params.get("value").and_then(|p| p.as_bool()).unwrap_or(false),
            },
            "panel" => {
                let display = graph.incoming((*id, 0)).and_then(|e| {
                    eval.outputs
                        .get(&e.from.0)
                        .and_then(|outs| outs.get(e.from.1 as usize))
                        .map(|v| v.describe())
                });
                let text = node
                    .params
                    .get("text")
                    .and_then(|p| p.as_text().map(|s| s.to_string()))
                    .unwrap_or_default();
                BodyWidget::Panel { display, text }
            }
            _ => BodyWidget::None,
        };
        let rows = in_specs.len().max(out_specs.len());
        let extra = widget_height(&widget);
        let h = TITLE_H + rows as f32 * ROW_H + extra + if extra > 0.0 { 10.0 } else { 4.0 };
        let world_min = egui::pos2(node.pos.0, node.pos.1);
        let rect = egui::Rect::from_min_size(xf.to_screen(world_min), egui::vec2(NODE_W, h) * xf.zoom);
        let pin_at = |x: f32, i: usize| {
            xf.to_screen(world_min + egui::vec2(x, TITLE_H + (i as f32 + 0.5) * ROW_H))
        };
        let ins = in_specs
            .iter()
            .enumerate()
            .map(|(i, s)| PortPin { pos: pin_at(0.0, i), name: s.name.to_string(), kind: s.ty })
            .collect();
        let outs = out_specs
            .iter()
            .enumerate()
            .map(|(i, s)| PortPin { pos: pin_at(NODE_W, i), name: s.name.to_string(), kind: s.ty })
            .collect();
        let widget_rect = egui::Rect::from_min_size(
            xf.to_screen(world_min + egui::vec2(8.0, TITLE_H + rows as f32 * ROW_H + 4.0)),
            egui::vec2(NODE_W - 16.0, extra) * xf.zoom,
        );
        out.push(Layout {
            id: *id,
            rect,
            ins,
            outs,
            label,
            category,
            widget,
            widget_rect,
            error: eval.errors.get(id).cloned(),
            preview: node.preview(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// tests (pure logic only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_transform_round_trip() {
        let xf = ViewXf { origin: egui::pos2(10.0, 20.0), pan: egui::vec2(33.0, -7.0), zoom: 1.7 };
        let w = egui::pos2(120.5, -44.25);
        let s = xf.to_screen(w);
        let back = xf.to_world(s);
        assert!((back - w).length() < 1e-3, "{back:?} vs {w:?}");
    }

    #[test]
    fn zoom_at_keeps_cursor_fixed() {
        let origin = egui::pos2(5.0, 8.0);
        let pan = egui::vec2(40.0, 12.0);
        let zoom = 1.0;
        let cursor = egui::pos2(300.0, 200.0);
        let before = ViewXf { origin, pan, zoom };
        let world = before.to_world(cursor);
        let (p2, z2) = zoom_at(pan, zoom, origin, cursor, 1.5);
        let after = ViewXf { origin, pan: p2, zoom: z2 };
        let s = after.to_screen(world);
        assert!((s - cursor).length() < 1e-2, "{s:?} vs {cursor:?}");
        assert!((z2 - 1.5).abs() < 1e-6);
    }

    #[test]
    fn zoom_clamped() {
        let (_, z) = zoom_at(egui::Vec2::ZERO, 1.0, egui::Pos2::ZERO, egui::Pos2::ZERO, 100.0);
        assert_eq!(z, MAX_ZOOM);
        let (_, z) = zoom_at(egui::Vec2::ZERO, 1.0, egui::Pos2::ZERO, egui::Pos2::ZERO, 0.0001);
        assert_eq!(z, MIN_ZOOM);
    }

    #[test]
    fn kind_compatibility() {
        assert!(kinds_compatible(ValueKind::Number, ValueKind::Number));
        assert!(kinds_compatible(ValueKind::Number, ValueKind::Any));
        assert!(kinds_compatible(ValueKind::Any, ValueKind::Mesh));
        assert!(kinds_compatible(ValueKind::Vector, ValueKind::Plane));
        assert!(kinds_compatible(ValueKind::Number, ValueKind::Bool));
        assert!(!kinds_compatible(ValueKind::Plane, ValueKind::Vector));
        assert!(!kinds_compatible(ValueKind::Number, ValueKind::Curve));
        assert!(!kinds_compatible(ValueKind::Bool, ValueKind::Text));
    }

    /// Regression test: node-body widgets whose rect lies outside the canvas
    /// (node dragged/panned past the panel edge) must not grow the resizable
    /// bottom panel. Before the `put_at` fix, `ui.put` expanded the panel's
    /// `min_rect`, which egui stores as next frame's panel height — the node
    /// editor would inflate on every repaint and squeeze the 3D viewport away.
    #[test]
    fn panel_height_stable_with_offscreen_node_widgets() {
        use crate::state::Document;
        use mantis_chain::Identity;

        let ctx = egui::Context::default();
        let mut editor = NodeEditor::new();
        let mut doc = Document::new(Identity::generate("t"));
        // Slider far above the canvas top and a toggle far below the bottom:
        // both body widgets land outside the panel rect.
        doc.apply_op(GraphOp::AddNode {
            id: NodeId(1),
            type_name: "number_slider".into(),
            pos: (100.0, -400.0),
        })
        .unwrap();
        doc.apply_op(GraphOp::AddNode {
            id: NodeId(2),
            type_name: "bool_toggle".into(),
            pos: (100.0, 4000.0),
        })
        .unwrap();
        // A wired panel node ON the canvas: its widget arm draws no widget at
        // all (display-only), which exercises the empty-scope path — that
        // alone used to leak a few pixels of panel height per repaint.
        doc.apply_op(GraphOp::AddNode {
            id: NodeId(3),
            type_name: "panel".into(),
            pos: (300.0, 60.0),
        })
        .unwrap();
        doc.apply_op(GraphOp::Connect { from: (NodeId(1), 0), to: (NodeId(3), 0) }).unwrap();
        doc.evaluate();

        let mut heights: Vec<f32> = Vec::new();
        for i in 0..8 {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            ));
            // Wiggle the pointer: each move is a repaint, which is what made
            // the panel visibly creep in the app.
            input
                .events
                .push(egui::Event::PointerMoved(egui::pos2(200.0 + i as f32 * 7.0, 700.0)));
            let mut height = 0.0f32;
            let _ = ctx.run(input, |ctx| {
                // Same panel setup as MantisApp::update.
                let panel = egui::TopBottomPanel::bottom("mantis_node_editor")
                    .resizable(true)
                    .default_height(320.0)
                    .min_height(120.0)
                    .frame(egui::Frame::default())
                    .show(ctx, |ui| {
                        let mut errors = Vec::new();
                        editor.ui(ui, &mut doc, &mut errors);
                    });
                egui::CentralPanel::default().show(ctx, |_| {});
                // This rect is what egui stores as next frame's panel height.
                height = panel.response.rect.height();
            });
            heights.push(height);
        }
        // The stored rect must stay at the configured 320 on every frame: the
        // pre-fix bug inflated it to include the off-canvas widget rects
        // (immediately for the far-away toggle, creeping for the slider).
        for (i, h) in heights.iter().enumerate() {
            assert!(
                (*h - 320.0).abs() < 0.75,
                "frame {i}: panel height {h} instead of 320 — off-canvas node \
                 widgets inflated the panel: {heights:?}"
            );
        }
    }

    #[test]
    fn every_kind_has_distinct_color() {
        let kinds = [
            ValueKind::Any,
            ValueKind::Number,
            ValueKind::Bool,
            ValueKind::Text,
            ValueKind::Vector,
            ValueKind::Plane,
            ValueKind::Curve,
            ValueKind::Mesh,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for k in kinds {
            let c = kind_color(k);
            assert!(seen.insert(c.to_array()), "duplicate color for {k:?}");
        }
    }
}
