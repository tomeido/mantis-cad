//! The top-level eframe application: layout, sync flows, toasts, persistence.

use crate::chain_panel;
use crate::node_editor::NodeEditor;
use crate::state::Document;
use crate::sync::{Flow, SyncClient, SyncEvent, DEFAULT_SERVER_URL};
use crate::util::{format_bytes, now_ms};
use crate::viewport::{self, ViewportPanel};
use mantis_chain::Identity;
use mantis_graph::NodeId;
use std::collections::BTreeSet;

// Persistence keys (eframe storage).
const K_NAME: &str = "mantis.identity.name";
const K_SECRET: &str = "mantis.identity.secret";
const K_URL: &str = "mantis.server.url";
const K_AUTO: &str = "mantis.server.auto_pull";

/// Seconds between auto-pull polls.
const AUTO_PULL_PERIOD: f64 = 3.0;
/// Seconds a toast stays visible.
const TOAST_SECS: f64 = 4.5;

struct Toast {
    text: String,
    error: bool,
    expires: f64,
}

/// MantisCAD: featherweight parametric CAD with an op-log blockchain.
pub struct MantisApp {
    doc: Document,
    editor: NodeEditor,
    viewport: ViewportPanel,
    sync: SyncClient,
    commit_msg: String,
    toasts: Vec<Toast>,
    log: Vec<String>,
    last_selection: BTreeSet<NodeId>,
    show_chain: bool,
    /// Suppress chatty toasts for background (auto-pull) flows.
    quiet_flow: bool,
    /// egui time of the current frame.
    now: f64,
}

impl MantisApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> MantisApp {
        let get = |k: &str| cc.storage.and_then(|s| s.get_string(k));
        let name = get(K_NAME)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "anon".to_string());
        // Restore the signing identity; generate (and later persist) a fresh
        // one on first run or if the stored secret is corrupt.
        let identity = get(K_SECRET)
            .and_then(|hex| Identity::from_secret_hex(&name, &hex).ok())
            .unwrap_or_else(|| Identity::generate(&name));
        let mut sync =
            SyncClient::new(get(K_URL).unwrap_or_else(|| DEFAULT_SERVER_URL.to_string()));
        sync.auto_pull = get(K_AUTO).as_deref() == Some("1");
        MantisApp {
            doc: Document::new(identity),
            editor: NodeEditor::new(),
            viewport: ViewportPanel::new(),
            sync,
            commit_msg: String::new(),
            toasts: Vec::new(),
            log: Vec::new(),
            last_selection: BTreeSet::new(),
            show_chain: true,
            quiet_flow: false,
            now: 0.0,
        }
    }

    // ------------------------------------------------------------------
    // toasts / log
    // ------------------------------------------------------------------

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.log_line(&text);
        self.toasts.push(Toast { text, error, expires: self.now + TOAST_SECS });
    }

    fn log_line(&mut self, text: &str) {
        self.log.push(text.to_string());
        if self.log.len() > 120 {
            self.log.remove(0);
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.expires > self.now);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("mantis_toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for t in &self.toasts {
                    let (bg, fg) = if t.error {
                        (
                            egui::Color32::from_rgb(0x48, 0x22, 0x22),
                            egui::Color32::from_rgb(0xff, 0xb4, 0xa8),
                        )
                    } else {
                        (
                            egui::Color32::from_rgb(0x20, 0x36, 0x28),
                            egui::Color32::from_rgb(0xa8, 0xe8, 0xb4),
                        )
                    };
                    egui::Frame::popup(ui.style()).fill(bg).show(ui, |ui| {
                        ui.label(egui::RichText::new(&t.text).color(fg));
                    });
                }
            });
        // Keep repainting so toasts expire even without input.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    // ------------------------------------------------------------------
    // sync flows
    // ------------------------------------------------------------------

    fn process_sync(&mut self, ctx: &egui::Context) {
        for ev in self.sync.drain() {
            let flow = self.sync.flow;
            match (flow, ev) {
                // ---------------- pull ----------------
                (Flow::Pull, SyncEvent::Info { len }) => {
                    let ours = self.doc.chain.len();
                    if len > ours {
                        self.sync.fetch_blocks(ours, ctx);
                    } else {
                        self.sync.flow = Flow::Idle;
                        if !self.quiet_flow {
                            self.toast("chain up to date", false);
                        }
                    }
                }
                (Flow::Pull, SyncEvent::Blocks { blocks, .. }) => {
                    self.sync.flow = Flow::Idle;
                    self.merge_blocks(&blocks);
                }
                // ---------------- push ----------------
                (Flow::Push { .. }, SyncEvent::Info { len }) => {
                    let ours = self.doc.chain.len();
                    if len > ours {
                        // Server is ahead: pull + merge first.
                        self.sync.fetch_blocks(ours, ctx);
                    } else {
                        self.post_tail_from(len, ctx);
                    }
                }
                (Flow::Push { .. }, SyncEvent::Blocks { from, blocks }) => {
                    let server_len = from + blocks.len();
                    if self.merge_blocks(&blocks) {
                        self.post_tail_from(server_len, ctx);
                    } else {
                        self.sync.flow = Flow::Idle;
                    }
                }
                (Flow::Push { .. }, SyncEvent::PushOk { len }) => {
                    self.sync.flow = Flow::Idle;
                    self.toast(format!("pushed ✓ (server at {len} blocks)"), false);
                }
                (Flow::Push { retried }, SyncEvent::PushConflict) => {
                    if retried {
                        self.sync.flow = Flow::Idle;
                        self.toast("push conflict: server head keeps moving, try again", true);
                    } else {
                        // Head moved under us: pull, merge, retry once.
                        self.sync.flow = Flow::Push { retried: true };
                        self.sync.fetch_info(ctx);
                    }
                }
                // ---------------- failures / stale ----------------
                (_, SyncEvent::Failed { context, msg }) => {
                    self.sync.flow = Flow::Idle;
                    let line = format!("sync {context} failed: {msg}");
                    if self.quiet_flow {
                        self.log_line(&line);
                    } else {
                        self.toast(line, true);
                    }
                }
                (Flow::Idle, _) => {} // stale event after a completed flow
                (Flow::Pull, SyncEvent::PushOk { .. })
                | (Flow::Pull, SyncEvent::PushConflict) => {}
            }
        }
        if self.sync.flow == Flow::Idle && self.quiet_flow && !self.sync.busy() {
            self.quiet_flow = false;
        }
    }

    /// Merge pulled blocks into the document; toast the outcome.
    /// Returns false when the merge failed (flow should stop).
    fn merge_blocks(&mut self, blocks: &[mantis_chain::Block]) -> bool {
        match self.doc.merge_remote(blocks) {
            Ok(report) => {
                if report.appended > 0 {
                    self.toast(
                        format!(
                            "pulled {} block{}",
                            report.appended,
                            if report.appended == 1 { "" } else { "s" }
                        ),
                        false,
                    );
                }
                if report.dropped > 0 {
                    self.toast(
                        format!(
                            "{} pending op{} no longer applied and {} dropped",
                            report.dropped,
                            if report.dropped == 1 { "" } else { "s" },
                            if report.dropped == 1 { "was" } else { "were" }
                        ),
                        true,
                    );
                }
                true
            }
            Err(e) => {
                self.toast(format!("merge failed: {e}"), true);
                false
            }
        }
    }

    /// POST our blocks from `server_len` onward (the tail the server lacks).
    fn post_tail_from(&mut self, server_len: usize, ctx: &egui::Context) {
        let tail: Vec<mantis_chain::Block> = self
            .doc
            .chain
            .blocks
            .get(server_len..)
            .map(|s| s.to_vec())
            .unwrap_or_default();
        if tail.is_empty() {
            self.sync.flow = Flow::Idle;
            if !self.quiet_flow {
                self.toast("nothing to push (commit first)", false);
            }
        } else {
            self.sync.post_blocks(&tail, ctx);
        }
    }

    // ------------------------------------------------------------------
    // panels
    // ------------------------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context, errors: &mut Vec<String>) {
        egui::TopBottomPanel::top("mantis_top").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("MantisCAD")
                        .strong()
                        .size(16.0)
                        .color(egui::Color32::from_rgb(0x7d, 0x9f, 0xc4)),
                );
                ui.separator();
                ui.label("you:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.doc.identity.name).desired_width(90.0),
                )
                .on_hover_text("author name recorded on commits");
                ui.separator();
                ui.label("server:");
                ui.add(egui::TextEdit::singleline(&mut self.sync.url).desired_width(170.0));
                let busy = self.sync.busy();
                if ui
                    .add_enabled(!busy, egui::Button::new("⬇ pull"))
                    .on_hover_text("fetch new blocks from the server")
                    .clicked()
                {
                    self.quiet_flow = false;
                    self.sync.start_pull(ctx);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("⬆ push"))
                    .on_hover_text("send committed blocks to the server")
                    .clicked()
                {
                    self.quiet_flow = false;
                    self.sync.start_push(ctx);
                }
                ui.checkbox(&mut self.sync.auto_pull, "auto")
                    .on_hover_text("poll the server every few seconds");
                if busy {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.show_chain { "chain ▸" } else { "◂ chain" };
                    if ui.button(label).clicked() {
                        self.show_chain = !self.show_chain;
                    }
                });
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label("commit:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.commit_msg)
                        .hint_text("message…")
                        .desired_width(220.0),
                );
                let can_commit = self.doc.editable() && !self.doc.pending.is_empty();
                let commit_label = format!("commit ({})", self.doc.pending.len());
                if ui.add_enabled(can_commit, egui::Button::new(commit_label)).clicked() {
                    match self.doc.commit(&self.commit_msg, now_ms()) {
                        Ok(n) => {
                            self.toast(
                                format!("committed {n} op{}", if n == 1 { "" } else { "s" }),
                                false,
                            );
                            self.commit_msg.clear();
                        }
                        Err(e) => errors.push(e),
                    }
                }
                ui.separator();
                if self.doc.is_time_traveling() {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe8, 0xc0, 0x6a),
                        format!("⏱ block #{} — read-only", self.doc.viewed_block()),
                    );
                    if ui.button("back to head").clicked() {
                        if let Err(e) = self.doc.set_view(None) {
                            errors.push(e);
                        }
                    }
                    ui.separator();
                }
                ui.weak(self.stats_label());
            });
            ui.add_space(2.0);
        });
    }

    /// "N blocks · M ops · X KB on chain ↔ Y MB geometry (Z× lighter)".
    fn stats_label(&self) -> String {
        let chain_bytes = self.doc.chain.byte_size();
        let geo_bytes = self.viewport.geometry_bytes();
        let mut s = format!(
            "{} blocks · {} ops · {} on chain ↔ {} geometry",
            self.doc.chain.len(),
            self.doc.chain.total_ops(),
            format_bytes(chain_bytes),
            format_bytes(geo_bytes),
        );
        if geo_bytes > 0 && chain_bytes > 0 {
            let ratio = geo_bytes as f64 / chain_bytes as f64;
            if ratio >= 1.0 {
                s.push_str(&format!(" ({ratio:.0}× lighter)"));
            }
        }
        s
    }
}

impl eframe::App for MantisApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.now = ctx.input(|i| i.time);
        let mut errors: Vec<String> = Vec::new();

        // ---- network ----------------------------------------------------
        self.process_sync(ctx);
        if self.sync.auto_pull {
            if !self.sync.busy() && self.now - self.sync.last_auto_pull >= AUTO_PULL_PERIOD {
                self.sync.last_auto_pull = self.now;
                self.quiet_flow = true;
                self.sync.start_pull(ctx);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }

        // ---- evaluate (cached; cheap when nothing changed) -----------------
        self.doc.evaluate();

        // ---- layout -------------------------------------------------------
        self.top_bar(ctx, &mut errors);
        if self.show_chain {
            egui::SidePanel::right("mantis_chain_panel")
                .resizable(true)
                .default_width(300.0)
                .min_width(200.0)
                .show(ctx, |ui| {
                    chain_panel::ui(ui, &mut self.doc, &self.log, &mut errors);
                });
        }
        egui::TopBottomPanel::bottom("mantis_node_editor")
            .resizable(true)
            .default_height((ctx.screen_rect().height() * 0.40).max(160.0))
            .min_height(120.0)
            .frame(egui::Frame::default())
            .show(ctx, |ui| {
                self.editor.ui(ui, &mut self.doc, &mut errors);
            });

        // ---- rebuild the 3D scene only when something changed ---------------
        let sel_changed = self.editor.selection != self.last_selection;
        if self.doc.take_scene_dirty() || sel_changed {
            self.doc.evaluate();
            self.viewport.rebuild_scene(&self.doc, &self.editor.selection);
            if sel_changed {
                self.last_selection = self.editor.selection.clone();
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::default())
            .show(ctx, |ui| {
                self.viewport.ui(ui);
            });

        // ---- gesture coalescing fallback -----------------------------------
        // If a slider/move gesture is still open but no pointer button is down
        // and nothing has keyboard focus, the gesture is over: record its one op.
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        let has_focus = ctx.memory(|m| m.focused().is_some());
        if self.doc.gesture_active() && !pointer_down && !has_focus {
            self.doc.end_gesture();
        }

        for e in errors {
            self.toast(e, true);
        }
        self.show_toasts(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(K_NAME, self.doc.identity.name.clone());
        storage.set_string(K_SECRET, self.doc.identity.secret_hex());
        storage.set_string(K_URL, self.sync.url.clone());
        storage.set_string(K_AUTO, if self.sync.auto_pull { "1" } else { "0" }.to_string());
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(gl) = gl {
            viewport::destroy_gl(&self.viewport.shared, gl);
        }
    }
}
