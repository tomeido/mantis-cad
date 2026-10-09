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
}

#[test]
fn out_of_range_revision_is_a_runtime_failure() {
    let output = run_diff(&["--from", "0", "--to", "99"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("revision 99 is out of range"), "{stderr}");
    assert!(stderr.contains("head revision is 2"), "{stderr}");
}
