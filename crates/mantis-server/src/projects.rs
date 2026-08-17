//! Durable multi-project registry used by the `/api/v2` server.

use crate::rate_limit::WriteRateLimiter;
use crate::storage;
use mantis_chain::{Chain, ChainAudit, ChainError};
use mantis_graph::Graph;
use mantis_protocol::{
    AccessLedgerV1, AccessRecordV1, ApiInfoV2, BlocksPageV2, ChainStateV1, ErrorDetailV1,
    ErrorResponseV1, HashHex, ProjectBootstrapV1, ProjectInfoV2, ProjectManifestV1, ProjectSlug,
    ProjectSummaryV1, ProtocolError, PublicKeyHex, PushRequestV2, PushResponseV2,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub const DEFAULT_PAGE_LIMIT: usize = 256;
pub const MAX_PAGE_LIMIT: usize = 4096;
pub const MAX_PUSH_BLOCKS: usize = 256;
pub const MAX_PUSH_OPS: usize = 50_000;
pub const DEFAULT_MAX_PROJECT_BYTES: usize = 24 * 1024 * 1024;
/// A complete signed bootstrap must remain importable through the public
/// project-create endpoint. Runtime writes enforce this cap as well, so an
/// export can never grow into a document the server cannot restore atomically.
pub const MAX_PROJECT_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

static CREATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ProjectError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub context: Box<ProjectErrorContext>,
}

#[derive(Debug, Default)]
pub struct ProjectErrorContext {
    pub project: Option<ProjectSlug>,
    pub block: Option<u64>,
    pub op: Option<u64>,
    pub access_record: Option<u64>,
    pub state: Option<(u64, HashHex)>,
}

impl ProjectError {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            context: Box::default(),
        }
    }

    pub fn for_project(mut self, project: &ProjectSlug) -> Self {
        self.context.project = Some(project.clone());
        self
    }

    fn with_state(mut self, chain: &Chain) -> Self {
        self.context.state = chain_state_tuple(chain).ok();
        self
    }

    pub fn response(&self) -> ErrorResponseV1 {
        ErrorResponseV1 {
            error: ErrorDetailV1 {
                code: self.code.clone(),
                message: self.message.clone(),
                project: self.context.project.clone(),
                block: self.context.block,
                op: self.context.op,
                access_record: self.context.access_record,
            },
            len: self.context.state.as_ref().map(|state| state.0),
            head: self.context.state.as_ref().map(|state| state.1.clone()),
        }
    }
}

impl From<ChainError> for ProjectError {
    fn from(error: ChainError) -> Self {
        let status = if matches!(error, ChainError::Diverged { .. }) {
            409
        } else {
            422
        };
        Self {
            status,
            code: error.code().to_string(),
            message: error.to_string(),
            context: Box::new(ProjectErrorContext {
                block: error.block_index(),
                op: error
                    .operation_index()
                    .and_then(|value| u64::try_from(value).ok()),
                ..ProjectErrorContext::default()
            }),
        }
    }
}

fn protocol_error(error: ProtocolError, project: Option<&ProjectSlug>) -> ProjectError {
    let status = match &error {
        ProtocolError::UntrustedOperator | ProtocolError::AccessUnauthorized { .. } => 403,
        ProtocolError::AccessBadIndex { .. }
        | ProtocolError::AccessBadPrevHash { .. }
        | ProtocolError::AccessUnknownMember { .. }
        | ProtocolError::AccessLastOwner { .. }
        | ProtocolError::AccessNoop { .. } => 409,
        _ => 422,
    };
    let mut result = ProjectError::new(status, error.code(), error.to_string());
    result.context.access_record = error.access_record_index();
    if let ProtocolError::BootstrapUnauthorizedAuthor { block } = error {
        result.context.block = Some(block);
    }
    if let Some(project) = project {
        result.context.project = Some(project.clone());
    }
    result
}

#[derive(Debug, Serialize)]
pub struct AccessPageV1 {
    pub project_id: ProjectSlug,
    pub from: u64,
    pub records: Vec<AccessRecordV1>,
    pub next_from: Option<u64>,
    pub state: mantis_protocol::AccessStateV1,
}

struct ProjectPaths {
    root: PathBuf,
}

impl ProjectPaths {
    fn create(&self) -> PathBuf {
        self.root.join("project-create.json")
    }

    fn manifest(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    fn chain(&self) -> PathBuf {
        self.root.join("chain.json")
    }

    fn access(&self) -> PathBuf {
        self.root.join("access-log.json")
    }
}

struct ProjectState {
    create: mantis_protocol::ProjectCreateV1,
    manifest: ProjectManifestV1,
    chain: Chain,
    graph: Graph,
    audit: ChainAudit,
    chain_state: ChainStateV1,
    access: AccessLedgerV1,
    paths: ProjectPaths,
}

impl ProjectState {
    fn load(
        root: PathBuf,
        allowed_operators: &BTreeSet<PublicKeyHex>,
        max_project_bytes: usize,
    ) -> Result<Self, ProjectError> {
        let paths = ProjectPaths { root };
        let create: mantis_protocol::ProjectCreateV1 = storage::load_json(&paths.create())
            .map_err(|message| ProjectError::new(500, "invalid_project_create", message))?;
        let manifest: ProjectManifestV1 = storage::load_json(&paths.manifest())
            .map_err(|message| ProjectError::new(500, "invalid_project_manifest", message))?;
        let chain: Chain = storage::load_json(&paths.chain())
            .map_err(|message| ProjectError::new(500, "invalid_project_chain", message))?;
        let records: Vec<AccessRecordV1> = storage::load_json(&paths.access())
            .map_err(|message| ProjectError::new(500, "invalid_access_log", message))?;
        let bootstrap = ProjectBootstrapV1 {
            create: create.clone(),
            manifest: manifest.clone(),
            chain: chain.clone(),
            access_log: records,
        };
        bootstrap.verify(allowed_operators).map_err(|error| {
            ProjectError::new(500, "invalid_project", error.to_string())
                .for_project(&manifest.project_id)
        })?;
        validate_project_sizes(
            &bootstrap.create,
            &bootstrap.manifest,
            &bootstrap.chain,
            &bootstrap.access_log,
            max_project_bytes,
        )
        .map_err(|message| {
            ProjectError::new(500, "project_quota_exceeded", message)
                .for_project(&manifest.project_id)
        })?;
        let access = AccessLedgerV1::replay(&manifest, &bootstrap.access_log).map_err(|error| {
            ProjectError::new(500, "invalid_access_log", error.to_string())
                .for_project(&manifest.project_id)
        })?;
        let graph = chain.replay(None).map_err(|error| {
            ProjectError::new(500, "invalid_project_chain", error.to_string())
                .for_project(&manifest.project_id)
        })?;
        let audit = chain.audit().map_err(|error| {
            ProjectError::new(500, "invalid_project_chain", error.to_string())
                .for_project(&manifest.project_id)
        })?;
        let chain_state = chain_state_from_audit(&audit)?;
        Ok(Self {
            create,
            manifest,
            chain,
            graph,
            audit,
            chain_state,
            access,
            paths,
        })
    }

    fn from_bootstrap(root: PathBuf, bootstrap: ProjectBootstrapV1) -> Result<Self, ProjectError> {
        let access = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log)
            .map_err(|error| ProjectError::new(422, "invalid_access_log", error.to_string()))?;
        let graph = bootstrap.chain.replay(None).map_err(ProjectError::from)?;
        let audit = bootstrap.chain.audit().map_err(ProjectError::from)?;
        let chain_state = chain_state_from_audit(&audit)?;
        Ok(Self {
            create: bootstrap.create,
            manifest: bootstrap.manifest,
            chain: bootstrap.chain,
            graph,
            audit,
            chain_state,
            access,
            paths: ProjectPaths { root },
        })
    }

    fn state(&self) -> Result<ChainStateV1, ProjectError> {
        Ok(self.chain_state.clone())
    }

    fn info(&self) -> Result<ProjectInfoV2, ProjectError> {
        Ok(ProjectInfoV2 {
            manifest: self.manifest.clone(),
            state: self.state()?,
            access: self.access.state(),
        })
    }

    fn summary(&self) -> Result<ProjectSummaryV1, ProjectError> {
        Ok(ProjectSummaryV1 {
            project_id: self.manifest.project_id.clone(),
            title: self.access.effective_title().to_string(),
            archived: self.access.effective_archived(),
            chain_format_version: self.manifest.chain_format_version,
            chain_id: self.manifest.chain_id.clone(),
            genesis_hash: self.manifest.genesis_hash.clone(),
            state: self.state()?,
        })
    }

    fn matches_bootstrap_anchors(&self, bootstrap: &ProjectBootstrapV1) -> bool {
        self.create == bootstrap.create
            && self.manifest == bootstrap.manifest
            && self.chain.len() == bootstrap.chain.len()
            && bootstrap
                .chain
                .blocks
                .last()
                .map(|block| block.hash.as_str())
                == Some(self.chain.head().hash.as_str())
            && self.access.records().len() == bootstrap.access_log.len()
            && self.access.records().last().map(|record| &record.hash)
                == bootstrap.access_log.last().map(|record| &record.hash)
    }
}

type ProjectHandle = Arc<Mutex<ProjectState>>;

pub struct ProjectRegistry {
    data_dir: PathBuf,
    projects: RwLock<BTreeMap<ProjectSlug, ProjectHandle>>,
    allowed_operators: BTreeSet<PublicKeyHex>,
    limiter: Mutex<WriteRateLimiter>,
    commit_lock: Mutex<()>,
    ready: AtomicBool,
    mutations_allowed: AtomicBool,
    max_project_bytes: usize,
}

impl ProjectRegistry {
    pub fn open(
        data_dir: PathBuf,
        operator_keys: &[String],
        max_project_bytes: usize,
    ) -> Result<Self, ProjectError> {
        let allowed_operators = operator_keys
            .iter()
            .map(|value| PublicKeyHex::from_str(value))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(|error| ProjectError::new(400, "bad_operator_key", error.to_string()))?;
        let projects_dir = data_dir.join("projects");
        std::fs::create_dir_all(&projects_dir).map_err(|error| {
            ProjectError::new(
                500,
                "storage_unavailable",
                format!("cannot create {}: {error}", projects_dir.display()),
            )
        })?;

        let mut projects = BTreeMap::new();
        let entries = std::fs::read_dir(&projects_dir).map_err(|error| {
            ProjectError::new(
                500,
                "storage_unavailable",
                format!("cannot list {}: {error}", projects_dir.display()),
            )
        })?;
        let mut legacy_count = 0usize;
        for entry in entries {
            let entry = entry.map_err(|error| {
                ProjectError::new(500, "storage_unavailable", error.to_string())
            })?;
            let file_type = entry.file_type().map_err(|error| {
                ProjectError::new(500, "storage_unavailable", error.to_string())
            })?;
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                return Err(ProjectError::new(
                    500,
                    "invalid_project_directory",
                    "project directory name is not UTF-8",
                ));
            };
            if name.starts_with('.') {
                continue;
            }
            let slug = ProjectSlug::from_str(&name).map_err(|error| {
                ProjectError::new(500, "invalid_project_directory", error.to_string())
            })?;
            let state = ProjectState::load(entry.path(), &allowed_operators, max_project_bytes)?;
            if state.manifest.project_id != slug {
                return Err(ProjectError::new(
                    500,
                    "project_directory_mismatch",
                    format!("directory {slug} does not match manifest"),
                ));
            }
            if state.manifest.chain_id.is_none() {
                legacy_count += 1;
                if legacy_count > 1 {
                    return Err(ProjectError::new(
                        500,
                        "multiple_legacy_projects",
                        "at most one legacy v1 project may be loaded",
                    ));
                }
            }
            if projects
                .insert(slug.clone(), Arc::new(Mutex::new(state)))
                .is_some()
            {
                return Err(ProjectError::new(
                    500,
                    "duplicate_project",
                    format!("duplicate project {slug}"),
                ));
            }
        }
        ensure_unique_genesis(projects.values())?;

        let registry = Self {
            data_dir,
            projects: RwLock::new(projects),
            allowed_operators,
            limiter: Mutex::new(WriteRateLimiter::new(120, 20, 30, 10)),
            commit_lock: Mutex::new(()),
            ready: AtomicBool::new(true),
            mutations_allowed: AtomicBool::new(true),
            max_project_bytes,
        };
        registry.probe_storage()?;
        Ok(registry)
    }

    pub fn api_info(&self) -> ApiInfoV2 {
        let mut info = ApiInfoV2::new(
            env!("CARGO_PKG_VERSION"),
            option_env!("MANTIS_GIT_SHA").unwrap_or("unknown"),
        );
        info.capabilities.push("multi_project".into());
        info
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn summaries(&self, include_archived: bool) -> Result<Vec<ProjectSummaryV1>, ProjectError> {
        let map = self
            .projects
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let mut summaries = Vec::with_capacity(map.len());
        for handle in map.values() {
            let state = handle.lock().unwrap_or_else(|error| error.into_inner());
            let summary = state.summary()?;
            if include_archived || !summary.archived {
                summaries.push(summary);
            }
        }
        Ok(summaries)
    }

    pub fn info(&self, project: &ProjectSlug) -> Result<ProjectInfoV2, ProjectError> {
        let handle = self.project(project)?;
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        state.info()
    }

    pub fn create_proof(
        &self,
        project: &ProjectSlug,
    ) -> Result<mantis_protocol::ProjectCreateV1, ProjectError> {
        let handle = self.project(project)?;
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        Ok(state.create.clone())
    }

    pub fn audit(&self, project: &ProjectSlug) -> Result<ChainAudit, ProjectError> {
        let handle = self.project(project)?;
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        Ok(state.audit.clone())
    }

    pub fn blocks(
        &self,
        project: &ProjectSlug,
        from: usize,
        limit: usize,
    ) -> Result<BlocksPageV2, ProjectError> {
        let handle = self.project(project)?;
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        let start = from.min(state.chain.blocks.len());
        let end = start.saturating_add(limit).min(state.chain.blocks.len());
        Ok(BlocksPageV2 {
            project_id: project.clone(),
            from: u64::try_from(start).unwrap_or(u64::MAX),
            blocks: state.chain.blocks[start..end].to_vec(),
            next_from: (end < state.chain.blocks.len())
                .then(|| u64::try_from(end).unwrap_or(u64::MAX)),
            state: state.state()?,
        })
    }

    pub fn raw_blocks(
        &self,
        project: &ProjectSlug,
        from: usize,
        limit: Option<usize>,
    ) -> Result<(Vec<mantis_chain::Block>, ChainStateV1), ProjectError> {
        let handle = self.project(project)?;
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        let start = from.min(state.chain.blocks.len());
        let end = limit
            .map(|limit| start.saturating_add(limit).min(state.chain.blocks.len()))
            .unwrap_or(state.chain.blocks.len());
        Ok((state.chain.blocks[start..end].to_vec(), state.state()?))
    }

    pub fn access_records(
        &self,
        project: &ProjectSlug,
        from: usize,
        limit: usize,
    ) -> Result<AccessPageV1, ProjectError> {
        let handle = self.project(project)?;
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        let records = state.access.records();
        let start = from.min(records.len());
        let end = start.saturating_add(limit).min(records.len());
        Ok(AccessPageV1 {
            project_id: project.clone(),
            from: u64::try_from(start).unwrap_or(u64::MAX),
            records: records[start..end].to_vec(),
            next_from: (end < records.len()).then(|| u64::try_from(end).unwrap_or(u64::MAX)),
            state: state.access.state(),
        })
    }

    pub fn create(&self, bootstrap: ProjectBootstrapV1) -> Result<ProjectInfoV2, ProjectError> {
        let project = bootstrap.manifest.project_id.clone();
        self.ensure_mutations_allowed(Some(&project))?;
        // Existing slugs are resolved from compact signed/hash-chain anchors
        // before any attacker-controlled history is replayed. Returning the
        // already trusted state is safe even if the redundant request body is
        // otherwise malformed; a different commitment fails closed.
        {
            let map = self
                .projects
                .read()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(existing) = map.get(&project) {
                let state = existing.lock().unwrap_or_else(|error| error.into_inner());
                if state.matches_bootstrap_anchors(&bootstrap) {
                    return state.info();
                }
                return Err(ProjectError::new(
                    409,
                    "project_exists",
                    format!("project {project} already exists"),
                )
                .for_project(&project));
            }
        }
        bootstrap
            .verify(&self.allowed_operators)
            .map_err(|error| protocol_error(error, Some(&project)))?;
        validate_project_sizes(
            &bootstrap.create,
            &bootstrap.manifest,
            &bootstrap.chain,
            &bootstrap.access_log,
            self.max_project_bytes,
        )
        .map_err(|message| {
            ProjectError::new(413, "project_quota_exceeded", message).for_project(&project)
        })?;

        let mut map = self
            .projects
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_mutations_allowed(Some(&project))?;
        if let Some(existing) = map.get(&project) {
            let state = existing.lock().unwrap_or_else(|error| error.into_inner());
            if state.matches_bootstrap_anchors(&bootstrap) {
                return state.info();
            }
            return Err(ProjectError::new(
                409,
                "project_exists",
                format!("project {project} already exists"),
            )
            .for_project(&project));
        }
        if bootstrap.manifest.chain_id.is_none()
            && map.values().any(|handle| {
                handle
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .manifest
                    .chain_id
                    .is_none()
            })
        {
            return Err(ProjectError::new(
                409,
                "legacy_project_exists",
                "only one legacy v1 project is allowed",
            ));
        }
        let genesis = bootstrap.manifest.genesis_hash.clone();
        for handle in map.values() {
            let state = handle.lock().unwrap_or_else(|error| error.into_inner());
            if state.manifest.genesis_hash == genesis {
                return Err(ProjectError::new(
                    409,
                    "genesis_already_bound",
                    "this chain genesis is already bound to another project",
                ));
            }
        }

        let final_root = self.projects_dir().join(project.as_str());
        if final_root.exists() {
            // Recover a complete directory left after an ambiguous directory
            // fsync failure. Never accept a partial or different project.
            let recovered = ProjectState::load(
                final_root.clone(),
                &self.allowed_operators,
                self.max_project_bytes,
            )?;
            if !recovered.matches_bootstrap_anchors(&bootstrap) {
                self.ready.store(false, Ordering::Relaxed);
                return Err(ProjectError::new(
                    500,
                    "storage_conflict",
                    format!(
                        "project path exists outside the registry: {}",
                        final_root.display()
                    ),
                )
                .for_project(&project));
            }
            let info = recovered.info()?;
            // Do not take the global commit lock until after all existing
            // project guards have been released. Push/access hold a project
            // guard before this lock, so the opposite order would deadlock.
            let _commit = self
                .commit_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.ensure_mutations_allowed(Some(&project))?;
            map.insert(project, Arc::new(Mutex::new(recovered)));
            self.ready.store(true, Ordering::Relaxed);
            return Ok(info);
        }
        self.limit(bootstrap.create.operator_pk.as_str())?;
        let sequence = CREATE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp_root = self.projects_dir().join(format!(
            ".creating-{}-{}-{sequence}",
            project.as_str(),
            std::process::id()
        ));
        if tmp_root.exists() {
            return Err(ProjectError::new(
                500,
                "storage_conflict",
                format!("temporary project path exists: {}", tmp_root.display()),
            ));
        }
        std::fs::create_dir(&tmp_root)
            .map_err(|error| ProjectError::new(500, "persistence_failed", error.to_string()))?;
        let mut state = match ProjectState::from_bootstrap(tmp_root.clone(), bootstrap) {
            Ok(state) => state,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&tmp_root);
                return Err(error);
            }
        };
        let info = state.info()?;
        if let Err(error) = persist_complete(&state) {
            let _ = std::fs::remove_dir_all(&tmp_root);
            self.ready.store(false, Ordering::Relaxed);
            return Err(ProjectError::new(
                500,
                "persistence_failed",
                error.to_string(),
            ));
        }
        // Staging files are not visible as a project. Serialize only the
        // atomic publication with other mutations, after every existing
        // project mutex used by the uniqueness scans above has been dropped.
        let _commit = self
            .commit_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Err(error) = self.ensure_mutations_allowed(Some(&project)) {
            let _ = std::fs::remove_dir_all(&tmp_root);
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&tmp_root, &final_root) {
            let _ = std::fs::remove_dir_all(&tmp_root);
            self.ready.store(false, Ordering::Relaxed);
            return Err(ProjectError::new(
                500,
                "persistence_failed",
                error.to_string(),
            ));
        }
        if let Err(error) = storage::sync_directory(&self.projects_dir()) {
            let rollback = std::fs::remove_dir_all(&final_root)
                .and_then(|()| storage::sync_directory(&self.projects_dir()));
            return match rollback {
                Ok(()) => {
                    self.ready.store(false, Ordering::Relaxed);
                    Err(
                        ProjectError::new(500, "persistence_failed", error.to_string())
                            .for_project(&project),
                    )
                }
                Err(rollback) => {
                    self.latch_storage_uncertain();
                    let uncertain = ProjectError::new(
                        500,
                        "persistence_uncertain",
                        format!(
                            "project directory was published but its durability is uncertain: {error}; rollback also failed: {rollback}"
                        ),
                    )
                    .for_project(&project)
                    .with_state(&state.chain);
                    state.paths = ProjectPaths {
                        root: final_root.clone(),
                    };
                    if final_root.is_dir() {
                        map.insert(project.clone(), Arc::new(Mutex::new(state)));
                    }
                    Err(uncertain)
                }
            };
        }
        state.paths = ProjectPaths { root: final_root };
        map.insert(project, Arc::new(Mutex::new(state)));
        self.ready.store(true, Ordering::Relaxed);
        Ok(info)
    }

    pub fn push(
        &self,
        project: &ProjectSlug,
        request: PushRequestV2,
    ) -> Result<PushResponseV2, ProjectError> {
        self.ensure_mutations_allowed(Some(project))?;
        if request.blocks.is_empty() {
            return Err(
                ProjectError::new(400, "empty_push", "at least one new block is required")
                    .for_project(project),
            );
        }
        if request.blocks.len() > MAX_PUSH_BLOCKS {
            return Err(ProjectError::new(
                413,
                "too_many_blocks",
                format!("a push may contain at most {MAX_PUSH_BLOCKS} blocks"),
            )
            .for_project(project));
        }
        let op_count = request
            .blocks
            .iter()
            .try_fold(0usize, |total, block| total.checked_add(block.ops.len()))
            .ok_or_else(|| {
                ProjectError::new(413, "too_many_operations", "operation count overflow")
            })?;
        if op_count > MAX_PUSH_OPS {
            return Err(ProjectError::new(
                413,
                "too_many_operations",
                format!("a push may contain at most {MAX_PUSH_OPS} operations"),
            )
            .for_project(project));
        }
        let handle = self.project(project)?;
        let mut state = handle.lock().unwrap_or_else(|error| error.into_inner());
        self.ensure_mutations_allowed(Some(project))?;
        let current_len = u64::try_from(state.chain.len()).unwrap_or(u64::MAX);
        let current_head = HashHex::from_str(&state.chain.head().hash)
            .map_err(|error| ProjectError::new(500, "invalid_chain_head", error.to_string()))?;
        if request.base_len != current_len || request.base_head != current_head {
            return Err(ProjectError::new(
                409,
                "diverged",
                "push base does not match the current project head",
            )
            .for_project(project)
            .with_state(&state.chain));
        }
        if state.access.effective_archived() {
            return Err(
                ProjectError::new(403, "project_archived", "project is archived")
                    .for_project(project)
                    .with_state(&state.chain),
            );
        }
        if request.blocks.first().map(|block| block.index) != Some(request.base_len) {
            return Err(ProjectError::new(
                409,
                "push_index_mismatch",
                "the first pushed block must immediately follow the declared base",
            )
            .for_project(project)
            .with_state(&state.chain));
        }
        for (offset, block) in request.blocks.iter().enumerate() {
            let key = PublicKeyHex::from_str(&block.author_pk).map_err(|error| {
                ProjectError::new(422, "bad_key", error.to_string())
                    .for_project(project)
                    .with_state(&state.chain)
            })?;
            if !state.access.can_write(&key) {
                let mut error = ProjectError::new(
                    403,
                    "author_not_allowed",
                    format!("author {} is not allowed to write this project", key),
                )
                .for_project(project)
                .with_state(&state.chain);
                error.context.block = u64::try_from(offset)
                    .ok()
                    .map(|value| request.base_len + value);
                return Err(error);
            }
        }
        state
            .chain
            .verify_extension_crypto(&request.blocks)
            .map_err(|error| {
                ProjectError::from(error)
                    .for_project(project)
                    .with_state(&state.chain)
            })?;
        for key in request
            .blocks
            .iter()
            .map(|block| block.author_pk.as_str())
            .collect::<BTreeSet<_>>()
        {
            self.limit(key)?;
        }
        let mut candidate = state.chain.clone();
        let mut candidate_graph = state.graph.clone();
        let appended = candidate
            .try_extend_trusted(&mut candidate_graph, &request.blocks)
            .map_err(|error| {
                ProjectError::from(error)
                    .for_project(project)
                    .with_state(&state.chain)
            })?;
        validate_project_sizes(
            &state.create,
            &state.manifest,
            &candidate,
            state.access.records(),
            self.max_project_bytes,
        )
        .map_err(|message| {
            ProjectError::new(413, "project_quota_exceeded", message)
                .for_project(project)
                .with_state(&state.chain)
        })?;
        let candidate_audit = candidate.audit().map_err(|error| {
            ProjectError::from(error)
                .for_project(project)
                .with_state(&state.chain)
        })?;
        let candidate_state = chain_state_from_audit(&candidate_audit)?;
        let _commit = self
            .commit_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_mutations_allowed(Some(project))?;
        match storage::persist_json_existing_observed(&candidate, &state.paths.chain()) {
            Ok(()) => {}
            Err(storage::PersistFailure::NotPublished(error)) => {
                self.ready.store(false, Ordering::Relaxed);
                return Err(
                    ProjectError::new(500, "persistence_failed", error.to_string())
                        .for_project(project)
                        .with_state(&state.chain),
                );
            }
            Err(storage::PersistFailure::Published(error)) => {
                state.chain = candidate;
                state.graph = candidate_graph;
                state.audit = candidate_audit;
                state.chain_state = candidate_state;
                self.latch_storage_uncertain();
                return Err(ProjectError::new(
                    500,
                    "persistence_uncertain",
                    format!(
                        "candidate chain is visible but directory durability is uncertain: {error}"
                    ),
                )
                .for_project(project)
                .with_state(&state.chain));
            }
        }
        state.chain = candidate;
        state.graph = candidate_graph;
        state.audit = candidate_audit;
        state.chain_state = candidate_state;
        self.ready.store(true, Ordering::Relaxed);
        Ok(PushResponseV2 {
            len: state.chain_state.len,
            head: state.chain_state.head.clone(),
            appended: u64::try_from(appended).unwrap_or(u64::MAX),
        })
    }

    pub fn append_access(
        &self,
        project: &ProjectSlug,
        records: Vec<AccessRecordV1>,
    ) -> Result<mantis_protocol::AccessStateV1, ProjectError> {
        self.ensure_mutations_allowed(Some(project))?;
        if records.is_empty() {
            return Err(ProjectError::new(
                400,
                "empty_access_update",
                "at least one access record is required",
            ));
        }
        if records.len() > MAX_PUSH_BLOCKS {
            return Err(ProjectError::new(
                413,
                "too_many_access_records",
                format!("at most {MAX_PUSH_BLOCKS} access records may be appended"),
            ));
        }
        let handle = self.project(project)?;
        let mut state = handle.lock().unwrap_or_else(|error| error.into_inner());
        self.ensure_mutations_allowed(Some(project))?;
        state
            .access
            .verify_extension_crypto(&records)
            .map_err(|error| protocol_error(error, Some(project)))?;
        // Authorization and ACL semantics are also checked before consuming
        // a signer/global rate-limit token. An outsider can make a perfectly
        // valid signature for their own key, so cryptographic validity alone
        // is not enough to charge the shared bucket.
        let mut candidate = state.access.clone();
        candidate
            .try_extend(&records)
            .map_err(|error| protocol_error(error, Some(project)))?;
        for actor in records
            .iter()
            .map(|record| record.actor_pk.as_str())
            .collect::<BTreeSet<_>>()
        {
            self.limit(actor)?;
        }
        validate_project_sizes(
            &state.create,
            &state.manifest,
            &state.chain,
            candidate.records(),
            self.max_project_bytes,
        )
        .map_err(|message| {
            ProjectError::new(413, "project_quota_exceeded", message).for_project(project)
        })?;
        let _commit = self
            .commit_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure_mutations_allowed(Some(project))?;
        match storage::persist_json_existing_observed(candidate.records(), &state.paths.access()) {
            Ok(()) => {}
            Err(storage::PersistFailure::NotPublished(error)) => {
                self.ready.store(false, Ordering::Relaxed);
                return Err(
                    ProjectError::new(500, "persistence_failed", error.to_string())
                        .for_project(project)
                        .with_state(&state.chain),
                );
            }
            Err(storage::PersistFailure::Published(error)) => {
                let observed = candidate.state();
                state.access = candidate;
                self.latch_storage_uncertain();
                return Err(ProjectError::new(
                    500,
                    "persistence_uncertain",
                    format!(
                        "candidate access log len={} head={} is visible but directory durability is uncertain: {error}",
                        observed.len, observed.head
                    ),
                )
                .for_project(project)
                .with_state(&state.chain));
            }
        }
        state.access = candidate;
        self.ready.store(true, Ordering::Relaxed);
        Ok(state.access.state())
    }

    fn project(&self, project: &ProjectSlug) -> Result<ProjectHandle, ProjectError> {
        self.projects
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(project)
            .cloned()
            .ok_or_else(|| {
                ProjectError::new(
                    404,
                    "project_not_found",
                    format!("project {project} not found"),
                )
                .for_project(project)
            })
    }

    fn projects_dir(&self) -> PathBuf {
        self.data_dir.join("projects")
    }

    fn probe_storage(&self) -> Result<(), ProjectError> {
        let sequence = CREATE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = self.data_dir.join(format!(
            ".mantis-ready-probe-{}-{sequence}.json",
            std::process::id()
        ));
        storage::persist_json(&serde_json::json!({"ready": true}), &path).map_err(|error| {
            self.ready.store(false, Ordering::Relaxed);
            ProjectError::new(500, "storage_unavailable", error.to_string())
        })?;
        std::fs::remove_file(&path).map_err(|error| {
            self.ready.store(false, Ordering::Relaxed);
            ProjectError::new(500, "storage_unavailable", error.to_string())
        })?;
        storage::sync_directory(&self.data_dir).map_err(|error| {
            self.ready.store(false, Ordering::Relaxed);
            ProjectError::new(500, "storage_unavailable", error.to_string())
        })
    }

    fn limit(&self, public_key: &str) -> Result<(), ProjectError> {
        let mut limiter = self
            .limiter
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if limiter.allow(public_key) {
            Ok(())
        } else {
            Err(ProjectError::new(
                429,
                "rate_limited",
                "write rate limit exceeded; retry later",
            ))
        }
    }

    fn ensure_mutations_allowed(&self, project: Option<&ProjectSlug>) -> Result<(), ProjectError> {
        if self.mutations_allowed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut error = ProjectError::new(
            503,
            "storage_not_ready",
            "writes are disabled after an uncertain persistence outcome; restart and audit storage",
        );
        if let Some(project) = project {
            error.context.project = Some(project.clone());
        }
        Err(error)
    }

    fn latch_storage_uncertain(&self) {
        self.ready.store(false, Ordering::Release);
        self.mutations_allowed.store(false, Ordering::Release);
    }
}

fn chain_state_from_audit(audit: &ChainAudit) -> Result<ChainStateV1, ProjectError> {
    Ok(ChainStateV1 {
        len: u64::try_from(audit.block_count).unwrap_or(u64::MAX),
        head: HashHex::from_str(&audit.head_hash)
            .map_err(|error| ProjectError::new(500, "invalid_chain_head", error.to_string()))?,
        genesis: HashHex::from_str(&audit.genesis_hash)
            .map_err(|error| ProjectError::new(500, "invalid_genesis", error.to_string()))?,
        total_ops: u64::try_from(audit.operation_count).unwrap_or(u64::MAX),
    })
}

fn chain_state_tuple(chain: &Chain) -> Result<(u64, HashHex), ProjectError> {
    Ok((
        u64::try_from(chain.len()).unwrap_or(u64::MAX),
        HashHex::from_str(&chain.head().hash)
            .map_err(|error| ProjectError::new(500, "invalid_chain_head", error.to_string()))?,
    ))
}

#[derive(Serialize)]
struct ProjectDocumentRef<'a> {
    create: &'a mantis_protocol::ProjectCreateV1,
    manifest: &'a ProjectManifestV1,
    chain: &'a Chain,
    access_log: &'a [AccessRecordV1],
}

fn validate_project_sizes(
    create: &mantis_protocol::ProjectCreateV1,
    manifest: &ProjectManifestV1,
    chain: &Chain,
    access_log: &[AccessRecordV1],
    max_project_bytes: usize,
) -> Result<(), String> {
    validate_project_sizes_with_document_limit(
        create,
        manifest,
        chain,
        access_log,
        max_project_bytes,
        MAX_PROJECT_DOCUMENT_BYTES,
    )
}

fn validate_project_sizes_with_document_limit(
    create: &mantis_protocol::ProjectCreateV1,
    manifest: &ProjectManifestV1,
    chain: &Chain,
    access_log: &[AccessRecordV1],
    max_project_bytes: usize,
    max_document_bytes: usize,
) -> Result<(), String> {
    let chain_bytes = chain.byte_size();
    if chain_bytes > max_project_bytes {
        return Err(format!(
            "project chain is {chain_bytes} bytes; limit is {max_project_bytes} bytes"
        ));
    }
    let document_bytes = serde_json::to_vec(&ProjectDocumentRef {
        create,
        manifest,
        chain,
        access_log,
    })
    .map_err(|error| format!("cannot measure project document: {error}"))?
    .len();
    if document_bytes > max_document_bytes {
        return Err(format!(
            "complete project document is {document_bytes} bytes; import limit is {max_document_bytes} bytes"
        ));
    }
    Ok(())
}

fn persist_complete(state: &ProjectState) -> std::io::Result<()> {
    storage::persist_json_existing(&state.create, &state.paths.create())?;
    storage::persist_json_existing(&state.manifest, &state.paths.manifest())?;
    storage::persist_json_existing(&state.chain, &state.paths.chain())?;
    storage::persist_json_existing(state.access.records(), &state.paths.access())
}

fn ensure_unique_genesis<'a>(
    projects: impl Iterator<Item = &'a ProjectHandle>,
) -> Result<(), ProjectError> {
    let mut seen = BTreeSet::new();
    for handle in projects {
        let state = handle.lock().unwrap_or_else(|error| error.into_inner());
        if !seen.insert(state.manifest.genesis_hash.clone()) {
            return Err(ProjectError::new(
                500,
                "duplicate_genesis_binding",
                "two project manifests bind the same chain genesis",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_graph::{GraphOp, NodeId, ParamValue};
    use mantis_protocol::{AccessActionV1, ProjectRoleV1};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mantis-project-registry-test-{}-{n}",
            std::process::id()
        ))
    }

    #[test]
    fn empty_registry_is_ready_and_reports_v2() {
        let dir = temp_dir();
        let registry = ProjectRegistry::open(dir.clone(), &[], DEFAULT_MAX_PROJECT_BYTES).unwrap();
        assert!(registry.is_ready());
        assert!(registry.summaries(false).unwrap().is_empty());
        let api_info = registry.api_info();
        assert_eq!(api_info.api_version, 2);
        for required in ApiInfoV2::new("test", "test").capabilities {
            assert!(
                api_info.capabilities.contains(&required),
                "server omitted protocol capability {required}"
            );
        }
        assert!(api_info.capabilities.contains(&"multi_project".into()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn signed_bootstrap_survives_restart() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let chain_id = mantis_protocol::ChainId::from_str(&"12".repeat(32)).unwrap();
        let project = ProjectSlug::from_str("demo-project").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Demo Project",
            chain_id,
            PublicKeyHex::from_str(&owner.public_hex()).unwrap(),
            1_000,
            &operator,
        )
        .unwrap();
        let keys = vec![operator.public_hex()];
        let manifest = bootstrap.manifest.clone();
        let mut local_chain = bootstrap.chain.clone();
        let registry =
            ProjectRegistry::open(dir.clone(), &keys, DEFAULT_MAX_PROJECT_BYTES).unwrap();
        let info = registry.create(bootstrap).unwrap();
        assert_eq!(info.manifest.project_id, project);

        local_chain
            .append(
                vec![
                    GraphOp::AddNode {
                        id: NodeId(1),
                        type_name: "number_slider".into(),
                        pos: (0.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: NodeId(1),
                        key: "value".into(),
                        value: ParamValue::Number(2.0),
                    },
                ],
                "owner edit",
                &owner,
                2_000,
            )
            .unwrap();
        let pushed = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: manifest.genesis_hash.clone(),
                    blocks: local_chain.blocks[1..].to_vec(),
                },
            )
            .unwrap();
        assert_eq!(pushed.len, 2);
        drop(registry);

        let reopened =
            ProjectRegistry::open(dir.clone(), &keys, DEFAULT_MAX_PROJECT_BYTES).unwrap();
        assert_eq!(reopened.summaries(false).unwrap().len(), 1);
        assert_eq!(reopened.info(&project).unwrap().state.len, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn access_grant_controls_writes_and_archive_is_read_only() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let writer = Identity::generate("writer");
        let outsider = Identity::generate("outsider");
        let project = ProjectSlug::new("access-demo").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Access Demo",
            mantis_protocol::ChainId::new("34".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let manifest = bootstrap.manifest.clone();
        let mut writer_chain = bootstrap.chain.clone();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        registry.create(bootstrap).unwrap();

        let writer_pk = PublicKeyHex::new(writer.public_hex()).unwrap();
        let access = registry.info(&project).unwrap().access;
        let grant = AccessRecordV1::new_signed(
            access.len,
            &manifest,
            access.head,
            200,
            AccessActionV1::Grant {
                public_key: writer_pk,
                role: ProjectRoleV1::Writer,
                label: Some("Grasshopper agent".into()),
            },
            &owner,
        )
        .unwrap();
        let access = registry.append_access(&project, vec![grant]).unwrap();
        assert_eq!(access.members.len(), 2);

        writer_chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "writer edit",
                &writer,
                300,
            )
            .unwrap();
        registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: manifest.genesis_hash.clone(),
                    blocks: writer_chain.blocks[1..].to_vec(),
                },
            )
            .unwrap();

        let mut outsider_chain = writer_chain.clone();
        outsider_chain
            .append(
                vec![GraphOp::SetParam {
                    id: NodeId(1),
                    key: "value".into(),
                    value: ParamValue::Number(9.0),
                }],
                "outsider edit",
                &outsider,
                400,
            )
            .unwrap();
        let error = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 2,
                    base_head: HashHex::new(writer_chain.head().hash.clone()).unwrap(),
                    blocks: outsider_chain.blocks[2..].to_vec(),
                },
            )
            .unwrap_err();
        assert_eq!(
            (error.status, error.code.as_str()),
            (403, "author_not_allowed")
        );

        let access = registry.info(&project).unwrap().access;
        let archive = AccessRecordV1::new_signed(
            access.len,
            &manifest,
            access.head,
            500,
            AccessActionV1::Archive,
            &owner,
        )
        .unwrap();
        assert!(
            registry
                .append_access(&project, vec![archive])
                .unwrap()
                .archived
        );

        let mut archived_chain = writer_chain.clone();
        archived_chain
            .append(
                vec![GraphOp::SetParam {
                    id: NodeId(1),
                    key: "value".into(),
                    value: ParamValue::Number(3.0),
                }],
                "after archive",
                &writer,
                600,
            )
            .unwrap();
        let error = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 2,
                    base_head: HashHex::new(writer_chain.head().hash.clone()).unwrap(),
                    blocks: archived_chain.blocks[2..].to_vec(),
                },
            )
            .unwrap_err();
        assert_eq!(
            (error.status, error.code.as_str()),
            (403, "project_archived")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn create_is_exactly_idempotent_and_enforces_initial_quota() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("idempotent-demo").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Idempotent Demo",
            mantis_protocol::ChainId::new("45".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        assert!(validate_project_sizes_with_document_limit(
            &bootstrap.create,
            &bootstrap.manifest,
            &bootstrap.chain,
            &bootstrap.access_log,
            DEFAULT_MAX_PROJECT_BYTES,
            1,
        )
        .unwrap_err()
        .contains("complete project document"));
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        registry.create(bootstrap.clone()).unwrap();
        registry.create(bootstrap.clone()).unwrap();

        let mut different = bootstrap.clone();
        different.access_log.push(
            AccessRecordV1::new_signed(
                1,
                &different.manifest,
                different.access_log[0].hash.clone(),
                200,
                AccessActionV1::Rename {
                    title: "Unexpected Rename".into(),
                },
                &owner,
            )
            .unwrap(),
        );
        let error = registry.create(different).unwrap_err();
        assert_eq!((error.status, error.code.as_str()), (409, "project_exists"));
        let _ = std::fs::remove_dir_all(&dir);

        let quota_dir = temp_dir();
        let quota_registry =
            ProjectRegistry::open(quota_dir.clone(), &[operator.public_hex()], 1).unwrap();
        let error = quota_registry.create(bootstrap).unwrap_err();
        assert_eq!(
            (error.status, error.code.as_str()),
            (413, "project_quota_exceeded")
        );
        assert!(quota_registry.summaries(true).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(quota_dir);
    }

    #[test]
    fn push_requires_a_new_tail_and_never_recreates_a_deleted_project() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("durability-demo").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Durability Demo",
            mantis_protocol::ChainId::new("67".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let mut local_chain = bootstrap.chain.clone();
        let genesis = bootstrap.manifest.genesis_hash.clone();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        registry.create(bootstrap).unwrap();

        let empty = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: genesis.clone(),
                    blocks: vec![],
                },
            )
            .unwrap_err();
        assert_eq!((empty.status, empty.code.as_str()), (400, "empty_push"));

        local_chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "owner edit",
                &owner,
                200,
            )
            .unwrap();
        let mut wrong_index = local_chain.blocks[1].clone();
        wrong_index.index = 0;
        wrong_index.hash = wrong_index.compute_hash();
        wrong_index.sig = owner.sign_hash_hex(&wrong_index.hash);
        let mismatch = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: genesis.clone(),
                    blocks: vec![wrong_index],
                },
            )
            .unwrap_err();
        assert_eq!(
            (mismatch.status, mismatch.code.as_str()),
            (409, "push_index_mismatch")
        );

        let project_root = dir.join("projects").join(project.as_str());
        std::fs::remove_dir_all(&project_root).unwrap();
        let failure = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: genesis,
                    blocks: local_chain.blocks[1..].to_vec(),
                },
            )
            .unwrap_err();
        assert_eq!(
            (failure.status, failure.code.as_str()),
            (500, "persistence_failed")
        );
        assert_eq!(registry.info(&project).unwrap().state.len, 1);
        assert!(!project_root.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn access_state_conflicts_have_http_conflict_status() {
        for error in [
            ProtocolError::AccessUnknownMember { at: 1 },
            ProtocolError::AccessLastOwner { at: 1 },
            ProtocolError::AccessNoop { at: 1 },
        ] {
            let mapped = protocol_error(error, None);
            assert_eq!(mapped.status, 409);
            assert_eq!(mapped.context.access_record, Some(1));
        }
    }

    #[test]
    fn invalid_signed_access_updates_do_not_consume_trusted_writer_quota() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("quota-integrity").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Quota Integrity",
            mantis_protocol::ChainId::new("89".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let manifest = bootstrap.manifest.clone();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        registry.create(bootstrap).unwrap();

        let access = registry.info(&project).unwrap().access;
        let valid = AccessRecordV1::new_signed(
            access.len,
            &manifest,
            access.head.clone(),
            200,
            AccessActionV1::Rename {
                title: "Still Valid".into(),
            },
            &owner,
        )
        .unwrap();
        let mut invalid = valid.clone();
        invalid.hash = HashHex::zero();
        for _ in 0..25 {
            let error = registry
                .append_access(&project, vec![invalid.clone()])
                .unwrap_err();
            assert_eq!(error.code, "access_bad_hash");
        }
        for attempt in 0..25 {
            let outsider = Identity::generate(&format!("outsider-{attempt}"));
            let unauthorized = AccessRecordV1::new_signed(
                access.len,
                &manifest,
                access.head.clone(),
                300 + attempt,
                AccessActionV1::Rename {
                    title: format!("Unauthorized {attempt}"),
                },
                &outsider,
            )
            .unwrap();
            let error = registry
                .append_access(&project, vec![unauthorized])
                .unwrap_err();
            assert_eq!(error.code, "access_unauthorized");
        }
        assert_eq!(
            registry.append_access(&project, vec![valid]).unwrap().title,
            "Still Valid"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_create_signatures_do_not_exhaust_operator_quota() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("create-quota").unwrap(),
            "Create Quota",
            mantis_protocol::ChainId::new("9a".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        let mut invalid = bootstrap.clone();
        invalid.create.hash = HashHex::zero();
        for _ in 0..25 {
            let error = registry.create(invalid.clone()).unwrap_err();
            assert_eq!(error.code, "bad_create_hash");
        }
        registry.create(bootstrap).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn forged_signatures_do_not_exhaust_trusted_writer_quota() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("push-quota").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Push Quota",
            mantis_protocol::ChainId::new("bc".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let genesis = bootstrap.manifest.genesis_hash.clone();
        let mut local_chain = bootstrap.chain.clone();
        local_chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "valid owner edit",
                &owner,
                200,
            )
            .unwrap();
        let valid_block = local_chain.blocks[1].clone();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        registry.create(bootstrap).unwrap();

        for value in 1_u8..=25 {
            let mut forged = valid_block.clone();
            // Keep the currently authorized public key but forge proof of key
            // possession. These requests must fail before the signer/global
            // token buckets and before any trusted-prefix replay.
            forged.sig = format!("{value:02x}").repeat(64);
            let error = registry
                .push(
                    &project,
                    PushRequestV2 {
                        base_len: 1,
                        base_head: genesis.clone(),
                        blocks: vec![forged],
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, "bad_signature");
        }
        registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: genesis,
                    blocks: vec![valid_block],
                },
            )
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn audit_reads_return_the_cached_validated_checkpoint() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("cached-audit").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Cached Audit",
            mantis_protocol::ChainId::new("cd".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        registry.create(bootstrap).unwrap();
        let cached = registry.audit(&project).unwrap();
        let cached_state = registry.info(&project).unwrap().state;
        {
            let handle = registry.project(&project).unwrap();
            let mut state = handle.lock().unwrap_or_else(|error| error.into_inner());
            // Deliberate invariant violation: a fresh full audit would fail,
            // proving public audit reads only clone the startup/write cache.
            state.chain.blocks[0].hash = "ff".repeat(32);
            state.chain.blocks[0]
                .ops
                .push(GraphOp::RemoveNode { id: NodeId(999) });
        }
        assert_eq!(registry.audit(&project).unwrap(), cached);
        assert_eq!(registry.info(&project).unwrap().state, cached_state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn post_replace_chain_failure_publishes_candidate_and_fail_stops() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("uncertain-chain").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Uncertain Chain",
            mantis_protocol::ChainId::new("de".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let genesis = bootstrap.manifest.genesis_hash.clone();
        let keys = vec![operator.public_hex()];
        let mut local = bootstrap.chain.clone();
        local
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "first",
                &owner,
                200,
            )
            .unwrap();
        let registry =
            ProjectRegistry::open(dir.clone(), &keys, DEFAULT_MAX_PROJECT_BYTES).unwrap();
        registry.create(bootstrap).unwrap();
        let chain_path = dir
            .join("projects")
            .join(project.as_str())
            .join("chain.json");
        storage::fail_parent_sync_for_test(&chain_path);
        let error = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 1,
                    base_head: genesis,
                    blocks: local.blocks[1..].to_vec(),
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "persistence_uncertain");
        assert_eq!(error.context.state.as_ref().map(|state| state.0), Some(2));
        assert_eq!(registry.info(&project).unwrap().state.len, 2);
        assert_eq!(storage::load_json::<Chain>(&chain_path).unwrap().len(), 2);
        assert!(!registry.is_ready());

        let blocked = registry
            .push(
                &project,
                PushRequestV2 {
                    base_len: 2,
                    base_head: HashHex::new(local.head().hash.clone()).unwrap(),
                    blocks: vec![],
                },
            )
            .unwrap_err();
        assert_eq!(
            (blocked.status, blocked.code.as_str()),
            (503, "storage_not_ready")
        );
        drop(registry);
        let reopened =
            ProjectRegistry::open(dir.clone(), &keys, DEFAULT_MAX_PROJECT_BYTES).unwrap();
        assert_eq!(reopened.info(&project).unwrap().state.len, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn post_replace_access_failure_publishes_candidate_and_fail_stops() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("uncertain-access").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Uncertain Access",
            mantis_protocol::ChainId::new("ef".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let manifest = bootstrap.manifest.clone();
        let keys = vec![operator.public_hex()];
        let registry =
            ProjectRegistry::open(dir.clone(), &keys, DEFAULT_MAX_PROJECT_BYTES).unwrap();
        registry.create(bootstrap).unwrap();
        let access = registry.info(&project).unwrap().access;
        let rename = AccessRecordV1::new_signed(
            access.len,
            &manifest,
            access.head,
            200,
            AccessActionV1::Rename {
                title: "Published Candidate".into(),
            },
            &owner,
        )
        .unwrap();
        let access_path = dir
            .join("projects")
            .join(project.as_str())
            .join("access-log.json");
        storage::fail_parent_sync_for_test(&access_path);
        let error = registry
            .append_access(&project, vec![rename.clone()])
            .unwrap_err();
        assert_eq!(error.code, "persistence_uncertain");
        assert_eq!(
            registry.info(&project).unwrap().access.title,
            "Published Candidate"
        );
        assert_eq!(
            storage::load_json::<Vec<AccessRecordV1>>(&access_path)
                .unwrap()
                .len(),
            2
        );
        let blocked = registry.append_access(&project, vec![rename]).unwrap_err();
        assert_eq!(
            (blocked.status, blocked.code.as_str()),
            (503, "storage_not_ready")
        );
        drop(registry);
        let reopened =
            ProjectRegistry::open(dir.clone(), &keys, DEFAULT_MAX_PROJECT_BYTES).unwrap();
        assert_eq!(
            reopened.info(&project).unwrap().access.title,
            "Published Candidate"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ambiguous_create_rollback_latches_all_mutations() {
        let dir = temp_dir();
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let project = ProjectSlug::new("uncertain-create").unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            project.clone(),
            "Uncertain Create",
            mantis_protocol::ChainId::new("f0".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let registry = ProjectRegistry::open(
            dir.clone(),
            &[operator.public_hex()],
            DEFAULT_MAX_PROJECT_BYTES,
        )
        .unwrap();
        storage::fail_directory_sync_for_test(&dir.join("projects"), 2);
        let error = registry.create(bootstrap.clone()).unwrap_err();
        assert_eq!(error.code, "persistence_uncertain");
        assert!(!registry.is_ready());
        let blocked = registry.create(bootstrap).unwrap_err();
        assert_eq!(
            (blocked.status, blocked.code.as_str()),
            (503, "storage_not_ready")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
