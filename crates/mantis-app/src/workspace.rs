//! Durable, versioned client workspaces.
//!
//! A workspace contains the immutable chain and the local work-in-progress
//! ledger. Signing material deliberately lives in device settings instead of
//! the portable workspace value.

use mantis_chain::Chain;
use mantis_graph::{GraphOp, NodeId};
use mantis_protocol::{
    HashHex, PortableWorkspaceV1, ProjectSlug, RemoteAccessV1, RemoteProjectV1,
    PORTABLE_WORKSPACE_VERSION,
};
use serde::{Deserialize, Serialize};

pub const WORKSPACE_FORMAT_VERSION: u32 = 1;
pub const CATALOG_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessState {
    #[default]
    Unknown,
    ReadOnly,
    Writer,
    Owner,
}

impl AccessState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "access unknown",
            Self::ReadOnly => "read-only",
            Self::Writer => "writer",
            Self::Owner => "owner",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteAnchorV1 {
    pub base_url: String,
    pub project_id: String,
    /// Imported origins require one explicit network action before automatic
    /// checks are allowed. This local consent bit is never portable.
    #[serde(default)]
    pub connection_confirmed: bool,
    #[serde(default)]
    pub chain_format_version: u32,
    #[serde(default)]
    pub chain_id: Option<String>,
    #[serde(default)]
    pub genesis_hash: String,
    #[serde(default)]
    pub last_synced_len: usize,
    #[serde(default)]
    pub last_synced_head: String,
    #[serde(default)]
    pub access: AccessState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraMetadataV1 {
    pub target: [f64; 3],
    pub distance: f64,
    pub yaw: f64,
    pub pitch: f64,
}

impl Default for CameraMetadataV1 {
    fn default() -> Self {
        Self {
            target: [0.0; 3],
            distance: 14.0,
            yaw: 0.9,
            pitch: 0.55,
        }
    }
}

impl CameraMetadataV1 {
    fn validate(&self) -> Result<(), String> {
        if self.target.iter().any(|value| !value.is_finite())
            || !self.distance.is_finite()
            || !(0.05..=400.0).contains(&self.distance)
            || !self.yaw.is_finite()
            || !self.pitch.is_finite()
            || self.pitch.abs() > 1.55
        {
            return Err("workspace camera metadata is invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewMetadataV1 {
    #[serde(default)]
    pub viewed_block: Option<usize>,
    #[serde(default)]
    pub show_chain: bool,
    #[serde(default)]
    pub selected_nodes: Vec<NodeId>,
    #[serde(default)]
    pub camera: CameraMetadataV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSnapshotV1 {
    pub format_version: u32,
    pub id: String,
    pub name: String,
    pub updated_ms: u64,
    pub chain: Chain,
    #[serde(default)]
    pub pending: Vec<GraphOp>,
    /// Operations that could not be replayed after a remote fast-forward.
    /// They remain exportable/recoverable and are never silently discarded.
    #[serde(default)]
    pub recovery_ops: Vec<GraphOp>,
    #[serde(default)]
    pub remote: Option<RemoteAnchorV1>,
    #[serde(default)]
    pub view: ViewMetadataV1,
}

impl WorkspaceSnapshotV1 {
    pub fn validate_envelope(&self) -> Result<(), String> {
        if self.format_version != WORKSPACE_FORMAT_VERSION {
            return Err(format!(
                "unsupported workspace format {} (expected {})",
                self.format_version, WORKSPACE_FORMAT_VERSION
            ));
        }
        if self.id.trim().is_empty() {
            return Err("workspace id is empty".into());
        }
        if self.name.trim().is_empty() {
            return Err("workspace name is empty".into());
        }
        self.view.camera.validate()?;
        Ok(())
    }

    pub fn to_portable(&self) -> Result<PortableWorkspaceV1, String> {
        self.validate_envelope()?;
        let remote = self
            .remote
            .as_ref()
            .filter(|remote| !remote.genesis_hash.is_empty() && !remote.last_synced_head.is_empty())
            .map(|remote| {
                Ok::<RemoteProjectV1, String>(RemoteProjectV1 {
                    base_url: remote.base_url.clone(),
                    project_id: ProjectSlug::new(&remote.project_id)
                        .map_err(|error| error.to_string())?,
                    chain_format_version: remote.chain_format_version,
                    chain_id: remote
                        .chain_id
                        .as_ref()
                        .map(mantis_protocol::ChainId::new)
                        .transpose()
                        .map_err(|error| error.to_string())?,
                    genesis_hash: HashHex::new(&remote.genesis_hash)
                        .map_err(|error| error.to_string())?,
                    last_synced_len: u64::try_from(remote.last_synced_len).map_err(|_| {
                        "last synced length does not fit portable format".to_string()
                    })?,
                    last_synced_head: HashHex::new(&remote.last_synced_head)
                        .map_err(|error| error.to_string())?,
                    access: match remote.access {
                        AccessState::Unknown => RemoteAccessV1::Unknown,
                        AccessState::ReadOnly => RemoteAccessV1::ReadOnly,
                        AccessState::Writer => RemoteAccessV1::Writer,
                        AccessState::Owner => RemoteAccessV1::Owner,
                    },
                })
            })
            .transpose()?;
        let portable = PortableWorkspaceV1 {
            format_version: PORTABLE_WORKSPACE_VERSION,
            id: self.id.clone(),
            name: self.name.clone(),
            updated_ms: self.updated_ms,
            chain: self.chain.clone(),
            pending: self.pending.clone(),
            recovery_ops: self.recovery_ops.clone(),
            remote,
        };
        portable.validate().map_err(|error| error.to_string())?;
        Ok(portable)
    }

    pub fn from_portable(portable: PortableWorkspaceV1) -> Result<Self, String> {
        portable.validate().map_err(|error| error.to_string())?;
        let remote = portable
            .remote
            .map(|remote| {
                Ok::<RemoteAnchorV1, String>(RemoteAnchorV1 {
                    base_url: remote.base_url,
                    project_id: remote.project_id.into_string(),
                    connection_confirmed: false,
                    chain_format_version: remote.chain_format_version,
                    chain_id: remote.chain_id.map(|value| value.into_string()),
                    genesis_hash: remote.genesis_hash.into_string(),
                    last_synced_len: usize::try_from(remote.last_synced_len)
                        .map_err(|_| "last synced length does not fit this client".to_string())?,
                    last_synced_head: remote.last_synced_head.into_string(),
                    access: match remote.access {
                        RemoteAccessV1::Unknown => AccessState::Unknown,
                        RemoteAccessV1::ReadOnly => AccessState::ReadOnly,
                        RemoteAccessV1::Writer => AccessState::Writer,
                        RemoteAccessV1::Owner => AccessState::Owner,
                    },
                })
            })
            .transpose()?;
        Ok(Self {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: portable.id,
            name: portable.name,
            updated_ms: portable.updated_ms,
            chain: portable.chain,
            pending: portable.pending,
            recovery_ops: portable.recovery_ops,
            remote,
            view: ViewMetadataV1 {
                show_chain: true,
                ..Default::default()
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSettingsV1 {
    pub author_name: String,
    pub secret_hex: String,
    pub default_server_url: String,
    #[serde(default)]
    pub background_check: bool,
    #[serde(default)]
    pub key_backup_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCatalogV1 {
    pub format_version: u32,
    pub active_id: String,
    pub settings: DeviceSettingsV1,
    pub workspaces: Vec<WorkspaceSnapshotV1>,
}

impl WorkspaceCatalogV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != CATALOG_FORMAT_VERSION {
            return Err(format!(
                "unsupported catalog format {} (expected {})",
                self.format_version, CATALOG_FORMAT_VERSION
            ));
        }
        if self.workspaces.is_empty() {
            return Err("workspace catalog is empty".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for workspace in &self.workspaces {
            workspace.validate_envelope()?;
            if !ids.insert(&workspace.id) {
                return Err(format!("duplicate workspace id `{}`", workspace.id));
            }
        }
        if !ids.contains(&self.active_id) {
            return Err("active workspace does not exist".into());
        }
        Ok(())
    }

    pub fn active(&self) -> Option<&WorkspaceSnapshotV1> {
        self.workspaces.iter().find(|w| w.id == self.active_id)
    }

    pub fn replace(&mut self, snapshot: WorkspaceSnapshotV1) {
        if let Some(current) = self.workspaces.iter_mut().find(|w| w.id == snapshot.id) {
            *current = snapshot;
        } else {
            self.workspaces.push(snapshot);
        }
    }
}

#[derive(Debug)]
pub enum PersistEvent {
    Loaded(Result<Option<String>, String>),
    Saved { generation: u64 },
    SaveFailed { generation: u64, error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveTicket {
    pub generation: u64,
    /// Native atomic writes can confirm durability synchronously. Browser
    /// writes remain false until their IndexedDB transaction commits.
    pub durable: bool,
}

/// Tracks whether the latest catalog generation containing the current
/// signing identity has reached durable storage. In particular, a completion
/// for an older IndexedDB transaction must never authorize deletion of the
/// eframe fallback after an identity change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatalogDurability {
    pub durable_catalog_confirmed: bool,
    latest_requested_generation: u64,
    required_generation: u64,
}

impl CatalogDurability {
    pub fn restored_catalog(&mut self) {
        self.durable_catalog_confirmed = true;
        self.required_generation = 0;
    }

    pub fn require_durable_save(&mut self) {
        self.durable_catalog_confirmed = false;
        self.required_generation = self.latest_requested_generation.saturating_add(1);
    }

    pub fn requested(&mut self, ticket: SaveTicket) {
        if ticket.generation < self.latest_requested_generation {
            return;
        }
        self.latest_requested_generation = ticket.generation;
        // A new asynchronous write makes the newest catalog uncertain until
        // that exact transaction commits. Native writes report durability in
        // the ticket and can be confirmed immediately.
        self.durable_catalog_confirmed =
            ticket.durable && ticket.generation >= self.required_generation;
    }

    pub fn saved(&mut self, generation: u64) {
        if generation == self.latest_requested_generation && generation >= self.required_generation
        {
            self.durable_catalog_confirmed = true;
        }
    }

    pub fn failed(&mut self, generation: u64) {
        if generation == self.latest_requested_generation {
            self.durable_catalog_confirmed = false;
        }
    }

    pub fn is_latest(&self, generation: u64) -> bool {
        generation == self.latest_requested_generation
    }
}

// Native persistence is deliberately a single atomically replaced catalog.
// It is easy to back up and cannot expose a half-written active workspace.
#[cfg(not(target_arch = "wasm32"))]
pub struct Persistence {
    path: std::path::PathBuf,
    events: Vec<PersistEvent>,
    next_generation: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Persistence {
    pub fn new() -> Self {
        let path = native_catalog_path();
        let loaded = match std::fs::read_to_string(&path) {
            Ok(json) => Ok(Some(json)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        };
        Self {
            path,
            events: vec![PersistEvent::Loaded(loaded)],
            next_generation: 0,
        }
    }

    pub fn drain(&mut self) -> Vec<PersistEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn save(&mut self, json: String) -> SaveTicket {
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("workspace save generation exhausted");
        let generation = self.next_generation;
        match atomic_write(&self.path, json.as_bytes()) {
            Ok(()) => {
                self.events.push(PersistEvent::Saved { generation });
                SaveTicket {
                    generation,
                    durable: true,
                }
            }
            Err(error) => {
                self.events.push(PersistEvent::SaveFailed {
                    generation,
                    error: format!("cannot save {}: {error}", self.path.display()),
                });
                SaveTicket {
                    generation,
                    durable: false,
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn native_catalog_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("MANTIS_WORKSPACE_DIR") {
        return std::path::PathBuf::from(path).join("workspaces-v1.json");
    }
    #[cfg(target_os = "windows")]
    if let Some(path) = std::env::var_os("APPDATA") {
        return std::path::PathBuf::from(path)
            .join("MantisCAD")
            .join("workspaces-v1.json");
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(path)
            .join("Library/Application Support/MantisCAD")
            .join("workspaces-v1.json");
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return std::path::PathBuf::from(path)
            .join("mantis-cad")
            .join("workspaces-v1.json");
    }
    if let Some(path) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(path)
            .join(".local/share/mantis-cad")
            .join("workspaces-v1.json");
    }
    std::env::temp_dir()
        .join("mantis-cad")
        .join("workspaces-v1.json")
}

#[cfg(not(target_arch = "wasm32"))]
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "catalog path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    replace_atomic(&tmp, path)?;
    #[cfg(unix)]
    if let Err(error) = std::fs::File::open(parent).and_then(|dir| dir.sync_all()) {
        return Err(post_replace_durability_error(path, bytes, error));
    }
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), unix))]
fn post_replace_durability_error(
    path: &std::path::Path,
    expected: &[u8],
    sync_error: std::io::Error,
) -> std::io::Error {
    let verification = match std::fs::read(path) {
        Ok(actual) if actual == expected => {
            "replacement bytes were verified, but crash durability is uncertain".to_string()
        }
        Ok(_) => "replacement bytes do not match the catalog that was written".to_string(),
        Err(error) => format!("replacement could not be verified: {error}"),
    };
    std::io::Error::new(
        sync_error.kind(),
        format!("cannot sync catalog directory: {sync_error}; {verification}"),
    )
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "windows")))]
fn replace_atomic(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

/// `std::fs::rename` cannot replace an existing destination on Windows.
/// MoveFileExW with REPLACE_EXISTING keeps the temp-file + atomic-replace
/// contract used by the native workspace store.
#[cfg(target_os = "windows")]
fn replace_atomic(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that stay
    // alive for the call, and the flags are valid MoveFileExW flags.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
pub struct Persistence {
    db: std::rc::Rc<std::cell::RefCell<Option<web_sys::IdbDatabase>>>,
    pending: std::rc::Rc<std::cell::RefCell<Option<(u64, String)>>>,
    events: std::rc::Rc<std::cell::RefCell<Vec<PersistEvent>>>,
    next_generation: u64,
}

#[cfg(target_arch = "wasm32")]
impl Persistence {
    pub fn new() -> Self {
        use wasm_bindgen::{closure::Closure, JsCast as _};

        let db = std::rc::Rc::new(std::cell::RefCell::new(None));
        let pending = std::rc::Rc::new(std::cell::RefCell::new(None));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let persistence = Self {
            db: db.clone(),
            pending: pending.clone(),
            events: events.clone(),
            next_generation: 0,
        };

        let Some(factory) = web_sys::window().and_then(|w| w.indexed_db().ok().flatten()) else {
            events.borrow_mut().push(PersistEvent::Loaded(Err(
                "IndexedDB is unavailable; browser storage may be disabled".into(),
            )));
            return persistence;
        };
        let request = match factory.open_with_u32("mantis-cad", 1) {
            Ok(request) => request,
            Err(e) => {
                events.borrow_mut().push(PersistEvent::Loaded(Err(format!(
                    "cannot open IndexedDB: {e:?}"
                ))));
                return persistence;
            }
        };

        let upgrade_request = request.clone();
        let on_upgrade = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if let Ok(value) = upgrade_request.result() {
                if let Ok(database) = value.dyn_into::<web_sys::IdbDatabase>() {
                    if !database.object_store_names().contains("state") {
                        let _ = database.create_object_store("state");
                    }
                }
            }
        });
        request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
        on_upgrade.forget();

        let success_request = request.clone();
        let success_db = db.clone();
        let success_pending = pending.clone();
        let success_events = events.clone();
        let on_success = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let Ok(value) = success_request.result() else {
                success_events.borrow_mut().push(PersistEvent::Loaded(Err(
                    "IndexedDB returned no database".into(),
                )));
                return;
            };
            let Ok(database) = value.dyn_into::<web_sys::IdbDatabase>() else {
                success_events.borrow_mut().push(PersistEvent::Loaded(Err(
                    "IndexedDB returned an invalid database".into(),
                )));
                return;
            };
            *success_db.borrow_mut() = Some(database.clone());
            web_load(&database, success_events.clone());
            if let Some((generation, json)) = success_pending.borrow_mut().take() {
                web_save(&database, generation, json, success_events.clone());
            }
        });
        request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        on_success.forget();

        let error_events = events.clone();
        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            error_events
                .borrow_mut()
                .push(PersistEvent::Loaded(Err("opening IndexedDB failed".into())));
        });
        request.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
        persistence
    }

    pub fn drain(&mut self) -> Vec<PersistEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    pub fn save(&mut self, json: String) -> SaveTicket {
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("workspace save generation exhausted");
        let generation = self.next_generation;
        if let Some(database) = self.db.borrow().as_ref().cloned() {
            web_save(&database, generation, json, self.events.clone());
        } else {
            // Coalesce writes while the asynchronous open is in flight.
            *self.pending.borrow_mut() = Some((generation, json));
        }
        SaveTicket {
            generation,
            durable: false,
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn web_load(
    database: &web_sys::IdbDatabase,
    events: std::rc::Rc<std::cell::RefCell<Vec<PersistEvent>>>,
) {
    use wasm_bindgen::{closure::Closure, JsCast as _};
    let result = database
        .transaction_with_str("state")
        .and_then(|tx| tx.object_store("state"))
        .and_then(|store| store.get(&wasm_bindgen::JsValue::from_str("catalog")));
    let Ok(request) = result else {
        events.borrow_mut().push(PersistEvent::Loaded(Err(
            "cannot read IndexedDB catalog".into()
        )));
        return;
    };
    let success_request = request.clone();
    let success_events = events.clone();
    let on_success = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        let loaded = success_request
            .result()
            .map_err(|e| format!("cannot read IndexedDB result: {e:?}"))
            .and_then(|value| {
                if value.is_null() || value.is_undefined() {
                    Ok(None)
                } else {
                    value
                        .as_string()
                        .map(Some)
                        .ok_or_else(|| "IndexedDB catalog is not text".into())
                }
            });
        success_events
            .borrow_mut()
            .push(PersistEvent::Loaded(loaded));
    });
    request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
    on_success.forget();
    let error_events = events.clone();
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        error_events.borrow_mut().push(PersistEvent::Loaded(Err(
            "reading IndexedDB catalog failed".into(),
        )));
    });
    request.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();
}

#[cfg(target_arch = "wasm32")]
fn web_save(
    database: &web_sys::IdbDatabase,
    generation: u64,
    json: String,
    events: std::rc::Rc<std::cell::RefCell<Vec<PersistEvent>>>,
) {
    use wasm_bindgen::{closure::Closure, JsCast as _};
    let Ok(transaction) =
        database.transaction_with_str_and_mode("state", web_sys::IdbTransactionMode::Readwrite)
    else {
        events.borrow_mut().push(PersistEvent::SaveFailed {
            generation,
            error: "cannot start IndexedDB catalog transaction".into(),
        });
        return;
    };

    let terminal = std::rc::Rc::new(std::cell::Cell::new(false));
    let complete_terminal = terminal.clone();
    let complete_events = events.clone();
    let on_complete = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if !complete_terminal.replace(true) {
            complete_events
                .borrow_mut()
                .push(PersistEvent::Saved { generation });
        }
    });
    transaction.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
    on_complete.forget();

    let abort_terminal = terminal.clone();
    let abort_events = events.clone();
    let on_abort = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if !abort_terminal.replace(true) {
            abort_events.borrow_mut().push(PersistEvent::SaveFailed {
                generation,
                error: "IndexedDB catalog transaction was aborted; the legacy key fallback was preserved"
                    .into(),
            });
        }
    });
    transaction.set_onabort(Some(on_abort.as_ref().unchecked_ref()));
    on_abort.forget();

    let error_terminal = terminal.clone();
    let error_events = events.clone();
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if !error_terminal.replace(true) {
            error_events.borrow_mut().push(PersistEvent::SaveFailed {
                generation,
                error:
                    "IndexedDB catalog transaction failed; the legacy key fallback was preserved"
                        .into(),
            });
        }
    });
    transaction.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    let result = transaction.object_store("state").and_then(|store| {
        store.put_with_key(
            &wasm_bindgen::JsValue::from_str(&json),
            &wasm_bindgen::JsValue::from_str("catalog"),
        )
    });
    if result.is_err() && !terminal.replace(true) {
        events.borrow_mut().push(PersistEvent::SaveFailed {
            generation,
            error: "cannot write IndexedDB catalog; the legacy key fallback was preserved".into(),
        });
        let _ = transaction.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: &str) -> WorkspaceSnapshotV1 {
        WorkspaceSnapshotV1 {
            format_version: WORKSPACE_FORMAT_VERSION,
            id: id.into(),
            name: id.into(),
            updated_ms: 1,
            chain: Chain::new(),
            pending: Vec::new(),
            recovery_ops: Vec::new(),
            remote: None,
            view: ViewMetadataV1::default(),
        }
    }

    #[test]
    fn catalog_round_trip_and_validation() {
        let mut catalog = WorkspaceCatalogV1 {
            format_version: CATALOG_FORMAT_VERSION,
            active_id: "one".into(),
            settings: DeviceSettingsV1 {
                author_name: "alice".into(),
                secret_hex: "00".repeat(32),
                default_server_url: "".into(),
                background_check: false,
                key_backup_confirmed: false,
            },
            workspaces: vec![snapshot("one")],
        };
        catalog.workspaces[0].view.camera = CameraMetadataV1 {
            target: [1.25, -3.5, 8.0],
            distance: 42.0,
            yaw: -0.75,
            pitch: 0.3,
        };
        catalog.validate().unwrap();
        let json = serde_json::to_string(&catalog).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspaceCatalogV1>(&json).unwrap(),
            catalog
        );
    }

    #[test]
    fn catalog_rejects_duplicate_and_missing_active_ids() {
        let mut catalog = WorkspaceCatalogV1 {
            format_version: CATALOG_FORMAT_VERSION,
            active_id: "missing".into(),
            settings: DeviceSettingsV1 {
                author_name: "a".into(),
                secret_hex: "00".repeat(32),
                default_server_url: "".into(),
                background_check: false,
                key_backup_confirmed: false,
            },
            workspaces: vec![snapshot("one")],
        };
        assert!(catalog.validate().is_err());
        catalog.active_id = "one".into();
        catalog.workspaces.push(snapshot("one"));
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn catalog_durability_ignores_stale_async_completion() {
        let mut durability = CatalogDurability::default();
        durability.require_durable_save();
        durability.requested(SaveTicket {
            generation: 1,
            durable: false,
        });
        durability.requested(SaveTicket {
            generation: 2,
            durable: false,
        });

        durability.saved(1);
        assert!(!durability.durable_catalog_confirmed);
        durability.saved(2);
        assert!(durability.durable_catalog_confirmed);
    }

    #[test]
    fn identity_change_requires_a_new_generation() {
        let mut durability = CatalogDurability::default();
        durability.restored_catalog();
        durability.requested(SaveTicket {
            generation: 1,
            durable: false,
        });
        durability.saved(1);
        assert!(durability.durable_catalog_confirmed);

        durability.require_durable_save();
        durability.saved(1);
        assert!(!durability.durable_catalog_confirmed);
        durability.requested(SaveTicket {
            generation: 2,
            durable: false,
        });
        durability.saved(2);
        assert!(durability.durable_catalog_confirmed);
    }

    #[test]
    fn native_ticket_can_confirm_the_required_generation_immediately() {
        let mut durability = CatalogDurability::default();
        durability.require_durable_save();
        durability.requested(SaveTicket {
            generation: 1,
            durable: true,
        });
        assert!(durability.durable_catalog_confirmed);
    }

    #[test]
    fn portable_workspace_round_trip_keeps_wip_but_not_view_state() {
        let mut source = snapshot("one");
        source.view.viewed_block = Some(0);
        source.remote = Some(RemoteAnchorV1 {
            base_url: "https://cad.example".into(),
            project_id: "demo".into(),
            connection_confirmed: true,
            chain_format_version: 1,
            chain_id: None,
            genesis_hash: source.chain.blocks[0].hash.clone(),
            last_synced_len: source.chain.len(),
            last_synced_head: source.chain.head().hash.clone(),
            access: AccessState::ReadOnly,
        });
        let portable = source.to_portable().unwrap();
        let restored = WorkspaceSnapshotV1::from_portable(portable).unwrap();
        assert_eq!(restored.chain, source.chain);
        assert_eq!(restored.pending, source.pending);
        assert_eq!(restored.view.viewed_block, None);
        assert!(restored.view.show_chain);
        assert!(!restored.remote.unwrap().connection_confirmed);
    }

    #[test]
    fn same_origin_remote_stays_portable_across_subpath_deployments() {
        let mut source = snapshot("same-origin");
        source.remote = Some(RemoteAnchorV1 {
            // A compile-time deployment prefix such as `/mantis` is not an
            // origin and must not be baked into the catalog or portable file.
            base_url: String::new(),
            project_id: "demo".into(),
            connection_confirmed: true,
            chain_format_version: source.chain.format_version().unwrap(),
            chain_id: source.chain.chain_id().unwrap().map(str::to_owned),
            genesis_hash: source.chain.blocks[0].hash.clone(),
            last_synced_len: source.chain.len(),
            last_synced_head: source.chain.head().hash.clone(),
            access: AccessState::ReadOnly,
        });
        let catalog = WorkspaceCatalogV1 {
            format_version: CATALOG_FORMAT_VERSION,
            active_id: source.id.clone(),
            settings: DeviceSettingsV1 {
                author_name: "alice".into(),
                secret_hex: "00".repeat(32),
                default_server_url: String::new(),
                background_check: false,
                key_backup_confirmed: true,
            },
            workspaces: vec![source],
        };

        catalog.validate().unwrap();
        let decoded: WorkspaceCatalogV1 =
            serde_json::from_str(&serde_json::to_string(&catalog).unwrap()).unwrap();
        let portable = decoded.active().unwrap().to_portable().unwrap();
        assert_eq!(portable.remote.as_ref().unwrap().base_url, "");
        let restored = WorkspaceSnapshotV1::from_portable(portable).unwrap();
        assert_eq!(restored.remote.unwrap().base_url, "");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn atomic_write_replaces_an_existing_catalog() {
        let directory = std::env::temp_dir().join(format!(
            "mantis-workspace-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("workspaces-v1.json");
        atomic_write(&path, b"first").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_failure_is_not_reported_as_durable() {
        let directory = std::env::temp_dir().join(format!(
            "mantis-workspace-sync-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("workspaces-v1.json");
        std::fs::write(&path, b"verified").unwrap();
        let error = post_replace_durability_error(
            &path,
            b"verified",
            std::io::Error::other("injected sync failure"),
        );
        let message = error.to_string();
        assert!(message.contains("injected sync failure"));
        assert!(message.contains("crash durability is uncertain"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
