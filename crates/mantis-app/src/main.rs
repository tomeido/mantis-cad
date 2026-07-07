//! MantisCAD GUI — native and wasm entry points.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chain_panel;
mod node_editor;
mod state;
mod sync;
mod util;
mod viewport;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("MantisCAD"),
        multisampling: 4,
        depth_buffer: 24,
        ..Default::default()
    };
    eframe::run_native(
        "MantisCAD",
        options,
        Box::new(|cc| Ok(Box::new(app::MantisApp::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;

    let web_options = eframe::WebOptions {
        depth_buffer: 24,
        ..Default::default()
    };
    wasm_bindgen_futures::spawn_local(async move {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("no browser document");
        let canvas = document
            .get_element_by_id("mantis_canvas")
            .expect("no element with id mantis_canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("mantis_canvas is not a <canvas>");
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(app::MantisApp::new(cc)))),
            )
            .await;
        // Swap the loading indicator out once the app is up (or show the error).
        if let Some(loading) = document.get_element_by_id("loading") {
            match &result {
                Ok(()) => loading.remove(),
                Err(e) => loading.set_text_content(Some(&format!(
                    "MantisCAD failed to start: {e:?}"
                ))),
            }
        }
    });
}
