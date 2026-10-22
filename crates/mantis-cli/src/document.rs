//! File envelopes shared by the GUI and headless tools.
//!
//! Bare chains remain supported. Portable workspaces retain unsigned working
//! edits and recovery operations without inventing an identity for observation.

use crate::CliError;
use mantis_chain::Chain;
use mantis_graph::GraphOp;
use mantis_protocol::PortableWorkspaceV1;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{io::Read as _, path::Path};

pub(crate) const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Commit to the observed working definition without signing unsigned edits.
/// Struct field order, GraphOp serialization and parameter keys are stable.
pub(crate) fn working_state_hash(head: &str, pending: &[GraphOp]) -> String {
    #[derive(Serialize)]
    struct WorkingState<'a> {
        head: &'a str,
        pending: &'a [GraphOp],
    }
    let mut hash = Sha256::new();
    hash.update(b"mantis.working-state.v1\0");
    hash.update(
        serde_json::to_vec(&WorkingState { head, pending })
            .expect("validated graph operations serialize"),
    );
    format!("{:x}", hash.finalize())
}

pub(crate) enum DocumentFile {
    Chain(Chain),
    Workspace(Box<PortableWorkspaceV1>),
}

impl DocumentFile {
    pub(crate) fn load(path: &Path) -> Result<Self, CliError> {
        let file = std::fs::File::open(path).map_err(|error| {
            CliError::coded(
                "read_failed",
                format!("cannot read {}: {error}", path.display()),
                json!({"path": path}),
            )
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_DOCUMENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                CliError::coded(
                    "read_failed",
                    format!("cannot read {}: {error}", path.display()),
                    json!({"path": path}),
                )
            })?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(CliError::coded(
                "document_too_large",
                "document exceeds the 64 MiB file limit",
                json!({"path": path, "limit_bytes": MAX_DOCUMENT_BYTES}),
            ));
        }
        let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            CliError::coded(
                "invalid_chain",
                format!("invalid document JSON in {}: {error}", path.display()),
                json!({"path": path}),
            )
        })?;
        if json.get("chain").is_some() {
            let workspace: PortableWorkspaceV1 = serde_json::from_value(json).map_err(|error| {
                CliError::coded(
                    "invalid_workspace",
                    format!("invalid .mantis workspace: {error}"),
                    json!({"path": path}),
                )
            })?;
            workspace.validate().map_err(|error| {
                CliError::coded(
                    "invalid_workspace",
                    format!("invalid .mantis workspace: {error}"),
                    json!({"path": path}),
                )
            })?;
            let mut graph = workspace.chain.replay(None).map_err(|error| {
                CliError::coded(
                    "invalid_workspace",
                    format!("cannot replay workspace chain: {error}"),
                    json!({"path": path}),
                )
            })?;
            graph
                .apply_all(&workspace.pending)
                .map_err(|(index, error)| {
                    CliError::coded(
                        "invalid_workspace_pending",
                        format!("workspace pending operation {index} cannot be replayed: {error}"),
                        json!({"path": path, "op": index}),
                    )
                })?;
            Ok(Self::Workspace(Box::new(workspace)))
        } else {
            // Chain::from_json supplies the stable chain validation codes.
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                CliError::coded(
                    "invalid_chain",
                    format!("document is not UTF-8: {error}"),
                    json!({"path": path}),
                )
            })?;
            Chain::from_json(text).map(Self::Chain).map_err(|error| {
                CliError::coded(
                    "invalid_chain",
                    format!(
                        "invalid chain in {}: [{}] {error}",
                        path.display(),
                        error.code()
                    ),
                    json!({"path": path, "chain_error": error.code()}),
                )
            })
        }
    }

    pub(crate) fn chain(&self) -> &Chain {
        match self {
            Self::Chain(chain) => chain,
            Self::Workspace(workspace) => &workspace.chain,
        }
    }

    pub(crate) fn chain_mut(&mut self) -> &mut Chain {
        match self {
            Self::Chain(chain) => chain,
            Self::Workspace(workspace) => &mut workspace.chain,
        }
    }

    pub(crate) fn pending(&self) -> &[GraphOp] {
        match self {
            Self::Chain(_) => &[],
            Self::Workspace(workspace) => &workspace.pending,
        }
    }

    pub(crate) fn state_hash(&self) -> String {
        working_state_hash(&self.chain().head().hash, self.pending())
    }

    /// Called only after the caller has explicitly signed the complete batch.
    pub(crate) fn committed(&mut self, updated_ms: u64) {
        if let Self::Workspace(workspace) = self {
            workspace.pending.clear();
            workspace.updated_ms = updated_ms;
        }
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, CliError> {
        let text = match self {
            Self::Chain(chain) => chain.to_json(),
            Self::Workspace(workspace) => {
                workspace.validate().map_err(|error| {
                    CliError::coded(
                        "invalid_workspace",
                        format!("cannot save workspace: {error}"),
                        json!({}),
                    )
                })?;
                serde_json::to_string_pretty(workspace).map_err(|error| {
                    CliError::runtime(format!("cannot serialize workspace: {error}"))
                })?
            }
        };
        if text.len() + 1 > MAX_DOCUMENT_BYTES {
            return Err(CliError::coded(
                "document_too_large",
                "result exceeds the 64 MiB file limit",
                json!({"limit_bytes": MAX_DOCUMENT_BYTES}),
            ));
        }
        Ok(format!("{text}\n").into_bytes())
    }

    pub(crate) fn into_chain(self) -> Chain {
        match self {
            Self::Chain(chain) => chain,
            Self::Workspace(workspace) => workspace.chain,
        }
    }
}
