//! `diff` subcommand: compare committed graph state and currently-derived
//! geometry without making either snapshot part of the chain.

use crate::{load_chain, CliError};
use mantis_history::{
    compare_revisions, ChangeClassification, GeometryDiffStatus, HistoryDiff, RevisionGeometry,
};
use std::fmt::Write as _;

const DIFF_USAGE: &str = "usage: mantis-cli diff FILE [--from N] [--to N] [--json]";
const MAX_EVALUATION_ERRORS_SHOWN: usize = 3;

#[derive(Debug, Default, PartialEq, Eq)]
struct DiffArgs {
    path: String,
    from: Option<usize>,
    to: Option<usize>,
    json: bool,
}

pub(crate) fn cmd_diff(args: &[String]) -> Result<String, CliError> {
    let args = parse_args(args)?;
    let chain = load_chain(&args.path)?;
    let head = chain
        .len()
        .checked_sub(1)
        .ok_or_else(|| CliError::runtime("cannot compare an empty chain"))?;
    let to = args.to.unwrap_or(head);
    let from = args.from.unwrap_or_else(|| to.saturating_sub(1));

    // Range and ordering checks deliberately live in mantis-history so every
    // caller gets the same non-clamping behavior and diagnostic.
    let report = compare_revisions(&chain, from, to)
        .map_err(|error| CliError::runtime(format!("cannot compare revisions: {error}")))?;

    if args.json {
        let mut json = serde_json::to_string_pretty(&report).map_err(|error| {
            CliError::runtime(format!("cannot serialize history diff: {error}"))
        })?;
        json.push('\n');
        Ok(json)
    } else {
        Ok(format_human(&report))
    }
}

fn parse_args(args: &[String]) -> Result<DiffArgs, CliError> {
    let mut parsed = DiffArgs::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" => parsed.from = Some(parse_revision("--from", it.next())?),
            "--to" => parsed.to = Some(parse_revision("--to", it.next())?),
            "--json" => parsed.json = true,
            other if parsed.path.is_empty() && !other.starts_with("--") => {
                parsed.path = other.to_string();
            }
            other => {
                return Err(CliError::usage(format!(
                    "diff: unknown argument {other}\n\n{DIFF_USAGE}"
                )))
            }
        }
    }
    if parsed.path.is_empty() {
        return Err(CliError::usage(DIFF_USAGE));
    }
    Ok(parsed)
}

fn parse_revision(flag: &str, value: Option<&String>) -> Result<usize, CliError> {
    let value = value.ok_or_else(|| CliError::usage(format!("{flag} needs a value")))?;
    value
        .parse::<usize>()
        .map_err(|_| CliError::usage(format!("invalid {flag}: {value}")))
}

fn format_human(report: &HistoryDiff) -> String {
    let mut output = String::new();
    let operation_count: usize = report
        .commits
        .iter()
        .map(|commit| commit.operations.len())
        .sum();
    let before = &report.geometry.before.summary;
    let after = &report.geometry.after.summary;

    writeln!(
        output,
        "revisions: {} {} {:?} -> {} {} {:?}",
        report.from.index,
        report.from.author,
        report.from.message,
        report.to.index,
        report.to.author,
        report.to.message
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "commits: {}, operations: {}",
        report.commits.len(),
        operation_count
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "definition: nodes +{} -{} ~{}; edges +{} -{}",
        report.definition.added_nodes.len(),
        report.definition.removed_nodes.len(),
        report.definition.modified_nodes.len(),
        report.definition.added_connections.len(),
        report.definition.removed_connections.len()
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "classification: {}",
        classification_name(report.classification)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "geometry ({}): objects {} -> {}; points {} -> {}; curves {} -> {}; meshes {} -> {}",
        geometry_status_name(report.geometry.status),
        before.object_count,
        after.object_count,
        before.point_count,
        after.point_count,
        before.curve_count,
        after.curve_count,
        before.mesh_count,
        after.mesh_count
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "mesh metrics: area {} -> {}; signed volume {} -> {}",
        metric(before.mesh_surface_area),
        metric(after.mesh_surface_area),
        metric(before.mesh_signed_volume),
        metric(after.mesh_signed_volume)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "object changes: +{} -{} ~{}",
        report.geometry.added_objects.len(),
        report.geometry.removed_objects.len(),
        report.geometry.modified_objects.len()
    )
    .expect("writing to a String cannot fail");

    if !report.geometry.before.is_complete() || !report.geometry.after.is_complete() {
        output.push_str(incomplete_warning(report.geometry.status));
        output.push('\n');
        append_incomplete_details(&mut output, "before", &report.geometry.before);
        append_incomplete_details(&mut output, "after", &report.geometry.after);
    }
    output
}

fn incomplete_warning(status: GeometryDiffStatus) -> &'static str {
    match status {
        GeometryDiffStatus::Changed => {
            "warning: geometry changed, but the reported object sets are incomplete"
        }
        GeometryDiffStatus::Incomplete => {
            "warning: geometry comparison is incomplete; unchanged geometry cannot be guaranteed"
        }
        GeometryDiffStatus::Unchanged => {
            "warning: geometry is unchanged, but the reported object sets are incomplete"
        }
    }
}

fn append_incomplete_details(output: &mut String, label: &str, geometry: &RevisionGeometry) {
    writeln!(
        output,
        "  {label}: evaluation errors {}, truncated lists {}, invalid objects {}",
        geometry.evaluation_errors.len(),
        geometry.truncated_list_count,
        geometry.invalid_geometry_count
    )
    .expect("writing to a String cannot fail");
    for error in geometry
        .evaluation_errors
        .iter()
        .take(MAX_EVALUATION_ERRORS_SHOWN)
    {
        writeln!(
            output,
            "  {label} eval error {} ({}): {}",
            error.node_id, error.node_type, error.message
        )
        .expect("writing to a String cannot fail");
    }
    let omitted = geometry
        .evaluation_errors
        .len()
        .saturating_sub(MAX_EVALUATION_ERRORS_SHOWN);
    if omitted > 0 {
        writeln!(
            output,
            "  {label}: {omitted} more evaluation error(s) omitted"
        )
        .expect("writing to a String cannot fail");
    }
}

fn classification_name(classification: ChangeClassification) -> &'static str {
    match classification {
        ChangeClassification::NoEffect => "no_effect",
        ChangeClassification::LayoutOnly => "layout_only",
        ChangeClassification::DefinitionOnly => "definition_only",
        ChangeClassification::GeometryChanged => "geometry_changed",
        ChangeClassification::Incomplete => "incomplete",
    }
}

fn geometry_status_name(status: GeometryDiffStatus) -> &'static str {
    match status {
        GeometryDiffStatus::Unchanged => "unchanged",
        GeometryDiffStatus::Changed => "changed",
        GeometryDiffStatus::Incomplete => "incomplete",
    }
}

fn metric(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "n/a".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_chain::Identity;
    use mantis_graph::{GraphOp, NodeId, ParamValue};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn temp_chain_path() -> PathBuf {
        let sequence = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "mantis-cli-diff-test-{}-{sequence}.json",
            std::process::id()
        ))
    }

    fn write_demo_chain() -> PathBuf {
        let alice = Identity::generate("alice");
        let bob = Identity::generate("bob");
        let chain = crate::demo::build_demo_chain(&alice, &bob, 1000, 2000).unwrap();
        let path = temp_chain_path();
        std::fs::write(&path, chain.to_json()).unwrap();
        path
    }

    #[test]
    fn parser_accepts_options_in_any_order() {
        let parsed = parse_args(&strings(&[
            "--json",
            "--to",
            "4",
            "chain.json",
            "--from",
            "2",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            DiffArgs {
                path: "chain.json".into(),
                from: Some(2),
                to: Some(4),
                json: true,
            }
        );
    }

    #[test]
    fn parser_rejects_missing_path_and_bad_revisions() {
        assert!(matches!(parse_args(&[]), Err(CliError::Usage(_))));
        assert!(matches!(
            parse_args(&strings(&["chain.json", "--from", "nope"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            parse_args(&strings(&["chain.json", "--to"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn defaults_compare_previous_revision_to_head() {
        let path = write_demo_chain();
        let output = cmd_diff(&strings(&[path.to_str().unwrap(), "--json"])).unwrap();
        let report: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["from"]["index"], 1);
        assert_eq!(report["to"]["index"], 2);
        assert_eq!(report["commits"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn genesis_default_clamps_from_to_zero() {
        let path = temp_chain_path();
        std::fs::write(&path, mantis_chain::Chain::new().to_json()).unwrap();
        let output = cmd_diff(&strings(&[path.to_str().unwrap()])).unwrap();
        assert!(output.contains("revisions: 0 genesis"), "{output}");
        assert!(output.contains("commits: 0, operations: 0"), "{output}");
        assert!(output.contains("classification: no_effect"), "{output}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn history_range_errors_are_runtime_errors_without_clamping() {
        let path = write_demo_chain();
        let result = cmd_diff(&strings(&[
            path.to_str().unwrap(),
            "--from",
            "0",
            "--to",
            "99",
        ]));
        match result {
            Err(CliError::Runtime(message)) => {
                assert!(message.contains("revision 99 is out of range"), "{message}");
                assert!(message.contains("head revision is 2"), "{message}");
            }
            other => panic!("expected runtime range error, got {other:?}"),
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn human_output_contains_required_summary_sections() {
        let path = write_demo_chain();
        let output = cmd_diff(&strings(&[
            path.to_str().unwrap(),
            "--from",
            "1",
            "--to",
            "2",
        ]))
        .unwrap();
        assert!(output.contains("revisions: 1 alice"), "{output}");
        assert!(output.contains(" -> 2 bob"), "{output}");
        assert!(output.contains("commits: 1, operations:"), "{output}");
        assert!(output.contains("definition: nodes +"), "{output}");
        assert!(output.contains("; edges +"), "{output}");
        assert!(
            output.contains("classification: geometry_changed"),
            "{output}"
        );
        assert!(output.contains("geometry (changed): objects"), "{output}");
        assert!(output.contains("mesh metrics: area"), "{output}");
        assert!(output.contains("signed volume"), "{output}");
        assert!(output.contains("object changes: +"), "{output}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn incomplete_evaluation_is_an_actionable_warning() {
        let mut chain = mantis_chain::Chain::new();
        chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(9),
                    type_name: "future_component".into(),
                    pos: (0.0, 0.0),
                }],
                "unsupported node",
                &Identity::generate("alice"),
                1000,
            )
            .unwrap();
        let report = compare_revisions(&chain, 0, 1).unwrap();
        let output = format_human(&report);
        assert!(output.contains("classification: incomplete"), "{output}");
        assert!(
            output.contains("warning: geometry comparison is incomplete"),
            "{output}"
        );
        assert!(output.contains("after: evaluation errors 1"), "{output}");
        assert!(output.contains("after eval error"), "{output}");
        assert!(output.contains("future_component"), "{output}");
    }

    #[test]
    fn definite_change_with_partial_evaluation_gets_a_precise_warning() {
        let alice = Identity::generate("alice");
        let bob = Identity::generate("bob");
        let mut chain = crate::demo::build_demo_chain(&alice, &bob, 1000, 2000).unwrap();
        chain
            .append(
                vec![
                    GraphOp::SetParam {
                        id: NodeId(1_u128 << 96),
                        key: "value".into(),
                        value: ParamValue::Number(4.0),
                    },
                    GraphOp::AddNode {
                        id: NodeId(99_u128 << 96),
                        type_name: "future_component".into(),
                        pos: (0.0, 0.0),
                    },
                ],
                "visible change with partial evaluation",
                &alice,
                3000,
            )
            .unwrap();

        let changed = compare_revisions(&chain, 2, 3).unwrap();
        assert_eq!(changed.geometry.status, GeometryDiffStatus::Changed);
        assert!(!changed.geometry.after.is_complete());
        let output = format_human(&changed);
        assert!(
            output
                .contains("warning: geometry changed, but the reported object sets are incomplete"),
            "{output}"
        );
        assert!(
            !output.contains("unchanged geometry cannot be guaranteed"),
            "{output}"
        );

        let unchanged = compare_revisions(&chain, 3, 3).unwrap();
        assert_eq!(unchanged.geometry.status, GeometryDiffStatus::Unchanged);
        assert!(!unchanged.geometry.before.is_complete());
        let output = format_human(&unchanged);
        assert!(
            output.contains(
                "warning: geometry is unchanged, but the reported object sets are incomplete"
            ),
            "{output}"
        );
    }
}
