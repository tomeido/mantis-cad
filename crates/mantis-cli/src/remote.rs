//! Bounded, project-scoped sync of already signed chain documents.
//!
//! A pull freezes and verifies the remote checkpoint before replacing a file.
//! A push proves the remote history is a prefix, then publishes bounded CAS
//! batches. Conflicts are returned to the caller without an automatic retry.

use crate::{agent::write_chain_atomic, local_io::DocumentLocks, CliError};
use mantis_chain::Chain;
use mantis_protocol::{
    BlocksPageV2, ChainStateV1, HashHex, ProjectInfoV2, ProjectSlug, PushRequestV2, PushResponseV2,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{io::Read, path::Path, time::Duration};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const PAGE_BLOCKS: usize = 256;
const MAX_PUSH_OPS: usize = 50_000;

struct Options<'a> {
    file: &'a Path,
    endpoint: String,
    project: ProjectSlug,
}

impl<'a> Options<'a> {
    fn parse(args: &'a [String]) -> Result<Self, CliError> {
        let file = args
            .first()
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(|| CliError::usage("pull/push needs FILE --server URL --project SLUG"))?;
        let mut server = None;
        let mut project = None;
        let mut tail = args[1..].iter();
        while let Some(option) = tail.next() {
            let destination = match option.as_str() {
                "--server" => &mut server,
                "--project" => &mut project,
                _ => return Err(CliError::usage(format!("unknown sync option: {option}"))),
            };
            if destination.is_some() {
                return Err(CliError::usage(format!("duplicate option: {option}")));
            }
            let value = tail
                .next()
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| CliError::usage(format!("{option} needs a value")))?;
            *destination = Some(value.as_str());
        }
        let server = server.ok_or_else(|| CliError::usage("missing --server URL"))?;
        let project =
            ProjectSlug::new(project.ok_or_else(|| CliError::usage("missing --project SLUG"))?)
                .map_err(|error| CliError::usage(error.to_string()))?;
        let base = server_url(server)?;
        Ok(Self {
            file: Path::new(file),
            endpoint: format!("{base}/api/v2/projects/{}", project.as_str()),
            project,
        })
    }
}

fn server_url(value: &str) -> Result<String, CliError> {
    let url = url::Url::parse(value)
        .map_err(|_| CliError::usage("--server needs an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || value.contains('\\')
    {
        return Err(CliError::usage(
            "--server must be an HTTP(S) URL without credentials, query, or fragment",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        // Signed project data must never be forwarded to a redirect target.
        .max_redirects(0)
        .max_redirects_will_error(false)
        .build()
        .into()
}

fn invalid_remote(message: impl Into<String>) -> CliError {
    CliError::coded("invalid_remote", message, json!({}))
}

fn changed_remote() -> CliError {
    CliError::coded(
        "remote_changed",
        "remote checkpoint changed during download; run the command again",
        json!({"retryable": true}),
    )
}

fn decode_response<T: DeserializeOwned>(
    response: ureq::http::Response<ureq::Body>,
    remaining: &mut usize,
) -> Result<T, CliError> {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(*remaining as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            CliError::coded(
                "network_error",
                format!("cannot read server response: {error}"),
                json!({"http_status":status}),
            )
        })?;
    if bytes.len() > *remaining {
        return Err(CliError::coded(
            "response_too_large",
            "remote download exceeds the 32 MiB limit",
            json!({"limit_bytes":MAX_BYTES}),
        ));
    }
    *remaining -= bytes.len();
    if !(200..300).contains(&status) {
        let document: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = document
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("http_error");
        let message = document
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("server returned HTTP {status}"));
        return Err(CliError::coded(
            code,
            message,
            json!({"http_status":status,"server":document}),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| invalid_remote(format!("invalid JSON from server: {error}")))
}

fn get_json<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    remaining: &mut usize,
) -> Result<T, CliError> {
    let response = agent.get(url).call().map_err(|error| {
        CliError::coded(
            "network_error",
            format!("remote request failed: {error}"),
            json!({}),
        )
    })?;
    decode_response(response, remaining)
}

fn download(options: &Options<'_>, agent: &ureq::Agent) -> Result<Chain, CliError> {
    let mut remaining = MAX_BYTES;
    let info_url = format!("{}/info", options.endpoint);
    let info: ProjectInfoV2 = get_json(agent, &info_url, &mut remaining)?;
    if info.manifest.project_id != options.project || info.state.len == 0 {
        return Err(invalid_remote(
            "server returned an invalid project or empty checkpoint",
        ));
    }
    let mut blocks = Vec::new();
    let mut from = 0u64;
    loop {
        let page: BlocksPageV2 = get_json(
            agent,
            &format!(
                "{}/blocks?from={from}&limit={PAGE_BLOCKS}",
                options.endpoint
            ),
            &mut remaining,
        )?;
        if page.state != info.state {
            return Err(changed_remote());
        }
        let end = from
            .checked_add(page.blocks.len() as u64)
            .ok_or_else(|| invalid_remote("block page cursor overflow"))?;
        let expected_next = (end < info.state.len).then_some(end);
        if page.project_id != options.project
            || page.from != from
            || page.blocks.is_empty()
            || page.blocks.len() > PAGE_BLOCKS
            || end > info.state.len
            || page.next_from != expected_next
        {
            return Err(invalid_remote(
                "invalid project, cursor, or page length in block response",
            ));
        }
        blocks.extend(page.blocks);
        if let Some(next) = page.next_from {
            from = next;
        } else {
            break;
        }
    }
    let chain = Chain { blocks };
    info.manifest.validate_chain(&chain).map_err(|error| {
        invalid_remote(format!("remote manifest/chain validation failed: {error}"))
    })?;
    let state = ChainStateV1::from_chain(&chain)
        .map_err(|error| invalid_remote(format!("remote chain validation failed: {error}")))?;
    if state != info.state {
        return Err(invalid_remote("downloaded chain does not match the advertised genesis, head, length, or operation count"));
    }
    let final_info: ProjectInfoV2 = get_json(agent, &info_url, &mut remaining)?;
    if final_info.manifest != info.manifest || final_info.state != info.state {
        return Err(changed_remote());
    }
    Ok(chain)
}

fn read_local(path: &Path) -> Result<Chain, CliError> {
    let file = std::fs::File::open(path).map_err(|error| {
        CliError::coded(
            "read_failed",
            format!("cannot open {}: {error}", path.display()),
            json!({"path":path}),
        )
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            CliError::coded(
                "read_failed",
                format!("cannot read {}: {error}", path.display()),
                json!({"path":path}),
            )
        })?;
    if bytes.len() > MAX_BYTES {
        return Err(CliError::coded(
            "document_too_large",
            "local chain exceeds the 32 MiB limit",
            json!({"path":path,"limit_bytes":MAX_BYTES}),
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        CliError::coded(
            "invalid_chain",
            "local chain is not UTF-8",
            json!({"path":path}),
        )
    })?;
    if serde_json::from_str::<Value>(text).is_ok_and(|document| document.get("chain").is_some()) {
        return Err(CliError::coded(
            "workspace_sync_requires_chain",
            "remote CLI sync requires a committed bare chain document; use GUI sync for .mantis workspaces so pending and recovery edits are preserved",
            json!({"path":path}),
        ));
    }
    Chain::from_json(text).map_err(|error| {
        CliError::coded(
            "invalid_chain",
            error.to_string(),
            json!({"path":path,"chain_error":error.code()}),
        )
    })
}

fn ensure_prefix(prefix: &Chain, longer: &Chain) -> Result<(), CliError> {
    if prefix.len() > longer.len()
        || prefix
            .blocks
            .iter()
            .zip(&longer.blocks)
            .any(|(a, b)| a.hash != b.hash)
    {
        return Err(CliError::coded("diverged", "histories cannot be fast-forwarded; preserve local edits and resolve the divergent or newer history explicitly", json!({"prefix_len":prefix.len(),"prefix_head":prefix.head().hash,"target_len":longer.len(),"target_head":longer.head().hash})));
    }
    Ok(())
}

pub(crate) fn cmd_pull(args: &[String]) -> Result<String, CliError> {
    let options = Options::parse(args)?;
    let _lock = DocumentLocks::acquire(&[options.file])?;
    let local = match options.file.try_exists() {
        Ok(true) => Some(read_local(options.file)?),
        Ok(false) => None,
        Err(error) => {
            return Err(CliError::coded(
                "read_failed",
                error.to_string(),
                json!({"path":options.file}),
            ))
        }
    };
    let remote = download(&options, &http_agent())?;
    if let Some(local) = &local {
        ensure_prefix(local, &remote)?;
    }
    // Pretty JSON is the canonical local representation; bound it as well as
    // the wire download before writing any bytes to the destination.
    if remote.to_json().len() + 1 > MAX_BYTES {
        return Err(CliError::coded(
            "document_too_large",
            "downloaded chain exceeds the 32 MiB local document limit",
            json!({"limit_bytes":MAX_BYTES}),
        ));
    }
    let appended = remote.len() - local.as_ref().map_or(0, Chain::len);
    if local.as_ref() != Some(&remote) {
        write_chain_atomic(options.file, &remote)?;
    }
    Ok(format!(
        "{}\n",
        json!({"ok":true,"action":"pull","project":options.project,"len":remote.len(),"head":remote.head().hash,"downloaded_blocks":appended})
    ))
}

fn push_batches(local: &Chain, base_len: usize) -> Result<Vec<PushRequestV2>, CliError> {
    let mut batches = Vec::new();
    let mut from = base_len;
    while from < local.len() {
        let mut end = from;
        let mut operations = 0;
        let mut block_bytes = 0;
        while end < local.len() && end - from < PAGE_BLOCKS {
            let block = &local.blocks[end];
            let bytes = serde_json::to_vec(block)
                .map_err(|error| invalid_remote(error.to_string()))?
                .len();
            if operations + block.ops.len() > MAX_PUSH_OPS
                || block_bytes + bytes + (end - from) + 256 > MAX_BYTES
            {
                break;
            }
            operations += block.ops.len();
            block_bytes += bytes;
            end += 1;
        }
        if end == from {
            return Err(CliError::coded(
                "block_too_large",
                "one signed block exceeds the server's 50,000 operation or 32 MiB push limit",
                json!({"block":from}),
            ));
        }
        let request = PushRequestV2 {
            base_len: from as u64,
            base_head: HashHex::new(local.blocks[from - 1].hash.clone())
                .map_err(|error| invalid_remote(error.to_string()))?,
            blocks: local.blocks[from..end].to_vec(),
        };
        // Check the exact envelope before issuing any requests.
        if serde_json::to_vec(&request)
            .map_err(|error| invalid_remote(error.to_string()))?
            .len()
            > MAX_BYTES
        {
            return Err(CliError::coded(
                "request_too_large",
                "push request exceeds 32 MiB",
                json!({"block":from}),
            ));
        }
        batches.push(request);
        from = end;
    }
    Ok(batches)
}

pub(crate) fn cmd_push(args: &[String]) -> Result<String, CliError> {
    let options = Options::parse(args)?;
    let _lock = DocumentLocks::acquire(&[options.file])?;
    let local = read_local(options.file)?;
    let agent = http_agent();
    let remote = download(&options, &agent)?;
    ensure_prefix(&remote, &local)?;
    let batches = push_batches(&local, remote.len())?;
    let mut appended = 0;
    let mut remaining = MAX_BYTES;
    for request in batches {
        let body =
            serde_json::to_vec(&request).map_err(|error| invalid_remote(error.to_string()))?;
        let result = (|| {
            let response = agent
                .post(&format!("{}/blocks", options.endpoint))
                .header("Content-Type", "application/json")
                .send(body.as_slice())
                .map_err(|error| {
                    CliError::coded(
                        "network_error",
                        format!(
                            "push request failed: {error}; inspect remote state before retrying"
                        ),
                        json!({"outcome_unknown":true}),
                    )
                })?;
            let result: PushResponseV2 = decode_response(response, &mut remaining)?;
            let expected_len = request.base_len + request.blocks.len() as u64;
            if result.len != expected_len
                || result.appended != request.blocks.len() as u64
                || result.head.as_str() != local.blocks[expected_len as usize - 1].hash
            {
                return Err(invalid_remote("push acknowledgement does not match the submitted signed blocks; inspect remote state before retrying"));
            }
            Ok(result)
        })();
        match result {
            Ok(result) => appended += result.appended,
            Err(CliError::Detailed {
                code,
                message,
                mut context,
            }) => {
                context["acknowledged_blocks"] = json!(appended);
                let acknowledged_len = remote.len() + appended as usize;
                context["last_acknowledged_len"] = json!(acknowledged_len);
                context["last_acknowledged_head"] = json!(local.blocks[acknowledged_len - 1].hash);
                return Err(CliError::coded(&code, message, context));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(format!(
        "{}\n",
        json!({"ok":true,"action":"push","project":options.project,"len":local.len(),"head":local.head().hash,"appended":appended})
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_graph::{GraphOp, NodeId};
    use mantis_protocol::{AccessLedgerV1, ProjectBootstrapV1, PublicKeyHex};
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, Mutex,
        },
        thread::{self, JoinHandle},
    };

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Document(PathBuf);
    impl Document {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "mantis-remote-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory.join("model.json"))
        }
    }
    impl Drop for Document {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    struct Fixture {
        server: String,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }
    impl Fixture {
        fn new(
            mut handler: impl FnMut(&str, &str, Value) -> (u16, Value) + Send + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = format!("http://{}/mantis/", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            let thread = thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    let request = first.split_whitespace().collect::<Vec<_>>();
                    let mut length = 0;
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        if header == "\r\n" || header.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                length = value.trim().parse().unwrap();
                            }
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let body = if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    };
                    let (status, document) = handler(request[0], request[1], body);
                    let bytes = serde_json::to_vec(&document).unwrap();
                    write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                    stream.write_all(&bytes).unwrap();
                }
            });
            Self {
                server,
                stop,
                thread: Some(thread),
            }
        }
        fn args(&self, file: &Path) -> Vec<String> {
            [
                file.to_str().unwrap(),
                "--server",
                &self.server,
                "--project",
                "fixture",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let result = self.thread.take().unwrap().join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }

    fn extend(chain: &mut Chain, count: usize) {
        let author = Identity::from_secret_hex("owner", &"07".repeat(32)).unwrap();
        for _ in 0..count {
            let index = chain.len();
            chain
                .append(
                    vec![GraphOp::AddNode {
                        id: NodeId(index as u128),
                        type_name: "sphere".into(),
                        pos: (0.0, 0.0),
                    }],
                    "fixture",
                    &author,
                    index as u64,
                )
                .unwrap();
        }
    }

    fn info(chain: &Chain) -> ProjectInfoV2 {
        let owner = Identity::from_secret_hex("owner", &"07".repeat(32)).unwrap();
        let bootstrap = ProjectBootstrapV1::new_legacy_signed(
            ProjectSlug::new("fixture").unwrap(),
            "Fixture",
            chain.clone(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            1,
            &owner,
        )
        .unwrap();
        let access = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        ProjectInfoV2::from_parts(&bootstrap.manifest, chain, &access).unwrap()
    }

    fn read_route(path: &str, chain: &Chain) -> Value {
        assert!(
            path.starts_with("/mantis/api/v2/projects/fixture/"),
            "prefix lost: {path}"
        );
        if path.ends_with("/info") {
            return json!(info(chain));
        }
        let from: usize = path
            .split("from=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let end = (from + 2).min(chain.len());
        json!(BlocksPageV2 {
            project_id: ProjectSlug::new("fixture").unwrap(),
            from: from as u64,
            blocks: chain.blocks[from..end].to_vec(),
            next_from: (end < chain.len()).then_some(end as u64),
            state: ChainStateV1::from_chain(chain).unwrap()
        })
    }

    #[test]
    fn paged_pull_clones_then_fast_forwards_without_losing_prefix() {
        let mut chain = Chain::new();
        extend(&mut chain, 3);
        let remote = Arc::new(Mutex::new(chain.clone()));
        let server_chain = remote.clone();
        let fixture = Fixture::new(move |method, path, _| {
            assert_eq!(method, "GET");
            (200, read_route(path, &server_chain.lock().unwrap()))
        });
        let file = Document::new();
        cmd_pull(&fixture.args(&file.0)).unwrap();
        assert_eq!(read_local(&file.0).unwrap(), chain);
        extend(&mut remote.lock().unwrap(), 1);
        cmd_pull(&fixture.args(&file.0)).unwrap();
        assert_eq!(read_local(&file.0).unwrap(), *remote.lock().unwrap());
    }

    #[test]
    fn pull_refuses_newer_local_and_keeps_original_bytes() {
        let chain = Chain::new();
        let fixture = Fixture::new(move |_, path, _| (200, read_route(path, &chain)));
        let file = Document::new();
        let mut local = Chain::new();
        extend(&mut local, 1);
        let original = local.to_json();
        std::fs::write(&file.0, &original).unwrap();
        assert!(
            matches!(cmd_pull(&fixture.args(&file.0)),Err(CliError::Detailed {code,..}) if code == "diverged")
        );
        assert_eq!(std::fs::read_to_string(&file.0).unwrap(), original);
    }

    #[test]
    fn pull_refuses_a_changing_checkpoint_before_creating_file() {
        let initial = Chain::new();
        let mut changed = initial.clone();
        extend(&mut changed, 1);
        let fixture = Fixture::new(move |_, path, _| {
            (
                200,
                read_route(
                    path,
                    if path.ends_with("/info") {
                        &initial
                    } else {
                        &changed
                    },
                ),
            )
        });
        let file = Document::new();
        assert!(
            matches!(cmd_pull(&fixture.args(&file.0)),Err(CliError::Detailed {code,..}) if code == "remote_changed")
        );
        assert!(!file.0.exists());
    }

    #[test]
    fn push_sends_only_signed_tail_with_compare_and_swap_checkpoint() {
        let mut remote = Chain::new();
        extend(&mut remote, 1);
        let mut local = remote.clone();
        extend(&mut local, 2);
        let shared = Arc::new(Mutex::new(remote));
        let state = shared.clone();
        let fixture = Fixture::new(move |method, path, body| {
            let mut chain = state.lock().unwrap();
            if method == "GET" {
                return (200, read_route(path, &chain));
            }
            assert_eq!(path, "/mantis/api/v2/projects/fixture/blocks");
            let request: PushRequestV2 = serde_json::from_value(body).unwrap();
            assert_eq!(request.base_len, chain.len() as u64);
            assert_eq!(request.base_head.as_str(), chain.head().hash);
            assert_eq!(request.blocks.len(), 2);
            chain.blocks.extend(request.blocks);
            chain.validate().unwrap();
            (
                200,
                json!(PushResponseV2 {
                    len: chain.len() as u64,
                    head: HashHex::new(chain.head().hash.clone()).unwrap(),
                    appended: 2
                }),
            )
        });
        let file = Document::new();
        std::fs::write(&file.0, local.to_json()).unwrap();
        let output: Value =
            serde_json::from_str(&cmd_push(&fixture.args(&file.0)).unwrap()).unwrap();
        assert_eq!(output["appended"], 2);
        assert_eq!(*shared.lock().unwrap(), local);
    }

    #[test]
    fn push_preserves_conflict_code_and_context_without_retry_or_local_mutation() {
        let remote = Chain::new();
        let mut local = remote.clone();
        extend(&mut local, 1);
        let attempts = Arc::new(AtomicU64::new(0));
        let posted = attempts.clone();
        let fixture = Fixture::new(move |method, path, _| {
            if method == "GET" {
                return (200, read_route(path, &remote));
            }
            posted.fetch_add(1, Ordering::Relaxed);
            (
                409,
                json!({"error":{"code":"diverged","message":"checkpoint moved","project":"fixture"},"len":2,"head":remote.head().hash}),
            )
        });
        let file = Document::new();
        let original = local.to_json();
        std::fs::write(&file.0, &original).unwrap();
        match cmd_push(&fixture.args(&file.0)) {
            Err(CliError::Detailed { code, context, .. }) => {
                assert_eq!(code, "diverged");
                assert_eq!(context["http_status"], 409);
                assert_eq!(context["server"]["len"], 2);
                assert_eq!(context["acknowledged_blocks"], 0);
            }
            other => panic!("unexpected result {other:?}"),
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(std::fs::read_to_string(&file.0).unwrap(), original);
    }

    #[test]
    fn remote_urls_keep_subpaths_and_reject_embedded_credentials_or_query() {
        assert_eq!(
            server_url("https://cad.example/mantis/").unwrap(),
            "https://cad.example/mantis"
        );
        for url in [
            "https://user:secret@cad.example",
            "https://cad.example?q=secret",
            "https://cad.example/#secret",
            "file:///tmp/model",
            "https://cad.example\\elsewhere",
            " https://cad.example",
        ] {
            assert!(server_url(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn portable_workspace_sync_is_rejected_without_stripping_pending_edits() {
        let file = Document::new();
        let original = json!({"chain":Chain::new(),"pending":[{"user_work":"preserve"}],"recovery_ops":[{"unresolved_work":"preserve"}]}).to_string();
        std::fs::write(&file.0, &original).unwrap();
        assert!(
            matches!(read_local(&file.0), Err(CliError::Detailed {code,..}) if code == "workspace_sync_requires_chain")
        );
        assert_eq!(std::fs::read_to_string(&file.0).unwrap(), original);
    }

    #[test]
    fn push_planner_splits_at_block_and_operation_limits_before_sending() {
        let mut chain = Chain::new();
        extend(&mut chain, 1);
        let block = chain.blocks[1].clone();
        chain.blocks.extend(std::iter::repeat_n(block, PAGE_BLOCKS));
        let batches = push_batches(&chain, 1).unwrap();
        assert_eq!(
            batches
                .iter()
                .map(|request| request.blocks.len())
                .collect::<Vec<_>>(),
            vec![256, 1]
        );
        chain.blocks.truncate(3);
        let operation = chain.blocks[1].ops[0].clone();
        chain.blocks[1].ops = vec![operation; 30_000];
        chain.blocks[2].ops = chain.blocks[1].ops.clone();
        assert_eq!(push_batches(&chain, 1).unwrap().len(), 2);
        let operation = chain.blocks[1].ops[0].clone();
        chain.blocks[1].ops.resize(50_001, operation);
        assert!(
            matches!(push_batches(&chain,1),Err(CliError::Detailed {code,..}) if code == "block_too_large")
        );
    }
}
