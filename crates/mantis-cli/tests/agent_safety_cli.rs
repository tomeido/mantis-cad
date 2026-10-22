use mantis_chain::{Chain, Identity};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    chain: PathBuf,
    ops: PathBuf,
    identity: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "mantis-agent-safety-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let chain = directory.join("model.json");
        let ops = directory.join("edit.json");
        let identity = directory.join("identity.json");
        let signer = Identity::from_secret_hex("test-agent", &"42".repeat(32)).unwrap();
        std::fs::write(&chain, Chain::new().to_json()).unwrap();
        std::fs::write(&identity, json!({"name": signer.name, "secret": signer.secret_hex(), "public": signer.public_hex()}).to_string()).unwrap();
        std::fs::write(&ops, json!([{"op":"AddNode", "id":"10000000000000000000000000000001", "type_name":"sphere", "pos":[0,0]}]).to_string()).unwrap();
        Self {
            directory,
            chain,
            ops,
            identity,
        }
    }

    fn apply(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mantis-cli"))
            .arg("apply")
            .arg(&self.chain)
            .arg("--ops")
            .arg(&self.ops)
            .arg("--identity")
            .arg(&self.identity)
            .args(["--message", "test change", "--json-errors"])
            .args(extra)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn stale_observation_rejects_without_writing_and_current_head_commits() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.chain).unwrap();
    let stale = fixture.apply(&["--expected-head", &"0".repeat(64)]);
    assert_eq!(stale.status.code(), Some(1));
    assert!(stale.stdout.is_empty());
    let error: Value = serde_json::from_slice(&stale.stderr).unwrap();
    assert_eq!(error["error"]["code"], "head_mismatch");
    assert_eq!(
        error["error"]["context"]["actual_head"],
        Chain::new().head().hash
    );
    assert_eq!(std::fs::read(&fixture.chain).unwrap(), before);
    let current = Chain::new().head().hash.clone();
    let trial = fixture.apply(&["--expected-head", &current, "--dry-run"]);
    assert!(
        trial.status.success(),
        "{}",
        String::from_utf8_lossy(&trial.stderr)
    );
    assert_eq!(std::fs::read(&fixture.chain).unwrap(), before);
    let committed = fixture.apply(&["--expected-head", &current]);
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    assert_eq!(
        Chain::from_json(&std::fs::read_to_string(&fixture.chain).unwrap())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn another_process_lock_blocks_apply_then_releases_cleanly() {
    let fixture = Fixture::new();
    let lock_path = fixture.directory.join(".model.json.mantis-lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    lock.lock().unwrap();
    let before = std::fs::read(&fixture.chain).unwrap();
    let busy = fixture.apply(&[]);
    assert_eq!(busy.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&busy.stderr).unwrap();
    assert_eq!(error["error"]["code"], "document_busy");
    assert_eq!(error["error"]["context"]["retryable"], true);
    assert_eq!(std::fs::read(&fixture.chain).unwrap(), before);
    drop(lock);
    let committed = fixture.apply(&[]);
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
}

#[test]
fn errors_preserve_operation_context_and_usage_exit_status() {
    let fixture = Fixture::new();
    std::fs::write(
        &fixture.ops,
        json!([{"op":"RemoveNode", "id":"10000000000000000000000000000001"}]).to_string(),
    )
    .unwrap();
    let rejected = fixture.apply(&[]);
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["error"]["code"], "graph_op_rejected");
    assert_eq!(error["error"]["context"]["op"], 0);
    let usage = Command::new(env!("CARGO_BIN_EXE_mantis-cli"))
        .args(["--json-errors", "apply"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&usage.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_arguments");
    let help = Command::new(env!("CARGO_BIN_EXE_mantis-cli"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
}

#[test]
fn global_json_errors_preserves_literal_message_value() {
    let fixture = Fixture::new();
    let result = Command::new(env!("CARGO_BIN_EXE_mantis-cli"))
        .arg("--json-errors")
        .arg("apply")
        .arg(&fixture.chain)
        .arg("--ops")
        .arg(&fixture.ops)
        .arg("--identity")
        .arg(&fixture.identity)
        .args(["--message", "--json-errors"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let chain = Chain::from_json(&std::fs::read_to_string(&fixture.chain).unwrap()).unwrap();
    assert_eq!(chain.head().message, "--json-errors");
}
