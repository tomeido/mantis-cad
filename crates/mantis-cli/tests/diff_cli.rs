use mantis_chain::{Chain, Identity};
use mantis_graph::{GraphOp, NodeId, ParamValue};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn example_chain() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/demo-chain.json")
        .canonicalize()
        .expect("the checked-in demo chain must exist")
}

fn run_diff(extra_args: &[&str]) -> Output {
    run_diff_for_path(&example_chain(), extra_args)
}

fn run_diff_for_path(path: &Path, extra_args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mantis-cli"))
        .arg("diff")
        .arg(path)
        .args(extra_args)
        // Prove the fixture does not rely on Cargo's process working directory.
        .current_dir(std::env::temp_dir())
        .output()
        .expect("mantis-cli should launch")
}

struct HiddenMeshFixture {
    path: PathBuf,
}

impl HiddenMeshFixture {
    fn new() -> Self {
        let author = Identity::generate("hidden-mesh-test");
        let mut chain = Chain::new();
        chain
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
                        value: ParamValue::Number(1.0),
                    },
                    GraphOp::AddNode {
                        id: NodeId(2),
                        type_name: "box_mesh".into(),
                        pos: (100.0, 0.0),
                    },
                    GraphOp::SetParam {
                        id: NodeId(2),
                        key: "__preview".into(),
                        value: ParamValue::Bool(false),
                    },
                    GraphOp::Connect {
                        from: (NodeId(1), 0),
                        to: (NodeId(2), 1),
                    },
                ],
                "hidden unit box",
                &author,
                1,
            )
            .unwrap();
        chain
            .append(
                vec![GraphOp::SetParam {
                    id: NodeId(1),
                    key: "value".into(),
                    value: ParamValue::Number(2.0),
                }],
                "double hidden box width",
                &author,
                2,
            )
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "mantis-cli-diff-hidden-mesh-{}.json",
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(chain.to_json().as_bytes()).unwrap();
        Self { path }
    }
}

impl Drop for HiddenMeshFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn input_byte_limit_accepts_exact_boundary_and_rejects_one_more_byte() {
    // Exercise the documented production limit through the real CLI process.
    const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;
    let path = std::env::temp_dir().join(format!(
        "mantis-cli-diff-byte-limit-{}.json",
        std::process::id()
    ));
    let fixture = std::fs::read(example_chain()).unwrap();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&fixture).unwrap();
    std::io::copy(
        &mut std::io::repeat(b' ').take(MAX_INPUT_BYTES - fixture.len() as u64),
        &mut file,
    )
    .unwrap();
    drop(file);

    for extra_args in [&[][..], &["--json"][..]] {
        let expected = run_diff(extra_args);
        assert!(expected.status.success());
        let at_limit = run_diff_for_path(&path, extra_args);
        assert!(
            at_limit.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&at_limit.stderr)
        );
        assert_eq!(at_limit.stdout, expected.stdout);
        assert_eq!(at_limit.stderr, expected.stderr);
    }

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b" ").unwrap();
    drop(file);
    let oversized = run_diff_for_path(&path, &["--json"]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(oversized.status.code(), Some(1));
    assert!(oversized.stdout.is_empty());
    let stderr = String::from_utf8(oversized.stderr).unwrap();
    assert!(stderr.contains("diff input"), "{stderr}");
    assert!(stderr.contains(path.to_string_lossy().as_ref()), "{stderr}");
    assert!(stderr.contains("33554432-byte limit"), "{stderr}");
}

#[test]
fn demo_revisions_one_to_two_have_human_summary() {
    let output = run_diff(&["--from", "1", "--to", "2"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("revisions: 1 alice"), "{stdout}");
    assert!(stdout.contains(" -> 2 bob"), "{stdout}");
    assert!(
        stdout.contains("classification: geometry_changed"),
        "{stdout}"
    );
    assert!(stdout.contains("geometry (changed):"), "{stdout}");
    assert!(stdout.contains("geometry scope: preview"), "{stdout}");
    assert!(stdout.contains("object changes:"), "{stdout}");
}

#[test]
fn demo_revisions_one_to_two_serialize_the_history_report() {
    let output = run_diff(&["--from", "1", "--to", "2", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["from"]["index"], 1);
    assert_eq!(report["to"]["index"], 2);
    assert_eq!(report["commits"].as_array().unwrap().len(), 1);
    assert_eq!(report["classification"], "geometry_changed");
    assert_eq!(report["geometry"]["status"], "changed");
    assert_eq!(report["geometry"]["scope"], "preview");
}

#[test]
fn evaluator_provenance_is_stable_and_visible_without_rewriting_the_chain() {
    let path = example_chain();
    let before = std::fs::read(&path).unwrap();
    let output = run_diff(&["--from", "1", "--to", "2", "--json"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let provenance = &report["evaluator_provenance"];
    let hash = provenance["registry_contract_hash"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(hash
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    let build_id = option_env!("MANTIS_GIT_SHA")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    assert_eq!(provenance["build_id"], build_id);

    let repeated = run_diff(&["--from", "1", "--to", "2", "--json"]);
    assert!(repeated.status.success());
    assert_eq!(output.stdout, repeated.stdout);
    let human = run_diff(&["--from", "1", "--to", "2"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains(&format!(
        "evaluator: {}; build {build_id:?}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(human.contains(&format!("registry contract: sha256:{hash}")));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn default_and_explicit_preview_scope_have_identical_output() {
    for extra_args in [&[][..], &["--json"][..]] {
        let default = run_diff(extra_args);
        let mut preview_args = extra_args.to_vec();
        preview_args.extend(["--geometry-scope", "preview"]);
        let preview = run_diff(&preview_args);
        assert!(
            default.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&default.stderr)
        );
        assert!(
            preview.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&preview.stderr)
        );
        assert_eq!(default.stdout, preview.stdout);
        assert_eq!(default.stderr, preview.stderr);
    }
}

#[test]
fn all_scope_detects_hidden_mesh_changes_without_mutating_signed_input() {
    let fixture = HiddenMeshFixture::new();
    let before = std::fs::read(&fixture.path).unwrap();
    let signed_chain = Chain::from_json(std::str::from_utf8(&before).unwrap()).unwrap();

    for (scope_args, expected_scope, classification, status, object_count, modified_count) in [
        (&[][..], "preview", "definition_only", "unchanged", 0, 0),
        (
            &["--geometry-scope", "preview"][..],
            "preview",
            "definition_only",
            "unchanged",
            0,
            0,
        ),
        (
            &["--geometry-scope", "all"][..],
            "all",
            "geometry_changed",
            "changed",
            1,
            1,
        ),
    ] {
        let mut args = scope_args.to_vec();
        args.push("--json");
        let output = run_diff_for_path(&fixture.path, &args);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["geometry"]["scope"], expected_scope);
        assert_eq!(report["classification"], classification);
        assert_eq!(report["geometry"]["status"], status);
        for revision in ["before", "after"] {
            assert_eq!(
                report["geometry"][revision]["summary"]["object_count"],
                object_count
            );
        }
        assert_eq!(
            report["geometry"]["modified_objects"]
                .as_array()
                .unwrap()
                .len(),
            modified_count
        );
    }

    let output = run_diff_for_path(&fixture.path, &["--geometry-scope", "all"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("geometry scope: all"), "{stdout}");
    assert!(
        stdout.contains("classification: geometry_changed"),
        "{stdout}"
    );

    let after = std::fs::read(&fixture.path).unwrap();
    assert_eq!(after, before, "diff must not rewrite signed chain input");
    assert_eq!(
        Chain::from_json(std::str::from_utf8(&after).unwrap()).unwrap(),
        signed_chain
    );
}

#[test]
fn invalid_or_missing_geometry_scope_is_a_usage_error() {
    for args in [
        &["--geometry-scope"][..],
        &["--geometry-scope", "hidden"][..],
        &["--geometry-scope", "--json"][..],
    ] {
        let output = run_diff(args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty(), "args: {args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("--geometry-scope"), "{stderr}");
    }
}

#[test]
fn out_of_range_revision_is_a_runtime_failure() {
    let output = run_diff(&["--from", "0", "--to", "99"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("revision 99 is out of range"), "{stderr}");
    assert!(stderr.contains("head revision is 2"), "{stderr}");
}
