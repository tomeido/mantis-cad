use std::path::PathBuf;
use std::process::{Command, Output};

fn example_chain() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/demo-chain.json")
        .canonicalize()
        .expect("the checked-in demo chain must exist")
}

fn run_diff(extra_args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mantis-cli"))
        .arg("diff")
        .arg(example_chain())
        .args(extra_args)
        // Prove the fixture does not rely on Cargo's process working directory.
        .current_dir(std::env::temp_dir())
        .output()
        .expect("mantis-cli should launch")
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
