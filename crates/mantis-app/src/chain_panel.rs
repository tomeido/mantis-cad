//! Right-side chain panel: block list, time-travel slider, pending ops and
//! the session log.

use crate::state::Document;
use crate::util::format_bytes;

const BANNER: egui::Color32 = egui::Color32::from_rgb(0xe8, 0xc0, 0x6a);

/// Draw the chain panel. Errors (e.g. replay failures on time travel) are
/// pushed into `errors` and toasted by the app.
pub fn ui(ui: &mut egui::Ui, doc: &mut Document, log: &[String], errors: &mut Vec<String>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("Chain");
        ui.weak(format!(
            "{} blocks · {} ops · {}",
            doc.chain.len(),
            doc.chain.total_ops(),
            format_bytes(doc.chain.byte_size()),
        ));
    });
    ui.separator();

    // ---- time travel ------------------------------------------------------
    let head = doc.chain.len().saturating_sub(1);
    if head > 0 {
        let mut v = doc.viewed_block();
        let resp = ui.add(egui::Slider::new(&mut v, 0..=head).text("view block"));
        if resp.changed() {
            let target = if v >= head { None } else { Some(v) };
            if let Err(e) = doc.set_view(target) {
                errors.push(e);
            }
        }
    }
    if doc.is_time_traveling() {
        ui.colored_label(
            BANNER,
            format!("⏱ viewing block #{} — read-only", doc.viewed_block()),
        );
        if ui.button("back to head").clicked() {
            if let Err(e) = doc.set_view(None) {
                errors.push(e);
            }
        }
        ui.separator();
    }

    // ---- block list (newest first) -----------------------------------------
    let viewed = doc.viewed_block();
    let avail = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt("chain_blocks")
        .max_height((avail * 0.42).max(110.0))
        .show(ui, |ui| {
            let mut clicked: Option<usize> = None;
            for (i, b) in doc.chain.blocks.iter().enumerate().rev() {
                let msg: String = b.message.chars().take(28).collect();
                let ellipsis = if b.message.chars().count() > 28 { "…" } else { "" };
                let text = format!(
                    "#{} {} · {}{} · {} op{} · {}",
                    b.index,
                    b.author,
                    msg,
                    ellipsis,
                    b.ops.len(),
                    if b.ops.len() == 1 { "" } else { "s" },
                    format_bytes(b.byte_size()),
                );
                let hash_prefix: String = b.hash.chars().take(16).collect();
                let resp = ui
                    .selectable_label(i == viewed, text)
                    .on_hover_text(format!("hash {hash_prefix}…\n{}", b.message));
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
            if let Some(i) = clicked {
                let target = if i >= head { None } else { Some(i) };
                if let Err(e) = doc.set_view(target) {
                    errors.push(e);
                }
            }
        });

    ui.separator();

    // ---- pending ops --------------------------------------------------------
    ui.label(format!("Pending ops ({})", doc.pending.len()));
    egui::ScrollArea::vertical()
        .id_salt("pending_ops")
        .max_height(110.0)
        .show(ui, |ui| {
            if doc.pending.is_empty() {
                ui.weak("none — edits collect here until you commit");
            }
            for op in &doc.pending {
                ui.small(op.describe());
            }
        });

    ui.separator();

    // ---- log ------------------------------------------------------------------
    ui.label("Log");
    egui::ScrollArea::vertical()
        .id_salt("session_log")
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if log.is_empty() {
                ui.weak("(empty)");
            }
            for line in log.iter() {
                ui.weak(line);
            }
        });
}
