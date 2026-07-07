//! mantis-server — chain sync API + static hosting of the wasm build.
//!
//! Single-threaded tiny_http accept loop; no async. State is a
//! `Mutex<Chain>` persisted (write-tmp-then-rename) after every accepted
//! extension.
//!
//! Routes (see ARCHITECTURE.md):
//!   GET  /api/info          -> {"len":N,"head":"<hex>"}
//!   GET  /api/blocks?from=N -> JSON array of blocks N..end (bad/missing from -> 0)
//!   POST /api/blocks        -> body: JSON array of blocks; 200 {"len","appended"},
//!                              409 {"len","head"} on divergence/validation error,
//!                              400 on garbage
//!   OPTIONS *               -> 204 + CORS preflight headers
//!   GET  /<path>            -> static files under --dist (path-traversal safe),
//!                              "/" -> index.html; 404 otherwise
//!
//! Every response carries `Access-Control-Allow-Origin: *`.

use mantis_chain::Chain;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// Upper bound on accepted POST bodies (the op-log stays tiny; anything
/// bigger than this is abuse, not a chain).
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

// ---------------------------------------------------------------------------
// configuration / args
// ---------------------------------------------------------------------------

/// Parsed command line.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    port: u16,
    chain_path: PathBuf,
    dist: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 7878,
            chain_path: PathBuf::from("mantis-chain.json"),
            dist: None,
        }
    }
}

/// Hand-rolled `--port N --chain PATH --dist PATH` parsing.
fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<Config, String> {
    let mut cfg = Config::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let v = args.next().ok_or("--port needs a value")?;
                cfg.port = v
                    .parse::<u16>()
                    .map_err(|_| format!("invalid port: {v}"))?;
            }
            "--chain" => {
                let v = args.next().ok_or("--chain needs a value")?;
                cfg.chain_path = PathBuf::from(v);
            }
            "--dist" => {
                let v = args.next().ok_or("--dist needs a value")?;
                cfg.dist = Some(PathBuf::from(v));
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    Ok(cfg)
}

const USAGE: &str = "usage: mantis-server [--port N] [--chain PATH] [--dist DIR]
  --port N      listen port (default 7878)
  --chain PATH  chain JSON file (default mantis-chain.json)
  --dist DIR    serve static files from DIR (wasm app)";

// ---------------------------------------------------------------------------
// response helpers
// ---------------------------------------------------------------------------

/// Build a header from static-ish strings; `None` only on malformed input,
/// which never happens for the literals used here.
fn hdr(key: &str, value: &str) -> Option<Header> {
    Header::from_bytes(key.as_bytes(), value.as_bytes()).ok()
}

/// Attach CORS header (every response gets one).
fn with_cors<R: Read>(mut resp: Response<R>) -> Response<R> {
    if let Some(h) = hdr("Access-Control-Allow-Origin", "*") {
        resp = resp.with_header(h);
    }
    resp
}

/// JSON response with status + CORS.
fn json_response(status: u16, body: String) -> Response<Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(body).with_status_code(StatusCode(status));
    if let Some(h) = hdr("Content-Type", "application/json") {
        resp = resp.with_header(h);
    }
    with_cors(resp)
}

/// `{"error":"..."}` with proper JSON escaping.
fn error_json(status: u16, msg: &str) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_string(&serde_json::json!({ "error": msg }))
        .unwrap_or_else(|_| "{\"error\":\"internal\"}".to_string());
    json_response(status, body)
}

/// `{"len":N,"head":"<hex>"}` for the current chain state.
fn info_json(chain: &Chain) -> String {
    serde_json::to_string(&serde_json::json!({
        "len": chain.len(),
        "head": chain.head().hash,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// persistence
// ---------------------------------------------------------------------------

/// Atomic-ish persist: write `<path>.tmp`, then rename over `path`.
fn persist(chain: &Chain, path: &Path) -> std::io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    std::fs::write(&tmp, chain.to_json())?;
    std::fs::rename(&tmp, path)
}

/// Load the chain from `path` if it exists (validating), else a fresh chain.
/// A present-but-invalid file is an error — never silently clobber history.
fn load_chain(path: &Path) -> Result<Chain, String> {
    if !path.exists() {
        return Ok(Chain::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Chain::from_json(&text).map_err(|e| format!("invalid chain in {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// static files
// ---------------------------------------------------------------------------

/// Content-Type by file extension.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Percent-decode a URL path. Returns None on malformed escapes or invalid
/// UTF-8 (both rejected with 400 by the caller).
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)? as char;
            let lo = *bytes.get(i + 2)? as char;
            let byte = (hi.to_digit(16)? as u8) << 4 | lo.to_digit(16)? as u8;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Serve a file below `dist`, traversal-safe. `url_path` is the raw request
/// path (query already stripped).
fn serve_static(dist: &Path, url_path: &str) -> Response<Cursor<Vec<u8>>> {
    // Reject traversal *before and after* decoding: "%2e%2e", "..%2f" etc.
    if url_path.contains("..") {
        return error_json(400, "path traversal rejected");
    }
    let Some(decoded) = percent_decode(url_path) else {
        return error_json(400, "malformed percent-encoding");
    };
    if decoded.contains("..") || decoded.contains('\0') || decoded.contains('\\') {
        return error_json(400, "path traversal rejected");
    }
    let mut rel = decoded.trim_start_matches('/').to_string();
    if rel.is_empty() || rel.ends_with('/') {
        rel.push_str("index.html");
    }
    // Belt and braces: only plain path components may remain.
    let rel_path = PathBuf::from(&rel);
    if !rel_path
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return error_json(400, "path traversal rejected");
    }
    let full = dist.join(&rel_path);
    match std::fs::read(&full) {
        Ok(bytes) => {
            let mut resp = Response::from_data(bytes);
            if let Some(h) = hdr("Content-Type", content_type(&rel_path)) {
                resp = resp.with_header(h);
            }
            with_cors(resp)
        }
        Err(_) => error_json(404, "not found"),
    }
}

// ---------------------------------------------------------------------------
// request handling
// ---------------------------------------------------------------------------

/// Extract `from` from a `/api/blocks?from=N` query. Bad or missing -> 0.
fn parse_from(query: Option<&str>) -> usize {
    let Some(q) = query else { return 0 };
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("from=") {
            return v.parse::<usize>().unwrap_or(0);
        }
    }
    0
}

/// Handle one request. Never panics; all IO/parse failures become responses.
fn handle(
    mut req: Request,
    chain: &Mutex<Chain>,
    chain_path: &Path,
    dist: Option<&Path>,
) {
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (url.clone(), None),
    };
    let method = req.method().clone();

    let response: Response<Cursor<Vec<u8>>> = match (&method, path.as_str()) {
        (Method::Options, _) => {
            let mut resp = Response::from_string(String::new()).with_status_code(StatusCode(204));
            for (k, v) in [
                ("Access-Control-Allow-Methods", "POST, GET, OPTIONS"),
                ("Access-Control-Allow-Headers", "content-type"),
            ] {
                if let Some(h) = hdr(k, v) {
                    resp = resp.with_header(h);
                }
            }
            with_cors(resp)
        }
        (Method::Get, "/api/info") => {
            let guard = chain.lock().unwrap_or_else(|e| e.into_inner());
            json_response(200, info_json(&guard))
        }
        (Method::Get, "/api/blocks") => {
            let guard = chain.lock().unwrap_or_else(|e| e.into_inner());
            let from = parse_from(query.as_deref()).min(guard.blocks.len());
            match serde_json::to_string(&guard.blocks[from..]) {
                Ok(body) => json_response(200, body),
                Err(e) => error_json(500, &format!("serialize failed: {e}")),
            }
        }
        (Method::Post, "/api/blocks") => handle_post_blocks(&mut req, chain, chain_path),
        (Method::Get, _) => match dist {
            Some(d) => serve_static(d, &path),
            None => error_json(404, "not found"),
        },
        _ => error_json(404, "not found"),
    };

    if let Err(e) = req.respond(response) {
        eprintln!("mantis-server: failed to send response: {e}");
    }
}

/// POST /api/blocks: parse a JSON array of blocks, try to fast-forward.
fn handle_post_blocks(
    req: &mut Request,
    chain: &Mutex<Chain>,
    chain_path: &Path,
) -> Response<Cursor<Vec<u8>>> {
    let mut body = Vec::new();
    let mut limited = req.as_reader().take(MAX_BODY_BYTES as u64 + 1);
    if let Err(e) = limited.read_to_end(&mut body) {
        return error_json(400, &format!("cannot read body: {e}"));
    }
    if body.len() > MAX_BODY_BYTES {
        return error_json(413, "body too large");
    }
    let blocks: Vec<mantis_chain::Block> = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return error_json(400, &format!("bad block JSON: {e}")),
    };

    let mut guard = chain.lock().unwrap_or_else(|e| e.into_inner());
    match guard.try_extend(&blocks) {
        Ok(appended) => {
            if appended > 0 {
                if let Err(e) = persist(&guard, chain_path) {
                    eprintln!(
                        "mantis-server: WARNING: failed to persist chain to {}: {e}",
                        chain_path.display()
                    );
                }
            }
            let body = serde_json::to_string(&serde_json::json!({
                "len": guard.len(),
                "appended": appended,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            json_response(200, body)
        }
        Err(_diverged_or_invalid) => json_response(409, info_json(&guard)),
    }
}

// ---------------------------------------------------------------------------
// server loop
// ---------------------------------------------------------------------------

/// Accept loop. Factored out of `main` so tests can run it on an
/// OS-assigned port (`Server::http("127.0.0.1:0")`).
fn run(server: Server, chain: Arc<Mutex<Chain>>, chain_path: PathBuf, dist: Option<PathBuf>) {
    for request in server.incoming_requests() {
        handle(request, &chain, &chain_path, dist.as_deref());
    }
}

fn main() {
    let cfg = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let chain = match load_chain(&cfg.chain_path) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("mantis-server: {msg}");
            std::process::exit(1);
        }
    };
    let addr = format!("0.0.0.0:{}", cfg.port);
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mantis-server: cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "mantis-server listening on http://{addr} — chain {} ({} blocks), dist {}",
        cfg.chain_path.display(),
        chain.len(),
        cfg.dist
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "(none)".to_string()),
    );
    run(server, Arc::new(Mutex::new(chain)), cfg.chain_path, cfg.dist);
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::{Block, Identity};
    use mantis_graph::{GraphOp, NodeId, ParamValue};
    use std::io::Write;
    use std::net::{SocketAddr, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Unique temp path per test (no clock, no randomness needed).
    fn temp_path(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "mantis-server-test-{}-{n}-{tag}",
            std::process::id()
        ))
    }

    /// Start the server in-process on an OS-assigned port.
    fn start(chain: Chain, dist: Option<PathBuf>) -> (SocketAddr, Arc<Mutex<Chain>>, PathBuf) {
        let server = Server::http("127.0.0.1:0").expect("bind test server");
        let addr = server.server_addr().to_ip().expect("ip listener");
        let state = Arc::new(Mutex::new(chain));
        let chain_path = temp_path("chain.json");
        let (st, cp, d) = (state.clone(), chain_path.clone(), dist);
        std::thread::spawn(move || run(server, st, cp, d));
        (addr, state, chain_path)
    }

    /// Raw HTTP round-trip over TcpStream: returns (status, headers, body).
    fn http(addr: SocketAddr, raw: &str) -> (u16, String, String) {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream.write_all(raw.as_bytes()).expect("send");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read");
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text
            .split_once("\r\n\r\n")
            .unwrap_or((text.as_str(), ""));
        let status: u16 = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, head.to_string(), body.to_string())
    }

    fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
        http(
            addr,
            &format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"),
        )
    }

    fn post(addr: SocketAddr, path: &str, body: &str) -> (u16, String, String) {
        http(
            addr,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    fn demo_ops() -> Vec<GraphOp> {
        vec![
            GraphOp::AddNode {
                id: NodeId(1),
                type_name: "number_slider".into(),
                pos: (0.0, 0.0),
            },
            GraphOp::SetParam {
                id: NodeId(1),
                key: "value".into(),
                value: ParamValue::Number(3.0),
            },
        ]
    }

    /// A 1-extension chain + the blocks to push (everything after genesis).
    fn signed_extension() -> (Identity, Chain, Vec<Block>) {
        let id = Identity::generate("alice");
        let mut chain = Chain::new();
        chain.append(demo_ops(), "add slider", &id, 1000).unwrap();
        let tail = chain.blocks[1..].to_vec();
        (id, chain, tail)
    }

    // -- args -----------------------------------------------------------------

    #[test]
    fn parse_args_defaults_and_flags() {
        let cfg = parse_args(std::iter::empty()).unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.port, 7878);
        assert_eq!(cfg.chain_path, PathBuf::from("mantis-chain.json"));
        assert!(cfg.dist.is_none());

        let cfg = parse_args(
            ["--port", "9000", "--chain", "/tmp/c.json", "--dist", "web"]
                .iter()
                .map(|s| s.to_string()),
        )
        .unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.chain_path, PathBuf::from("/tmp/c.json"));
        assert_eq!(cfg.dist, Some(PathBuf::from("web")));

        assert!(parse_args(["--port"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["--port", "banana"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["--wat"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn parse_from_handles_garbage() {
        assert_eq!(parse_from(None), 0);
        assert_eq!(parse_from(Some("from=3")), 3);
        assert_eq!(parse_from(Some("x=1&from=7")), 7);
        assert_eq!(parse_from(Some("from=banana")), 0);
        assert_eq!(parse_from(Some("from=-1")), 0);
        assert_eq!(parse_from(Some("")), 0);
    }

    // -- API ------------------------------------------------------------------

    #[test]
    fn info_reports_genesis() {
        let (addr, _, _) = start(Chain::new(), None);
        let (status, head, body) = get(addr, "/api/info");
        assert_eq!(status, 200, "{body}");
        assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
        assert!(head.to_lowercase().contains("content-type: application/json"), "{head}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["len"], 1);
        assert_eq!(v["head"], Chain::new().head().hash);
    }

    #[test]
    fn push_pull_repush_fork_cycle() {
        let (id, pushed, tail) = signed_extension();
        let (addr, state, chain_path) = start(Chain::new(), None);

        // push a signed 1-block extension -> 200 appended 1
        let body_json = serde_json::to_string(&tail).unwrap();
        let (status, _, body) = post(addr, "/api/blocks", &body_json);
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["len"], 2);
        assert_eq!(v["appended"], 1);

        // accepted extension was persisted (validates on reload)
        let reloaded = load_chain(&chain_path).expect("persisted chain loads");
        assert_eq!(reloaded, pushed);

        // re-push the same blocks -> 200 appended 0
        let (status, _, body) = post(addr, "/api/blocks", &body_json);
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["len"], 2);
        assert_eq!(v["appended"], 0);

        // forked block at the same index -> 409 with our head
        let mut fork = Chain::new();
        fork.append(
            vec![GraphOp::AddNode {
                id: NodeId(0xf00d),
                type_name: "circle".into(),
                pos: (1.0, 1.0),
            }],
            "fork",
            &id,
            2000,
        )
        .unwrap();
        let fork_json = serde_json::to_string(&fork.blocks[1..]).unwrap();
        let (status, _, body) = post(addr, "/api/blocks", &fork_json);
        assert_eq!(status, 409, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["len"], 2);
        assert_eq!(v["head"], pushed.head().hash);

        // server state unchanged by the fork attempt
        let guard = state.lock().unwrap();
        assert_eq!(*guard, pushed);
        drop(guard);

        let _ = std::fs::remove_file(&chain_path);
    }

    #[test]
    fn blocks_from_round_trip() {
        let (_, pushed, tail) = signed_extension();
        let (addr, _, chain_path) = start(pushed.clone(), None);

        // from=1 -> exactly the pushed tail
        let (status, _, body) = get(addr, "/api/blocks?from=1");
        assert_eq!(status, 200);
        let got: Vec<Block> = serde_json::from_str(&body).unwrap();
        assert_eq!(got, tail);

        // from=0 / missing / garbage -> whole chain
        for q in ["/api/blocks?from=0", "/api/blocks", "/api/blocks?from=x"] {
            let (status, _, body) = get(addr, q);
            assert_eq!(status, 200, "{q}");
            let got: Vec<Block> = serde_json::from_str(&body).unwrap();
            assert_eq!(got, pushed.blocks, "{q}");
        }

        // from beyond end -> empty array
        let (status, _, body) = get(addr, "/api/blocks?from=99");
        assert_eq!(status, 200);
        let got: Vec<Block> = serde_json::from_str(&body).unwrap();
        assert!(got.is_empty());

        let _ = std::fs::remove_file(&chain_path);
    }

    #[test]
    fn garbage_post_is_400() {
        let (addr, state, _) = start(Chain::new(), None);
        let (status, _, body) = post(addr, "/api/blocks", "{not json");
        assert_eq!(status, 400, "{body}");
        assert!(body.contains("error"), "{body}");
        // an object instead of an array is also garbage
        let (status, _, _) = post(addr, "/api/blocks", "{}");
        assert_eq!(status, 400);
        assert_eq!(state.lock().unwrap().len(), 1);
    }

    #[test]
    fn tampered_block_is_409() {
        let (_, _, mut tail) = signed_extension();
        tail[0].message = "tampered".into();
        let (addr, state, _) = start(Chain::new(), None);
        let (status, _, body) = post(addr, "/api/blocks", &serde_json::to_string(&tail).unwrap());
        assert_eq!(status, 409, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["len"], 1);
        assert_eq!(state.lock().unwrap().len(), 1);
    }

    #[test]
    fn options_preflight() {
        let (addr, _, _) = start(Chain::new(), None);
        let (status, head, _) = http(
            addr,
            "OPTIONS /api/blocks HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 204);
        assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
        assert!(head.contains("Access-Control-Allow-Methods"), "{head}");
        assert!(head.contains("POST, GET"), "{head}");
        assert!(head.contains("Access-Control-Allow-Headers"), "{head}");
        assert!(head.to_lowercase().contains("content-type"), "{head}");
    }

    #[test]
    fn unknown_routes_404_without_dist() {
        let (addr, _, _) = start(Chain::new(), None);
        for path in ["/", "/index.html", "/api/nope"] {
            let (status, head, _) = get(addr, path);
            assert_eq!(status, 404, "{path}");
            assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
        }
        // non-GET/POST/OPTIONS -> 404 too (never crash)
        let (status, _, _) = http(
            addr,
            "DELETE /api/blocks HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 404);
    }

    // -- static hosting ---------------------------------------------------------

    fn make_dist() -> PathBuf {
        let dist = temp_path("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "<h1>mantis</h1>").unwrap();
        std::fs::write(dist.join("app.js"), "console.log(1)").unwrap();
        std::fs::write(dist.join("app.wasm"), [0x00, 0x61, 0x73, 0x6d]).unwrap();
        std::fs::write(dist.join("assets").join("style.css"), "body{}").unwrap();
        dist
    }

    #[test]
    fn static_files_served_with_content_types() {
        let dist = make_dist();
        let (addr, _, _) = start(Chain::new(), Some(dist.clone()));

        let (status, head, body) = get(addr, "/");
        assert_eq!(status, 200);
        assert!(head.contains("text/html"), "{head}");
        assert_eq!(body, "<h1>mantis</h1>");
        assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");

        let (status, head, _) = get(addr, "/app.js");
        assert_eq!(status, 200);
        assert!(head.contains("text/javascript"), "{head}");

        let (status, head, _) = get(addr, "/app.wasm");
        assert_eq!(status, 200);
        assert!(head.contains("application/wasm"), "{head}");

        let (status, head, _) = get(addr, "/assets/style.css");
        assert_eq!(status, 200);
        assert!(head.contains("text/css"), "{head}");

        let (status, _, _) = get(addr, "/missing.png");
        assert_eq!(status, 404);

        // API still wins over static
        let (status, _, body) = get(addr, "/api/info");
        assert_eq!(status, 200);
        assert!(body.contains("head"), "{body}");

        let _ = std::fs::remove_dir_all(&dist);
    }

    #[test]
    fn path_traversal_rejected() {
        let dist = make_dist();
        // a juicy target one level above dist
        let secret = dist.parent().unwrap().join("secret-mantis-test.txt");
        std::fs::write(&secret, "s3cr3t").unwrap();
        let (addr, _, _) = start(Chain::new(), Some(dist.clone()));

        for path in [
            "/..%2fsecret-mantis-test.txt",
            "/../secret-mantis-test.txt",
            "/%2e%2e/secret-mantis-test.txt",
            "/assets/../../secret-mantis-test.txt",
            "/..",
        ] {
            let (status, _, body) = get(addr, path);
            assert!(
                (400..500).contains(&status),
                "{path} -> {status} {body}"
            );
            assert!(!body.contains("s3cr3t"), "{path} leaked: {body}");
        }

        let _ = std::fs::remove_file(&secret);
        let _ = std::fs::remove_dir_all(&dist);
    }

    #[test]
    fn percent_decode_cases() {
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/plain").as_deref(), Some("/plain"));
        assert_eq!(percent_decode("/%2e%2e").as_deref(), Some("/.."));
        assert_eq!(percent_decode("/bad%zz"), None);
        assert_eq!(percent_decode("/trunc%2"), None);
    }

    // -- persistence ------------------------------------------------------------

    #[test]
    fn load_chain_missing_fresh_invalid() {
        let path = temp_path("load.json");
        // missing -> fresh
        let c = load_chain(&path).unwrap();
        assert_eq!(c, Chain::new());
        // valid file -> loads
        let (_, chain, _) = signed_extension();
        persist(&chain, &path).unwrap();
        assert_eq!(load_chain(&path).unwrap(), chain);
        // tampered file -> error, not silent reset
        let text = std::fs::read_to_string(&path).unwrap().replace("add slider", "EVIL");
        std::fs::write(&path, text).unwrap();
        assert!(load_chain(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
