//! mantis-cli — headless MantisCAD tools.
//!
//! Subcommands:
//!   keygen --name NAME [--out FILE]      generate a signing identity
//!   inspect FILE                         table of blocks in a chain file
//!   verify FILE                          full chain validation
//!   replay FILE [--upto N] [--obj OUT]   replay ops -> graph -> meshes
//!   demo [--out FILE]                    generate the demo collaboration chain
//!
//! The CLI is a UI edge: reading the clock (`demo` timestamps) and OS
//! randomness (`keygen`) are allowed here — never inside the libraries.

mod demo;
mod replay;

use mantis_chain::{Chain, Identity};

const USAGE: &str = "mantis-cli — headless MantisCAD tools

USAGE:
  mantis-cli keygen --name NAME [--out FILE]   generate identity JSON
                                               {\"name\":..,\"secret\":hex,\"public\":hex}
  mantis-cli inspect FILE                      list blocks (idx, author, ops, bytes)
  mantis-cli verify FILE                       validate chain, print OK or the error
  mantis-cli replay FILE [--upto N] [--obj OUT.obj]
                                               replay op-log, evaluate graph,
                                               merge preview meshes, export OBJ
  mantis-cli demo [--out FILE]                 write demo chain (default demo-chain.json)";

/// CLI failure: usage errors exit 2, runtime errors exit 1.
#[derive(Debug, PartialEq)]
enum CliError {
    Usage(String),
    Runtime(String),
}

impl CliError {
    fn usage(msg: impl Into<String>) -> CliError {
        CliError::Usage(msg.into())
    }
    fn runtime(msg: impl Into<String>) -> CliError {
        CliError::Runtime(msg.into())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(output) => print!("{output}"),
        Err(CliError::Usage(msg)) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
        Err(CliError::Runtime(msg)) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    }
}

/// Route a command line to its subcommand; returns the text to print.
fn dispatch(args: &[String]) -> Result<String, CliError> {
    let Some(cmd) = args.first() else {
        return Err(CliError::usage(USAGE));
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "keygen" => cmd_keygen(rest),
        "inspect" => cmd_inspect(rest),
        "verify" => cmd_verify(rest),
        "replay" => cmd_replay(rest),
        "demo" => cmd_demo(rest),
        "-h" | "--help" | "help" => Err(CliError::usage(USAGE)),
        other => Err(CliError::usage(format!("unknown subcommand: {other}\n\n{USAGE}"))),
    }
}

/// Load + validate a chain file (Chain::from_json validates fully).
fn load_chain(path: &str) -> Result<Chain, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::runtime(format!("cannot read {path}: {e}")))?;
    Chain::from_json(&text)
        .map_err(|e| CliError::runtime(format!("invalid chain in {path}: {e}")))
}

// ---------------------------------------------------------------------------
// keygen
// ---------------------------------------------------------------------------

/// The identity JSON format: {"name":..,"secret":hex,"public":hex}.
fn keygen_json(identity: &Identity) -> String {
    format!(
        "{{\"name\":{},\"secret\":\"{}\",\"public\":\"{}\"}}",
        serde_json::to_string(&identity.name).unwrap_or_else(|_| "\"\"".to_string()),
        identity.secret_hex(),
        identity.public_hex(),
    )
}

fn cmd_keygen(args: &[String]) -> Result<String, CliError> {
    let mut name: Option<String> = None;
    let mut out: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--name" => name = Some(it.next().ok_or(CliError::usage("--name needs a value"))?.clone()),
            "--out" => out = Some(it.next().ok_or(CliError::usage("--out needs a value"))?.clone()),
            other => return Err(CliError::usage(format!("keygen: unknown argument {other}"))),
        }
    }
    let name = name.ok_or(CliError::usage("keygen requires --name NAME"))?;
    let identity = Identity::generate(&name);
    let json = keygen_json(&identity);
    match out {
        Some(path) => {
            std::fs::write(&path, format!("{json}\n"))
                .map_err(|e| CliError::runtime(format!("cannot write {path}: {e}")))?;
            Ok(format!("wrote {path} (public {})\n", identity.public_hex()))
        }
        None => Ok(format!("{json}\n")),
    }
}

// ---------------------------------------------------------------------------
// inspect / verify
// ---------------------------------------------------------------------------

fn cmd_inspect(args: &[String]) -> Result<String, CliError> {
    let [path] = args else {
        return Err(CliError::usage("usage: mantis-cli inspect FILE"));
    };
    let chain = load_chain(path)?;
    let mut s = String::new();
    s.push_str(&format!(
        "{:>4}  {:<16} {:>5} {:>8}  message\n",
        "idx", "author", "ops", "bytes"
    ));
    for b in &chain.blocks {
        s.push_str(&format!(
            "{:>4}  {:<16} {:>5} {:>8}  {}\n",
            b.index,
            b.author,
            b.ops.len(),
            b.byte_size(),
            b.message
        ));
    }
    s.push_str(&format!(
        "totals: {} blocks, {} ops, {} bytes\n",
        chain.len(),
        chain.total_ops(),
        chain.byte_size()
    ));
    Ok(s)
}

fn cmd_verify(args: &[String]) -> Result<String, CliError> {
    let [path] = args else {
        return Err(CliError::usage("usage: mantis-cli verify FILE"));
    };
    load_chain(path)?; // from_json validates hashes, sigs, links + replay
    Ok("OK\n".to_string())
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

fn cmd_replay(args: &[String]) -> Result<String, CliError> {
    let mut path: Option<String> = None;
    let mut upto: Option<usize> = None;
    let mut obj: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--upto" => {
                let v = it.next().ok_or(CliError::usage("--upto needs a value"))?;
                upto = Some(
                    v.parse::<usize>()
                        .map_err(|_| CliError::usage(format!("invalid --upto: {v}")))?,
                );
            }
            "--obj" => obj = Some(it.next().ok_or(CliError::usage("--obj needs a value"))?.clone()),
            other if path.is_none() && !other.starts_with("--") => {
                path = Some(other.to_string());
            }
            other => return Err(CliError::usage(format!("replay: unknown argument {other}"))),
        }
    }
    let path = path.ok_or(CliError::usage("usage: mantis-cli replay FILE [--upto N] [--obj OUT.obj]"))?;

    let chain = load_chain(&path)?;
    let report = replay::replay_report(&chain, upto).map_err(CliError::runtime)?;

    let mut s = String::new();
    for line in &report.node_lines {
        s.push_str(line);
        s.push('\n');
    }
    if report.error_count > 0 {
        s.push_str(&format!("{} node(s) errored\n", report.error_count));
    }
    s.push_str(&format!(
        "meshes: {} ({} vertices, {} triangles)\n",
        report.mesh_count,
        report.mesh.vertex_count(),
        report.mesh.triangle_count()
    ));
    let geo_bytes = report.mesh.approx_byte_size();
    let chain_bytes = chain.byte_size().max(1);
    s.push_str(&format!(
        "geometry ≈ {geo_bytes} bytes vs chain {chain_bytes} bytes ({:.1}x compression)\n",
        geo_bytes as f64 / chain_bytes as f64
    ));
    if let Some(obj_path) = obj {
        std::fs::write(&obj_path, report.mesh.to_obj())
            .map_err(|e| CliError::runtime(format!("cannot write {obj_path}: {e}")))?;
        s.push_str(&format!("wrote {obj_path}\n"));
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// demo
// ---------------------------------------------------------------------------

/// Milliseconds since the Unix epoch. Clock reads are allowed at the CLI
/// edge; timestamps land inside blocks, never inside graph evaluation.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn cmd_demo(args: &[String]) -> Result<String, CliError> {
    let mut out = "demo-chain.json".to_string();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = it.next().ok_or(CliError::usage("--out needs a value"))?.clone(),
            other => return Err(CliError::usage(format!("demo: unknown argument {other}"))),
        }
    }
    let alice = Identity::generate("alice");
    let bob = Identity::generate("bob");
    let t1 = now_ms();
    let chain = demo::build_demo_chain(&alice, &bob, t1, t1 + 1).map_err(CliError::runtime)?;
    std::fs::write(&out, chain.to_json())
        .map_err(|e| CliError::runtime(format!("cannot write {out}: {e}")))?;
    Ok(format!(
        "wrote {out}: {} blocks ({} by alice, bob), {} ops, {} bytes\n",
        chain.len(),
        chain.len() - 1,
        chain.total_ops(),
        chain.byte_size()
    ))
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("mantis-cli-test-{}-{n}-{tag}", std::process::id()))
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn demo_chain() -> Chain {
        let alice = Identity::generate("alice");
        let bob = Identity::generate("bob");
        demo::build_demo_chain(&alice, &bob, 1000, 2000).expect("demo builds")
    }

    // -- keygen ----------------------------------------------------------------

    #[test]
    fn keygen_round_trips_through_json() {
        let id = Identity::generate("carol");
        let json = keygen_json(&id);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["name"], "carol");
        let secret = v["secret"].as_str().unwrap();
        let public = v["public"].as_str().unwrap();
        assert_eq!(secret.len(), 64);
        assert_eq!(public.len(), 64);
        let restored = Identity::from_secret_hex("carol", secret).expect("secret restores");
        assert_eq!(restored.public_hex(), public);
        assert_eq!(restored.secret_hex(), secret);
    }

    #[test]
    fn keygen_name_with_quotes_is_escaped() {
        let id = Identity::generate("evil \"quote\" \\ name");
        let json = keygen_json(&id);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON despite quotes");
        assert_eq!(v["name"], "evil \"quote\" \\ name");
    }

    #[test]
    fn keygen_cmd_stdout_and_file() {
        let out = dispatch(&args(&["keygen", "--name", "dave"])).unwrap();
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["name"], "dave");

        let path = temp_path("id.json");
        let msg = dispatch(&args(&["keygen", "--name", "dave", "--out", path.to_str().unwrap()]))
            .unwrap();
        assert!(msg.contains("wrote"), "{msg}");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let restored =
            Identity::from_secret_hex("dave", saved["secret"].as_str().unwrap()).unwrap();
        assert_eq!(restored.public_hex(), saved["public"].as_str().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn keygen_requires_name() {
        assert!(matches!(
            dispatch(&args(&["keygen"])),
            Err(CliError::Usage(_))
        ));
    }

    // -- demo -------------------------------------------------------------------

    #[test]
    fn demo_chain_builds_and_validates() {
        let chain = demo_chain();
        chain.validate().expect("demo chain validates");
        assert_eq!(chain.len(), 3); // genesis + alice + bob
        assert_eq!(chain.blocks[1].author, "alice");
        assert_eq!(chain.blocks[1].message, "tower profile");
        assert_eq!(chain.blocks[2].author, "bob");
        assert_eq!(chain.blocks[2].message, "loft the tower");
        assert!(chain.total_ops() >= 30, "ops: {}", chain.total_ops());
        // the op-log stays tiny: a whole building in a few KB
        assert!(chain.byte_size() < 20_000, "bytes: {}", chain.byte_size());
        // round-trips losslessly through JSON (with validation)
        let back = Chain::from_json(&chain.to_json()).expect("round trip");
        assert_eq!(back, chain);
    }

    #[test]
    fn demo_cmd_writes_valid_file() {
        let path = temp_path("demo-chain.json");
        let msg = dispatch(&args(&["demo", "--out", path.to_str().unwrap()])).unwrap();
        assert!(msg.contains("ops"), "{msg}");
        let chain = Chain::from_json(&std::fs::read_to_string(&path).unwrap())
            .expect("written demo chain validates");
        assert_eq!(chain.len(), 3);
        let _ = std::fs::remove_file(&path);
    }

    // -- replay -----------------------------------------------------------------

    #[test]
    fn replay_demo_produces_tower_mesh() {
        let chain = demo_chain();
        let report = replay::replay_report(&chain, None).expect("replay evaluates");
        assert_eq!(report.error_count, 0, "{:?}", report.node_lines);
        assert!(report.mesh_count > 0, "no meshes collected");
        assert!(report.mesh.vertex_count() > 0);
        assert!(report.mesh.triangle_count() > 0);
        // 12 nodes in the demo graph, one line each
        assert_eq!(report.node_lines.len(), 12);
        assert!(
            report.node_lines.iter().any(|l| l.contains("loft") && l.contains("Mesh")),
            "loft line missing mesh: {:?}",
            report.node_lines
        );
        // the whole point: derived geometry dwarfs the op-log
        assert!(report.mesh.approx_byte_size() > chain.byte_size());
    }

    #[test]
    fn replay_obj_export_starts_with_vertices() {
        let chain = demo_chain();
        let report = replay::replay_report(&chain, None).unwrap();
        let obj = report.mesh.to_obj();
        assert!(obj.starts_with("v "), "OBJ starts: {:?}", &obj[..obj.len().min(40)]);
        assert!(obj.contains("\nvn "), "has normals");
        assert!(obj.contains("\nf "), "has faces");
        let f_lines = obj.lines().filter(|l| l.starts_with("f ")).count();
        assert_eq!(f_lines, report.mesh.triangle_count());
    }

    #[test]
    fn replay_upto_prefix_has_no_mesh_yet() {
        let chain = demo_chain();
        // through block 1 only: profile exists, no loft node yet
        let report = replay::replay_report(&chain, Some(1)).expect("prefix replays");
        assert_eq!(report.mesh_count, 0);
        assert_eq!(report.node_lines.len(), 6);
        // upto 0 -> genesis only -> empty graph
        let report = replay::replay_report(&chain, Some(0)).unwrap();
        assert!(report.node_lines.is_empty());
    }

    #[test]
    fn replay_cmd_end_to_end_with_obj() {
        let chain_path = temp_path("replay-chain.json");
        std::fs::write(&chain_path, demo_chain().to_json()).unwrap();
        let obj_path = temp_path("tower.obj");
        let out = dispatch(&args(&[
            "replay",
            chain_path.to_str().unwrap(),
            "--obj",
            obj_path.to_str().unwrap(),
        ]))
        .unwrap();
        assert!(out.contains("meshes: 1"), "{out}");
        assert!(out.contains("compression"), "{out}");
        let obj = std::fs::read_to_string(&obj_path).unwrap();
        assert!(obj.starts_with("v "));
        let _ = std::fs::remove_file(&chain_path);
        let _ = std::fs::remove_file(&obj_path);
    }

    #[test]
    fn replay_bad_upto_is_usage_error() {
        assert!(matches!(
            dispatch(&args(&["replay", "x.json", "--upto", "banana"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            dispatch(&args(&["replay"])),
            Err(CliError::Usage(_))
        ));
    }

    // -- inspect / verify ---------------------------------------------------------

    #[test]
    fn inspect_lists_blocks_and_totals() {
        let path = temp_path("inspect-chain.json");
        std::fs::write(&path, demo_chain().to_json()).unwrap();
        let out = dispatch(&args(&["inspect", path.to_str().unwrap()])).unwrap();
        assert!(out.contains("genesis"), "{out}");
        assert!(out.contains("alice"), "{out}");
        assert!(out.contains("bob"), "{out}");
        assert!(out.contains("tower profile"), "{out}");
        assert!(out.contains("loft the tower"), "{out}");
        assert!(out.contains("totals: 3 blocks"), "{out}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_ok_and_tampered() {
        let path = temp_path("verify-chain.json");
        let chain = demo_chain();
        std::fs::write(&path, chain.to_json()).unwrap();
        assert_eq!(
            dispatch(&args(&["verify", path.to_str().unwrap()])).unwrap(),
            "OK\n"
        );
        // tamper: hash check must fail with a precise error
        let evil = chain.to_json().replace("tower profile", "TOWER PROFILE");
        std::fs::write(&path, evil).unwrap();
        match dispatch(&args(&["verify", path.to_str().unwrap()])) {
            Err(CliError::Runtime(msg)) => assert!(msg.contains("BadHash"), "{msg}"),
            other => panic!("expected runtime error, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verify_missing_file_is_runtime_error() {
        assert!(matches!(
            dispatch(&args(&["verify", "/nonexistent/nope.json"])),
            Err(CliError::Runtime(_))
        ));
    }

    // -- dispatch -------------------------------------------------------------------

    #[test]
    fn no_args_and_unknown_subcommand_show_usage() {
        match dispatch(&[]) {
            Err(CliError::Usage(msg)) => assert!(msg.contains("USAGE"), "{msg}"),
            other => panic!("expected usage, got {other:?}"),
        }
        assert!(matches!(
            dispatch(&args(&["frobnicate"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(dispatch(&args(&["--help"])), Err(CliError::Usage(_))));
    }
}
