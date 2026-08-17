//! Top-level application: workspaces, explicit sync, UI and persistence.

use crate::chain_panel;
use crate::key_backup;
use crate::node_editor::NodeEditor;
use crate::state::Document;
use crate::sync::{
    build_push_chunk, compare_heads, next_page, Flow, HeadRelation, PageState, PushProgress,
    RemoteInfo, SyncClient, SyncEvent, DEFAULT_PROJECT_ID, DEFAULT_SERVER_URL,
};
use crate::util::{format_bytes, now_ms};
use crate::viewport::{self, ViewportPanel};
use crate::workspace::{
    AccessState, CameraMetadataV1, CatalogDurability, DeviceSettingsV1, PersistEvent, Persistence,
    RemoteAnchorV1, ViewMetadataV1, WorkspaceCatalogV1, WorkspaceSnapshotV1,
    CATALOG_FORMAT_VERSION, WORKSPACE_FORMAT_VERSION,
};
use mantis_chain::Identity;
use mantis_graph::NodeId;
use mantis_kernel::Vec3;
use mantis_protocol::ProjectSummaryV1;
use std::collections::BTreeSet;

// Legacy eframe keys. They are read once to seed the new durable catalog and
// retained as a compatibility fallback for users upgrading from 0.1.
const K_NAME: &str = "mantis.identity.name";
const K_SECRET: &str = "mantis.identity.secret";
const K_URL: &str = "mantis.server.url";
const K_AUTO: &str = "mantis.server.auto_pull";

const REMOTE_CHECK_PERIOD: f64 = 3.0;
const AUTOSAVE_DEBOUNCE: f64 = 0.5;
const TOAST_SECS: f64 = 4.5;

struct Toast {
    text: String,
    error: bool,
    expires: f64,
}

#[derive(Default)]
struct KeyBackupDialog {
    open: bool,
    password: String,
    payload: String,
    error: String,
    generated_for: Option<String>,
}

#[derive(Default)]
struct WorkspaceTransferDialog {
    open: bool,
    payload: String,
    error: String,
}

pub struct MantisApp {
    doc: Document,
    editor: NodeEditor,
    viewport: ViewportPanel,
    sync: SyncClient,
    remote_attached: bool,
    remote_connection_confirmed: bool,
    remote_ahead: bool,
    adopting_remote: bool,
    push_progress: Option<PushProgress>,
    workspace_name: String,
    catalog: WorkspaceCatalogV1,
    persistence: Persistence,
    storage_ready: bool,
    storage_disabled: bool,
    catalog_durability: CatalogDurability,
    corrupt_catalog: Option<String>,
    last_observed_catalog: String,
    dirty_since: Option<f64>,
    new_workspace_name: String,
    show_new_workspace: bool,
    show_remote_projects: bool,
    remote_projects: Vec<ProjectSummaryV1>,
    delete_workspace: Option<String>,
    key_dialog: KeyBackupDialog,
    transfer_dialog: WorkspaceTransferDialog,
    show_recovery: bool,
    commit_msg: String,
    toasts: Vec<Toast>,
    log: Vec<String>,
    last_selection: BTreeSet<NodeId>,
    show_chain: bool,
    quiet_flow: bool,
    now: f64,
}

impl MantisApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> MantisApp {
        let get = |key: &str| cc.storage.and_then(|storage| storage.get_string(key));
        let name = get(K_NAME)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "anon".to_string());
        let identity = get(K_SECRET)
            .and_then(|secret| Identity::from_secret_hex(&name, &secret).ok())
            .unwrap_or_else(|| Identity::generate(&name));
        let doc = Document::new_scoped(identity)
            .expect("secure randomness is required to create a scoped workspace");
        let url = get(K_URL).unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
        let background_check = get(K_AUTO).as_deref() == Some("1");
        let public = doc.identity.public_hex();
        let id = format!("local-{}", &public[..12]);
        let web_remote = cfg!(target_arch = "wasm32");
        let remote = web_remote.then(|| RemoteAnchorV1 {
            base_url: url.clone(),
            project_id: DEFAULT_PROJECT_ID.into(),
            connection_confirmed: true,
            chain_format_version: 0,
            chain_id: None,
            genesis_hash: String::new(),
            last_synced_len: 0,
            last_synced_head: String::new(),
            access: AccessState::Unknown,
        });
        let snapshot = WorkspaceSnapshotV1 {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: id.clone(),
            name: "Untitled".into(),
            updated_ms: now_ms(),
            chain: doc.chain.clone(),
            pending: Vec::new(),
            recovery_ops: Vec::new(),
            remote,
            view: ViewMetadataV1 {
                viewed_block: None,
                show_chain: true,
                selected_nodes: Vec::new(),
                camera: CameraMetadataV1::default(),
            },
        };
        let catalog = WorkspaceCatalogV1 {
            format_version: CATALOG_FORMAT_VERSION,
            active_id: id,
            settings: DeviceSettingsV1 {
                author_name: doc.identity.name.clone(),
                secret_hex: doc.identity.secret_hex(),
                default_server_url: url.clone(),
                background_check,
                key_backup_confirmed: false,
            },
            workspaces: vec![snapshot],
        };
        let mut sync = SyncClient::new(url);
        sync.auto_pull = background_check;
        MantisApp {
            doc,
            editor: NodeEditor::new(),
            viewport: ViewportPanel::new(),
            sync,
            remote_attached: web_remote,
            remote_connection_confirmed: true,
            remote_ahead: false,
            adopting_remote: false,
            push_progress: None,
            workspace_name: "Untitled".into(),
            catalog,
            persistence: Persistence::new(),
            storage_ready: false,
            storage_disabled: false,
            catalog_durability: CatalogDurability::default(),
            corrupt_catalog: None,
            last_observed_catalog: String::new(),
            dirty_since: None,
            new_workspace_name: "Untitled".into(),
            show_new_workspace: false,
            show_remote_projects: false,
            remote_projects: Vec::new(),
            delete_workspace: None,
            key_dialog: KeyBackupDialog::default(),
            transfer_dialog: WorkspaceTransferDialog::default(),
            show_recovery: false,
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
    // durable catalog
    // ------------------------------------------------------------------

    fn process_persistence(&mut self) {
        for event in self.persistence.drain() {
            match event {
                PersistEvent::Loaded(Ok(Some(json))) => {
                    let loaded = serde_json::from_str::<WorkspaceCatalogV1>(&json)
                        .map_err(|e| format!("saved workspace catalog is invalid JSON: {e}"))
                        .and_then(|catalog| {
                            catalog.validate()?;
                            Ok(catalog)
                        })
                        .and_then(|catalog| self.install_catalog(catalog));
                    match loaded {
                        Ok(()) => {
                            self.storage_ready = true;
                            self.catalog_durability.restored_catalog();
                            self.last_observed_catalog =
                                self.catalog_json(false).unwrap_or_default();
                            self.log_line("restored workspace catalog");
                        }
                        Err(error) => {
                            self.storage_ready = true;
                            self.catalog_durability.require_durable_save();
                            self.corrupt_catalog = Some(json);
                            self.toast(format!("saved workspaces were not opened: {error}"), true);
                        }
                    }
                }
                PersistEvent::Loaded(Ok(None)) => {
                    self.storage_ready = true;
                    self.catalog_durability.require_durable_save();
                    self.persist_now();
                    self.log_line("migrated legacy settings into workspace storage");
                }
                PersistEvent::Loaded(Err(error)) => {
                    self.storage_ready = true;
                    self.storage_disabled = true;
                    self.catalog_durability.require_durable_save();
                    self.toast(error, true);
                }
                PersistEvent::Saved { generation } => {
                    self.catalog_durability.saved(generation);
                }
                PersistEvent::SaveFailed { generation, error } => {
                    let latest = self.catalog_durability.is_latest(generation);
                    self.catalog_durability.failed(generation);
                    if latest {
                        self.storage_disabled = true;
                    }
                    self.toast(error, true);
                }
            }
        }
    }

    fn install_catalog(&mut self, catalog: WorkspaceCatalogV1) -> Result<(), String> {
        let identity =
            Identity::from_secret_hex(&catalog.settings.author_name, &catalog.settings.secret_hex)
                .map_err(|e| format!("saved identity is invalid: {e}"))?;
        let snapshot = catalog
            .active()
            .cloned()
            .ok_or_else(|| "active workspace is missing".to_string())?;
        snapshot.validate_envelope()?;
        let doc = Document::restore(
            identity,
            snapshot.chain.clone(),
            snapshot.pending.clone(),
            snapshot.recovery_ops.clone(),
            snapshot.view.viewed_block,
        )?;
        let remote = snapshot.remote.clone();
        let mut sync = SyncClient::new(
            remote
                .as_ref()
                .map(|anchor| anchor.base_url.clone())
                .unwrap_or_else(|| catalog.settings.default_server_url.clone()),
        );
        if let Some(anchor) = &remote {
            sync.project_id = anchor.project_id.clone();
            sync.last_info = remote_info_from_anchor(anchor);
        }
        sync.auto_pull = catalog.settings.background_check;
        self.doc = doc;
        self.workspace_name = snapshot.name;
        self.show_chain = snapshot.view.show_chain;
        self.editor.selection = snapshot.view.selected_nodes.into_iter().collect();
        restore_camera(&mut self.viewport, &snapshot.view.camera);
        self.last_selection.clear();
        self.sync = sync;
        self.sync.public_key = self.doc.identity.public_hex();
        self.sync.local_chain_format = self.doc.chain.format_version().unwrap_or(1);
        self.remote_attached = remote.is_some();
        self.remote_connection_confirmed = remote
            .as_ref()
            .is_none_or(|anchor| anchor.connection_confirmed);
        self.remote_ahead = false;
        self.adopting_remote = false;
        self.push_progress = None;
        self.catalog = catalog;
        Ok(())
    }

    fn snapshot(&self, update_timestamp: bool) -> WorkspaceSnapshotV1 {
        let previous = self.catalog.active();
        let base_url = self.sync.url.trim().trim_end_matches('/').to_string();
        let previous_remote = previous
            .and_then(|workspace| workspace.remote.as_ref())
            .filter(|remote| {
                remote.base_url.trim().trim_end_matches('/') == base_url
                    && remote.project_id == self.sync.project_id
            });
        let info = self.sync.last_info.as_ref();
        let synced_info = info.filter(|value| self.remote_is_local_prefix(value));
        let remote = self.remote_attached.then(|| RemoteAnchorV1 {
            base_url,
            project_id: self.sync.project_id.clone(),
            connection_confirmed: self.remote_connection_confirmed,
            chain_format_version: info
                .map(|value| value.chain_format_version)
                .or_else(|| previous_remote.map(|value| value.chain_format_version))
                .unwrap_or_else(|| self.doc.chain.format_version().unwrap_or(1)),
            chain_id: info
                .and_then(|value| value.chain_id.clone())
                .or_else(|| previous_remote.and_then(|value| value.chain_id.clone()))
                .or_else(|| self.doc.chain.chain_id().ok().flatten().map(str::to_owned)),
            genesis_hash: info
                .map(|value| value.genesis_hash.clone())
                .or_else(|| previous_remote.map(|value| value.genesis_hash.clone()))
                .unwrap_or_else(|| self.doc.chain.blocks[0].hash.clone()),
            last_synced_len: synced_info
                .map(|value| value.len)
                .or_else(|| previous_remote.map(|value| value.last_synced_len))
                .unwrap_or(0),
            last_synced_head: synced_info
                .map(|value| value.head.clone())
                .or_else(|| previous_remote.map(|value| value.last_synced_head.clone()))
                .unwrap_or_default(),
            access: info
                .map(|value| value.access)
                .or_else(|| previous_remote.map(|value| value.access))
                .unwrap_or_default(),
        });
        WorkspaceSnapshotV1 {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: self.catalog.active_id.clone(),
            name: nonempty_name(&self.workspace_name),
            updated_ms: if update_timestamp {
                now_ms()
            } else {
                previous.map(|value| value.updated_ms).unwrap_or(0)
            },
            chain: self.doc.chain.clone(),
            pending: self.doc.pending.clone(),
            recovery_ops: self.doc.recovery_ops.clone(),
            remote,
            view: ViewMetadataV1 {
                viewed_block: self.doc.view_index(),
                show_chain: self.show_chain,
                selected_nodes: self.editor.selection.iter().copied().collect(),
                camera: CameraMetadataV1 {
                    target: [
                        self.viewport.camera.target.x,
                        self.viewport.camera.target.y,
                        self.viewport.camera.target.z,
                    ],
                    distance: self.viewport.camera.distance,
                    yaw: self.viewport.camera.yaw,
                    pitch: self.viewport.camera.pitch,
                },
            },
        }
    }

    fn refresh_catalog(&mut self, update_timestamp: bool) {
        self.catalog.settings.author_name = self.doc.identity.name.clone();
        self.catalog.settings.secret_hex = self.doc.identity.secret_hex();
        self.catalog.settings.default_server_url = self.sync.url.clone();
        self.catalog.settings.background_check = self.sync.auto_pull;
        let snapshot = self.snapshot(update_timestamp);
        self.catalog.replace(snapshot);
    }

    fn catalog_json(&mut self, update_timestamp: bool) -> Result<String, String> {
        self.refresh_catalog(update_timestamp);
        serde_json::to_string_pretty(&self.catalog)
            .map_err(|e| format!("cannot serialize workspaces: {e}"))
    }

    fn persist_now(&mut self) {
        if !self.storage_ready || self.storage_disabled || self.corrupt_catalog.is_some() {
            return;
        }
        match self.catalog_json(true) {
            Ok(json) => {
                self.last_observed_catalog = json.clone();
                self.dirty_since = None;
                let ticket = self.persistence.save(json);
                self.catalog_durability.requested(ticket);
            }
            Err(error) => self.toast(error, true),
        }
    }

    fn autosave(&mut self, ctx: &egui::Context) {
        if !self.storage_ready || self.storage_disabled || self.corrupt_catalog.is_some() {
            return;
        }
        let Ok(json) = self.catalog_json(false) else {
            return;
        };
        if json != self.last_observed_catalog && self.dirty_since.is_none() {
            self.dirty_since = Some(self.now);
        }
        if self
            .dirty_since
            .is_some_and(|since| self.now - since >= AUTOSAVE_DEBOUNCE)
        {
            self.persist_now();
        } else if self.dirty_since.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn switch_workspace(&mut self, id: &str) -> Result<(), String> {
        if id == self.catalog.active_id {
            return Ok(());
        }
        self.persist_now();
        let snapshot = self
            .catalog
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .cloned()
            .ok_or_else(|| format!("workspace `{id}` no longer exists"))?;
        let identity =
            Identity::from_secret_hex(&self.doc.identity.name, &self.doc.identity.secret_hex())
                .map_err(|e| format!("cannot restore signing identity: {e}"))?;
        let doc = Document::restore(
            identity,
            snapshot.chain.clone(),
            snapshot.pending.clone(),
            snapshot.recovery_ops.clone(),
            snapshot.view.viewed_block,
        )?;
        self.catalog.active_id = id.to_string();
        self.doc = doc;
        self.workspace_name = snapshot.name;
        self.show_chain = snapshot.view.show_chain;
        self.editor.selection = snapshot.view.selected_nodes.into_iter().collect();
        restore_camera(&mut self.viewport, &snapshot.view.camera);
        self.last_selection.clear();
        if let Some(remote) = snapshot.remote {
            self.remote_attached = true;
            self.remote_connection_confirmed = remote.connection_confirmed;
            self.sync = SyncClient::new(remote.base_url.clone());
            self.sync.project_id = remote.project_id.clone();
            self.sync.last_info = remote_info_from_anchor(&remote);
        } else {
            self.remote_attached = false;
            self.remote_connection_confirmed = true;
            self.sync = SyncClient::new(self.catalog.settings.default_server_url.clone());
        }
        self.sync.auto_pull = self.catalog.settings.background_check;
        self.sync.public_key = self.doc.identity.public_hex();
        self.sync.local_chain_format = self.doc.chain.format_version().unwrap_or(1);
        self.remote_ahead = false;
        self.adopting_remote = false;
        self.push_progress = None;
        self.persist_now();
        Ok(())
    }

    fn create_workspace(&mut self, name: &str) -> Result<(), String> {
        self.persist_now();
        let id = next_workspace_id(&self.catalog);
        let identity =
            Identity::from_secret_hex(&self.doc.identity.name, &self.doc.identity.secret_hex())
                .map_err(|e| format!("cannot restore signing identity: {e}"))?;
        let doc = Document::new_scoped(identity)?;
        let snapshot = WorkspaceSnapshotV1 {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: id.clone(),
            name: nonempty_name(name),
            updated_ms: now_ms(),
            chain: doc.chain.clone(),
            pending: Vec::new(),
            recovery_ops: Vec::new(),
            remote: None,
            view: ViewMetadataV1 {
                show_chain: true,
                ..Default::default()
            },
        };
        self.catalog.active_id = id;
        self.catalog.workspaces.push(snapshot);
        self.doc = doc;
        self.workspace_name = nonempty_name(name);
        self.editor.selection.clear();
        self.last_selection.clear();
        self.viewport.camera = Default::default();
        self.show_chain = true;
        self.remote_attached = false;
        self.remote_connection_confirmed = true;
        let mut sync = SyncClient::new(self.catalog.settings.default_server_url.clone());
        sync.auto_pull = self.catalog.settings.background_check;
        sync.public_key = self.doc.identity.public_hex();
        sync.local_chain_format = self.doc.chain.format_version().unwrap_or(1);
        self.sync = sync;
        self.remote_ahead = false;
        self.adopting_remote = false;
        self.push_progress = None;
        self.persist_now();
        Ok(())
    }

    fn duplicate_workspace(&mut self) -> Result<(), String> {
        self.persist_now();
        let mut copy = self.snapshot(true);
        copy.id = next_workspace_id(&self.catalog);
        copy.name = format!("{} copy", nonempty_name(&self.workspace_name));
        self.catalog.active_id = copy.id.clone();
        self.catalog.workspaces.push(copy.clone());
        self.workspace_name = copy.name;
        self.persist_now();
        Ok(())
    }

    fn delete_workspace_now(&mut self, id: &str) -> Result<(), String> {
        if self.catalog.workspaces.len() <= 1 {
            return Err("the last workspace cannot be deleted".into());
        }
        let was_active = self.catalog.active_id == id;
        self.persist_now();
        if was_active {
            let next = self
                .catalog
                .workspaces
                .iter()
                .find(|workspace| workspace.id != id)
                .map(|workspace| workspace.id.clone())
                .ok_or_else(|| "the last workspace cannot be deleted".to_string())?;
            self.switch_workspace(&next)?;
        }
        self.catalog
            .workspaces
            .retain(|workspace| workspace.id != id);
        self.persist_now();
        Ok(())
    }

    // ------------------------------------------------------------------
    // toasts / log
    // ------------------------------------------------------------------

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.log_line(&text);
        self.toasts.push(Toast {
            text,
            error,
            expires: self.now + TOAST_SECS,
        });
    }

    fn log_line(&mut self, text: &str) {
        self.log.push(text.to_string());
        if self.log.len() > 120 {
            self.log.remove(0);
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|toast| toast.expires > self.now);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("mantis_toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for toast in &self.toasts {
                    let (background, foreground) = if toast.error {
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
                    egui::Frame::popup(ui.style())
                        .fill(background)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(&toast.text).color(foreground));
                        });
                }
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    // ------------------------------------------------------------------
    // sync flows
    // ------------------------------------------------------------------

    fn process_sync(&mut self, ctx: &egui::Context) {
        for event in self.sync.drain() {
            let flow = self.sync.flow;
            match (flow, event) {
                (
                    flow,
                    SyncEvent::PreflightOk {
                        api_version,
                        app_version,
                        git_sha,
                    },
                ) if flow != Flow::Idle => {
                    self.sync.server_api_version = api_version;
                    self.sync.server_app_version = app_version;
                    self.sync.server_git_sha = git_sha;
                    self.sync.continue_after_preflight(ctx);
                }
                (Flow::Projects, SyncEvent::Projects(projects)) => {
                    self.sync.flow = Flow::Idle;
                    self.remote_projects = projects;
                    self.show_remote_projects = true;
                }
                (Flow::Check, SyncEvent::Info(info)) => {
                    if let Err(error) = self.handle_check_info(info) {
                        if self.quiet_flow {
                            self.log_line(&format!("remote check rejected: {error}"));
                        } else {
                            self.toast(error, true);
                        }
                    }
                    self.sync.flow = Flow::Idle;
                }
                (Flow::Pull, SyncEvent::Info(info)) => self.handle_pull_info(info, ctx),
                (
                    Flow::Pull,
                    SyncEvent::Blocks {
                        from,
                        blocks,
                        next_from,
                        state,
                    },
                ) => self.handle_pull_blocks(from, blocks, next_from, state, ctx, false),
                (Flow::Push { .. }, SyncEvent::Info(info)) => self.handle_push_info(info, ctx),
                (
                    Flow::Push { .. },
                    SyncEvent::Blocks {
                        from,
                        blocks,
                        next_from,
                        state,
                    },
                ) => self.handle_pull_blocks(from, blocks, next_from, state, ctx, true),
                (
                    Flow::Push { .. },
                    SyncEvent::PushOk {
                        len,
                        head,
                        appended,
                    },
                ) => self.handle_push_ok(len, head, appended, ctx),
                (Flow::Push { .. }, SyncEvent::PushConflict { code, msg }) => {
                    self.sync.flow = Flow::Idle;
                    self.push_progress = None;
                    self.remote_ahead = true;
                    self.toast(
                        format!("push conflict [{code}]: {msg}; Pull and review before retrying"),
                        true,
                    );
                }
                (
                    active_flow,
                    SyncEvent::Failed {
                        context,
                        status,
                        code,
                        msg,
                    },
                ) => {
                    self.sync.flow = Flow::Idle;
                    if matches!(active_flow, Flow::Push { .. }) {
                        self.push_progress = None;
                    }
                    let status = status
                        .map(|value| format!(" HTTP {value}"))
                        .unwrap_or_default();
                    let line = format!("sync {context} failed{status} [{code}]: {msg}");
                    if self.quiet_flow {
                        self.log_line(&line);
                    } else {
                        self.toast(line, true);
                    }
                }
                (Flow::Idle, _) => {}
                (Flow::Projects | Flow::Check | Flow::Pull | Flow::Push { .. }, _) => {}
            }
        }
        if self.sync.flow == Flow::Idle && self.quiet_flow {
            self.quiet_flow = false;
        }
    }

    fn handle_check_info(&mut self, info: RemoteInfo) -> Result<(), String> {
        self.check_remote_identity(&info)?;
        match compare_heads(&self.doc.chain.blocks, info.len, &info.head) {
            HeadRelation::Same | HeadRelation::LocalAhead => self.remote_ahead = false,
            HeadRelation::RemoteAhead => self.remote_ahead = true,
            HeadRelation::Diverged | HeadRelation::InvalidRemote => {
                self.log_line("remote check detected divergent or invalid history");
                self.remote_ahead = true;
            }
        }
        self.sync.last_info = Some(info);
        Ok(())
    }

    fn remote_is_local_prefix(&self, info: &RemoteInfo) -> bool {
        matches!(
            compare_heads(&self.doc.chain.blocks, info.len, &info.head),
            HeadRelation::Same | HeadRelation::LocalAhead
        )
    }

    fn check_remote_identity(&self, info: &RemoteInfo) -> Result<(), String> {
        Self::validate_remote_identity(
            &self.doc.chain,
            self.doc.is_pristine(),
            &self.sync.project_id,
            info,
        )
    }

    fn validate_remote_identity(
        local_chain: &mantis_chain::Chain,
        local_is_pristine: bool,
        requested_project: &str,
        info: &RemoteInfo,
    ) -> Result<(), String> {
        if !info.project_id.is_empty() && info.project_id != requested_project {
            return Err(format!(
                "remote returned project `{}` while `{}` was requested",
                info.project_id, requested_project
            ));
        }
        let local_genesis = &local_chain.blocks[0].hash;
        if !info.genesis_hash.is_empty()
            && info.genesis_hash != *local_genesis
            && !local_is_pristine
        {
            return Err(
                "remote genesis differs from this workspace; open it in a new workspace".into(),
            );
        }
        if info.genesis_hash == *local_genesis {
            let local_format = local_chain
                .format_version()
                .map_err(|error| format!("local chain format is invalid: {error}"))?;
            if info.chain_format_version != local_format {
                return Err(format!(
                    "chain format mismatch: local v{local_format}, remote v{}",
                    info.chain_format_version
                ));
            }
            let local_chain_id = local_chain
                .chain_id()
                .map_err(|error| format!("local chain id is invalid: {error}"))?;
            if info.chain_id.as_deref() != local_chain_id {
                return Err("remote chain id differs from this workspace".into());
            }
        }
        Ok(())
    }

    fn handle_pull_info(&mut self, info: RemoteInfo, ctx: &egui::Context) {
        if let Err(error) = self.check_remote_identity(&info) {
            self.sync.flow = Flow::Idle;
            self.toast(error, true);
            return;
        }
        let ours = self.doc.chain.len();
        let local_head = self.doc.chain.head().hash.clone();
        let needs_adoption = !info.genesis_hash.is_empty()
            && info.genesis_hash != self.doc.chain.blocks[0].hash
            && self.doc.is_pristine();
        self.sync.last_info = Some(info.clone());
        if needs_adoption {
            self.adopting_remote = true;
            self.sync.fetch_blocks(0, ctx);
        } else if info.len > ours {
            self.sync.fetch_blocks(ours, ctx);
        } else if info.len == ours && info.head != local_head {
            self.sync.flow = Flow::Idle;
            self.remote_ahead = true;
            self.toast(
                "divergent history: local and remote have equal length but different heads",
                true,
            );
        } else if info.len < ours && !self.remote_is_local_prefix(&info) {
            self.sync.flow = Flow::Idle;
            self.remote_ahead = true;
            self.toast(
                "divergent history: remote head is not a prefix of local history",
                true,
            );
        } else {
            self.sync.flow = Flow::Idle;
            self.remote_ahead = false;
            if info.len < ours {
                self.toast(
                    "local commits are ahead; Push explicitly to publish them",
                    false,
                );
            } else {
                self.toast("chain up to date", false);
            }
        }
    }

    fn handle_push_info(&mut self, info: RemoteInfo, ctx: &egui::Context) {
        self.push_progress = None;
        if let Err(error) = self.check_remote_identity(&info) {
            self.sync.flow = Flow::Idle;
            self.toast(error, true);
            return;
        }
        if matches!(info.access, AccessState::ReadOnly) {
            self.sync.flow = Flow::Idle;
            self.toast("this signing key has read-only access", true);
            return;
        }
        let ours = self.doc.chain.len();
        if info.len > ours {
            self.sync.last_info = Some(info);
            self.sync.flow = Flow::Idle;
            self.remote_ahead = true;
            self.toast("remote is ahead; Pull and review before Push", true);
            return;
        }
        if info.len == 0 || info.len > self.doc.chain.blocks.len() {
            self.sync.flow = Flow::Idle;
            self.toast("remote returned an invalid chain length", true);
            return;
        }
        let expected = &self.doc.chain.blocks[info.len - 1].hash;
        if *expected != info.head {
            self.sync.flow = Flow::Idle;
            self.remote_ahead = true;
            self.toast(
                "divergent history: remote head is not a prefix of local history",
                true,
            );
            return;
        }
        let len = info.len;
        let head = info.head.clone();
        self.sync.last_info = Some(info);
        if len == ours {
            self.sync.flow = Flow::Idle;
            self.toast("nothing to push (Commit first)", false);
            return;
        }
        match PushProgress::new(&self.doc.chain.blocks, len, &head) {
            Ok(progress) => {
                self.push_progress = Some(progress);
                if let Err(error) = self.post_next_push_chunk(ctx) {
                    self.sync.flow = Flow::Idle;
                    self.push_progress = None;
                    self.toast(format!("push cannot start: {error}"), true);
                }
            }
            Err(error) => {
                self.sync.flow = Flow::Idle;
                self.toast(format!("push cannot start: {error}"), true);
            }
        }
    }

    fn handle_push_ok(&mut self, len: usize, head: String, appended: usize, ctx: &egui::Context) {
        let completed = match self.push_progress.as_mut() {
            Some(progress) => progress.acknowledge(&self.doc.chain.blocks, len, &head, appended),
            None => Err("received a push response without active progress".into()),
        };
        let completed = match completed {
            Ok(completed) => completed,
            Err(error) => {
                self.sync.flow = Flow::Idle;
                self.push_progress = None;
                self.remote_ahead = true;
                self.toast(
                    format!("invalid push response: {error}; Check or Pull before retrying"),
                    true,
                );
                return;
            }
        };
        if let Some(info) = &mut self.sync.last_info {
            info.len = len;
            info.head = head;
        }
        if completed {
            let total = self
                .push_progress
                .as_ref()
                .map(PushProgress::total_appended)
                .unwrap_or(0);
            self.sync.flow = Flow::Idle;
            self.push_progress = None;
            self.remote_ahead = false;
            self.toast(
                format!("pushed {total} block(s) ✓ (remote at {len})"),
                false,
            );
        } else if let Err(error) = self.post_next_push_chunk(ctx) {
            self.sync.flow = Flow::Idle;
            self.push_progress = None;
            self.remote_ahead = true;
            self.toast(format!("push stopped: {error}"), true);
        }
    }

    fn handle_pull_blocks(
        &mut self,
        from: usize,
        mut blocks: Vec<mantis_chain::Block>,
        next_from: Option<usize>,
        page_state: PageState,
        ctx: &egui::Context,
        push_flow: bool,
    ) {
        let Some(target) = self.sync.last_info.clone() else {
            self.sync.flow = Flow::Idle;
            self.toast("received blocks without a frozen Pull target", true);
            return;
        };
        if page_state.genesis != target.genesis_hash
            || page_state.len < target.len
            || (page_state.len == target.len && page_state.head != target.head)
        {
            self.sync.flow = Flow::Idle;
            self.toast("remote changed incompatibly during paginated Pull", true);
            return;
        }
        let expected_from = if self.adopting_remote {
            0
        } else {
            self.doc.chain.len()
        };
        if from != expected_from {
            self.sync.flow = Flow::Idle;
            self.toast(
                format!("non-contiguous block page {from}; expected {expected_from}"),
                true,
            );
            return;
        }
        let remaining = target.len.saturating_sub(from);
        blocks.truncate(remaining);
        if blocks.is_empty() && from < target.len {
            self.sync.flow = Flow::Idle;
            self.toast(
                "remote returned an empty block page before the Pull target",
                true,
            );
            return;
        }
        let ok = if self.adopting_remote {
            self.adopting_remote = false;
            match self.doc.adopt_remote(blocks) {
                Ok(_) => true,
                Err(error) => {
                    self.toast(error, true);
                    false
                }
            }
        } else {
            self.merge_blocks(&blocks)
        };
        if !ok {
            self.sync.flow = Flow::Idle;
            return;
        }
        match next_page(self.doc.chain.len(), target.len, next_from) {
            Ok(Some(next)) => self.sync.fetch_blocks(next, ctx),
            Ok(None) => {
                if self.doc.chain.head().hash != target.head {
                    self.sync.flow = Flow::Idle;
                    self.remote_ahead = true;
                    self.toast("Pull target head did not match the replayed history", true);
                    return;
                }
                self.sync.flow = Flow::Idle;
                self.remote_ahead = false;
                self.toast(
                    format!("opened remote history ({} blocks)", target.len),
                    false,
                );
                if push_flow {
                    self.toast("remote changes pulled; review, then Push again", false);
                }
            }
            Err(error) => {
                self.sync.flow = Flow::Idle;
                self.remote_ahead = true;
                self.toast(format!("pagination failed: {error}"), true);
            }
        }
    }

    fn merge_blocks(&mut self, blocks: &[mantis_chain::Block]) -> bool {
        match self.doc.merge_remote(blocks) {
            Ok(report) => {
                if report.appended > 0 {
                    self.toast(format!("pulled {} block(s)", report.appended), false);
                }
                if report.dropped > 0 {
                    self.show_recovery = true;
                    self.toast(
                        format!(
                            "{} pending op(s) conflicted and were preserved in Recovery",
                            report.dropped
                        ),
                        true,
                    );
                }
                true
            }
            Err(error) => {
                self.toast(format!("merge failed: {error}"), true);
                false
            }
        }
    }

    fn post_next_push_chunk(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let progress = self
            .push_progress
            .as_ref()
            .ok_or_else(|| "push progress is missing".to_string())?;
        let base_len = progress.acknowledged_len();
        let chunk = build_push_chunk(
            &self.doc.chain.blocks,
            base_len,
            progress.acknowledged_head(),
            progress.target_len(),
        )?;
        let end_len = chunk.end_len;
        let body_len = chunk.body.len();
        let block_count = chunk.block_count;
        let op_count = chunk.op_count;
        self.push_progress
            .as_mut()
            .expect("push progress checked above")
            .mark_inflight(end_len)?;
        self.log_line(&format!(
            "pushing blocks {base_len}..{end_len} ({block_count} blocks, {op_count} ops, {body_len} bytes)"
        ));
        self.sync.post_push_body(chunk.body, ctx);
        Ok(())
    }

    // ------------------------------------------------------------------
    // UI helpers
    // ------------------------------------------------------------------

    fn undo_pending(&mut self, errors: &mut Vec<String>) {
        match self.doc.undo_pending() {
            Ok(count) => self.toast(format!("undid {count} pending op(s)"), false),
            Err(error) => errors.push(error),
        }
    }

    fn redo_pending(&mut self, errors: &mut Vec<String>) {
        match self.doc.redo_pending() {
            Ok(count) => self.toast(format!("redid {count} pending op(s)"), false),
            Err(error) => errors.push(error),
        }
    }

    fn history_shortcuts(&mut self, ctx: &egui::Context, errors: &mut Vec<String>) {
        if ctx.wants_keyboard_input() || !self.doc.editable() {
            return;
        }
        let redo = ctx.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            )) || input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::Y,
            ))
        });
        if redo {
            if self.doc.can_redo() {
                self.redo_pending(errors);
            }
            return;
        }
        let undo = ctx.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::Z,
            ))
        });
        if undo && self.doc.can_undo() {
            self.undo_pending(errors);
        }
    }

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
                ui.label("workspace:");
                if self.storage_disabled {
                    ui.colored_label(egui::Color32::LIGHT_RED, "storage unavailable");
                }
                let active = self.catalog.active_id.clone();
                let mut selected = None;
                egui::ComboBox::from_id_salt("workspace_picker")
                    .selected_text(nonempty_name(&self.workspace_name))
                    .show_ui(ui, |ui| {
                        for workspace in &self.catalog.workspaces {
                            if ui
                                .selectable_label(workspace.id == active, &workspace.name)
                                .clicked()
                            {
                                selected = Some(workspace.id.clone());
                            }
                        }
                    });
                if let Some(id) = selected {
                    if let Err(error) = self.switch_workspace(&id) {
                        errors.push(error);
                    }
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.workspace_name)
                        .desired_width(110.0)
                        .hint_text("workspace name"),
                );
                if ui.button("＋").on_hover_text("new workspace").clicked() {
                    self.new_workspace_name = "Untitled".into();
                    self.show_new_workspace = true;
                }
                if ui.button("duplicate").clicked() {
                    if let Err(error) = self.duplicate_workspace() {
                        errors.push(error);
                    }
                }
                if ui
                    .button("file…")
                    .on_hover_text("import/export .mantis")
                    .clicked()
                {
                    self.transfer_dialog.open = true;
                    self.transfer_dialog.error.clear();
                }
                if ui.button("delete").clicked() {
                    self.delete_workspace = Some(active);
                }
                ui.separator();
                ui.label("you:");
                ui.add(egui::TextEdit::singleline(&mut self.doc.identity.name).desired_width(80.0))
                    .on_hover_text("author name recorded on commits");
                if ui.button("key…").clicked() {
                    self.key_dialog.open = true;
                    self.key_dialog.error.clear();
                }
                if !self.catalog.settings.key_backup_confirmed {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe8, 0xc0, 0x6a),
                        "key backup not confirmed",
                    )
                    .on_hover_text(
                        "Losing this device key loses signing access. Create an encrypted backup, copy or save it, then confirm it here.",
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.show_chain {
                        "chain ▸"
                    } else {
                        "◂ chain"
                    };
                    if ui.button(label).clicked() {
                        self.show_chain = !self.show_chain;
                    }
                });
            });
            ui.horizontal(|ui| {
                let busy = self.sync.busy();
                let attach_label = if self.remote_attached {
                    "detach"
                } else {
                    "attach"
                };
                if ui
                    .add_enabled(!busy, egui::Button::new(attach_label))
                    .clicked()
                {
                    self.remote_attached = !self.remote_attached;
                    self.remote_connection_confirmed = true;
                    self.sync.last_info = None;
                    self.remote_ahead = false;
                }
                ui.label("server:");
                if ui
                    .add_enabled(
                        self.remote_attached && !self.sync.busy(),
                        egui::TextEdit::singleline(&mut self.sync.url)
                            .desired_width(170.0)
                            .hint_text("same origin"),
                    )
                    .changed()
                {
                    self.sync.last_info = None;
                    self.remote_connection_confirmed = false;
                    self.sync.server_api_version = 0;
                    self.sync.server_app_version.clear();
                    self.sync.server_git_sha.clear();
                    self.remote_ahead = false;
                }
                ui.label("project:");
                if ui
                    .add_enabled(
                        self.remote_attached && !self.sync.busy(),
                        egui::TextEdit::singleline(&mut self.sync.project_id).desired_width(100.0),
                    )
                    .changed()
                {
                    self.sync.last_info = None;
                    self.remote_connection_confirmed = false;
                    self.remote_ahead = false;
                }
                if ui
                    .add_enabled(self.remote_attached && !busy, egui::Button::new("Check"))
                    .on_hover_text("read remote head without changing this workspace")
                    .clicked()
                {
                    self.quiet_flow = false;
                    self.remote_connection_confirmed = true;
                    self.sync.start_check(ctx);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Browse…"))
                    .on_hover_text("list public projects on this server")
                    .clicked()
                {
                    self.quiet_flow = false;
                    self.remote_connection_confirmed = true;
                    self.sync.start_projects(ctx);
                }
                if ui
                    .add_enabled(self.remote_attached && !busy, egui::Button::new("⬇ Pull"))
                    .on_hover_text("explicitly fetch and replay remote blocks")
                    .clicked()
                {
                    self.quiet_flow = false;
                    self.remote_connection_confirmed = true;
                    self.sync.start_pull(ctx);
                }
                let writable = self
                    .sync
                    .last_info
                    .as_ref()
                    .map(|info| info.access != AccessState::ReadOnly)
                    .unwrap_or(true);
                if ui
                    .add_enabled(
                        self.remote_attached && !busy && writable,
                        egui::Button::new("⬆ Push"),
                    )
                    .on_hover_text("explicitly publish committed blocks")
                    .clicked()
                {
                    self.quiet_flow = false;
                    self.push_progress = None;
                    self.remote_connection_confirmed = true;
                    self.sync.start_push(ctx);
                }
                ui.checkbox(&mut self.sync.auto_pull, "background check")
                    .on_hover_text(
                        "only checks whether remote is ahead; never Pulls automatically",
                    );
                if self.remote_attached && !self.remote_connection_confirmed {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe8, 0xc0, 0x6a),
                        "unconfirmed remote",
                    )
                    .on_hover_text(
                        "Imported server addresses never connect automatically. Use Check, Pull, Push, or attach explicitly to confirm this origin.",
                    );
                }
                if self.remote_ahead {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe8, 0xc0, 0x6a),
                        "remote ahead/diverged",
                    );
                } else if let Some(info) = &self.sync.last_info {
                    ui.weak(info.access.label());
                }
                if !self.sync.server_app_version.is_empty() {
                    let revision: String = self.sync.server_git_sha.chars().take(12).collect();
                    ui.weak(format!("server {}", self.sync.server_app_version))
                        .on_hover_text(format!(
                            "API v{}\nrevision {}",
                            self.sync.server_api_version,
                            if revision.is_empty() {
                                "unknown"
                            } else {
                                &revision
                            }
                        ));
                }
                if busy {
                    ui.spinner();
                }
            });
            ui.horizontal(|ui| {
                ui.label("commit:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.commit_msg)
                        .hint_text("message…")
                        .desired_width(220.0),
                );
                let can_commit = self.doc.editable() && !self.doc.pending.is_empty();
                if ui
                    .add_enabled(
                        can_commit,
                        egui::Button::new(format!("Commit ({})", self.doc.pending.len())),
                    )
                    .clicked()
                {
                    match self.doc.commit(&self.commit_msg, now_ms()) {
                        Ok(count) => {
                            self.toast(format!("committed {count} op(s)"), false);
                            self.commit_msg.clear();
                        }
                        Err(error) => errors.push(error),
                    }
                }
                ui.separator();
                if ui
                    .add_enabled(self.doc.can_undo(), egui::Button::new("↶ undo"))
                    .on_hover_text(format!(
                        "Undo one uncommitted edit (Ctrl/Cmd+Z)\n{} step(s) available",
                        self.doc.undo_depth()
                    ))
                    .clicked()
                {
                    self.undo_pending(errors);
                }
                if ui
                    .add_enabled(self.doc.can_redo(), egui::Button::new("↷ redo"))
                    .on_hover_text(format!(
                        "Redo one uncommitted edit\n{} step(s) available",
                        self.doc.redo_depth()
                    ))
                    .clicked()
                {
                    self.redo_pending(errors);
                }
                if !self.doc.recovery_ops.is_empty()
                    && ui
                        .button(format!("Recovery ({})", self.doc.recovery_ops.len()))
                        .clicked()
                {
                    self.show_recovery = true;
                }
                ui.separator();
                if self.doc.is_time_traveling() {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe8, 0xc0, 0x6a),
                        format!("⏱ block #{} — read-only", self.doc.viewed_block()),
                    );
                    if ui.button("back to head").clicked() {
                        if let Err(error) = self.doc.set_view(None) {
                            errors.push(error);
                        }
                    }
                    ui.separator();
                }
                ui.weak(self.stats_label());
            });
            ui.add_space(2.0);
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, errors: &mut Vec<String>) {
        if self.show_new_workspace {
            let mut open = true;
            egui::Window::new("New workspace")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Workspace name");
                    ui.text_edit_singleline(&mut self.new_workspace_name);
                    if ui.button("Create").clicked() {
                        let name = self.new_workspace_name.clone();
                        match self.create_workspace(&name) {
                            Ok(()) => self.show_new_workspace = false,
                            Err(error) => errors.push(error),
                        }
                    }
                });
            if !open {
                self.show_new_workspace = false;
            }
        }

        if let Some(id) = self.delete_workspace.clone() {
            let mut open = true;
            egui::Window::new("Delete workspace?")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("This removes its local snapshot from this device.");
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.delete_workspace = None;
                        }
                        if ui.button("Delete").clicked() {
                            if let Err(error) = self.delete_workspace_now(&id) {
                                errors.push(error);
                            }
                            self.delete_workspace = None;
                        }
                    });
                });
            if !open {
                self.delete_workspace = None;
            }
        }

        self.key_backup_dialog(ctx);
        self.workspace_transfer_dialog(ctx, errors);
        self.remote_projects_dialog(ctx, errors);
        self.recovery_dialog(ctx, errors);
    }

    fn remote_projects_dialog(&mut self, ctx: &egui::Context, errors: &mut Vec<String>) {
        if !self.show_remote_projects {
            return;
        }
        let mut open = true;
        let mut selected: Option<(String, String)> = None;
        egui::Window::new("Open remote project")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                let endpoint = if self.sync.url.trim().is_empty() {
                    "this site"
                } else {
                    self.sync.url.trim()
                };
                ui.label(format!(
                    "{} public project(s) at {endpoint}",
                    self.remote_projects.len()
                ));
                if self.remote_projects.is_empty() {
                    ui.weak("No active public projects were returned.");
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for project in &self.remote_projects {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.strong(&project.title);
                                    ui.weak(format!(
                                        "{} · {} blocks · chain v{}{}",
                                        project.project_id,
                                        project.state.len,
                                        project.chain_format_version,
                                        if project.archived { " · archived" } else { "" }
                                    ));
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !project.archived,
                                                egui::Button::new("Open"),
                                            )
                                            .clicked()
                                        {
                                            selected = Some((
                                                project.project_id.to_string(),
                                                project.title.clone(),
                                            ));
                                        }
                                    },
                                );
                            });
                            ui.separator();
                        }
                    });
                if ui.button("Refresh").clicked() {
                    self.sync.start_projects(ctx);
                    self.show_remote_projects = false;
                }
            });
        if let Some((project_id, title)) = selected {
            let server_url = self.sync.url.clone();
            match self.create_workspace(&title) {
                Ok(()) => {
                    self.remote_attached = true;
                    self.sync.url = server_url;
                    self.sync.project_id = project_id;
                    self.sync.last_info = None;
                    self.show_remote_projects = false;
                    self.sync.start_pull(ctx);
                }
                Err(error) => errors.push(error),
            }
        }
        if !open {
            self.show_remote_projects = false;
        }
    }

    fn workspace_transfer_dialog(&mut self, ctx: &egui::Context, errors: &mut Vec<String>) {
        if !self.transfer_dialog.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Mantis workspace file")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(
                    "Portable .mantis JSON includes chain, pending work, recovery ops and a validated remote anchor. It never includes your signing key.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Export active workspace").clicked() {
                        let result = self
                            .snapshot(true)
                            .to_portable()
                            .and_then(|portable| {
                                serde_json::to_string_pretty(&portable)
                                    .map_err(|error| error.to_string())
                            });
                        match result {
                            Ok(payload) => {
                                self.transfer_dialog.payload = payload;
                                self.transfer_dialog.error.clear();
                            }
                            Err(error) => self.transfer_dialog.error = error,
                        }
                    }
                    if ui.button("Import pasted workspace").clicked() {
                        let result = serde_json::from_str::<mantis_protocol::PortableWorkspaceV1>(
                            &self.transfer_dialog.payload,
                        )
                        .map_err(|error| format!("invalid .mantis JSON: {error}"))
                        .and_then(WorkspaceSnapshotV1::from_portable)
                        .and_then(|mut snapshot| {
                            if self
                                .catalog
                                .workspaces
                                .iter()
                                .any(|workspace| workspace.id == snapshot.id)
                            {
                                snapshot.id = next_workspace_id(&self.catalog);
                                snapshot.name = format!("{} import", snapshot.name);
                            }
                            let id = snapshot.id.clone();
                            self.catalog.workspaces.push(snapshot);
                            if let Err(error) = self.switch_workspace(&id) {
                                self.catalog.workspaces.retain(|workspace| workspace.id != id);
                                return Err(error);
                            }
                            Ok(())
                        });
                        match result {
                            Ok(()) => {
                                self.transfer_dialog.error.clear();
                                self.transfer_dialog.open = false;
                            }
                            Err(error) => {
                                self.transfer_dialog.error = error.clone();
                                errors.push(error);
                            }
                        }
                    }
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(self.transfer_dialog.payload.clone());
                    }
                });
                ui.add(
                    egui::TextEdit::multiline(&mut self.transfer_dialog.payload)
                        .desired_rows(14)
                        .code_editor()
                        .hint_text("paste JSON here or save exported text as project.mantis"),
                );
                if !self.transfer_dialog.error.is_empty() {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.transfer_dialog.error);
                }
            });
        if !open {
            self.transfer_dialog.open = false;
        }
    }

    fn key_backup_dialog(&mut self, ctx: &egui::Context) {
        if !self.key_dialog.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Signing key backup")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(format!("Public key: {}", self.doc.identity.public_hex()));
                ui.label("Use at least 8 characters. The password is never saved.");
                ui.add(
                    egui::TextEdit::singleline(&mut self.key_dialog.password)
                        .password(true)
                        .hint_text("backup password"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Create encrypted backup").clicked() {
                        match key_backup::export(&self.doc.identity, &self.key_dialog.password) {
                            Ok(payload) => {
                                self.key_dialog.payload = payload;
                                self.key_dialog.error.clear();
                                self.key_dialog.generated_for =
                                    Some(self.doc.identity.public_hex());
                                self.key_dialog.password.clear();
                            }
                            Err(error) => self.key_dialog.error = error,
                        }
                    }
                    if ui
                        .add_enabled(!self.sync.busy(), egui::Button::new("Import pasted backup"))
                        .clicked()
                    {
                        match key_backup::import(
                            &self.key_dialog.payload,
                            &self.key_dialog.password,
                        ) {
                            Ok(identity) => {
                                self.catalog_durability.require_durable_save();
                                self.doc.identity = identity;
                                self.key_dialog.error.clear();
                                self.key_dialog.password.clear();
                                self.catalog.settings.key_backup_confirmed = false;
                                self.key_dialog.generated_for = None;
                                self.sync.last_info = None;
                                self.persist_now();
                            }
                            Err(error) => self.key_dialog.error = error,
                        }
                    }
                    if ui
                        .add_enabled(
                            !self.key_dialog.payload.is_empty(),
                            egui::Button::new("Copy"),
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(self.key_dialog.payload.clone());
                        if generated_backup_matches(
                            &self.key_dialog,
                            &self.doc.identity.public_hex(),
                        ) {
                            self.catalog.settings.key_backup_confirmed = true;
                        }
                    }
                });
                let payload_response = ui.add(
                    egui::TextEdit::multiline(&mut self.key_dialog.payload)
                        .desired_rows(10)
                        .code_editor()
                        .hint_text("paste or save this JSON as a .mantis-key file"),
                );
                if payload_response.changed() {
                    self.key_dialog.generated_for = None;
                }
                let generated_for_current =
                    generated_backup_matches(&self.key_dialog, &self.doc.identity.public_hex());
                if ui
                    .add_enabled(
                        generated_for_current,
                        egui::Button::new("I saved this encrypted backup"),
                    )
                    .clicked()
                {
                    self.catalog.settings.key_backup_confirmed = true;
                }
                if self.catalog.settings.key_backup_confirmed {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "backup saved ✓");
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe8, 0xc0, 0x6a),
                        "backup is not yet confirmed",
                    );
                }
                if !self.key_dialog.error.is_empty() {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.key_dialog.error);
                }
            });
        if !open {
            self.key_dialog.open = false;
            self.key_dialog.password.clear();
        }
    }

    fn recovery_dialog(&mut self, ctx: &egui::Context, errors: &mut Vec<String>) {
        if !self.show_recovery {
            return;
        }
        let mut open = true;
        egui::Window::new("Conflict recovery")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(
                    "These operations did not apply after Pull. They remain stored until you retry or explicitly clear them.",
                );
                if ui.button("Retry all atomically").clicked() {
                    let ops = self.doc.recovery_ops.clone();
                    match self.doc.apply_ops(ops) {
                        Ok(_) => {
                            self.doc.recovery_ops.clear();
                            self.show_recovery = false;
                        }
                        Err(error) => errors.push(format!("recovery retry failed: {error}")),
                    }
                }
                let json = serde_json::to_string_pretty(&self.doc.recovery_ops)
                    .unwrap_or_else(|_| "[]".into());
                if ui.button("Copy recovery JSON").clicked() {
                    ui.ctx().copy_text(json.clone());
                }
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.monospace(json);
                });
                ui.separator();
                if ui.button("Clear recovery operations").clicked() {
                    self.doc.recovery_ops.clear();
                    self.show_recovery = false;
                }
            });
        if !open {
            self.show_recovery = false;
        }
    }

    fn storage_gate(&mut self, ctx: &egui::Context) -> bool {
        if !self.storage_ready {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Opening durable workspaces…");
                    });
                });
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return false;
        }
        let Some(raw) = self.corrupt_catalog.clone() else {
            return true;
        };
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Workspace recovery required");
            ui.label("The saved catalog failed validation and has not been overwritten.");
            if ui.button("Copy original catalog JSON").clicked() {
                ui.ctx().copy_text(raw.clone());
            }
            if ui
                .button("Start fresh after copying recovery data")
                .clicked()
            {
                self.corrupt_catalog = None;
                self.persist_now();
            }
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    ui.monospace(&raw);
                });
        });
        false
    }

    fn stats_label(&self) -> String {
        let chain_bytes = self.doc.chain.byte_size();
        let geometry_bytes = self.viewport.geometry_bytes();
        let mut value = format!(
            "{} blocks · {} ops · {} on chain ↔ {} geometry",
            self.doc.chain.len(),
            self.doc.chain.total_ops(),
            format_bytes(chain_bytes),
            format_bytes(geometry_bytes),
        );
        if geometry_bytes > 0 && chain_bytes > 0 {
            let ratio = geometry_bytes as f64 / chain_bytes as f64;
            if ratio >= 1.0 {
                value.push_str(&format!(" ({ratio:.0}× lighter)"));
            }
        }
        value
    }
}

impl eframe::App for MantisApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.now = ctx.input(|input| input.time);
        self.process_persistence();
        if !self.storage_gate(ctx) {
            self.show_toasts(ctx);
            return;
        }
        let mut errors = Vec::new();

        self.sync.public_key = self.doc.identity.public_hex();
        self.sync.local_chain_format = self.doc.chain.format_version().unwrap_or(1);
        self.process_sync(ctx);
        if self.remote_attached && self.remote_connection_confirmed && self.sync.auto_pull {
            if !self.sync.busy() && self.now - self.sync.last_auto_pull >= REMOTE_CHECK_PERIOD {
                self.sync.last_auto_pull = self.now;
                self.quiet_flow = true;
                self.sync.start_check(ctx);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }

        self.doc.evaluate();
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
            .show(ctx, |ui| self.editor.ui(ui, &mut self.doc, &mut errors));

        self.history_shortcuts(ctx, &mut errors);
        let selection_changed = self.editor.selection != self.last_selection;
        if self.doc.take_scene_dirty() || selection_changed {
            self.doc.evaluate();
            self.viewport
                .rebuild_scene(&self.doc, &self.editor.selection);
            if selection_changed {
                self.last_selection = self.editor.selection.clone();
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::default())
            .show(ctx, |ui| self.viewport.ui(ui));

        let pointer_down = ctx.input(|input| input.pointer.any_down());
        let has_focus = ctx.memory(|memory| memory.focused().is_some());
        if self.doc.gesture_active() && !pointer_down && !has_focus {
            self.doc.end_gesture();
        }

        self.dialogs(ctx, &mut errors);
        for error in errors {
            self.toast(error, true);
        }
        self.autosave(ctx);
        self.show_toasts(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.doc.end_gesture();
        let legacy_secret = legacy_secret_for_save(
            self.catalog_durability.durable_catalog_confirmed,
            self.storage_ready,
            self.storage_disabled,
            self.corrupt_catalog.is_some(),
            &self.doc.identity.secret_hex(),
        );
        self.persist_now();
        storage.set_string(K_NAME, self.doc.identity.name.clone());
        storage.set_string(K_SECRET, legacy_secret);
        storage.set_string(K_URL, self.sync.url.clone());
        storage.set_string(K_AUTO, if self.sync.auto_pull { "1" } else { "0" }.into());
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        self.doc.end_gesture();
        self.persist_now();
        if let Some(gl) = gl {
            viewport::destroy_gl(&self.viewport.shared, gl);
        }
    }
}

fn nonempty_name(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "Untitled".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

fn next_workspace_id(catalog: &WorkspaceCatalogV1) -> String {
    let prefix = format!("workspace-{}", now_ms());
    if !catalog
        .workspaces
        .iter()
        .any(|workspace| workspace.id == prefix)
    {
        return prefix;
    }
    for suffix in 2.. {
        let id = format!("{prefix}-{suffix}");
        if !catalog
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
        {
            return id;
        }
    }
    unreachable!()
}

fn remote_info_from_anchor(anchor: &RemoteAnchorV1) -> Option<RemoteInfo> {
    if anchor.last_synced_head.is_empty() {
        return None;
    }
    Some(RemoteInfo {
        api_version: 2,
        chain_format_version: anchor.chain_format_version,
        project_id: anchor.project_id.clone(),
        chain_id: anchor.chain_id.clone(),
        genesis_hash: anchor.genesis_hash.clone(),
        len: anchor.last_synced_len,
        head: anchor.last_synced_head.clone(),
        access: anchor.access,
    })
}

fn restore_camera(viewport: &mut ViewportPanel, metadata: &CameraMetadataV1) {
    viewport.camera.target = Vec3::new(metadata.target[0], metadata.target[1], metadata.target[2]);
    viewport.camera.distance = metadata.distance;
    viewport.camera.yaw = metadata.yaw;
    viewport.camera.pitch = metadata.pitch;
}

fn legacy_secret_for_save(
    durable_catalog_confirmed: bool,
    storage_ready: bool,
    storage_disabled: bool,
    catalog_corrupt: bool,
    secret: &str,
) -> String {
    // Once the durable catalog is authoritative, erase the legacy eframe
    // duplicate. Keep it only while migration has not completed or recovery
    // is required, so a storage failure cannot destroy the sole usable key.
    if durable_catalog_confirmed && storage_ready && !storage_disabled && !catalog_corrupt {
        String::new()
    } else {
        secret.to_owned()
    }
}

fn generated_backup_matches(dialog: &KeyBackupDialog, public_key: &str) -> bool {
    !dialog.payload.is_empty() && dialog.generated_for.as_deref() == Some(public_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_camera_is_restored_exactly() {
        let metadata = CameraMetadataV1 {
            target: [2.0, -4.5, 9.25],
            distance: 31.0,
            yaw: -1.2,
            pitch: 0.42,
        };
        let mut viewport = ViewportPanel::new();
        restore_camera(&mut viewport, &metadata);
        assert_eq!(viewport.camera.target, Vec3::new(2.0, -4.5, 9.25));
        assert_eq!(viewport.camera.distance, 31.0);
        assert_eq!(viewport.camera.yaw, -1.2);
        assert_eq!(viewport.camera.pitch, 0.42);
    }

    #[test]
    fn legacy_secret_is_cleared_only_after_catalog_migration() {
        assert_eq!(
            legacy_secret_for_save(true, true, false, false, "secret"),
            ""
        );
        assert_eq!(
            legacy_secret_for_save(false, true, false, false, "secret"),
            "secret"
        );
        assert_eq!(
            legacy_secret_for_save(true, false, false, false, "secret"),
            "secret"
        );
        assert_eq!(
            legacy_secret_for_save(true, true, true, false, "secret"),
            "secret"
        );
        assert_eq!(
            legacy_secret_for_save(true, true, false, true, "secret"),
            "secret"
        );
    }

    #[test]
    fn backup_confirmation_only_applies_to_generated_current_key() {
        let mut dialog = KeyBackupDialog {
            payload: "encrypted".into(),
            generated_for: Some("current".into()),
            ..Default::default()
        };
        assert!(generated_backup_matches(&dialog, "current"));
        assert!(!generated_backup_matches(&dialog, "different"));
        dialog.generated_for = None;
        assert!(!generated_backup_matches(&dialog, "current"));
    }

    #[test]
    fn remote_check_identity_is_project_bound_before_state_is_accepted() {
        let chain = mantis_chain::Chain::new_scoped(&"ab".repeat(32)).unwrap();
        let info = RemoteInfo {
            api_version: 2,
            chain_format_version: chain.format_version().unwrap(),
            project_id: "different-project".into(),
            chain_id: chain.chain_id().unwrap().map(str::to_owned),
            genesis_hash: chain.blocks[0].hash.clone(),
            len: chain.len(),
            head: chain.head().hash.clone(),
            access: AccessState::Owner,
        };

        let error = MantisApp::validate_remote_identity(&chain, true, "requested-project", &info)
            .unwrap_err();
        assert!(error.contains("different-project"), "{error}");
        assert!(error.contains("requested-project"), "{error}");
    }
}
