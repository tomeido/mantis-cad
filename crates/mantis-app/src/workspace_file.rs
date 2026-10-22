//! Portable workspace files share one parser across paste, native and browser I/O.
use crate::workspace::{ViewMetadataV1, WorkspaceSnapshotV1, WORKSPACE_FORMAT_VERSION};
use std::sync::mpsc;

pub const MAX_WORKSPACE_BYTES: usize = 64 * 1024 * 1024;

pub fn decode(payload: &[u8]) -> Result<WorkspaceSnapshotV1, String> {
    if payload.len() > MAX_WORKSPACE_BYTES {
        return Err("Workspace files must be smaller than 64 MiB.".into());
    }
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|error| format!("Invalid .mantis workspace: {error}"))?;
    // Legacy/agent CLI chain files have a root `blocks` array. A malformed
    // portable workspace must never silently fall back to this format.
    if value.get("blocks").is_some() && value.get("chain").is_none() {
        let chain: mantis_chain::Chain = serde_json::from_value(value)
            .map_err(|error| format!("Invalid chain JSON: {error}"))?;
        chain
            .validate()
            .map_err(|error| format!("Invalid chain: {error}"))?;
        chain
            .replay(None)
            .map_err(|error| format!("Cannot replay chain: {error}"))?;
        let short_head = &chain.head().hash[..12];
        return Ok(WorkspaceSnapshotV1 {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: format!("chain-{short_head}"),
            name: format!("Chain {short_head}"),
            updated_ms: 0,
            chain,
            pending: Vec::new(),
            recovery_ops: Vec::new(),
            remote: None,
            view: ViewMetadataV1 {
                show_chain: true,
                ..Default::default()
            },
        });
    }
    let portable = serde_json::from_value::<mantis_protocol::PortableWorkspaceV1>(value)
        .map_err(|error| format!("Invalid .mantis workspace: {error}"))?;
    WorkspaceSnapshotV1::from_portable(portable)
}

pub fn encode(snapshot: &WorkspaceSnapshotV1) -> Result<String, String> {
    let payload = serde_json::to_string_pretty(&snapshot.to_portable()?)
        .map_err(|error| format!("Cannot serialize workspace: {error}"))?;
    if payload.len() > MAX_WORKSPACE_BYTES {
        return Err("Workspace files must be smaller than 64 MiB.".into());
    }
    Ok(payload)
}

fn filename(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, '-' | '_' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    format!(
        "{}.mantis",
        if name.trim().is_empty() {
            "project"
        } else {
            name.trim()
        }
    )
}

pub struct WorkspaceFiles {
    sender: mpsc::Sender<Result<Vec<u8>, String>>,
    receiver: mpsc::Receiver<Result<Vec<u8>, String>>,
}

impl Default for WorkspaceFiles {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self { sender, receiver }
    }
}

impl WorkspaceFiles {
    pub fn drain(&self) -> Vec<Result<Vec<u8>, String>> {
        self.receiver.try_iter().collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(&self, ctx: &egui::Context) -> Result<(), String> {
        use std::io::Read as _;
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Open Mantis workspace")
            .add_filter("Mantis workspace", &["mantis"])
            .add_filter("Mantis chain JSON", &["json"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            let result = std::fs::File::open(&path)
                .and_then(|file| {
                    let mut payload = Vec::new();
                    file.take(MAX_WORKSPACE_BYTES as u64 + 1)
                        .read_to_end(&mut payload)?;
                    Ok(payload)
                })
                .map_err(|error| format!("Cannot open {}: {error}", path.display()));
            let _ = self.sender.send(result);
            ctx.request_repaint();
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub fn open(&self, ctx: &egui::Context) -> Result<(), String> {
        use wasm_bindgen::{closure::Closure, JsCast as _};
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or("Browser document unavailable")?;
        let body = document.body().ok_or("Browser document body unavailable")?;
        let input = document
            .create_element("input")
            .map_err(|_| "Cannot open file picker")?
            .dyn_into::<web_sys::HtmlInputElement>()
            .map_err(|_| "File picker unavailable")?;
        input.set_type("file");
        input.set_accept(".mantis,.json,application/json");
        input
            .set_attribute("hidden", "")
            .map_err(|_| "Cannot create file picker")?;
        body.append_child(&input)
            .map_err(|_| "Cannot attach file picker")?;
        let selected_input = input.clone();
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let selected = Closure::once_into_js(move |_: web_sys::Event| {
            let file = selected_input.files().and_then(|files| files.get(0));
            selected_input.set_onchange(None);
            selected_input.set_oncancel(None);
            selected_input.remove();
            if let Some(file) = file {
                wasm_bindgen_futures::spawn_local(async move {
                    let result = if file.size() > MAX_WORKSPACE_BYTES as f64 {
                        Err("Workspace files must be smaller than 64 MiB.".into())
                    } else {
                        wasm_bindgen_futures::JsFuture::from(file.array_buffer())
                            .await
                            .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                            .map_err(|_| "Cannot read selected workspace file".into())
                    };
                    let _ = sender.send(result);
                    ctx.request_repaint();
                });
            }
        });
        input.set_onchange(Some(selected.unchecked_ref()));
        input.set_oncancel(Some(selected.unchecked_ref()));
        input.click();
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save(payload: &str, name: &str) -> Result<Option<String>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_title("Save Mantis workspace")
        .add_filter("Mantis workspace", &["mantis"])
        .set_file_name(filename(name))
        .save_file()
    else {
        return Ok(None);
    };
    crate::workspace::atomic_write(&path, payload.as_bytes())
        .map_err(|error| format!("Cannot save {}: {error}", path.display()))?;
    Ok(Some(format!("Saved {}", path.display())))
}

#[cfg(target_arch = "wasm32")]
pub fn save(payload: &str, name: &str) -> Result<Option<String>, String> {
    crate::commands::download_text(payload, &filename(name))?;
    Ok(Some("Workspace download started.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Document;
    use mantis_chain::Identity;
    use mantis_graph::{GraphOp, NodeId};

    #[test]
    fn file_roundtrip_preserves_uncommitted_and_recovery_work_without_signing_key() {
        let identity = Identity::generate("file author");
        let secret = identity.secret_hex();
        let mut doc = Document::new_scoped(identity).unwrap();
        doc.apply_op(GraphOp::AddNode {
            id: NodeId(7),
            type_name: "circle".into(),
            pos: (2., 3.),
        })
        .unwrap();
        let snapshot = WorkspaceSnapshotV1 {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: "file-test".into(),
            name: "File test".into(),
            updated_ms: 1,
            chain: doc.chain,
            pending: doc.pending,
            recovery_ops: vec![GraphOp::RemoveNode { id: NodeId(99) }],
            remote: None,
            view: ViewMetadataV1::default(),
        };
        let payload = encode(&snapshot).unwrap();
        assert!(!payload.contains(&secret));
        assert!(!payload.contains("secret_hex"));
        let decoded = decode(payload.as_bytes()).unwrap();
        assert_eq!(decoded.chain, snapshot.chain);
        assert_eq!(decoded.pending, snapshot.pending);
        assert_eq!(decoded.recovery_ops, snapshot.recovery_ops);
    }

    #[test]
    fn invalid_or_device_catalog_files_cannot_be_imported_as_workspaces() {
        assert!(decode(b"not JSON").unwrap_err().contains("Invalid .mantis"));
        assert!(decode(
            br#"{"format_version":1,"settings":{"secret_hex":"private"},"workspaces":[]}"#
        )
        .is_err());
        let oversized = vec![b' '; MAX_WORKSPACE_BYTES + 1];
        assert!(decode(&oversized).unwrap_err().contains("64 MiB"));
    }

    #[test]
    fn signed_bare_chain_from_agent_imports_as_a_portable_workspace() {
        let mut doc = Document::new_scoped(Identity::generate("agent")).unwrap();
        doc.apply_op(GraphOp::AddNode {
            id: NodeId(42),
            type_name: "circle".into(),
            pos: (0., 0.),
        })
        .unwrap();
        doc.commit("Agent created circle", 123).unwrap();
        let payload = serde_json::to_vec(&doc.chain).unwrap();
        let decoded = decode(&payload).unwrap();
        assert_eq!(decoded.chain, doc.chain);
        assert_eq!(decoded.chain.replay(None).unwrap(), doc.graph);
        assert!(decoded.pending.is_empty());
        assert!(decoded.remote.is_none());
        decode(encode(&decoded).unwrap().as_bytes()).unwrap();
        let mut damaged: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        damaged["blocks"][1]["hash"] = serde_json::Value::String("0".repeat(64));
        assert!(decode(&serde_json::to_vec(&damaged).unwrap()).is_err());
    }
}
