//! Chain sync over the mantis-server HTTP API via `ehttp` (works on native
//! and wasm). Responses are pushed into a shared inbox drained by the UI
//! thread each frame — no blocking, no async runtime.
//!
//! Flows (all driven step-by-step from the UI thread):
//! * Pull: GET /api/info → if server is ahead, GET /api/blocks?from=ours →
//!   `Document::merge_remote`.
//! * Push: GET /api/info → pull first if the server is ahead → POST our tail
//!   blocks → on 409 (head moved) pull and retry once.

use mantis_chain::Block;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// Default server address (matches mantis-server's default port).
pub const DEFAULT_SERVER_URL: &str = "http://localhost:7878";

#[derive(Debug, Deserialize)]
struct InfoResponse {
    len: usize,
    #[allow(dead_code)]
    #[serde(default)]
    head: String,
}

#[derive(Debug, Deserialize)]
struct LenResponse {
    #[serde(default)]
    len: usize,
}

/// A completed network step, produced by an ehttp callback.
#[derive(Debug)]
pub enum SyncEvent {
    /// GET /api/info result.
    Info { len: usize },
    /// GET /api/blocks?from=N result.
    Blocks { from: usize, blocks: Vec<Block> },
    /// POST /api/blocks accepted.
    PushOk { len: usize },
    /// POST /api/blocks rejected with 409 (server head moved).
    PushConflict,
    /// Any request/parse failure.
    Failed { context: &'static str, msg: String },
}

/// What multi-step flow is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Idle,
    Pull,
    /// `retried` is set after one 409-triggered pull+retry.
    Push { retried: bool },
}

/// Client-side sync state machine. All HTTP is fire-and-forget; results
/// arrive through `drain()` on subsequent frames.
pub struct SyncClient {
    pub url: String,
    pub auto_pull: bool,
    pub flow: Flow,
    /// Time (egui `input.time`) of the last auto-pull kick.
    pub last_auto_pull: f64,
    inbox: Arc<Mutex<Vec<SyncEvent>>>,
}

impl SyncClient {
    pub fn new(url: String) -> SyncClient {
        SyncClient {
            url,
            auto_pull: false,
            flow: Flow::Idle,
            last_auto_pull: f64::NEG_INFINITY,
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn busy(&self) -> bool {
        self.flow != Flow::Idle
    }

    /// Take everything the network callbacks delivered since last frame.
    pub fn drain(&mut self) -> Vec<SyncEvent> {
        match self.inbox.lock() {
            Ok(mut v) => std::mem::take(&mut *v),
            Err(_) => Vec::new(),
        }
    }

    fn base(&self) -> String {
        self.url.trim().trim_end_matches('/').to_string()
    }

    fn deliver(inbox: &Arc<Mutex<Vec<SyncEvent>>>, ctx: &egui::Context, ev: SyncEvent) {
        if let Ok(mut v) = inbox.lock() {
            v.push(ev);
        }
        ctx.request_repaint();
    }

    /// Kick off a pull flow (no-op while another flow runs).
    pub fn start_pull(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.flow = Flow::Pull;
        self.fetch_info(ctx);
    }

    /// Kick off a push flow (no-op while another flow runs).
    pub fn start_push(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.flow = Flow::Push { retried: false };
        self.fetch_info(ctx);
    }

    /// GET /api/info (used by both flows; the flow field disambiguates).
    pub fn fetch_info(&self, ctx: &egui::Context) {
        let url = format!("{}/api/info", self.base());
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let ev = match result {
                Ok(resp) if resp.ok => match serde_json::from_slice::<InfoResponse>(&resp.bytes) {
                    Ok(info) => SyncEvent::Info { len: info.len },
                    Err(e) => SyncEvent::Failed { context: "info", msg: e.to_string() },
                },
                Ok(resp) => SyncEvent::Failed {
                    context: "info",
                    msg: format!("HTTP {}", resp.status),
                },
                Err(e) => SyncEvent::Failed { context: "info", msg: e },
            };
            Self::deliver(&inbox, &ctx, ev);
        });
    }

    /// GET /api/blocks?from=N.
    pub fn fetch_blocks(&self, from: usize, ctx: &egui::Context) {
        let url = format!("{}/api/blocks?from={from}", self.base());
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let ev = match result {
                Ok(resp) if resp.ok => {
                    match serde_json::from_slice::<Vec<Block>>(&resp.bytes) {
                        Ok(blocks) => SyncEvent::Blocks { from, blocks },
                        Err(e) => SyncEvent::Failed { context: "blocks", msg: e.to_string() },
                    }
                }
                Ok(resp) => SyncEvent::Failed {
                    context: "blocks",
                    msg: format!("HTTP {}", resp.status),
                },
                Err(e) => SyncEvent::Failed { context: "blocks", msg: e },
            };
            Self::deliver(&inbox, &ctx, ev);
        });
    }

    /// POST /api/blocks with our tail blocks.
    pub fn post_blocks(&self, blocks: &[Block], ctx: &egui::Context) {
        let url = format!("{}/api/blocks", self.base());
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        let body = match serde_json::to_vec(blocks) {
            Ok(b) => b,
            Err(e) => {
                Self::deliver(
                    &inbox,
                    &ctx,
                    SyncEvent::Failed { context: "push", msg: e.to_string() },
                );
                return;
            }
        };
        let mut request = ehttp::Request::post(url, body);
        request
            .headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        ehttp::fetch(request, move |result| {
            let ev = match result {
                Ok(resp) if resp.ok => {
                    let len = serde_json::from_slice::<LenResponse>(&resp.bytes)
                        .map(|l| l.len)
                        .unwrap_or(0);
                    SyncEvent::PushOk { len }
                }
                Ok(resp) if resp.status == 409 => SyncEvent::PushConflict,
                Ok(resp) => SyncEvent::Failed {
                    context: "push",
                    msg: format!("HTTP {}", resp.status),
                },
                Err(e) => SyncEvent::Failed { context: "push", msg: e },
            };
            Self::deliver(&inbox, &ctx, ev);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_response_parses() {
        let info: InfoResponse = serde_json::from_str(r#"{"len":4,"head":"abcd"}"#).unwrap();
        assert_eq!(info.len, 4);
        let len: LenResponse = serde_json::from_str(r#"{"len":7}"#).unwrap();
        assert_eq!(len.len, 7);
    }

    #[test]
    fn base_url_trims_trailing_slash() {
        let c = SyncClient::new("http://x:1/".into());
        assert_eq!(c.base(), "http://x:1");
        let c = SyncClient::new("  http://x:1  ".into());
        assert_eq!(c.base(), "http://x:1");
    }

    #[test]
    fn drain_empties_inbox() {
        let mut c = SyncClient::new(DEFAULT_SERVER_URL.into());
        c.inbox.lock().unwrap().push(SyncEvent::Info { len: 3 });
        let evs = c.drain();
        assert_eq!(evs.len(), 1);
        assert!(c.drain().is_empty());
    }
}
