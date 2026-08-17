//! `mantis-admin` — signed project and membership administration.

mod key_backup;

use mantis_chain::{Chain, Identity, LEGACY_CHAIN_FORMAT_VERSION};
use mantis_protocol::{
    AccessActionV1, AccessLedgerV1, AccessRecordV1, AccessStateV1, BlocksPageV2, ChainId, HashHex,
    PortableWorkspaceV1, ProjectBootstrapV1, ProjectCreateV1, ProjectInfoV2, ProjectRoleV1,
    ProjectSlug, ProjectSummaryV1, PublicKeyHex,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_KEY_FILE_BYTES: usize = 1024 * 1024;
const MAX_JSON_FILE_BYTES: usize = 256 * 1024 * 1024;
const MAX_HTTP_ERROR_BYTES: usize = 64 * 1024;
const MAX_HTTP_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const USAGE: &str = r#"mantis-admin — signed MantisCAD project administration

Identity:
  mantis-admin identity generate --name NAME --out FILE [--password-file FILE]
  mantis-admin identity show --key FILE [--password-file FILE]

Projects:
  mantis-admin project list --server URL
  mantis-admin project create --server URL --id SLUG --title TITLE --owner-pk HEX --operator-key FILE [--from-workspace FILE] [--password-file FILE]
  mantis-admin project rename --server URL --project SLUG --title TITLE --admin-key FILE [--password-file FILE]
  mantis-admin project archive|unarchive --server URL --project SLUG --admin-key FILE [--password-file FILE]
  mantis-admin project export --server URL --project SLUG --out FILE
  mantis-admin project verify --file FILE --operator-pk HEX [--expected-project SLUG] [--expected-genesis HEX] [--expected-head HEX] [--expected-access-head HEX]
  mantis-admin project import --server URL --file FILE --operator-pk HEX --expected-head HEX --expected-access-head HEX [--expected-project SLUG] [--expected-genesis HEX]

Members:
  mantis-admin member list --server URL --project SLUG
  mantis-admin member add --server URL --project SLUG --public-key HEX --role owner|writer --admin-key FILE [--label TEXT] [--password-file FILE]
  mantis-admin member remove --server URL --project SLUG --public-key HEX --admin-key FILE [--password-file FILE]

Legacy migration:
  mantis-admin migrate-single-chain --source FILE --data-dir DIR --project SLUG --title TITLE --owner-key FILE [--password-file FILE]

Passwords are prompted without echo unless --password-file is supplied.
"#;

#[derive(Debug)]
struct Options {
    values: BTreeMap<String, String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        let mut index = 0;
        while index < args.len() {
            let key = args[index].as_str();
            if !key.starts_with("--") {
                return Err(format!("unexpected argument: {key}"));
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{key} needs a value"))?;
            if value.starts_with("--") {
                return Err(format!("{key} needs a value"));
            }
            if values.insert(key.to_string(), value.clone()).is_some() {
                return Err(format!("duplicate option: {key}"));
            }
            index += 2;
        }
        Ok(Self { values })
    }

    fn required(&self, key: &str) -> Result<&str, String> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing required option {key}"))
    }

    fn optional(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn reject_unknown(&self, allowed: &[&str]) -> Result<(), String> {
        for key in self.values.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("unknown option: {key}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectExportV1 {
    format_version: u32,
    exported_at_ms: u64,
    create: ProjectCreateV1,
    info: ProjectInfoV2,
    chain: Chain,
    access_log: Vec<AccessRecordV1>,
}

#[derive(Debug, Deserialize)]
struct AccessPageV1 {
    project_id: ProjectSlug,
    from: u64,
    records: Vec<AccessRecordV1>,
    next_from: Option<u64>,
    state: AccessStateV1,
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return;
    }
    if matches!(args[0].as_str(), "-V" | "--version") {
        println!("mantis-admin {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Err(error) = run(&args) {
        eprintln!("mantis-admin: {error}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("identity") => identity_command(&args[1..]),
        Some("project") => project_command(&args[1..]),
        Some("member") => member_command(&args[1..]),
        Some("migrate-single-chain") => migrate_command(&args[1..]),
        Some(command) => Err(format!("unknown command {command}\n\n{USAGE}")),
        None => Err(USAGE.to_string()),
    }
}

fn identity_command(args: &[String]) -> Result<(), String> {
    let (command, tail) = split_command(args)?;
    let options = Options::parse(tail)?;
    match command {
        "generate" => {
            options.reject_unknown(&["--name", "--out", "--password-file"])?;
            let name = options.required("--name")?;
            if name.trim().is_empty()
                || name.chars().count() > 120
                || name.chars().any(char::is_control)
            {
                return Err(
                    "identity name must be 1-120 characters and contain no control characters"
                        .into(),
                );
            }
            let output = PathBuf::from(options.required("--out")?);
            let password = read_password(&options, true)?;
            let identity = Identity::generate(name);
            let backup = key_backup::export(&identity, &password)?;
            write_new(&output, backup.as_bytes(), true)?;
            println!("created {}", output.display());
            println!("public key: {}", identity.public_hex());
            Ok(())
        }
        "show" => {
            options.reject_unknown(&["--key", "--password-file"])?;
            let identity = load_identity(Path::new(options.required("--key")?), &options)?;
            println!("name: {}", identity.name);
            println!("public key: {}", identity.public_hex());
            Ok(())
        }
        _ => Err(format!("unknown identity command {command}")),
    }
}

fn project_command(args: &[String]) -> Result<(), String> {
    let (command, tail) = split_command(args)?;
    let options = Options::parse(tail)?;
    match command {
        "list" => {
            options.reject_unknown(&["--server"])?;
            let server = server_url(options.required("--server")?)?;
            let projects: Vec<ProjectSummaryV1> =
                get_json(&format!("{server}/api/v2/projects?include_archived=1"))?;
            print_json(&projects)
        }
        "create" => create_project(&options),
        "rename" => {
            options.reject_unknown(&[
                "--server",
                "--project",
                "--title",
                "--admin-key",
                "--password-file",
            ])?;
            project_access_action(&options, |options| {
                Ok(AccessActionV1::Rename {
                    title: options.required("--title")?.to_string(),
                })
            })
        }
        "archive" => {
            options.reject_unknown(&["--server", "--project", "--admin-key", "--password-file"])?;
            project_access_action(&options, |_| Ok(AccessActionV1::Archive))
        }
        "unarchive" => {
            options.reject_unknown(&["--server", "--project", "--admin-key", "--password-file"])?;
            project_access_action(&options, |_| Ok(AccessActionV1::Unarchive))
        }
        "export" => export_project(&options),
        "verify" => verify_project_file(&options),
        "import" => import_project_file(&options),
        _ => Err(format!("unknown project command {command}")),
    }
}

fn member_command(args: &[String]) -> Result<(), String> {
    let (command, tail) = split_command(args)?;
    let options = Options::parse(tail)?;
    match command {
        "list" => {
            options.reject_unknown(&["--server", "--project"])?;
            let info = fetch_project_info(&options)?;
            print_json(&info.access)
        }
        "add" => {
            options.reject_unknown(&[
                "--server",
                "--project",
                "--public-key",
                "--role",
                "--label",
                "--admin-key",
                "--password-file",
            ])?;
            project_access_action(&options, |options| {
                let role = match options.required("--role")? {
                    "owner" => ProjectRoleV1::Owner,
                    "writer" => ProjectRoleV1::Writer,
                    value => return Err(format!("role must be owner or writer, got {value}")),
                };
                Ok(AccessActionV1::Grant {
                    public_key: PublicKeyHex::from_str(options.required("--public-key")?)
                        .map_err(|error| error.to_string())?,
                    role,
                    label: options.optional("--label").map(str::to_string),
                })
            })
        }
        "remove" => {
            options.reject_unknown(&[
                "--server",
                "--project",
                "--public-key",
                "--admin-key",
                "--password-file",
            ])?;
            project_access_action(&options, |options| {
                Ok(AccessActionV1::Revoke {
                    public_key: PublicKeyHex::from_str(options.required("--public-key")?)
                        .map_err(|error| error.to_string())?,
                })
            })
        }
        _ => Err(format!("unknown member command {command}")),
    }
}

fn create_project(options: &Options) -> Result<(), String> {
    options.reject_unknown(&[
        "--server",
        "--id",
        "--title",
        "--owner-pk",
        "--operator-key",
        "--from-workspace",
        "--password-file",
    ])?;
    let server = server_url(options.required("--server")?)?;
    let project =
        ProjectSlug::from_str(options.required("--id")?).map_err(|error| error.to_string())?;
    let title = options.required("--title")?;
    let owner = PublicKeyHex::from_str(options.required("--owner-pk")?)
        .map_err(|error| error.to_string())?;
    let operator = load_identity(Path::new(options.required("--operator-key")?), options)?;
    let now = now_ms()?;

    let bootstrap = if let Some(path) = options.optional("--from-workspace") {
        let workspace: PortableWorkspaceV1 = read_json_file(Path::new(path))?;
        workspace.validate().map_err(|error| error.to_string())?;
        bootstrap_from_chain(project, title, workspace.chain, owner, now, &operator)?
    } else {
        ProjectBootstrapV1::new_signed(
            project,
            title,
            ChainId::generate().map_err(|error| error.to_string())?,
            owner,
            now,
            &operator,
        )
        .map_err(|error| error.to_string())?
    };
    let info: ProjectInfoV2 = post_json(&format!("{server}/api/v2/projects"), &bootstrap)?;
    print_json(&info)
}

fn bootstrap_from_chain(
    project: ProjectSlug,
    title: &str,
    chain: Chain,
    owner: PublicKeyHex,
    now: u64,
    operator: &Identity,
) -> Result<ProjectBootstrapV1, String> {
    if chain.format_version().map_err(|error| error.to_string())? == LEGACY_CHAIN_FORMAT_VERSION {
        return ProjectBootstrapV1::new_legacy_signed(project, title, chain, owner, now, operator)
            .map_err(|error| error.to_string());
    }
    let create =
        ProjectCreateV1::new_signed_for_chain(project, title, &chain, owner.clone(), now, operator)
            .map_err(|error| error.to_string())?;
    let manifest = create
        .to_manifest(&chain)
        .map_err(|error| error.to_string())?;
    let access = AccessRecordV1::new_signed(
        0,
        &manifest,
        mantis_protocol::HashHex::zero(),
        now,
        AccessActionV1::Grant {
            public_key: owner,
            role: ProjectRoleV1::Owner,
            label: None,
        },
        operator,
    )
    .map_err(|error| error.to_string())?;
    Ok(ProjectBootstrapV1 {
        create,
        manifest,
        chain,
        access_log: vec![access],
    })
}

fn project_access_action(
    options: &Options,
    action: impl FnOnce(&Options) -> Result<AccessActionV1, String>,
) -> Result<(), String> {
    let server = server_url(options.required("--server")?)?;
    let project =
        ProjectSlug::from_str(options.required("--project")?).map_err(|error| error.to_string())?;
    let admin = load_identity(Path::new(options.required("--admin-key")?), options)?;
    let info: ProjectInfoV2 = get_json(&format!(
        "{server}/api/v2/projects/{}/info",
        project.as_str()
    ))?;
    ensure_requested_project(&project, &info)?;
    let record = AccessRecordV1::new_signed(
        info.access.len,
        &info.manifest,
        info.access.head,
        now_ms()?,
        action(options)?,
        &admin,
    )
    .map_err(|error| error.to_string())?;
    let state: AccessStateV1 = post_json(
        &format!("{server}/api/v2/projects/{}/access-log", project.as_str()),
        &vec![record],
    )?;
    print_json(&state)
}

/// Refuse to sign an ACL action when a server answers a project-scoped URL
/// with another project's manifest. Without this check, a malicious server
/// could obtain a valid owner signature for a different project and replay it
/// against that project's real access ledger.
fn ensure_requested_project(requested: &ProjectSlug, info: &ProjectInfoV2) -> Result<(), String> {
    if info.manifest.project_id != *requested {
        return Err(format!(
            "server returned project {} while {} was requested; refusing to sign",
            info.manifest.project_id, requested
        ));
    }
    Ok(())
}

fn fetch_project_info(options: &Options) -> Result<ProjectInfoV2, String> {
    let server = server_url(options.required("--server")?)?;
    let project =
        ProjectSlug::from_str(options.required("--project")?).map_err(|error| error.to_string())?;
    get_json(&format!(
        "{server}/api/v2/projects/{}/info",
        project.as_str()
    ))
}

fn export_project(options: &Options) -> Result<(), String> {
    options.reject_unknown(&["--server", "--project", "--out"])?;
    let server = server_url(options.required("--server")?)?;
    let project =
        ProjectSlug::from_str(options.required("--project")?).map_err(|error| error.to_string())?;
    let info: ProjectInfoV2 = get_json(&format!(
        "{server}/api/v2/projects/{}/info",
        project.as_str()
    ))?;
    let create: ProjectCreateV1 = get_json(&format!(
        "{server}/api/v2/projects/{}/create",
        project.as_str()
    ))?;
    let mut blocks = Vec::new();
    let mut from = 0_u64;
    loop {
        let page: BlocksPageV2 = get_json(&format!(
            "{server}/api/v2/projects/{}/blocks?from={from}&limit=4096",
            project.as_str()
        ))?;
        let next = validate_page_cursor(PageCursorCheck {
            kind: "block",
            expected_project: &project,
            requested_from: from,
            returned_project: &page.project_id,
            returned_from: page.from,
            item_count: page.blocks.len(),
            next_from: page.next_from,
            frozen_len: info.state.len,
            frozen_state_matches: page.state == info.state,
        })?;
        blocks.extend(page.blocks);
        match next {
            Some(next) => from = next,
            None => break,
        }
    }
    let chain = Chain { blocks };
    info.manifest
        .validate_chain(&chain)
        .map_err(|error| error.to_string())?;

    let mut access_log = Vec::new();
    let mut from = 0_u64;
    loop {
        let page: AccessPageV1 = get_json(&format!(
            "{server}/api/v2/projects/{}/access-log?from={from}&limit=4096",
            project.as_str()
        ))?;
        let next = validate_page_cursor(PageCursorCheck {
            kind: "access",
            expected_project: &project,
            requested_from: from,
            returned_project: &page.project_id,
            returned_from: page.from,
            item_count: page.records.len(),
            next_from: page.next_from,
            frozen_len: info.access.len,
            frozen_state_matches: page.state == info.access,
        })?;
        access_log.extend(page.records);
        match next {
            Some(next) => from = next,
            None => break,
        }
    }
    let access =
        AccessLedgerV1::replay(&info.manifest, &access_log).map_err(|error| error.to_string())?;
    if access.state().head != info.access.head {
        return Err("access log changed during export; retry".into());
    }
    let export = ProjectExportV1 {
        format_version: 1,
        exported_at_ms: now_ms()?,
        create,
        info,
        chain,
        access_log,
    };
    // The embedded key can establish internal cryptographic consistency, but
    // not deployment trust or canonicality. The output wording below keeps
    // that distinction explicit and asks for out-of-band verification.
    verify_export(
        &export,
        &std::collections::BTreeSet::from([export.create.operator_pk.clone()]),
    )?;
    let bytes = serde_json::to_vec_pretty(&export).map_err(|error| error.to_string())?;
    let output = Path::new(options.required("--out")?);
    write_new(output, &bytes, false)?;
    println!(
        "exported internally consistent project candidate to {}; verify it against a trusted operator key and external head anchors",
        output.display()
    );
    Ok(())
}

struct PageCursorCheck<'a> {
    kind: &'static str,
    expected_project: &'a ProjectSlug,
    requested_from: u64,
    returned_project: &'a ProjectSlug,
    returned_from: u64,
    item_count: usize,
    next_from: Option<u64>,
    frozen_len: u64,
    frozen_state_matches: bool,
}

fn validate_page_cursor(check: PageCursorCheck<'_>) -> Result<Option<u64>, String> {
    if check.returned_project != check.expected_project {
        return Err(format!(
            "{} page belongs to a different project",
            check.kind
        ));
    }
    if check.returned_from != check.requested_from {
        return Err(format!(
            "{} page returned cursor {}, expected {}",
            check.kind, check.returned_from, check.requested_from
        ));
    }
    if !check.frozen_state_matches {
        return Err(format!(
            "{} history changed during export; retry",
            check.kind
        ));
    }
    let count = u64::try_from(check.item_count).map_err(|_| {
        format!(
            "{} page item count does not fit the wire format",
            check.kind
        )
    })?;
    let end = check
        .returned_from
        .checked_add(count)
        .ok_or_else(|| format!("{} page cursor overflow", check.kind))?;
    if end > check.frozen_len {
        return Err(format!(
            "{} page extends beyond the frozen history",
            check.kind
        ));
    }
    match check.next_from {
        Some(next) if next == end && next > check.requested_from && next < check.frozen_len => {
            Ok(Some(next))
        }
        Some(next) => Err(format!(
            "{} page returned non-monotonic cursor {next} after {}",
            check.kind, check.requested_from
        )),
        None if end == check.frozen_len => Ok(None),
        None => Err(format!(
            "{} page ended at {end} before frozen length {}",
            check.kind, check.frozen_len
        )),
    }
}

fn verify_project_file(options: &Options) -> Result<(), String> {
    options.reject_unknown(&[
        "--file",
        "--operator-pk",
        "--expected-project",
        "--expected-genesis",
        "--expected-head",
        "--expected-access-head",
    ])?;
    let export: ProjectExportV1 = read_json_file(Path::new(options.required("--file")?))?;
    if export.format_version != 1 {
        return Err(format!(
            "unsupported project export version {}",
            export.format_version
        ));
    }
    let trusted = trusted_operator_keys(options.required("--operator-pk")?)?;
    verify_export(&export, &trusted)?;
    let anchored = verify_expected_anchors(&export, options, false)?;
    if anchored {
        println!(
            "integrity valid, operator trusted, and canonical heads externally anchored for project {}: {} blocks, head {}",
            export.info.manifest.project_id, export.info.state.len, export.info.state.head
        );
    } else {
        println!(
            "integrity valid and operator trusted for project {}: {} blocks, head {}",
            export.info.manifest.project_id, export.info.state.len, export.info.state.head
        );
        eprintln!(
            "WARNING: canonical chain/access heads were not both externally anchored; this does not distinguish an authorized historical fork"
        );
    }
    Ok(())
}

fn import_project_file(options: &Options) -> Result<(), String> {
    options.reject_unknown(&[
        "--server",
        "--file",
        "--operator-pk",
        "--expected-project",
        "--expected-genesis",
        "--expected-head",
        "--expected-access-head",
    ])?;
    let export: ProjectExportV1 = read_json_file(Path::new(options.required("--file")?))?;
    let trusted = trusted_operator_keys(options.required("--operator-pk")?)?;
    verify_export(&export, &trusted)?;
    verify_expected_anchors(&export, options, true)?;
    let server = server_url(options.required("--server")?)?;
    let bootstrap = ProjectBootstrapV1 {
        create: export.create,
        manifest: export.info.manifest,
        chain: export.chain,
        access_log: export.access_log,
    };
    let info: ProjectInfoV2 = post_json(&format!("{server}/api/v2/projects"), &bootstrap)?;
    print_json(&info)
}

fn verify_export(
    export: &ProjectExportV1,
    trusted_operators: &std::collections::BTreeSet<PublicKeyHex>,
) -> Result<(), String> {
    if export.format_version != 1 {
        return Err(format!(
            "unsupported project export version {}",
            export.format_version
        ));
    }
    let bootstrap = ProjectBootstrapV1 {
        create: export.create.clone(),
        manifest: export.info.manifest.clone(),
        chain: export.chain.clone(),
        access_log: export.access_log.clone(),
    };
    bootstrap
        .verify(trusted_operators)
        .map_err(|error| error.to_string())?;
    let access = AccessLedgerV1::replay(&export.info.manifest, &export.access_log)
        .map_err(|error| error.to_string())?;
    if mantis_protocol::ChainStateV1::from_chain(&export.chain)
        .map_err(|error| error.to_string())?
        != export.info.state
        || access.state() != export.info.access
    {
        return Err("export summaries do not match the verified ledgers".into());
    }
    Ok(())
}

fn trusted_operator_keys(value: &str) -> Result<std::collections::BTreeSet<PublicKeyHex>, String> {
    let keys = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PublicKeyHex::from_str)
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    if keys.is_empty() {
        return Err("--operator-pk must contain at least one trusted public key".into());
    }
    Ok(keys)
}

fn verify_expected_anchors(
    export: &ProjectExportV1,
    options: &Options,
    require_heads: bool,
) -> Result<bool, String> {
    if let Some(expected) = options.optional("--expected-project") {
        let expected = ProjectSlug::from_str(expected).map_err(|error| error.to_string())?;
        if export.info.manifest.project_id != expected {
            return Err(format!(
                "project anchor mismatch: expected {expected}, found {}",
                export.info.manifest.project_id
            ));
        }
    }
    if let Some(expected) = options.optional("--expected-genesis") {
        let expected = HashHex::from_str(expected).map_err(|error| error.to_string())?;
        if export.info.manifest.genesis_hash != expected {
            return Err(format!(
                "genesis anchor mismatch: expected {expected}, found {}",
                export.info.manifest.genesis_hash
            ));
        }
    }
    let expected_head = options
        .optional("--expected-head")
        .map(HashHex::from_str)
        .transpose()
        .map_err(|error| error.to_string())?;
    let expected_access_head = options
        .optional("--expected-access-head")
        .map(HashHex::from_str)
        .transpose()
        .map_err(|error| error.to_string())?;
    if require_heads && (expected_head.is_none() || expected_access_head.is_none()) {
        return Err(
            "project import requires out-of-band --expected-head and --expected-access-head anchors"
                .into(),
        );
    }
    if let Some(expected) = expected_head.as_ref() {
        if &export.info.state.head != expected {
            return Err(format!(
                "chain head anchor mismatch: expected {expected}, found {}",
                export.info.state.head
            ));
        }
    }
    if let Some(expected) = expected_access_head.as_ref() {
        if &export.info.access.head != expected {
            return Err(format!(
                "access head anchor mismatch: expected {expected}, found {}",
                export.info.access.head
            ));
        }
    }
    Ok(expected_head.is_some() && expected_access_head.is_some())
}

fn migrate_command(args: &[String]) -> Result<(), String> {
    let options = Options::parse(args)?;
    options.reject_unknown(&[
        "--source",
        "--data-dir",
        "--project",
        "--title",
        "--owner-key",
        "--password-file",
    ])?;
    let source_path = Path::new(options.required("--source")?);
    let source = read_file_limited(source_path, MAX_JSON_FILE_BYTES, "legacy chain")?;
    let source = String::from_utf8(source)
        .map_err(|_| format!("legacy chain is not UTF-8: {}", source_path.display()))?;
    let chain = Chain::from_json(&source).map_err(|error| error.to_string())?;
    if chain.format_version().map_err(|error| error.to_string())? != LEGACY_CHAIN_FORMAT_VERSION {
        return Err("migrate-single-chain accepts only a legacy v1 chain".into());
    }
    let owner = load_identity(Path::new(options.required("--owner-key")?), &options)?;
    let bootstrap = ProjectBootstrapV1::new_legacy_signed(
        ProjectSlug::from_str(options.required("--project")?).map_err(|error| error.to_string())?,
        options.required("--title")?,
        chain,
        PublicKeyHex::from_str(&owner.public_hex()).map_err(|error| error.to_string())?,
        now_ms()?,
        &owner,
    )
    .map_err(|error| error.to_string())?;
    let root = persist_migrated_project(Path::new(options.required("--data-dir")?), &bootstrap)?;
    println!("migrated legacy chain to {}", root.display());
    println!("operator/owner public key: {}", owner.public_hex());
    println!("add this public key to MANTIS_OPERATOR_KEYS before starting the server");
    Ok(())
}

fn persist_migrated_project(
    data_dir: &Path,
    bootstrap: &ProjectBootstrapV1,
) -> Result<PathBuf, String> {
    // Serialize and validate every artifact before touching the destination.
    bootstrap
        .verify(&std::collections::BTreeSet::from([bootstrap
            .create
            .operator_pk
            .clone()]))
        .map_err(|error| format!("invalid migration bootstrap: {error}"))?;
    if bootstrap
        .chain
        .format_version()
        .map_err(|error| format!("invalid migration chain: {error}"))?
        != LEGACY_CHAIN_FORMAT_VERSION
        || bootstrap.manifest.chain_id.is_some()
    {
        return Err("legacy migration requires a v1 chain without a chain id".into());
    }
    let documents = [
        (
            "project-create.json",
            serde_json::to_vec_pretty(&bootstrap.create),
        ),
        (
            "manifest.json",
            serde_json::to_vec_pretty(&bootstrap.manifest),
        ),
        ("chain.json", serde_json::to_vec_pretty(&bootstrap.chain)),
        (
            "access-log.json",
            serde_json::to_vec_pretty(&bootstrap.access_log),
        ),
    ]
    .map(|(name, bytes)| {
        bytes
            .map(|bytes| (name, bytes))
            .map_err(|error| format!("cannot serialize {name}: {error}"))
    });
    let documents = documents.into_iter().collect::<Result<Vec<_>, String>>()?;

    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("cannot create {}: {error}", data_dir.display()))?;
    let projects_dir = data_dir.join("projects");
    std::fs::create_dir_all(&projects_dir)
        .map_err(|error| format!("cannot create {}: {error}", projects_dir.display()))?;
    sync_directory(data_dir)
        .map_err(|error| format!("cannot sync {}: {error}", data_dir.display()))?;

    reject_existing_legacy_project(&projects_dir)?;
    let final_root = projects_dir.join(bootstrap.manifest.project_id.as_str());
    if final_root.exists() {
        return Err(format!(
            "project directory already exists: {}",
            final_root.display()
        ));
    }
    let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let staging = projects_dir.join(format!(
        ".migrating-{}-{}-{sequence}",
        bootstrap.manifest.project_id,
        std::process::id()
    ));
    std::fs::create_dir(&staging)
        .map_err(|error| format!("cannot create {}: {error}", staging.display()))?;

    let staged = (|| {
        for (name, bytes) in &documents {
            write_new(&staging.join(name), bytes, false)?;
        }
        sync_directory(&staging)
            .map_err(|error| format!("cannot sync {}: {error}", staging.display()))?;
        std::fs::rename(&staging, &final_root).map_err(|error| {
            format!(
                "cannot publish {} as {}: {error}",
                staging.display(),
                final_root.display()
            )
        })?;
        if let Err(error) = sync_directory(&projects_dir) {
            let rollback =
                std::fs::remove_dir_all(&final_root).and_then(|()| sync_directory(&projects_dir));
            return Err(match rollback {
                Ok(()) => format!("cannot sync {}: {error}", projects_dir.display()),
                Err(rollback) => format!(
                    "cannot sync {}: {error}; rollback failed: {rollback}",
                    projects_dir.display()
                ),
            });
        }
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(final_root)
}

fn reject_existing_legacy_project(projects_dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(projects_dir)
        .map_err(|error| format!("cannot list {}: {error}", projects_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("cannot inspect project store: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", entry.path().display()))?
            .is_dir()
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let manifest_path = entry.path().join("manifest.json");
        let manifest: mantis_protocol::ProjectManifestV1 = read_json_file(&manifest_path)
            .map_err(|error| format!("cannot validate existing project store: {error}"))?;
        if manifest.chain_id.is_none() {
            return Err(format!(
                "legacy project already exists: {}",
                manifest.project_id
            ));
        }
    }
    Ok(())
}

fn split_command(args: &[String]) -> Result<(&str, &[String]), String> {
    args.split_first()
        .map(|(command, tail)| (command.as_str(), tail))
        .ok_or_else(|| USAGE.to_string())
}

fn server_url(value: &str) -> Result<String, String> {
    let parsed =
        url::Url::parse(value.trim()).map_err(|error| format!("invalid server URL: {error}"))?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err("server URL must use https:// or http://".into());
    }
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(
            "server URL must be an origin without credentials, path, query, or fragment".into(),
        );
    }
    Ok(parsed.origin().ascii_serialization())
}

fn read_password(options: &Options, confirm: bool) -> Result<String, String> {
    if let Some(path) = options.optional("--password-file") {
        let password = read_file_limited(Path::new(path), 64 * 1024, "password file")?;
        let password = String::from_utf8(password)
            .map_err(|_| "password file is not valid UTF-8".to_string())?;
        let password = password.trim_end_matches(['\r', '\n']).to_string();
        if password.is_empty() {
            return Err("password file is empty".into());
        }
        return Ok(password);
    }
    let password = rpassword::prompt_password("Key password: ")
        .map_err(|error| format!("cannot read password: {error}"))?;
    if confirm {
        let second = rpassword::prompt_password("Confirm password: ")
            .map_err(|error| format!("cannot read password confirmation: {error}"))?;
        if password != second {
            return Err("passwords do not match".into());
        }
    }
    Ok(password)
}

fn load_identity(path: &Path, options: &Options) -> Result<Identity, String> {
    let json = read_file_limited(path, MAX_KEY_FILE_BYTES, "identity key")?;
    let json = String::from_utf8(json)
        .map_err(|_| format!("identity key is not UTF-8: {}", path.display()))?;
    key_backup::import(&json, &read_password(options, false)?)
}

fn now_ms() -> Result<u64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| "timestamp does not fit in u64".to_string())
}

fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let response = ureq::get(url)
        .timeout(HTTP_TIMEOUT)
        .call()
        .map_err(http_error)?;
    decode_json_response(response, url)
}

fn post_json<T: Serialize, R: DeserializeOwned>(url: &str, value: &T) -> Result<R, String> {
    let body = serde_json::to_vec(value)
        .map_err(|error| format!("cannot serialize request for {url}: {error}"))?;
    if body.len() > MAX_HTTP_REQUEST_BYTES {
        return Err(format!(
            "request for {url} is {} bytes; server limit is {MAX_HTTP_REQUEST_BYTES} bytes",
            body.len()
        ));
    }
    let response = ureq::post(url)
        .timeout(HTTP_TIMEOUT)
        .set("Content-Type", "application/json")
        .send_bytes(&body)
        .map_err(http_error)?;
    decode_json_response(response, url)
}

fn decode_json_response<T: DeserializeOwned>(
    response: ureq::Response,
    url: &str,
) -> Result<T, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_JSON_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read response from {url}: {error}"))?;
    if bytes.len() > MAX_JSON_FILE_BYTES {
        return Err(format!(
            "response from {url} exceeds {MAX_JSON_FILE_BYTES} bytes"
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid JSON from {url}: {error}"))
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, response) => {
            let mut bytes = Vec::new();
            let _ = response
                .into_reader()
                .take(MAX_HTTP_ERROR_BYTES as u64 + 1)
                .read_to_end(&mut bytes);
            let truncated = bytes.len() > MAX_HTTP_ERROR_BYTES;
            bytes.truncate(MAX_HTTP_ERROR_BYTES);
            let mut body = String::from_utf8_lossy(&bytes).into_owned();
            if truncated {
                body.push('…');
            }
            if body.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {body}")
            }
        }
        ureq::Error::Transport(error) => format!("network error: {error}"),
    }
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn read_json_file<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = read_file_limited(path, MAX_JSON_FILE_BYTES, "JSON file")?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

fn read_file_limited(path: &Path, limit: usize, label: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open {label} {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label} {}: {error}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!("{label} exceeds {limit} bytes: {}", path.display()));
    }
    Ok(bytes)
}

fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    let tmp = write_temporary_sibling(path, bytes, private)?;
    let result = (|| {
        // A hard link is an atomic, no-clobber publish on the same filesystem.
        // Checking `exists` before a replacing rename leaves a race in which a
        // concurrently created key backup could be overwritten.
        std::fs::hard_link(&tmp, path).map_err(|error| {
            if path.exists() {
                format!("refusing to overwrite {}", path.display())
            } else {
                format!("cannot create {}: {error}", path.display())
            }
        })?;
        if let Err(error) = std::fs::remove_file(&tmp) {
            let rollback = rollback_new_file(path);
            return Err(match rollback {
                Ok(()) => format!("cannot remove temporary file {}: {error}", tmp.display()),
                Err(rollback) => format!(
                    "cannot remove temporary file {}: {error}; rollback of {} failed: {rollback}",
                    tmp.display(),
                    path.display()
                ),
            });
        }
        if let Err(error) = sync_parent(path) {
            let rollback = rollback_new_file(path);
            return Err(match rollback {
                Ok(()) => error,
                Err(rollback) => {
                    format!("{error}; rollback of {} failed: {rollback}", path.display())
                }
            });
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn rollback_new_file(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path)
        .map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
    sync_parent(path)
}

fn write_temporary_sibling(path: &Path, bytes: &[u8], private: bool) -> Result<PathBuf, String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", tmp.display()))?;
        Ok(tmp.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_directory(parent).map_err(|error| format!("cannot sync {}: {error}", parent.display()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let sequence = TEST_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir().join(format!(
            "mantis-admin-test-{}-{sequence}-{tag}",
            std::process::id()
        ))
    }

    #[test]
    fn option_parser_is_strict() {
        let options = Options::parse(&[
            "--server".into(),
            "https://example.test".into(),
            "--project".into(),
            "demo".into(),
        ])
        .unwrap();
        assert_eq!(options.required("--project").unwrap(), "demo");
        assert!(options.reject_unknown(&["--server", "--project"]).is_ok());
        assert!(Options::parse(&["project".into()]).is_err());
        assert!(Options::parse(&["--x".into(), "1".into(), "--x".into(), "2".into()]).is_err());
    }

    #[test]
    fn server_url_requires_an_explicit_scheme() {
        assert_eq!(
            server_url("https://example.test/").unwrap(),
            "https://example.test"
        );
        assert_eq!(
            server_url(" http://localhost:7878/ ").unwrap(),
            "http://localhost:7878"
        );
        assert!(server_url("example.test").is_err());
        for invalid in [
            "ftp://example.test",
            "https://user@example.test",
            "https://example.test/path",
            "https://example.test?query=1",
            "https://example.test/#fragment",
        ] {
            assert!(server_url(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn write_new_is_atomic_and_never_overwrites() {
        let root = temp_dir("write-new");
        let path = root.join("nested/key.mantis-key");
        write_new(&path, b"first", true).unwrap();
        let error = write_new(&path, b"second", true).unwrap_err();
        assert!(error.contains("refusing to overwrite"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert!(std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_migration_publishes_a_complete_project_and_rejects_a_second() {
        let data_dir = temp_dir("legacy-migration");
        let operator = Identity::generate("legacy owner");
        let project = ProjectSlug::new("legacy-one").unwrap();
        let bootstrap = ProjectBootstrapV1::new_legacy_signed(
            project.clone(),
            "Legacy One",
            Chain::new(),
            PublicKeyHex::new(operator.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();

        let root = persist_migrated_project(&data_dir, &bootstrap).unwrap();
        assert_eq!(root, data_dir.join("projects").join(project.as_str()));
        for name in [
            "project-create.json",
            "manifest.json",
            "chain.json",
            "access-log.json",
        ] {
            assert!(root.join(name).is_file(), "missing {name}");
        }
        let restored = ProjectBootstrapV1 {
            create: read_json_file(&root.join("project-create.json")).unwrap(),
            manifest: read_json_file(&root.join("manifest.json")).unwrap(),
            chain: read_json_file(&root.join("chain.json")).unwrap(),
            access_log: read_json_file(&root.join("access-log.json")).unwrap(),
        };
        restored
            .verify(&std::collections::BTreeSet::from([bootstrap
                .create
                .operator_pk
                .clone()]))
            .unwrap();
        assert!(std::fs::read_dir(data_dir.join("projects"))
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".migrating-")));

        let second = ProjectBootstrapV1::new_legacy_signed(
            ProjectSlug::new("legacy-two").unwrap(),
            "Legacy Two",
            Chain::new(),
            PublicKeyHex::new(operator.public_hex()).unwrap(),
            200,
            &operator,
        )
        .unwrap();
        let error = persist_migrated_project(&data_dir, &second).unwrap_err();
        assert!(error.contains("legacy project already exists"), "{error}");
        assert!(!data_dir.join("projects/legacy-two").exists());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn import_verification_rejects_tampered_export_summaries() {
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("summary-check").unwrap(),
            "Summary Check",
            ChainId::new("ab".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let access = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let mut export = ProjectExportV1 {
            format_version: 1,
            exported_at_ms: 200,
            create: bootstrap.create,
            info: ProjectInfoV2::from_parts(&bootstrap.manifest, &bootstrap.chain, &access)
                .unwrap(),
            chain: bootstrap.chain,
            access_log: bootstrap.access_log,
        };
        let trusted = std::collections::BTreeSet::from([export.create.operator_pk.clone()]);
        verify_export(&export, &trusted).unwrap();
        let untrusted = std::collections::BTreeSet::from([PublicKeyHex::new(
            Identity::generate("other operator").public_hex(),
        )
        .unwrap()]);
        assert!(verify_export(&export, &untrusted)
            .unwrap_err()
            .contains("trusted operator"));

        let no_anchors = Options::parse(&[]).unwrap();
        assert!(verify_expected_anchors(&export, &no_anchors, true)
            .unwrap_err()
            .contains("requires out-of-band"));
        let anchored = Options::parse(&[
            "--expected-head".into(),
            export.info.state.head.to_string(),
            "--expected-access-head".into(),
            export.info.access.head.to_string(),
        ])
        .unwrap();
        assert!(verify_expected_anchors(&export, &anchored, true).unwrap());

        export.info.state.len += 1;
        assert!(verify_export(&export, &trusted)
            .unwrap_err()
            .contains("summaries do not match"));
    }

    #[test]
    fn access_signing_is_bound_to_the_requested_project() {
        let operator = Identity::generate("operator");
        let owner = Identity::generate("owner");
        let bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("actual-project").unwrap(),
            "Actual Project",
            ChainId::new("cd".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            100,
            &operator,
        )
        .unwrap();
        let access = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let info =
            ProjectInfoV2::from_parts(&bootstrap.manifest, &bootstrap.chain, &access).unwrap();

        ensure_requested_project(&ProjectSlug::new("actual-project").unwrap(), &info).unwrap();
        let error =
            ensure_requested_project(&ProjectSlug::new("requested-project").unwrap(), &info)
                .unwrap_err();
        assert!(error.contains("refusing to sign"), "{error}");
    }

    #[test]
    fn export_page_cursors_are_project_bound_frozen_and_monotonic() {
        let project = ProjectSlug::new("page-contract").unwrap();
        let other_project = ProjectSlug::new("other-project").unwrap();
        assert_eq!(
            validate_page_cursor(PageCursorCheck {
                kind: "block",
                expected_project: &project,
                requested_from: 0,
                returned_project: &project,
                returned_from: 0,
                item_count: 2,
                next_from: Some(2),
                frozen_len: 3,
                frozen_state_matches: true,
            })
            .unwrap(),
            Some(2)
        );
        assert_eq!(
            validate_page_cursor(PageCursorCheck {
                kind: "access",
                expected_project: &project,
                requested_from: 2,
                returned_project: &project,
                returned_from: 2,
                item_count: 1,
                next_from: None,
                frozen_len: 3,
                frozen_state_matches: true,
            })
            .unwrap(),
            None
        );

        for check in [
            PageCursorCheck {
                kind: "block",
                expected_project: &project,
                requested_from: 0,
                returned_project: &other_project,
                returned_from: 0,
                item_count: 1,
                next_from: Some(1),
                frozen_len: 3,
                frozen_state_matches: true,
            },
            PageCursorCheck {
                kind: "block",
                expected_project: &project,
                requested_from: 0,
                returned_project: &project,
                returned_from: 0,
                item_count: 0,
                next_from: Some(0),
                frozen_len: 3,
                frozen_state_matches: true,
            },
            PageCursorCheck {
                kind: "block",
                expected_project: &project,
                requested_from: 0,
                returned_project: &project,
                returned_from: 1,
                item_count: 1,
                next_from: Some(2),
                frozen_len: 3,
                frozen_state_matches: true,
            },
            PageCursorCheck {
                kind: "access",
                expected_project: &project,
                requested_from: 0,
                returned_project: &project,
                returned_from: 0,
                item_count: 1,
                next_from: None,
                frozen_len: 3,
                frozen_state_matches: true,
            },
            PageCursorCheck {
                kind: "access",
                expected_project: &project,
                requested_from: 0,
                returned_project: &project,
                returned_from: 0,
                item_count: 1,
                next_from: Some(1),
                frozen_len: 3,
                frozen_state_matches: false,
            },
        ] {
            assert!(validate_page_cursor(check).is_err());
        }
    }
}
