//! Explicit, project-scoped synchronization over `/api/v2`.
//!
//! Network callbacks only enqueue typed events. The UI owns the state machine,
//! which keeps background checks read-only and makes Pull/Push explicit.

use crate::workspace::AccessState;
use mantis_chain::Block;
use mantis_protocol::{
    ApiInfoV2, BlocksPageV2, ErrorResponseV1, HashHex, ProjectInfoV2, ProjectRoleV1, ProjectSlug,
    ProjectSummaryV1, PushRequestV2, PushResponseV2, API_VERSION,
};
use std::sync::{Arc, Mutex};

#[cfg(target_arch = "wasm32")]
pub const DEFAULT_SERVER_URL: &str = "";
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:7878";

// The deployment path is deliberately separate from `DEFAULT_SERVER_URL`.
// An empty server URL remains the portable/catalog representation of a
// browser same-origin remote; the compile-time path is applied only while
// constructing requests.
#[cfg(target_arch = "wasm32")]
const CONFIGURED_WEB_BASE_PATH: &str = match option_env!("MANTIS_WEB_BASE_PATH") {
    Some(path) => path,
    None => "",
};
#[cfg(not(target_arch = "wasm32"))]
const CONFIGURED_WEB_BASE_PATH: &str = "";

const fn is_unreserved_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

const fn valid_web_base_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.is_empty() || (bytes.len() == 1 && bytes[0] == b'/') {
        return true;
    }
    if bytes[0] != b'/' || bytes[bytes.len() - 1] == b'/' {
        return false;
    }

    let mut segment_start = 1;
    let mut index = 1;
    while index <= bytes.len() {
        if index == bytes.len() || bytes[index] == b'/' {
            let segment_len = index - segment_start;
            if segment_len == 0
                || (segment_len == 1 && bytes[segment_start] == b'.')
                || (segment_len == 2
                    && bytes[segment_start] == b'.'
                    && bytes[segment_start + 1] == b'.')
            {
                return false;
            }
            segment_start = index + 1;
        } else if !is_unreserved_path_byte(bytes[index]) {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(target_arch = "wasm32")]
const _: () = assert!(
    valid_web_base_path(CONFIGURED_WEB_BASE_PATH),
    "MANTIS_WEB_BASE_PATH must be empty, `/`, or a canonical absolute path without a trailing slash"
);

fn normalized_web_base_path(path: &str) -> &str {
    if path == "/" {
        ""
    } else {
        path
    }
}

fn api_root_for(server_url: &str, web_base_path: &str) -> String {
    debug_assert!(valid_web_base_path(web_base_path));
    let server_url = server_url.trim().trim_end_matches('/');
    let deployment_path = if server_url.is_empty() {
        normalized_web_base_path(web_base_path)
    } else {
        ""
    };
    format!("{server_url}{deployment_path}/api/v2")
}

pub const DEFAULT_PROJECT_ID: &str = "default";

/// Keep these in lockstep with the public `/api/v2` server contract.
pub const MAX_PUSH_BLOCKS: usize = 256;
pub const MAX_PUSH_OPS: usize = 50_000;
pub const MAX_PUSH_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadRelation {
    Same,
    RemoteAhead,
    LocalAhead,
    Diverged,
    InvalidRemote,
}

/// Compare a remote CAS checkpoint with local history. In particular, equal
/// lengths are not considered equal unless their heads match.
pub fn compare_heads(local: &[Block], remote_len: usize, remote_head: &str) -> HeadRelation {
    if remote_len == 0 || local.is_empty() {
        return HeadRelation::InvalidRemote;
    }
    if remote_len > local.len() {
        return HeadRelation::RemoteAhead;
    }
    let matches = local
        .get(remote_len - 1)
        .is_some_and(|block| block.hash == remote_head);
    if !matches {
        HeadRelation::Diverged
    } else if remote_len == local.len() {
        HeadRelation::Same
    } else {
        HeadRelation::LocalAhead
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    pub api_version: u32,
    pub chain_format_version: u32,
    pub project_id: String,
    pub chain_id: Option<String>,
    pub genesis_hash: String,
    pub len: usize,
    pub head: String,
    pub access: AccessState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageState {
    pub len: usize,
    pub head: String,
    pub genesis: String,
}

#[derive(Debug)]
pub struct PushChunk {
    pub end_len: usize,
    pub block_count: usize,
    pub op_count: usize,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushProgress {
    target_len: usize,
    acknowledged_len: usize,
    acknowledged_head: String,
    total_appended: usize,
    inflight_end: Option<usize>,
}

impl PushProgress {
    pub fn new(local: &[Block], base_len: usize, base_head: &str) -> Result<Self, String> {
        if base_len == 0 || base_len > local.len() {
            return Err("push base length is outside local history".into());
        }
        if local[base_len - 1].hash != base_head {
            return Err("push base head is not a local history prefix".into());
        }
        Ok(Self {
            target_len: local.len(),
            acknowledged_len: base_len,
            acknowledged_head: base_head.to_owned(),
            total_appended: 0,
            inflight_end: None,
        })
    }

    pub fn target_len(&self) -> usize {
        self.target_len
    }

    pub fn acknowledged_len(&self) -> usize {
        self.acknowledged_len
    }

    pub fn acknowledged_head(&self) -> &str {
        &self.acknowledged_head
    }

    pub fn total_appended(&self) -> usize {
        self.total_appended
    }

    pub fn mark_inflight(&mut self, end_len: usize) -> Result<(), String> {
        if self.inflight_end.is_some() {
            return Err("a push chunk is already in flight".into());
        }
        if end_len <= self.acknowledged_len || end_len > self.target_len {
            return Err("push chunk end is outside the frozen local target".into());
        }
        self.inflight_end = Some(end_len);
        Ok(())
    }

    /// Validate one CAS response before advancing to the next chunk. A server
    /// may never move this state to a non-local head or acknowledge only part
    /// of an atomic chunk.
    pub fn acknowledge(
        &mut self,
        local: &[Block],
        len: usize,
        head: &str,
        appended: usize,
    ) -> Result<bool, String> {
        let expected_end = self
            .inflight_end
            .ok_or_else(|| "received a push response without an in-flight chunk".to_string())?;
        if len != expected_end {
            return Err(format!(
                "push response length {len} does not match chunk end {expected_end}"
            ));
        }
        let expected_head = local
            .get(len.saturating_sub(1))
            .ok_or_else(|| "push response length is outside local history".to_string())?;
        if expected_head.hash != head {
            return Err("push response head is not the expected local prefix".into());
        }
        let expected_appended = expected_end - self.acknowledged_len;
        if appended != expected_appended {
            return Err(format!(
                "push response appended {appended} block(s), expected {expected_appended}"
            ));
        }
        let total_appended = self
            .total_appended
            .checked_add(appended)
            .ok_or_else(|| "push progress counter overflow".to_string())?;
        self.acknowledged_len = len;
        self.acknowledged_head = head.to_owned();
        self.total_appended = total_appended;
        self.inflight_end = None;
        Ok(self.acknowledged_len == self.target_len)
    }
}

#[derive(Debug, Clone, Copy)]
struct PushLimits {
    blocks: usize,
    ops: usize,
    body_bytes: usize,
}

const PUSH_LIMITS: PushLimits = PushLimits {
    blocks: MAX_PUSH_BLOCKS,
    ops: MAX_PUSH_OPS,
    body_bytes: MAX_PUSH_BODY_BYTES,
};

pub fn build_push_chunk(
    local: &[Block],
    base_len: usize,
    base_head: &str,
    target_len: usize,
) -> Result<PushChunk, String> {
    build_push_chunk_with_limits(local, base_len, base_head, target_len, PUSH_LIMITS)
}

fn build_push_chunk_with_limits(
    local: &[Block],
    base_len: usize,
    base_head: &str,
    target_len: usize,
    limits: PushLimits,
) -> Result<PushChunk, String> {
    if base_len >= target_len || target_len > local.len() {
        return Err("push range is outside the frozen local target".into());
    }
    let base_len_wire =
        u64::try_from(base_len).map_err(|_| "local chain length does not fit the protocol")?;
    let base_head_wire = HashHex::new(base_head).map_err(|error| error.to_string())?;
    let available = &local[base_len..target_len];
    let mut candidate_count = 0usize;
    let mut op_count = 0usize;
    for block in available.iter().take(limits.blocks) {
        if block.ops.len() > limits.ops && candidate_count == 0 {
            return Err(format!(
                "local block #{} alone has {} operations; the server limit is {}",
                block.index,
                block.ops.len(),
                limits.ops
            ));
        }
        let Some(next_ops) = op_count.checked_add(block.ops.len()) else {
            return Err("push operation count overflow".into());
        };
        if next_ops > limits.ops {
            break;
        }
        candidate_count += 1;
        op_count = next_ops;
    }
    if candidate_count == 0 {
        return Err("no local block fits the server push limits".into());
    }

    // Serialized request size is monotonic for a prefix. Binary search keeps
    // exact JSON sizing bounded even for requests near 32 MiB.
    let serialize = |count: usize| -> Result<Vec<u8>, String> {
        serde_json::to_vec(&PushRequestV2 {
            base_len: base_len_wire,
            base_head: base_head_wire.clone(),
            blocks: available[..count].to_vec(),
        })
        .map_err(|error| format!("cannot serialize push request: {error}"))
    };
    let first = serialize(1)?;
    if first.len() >= limits.body_bytes {
        return Err(format!(
            "local block #{} alone creates a {} byte push; it must be smaller than {} bytes",
            available[0].index,
            first.len(),
            limits.body_bytes
        ));
    }
    let mut low = 1usize;
    let mut high = candidate_count;
    let mut best_count = 1usize;
    let mut best_body = first;
    while low <= high {
        let middle = low + (high - low) / 2;
        let body = serialize(middle)?;
        if body.len() < limits.body_bytes {
            best_count = middle;
            best_body = body;
            low = middle + 1;
        } else {
            high = middle.saturating_sub(1);
        }
    }
    let selected_ops = available[..best_count]
        .iter()
        .map(|block| block.ops.len())
        .sum();
    Ok(PushChunk {
        end_len: base_len + best_count,
        block_count: best_count,
        op_count: selected_ops,
        body: best_body,
    })
}

impl RemoteInfo {
    fn from_response(
        value: ProjectInfoV2,
        public_key: &str,
        api_version: u32,
    ) -> Result<Self, String> {
        if value.state.len == 0 {
            return Err("remote project chain is empty".into());
        }
        if value.manifest.genesis_hash != value.state.genesis {
            return Err("remote manifest and chain state disagree on genesis".into());
        }
        let len = usize::try_from(value.state.len)
            .map_err(|_| "remote chain length does not fit this client".to_string())?;
        let access = if value.access.archived {
            AccessState::ReadOnly
        } else {
            value
                .access
                .members
                .iter()
                .find(|member| member.public_key.as_str() == public_key)
                .map(|member| match member.role {
                    ProjectRoleV1::Owner => AccessState::Owner,
                    ProjectRoleV1::Writer => AccessState::Writer,
                })
                .unwrap_or(AccessState::ReadOnly)
        };
        Ok(Self {
            api_version,
            chain_format_version: value.manifest.chain_format_version,
            project_id: value.manifest.project_id.into_string(),
            chain_id: value.manifest.chain_id.map(|id| id.into_string()),
            genesis_hash: value.manifest.genesis_hash.into_string(),
            len,
            head: value.state.head.into_string(),
            access,
        })
    }
}

#[derive(Debug)]
pub enum SyncEvent {
    PreflightOk {
        api_version: u32,
        app_version: String,
        git_sha: String,
    },
    Projects(Vec<ProjectSummaryV1>),
    Info(RemoteInfo),
    Blocks {
        from: usize,
        blocks: Vec<Block>,
        next_from: Option<usize>,
        state: PageState,
    },
    PushOk {
        len: usize,
        head: String,
        appended: usize,
    },
    PushConflict {
        code: String,
        msg: String,
    },
    Failed {
        context: &'static str,
        status: Option<u16>,
        code: String,
        msg: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Idle,
    Projects,
    Check,
    Pull,
    Push { retried: bool },
}

pub struct SyncClient {
    pub url: String,
    pub project_id: String,
    /// Historical field name retained for stored settings; this only checks
    /// whether remote is ahead and never performs a background Pull.
    pub auto_pull: bool,
    pub flow: Flow,
    pub last_auto_pull: f64,
    pub last_info: Option<RemoteInfo>,
    pub public_key: String,
    pub local_chain_format: u32,
    pub server_api_version: u32,
    pub server_app_version: String,
    pub server_git_sha: String,
    inbox: Arc<Mutex<Vec<SyncEvent>>>,
}

impl SyncClient {
    pub fn new(url: String) -> SyncClient {
        SyncClient {
            url,
            project_id: DEFAULT_PROJECT_ID.into(),
            auto_pull: false,
            flow: Flow::Idle,
            last_auto_pull: f64::NEG_INFINITY,
            last_info: None,
            public_key: String::new(),
            local_chain_format: 1,
            server_api_version: 0,
            server_app_version: String::new(),
            server_git_sha: String::new(),
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn busy(&self) -> bool {
        self.flow != Flow::Idle
    }

    pub fn drain(&mut self) -> Vec<SyncEvent> {
        match self.inbox.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(_) => Vec::new(),
        }
    }

    fn base(&self) -> String {
        self.url.trim().trim_end_matches('/').to_string()
    }

    fn api_root(&self) -> String {
        api_root_for(&self.base(), CONFIGURED_WEB_BASE_PATH)
    }

    fn project_path(&self, suffix: &str) -> Result<String, String> {
        validate_project_id(&self.project_id)?;
        Ok(format!(
            "{}/projects/{}/{}",
            self.api_root(),
            self.project_id,
            suffix.trim_start_matches('/')
        ))
    }

    fn projects_path(&self) -> String {
        format!("{}/projects", self.api_root())
    }

    fn api_info_path(&self) -> String {
        format!("{}/info", self.api_root())
    }

    fn deliver(inbox: &Arc<Mutex<Vec<SyncEvent>>>, ctx: &egui::Context, event: SyncEvent) {
        if let Ok(mut events) = inbox.lock() {
            events.push(event);
        }
        ctx.request_repaint();
    }

    pub fn start_check(&mut self, ctx: &egui::Context) {
        if !self.busy() {
            self.flow = Flow::Check;
            self.fetch_api_info(ctx);
        }
    }

    pub fn start_projects(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.flow = Flow::Projects;
        self.fetch_api_info(ctx);
    }

    pub fn start_pull(&mut self, ctx: &egui::Context) {
        if !self.busy() {
            self.flow = Flow::Pull;
            self.fetch_api_info(ctx);
        }
    }

    pub fn start_push(&mut self, ctx: &egui::Context) {
        if !self.busy() {
            self.flow = Flow::Push { retried: false };
            self.fetch_api_info(ctx);
        }
    }

    /// Continue the flow only after `/api/v2/info` passed compatibility
    /// checks. The UI invokes this when it drains `PreflightOk`.
    pub fn continue_after_preflight(&self, ctx: &egui::Context) {
        match self.flow {
            Flow::Projects => self.fetch_projects(ctx),
            Flow::Check | Flow::Pull | Flow::Push { .. } => self.fetch_info(ctx),
            Flow::Idle => {}
        }
    }

    fn fetch_api_info(&self, ctx: &egui::Context) {
        let url = self.api_info_path();
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        let local_chain_format = self.local_chain_format;
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let event = match result {
                Ok(response) if response.ok => {
                    match serde_json::from_slice::<ApiInfoV2>(&response.bytes) {
                        Ok(info) => match validate_api_info(&info, local_chain_format) {
                            Ok(()) => SyncEvent::PreflightOk {
                                api_version: info.api_version,
                                app_version: info.app_version,
                                git_sha: info.git_sha,
                            },
                            Err(msg) => SyncEvent::Failed {
                                context: "preflight",
                                status: Some(response.status),
                                code: "incompatible_server".into(),
                                msg,
                            },
                        },
                        Err(error) => {
                            parse_failed("preflight", Some(response.status), &response.bytes, error)
                        }
                    }
                }
                Ok(response) => http_failed("preflight", response.status, &response.bytes),
                Err(error) => SyncEvent::Failed {
                    context: "preflight",
                    status: None,
                    code: "network_error".into(),
                    msg: error,
                },
            };
            Self::deliver(&inbox, &ctx, event);
        });
    }

    fn fetch_projects(&self, ctx: &egui::Context) {
        let url = self.projects_path();
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let event = match result {
                Ok(response) if response.ok => {
                    match serde_json::from_slice::<Vec<ProjectSummaryV1>>(&response.bytes) {
                        Ok(projects) => SyncEvent::Projects(projects),
                        Err(error) => {
                            parse_failed("projects", Some(response.status), &response.bytes, error)
                        }
                    }
                }
                Ok(response) => http_failed("projects", response.status, &response.bytes),
                Err(error) => SyncEvent::Failed {
                    context: "projects",
                    status: None,
                    code: "network_error".into(),
                    msg: error,
                },
            };
            Self::deliver(&inbox, &ctx, event);
        });
    }

    pub fn fetch_info(&self, ctx: &egui::Context) {
        let url = match self.project_path("info") {
            Ok(url) => url,
            Err(msg) => {
                Self::deliver(
                    &self.inbox,
                    ctx,
                    SyncEvent::Failed {
                        context: "info",
                        status: None,
                        code: "invalid_project_id".into(),
                        msg,
                    },
                );
                return;
            }
        };
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        let public_key = self.public_key.clone();
        let api_version = self.server_api_version;
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let event = match result {
                Ok(response) if response.ok => {
                    match serde_json::from_slice::<ProjectInfoV2>(&response.bytes) {
                        Ok(info) => match RemoteInfo::from_response(info, &public_key, api_version)
                        {
                            Ok(info) => SyncEvent::Info(info),
                            Err(msg) => SyncEvent::Failed {
                                context: "info",
                                status: Some(response.status),
                                code: "invalid_response".into(),
                                msg,
                            },
                        },
                        Err(e) => parse_failed("info", Some(response.status), &response.bytes, e),
                    }
                }
                Ok(response) => http_failed("info", response.status, &response.bytes),
                Err(e) => SyncEvent::Failed {
                    context: "info",
                    status: None,
                    code: "network_error".into(),
                    msg: e,
                },
            };
            Self::deliver(&inbox, &ctx, event);
        });
    }

    pub fn fetch_blocks(&self, from: usize, ctx: &egui::Context) {
        let url = match self.project_path(&format!("blocks?from={from}&limit=256")) {
            Ok(url) => url,
            Err(msg) => {
                Self::deliver(
                    &self.inbox,
                    ctx,
                    SyncEvent::Failed {
                        context: "blocks",
                        status: None,
                        code: "invalid_project_id".into(),
                        msg,
                    },
                );
                return;
            }
        };
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        let project_id = self.project_id.clone();
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let event = match result {
                Ok(response) if response.ok => {
                    match serde_json::from_slice::<BlocksPageV2>(&response.bytes) {
                        Ok(page) => match page_event(page, from, &project_id) {
                            Ok(event) => event,
                            Err(msg) => SyncEvent::Failed {
                                context: "blocks",
                                status: Some(response.status),
                                code: "invalid_response".into(),
                                msg,
                            },
                        },
                        Err(e) => parse_failed("blocks", Some(response.status), &response.bytes, e),
                    }
                }
                Ok(response) => http_failed("blocks", response.status, &response.bytes),
                Err(e) => SyncEvent::Failed {
                    context: "blocks",
                    status: None,
                    code: "network_error".into(),
                    msg: e,
                },
            };
            Self::deliver(&inbox, &ctx, event);
        });
    }

    pub fn post_push_body(&self, body: Vec<u8>, ctx: &egui::Context) {
        let url = match self.project_path("blocks") {
            Ok(url) => url,
            Err(msg) => {
                Self::deliver(
                    &self.inbox,
                    ctx,
                    SyncEvent::Failed {
                        context: "push",
                        status: None,
                        code: "invalid_project_id".into(),
                        msg,
                    },
                );
                return;
            }
        };
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        let mut request = ehttp::Request::post(url, body);
        request.headers.insert("Content-Type", "application/json");
        ehttp::fetch(request, move |result| {
            let event = match result {
                Ok(response) if response.ok => {
                    match serde_json::from_slice::<PushResponseV2>(&response.bytes) {
                        Ok(value) => {
                            match (usize::try_from(value.len), usize::try_from(value.appended)) {
                                (Ok(len), Ok(appended)) => SyncEvent::PushOk {
                                    len,
                                    head: value.head.into_string(),
                                    appended,
                                },
                                _ => SyncEvent::Failed {
                                    context: "push",
                                    status: Some(response.status),
                                    code: "invalid_response".into(),
                                    msg: "remote push counters do not fit this client".into(),
                                },
                            }
                        }
                        Err(e) => parse_failed("push", Some(response.status), &response.bytes, e),
                    }
                }
                Ok(response) if response.status == 409 => {
                    let (code, msg) = error_detail(&response.bytes)
                        .unwrap_or_else(|| ("stale_head".into(), "remote head changed".into()));
                    SyncEvent::PushConflict { code, msg }
                }
                Ok(response) => http_failed("push", response.status, &response.bytes),
                Err(e) => SyncEvent::Failed {
                    context: "push",
                    status: None,
                    code: "network_error".into(),
                    msg: e,
                },
            };
            Self::deliver(&inbox, &ctx, event);
        });
    }
}

fn validate_api_info(info: &ApiInfoV2, local_chain_format: u32) -> Result<(), String> {
    if info.api_version != API_VERSION {
        return Err(format!(
            "server API v{} is incompatible with client API v{API_VERSION}",
            info.api_version
        ));
    }
    if !info.supported_chain_formats.contains(&local_chain_format) {
        return Err(format!(
            "server does not support local chain format v{local_chain_format}"
        ));
    }
    for capability in ["public_read", "paginated_blocks", "compare_and_swap_push"] {
        if !info.capabilities.iter().any(|value| value == capability) {
            return Err(format!(
                "server is missing required capability `{capability}`"
            ));
        }
    }
    Ok(())
}

fn page_event(
    page: BlocksPageV2,
    expected_from: usize,
    expected_project: &str,
) -> Result<SyncEvent, String> {
    if page.project_id.as_str() != expected_project {
        return Err(format!(
            "remote returned blocks for project `{}`, expected `{expected_project}`",
            page.project_id
        ));
    }
    let from = usize::try_from(page.from)
        .map_err(|_| "remote block offset does not fit this client".to_string())?;
    if from != expected_from {
        return Err(format!(
            "remote returned block offset {from}, expected {expected_from}"
        ));
    }
    let next_from = page
        .next_from
        .map(usize::try_from)
        .transpose()
        .map_err(|_| "remote next-page offset does not fit this client".to_string())?;
    let len = usize::try_from(page.state.len)
        .map_err(|_| "remote page chain length does not fit this client".to_string())?;
    Ok(SyncEvent::Blocks {
        from,
        blocks: page.blocks,
        next_from,
        state: PageState {
            len,
            head: page.state.head.into_string(),
            genesis: page.state.genesis.into_string(),
        },
    })
}

pub fn next_page(
    current_len: usize,
    target_len: usize,
    advertised_next: Option<usize>,
) -> Result<Option<usize>, String> {
    if current_len > target_len {
        return Err("block page passed the frozen Pull target".into());
    }
    if current_len == target_len {
        return Ok(None);
    }
    match advertised_next {
        Some(next) if next == current_len => Ok(Some(next)),
        Some(next) => Err(format!(
            "non-contiguous next page {next}; expected {current_len}"
        )),
        None => Err(format!(
            "remote ended pagination at {current_len} before target {target_len}"
        )),
    }
}

fn validate_project_id(project: &str) -> Result<(), String> {
    ProjectSlug::new(project).map(|_| ()).map_err(|_| {
        "project id must be 3–63 lowercase letters, digits, or internal hyphens".into()
    })
}

fn error_detail(bytes: &[u8]) -> Option<(String, String)> {
    let envelope = serde_json::from_slice::<ErrorResponseV1>(bytes).ok()?;
    Some((envelope.error.code, envelope.error.message))
}

fn http_failed(context: &'static str, status: u16, bytes: &[u8]) -> SyncEvent {
    let (code, msg) =
        error_detail(bytes).unwrap_or_else(|| ("http_error".into(), format!("HTTP {status}")));
    SyncEvent::Failed {
        context,
        status: Some(status),
        code,
        msg,
    }
}

fn parse_failed(
    context: &'static str,
    status: Option<u16>,
    bytes: &[u8],
    parse_error: serde_json::Error,
) -> SyncEvent {
    if let Some((code, msg)) = error_detail(bytes) {
        SyncEvent::Failed {
            context,
            status,
            code,
            msg,
        }
    } else {
        SyncEvent::Failed {
            context,
            status,
            code: "invalid_response".into(),
            msg: parse_error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_graph::{GraphOp, NodeId};

    fn push_block(index: usize, op_count: usize, message_bytes: usize) -> Block {
        Block {
            index: index as u64,
            prev_hash: format!("{:064x}", index),
            timestamp_ms: index as u64,
            author: "test".into(),
            author_pk: "11".repeat(32),
            message: "x".repeat(message_bytes),
            ops: vec![
                GraphOp::RemoveNode {
                    id: NodeId(index as u128 + 1),
                };
                op_count
            ],
            hash: format!("{:064x}", index + 1),
            sig: "22".repeat(64),
        }
    }

    fn push_history(tail_blocks: usize) -> Vec<Block> {
        (0..=tail_blocks)
            .map(|index| push_block(index, usize::from(index > 0), 0))
            .collect()
    }

    #[test]
    fn info_response_parses_version_and_access() {
        use mantis_protocol::{
            AccessMemberV1, AccessStateV1, ChainId, ChainStateV1, ProjectManifestV1, ProjectRoleV1,
            ProjectSlug, PublicKeyHex, PROJECT_MANIFEST_VERSION,
        };
        let key = PublicKeyHex::new("11".repeat(32)).unwrap();
        let info = ProjectInfoV2 {
            manifest: ProjectManifestV1 {
                schema_version: PROJECT_MANIFEST_VERSION,
                project_id: ProjectSlug::new("demo").unwrap(),
                title: "Demo".into(),
                chain_id: Some(ChainId::new("22".repeat(32)).unwrap()),
                genesis_hash: HashHex::new("33".repeat(32)).unwrap(),
                chain_format_version: 2,
                created_at_ms: 1,
                created_by: key.clone(),
                initial_owner: key.clone(),
                archived: false,
            },
            state: ChainStateV1 {
                len: 4,
                head: HashHex::new("44".repeat(32)).unwrap(),
                genesis: HashHex::new("33".repeat(32)).unwrap(),
                total_ops: 3,
            },
            access: AccessStateV1 {
                len: 1,
                head: HashHex::new("55".repeat(32)).unwrap(),
                members: vec![AccessMemberV1 {
                    public_key: key.clone(),
                    role: ProjectRoleV1::Writer,
                    label: None,
                    updated_at_ms: 1,
                    updated_by: key.clone(),
                }],
                title: "Demo".into(),
                archived: false,
            },
        };
        let info = RemoteInfo::from_response(info, key.as_str(), API_VERSION).unwrap();
        assert_eq!(info.len, 4);
        assert_eq!(info.access, AccessState::Writer);
    }

    #[test]
    fn same_origin_and_native_urls_are_joined_without_double_slash() {
        let mut client = SyncClient::new(String::new());
        client.project_id = "demo".into();
        assert_eq!(
            client.project_path("info").unwrap(),
            "/api/v2/projects/demo/info"
        );
        client.url = "http://x:1/".into();
        assert_eq!(
            client.project_path("blocks").unwrap(),
            "http://x:1/api/v2/projects/demo/blocks"
        );
    }

    #[test]
    fn configured_web_base_path_only_prefixes_same_origin_api_urls() {
        assert!(valid_web_base_path(""));
        assert!(valid_web_base_path("/"));
        assert!(valid_web_base_path("/mantis"));
        assert!(valid_web_base_path("/team/mantis-v1"));
        assert_eq!(api_root_for("", ""), "/api/v2");
        assert_eq!(api_root_for("", "/"), "/api/v2");
        assert_eq!(api_root_for("", "/mantis"), "/mantis/api/v2");
        assert_eq!(
            api_root_for("https://cad.example/", "/mantis"),
            "https://cad.example/api/v2"
        );
    }

    #[test]
    fn configured_web_base_path_rejects_ambiguous_or_noncanonical_values() {
        for invalid in [
            "mantis",
            "/mantis/",
            "//mantis",
            "/mantis//team",
            "/./mantis",
            "/../mantis",
            "/mantis?x=1",
            "/mantis#fragment",
            "/mantis\\windows",
            "/mantis path",
        ] {
            assert!(!valid_web_base_path(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn project_id_is_safe_as_a_path_segment() {
        assert!(validate_project_id("demo-project").is_ok());
        assert!(validate_project_id("../etc").is_err());
        assert!(validate_project_id("UPPER").is_err());
        assert!(validate_project_id("abc-").is_err());
        assert!(validate_project_id("x").is_err());
    }

    #[test]
    fn structured_error_is_preserved() {
        let body = br#"{"error":{"code":"author_not_allowed","message":"read only"}}"#;
        assert_eq!(
            error_detail(body),
            Some(("author_not_allowed".into(), "read only".into()))
        );
    }

    #[test]
    fn equal_length_with_different_head_is_divergent() {
        let chain = mantis_chain::Chain::new();
        assert_eq!(
            compare_heads(&chain.blocks, chain.len(), &"ff".repeat(32)),
            HeadRelation::Diverged
        );
        assert_eq!(
            compare_heads(&chain.blocks, chain.len(), &chain.head().hash),
            HeadRelation::Same
        );
    }

    #[test]
    fn api_preflight_rejects_version_format_and_capability_mismatch() {
        let good = ApiInfoV2::new("0.1.0", "test");
        assert!(validate_api_info(&good, 2).is_ok());

        let mut wrong_api = good.clone();
        wrong_api.api_version = 99;
        assert!(validate_api_info(&wrong_api, 2).is_err());

        let mut wrong_format = good.clone();
        wrong_format.supported_chain_formats = vec![1];
        assert!(validate_api_info(&wrong_format, 2).is_err());

        let mut missing_capability = good;
        missing_capability
            .capabilities
            .retain(|value| value != "paginated_blocks");
        assert!(validate_api_info(&missing_capability, 2).is_err());
    }

    #[test]
    fn pagination_continues_across_multiple_256_block_pages() {
        assert_eq!(next_page(256, 600, Some(256)).unwrap(), Some(256));
        assert_eq!(next_page(512, 600, Some(512)).unwrap(), Some(512));
        assert_eq!(next_page(600, 600, None).unwrap(), None);
        assert!(next_page(512, 600, None).is_err());
        assert!(next_page(512, 600, Some(513)).is_err());
    }

    #[test]
    fn push_chunks_257_blocks_and_accumulates_acknowledgements() {
        let local = push_history(257);
        let mut progress = PushProgress::new(&local, 1, &local[0].hash).unwrap();

        let first = build_push_chunk(
            &local,
            progress.acknowledged_len(),
            progress.acknowledged_head(),
            progress.target_len(),
        )
        .unwrap();
        assert_eq!(first.block_count, 256);
        assert_eq!(first.end_len, 257);
        progress.mark_inflight(first.end_len).unwrap();
        assert!(!progress
            .acknowledge(
                &local,
                first.end_len,
                &local[first.end_len - 1].hash,
                first.block_count,
            )
            .unwrap());

        let second = build_push_chunk(
            &local,
            progress.acknowledged_len(),
            progress.acknowledged_head(),
            progress.target_len(),
        )
        .unwrap();
        assert_eq!(second.block_count, 1);
        progress.mark_inflight(second.end_len).unwrap();
        assert!(progress
            .acknowledge(
                &local,
                second.end_len,
                &local[second.end_len - 1].hash,
                second.block_count,
            )
            .unwrap());
        assert_eq!(progress.total_appended(), 257);
    }

    #[test]
    fn push_chunk_honors_operation_boundary_and_rejects_oversized_block() {
        let local = vec![
            push_block(0, 0, 0),
            push_block(1, MAX_PUSH_OPS - 1, 0),
            push_block(2, 1, 0),
            push_block(3, 1, 0),
        ];
        let chunk = build_push_chunk(&local, 1, &local[0].hash, local.len()).unwrap();
        assert_eq!(chunk.block_count, 2);
        assert_eq!(chunk.op_count, MAX_PUSH_OPS);

        let oversized = vec![push_block(0, 0, 0), push_block(1, MAX_PUSH_OPS + 1, 0)];
        let error =
            build_push_chunk(&oversized, 1, &oversized[0].hash, oversized.len()).unwrap_err();
        assert!(error.contains("alone has 50001 operations"), "{error}");
    }

    #[test]
    fn push_chunk_uses_strict_serialized_body_boundary() {
        let local = vec![
            push_block(0, 0, 0),
            push_block(1, 1, 40),
            push_block(2, 1, 40),
        ];
        let unlimited = PushLimits {
            blocks: MAX_PUSH_BLOCKS,
            ops: MAX_PUSH_OPS,
            body_bytes: usize::MAX,
        };
        let both = build_push_chunk_with_limits(&local, 1, &local[0].hash, local.len(), unlimited)
            .unwrap();
        let one = build_push_chunk_with_limits(&local, 1, &local[0].hash, 2, unlimited).unwrap();
        assert!(one.body.len() < both.body.len());

        let only_one_fits = PushLimits {
            body_bytes: both.body.len(),
            ..unlimited
        };
        let chunk =
            build_push_chunk_with_limits(&local, 1, &local[0].hash, local.len(), only_one_fits)
                .unwrap();
        assert_eq!(chunk.block_count, 1);

        let first_is_exactly_the_limit = PushLimits {
            body_bytes: one.body.len(),
            ..unlimited
        };
        let error =
            build_push_chunk_with_limits(&local, 1, &local[0].hash, 2, first_is_exactly_the_limit)
                .unwrap_err();
        assert!(error.contains("alone creates"), "{error}");
    }

    #[test]
    fn push_progress_rejects_intermediate_non_local_head() {
        let local = push_history(257);
        let mut progress = PushProgress::new(&local, 1, &local[0].hash).unwrap();
        let chunk = build_push_chunk(&local, 1, &local[0].hash, local.len()).unwrap();
        progress.mark_inflight(chunk.end_len).unwrap();
        let error = progress
            .acknowledge(&local, chunk.end_len, &"ff".repeat(32), chunk.block_count)
            .unwrap_err();
        assert!(error.contains("expected local prefix"), "{error}");
        assert_eq!(progress.acknowledged_len(), 1);
    }

    #[test]
    fn replacing_sync_client_isolates_stale_callback_queue() {
        let stale = SyncClient::new("http://old".into());
        stale.inbox.lock().unwrap().push(SyncEvent::PushOk {
            len: 2,
            head: "11".repeat(32),
            appended: 1,
        });
        let mut replacement = SyncClient::new("http://new".into());
        assert!(replacement.drain().is_empty());
        assert_eq!(stale.inbox.lock().unwrap().len(), 1);
    }
}
