use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use mantis_chain::Identity;

struct Harness {
    ctx: egui::Context,
    editor: NodeEditor,
    doc: Document,
    canvas: egui::Rect,
    time: f64,
}

impl Harness {
    fn new() -> Self {
        let mut doc = Document::new(Identity::generate("selection-test"));
        for (id, pos) in [
            (1, (100.0, 100.0)),
            (2, (400.0, 100.0)),
            (3, (100.0, 320.0)),
        ] {
            doc.apply_op(GraphOp::AddNode {
                id: NodeId(id),
                type_name: "number_slider".into(),
                pos,
            })
            .unwrap();
        }
        let mut harness = Self {
            ctx: egui::Context::default(),
            editor: NodeEditor::new(),
            doc,
            canvas: egui::Rect::NOTHING,
            time: 0.0,
        };
        harness.editor.pan = egui::Vec2::ZERO;
        // Populate egui's previous-frame hit-test data before sending input.
        harness.frame(vec![], Modifiers::NONE);
        harness.frame(vec![], Modifiers::NONE);
        harness
    }

    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            time: Some(self.time),
            events,
            modifiers,
            ..Default::default()
        };
        let mut errors = Vec::new();
        let _ = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                self.canvas = ui.available_rect_before_wrap();
                self.editor.ui(ui, &mut self.doc, &mut errors);
            });
        });
        assert!(errors.is_empty(), "{errors:?}");
    }

    fn layout(&self, id: u128) -> Layout {
        build_layouts(
            &self.doc,
            &ViewXf {
                origin: self.canvas.min,
                pan: self.editor.pan,
                zoom: self.editor.zoom,
            },
        )
        .into_iter()
        .find(|layout| layout.id == NodeId(id))
        .unwrap()
    }

    fn press(&mut self, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
        self.move_to(pos, modifiers);
        self.frame(vec![button_event(pos, button, true, modifiers)], modifiers);
    }

    fn move_to(&mut self, pos: Pos2, modifiers: Modifiers) {
        self.frame(vec![Event::PointerMoved(pos)], modifiers);
    }

    fn release(&mut self, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
        self.frame(vec![button_event(pos, button, false, modifiers)], modifiers);
    }

    fn select_top_pair(&mut self) {
        let start = self.layout(1).rect.min - egui::vec2(20.0, 20.0);
        let end = self.layout(2).rect.center();
        self.press(start, PointerButton::Primary, Modifiers::NONE);
        self.move_to(end, Modifiers::NONE);
        self.release(end, PointerButton::Primary, Modifiers::NONE);
        assert_eq!(self.editor.selection, ids(&[1, 2]));
    }
}

fn ids(values: &[u128]) -> BTreeSet<NodeId> {
    values.iter().copied().map(NodeId).collect()
}

fn button_event(pos: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    }
}

fn key_event(key: egui::Key, pressed: bool) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn canvas_drag_selects_intersecting_nodes_live_without_editing_or_panning() {
    let mut h = Harness::new();
    h.editor.selection = ids(&[3]);
    let graph = h.doc.graph.clone();
    let pending = h.doc.pending.clone();
    let revision = h.doc.persistence_revision();
    let pan = h.editor.pan;
    let start = h.layout(1).rect.min - egui::vec2(20.0, 20.0);
    let end = h.layout(2).rect.center(); // Partial overlap must count.
    h.press(start, PointerButton::Primary, Modifiers::NONE);
    h.move_to(end, Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 2]));
    h.release(end, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 2]));
    assert_eq!(h.editor.pan, pan);
    assert_eq!(h.doc.graph, graph);
    assert_eq!(h.doc.pending, pending);
    assert_eq!(h.doc.persistence_revision(), revision);
    h.press(start, PointerButton::Primary, Modifiers::NONE);
    h.release(start, PointerButton::Primary, Modifiers::NONE);
    assert!(h.editor.selection.is_empty());
}

#[test]
fn shift_box_selection_preserves_initial_selection_but_forgets_shrunken_hits() {
    let mut h = Harness::new();
    h.editor.selection = ids(&[3]);
    let start = h.layout(1).rect.min - egui::vec2(20.0, 20.0);
    h.press(start, PointerButton::Primary, Modifiers::SHIFT);
    h.move_to(h.layout(2).rect.center(), Modifiers::SHIFT);
    assert_eq!(h.editor.selection, ids(&[1, 2, 3]));
    let end = h.layout(1).rect.center();
    // Releasing Shift during the gesture does not discard its original mode.
    h.move_to(end, Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 3]));
    h.release(end, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 3]));
}

#[test]
fn reverse_box_selection_uses_transformed_node_rectangles() {
    let mut h = Harness::new();
    h.editor.pan = egui::vec2(53.0, 17.0);
    h.editor.zoom = 1.5;
    h.frame(vec![], Modifiers::NONE);
    let start = h.layout(2).rect.max + egui::vec2(16.0, 16.0);
    let end = h.layout(1).rect.center();
    h.press(start, PointerButton::Primary, Modifiers::NONE);
    h.move_to(end, Modifiers::NONE);
    h.release(end, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 2]));
    assert_eq!(h.editor.pan, egui::vec2(53.0, 17.0));
    assert_eq!(h.editor.zoom, 1.5);
}

#[test]
fn escape_restores_selection_and_cancels_drag_until_pointer_release() {
    for release_together in [false, true] {
        let mut h = Harness::new();
        h.editor.selection = ids(&[3]);
        let start = h.layout(1).rect.min - egui::vec2(20.0, 20.0);
        let end = h.layout(2).rect.center();
        h.press(start, PointerButton::Primary, Modifiers::NONE);
        h.move_to(end, Modifiers::NONE);
        assert_eq!(h.editor.selection, ids(&[1, 2]));
        let mut events = vec![key_event(egui::Key::Escape, true)];
        if release_together {
            events.push(button_event(
                end,
                PointerButton::Primary,
                false,
                Modifiers::NONE,
            ));
        }
        h.frame(events, Modifiers::NONE);
        assert_eq!(h.editor.selection, ids(&[3]));
        h.move_to(h.layout(1).rect.center(), Modifiers::NONE);
        if !release_together {
            h.release(end, PointerButton::Primary, Modifiers::NONE);
        }
        assert_eq!(h.editor.selection, ids(&[3]));
    }
}

#[test]
fn release_position_is_included_without_a_final_pointer_move_event() {
    let mut h = Harness::new();
    let start = h.layout(1).rect.min - egui::vec2(20.0, 20.0);
    h.press(start, PointerButton::Primary, Modifiers::NONE);
    h.move_to(h.layout(1).rect.center(), Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1]));
    h.release(
        h.layout(2).rect.center(),
        PointerButton::Primary,
        Modifiers::NONE,
    );
    assert_eq!(h.editor.selection, ids(&[1, 2]));
}

#[test]
fn history_view_selects_displayed_nodes_without_editing_them() {
    let mut h = Harness::new();
    h.doc.commit("original layout", 1).unwrap();
    h.doc
        .apply_op(GraphOp::MoveNode {
            id: NodeId(1),
            pos: (1100.0, 500.0),
        })
        .unwrap();
    h.doc.commit("move at head", 2).unwrap();
    h.doc.set_view(Some(1)).unwrap();
    assert!(!h.doc.editable());
    h.frame(vec![], Modifiers::NONE);
    let graph = h.doc.graph.clone();
    let revision = h.doc.persistence_revision();
    h.select_top_pair();
    h.frame(vec![key_event(egui::Key::Delete, true)], Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 2]));
    assert_eq!(h.doc.graph, graph);
    assert!(h.doc.pending.is_empty());
    assert_eq!(h.doc.persistence_revision(), revision);
}

#[test]
fn active_box_freezes_navigation_and_clamps_selection_to_canvas() {
    let mut h = Harness::new();
    h.doc
        .apply_op(GraphOp::AddNode {
            id: NodeId(4),
            type_name: "number_slider".into(),
            pos: (1100.0, 100.0),
        })
        .unwrap();
    h.frame(vec![], Modifiers::NONE);
    let start = h.layout(1).rect.min - egui::vec2(20.0, 20.0);
    let end = h.layout(2).rect.center();
    h.press(start, PointerButton::Primary, Modifiers::NONE);
    h.move_to(end, Modifiers::NONE);
    h.frame(
        vec![
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::NONE,
            },
            button_event(end, PointerButton::Middle, true, Modifiers::NONE),
            key_event(egui::Key::Space, true),
        ],
        Modifiers::NONE,
    );
    h.move_to(end + egui::vec2(30.0, 0.0), Modifiers::NONE);
    assert_eq!(h.editor.pan, egui::Vec2::ZERO);
    assert_eq!(h.editor.zoom, 1.0);
    let outside = egui::pos2(1400.0, 270.0);
    h.move_to(outside, Modifiers::NONE);
    h.release(outside, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.editor.selection, ids(&[1, 2]));
}

#[test]
fn box_selected_nodes_move_and_delete_as_undoable_groups() {
    let mut h = Harness::new();
    h.select_top_pair();
    let before = h.doc.graph.clone();
    let pending = h.doc.pending.len();
    let start = h.layout(1).rect.min + egui::vec2(30.0, 12.0);
    let delta = egui::vec2(36.0, 28.0);
    h.press(start, PointerButton::Primary, Modifiers::NONE);
    h.move_to(start + delta, Modifiers::NONE);
    h.release(start + delta, PointerButton::Primary, Modifiers::NONE);
    for id in [NodeId(1), NodeId(2)] {
        let old = before.nodes[&id].pos;
        assert_eq!(
            h.doc.graph.nodes[&id].pos,
            (old.0 + delta.x, old.1 + delta.y)
        );
    }
    assert_eq!(
        h.doc.graph.nodes[&NodeId(3)].pos,
        before.nodes[&NodeId(3)].pos
    );
    assert_eq!(h.doc.pending.len(), pending + 2);
    h.doc.undo_pending().unwrap();
    assert_eq!(h.doc.graph, before);
    h.frame(vec![key_event(egui::Key::Delete, true)], Modifiers::NONE);
    assert_eq!(
        h.doc.graph.nodes.keys().copied().collect::<BTreeSet<_>>(),
        ids(&[3])
    );
    h.doc.undo_pending().unwrap();
    assert_eq!(h.doc.graph, before);
}

#[test]
fn port_and_slider_drags_do_not_start_box_selection() {
    for port in [true, false] {
        let mut h = Harness::new();
        h.editor.selection = ids(&[3]);
        let layout = h.layout(1);
        let start = if port {
            layout.outs[0].pos
        } else {
            layout.widget_rect.min + egui::vec2(25.0, 10.0)
        };
        let positions: Vec<_> = h.doc.graph.nodes.values().map(|node| node.pos).collect();
        let end = h.layout(2).rect.center();
        h.press(start, PointerButton::Primary, Modifiers::NONE);
        h.move_to(end, Modifiers::NONE);
        if port {
            assert!(h.editor.wire_drag.is_some());
        } else {
            assert!(
                h.doc.graph.nodes[&NodeId(1)]
                    .params
                    .get("value")
                    .and_then(ParamValue::as_number)
                    .is_some_and(|value| value != 5.0),
                "slider drag should change its value"
            );
        }
        h.release(end, PointerButton::Primary, Modifiers::NONE);
        if !port {
            assert!(h.doc.pending.iter().any(|op| matches!(
                op,
                GraphOp::SetParam { id: NodeId(1), key, .. } if key == "value"
            )));
        }
        assert_eq!(h.editor.selection, ids(&[3]), "port gesture: {port}");
        assert_eq!(h.editor.pan, egui::Vec2::ZERO);
        assert_eq!(
            h.doc
                .graph
                .nodes
                .values()
                .map(|node| node.pos)
                .collect::<Vec<_>>(),
            positions
        );
        assert!(h.doc.graph.edges.is_empty());
    }
}

#[test]
fn middle_and_space_primary_drags_pan_without_changing_selection() {
    for button in [PointerButton::Middle, PointerButton::Primary] {
        let mut h = Harness::new();
        h.editor.selection = ids(&[3]);
        let graph = h.doc.graph.clone();
        let start = h.canvas.min + egui::vec2(30.0, 30.0);
        let delta = egui::vec2(40.0, 25.0);
        if button == PointerButton::Primary {
            h.frame(vec![key_event(egui::Key::Space, true)], Modifiers::NONE);
        }
        h.press(start, button, Modifiers::NONE);
        h.move_to(start + delta, Modifiers::NONE);
        h.release(start + delta, button, Modifiers::NONE);
        assert_eq!(h.editor.pan, delta, "pan button: {button:?}");
        assert_eq!(h.editor.selection, ids(&[3]));
        assert_eq!(h.doc.graph, graph);
    }
}
