#![recursion_limit = "512"]
//! mantis-server — chain sync API + static hosting of the wasm build.
//!
//! The legacy single-chain mode uses a synchronous tiny_http loop around one
//! `Mutex<Chain>`. The v2 multi-project mode dispatches requests to a bounded
//! four-worker pool and keeps per-project state and cached audit material.
//! Accepted mutations are persisted with an observed atomic replacement before
//! a success response is sent.
//!
//! Routes (see ARCHITECTURE.md):
//!   GET  /api/info          -> {"len":N,"head":"<hex>"}
//!   GET  /api/audit         -> validated provenance/checkpoint summary
//!   GET  /api/blocks?from=N[&limit=N] -> JSON array of blocks
//!   POST /api/blocks        -> body: JSON array of blocks; 200 {"len","appended"},
//!                              409 on divergence, 422 on invalid blocks,
//!                              500 when durable persistence fails
//!   OPTIONS *               -> 204 + CORS preflight headers
//!   GET  /<path>            -> static files under --dist (path-traversal safe),
//!                              "/" -> index.html; 404 otherwise
//!
//! Legacy compatibility responses retain wildcard CORS. V2 defaults to
//! same-origin and permits only exact origins from `MANTIS_ALLOWED_ORIGINS`.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use mantis_chain::{Chain, ChainError};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

mod projects;
mod rate_limit;
mod storage;

/// The request cap equals the maximum complete signed project document. This
/// keeps atomic export/import possible without exposing an unauthenticated
/// oversized allocation on the project-create endpoint.
const MAX_BODY_BYTES: usize = projects::MAX_PROJECT_DOCUMENT_BYTES;

/// Upper bound for an explicitly paginated pull. Omitting `limit` retains the
/// v1 behavior (all remaining blocks) for existing GUI clients.
const MAX_BLOCKS_PAGE: usize = projects::MAX_PAGE_LIMIT;

/// Marker emitted by Trunk on its generated bootstrap and preload tags. It
/// must never reach a browser: every HTML response replaces it with a nonce.
const CSP_NONCE_PLACEHOLDER: &str = "{{__MANTIS_CSP_NONCE__}}";

// ---------------------------------------------------------------------------
// configuration / args
// ---------------------------------------------------------------------------

/// Parsed command line.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    port: u16,
    chain_path: PathBuf,
    data_dir: Option<PathBuf>,
    dist: Option<PathBuf>,
    public_base_path: String,
    operator_keys: Vec<String>,
    allowed_origins: Vec<String>,
    max_project_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 7878,
            chain_path: PathBuf::from("mantis-chain.json"),
            data_dir: None,
            dist: None,
            public_base_path: String::new(),
            operator_keys: Vec::new(),
            allowed_origins: Vec::new(),
            max_project_bytes: projects::DEFAULT_MAX_PROJECT_BYTES,
        }
    }
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let mut cfg = Self::default();
        if let Ok(value) = std::env::var("PORT") {
            cfg.port = value
                .parse::<u16>()
                .map_err(|_| format!("invalid PORT: {value}"))?;
        }
        if let Ok(value) = std::env::var("MANTIS_CHAIN_PATH") {
            if !value.trim().is_empty() {
                cfg.chain_path = PathBuf::from(value);
            }
        }
        if let Ok(value) = std::env::var("MANTIS_DATA_DIR") {
            if !value.trim().is_empty() {
                cfg.data_dir = Some(PathBuf::from(value));
            }
        }
        if let Ok(value) = std::env::var("MANTIS_DIST_DIR") {
            if !value.trim().is_empty() {
                cfg.dist = Some(PathBuf::from(value));
            }
        }
        if let Ok(value) = std::env::var("MANTIS_PUBLIC_BASE_PATH") {
            cfg.public_base_path = parse_public_base_path(&value)?;
        }
        if let Ok(value) = std::env::var("MANTIS_OPERATOR_KEYS") {
            cfg.operator_keys = csv_values(&value);
        }
        if let Ok(value) = std::env::var("MANTIS_ALLOWED_ORIGINS") {
            cfg.allowed_origins = csv_values(&value);
        }
        if let Ok(value) = std::env::var("MANTIS_MAX_PROJECT_BYTES") {
            let parsed = value
                .parse::<usize>()
                .map_err(|_| format!("invalid MANTIS_MAX_PROJECT_BYTES: {value}"))?;
            if parsed == 0 || parsed > projects::MAX_PROJECT_DOCUMENT_BYTES {
                return Err(format!(
                    "MANTIS_MAX_PROJECT_BYTES must be between 1 and {}",
                    projects::MAX_PROJECT_DOCUMENT_BYTES
                ));
            }
            cfg.max_project_bytes = parsed;
        }
        Ok(cfg)
    }
}

fn csv_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_public_base_path(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'/') || bytes.last() == Some(&b'/') {
        return Err(public_base_path_error());
    }

    let mut segment_start = 1;
    for index in 1..=bytes.len() {
        if index == bytes.len() || bytes[index] == b'/' {
            let segment = &bytes[segment_start..index];
            if segment.is_empty() || segment == b"." || segment == b".." {
                return Err(public_base_path_error());
            }
            segment_start = index + 1;
            continue;
        }
        if !bytes[index].is_ascii_alphanumeric()
            && !matches!(bytes[index], b'-' | b'.' | b'_' | b'~')
        {
            return Err(public_base_path_error());
        }
    }
    Ok(value.to_string())
}

fn public_base_path_error() -> String {
    "MANTIS_PUBLIC_BASE_PATH must be empty or a canonical absolute path such as /mantis (ASCII unreserved characters only, no trailing slash)".to_string()
}

/// Hand-rolled argument parsing. CLI flags override environment-derived base
/// values; `parse_args` keeps deterministic defaults for unit tests.
fn parse_args_from<I: Iterator<Item = String>>(
    mut cfg: Config,
    mut args: I,
) -> Result<Config, String> {
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let v = args.next().ok_or("--port needs a value")?;
                cfg.port = v.parse::<u16>().map_err(|_| format!("invalid port: {v}"))?;
            }
            "--chain" => {
                let v = args.next().ok_or("--chain needs a value")?;
                cfg.chain_path = PathBuf::from(v);
                cfg.data_dir = None;
            }
            "--data-dir" => {
                let v = args.next().ok_or("--data-dir needs a value")?;
                cfg.data_dir = Some(PathBuf::from(v));
            }
            "--dist" => {
                let v = args.next().ok_or("--dist needs a value")?;
                cfg.dist = Some(PathBuf::from(v));
            }
            "--public-base-path" => {
                let value = args.next().ok_or("--public-base-path needs a value")?;
                cfg.public_base_path = parse_public_base_path(&value)?;
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    Ok(cfg)
}

#[cfg(test)]
fn parse_args<I: Iterator<Item = String>>(args: I) -> Result<Config, String> {
    parse_args_from(Config::default(), args)
}

const USAGE: &str = "usage: mantis-server [--port N] [--chain PATH | --data-dir DIR] [--dist DIR] [--public-base-path PATH]
  --port N      listen port (default 7878)
  --chain PATH  chain JSON file (default mantis-chain.json)
  --data-dir DIR  multi-project data directory (recommended for deployments)
  --dist DIR    serve static files from DIR (wasm app)
  --public-base-path PATH  advertise a reverse-proxy path prefix in OpenAPI";

// ---------------------------------------------------------------------------
// response helpers
// ---------------------------------------------------------------------------

/// Build a header from static-ish strings; `None` only on malformed input,
/// which never happens for the literals used here.
fn hdr(key: &str, value: &str) -> Option<Header> {
    Header::from_bytes(key.as_bytes(), value.as_bytes()).ok()
}

/// Attach the legacy compatibility CORS headers.
fn with_cors<R: Read>(mut resp: Response<R>) -> Response<R> {
    for (key, value) in [
        ("Access-Control-Allow-Origin", "*"),
        (
            "Access-Control-Expose-Headers",
            "X-Mantis-Chain-Length, X-Mantis-From, X-Mantis-Next-From, X-Mantis-Head",
        ),
    ] {
        if let Some(h) = hdr(key, value) {
            resp = resp.with_header(h);
        }
    }
    resp
}

/// Legacy JSON response with status + wildcard CORS.
fn json_response(status: u16, body: String) -> Response<Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(body).with_status_code(StatusCode(status));
    for (key, value) in [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
    ] {
        if let Some(h) = hdr(key, value) {
            resp = resp.with_header(h);
        }
    }
    with_cors(with_security_headers(resp))
}

fn content_security_policy(nonce: Option<&str>) -> String {
    let nonce_source = nonce
        .map(|value| format!(" 'nonce-{value}'"))
        .unwrap_or_default();
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{nonce_source}; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
    )
}

fn with_security_headers<R: Read>(resp: Response<R>) -> Response<R> {
    with_security_headers_and_nonce(resp, None)
}

fn with_security_headers_and_nonce<R: Read>(
    mut resp: Response<R>,
    nonce: Option<&str>,
) -> Response<R> {
    for (key, value) in [
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
    ] {
        if let Some(header) = hdr(key, value) {
            resp = resp.with_header(header);
        }
    }
    if let Some(header) = hdr("Content-Security-Policy", &content_security_policy(nonce)) {
        resp = resp.with_header(header);
    }
    resp
}

/// Generic errors use the same object shape as chain validation errors so an
/// agent never has to branch on whether `error` is a string or an object.
fn error_json(status: u16, msg: &str) -> Response<Cursor<Vec<u8>>> {
    let code = match status {
        400 => "bad_request",
        404 => "not_found",
        405 => "method_not_allowed",
        413 => "body_too_large",
        _ if status >= 500 => "internal",
        _ => "request_failed",
    };
    let body = serde_json::to_string(&serde_json::json!({
        "error": { "code": code, "message": msg }
    }))
    .unwrap_or_else(|_| "{\"error\":{\"code\":\"internal\"}}".to_string());
    json_response(status, body)
}

fn same_origin_error_json(status: u16, code: &str, msg: &str) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::json!({
        "error": { "code": code, "message": msg }
    })
    .to_string();
    secure_json_response(status, body, None)
}

fn secure_json_response(
    status: u16,
    body: String,
    cors_origin: Option<&str>,
) -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    for (key, value) in [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
    ] {
        if let Some(header) = hdr(key, value) {
            response = response.with_header(header);
        }
    }
    if let Some(origin) = cors_origin {
        for (key, value) in [
            ("Access-Control-Allow-Origin", origin),
            (
                "Access-Control-Expose-Headers",
                "X-Mantis-Head, X-Mantis-Chain-Length",
            ),
            ("Vary", "Origin"),
        ] {
            if let Some(header) = hdr(key, value) {
                response = response.with_header(header);
            }
        }
    }
    with_security_headers(response)
}

/// Structured chain error with stable codes/context for non-interactive
/// clients. The current head is always included so a conflict can be repaired
/// without a second discovery request.
fn chain_error_json(status: u16, error: &ChainError, chain: &Chain) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_string(&serde_json::json!({
        "error": {
            "code": error.code(),
            "message": error.to_string(),
            "block": error.block_index(),
            "op": error.operation_index(),
        },
        "len": chain.len(),
        "head": chain.head().hash,
    }))
    .unwrap_or_else(|_| "{\"error\":{\"code\":\"internal\"}}".to_string());
    json_response(status, body)
}

fn persistence_error_json(chain: &Chain, msg: &str) -> Response<Cursor<Vec<u8>>> {
    persistence_state_error_json(500, "persistence_failed", chain, msg)
}

fn persistence_state_error_json(
    status: u16,
    code: &str,
    chain: &Chain,
    msg: &str,
) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_string(&serde_json::json!({
        "error": {
            "code": code,
            "message": msg,
        },
        "len": chain.len(),
        "head": chain.head().hash,
    }))
    .unwrap_or_else(|_| "{\"error\":{\"code\":\"persistence_failed\"}}".to_string());
    json_response(status, body)
}

/// `{"len":N,"head":"<hex>"}` for the current chain state.
fn info_json(chain: &Chain) -> String {
    serde_json::to_string(&serde_json::json!({
        "api_version": 1,
        "chain_format_version": chain
            .format_version()
            .unwrap_or(mantis_chain::LEGACY_CHAIN_FORMAT_VERSION),
        "len": chain.len(),
        "head": chain.head().hash,
        "genesis": chain.blocks.first().map(|block| block.hash.as_str()),
        "total_ops": chain.total_ops(),
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// persistence
// ---------------------------------------------------------------------------

/// Durable atomic replacement: fully write + sync `<path>.tmp`, then rename it
/// over `path`. Callers must not publish the candidate chain in memory until
/// this succeeds, otherwise an acknowledged block can disappear on restart.
fn persist(chain: &Chain, path: &Path) -> Result<(), storage::PersistFailure> {
    storage::persist_json_observed(chain, path)
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

/// Replace Trunk's build-time marker with a cryptographically random 128-bit
/// nonce. Returning an error keeps the server fail-closed if the operating
/// system RNG is unavailable or the declared UTF-8 HTML is malformed.
fn inject_html_nonce(bytes: Vec<u8>) -> Result<(Vec<u8>, String), String> {
    let mut random = [0_u8; 16];
    getrandom::getrandom(&mut random)
        .map_err(|error| format!("cannot generate CSP nonce: {error}"))?;
    let nonce = STANDARD.encode(random);
    let html = String::from_utf8(bytes)
        .map_err(|error| format!("static HTML is not valid UTF-8: {error}"))?;
    Ok((
        html.replace(CSP_NONCE_PLACEHOLDER, &nonce).into_bytes(),
        nonce,
    ))
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
    serve_static_policy(dist, url_path, true)
}

fn serve_static_same_origin(dist: &Path, url_path: &str) -> Response<Cursor<Vec<u8>>> {
    serve_static_policy(dist, url_path, false)
}

fn serve_static_policy(
    dist: &Path,
    url_path: &str,
    legacy_wildcard_cors: bool,
) -> Response<Cursor<Vec<u8>>> {
    let static_error = |status, message: &str| {
        if legacy_wildcard_cors {
            error_json(status, message)
        } else {
            same_origin_error_json(status, "static_file_error", message)
        }
    };
    // Reject traversal *before and after* decoding: "%2e%2e", "..%2f" etc.
    if url_path.contains("..") {
        return static_error(400, "path traversal rejected");
    }
    let Some(decoded) = percent_decode(url_path) else {
        return static_error(400, "malformed percent-encoding");
    };
    if decoded.contains("..") || decoded.contains('\0') || decoded.contains('\\') {
        return static_error(400, "path traversal rejected");
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
        return static_error(400, "path traversal rejected");
    }
    let full = dist.join(&rel_path);
    match std::fs::read(&full) {
        Ok(bytes) => {
            let is_html = matches!(
                rel_path.extension().and_then(|value| value.to_str()),
                Some("html" | "htm")
            );
            let (bytes, nonce) = if is_html {
                match inject_html_nonce(bytes) {
                    Ok((bytes, nonce)) => (bytes, Some(nonce)),
                    Err(error) => return static_error(500, &error),
                }
            } else {
                (bytes, None)
            };
            let mut resp = Response::from_data(bytes);
            if let Some(h) = hdr("Content-Type", content_type(&rel_path)) {
                resp = resp.with_header(h);
            }
            let cache = if rel_path.extension().and_then(|value| value.to_str()) == Some("html") {
                "no-cache"
            } else if matches!(
                rel_path.extension().and_then(|value| value.to_str()),
                Some("js" | "mjs" | "wasm")
            ) {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            if let Some(header) = hdr("Cache-Control", cache) {
                resp = resp.with_header(header);
            }
            let response = with_security_headers_and_nonce(resp, nonce.as_deref());
            if legacy_wildcard_cors {
                with_cors(response)
            } else {
                response
            }
        }
        Err(_) => static_error(404, "not found"),
    }
}

// ---------------------------------------------------------------------------
// request handling
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlocksQuery {
    from: usize,
    limit: Option<usize>,
}

/// Parse a blocks query strictly. Silently treating `from=typo` as zero can
/// make an agent unexpectedly download the entire history, so malformed,
/// duplicate, and unknown parameters are explicit 400 errors.
fn parse_blocks_query(query: Option<&str>) -> Result<BlocksQuery, String> {
    let mut from = None;
    let mut limit = None;
    let Some(query) = query else {
        return Ok(BlocksQuery {
            from: 0,
            limit: None,
        });
    };
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("query parameter needs a value: {pair}"))?;
        match key {
            "from" => {
                if from.is_some() {
                    return Err("duplicate query parameter: from".into());
                }
                from = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| format!("from must be a non-negative integer: {value}"))?,
                );
            }
            "limit" => {
                if limit.is_some() {
                    return Err("duplicate query parameter: limit".into());
                }
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| format!("limit must be a positive integer: {value}"))?;
                if parsed == 0 || parsed > MAX_BLOCKS_PAGE {
                    return Err(format!("limit must be between 1 and {MAX_BLOCKS_PAGE}"));
                }
                limit = Some(parsed);
            }
            other => return Err(format!("unknown query parameter: {other}")),
        }
    }
    Ok(BlocksQuery {
        from: from.unwrap_or(0),
        limit,
    })
}

fn blocks_json_response(chain: &Chain, query: BlocksQuery) -> Response<Cursor<Vec<u8>>> {
    let from = query.from.min(chain.blocks.len());
    let end = match query.limit {
        Some(limit) => from.saturating_add(limit).min(chain.blocks.len()),
        None => chain.blocks.len(),
    };
    let body = match serde_json::to_string(&chain.blocks[from..end]) {
        Ok(body) => body,
        Err(error) => return error_json(500, &format!("serialize failed: {error}")),
    };
    let mut response = json_response(200, body);
    for (key, value) in [
        ("X-Mantis-Chain-Length", chain.len().to_string()),
        ("X-Mantis-From", from.to_string()),
        ("X-Mantis-Next-From", end.to_string()),
        ("X-Mantis-Head", chain.head().hash.clone()),
    ] {
        if let Some(header) = hdr(key, &value) {
            response = response.with_header(header);
        }
    }
    response
}

/// Handle one request. Never panics; all IO/parse failures become responses.
fn handle(
    mut req: Request,
    chain: &Mutex<Chain>,
    chain_path: &Path,
    writes_allowed: &AtomicBool,
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
        (Method::Get, "/healthz") => {
            json_response(200, serde_json::json!({"status":"ok"}).to_string())
        }
        (Method::Get, "/readyz") => {
            let guard = chain.lock().unwrap_or_else(|error| error.into_inner());
            if !writes_allowed.load(Ordering::Acquire) {
                persistence_state_error_json(
                    503,
                    "storage_not_ready",
                    &guard,
                    "writes are disabled after an uncertain persistence outcome; restart and audit storage",
                )
            } else {
                match guard.audit() {
                    Ok(audit) => json_response(
                        200,
                        serde_json::json!({
                            "status": "ready",
                            "len": audit.block_count,
                            "head": audit.head_hash,
                        })
                        .to_string(),
                    ),
                    Err(error) => chain_error_json(503, &error, &guard),
                }
            }
        }
        (Method::Get, "/api/audit") => {
            let guard = chain.lock().unwrap_or_else(|e| e.into_inner());
            match guard.audit() {
                Ok(audit) => match serde_json::to_string(&audit) {
                    Ok(body) => json_response(200, body),
                    Err(error) => error_json(500, &format!("serialize failed: {error}")),
                },
                Err(error) => chain_error_json(500, &error, &guard),
            }
        }
        (Method::Get, "/api/blocks") => {
            let guard = chain.lock().unwrap_or_else(|e| e.into_inner());
            match parse_blocks_query(query.as_deref()) {
                Ok(blocks_query) => blocks_json_response(&guard, blocks_query),
                Err(error) => error_json(400, &error),
            }
        }
        (Method::Post, "/api/blocks") => {
            handle_post_blocks(&mut req, chain, chain_path, writes_allowed)
        }
        (_, "/api/info" | "/api/audit" | "/api/blocks") => {
            let mut response = error_json(405, "method not allowed for API endpoint");
            let allowed = if path == "/api/blocks" {
                "GET, POST, OPTIONS"
            } else {
                "GET, OPTIONS"
            };
            if let Some(header) = hdr("Allow", allowed) {
                response = response.with_header(header);
            }
            response
        }
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
    writes_allowed: &AtomicBool,
) -> Response<Cursor<Vec<u8>>> {
    if !writes_allowed.load(Ordering::Acquire) {
        let guard = chain.lock().unwrap_or_else(|error| error.into_inner());
        return persistence_state_error_json(
            503,
            "storage_not_ready",
            &guard,
            "writes are disabled after an uncertain persistence outcome; restart and audit storage",
        );
    }
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
    // Work on an isolated candidate. It becomes observable only after durable
    // persistence succeeds, so HTTP 200 always means a restart-safe commit.
    let mut candidate = guard.clone();
    match candidate.try_extend(&blocks) {
        Ok(appended) => {
            // Persist even an idempotent re-push. If the backing file was
            // removed or became unwritable at runtime, a fresh 200 must not
            // falsely claim that the acknowledged head is restart-safe.
            match persist(&candidate, chain_path) {
                Ok(()) => {}
                Err(storage::PersistFailure::NotPublished(error)) => {
                    eprintln!(
                        "mantis-server: failed to persist candidate chain to {}: {error}",
                        chain_path.display()
                    );
                    return persistence_error_json(
                        &guard,
                        &format!("chain was not accepted: {error}"),
                    );
                }
                Err(storage::PersistFailure::Published(error)) => {
                    *guard = candidate;
                    writes_allowed.store(false, Ordering::Release);
                    eprintln!(
                        "mantis-server: candidate chain is visible at {} but directory durability is uncertain: {error}",
                        chain_path.display()
                    );
                    return persistence_state_error_json(
                        500,
                        "persistence_uncertain",
                        &guard,
                        &format!(
                            "candidate chain is visible but directory durability is uncertain: {error}"
                        ),
                    );
                }
            }
            *guard = candidate;
            let body = serde_json::to_string(&serde_json::json!({
                "len": guard.len(),
                "appended": appended,
                "head": guard.head().hash,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            json_response(200, body)
        }
        Err(error @ ChainError::Diverged { .. }) => chain_error_json(409, &error, &guard),
        Err(error) => chain_error_json(422, &error, &guard),
    }
}

// ---------------------------------------------------------------------------
// server loop
// ---------------------------------------------------------------------------

/// Accept loop. Factored out of `main` so tests can run it on an
/// OS-assigned port (`Server::http("127.0.0.1:0")`).
fn run(server: Server, chain: Arc<Mutex<Chain>>, chain_path: PathBuf, dist: Option<PathBuf>) {
    let writes_allowed = AtomicBool::new(true);
    for request in server.incoming_requests() {
        handle(
            request,
            &chain,
            &chain_path,
            &writes_allowed,
            dist.as_deref(),
        );
    }
}

// ---------------------------------------------------------------------------
// multi-project v2 server
// ---------------------------------------------------------------------------

fn request_header(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str().trim().to_string())
}

fn cors_origin(
    req: &Request,
    allowed_origins: &[String],
) -> Result<Option<String>, projects::ProjectError> {
    let Some(origin) = request_header(req, "Origin") else {
        return Ok(None);
    };
    if allowed_origins.iter().any(|allowed| allowed == &origin) {
        return Ok(Some(origin));
    }
    let host = request_header(req, "Host").unwrap_or_default();
    let forwarded = request_header(req, "X-Forwarded-Proto")
        .and_then(|value| value.split(',').next().map(str::trim).map(str::to_string));
    let scheme = forwarded.as_deref().unwrap_or("http");
    if !host.is_empty() && origin == format!("{scheme}://{host}") {
        return Ok(Some(origin));
    }
    Err(projects::ProjectError::new(
        403,
        "origin_not_allowed",
        "request origin is not allowed",
    ))
}

fn v2_json<T: serde::Serialize>(
    status: u16,
    value: &T,
    origin: Option<&str>,
) -> Response<Cursor<Vec<u8>>> {
    match serde_json::to_string(value) {
        Ok(body) => secure_json_response(status, body, origin),
        Err(error) => same_origin_error_json(500, "serialization_failed", &error.to_string()),
    }
}

fn v2_error(error: projects::ProjectError, origin: Option<&str>) -> Response<Cursor<Vec<u8>>> {
    v2_json(error.status, &error.response(), origin)
}

fn read_json_body<T: serde::de::DeserializeOwned>(
    request: &mut Request,
) -> Result<T, projects::ProjectError> {
    let mut body = Vec::new();
    let mut limited = request.as_reader().take(MAX_BODY_BYTES as u64 + 1);
    limited
        .read_to_end(&mut body)
        .map_err(|error| projects::ProjectError::new(400, "body_read_failed", error.to_string()))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(projects::ProjectError::new(
            413,
            "body_too_large",
            format!("request body exceeds {MAX_BODY_BYTES} bytes"),
        ));
    }
    serde_json::from_slice(&body).map_err(|error| {
        projects::ProjectError::new(400, "bad_json", format!("invalid JSON body: {error}"))
    })
}

fn parse_project_path(
    path: &str,
) -> Result<Option<(mantis_protocol::ProjectSlug, &str)>, projects::ProjectError> {
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if segments.len() < 4 || segments[..3] != ["api", "v2", "projects"] {
        return Ok(None);
    }
    let project = mantis_protocol::ProjectSlug::from_str(segments[3])
        .map_err(|error| projects::ProjectError::new(400, "bad_project_id", error.to_string()))?;
    let resource = match segments.get(4).copied() {
        None => "info",
        Some(resource) if segments.len() == 5 => resource,
        Some(_) => {
            return Err(projects::ProjectError::new(
                404,
                "not_found",
                "unknown project resource",
            ))
        }
    };
    Ok(Some((project, resource)))
}

fn parse_page_query(query: Option<&str>) -> Result<(usize, usize), projects::ProjectError> {
    let query = parse_blocks_query(query)
        .map_err(|error| projects::ProjectError::new(400, "bad_query", error))?;
    Ok((
        query.from,
        query.limit.unwrap_or(projects::DEFAULT_PAGE_LIMIT),
    ))
}

fn parse_include_archived(query: Option<&str>) -> Result<bool, projects::ProjectError> {
    match query {
        None | Some("") => Ok(false),
        Some("include_archived=1") => Ok(true),
        Some("include_archived=0") => Ok(false),
        Some(_) => Err(projects::ProjectError::new(
            400,
            "bad_query",
            "projects query accepts only include_archived=0 or 1",
        )),
    }
}

fn openapi_v2(public_base_path: &str) -> serde_json::Value {
    fn json_response(description: &str, schema: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "description": description,
            "content": {"application/json": {"schema": schema}}
        })
    }

    let error_response = serde_json::json!({"$ref": "#/components/responses/Error"});
    let project_parameter = serde_json::json!({"$ref": "#/components/parameters/Project"});
    let page_parameters = serde_json::json!([
        {"$ref": "#/components/parameters/From"},
        {"$ref": "#/components/parameters/Limit"}
    ]);
    let mut document = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "MantisCAD collaboration API",
            "version": "2.0.0"
        },
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "paths": {
            "/api/v2/info": {"get": {
                "summary": "Server capabilities",
                "responses": {
                    "200": json_response("Capability contract", serde_json::json!({"$ref": "#/components/schemas/ApiInfoV2"})),
                    "default": error_response.clone()
                }
            }},
            "/api/v2/openapi.json": {"get": {
                "summary": "This OpenAPI document",
                "responses": {
                    "200": json_response("OpenAPI 3.1 document", serde_json::json!({"type": "object"})),
                    "default": error_response.clone()
                }
            }},
            "/api/v2/projects": {
                "get": {
                    "summary": "List public projects",
                    "parameters": [{"$ref": "#/components/parameters/IncludeArchived"}],
                    "responses": {
                        "200": json_response("Public projects", serde_json::json!({
                            "type": "array", "items": {"$ref": "#/components/schemas/ProjectSummaryV1"}
                        })),
                        "default": error_response.clone()
                    }
                },
                "post": {
                    "summary": "Atomically create an operator-signed project",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ProjectBootstrapV1"}}}
                    },
                    "responses": {
                        "201": json_response("Created project", serde_json::json!({"$ref": "#/components/schemas/ProjectInfoV2"})),
                        "default": error_response.clone()
                    }
                }
            },
            "/api/v2/projects/{project}/info": {
                "parameters": [project_parameter.clone()],
                "get": {
                    "summary": "Project state and public access list",
                    "responses": {
                        "200": json_response("Project information", serde_json::json!({"$ref": "#/components/schemas/ProjectInfoV2"})),
                        "default": error_response.clone()
                    }
                }
            },
            "/api/v2/projects/{project}/create": {
                "parameters": [project_parameter.clone()],
                "get": {
                    "summary": "Operator-signed project creation proof",
                    "responses": {
                        "200": json_response("Creation proof", serde_json::json!({"$ref": "#/components/schemas/ProjectCreateV1"})),
                        "default": error_response.clone()
                    }
                }
            },
            "/api/v2/projects/{project}/blocks": {
                "parameters": [project_parameter.clone()],
                "get": {
                    "summary": "Read a bounded block page",
                    "parameters": page_parameters.clone(),
                    "responses": {
                        "200": json_response("Block page", serde_json::json!({"$ref": "#/components/schemas/BlocksPageV2"})),
                        "default": error_response.clone()
                    }
                },
                "post": {
                    "summary": "Compare-and-swap signed blocks",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/PushRequestV2"}}}
                    },
                    "responses": {
                        "200": json_response("Accepted tail", serde_json::json!({"$ref": "#/components/schemas/PushResponseV2"})),
                        "default": error_response.clone()
                    }
                }
            },
            "/api/v2/projects/{project}/audit": {
                "parameters": [project_parameter.clone()],
                "get": {
                    "summary": "Return the cached startup/write-validated chain audit",
                    "responses": {
                        "200": json_response("Validated audit checkpoint", serde_json::json!({"$ref": "#/components/schemas/ChainAudit"})),
                        "default": error_response.clone()
                    }
                }
            },
            "/api/v2/projects/{project}/access-log": {
                "parameters": [project_parameter],
                "get": {
                    "summary": "Read signed access records",
                    "parameters": page_parameters,
                    "responses": {
                        "200": json_response("Access record page", serde_json::json!({"$ref": "#/components/schemas/AccessPageV1"})),
                        "default": error_response.clone()
                    }
                },
                "post": {
                    "summary": "Append owner-signed access records",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": {
                            "type": "array", "minItems": 1, "maxItems": projects::MAX_PUSH_BLOCKS,
                            "items": {"$ref": "#/components/schemas/AccessRecordV1"}
                        }}}
                    },
                    "responses": {
                        "200": json_response("Updated access state", serde_json::json!({"$ref": "#/components/schemas/AccessStateV1"})),
                        "default": error_response.clone()
                    }
                }
            },
            "/healthz": {"get": {
                "summary": "Liveness",
                "responses": {"200": json_response("Process is alive", serde_json::json!({"$ref": "#/components/schemas/Status"}))}
            }},
            "/readyz": {"get": {
                "summary": "Validated storage readiness",
                "responses": {
                    "200": json_response("Storage is ready", serde_json::json!({"$ref": "#/components/schemas/Status"})),
                    "503": json_response("Storage is not ready", serde_json::json!({"$ref": "#/components/schemas/Status"}))
                }
            }}
        },
        "components": {
            "parameters": {
                "Project": {
                    "name": "project", "in": "path", "required": true,
                    "schema": {"$ref": "#/components/schemas/ProjectSlug"}
                },
                "From": {
                    "name": "from", "in": "query", "required": false,
                    "schema": {"type": "integer", "format": "uint64", "minimum": 0, "default": 0}
                },
                "Limit": {
                    "name": "limit", "in": "query", "required": false,
                    "schema": {"type": "integer", "minimum": 1, "maximum": projects::MAX_PAGE_LIMIT, "default": projects::DEFAULT_PAGE_LIMIT}
                },
                "IncludeArchived": {
                    "name": "include_archived", "in": "query", "required": false,
                    "schema": {"type": "integer", "enum": [0, 1], "default": 0}
                }
            },
            "responses": {
                "Error": json_response("Structured error", serde_json::json!({"$ref": "#/components/schemas/ErrorResponseV1"}))
            },
            "schemas": {
                "ProjectSlug": {"type": "string", "minLength": 3, "maxLength": 63, "pattern": "^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$"},
                "HashHex": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "ChainId": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "PublicKeyHex": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "BlockAuthorKey": {"anyOf": [{"$ref": "#/components/schemas/PublicKeyHex"}, {"const": ""}], "description": "Canonical Ed25519 key, or empty only on genesis."},
                "SignatureHex": {"type": "string", "pattern": "^[0-9a-f]{128}$"},
                "NullableChainId": {"anyOf": [{"$ref": "#/components/schemas/ChainId"}, {"type": "null"}]},
                "Status": {
                    "type": "object", "additionalProperties": false, "required": ["status"],
                    "properties": {"status": {"type": "string"}}
                },
                "ApiInfoV2": {
                    "type": "object", "additionalProperties": false,
                    "required": ["api_version", "app_version", "git_sha", "supported_chain_formats", "capabilities"],
                    "properties": {
                        "api_version": {"type": "integer", "const": 2},
                        "app_version": {"type": "string"}, "git_sha": {"type": "string"},
                        "supported_chain_formats": {"type": "array", "items": {"type": "integer"}},
                        "capabilities": {"type": "array", "items": {"type": "string"}, "uniqueItems": true}
                    }
                },
                "ChainStateV1": {
                    "type": "object", "additionalProperties": false,
                    "required": ["len", "head", "genesis", "total_ops"],
                    "properties": {
                        "len": {"type": "integer", "format": "uint64", "minimum": 1},
                        "head": {"$ref": "#/components/schemas/HashHex"},
                        "genesis": {"$ref": "#/components/schemas/HashHex"},
                        "total_ops": {"type": "integer", "format": "uint64", "minimum": 0}
                    }
                },
                "ProjectManifestV1": {
                    "type": "object", "additionalProperties": false,
                    "required": ["schema_version", "project_id", "title", "chain_id", "genesis_hash", "chain_format_version", "created_at_ms", "created_by", "initial_owner", "archived"],
                    "properties": {
                        "schema_version": {"type": "integer", "const": 1},
                        "project_id": {"$ref": "#/components/schemas/ProjectSlug"},
                        "title": {"type": "string", "minLength": 1, "maxLength": 120},
                        "chain_id": {"$ref": "#/components/schemas/NullableChainId"},
                        "genesis_hash": {"$ref": "#/components/schemas/HashHex"},
                        "chain_format_version": {"type": "integer", "enum": [1, 2]},
                        "created_at_ms": {"type": "integer", "format": "uint64", "minimum": 0},
                        "created_by": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "initial_owner": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "archived": {"type": "boolean"}
                    }
                },
                "ProjectCreateV1": {
                    "type": "object", "additionalProperties": false,
                    "required": ["schema_version", "project_id", "title", "chain_id", "initial_len", "initial_head", "created_at_ms", "initial_owner", "operator_pk", "hash", "sig"],
                    "properties": {
                        "schema_version": {"type": "integer", "const": 1},
                        "project_id": {"$ref": "#/components/schemas/ProjectSlug"},
                        "title": {"type": "string", "minLength": 1, "maxLength": 120},
                        "chain_id": {"$ref": "#/components/schemas/NullableChainId"},
                        "initial_len": {"type": "integer", "format": "uint64", "minimum": 1},
                        "initial_head": {"$ref": "#/components/schemas/HashHex"},
                        "created_at_ms": {"type": "integer", "format": "uint64", "minimum": 0},
                        "initial_owner": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "operator_pk": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "hash": {"$ref": "#/components/schemas/HashHex"},
                        "sig": {"$ref": "#/components/schemas/SignatureHex"}
                    }
                },
                "NodeId": {"type": "string", "pattern": "^[0-9a-f]{32}$", "description": "Unique 128-bit node id in canonical lowercase hex."},
                "Position2": {
                    "type": "array", "minItems": 2, "maxItems": 2,
                    "prefixItems": [{"type": "number"}, {"type": "number"}], "items": false
                },
                "PortEndpoint": {
                    "type": "array", "minItems": 2, "maxItems": 2,
                    "prefixItems": [{"$ref": "#/components/schemas/NodeId"}, {"type": "integer", "minimum": 0, "maximum": 65535}],
                    "items": false,
                    "description": "[node id, zero-based port index]. Discover valid component ports with `mantis-cli catalog --json`; never guess them."
                },
                "ParamValue": {
                    "description": "Frozen externally-tagged persistent parameter value contract.",
                    "oneOf": [
                        {"type": "object", "additionalProperties": false, "required": ["Number"], "properties": {"Number": {"type": "number"}}},
                        {"type": "object", "additionalProperties": false, "required": ["Bool"], "properties": {"Bool": {"type": "boolean"}}},
                        {"type": "object", "additionalProperties": false, "required": ["Text"], "properties": {"Text": {"type": "string"}}}
                    ]
                },
                "GraphOp": {
                    "description": "The frozen MantisCAD edit contract. Read AGENT_PROTOCOL.md and run `mantis-cli catalog --json` to discover exact component names, ports, types, and examples before constructing operations.",
                    "oneOf": [
                        {"type": "object", "additionalProperties": false, "required": ["op", "id", "type_name", "pos"], "properties": {
                            "op": {"const": "AddNode"}, "id": {"$ref": "#/components/schemas/NodeId"},
                            "type_name": {"type": "string", "minLength": 1, "description": "Exact catalog component name."},
                            "pos": {"$ref": "#/components/schemas/Position2"}
                        }},
                        {"type": "object", "additionalProperties": false, "required": ["op", "id"], "properties": {
                            "op": {"const": "RemoveNode"}, "id": {"$ref": "#/components/schemas/NodeId"}
                        }},
                        {"type": "object", "additionalProperties": false, "required": ["op", "from", "to"], "properties": {
                            "op": {"const": "Connect"}, "from": {"$ref": "#/components/schemas/PortEndpoint"}, "to": {"$ref": "#/components/schemas/PortEndpoint"}
                        }},
                        {"type": "object", "additionalProperties": false, "required": ["op", "from", "to"], "properties": {
                            "op": {"const": "Disconnect"}, "from": {"$ref": "#/components/schemas/PortEndpoint"}, "to": {"$ref": "#/components/schemas/PortEndpoint"}
                        }},
                        {"type": "object", "additionalProperties": false, "required": ["op", "id", "key", "value"], "properties": {
                            "op": {"const": "SetParam"}, "id": {"$ref": "#/components/schemas/NodeId"},
                            "key": {"type": "string", "minLength": 1}, "value": {"$ref": "#/components/schemas/ParamValue"}
                        }},
                        {"type": "object", "additionalProperties": false, "required": ["op", "id", "pos"], "properties": {
                            "op": {"const": "MoveNode"}, "id": {"$ref": "#/components/schemas/NodeId"}, "pos": {"$ref": "#/components/schemas/Position2"}
                        }}
                    ]
                },
                "Block": {
                    "type": "object", "additionalProperties": false,
                    "required": ["index", "prev_hash", "timestamp_ms", "author", "author_pk", "message", "ops", "hash", "sig"],
                    "properties": {
                        "index": {"type": "integer", "format": "uint64", "minimum": 0},
                        "prev_hash": {"$ref": "#/components/schemas/HashHex"},
                        "timestamp_ms": {"type": "integer", "format": "uint64", "minimum": 0},
                        "author": {"type": "string"},
                        "author_pk": {"$ref": "#/components/schemas/BlockAuthorKey"},
                        "message": {"type": "string"},
                        "ops": {"type": "array", "items": {"$ref": "#/components/schemas/GraphOp"}},
                        "hash": {"$ref": "#/components/schemas/HashHex"},
                        "sig": {"type": "string", "pattern": "^(?:|[0-9a-f]{128})$"}
                    },
                    "allOf": [{
                        "if": {"required": ["index"], "properties": {"index": {"const": 0}}},
                        "then": {"properties": {"author_pk": {"const": ""}, "sig": {"const": ""}}},
                        "else": {"properties": {
                            "author_pk": {"$ref": "#/components/schemas/PublicKeyHex"},
                            "sig": {"$ref": "#/components/schemas/SignatureHex"}
                        }}
                    }]
                },
                "Chain": {
                    "type": "object", "additionalProperties": false, "required": ["blocks"],
                    "properties": {"blocks": {"type": "array", "minItems": 1, "items": {"$ref": "#/components/schemas/Block"}}}
                },
                "ProjectRoleV1": {"type": "string", "enum": ["owner", "writer"]},
                "AccessActionV1": {"oneOf": [
                    {"type": "object", "additionalProperties": false, "required": ["type", "public_key", "role"], "properties": {
                        "type": {"const": "grant"}, "public_key": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "role": {"$ref": "#/components/schemas/ProjectRoleV1"}, "label": {"anyOf": [{"type": "string", "minLength": 1, "maxLength": 80}, {"type": "null"}]}
                    }},
                    {"type": "object", "additionalProperties": false, "required": ["type", "public_key"], "properties": {
                        "type": {"const": "revoke"}, "public_key": {"$ref": "#/components/schemas/PublicKeyHex"}
                    }},
                    {"type": "object", "additionalProperties": false, "required": ["type", "title"], "properties": {
                        "type": {"const": "rename"}, "title": {"type": "string", "minLength": 1, "maxLength": 120}
                    }},
                    {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {"type": {"const": "archive"}}},
                    {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {"type": {"const": "unarchive"}}}
                ]},
                "AccessRecordV1": {
                    "type": "object", "additionalProperties": false,
                    "required": ["schema_version", "index", "project_id", "chain_id", "genesis_hash", "prev_hash", "timestamp_ms", "actor_pk", "action", "hash", "sig"],
                    "properties": {
                        "schema_version": {"type": "integer", "const": 1},
                        "index": {"type": "integer", "format": "uint64", "minimum": 0},
                        "project_id": {"$ref": "#/components/schemas/ProjectSlug"},
                        "chain_id": {"$ref": "#/components/schemas/NullableChainId"},
                        "genesis_hash": {"$ref": "#/components/schemas/HashHex"},
                        "prev_hash": {"$ref": "#/components/schemas/HashHex"},
                        "timestamp_ms": {"type": "integer", "format": "uint64", "minimum": 0},
                        "actor_pk": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "action": {"$ref": "#/components/schemas/AccessActionV1"},
                        "hash": {"$ref": "#/components/schemas/HashHex"},
                        "sig": {"$ref": "#/components/schemas/SignatureHex"}
                    }
                },
                "AccessMemberV1": {
                    "type": "object", "additionalProperties": false,
                    "required": ["public_key", "role", "updated_at_ms", "updated_by"],
                    "properties": {
                        "public_key": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "role": {"$ref": "#/components/schemas/ProjectRoleV1"},
                        "label": {"anyOf": [{"type": "string", "minLength": 1, "maxLength": 80}, {"type": "null"}]},
                        "updated_at_ms": {"type": "integer", "format": "uint64", "minimum": 0},
                        "updated_by": {"$ref": "#/components/schemas/PublicKeyHex"}
                    }
                },
                "AccessStateV1": {
                    "type": "object", "additionalProperties": false, "required": ["len", "head", "members", "title", "archived"],
                    "properties": {
                        "len": {"type": "integer", "format": "uint64", "minimum": 1},
                        "head": {"$ref": "#/components/schemas/HashHex"},
                        "members": {"type": "array", "items": {"$ref": "#/components/schemas/AccessMemberV1"}},
                        "title": {"type": "string", "minLength": 1, "maxLength": 120},
                        "archived": {"type": "boolean"}
                    }
                },
                "ProjectInfoV2": {
                    "type": "object", "additionalProperties": false, "required": ["manifest", "state", "access"],
                    "properties": {
                        "manifest": {"$ref": "#/components/schemas/ProjectManifestV1"},
                        "state": {"$ref": "#/components/schemas/ChainStateV1"},
                        "access": {"$ref": "#/components/schemas/AccessStateV1"}
                    }
                },
                "ProjectSummaryV1": {
                    "type": "object", "additionalProperties": false,
                    "required": ["project_id", "title", "archived", "chain_format_version", "chain_id", "genesis_hash", "state"],
                    "properties": {
                        "project_id": {"$ref": "#/components/schemas/ProjectSlug"},
                        "title": {"type": "string"}, "archived": {"type": "boolean"},
                        "chain_format_version": {"type": "integer", "enum": [1, 2]},
                        "chain_id": {"$ref": "#/components/schemas/NullableChainId"},
                        "genesis_hash": {"$ref": "#/components/schemas/HashHex"},
                        "state": {"$ref": "#/components/schemas/ChainStateV1"}
                    }
                },
                "ProjectBootstrapV1": {
                    "type": "object", "additionalProperties": false, "required": ["create", "manifest", "chain", "access_log"],
                    "properties": {
                        "create": {"$ref": "#/components/schemas/ProjectCreateV1"},
                        "manifest": {"$ref": "#/components/schemas/ProjectManifestV1"},
                        "chain": {"$ref": "#/components/schemas/Chain"},
                        "access_log": {"type": "array", "minItems": 1, "items": {"$ref": "#/components/schemas/AccessRecordV1"}}
                    }
                },
                "BlocksPageV2": {
                    "type": "object", "additionalProperties": false, "required": ["project_id", "from", "blocks", "next_from", "state"],
                    "properties": {
                        "project_id": {"$ref": "#/components/schemas/ProjectSlug"},
                        "from": {"type": "integer", "format": "uint64", "minimum": 0},
                        "blocks": {"type": "array", "items": {"$ref": "#/components/schemas/Block"}},
                        "next_from": {"type": ["integer", "null"], "format": "uint64", "minimum": 0},
                        "state": {"$ref": "#/components/schemas/ChainStateV1"}
                    }
                },
                "PushRequestV2": {
                    "type": "object", "additionalProperties": false, "required": ["base_len", "base_head", "blocks"],
                    "properties": {
                        "base_len": {"type": "integer", "format": "uint64", "minimum": 1},
                        "base_head": {"$ref": "#/components/schemas/HashHex"},
                        "blocks": {"type": "array", "minItems": 1, "maxItems": projects::MAX_PUSH_BLOCKS, "items": {"$ref": "#/components/schemas/Block"}}
                    }
                },
                "PushResponseV2": {
                    "type": "object", "additionalProperties": false, "required": ["len", "head", "appended"],
                    "properties": {
                        "len": {"type": "integer", "format": "uint64", "minimum": 1},
                        "head": {"$ref": "#/components/schemas/HashHex"},
                        "appended": {"type": "integer", "format": "uint64", "minimum": 1}
                    }
                },
                "AccessPageV1": {
                    "type": "object", "additionalProperties": false, "required": ["project_id", "from", "records", "next_from", "state"],
                    "properties": {
                        "project_id": {"$ref": "#/components/schemas/ProjectSlug"},
                        "from": {"type": "integer", "format": "uint64", "minimum": 0},
                        "records": {"type": "array", "items": {"$ref": "#/components/schemas/AccessRecordV1"}},
                        "next_from": {"type": ["integer", "null"], "format": "uint64", "minimum": 0},
                        "state": {"$ref": "#/components/schemas/AccessStateV1"}
                    }
                },
                "AuthorActivity": {
                    "type": "object", "required": ["public_key", "names", "block_count", "operation_count", "first_block", "last_block"],
                    "properties": {
                        "public_key": {"$ref": "#/components/schemas/PublicKeyHex"},
                        "names": {"type": "array", "items": {"type": "string"}},
                        "block_count": {"type": "integer", "minimum": 1},
                        "operation_count": {"type": "integer", "minimum": 0},
                        "first_block": {"type": "integer", "format": "uint64", "minimum": 1},
                        "last_block": {"type": "integer", "format": "uint64", "minimum": 1}
                    }
                },
                "ChainAudit": {
                    "type": "object", "required": ["format_version", "chain_id", "genesis_hash", "head_hash", "block_count", "signed_block_count", "operation_count", "byte_size", "authors"],
                    "properties": {
                        "format_version": {"type": "integer", "enum": [1, 2]},
                        "chain_id": {"$ref": "#/components/schemas/NullableChainId"},
                        "genesis_hash": {"$ref": "#/components/schemas/HashHex"},
                        "head_hash": {"$ref": "#/components/schemas/HashHex"},
                        "block_count": {"type": "integer", "minimum": 1},
                        "signed_block_count": {"type": "integer", "minimum": 0},
                        "operation_count": {"type": "integer", "minimum": 0},
                        "byte_size": {"type": "integer", "minimum": 1},
                        "authors": {"type": "array", "items": {"$ref": "#/components/schemas/AuthorActivity"}}
                    }
                },
                "ErrorDetailV1": {
                    "type": "object", "additionalProperties": false, "required": ["code", "message"],
                    "properties": {
                        "code": {"type": "string"}, "message": {"type": "string"},
                        "project": {"anyOf": [{"$ref": "#/components/schemas/ProjectSlug"}, {"type": "null"}]},
                        "block": {"anyOf": [{"type": "integer", "format": "uint64", "minimum": 0}, {"type": "null"}]},
                        "op": {"anyOf": [{"type": "integer", "format": "uint64", "minimum": 0}, {"type": "null"}]},
                        "access_record": {"anyOf": [{"type": "integer", "format": "uint64", "minimum": 0}, {"type": "null"}]}
                    }
                },
                "ErrorResponseV1": {
                    "type": "object", "additionalProperties": false, "required": ["error"],
                    "properties": {
                        "error": {"$ref": "#/components/schemas/ErrorDetailV1"},
                        "len": {"anyOf": [{"type": "integer", "format": "uint64", "minimum": 1}, {"type": "null"}]},
                        "head": {"anyOf": [{"$ref": "#/components/schemas/HashHex"}, {"type": "null"}]}
                    }
                }
            }
        }
    });
    if !public_base_path.is_empty() {
        document
            .as_object_mut()
            .expect("OpenAPI document is an object")
            .insert(
                "servers".to_string(),
                serde_json::json!([{"url": public_base_path}]),
            );
    }
    document
}

fn default_project() -> mantis_protocol::ProjectSlug {
    mantis_protocol::ProjectSlug::new("default").expect("static default project slug")
}

fn with_chain_state_headers(
    mut response: Response<Cursor<Vec<u8>>>,
    state: &mantis_protocol::ChainStateV1,
) -> Response<Cursor<Vec<u8>>> {
    for (key, value) in [
        ("X-Mantis-Chain-Length", state.len.to_string()),
        ("X-Mantis-Head", state.head.to_string()),
    ] {
        if let Some(header) = hdr(key, &value) {
            response = response.with_header(header);
        }
    }
    response
}

fn handle_v2(
    mut request: Request,
    registry: &projects::ProjectRegistry,
    dist: Option<&Path>,
    allowed_origins: &[String],
    public_base_path: &str,
) {
    let origin = match cors_origin(&request, allowed_origins) {
        Ok(origin) => origin,
        Err(error) => {
            let response = v2_error(error, None);
            let _ = request.respond(response);
            return;
        }
    };
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path.to_string(), Some(query.to_string())),
        None => (url, None),
    };
    let method = request.method().clone();

    let response = if method == Method::Options {
        let mut response = Response::from_string(String::new()).with_status_code(StatusCode(204));
        for (key, value) in [
            ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
            ("Access-Control-Allow-Headers", "content-type"),
            ("Access-Control-Max-Age", "600"),
        ] {
            if let Some(header) = hdr(key, value) {
                response = response.with_header(header);
            }
        }
        if let Some(origin) = origin.as_deref() {
            if let Some(header) = hdr("Access-Control-Allow-Origin", origin) {
                response = response.with_header(header);
            }
            if let Some(header) = hdr("Vary", "Origin") {
                response = response.with_header(header);
            }
        }
        with_security_headers(response)
    } else {
        let result: Result<Response<Cursor<Vec<u8>>>, projects::ProjectError> =
            (|| match (method.clone(), path.as_str()) {
                (Method::Get, "/healthz") => Ok(v2_json(
                    200,
                    &serde_json::json!({"status":"ok"}),
                    origin.as_deref(),
                )),
                (Method::Get, "/readyz") => {
                    let ready = registry.is_ready();
                    Ok(v2_json(
                        if ready { 200 } else { 503 },
                        &serde_json::json!({
                            "status": if ready { "ready" } else { "not_ready" }
                        }),
                        origin.as_deref(),
                    ))
                }
                (Method::Get, "/api/v2/info") => {
                    Ok(v2_json(200, &registry.api_info(), origin.as_deref()))
                }
                (Method::Get, "/api/v2/openapi.json") => Ok(v2_json(
                    200,
                    &openapi_v2(public_base_path),
                    origin.as_deref(),
                )),
                (Method::Get, "/api/v2/projects") => {
                    let include_archived = parse_include_archived(query.as_deref())?;
                    Ok(v2_json(
                        200,
                        &registry.summaries(include_archived)?,
                        origin.as_deref(),
                    ))
                }
                (Method::Post, "/api/v2/projects") => {
                    let bootstrap =
                        read_json_body::<mantis_protocol::ProjectBootstrapV1>(&mut request)?;
                    Ok(v2_json(
                        201,
                        &registry.create(bootstrap)?,
                        origin.as_deref(),
                    ))
                }
                (Method::Get, "/api/info") => {
                    let info = registry.info(&default_project())?;
                    Ok(v2_json(
                        200,
                        &serde_json::json!({
                            "api_version": 1,
                            "chain_format_version": info.manifest.chain_format_version,
                            "len": info.state.len,
                            "head": info.state.head,
                            "genesis": info.state.genesis,
                            "total_ops": info.state.total_ops,
                        }),
                        origin.as_deref(),
                    ))
                }
                (Method::Get, "/api/audit") => Ok(v2_json(
                    200,
                    &registry.audit(&default_project())?,
                    origin.as_deref(),
                )),
                (Method::Get, "/api/blocks") => {
                    let page = parse_blocks_query(query.as_deref())
                        .map_err(|error| projects::ProjectError::new(400, "bad_query", error))?;
                    let (blocks, state) =
                        registry.raw_blocks(&default_project(), page.from, page.limit)?;
                    Ok(with_chain_state_headers(
                        v2_json(200, &blocks, origin.as_deref()),
                        &state,
                    ))
                }
                (Method::Post, "/api/blocks") => {
                    let blocks = read_json_body::<Vec<mantis_chain::Block>>(&mut request)?;
                    let info = registry.info(&default_project())?;
                    let push = mantis_protocol::PushRequestV2 {
                        base_len: info.state.len,
                        base_head: info.state.head,
                        blocks,
                    };
                    Ok(v2_json(
                        200,
                        &registry.push(&default_project(), push)?,
                        origin.as_deref(),
                    ))
                }
                _ => {
                    let Some((project, resource)) = parse_project_path(&path)? else {
                        if method == Method::Get {
                            return Ok(match dist {
                                Some(dist) => serve_static_same_origin(dist, &path),
                                None => same_origin_error_json(404, "not_found", "not found"),
                            });
                        }
                        return Err(projects::ProjectError::new(404, "not_found", "not found"));
                    };
                    match (method, resource) {
                        (Method::Get, "info") => {
                            Ok(v2_json(200, &registry.info(&project)?, origin.as_deref()))
                        }
                        (Method::Get, "create") => Ok(v2_json(
                            200,
                            &registry.create_proof(&project)?,
                            origin.as_deref(),
                        )),
                        (Method::Get, "audit") => {
                            Ok(v2_json(200, &registry.audit(&project)?, origin.as_deref()))
                        }
                        (Method::Get, "blocks") => {
                            let (from, limit) = parse_page_query(query.as_deref())?;
                            Ok(v2_json(
                                200,
                                &registry.blocks(&project, from, limit)?,
                                origin.as_deref(),
                            ))
                        }
                        (Method::Post, "blocks") => {
                            let push =
                                read_json_body::<mantis_protocol::PushRequestV2>(&mut request)?;
                            Ok(v2_json(
                                200,
                                &registry.push(&project, push)?,
                                origin.as_deref(),
                            ))
                        }
                        (Method::Get, "access-log") => {
                            let (from, limit) = parse_page_query(query.as_deref())?;
                            Ok(v2_json(
                                200,
                                &registry.access_records(&project, from, limit)?,
                                origin.as_deref(),
                            ))
                        }
                        (Method::Post, "access-log") => {
                            let records = read_json_body::<Vec<mantis_protocol::AccessRecordV1>>(
                                &mut request,
                            )?;
                            Ok(v2_json(
                                200,
                                &registry.append_access(&project, records)?,
                                origin.as_deref(),
                            ))
                        }
                        _ => Err(projects::ProjectError::new(
                            405,
                            "method_not_allowed",
                            "method not allowed for endpoint",
                        )
                        .for_project(&project)),
                    }
                }
            })();
        match result {
            Ok(response) => response,
            Err(error) => v2_error(error, origin.as_deref()),
        }
    };

    if let Err(error) = request.respond(response) {
        eprintln!("mantis-server: failed to send response: {error}");
    }
}

fn run_v2(
    server: Server,
    registry: Arc<projects::ProjectRegistry>,
    dist: Option<PathBuf>,
    allowed_origins: Vec<String>,
    public_base_path: String,
) {
    let (sender, receiver) = mpsc::sync_channel::<Request>(128);
    let receiver = Arc::new(Mutex::new(receiver));
    for worker in 0..4 {
        let receiver = Arc::clone(&receiver);
        let registry = Arc::clone(&registry);
        let dist = dist.clone();
        let allowed_origins = allowed_origins.clone();
        let public_base_path = public_base_path.clone();
        std::thread::Builder::new()
            .name(format!("mantis-http-{worker}"))
            .spawn(move || loop {
                let request = {
                    let receiver = receiver.lock().unwrap_or_else(|error| error.into_inner());
                    receiver.recv()
                };
                match request {
                    Ok(request) => handle_v2(
                        request,
                        &registry,
                        dist.as_deref(),
                        &allowed_origins,
                        &public_base_path,
                    ),
                    Err(_) => break,
                }
            })
            .expect("spawn HTTP worker");
    }
    for request in server.incoming_requests() {
        if sender.send(request).is_err() {
            break;
        }
    }
}

fn main() {
    let base = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("mantis-server: {message}");
            std::process::exit(2);
        }
    };
    let cfg = match parse_args_from(base, std::env::args().skip(1)) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
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
    if let Some(data_dir) = cfg.data_dir.clone() {
        let registry = match projects::ProjectRegistry::open(
            data_dir.clone(),
            &cfg.operator_keys,
            cfg.max_project_bytes,
        ) {
            Ok(registry) => Arc::new(registry),
            Err(error) => {
                eprintln!(
                    "mantis-server: cannot open multi-project store {}: {}",
                    data_dir.display(),
                    error.message
                );
                std::process::exit(1);
            }
        };
        let project_count = registry
            .summaries(true)
            .map(|items| items.len())
            .unwrap_or(0);
        println!(
            "mantis-server listening on http://{addr} — data {} ({project_count} projects), dist {}",
            data_dir.display(),
            cfg.dist
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(none)".to_string()),
        );
        run_v2(
            server,
            registry,
            cfg.dist,
            cfg.allowed_origins,
            cfg.public_base_path,
        );
        return;
    }

    let chain = match load_chain(&cfg.chain_path) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("mantis-server: {msg}");
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
    run(
        server,
        Arc::new(Mutex::new(chain)),
        cfg.chain_path,
        cfg.dist,
    );
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::{Block, ChainAudit, Identity};
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
        start_at(chain, dist, temp_path("chain.json"))
    }

    fn start_at(
        chain: Chain,
        dist: Option<PathBuf>,
        chain_path: PathBuf,
    ) -> (SocketAddr, Arc<Mutex<Chain>>, PathBuf) {
        let server = Server::http("127.0.0.1:0").expect("bind test server");
        let addr = server.server_addr().to_ip().expect("ip listener");
        let state = Arc::new(Mutex::new(chain));
        let (st, cp, d) = (state.clone(), chain_path.clone(), dist);
        std::thread::spawn(move || run(server, st, cp, d));
        (addr, state, chain_path)
    }

    fn start_v2_test(
        operator_keys: Vec<String>,
        allowed_origins: Vec<String>,
    ) -> (SocketAddr, PathBuf) {
        start_v2_test_at(operator_keys, allowed_origins, "")
    }

    fn start_v2_test_at(
        operator_keys: Vec<String>,
        allowed_origins: Vec<String>,
        public_base_path: &str,
    ) -> (SocketAddr, PathBuf) {
        let data_dir = temp_path("projects");
        let registry = Arc::new(
            projects::ProjectRegistry::open(
                data_dir.clone(),
                &operator_keys,
                projects::DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap(),
        );
        let server = Server::http("127.0.0.1:0").expect("bind v2 test server");
        let addr = server.server_addr().to_ip().expect("ip listener");
        let public_base_path = public_base_path.to_string();
        std::thread::spawn(move || {
            run_v2(server, registry, None, allowed_origins, public_base_path)
        });
        (addr, data_dir)
    }

    /// Raw HTTP round-trip over TcpStream: returns (status, headers, body).
    fn http(addr: SocketAddr, raw: &str) -> (u16, String, String) {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream.write_all(raw.as_bytes()).expect("send");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read");
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
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

    fn response_header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
        headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn csp_directive<'a>(csp: &'a str, name: &str) -> Option<&'a str> {
        csp.split(';').map(str::trim).find(|directive| {
            directive
                .split_whitespace()
                .next()
                .is_some_and(|key| key.eq_ignore_ascii_case(name))
        })
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
        assert!(cfg.data_dir.is_none());
        assert!(cfg.dist.is_none());
        assert!(cfg.public_base_path.is_empty());

        let cfg = parse_args(
            [
                "--port",
                "9000",
                "--chain",
                "/tmp/c.json",
                "--dist",
                "web",
                "--public-base-path",
                "/mantis",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .unwrap();
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.chain_path, PathBuf::from("/tmp/c.json"));
        assert_eq!(cfg.dist, Some(PathBuf::from("web")));
        assert_eq!(cfg.public_base_path, "/mantis");

        let cfg = parse_args(
            ["--data-dir", "/var/data", "--port", "8080"]
                .iter()
                .map(|s| s.to_string()),
        )
        .unwrap();
        assert_eq!(cfg.data_dir, Some(PathBuf::from("/var/data")));
        assert_eq!(cfg.port, 8080);

        assert_eq!(csv_values(" one, two ,,three "), ["one", "two", "three"]);

        assert!(parse_args(["--port"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["--port", "banana"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["--public-base-path"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["--wat"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn public_base_path_requires_a_canonical_safe_prefix() {
        for valid in ["", "/mantis", "/team/mantis-v1", "/a.b_c~d"] {
            assert_eq!(parse_public_base_path(valid).unwrap(), valid);
        }
        for invalid in [
            "/",
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
            "/한글",
        ] {
            assert!(
                parse_public_base_path(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn blocks_query_is_strict_and_supports_bounded_pages() {
        assert_eq!(
            parse_blocks_query(None).unwrap(),
            BlocksQuery {
                from: 0,
                limit: None
            }
        );
        assert_eq!(
            parse_blocks_query(Some("from=3&limit=7")).unwrap(),
            BlocksQuery {
                from: 3,
                limit: Some(7)
            }
        );
        assert_eq!(parse_blocks_query(Some("")).unwrap().from, 0);
        for bad in [
            "from=banana",
            "from=-1",
            "limit=0",
            "limit=999999",
            "x=1",
            "from=1&from=2",
            "limit",
        ] {
            assert!(parse_blocks_query(Some(bad)).is_err(), "accepted {bad}");
        }
    }

    // -- API ------------------------------------------------------------------

    #[test]
    fn info_reports_genesis() {
        let (addr, _, _) = start(Chain::new(), None);
        let (status, head, body) = get(addr, "/api/info");
        assert_eq!(status, 200, "{body}");
        assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
        assert!(
            head.to_lowercase()
                .contains("content-type: application/json"),
            "{head}"
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["api_version"], 1);
        assert_eq!(
            v["chain_format_version"],
            mantis_chain::LEGACY_CHAIN_FORMAT_VERSION
        );
        assert_eq!(v["len"], 1);
        assert_eq!(v["head"], Chain::new().head().hash);
        assert_eq!(v["genesis"], Chain::new().head().hash);
        assert_eq!(v["total_ops"], 0);
    }

    #[test]
    fn health_and_readiness_report_valid_chain() {
        let (addr, _, _) = start(Chain::new(), None);
        let (status, _, body) = get(addr, "/healthz");
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["status"],
            "ok"
        );

        let (status, _, body) = get(addr, "/readyz");
        assert_eq!(status, 200, "{body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["status"], "ready");
        assert_eq!(value["head"], Chain::new().head().hash);
    }

    #[test]
    fn v2_project_creation_list_and_same_origin_policy() {
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let bootstrap = mantis_protocol::ProjectBootstrapV1::new_signed(
            mantis_protocol::ProjectSlug::new("http-demo").unwrap(),
            "HTTP Demo",
            mantis_protocol::ChainId::new("56".repeat(32)).unwrap(),
            mantis_protocol::PublicKeyHex::new(owner.public_hex()).unwrap(),
            1_000,
            &operator,
        )
        .unwrap();
        let (addr, data_dir) = start_v2_test(
            vec![operator.public_hex()],
            vec!["https://editor.example".into()],
        );

        let body = serde_json::to_string(&bootstrap).unwrap();
        let (status, headers, response) = post(addr, "/api/v2/projects", &body);
        assert_eq!(status, 201, "{response}");
        assert!(
            !headers.contains("Access-Control-Allow-Origin: *"),
            "{headers}"
        );

        let (status, _, response) = get(addr, "/api/v2/projects");
        assert_eq!(status, 200, "{response}");
        let projects: Vec<mantis_protocol::ProjectSummaryV1> =
            serde_json::from_str(&response).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project_id.as_str(), "http-demo");

        let (status, headers, response) = http(
            addr,
            "GET /api/v2/info HTTP/1.1\r\nHost: localhost\r\nOrigin: https://editor.example\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 200, "{response}");
        assert!(
            headers.contains("Access-Control-Allow-Origin: https://editor.example"),
            "{headers}"
        );

        let (status, headers, _) = http(
            addr,
            "GET /api/v2/info HTTP/1.1\r\nHost: localhost\r\nOrigin: https://evil.example\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 403);
        assert!(!headers.contains("Access-Control-Allow-Origin"));
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn v2_openapi_route_has_complete_operations_and_core_schemas() {
        let (addr, data_dir) = start_v2_test(vec![], vec![]);
        let (status, _, body) = get(addr, "/api/v2/openapi.json");
        assert_eq!(status, 200, "{body}");
        let document: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert!(document.get("servers").is_none());

        let paths = document["paths"].as_object().expect("OpenAPI paths");
        for required in [
            "/api/v2/info",
            "/api/v2/projects",
            "/api/v2/projects/{project}/blocks",
            "/api/v2/projects/{project}/access-log",
        ] {
            assert!(
                paths.contains_key(required),
                "missing OpenAPI path {required}"
            );
        }
        for (path, item) in paths {
            for method in ["get", "post"] {
                if let Some(operation) = item.get(method) {
                    assert!(
                        operation
                            .get("responses")
                            .and_then(serde_json::Value::as_object)
                            .is_some_and(|responses| !responses.is_empty()),
                        "{method} {path} has no responses"
                    );
                }
            }
        }
        assert!(
            document["paths"]["/api/v2/projects"]["post"]["requestBody"]["required"]
                .as_bool()
                .unwrap()
        );
        for schema in [
            "ProjectBootstrapV1",
            "ProjectInfoV2",
            "PushRequestV2",
            "AccessRecordV1",
            "GraphOp",
            "NodeId",
            "ParamValue",
            "ErrorResponseV1",
        ] {
            assert!(
                document["components"]["schemas"].get(schema).is_some(),
                "missing OpenAPI schema {schema}"
            );
        }
        assert_eq!(
            document["components"]["schemas"]["GraphOp"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
        assert_eq!(
            document["components"]["schemas"]["ParamValue"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(document["components"]["schemas"]["GraphOp"]["description"]
            .as_str()
            .unwrap()
            .contains("mantis-cli catalog --json"));
        let block_key_variants = document["components"]["schemas"]["BlockAuthorKey"]["anyOf"]
            .as_array()
            .unwrap();
        assert!(
            block_key_variants
                .iter()
                .any(|variant| variant.get("const")
                    == Some(&serde_json::Value::String(String::new())))
        );
        let genesis = serde_json::to_value(&Chain::new().blocks[0]).unwrap();
        assert_eq!(genesis["author_pk"], "");
        let (_, signed, _) = signed_extension();
        let signed_key = signed.blocks[1].author_pk.as_str();
        assert!(mantis_protocol::PublicKeyHex::new(signed_key).is_ok());

        for schema in [
            &document["components"]["schemas"]["AccessActionV1"]["oneOf"][0]["properties"]["label"],
            &document["components"]["schemas"]["AccessMemberV1"]["properties"]["label"],
            &document["components"]["schemas"]["ErrorDetailV1"]["properties"]["project"],
            &document["components"]["schemas"]["ErrorResponseV1"]["properties"]["head"],
        ] {
            assert!(schema["anyOf"]
                .as_array()
                .is_some_and(|variants| variants.iter().any(|variant| variant["type"] == "null")));
        }

        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let bootstrap = mantis_protocol::ProjectBootstrapV1::new_signed(
            mantis_protocol::ProjectSlug::new("schema-contract").unwrap(),
            "Schema Contract",
            mantis_protocol::ChainId::new("78".repeat(32)).unwrap(),
            mantis_protocol::PublicKeyHex::new(owner.public_hex()).unwrap(),
            1,
            &operator,
        )
        .unwrap();
        let mut initial_action = serde_json::to_value(&bootstrap.access_log[0].action).unwrap();
        assert!(initial_action.get("label").is_none());
        initial_action["label"] = serde_json::Value::Null;
        serde_json::from_value::<mantis_protocol::AccessActionV1>(initial_action).unwrap();
        let generic_error = serde_json::to_value(
            projects::ProjectError::new(400, "bad_request", "bad request").response(),
        )
        .unwrap();
        assert!(generic_error["error"].get("project").is_none());
        assert!(generic_error.get("head").is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn v2_openapi_advertises_configured_public_base_path() {
        let (addr, data_dir) = start_v2_test_at(vec![], vec![], "/mantis");
        let (status, _, body) = get(addr, "/api/v2/openapi.json");
        assert_eq!(status, 200, "{body}");
        let document: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(document["servers"], serde_json::json!([{"url": "/mantis"}]));
        assert!(document["paths"].get("/api/v2/info").is_some());
        assert!(document["paths"].get("/mantis/api/v2/info").is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn audit_endpoint_returns_validated_provenance_checkpoint() {
        let (_, pushed, _) = signed_extension();
        let (addr, _, _) = start(pushed.clone(), None);
        let (status, _, body) = get(addr, "/api/audit");
        assert_eq!(status, 200, "{body}");
        let audit: ChainAudit = serde_json::from_str(&body).unwrap();
        assert_eq!(audit, pushed.audit().unwrap());
        assert_eq!(audit.head_hash, pushed.head().hash);
        assert_eq!(audit.authors.len(), 1);
        assert_eq!(audit.authors[0].names, ["alice"]);
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

        // Even if the backing file disappears at runtime, an idempotent
        // re-push repairs it before returning 200 appended 0.
        std::fs::remove_file(&chain_path).unwrap();
        let (status, _, body) = post(addr, "/api/blocks", &body_json);
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["len"], 2);
        assert_eq!(v["appended"], 0);
        assert_eq!(load_chain(&chain_path).unwrap(), pushed);

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
        assert_eq!(v["error"]["code"], "diverged");
        assert_eq!(v["error"]["block"], 1);
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

        // Optional bounded pages retain the plain-array response shape and
        // expose deterministic cursor/head metadata in readable CORS headers.
        let (status, headers, body) = get(addr, "/api/blocks?from=0&limit=1");
        assert_eq!(status, 200);
        let got: Vec<Block> = serde_json::from_str(&body).unwrap();
        assert_eq!(got, pushed.blocks[..1]);
        assert!(headers.contains("X-Mantis-Chain-Length: 2"), "{headers}");
        assert!(headers.contains("X-Mantis-From: 0"), "{headers}");
        assert!(headers.contains("X-Mantis-Next-From: 1"), "{headers}");
        assert!(
            headers.contains(&format!("X-Mantis-Head: {}", pushed.head().hash)),
            "{headers}"
        );

        // from=0 / missing -> whole chain for backwards-compatible GUI sync
        for q in ["/api/blocks?from=0", "/api/blocks"] {
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

        // Malformed cursors are never reinterpreted as a full-history pull.
        for q in [
            "/api/blocks?from=x",
            "/api/blocks?limit=0",
            "/api/blocks?unknown=1",
        ] {
            let (status, _, body) = get(addr, q);
            assert_eq!(status, 400, "{q}: {body}");
            assert!(body.contains("error"), "{body}");
        }

        let _ = std::fs::remove_file(&chain_path);
    }

    #[test]
    fn garbage_post_is_400() {
        let (addr, state, _) = start(Chain::new(), None);
        let (status, _, body) = post(addr, "/api/blocks", "{not json");
        assert_eq!(status, 400, "{body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"]["code"], "bad_request");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bad block JSON"));
        // an object instead of an array is also garbage
        let (status, _, _) = post(addr, "/api/blocks", "{}");
        assert_eq!(status, 400);
        assert_eq!(state.lock().unwrap().len(), 1);
    }

    #[test]
    fn tampered_block_is_structured_422_not_a_retryable_conflict() {
        let (_, _, mut tail) = signed_extension();
        tail[0].message = "tampered".into();
        let (addr, state, _) = start(Chain::new(), None);
        let (status, _, body) = post(addr, "/api/blocks", &serde_json::to_string(&tail).unwrap());
        assert_eq!(status, 422, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], "bad_hash");
        assert_eq!(v["error"]["block"], 1);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("content hash"));
        assert_eq!(v["len"], 1);
        assert_eq!(v["head"], Chain::new().head().hash);
        assert_eq!(state.lock().unwrap().len(), 1);
    }

    #[test]
    fn signed_but_unreplayable_block_reports_op_context() {
        let identity = Identity::generate("fallible-agent");
        let genesis = Chain::new();
        let mut block = Block {
            index: 1,
            prev_hash: genesis.head().hash.clone(),
            timestamp_ms: 1,
            author: identity.name.clone(),
            author_pk: identity.public_hex(),
            message: "references a node that does not exist".into(),
            ops: vec![GraphOp::SetParam {
                id: NodeId(0xdead),
                key: "value".into(),
                value: ParamValue::Number(1.0),
            }],
            hash: String::new(),
            sig: String::new(),
        };
        block.hash = block.compute_hash();
        block.sig = identity.sign_hash_hex(&block.hash);
        let (addr, state, _) = start(genesis.clone(), None);

        let (status, _, body) = post(
            addr,
            "/api/blocks",
            &serde_json::to_string(&[block]).unwrap(),
        );
        assert_eq!(status, 422, "{body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"]["code"], "bad_ops");
        assert_eq!(value["error"]["block"], 1);
        assert_eq!(value["error"]["op"], 0);
        assert_eq!(*state.lock().unwrap(), genesis);
    }

    #[test]
    fn persistence_failure_is_500_and_never_publishes_candidate() {
        let (_, _, tail) = signed_extension();
        let non_directory_parent = temp_path("not-a-directory");
        std::fs::write(&non_directory_parent, b"blocks child creation").unwrap();
        let chain_path = non_directory_parent.join("chain.json");
        let (addr, state, _) = start_at(Chain::new(), None, chain_path.clone());

        let (status, _, body) = post(addr, "/api/blocks", &serde_json::to_string(&tail).unwrap());
        assert_eq!(status, 500, "{body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"]["code"], "persistence_failed");
        assert_eq!(value["len"], 1);
        assert_eq!(*state.lock().unwrap(), Chain::new());
        assert!(!chain_path.exists());
        let _ = std::fs::remove_file(&non_directory_parent);
    }

    #[test]
    fn legacy_post_replace_failure_publishes_candidate_and_fail_stops() {
        let (_, pushed, tail) = signed_extension();
        let chain_path = temp_path("legacy-uncertain.json");
        let (addr, state, _) = start_at(Chain::new(), None, chain_path.clone());
        storage::fail_parent_sync_for_test(&chain_path);

        let body = serde_json::to_string(&tail).unwrap();
        let (status, _, response) = post(addr, "/api/blocks", &body);
        assert_eq!(status, 500, "{response}");
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "persistence_uncertain");
        assert_eq!(value["len"], 2);
        assert_eq!(*state.lock().unwrap(), pushed);
        assert_eq!(load_chain(&chain_path).unwrap(), pushed);

        let (status, _, response) = post(addr, "/api/blocks", &body);
        assert_eq!(status, 503, "{response}");
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "storage_not_ready");
        let (status, _, _) = get(addr, "/readyz");
        assert_eq!(status, 503);
        let _ = std::fs::remove_file(chain_path);
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
        // Known endpoints distinguish an unsupported method from a bad path.
        let (status, headers, body) = http(
            addr,
            "DELETE /api/blocks HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 405);
        assert!(headers.contains("Allow: GET, POST, OPTIONS"), "{headers}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"]["code"], "method_not_allowed");
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
    fn html_nonce_matches_csp_and_rotates_per_response() {
        let dist = make_dist();
        let template = format!(
            "<style>body{{}}</style>\
             <script type=\"module\" nonce=\"{CSP_NONCE_PLACEHOLDER}\">\
             console.log('mantis')</script>"
        );
        std::fs::write(dist.join("index.html"), template).unwrap();
        let (addr, _, _) = start(Chain::new(), Some(dist.clone()));

        let mut nonces = Vec::new();
        for _ in 0..2 {
            let (status, headers, body) = get(addr, "/");
            assert_eq!(status, 200, "{headers}\n{body}");
            assert!(!body.contains(CSP_NONCE_PLACEHOLDER), "{body}");

            let nonce = body
                .split_once("nonce=\"")
                .and_then(|(_, value)| value.split_once('"').map(|(nonce, _)| nonce))
                .expect("HTML nonce");
            assert_eq!(body.matches(nonce).count(), 1, "{body}");
            assert_eq!(STANDARD.decode(nonce).unwrap().len(), 16);

            let csp = response_header(&headers, "Content-Security-Policy")
                .expect("Content-Security-Policy header");
            let script_src = csp_directive(csp, "script-src").expect("script-src directive");
            assert!(
                script_src.contains(&format!(
                    "script-src 'self' 'wasm-unsafe-eval' 'nonce-{nonce}'"
                )),
                "{script_src}"
            );
            assert!(!script_src.contains("'unsafe-inline'"), "{script_src}");
            assert_eq!(
                csp_directive(csp, "style-src"),
                Some("style-src 'self' 'unsafe-inline'")
            );
            nonces.push(nonce.to_string());
        }
        assert_ne!(nonces[0], nonces[1], "nonce must rotate per response");

        let _ = std::fs::remove_dir_all(&dist);
    }

    #[test]
    fn invalid_utf8_html_fails_closed() {
        let dist = make_dist();
        std::fs::write(dist.join("broken.html"), [0xff, 0xfe]).unwrap();
        let (addr, _, _) = start(Chain::new(), Some(dist.clone()));

        let (status, headers, body) = get(addr, "/broken.html");
        assert_eq!(status, 500, "{headers}\n{body}");
        assert!(body.contains("static HTML is not valid UTF-8"), "{body}");
        let csp = response_header(&headers, "Content-Security-Policy")
            .expect("Content-Security-Policy header");
        let script_src = csp_directive(csp, "script-src").expect("script-src directive");
        assert!(!script_src.contains("'unsafe-inline'"), "{script_src}");

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
            assert!((400..500).contains(&status), "{path} -> {status} {body}");
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
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("add slider", "EVIL");
        std::fs::write(&path, text).unwrap();
        assert!(load_chain(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
