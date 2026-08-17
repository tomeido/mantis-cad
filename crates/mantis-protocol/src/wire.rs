use crate::{
    AccessLedgerV1, AccessStateV1, ChainId, HashHex, ProjectManifestV1, ProjectSlug, ProtocolError,
    API_VERSION, PORTABLE_WORKSPACE_VERSION,
};
use mantis_chain::{Block, Chain, LEGACY_CHAIN_FORMAT_VERSION, SCOPED_CHAIN_FORMAT_VERSION};
use mantis_graph::GraphOp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiInfoV2 {
    pub api_version: u32,
    pub app_version: String,
    pub git_sha: String,
    pub supported_chain_formats: Vec<u32>,
    pub capabilities: Vec<String>,
}

impl ApiInfoV2 {
    pub fn new(app_version: impl Into<String>, git_sha: impl Into<String>) -> Self {
        Self {
            api_version: API_VERSION,
            app_version: app_version.into(),
            git_sha: git_sha.into(),
            supported_chain_formats: vec![LEGACY_CHAIN_FORMAT_VERSION, SCOPED_CHAIN_FORMAT_VERSION],
            capabilities: vec![
                "public_read".into(),
                "project_scoped_chains".into(),
                "signed_access_log".into(),
                "compare_and_swap_push".into(),
                "paginated_blocks".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainStateV1 {
    pub len: u64,
    pub head: HashHex,
    pub genesis: HashHex,
    pub total_ops: u64,
}

impl ChainStateV1 {
    pub fn from_chain(chain: &Chain) -> Result<Self, ProtocolError> {
        chain
            .validate()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        Ok(Self {
            len: chain.len() as u64,
            head: HashHex::new(chain.head().hash.clone())?,
            genesis: HashHex::new(chain.blocks[0].hash.clone())?,
            total_ops: chain.total_ops() as u64,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummaryV1 {
    pub project_id: ProjectSlug,
    pub title: String,
    pub archived: bool,
    pub chain_format_version: u32,
    pub chain_id: Option<ChainId>,
    pub genesis_hash: HashHex,
    pub state: ChainStateV1,
}

impl ProjectSummaryV1 {
    pub fn from_parts(
        manifest: &ProjectManifestV1,
        chain: &Chain,
        access: &AccessLedgerV1,
    ) -> Result<Self, ProtocolError> {
        manifest.validate_chain(chain)?;
        Ok(Self {
            project_id: manifest.project_id.clone(),
            title: access.effective_title().to_owned(),
            archived: access.effective_archived(),
            chain_format_version: manifest.chain_format_version,
            chain_id: manifest.chain_id.clone(),
            genesis_hash: manifest.genesis_hash.clone(),
            state: ChainStateV1::from_chain(chain)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfoV2 {
    pub manifest: ProjectManifestV1,
    pub state: ChainStateV1,
    pub access: AccessStateV1,
}

impl ProjectInfoV2 {
    pub fn from_parts(
        manifest: &ProjectManifestV1,
        chain: &Chain,
        access: &AccessLedgerV1,
    ) -> Result<Self, ProtocolError> {
        manifest.validate_chain(chain)?;
        Ok(Self {
            manifest: manifest.clone(),
            state: ChainStateV1::from_chain(chain)?,
            access: access.state(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlocksPageV2 {
    pub project_id: ProjectSlug,
    pub from: u64,
    pub blocks: Vec<Block>,
    pub next_from: Option<u64>,
    pub state: ChainStateV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushRequestV2 {
    pub base_len: u64,
    pub base_head: HashHex,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushResponseV2 {
    pub len: u64,
    pub head: HashHex,
    pub appended: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetailV1 {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectSlug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_record: Option<u64>,
}

impl ErrorDetailV1 {
    pub fn from_protocol(error: &ProtocolError, project: Option<ProjectSlug>) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            project,
            block: None,
            op: None,
            access_record: error.access_record_index(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponseV1 {
    pub error: ErrorDetailV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<HashHex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAccessV1 {
    Unknown,
    ReadOnly,
    Writer,
    Owner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteProjectV1 {
    /// Empty means browser same-origin. Otherwise this is a canonical absolute
    /// HTTP(S) origin with no trailing slash.
    pub base_url: String,
    pub project_id: ProjectSlug,
    pub chain_format_version: u32,
    pub chain_id: Option<ChainId>,
    pub genesis_hash: HashHex,
    pub last_synced_len: u64,
    pub last_synced_head: HashHex,
    pub access: RemoteAccessV1,
}

fn validate_remote_base_url(value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() {
        return Ok(());
    }
    if !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(ProtocolError::InvalidRemoteBaseUrl);
    }
    let authority = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .ok_or(ProtocolError::InvalidRemoteBaseUrl)?;
    if authority.is_empty()
        || authority
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'?' | b'#' | b'@' | b'\\'))
    {
        return Err(ProtocolError::InvalidRemoteBaseUrl);
    }

    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let close = ipv6.find(']').ok_or(ProtocolError::InvalidRemoteBaseUrl)?;
        let host = &ipv6[..close];
        let remainder = &ipv6[close + 1..];
        if !host.bytes().all(|byte| {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || matches!(byte, b':' | b'.')
        }) || host.parse::<std::net::Ipv6Addr>().is_err()
        {
            return Err(ProtocolError::InvalidRemoteBaseUrl);
        }
        let port = if remainder.is_empty() {
            None
        } else {
            Some(
                remainder
                    .strip_prefix(':')
                    .ok_or(ProtocolError::InvalidRemoteBaseUrl)?,
            )
        };
        (host, port)
    } else {
        if authority.matches(':').count() > 1 {
            return Err(ProtocolError::InvalidRemoteBaseUrl);
        }
        let (host, port) = authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if host.is_empty()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
            || (host
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
                && host.parse::<std::net::Ipv4Addr>().is_err())
        {
            return Err(ProtocolError::InvalidRemoteBaseUrl);
        }
        (host, port)
    };
    if host.is_empty()
        || port.is_some_and(|port| {
            port.is_empty()
                || !port.bytes().all(|byte| byte.is_ascii_digit())
                || port.parse::<u16>().ok().filter(|port| *port != 0).is_none()
        })
    {
        return Err(ProtocolError::InvalidRemoteBaseUrl);
    }
    Ok(())
}

/// Key-free interchange envelope. Device catalogs, identities, and transient
/// viewport state intentionally stay out of exported project documents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortableWorkspaceV1 {
    pub format_version: u32,
    pub id: String,
    pub name: String,
    pub updated_ms: u64,
    pub chain: Chain,
    pub pending: Vec<GraphOp>,
    #[serde(default)]
    pub recovery_ops: Vec<GraphOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteProjectV1>,
}

impl PortableWorkspaceV1 {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.format_version != PORTABLE_WORKSPACE_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                kind: "portable workspace",
                found: self.format_version,
            });
        }
        if self.id.is_empty()
            || self.id.chars().count() > 128
            || self.id.chars().any(char::is_control)
            || self.name.is_empty()
            || self.name.chars().count() > 120
            || self.name.chars().any(char::is_control)
        {
            return Err(ProtocolError::InvalidTitle);
        }
        self.chain
            .validate()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        if self
            .pending
            .iter()
            .chain(self.recovery_ops.iter())
            .any(|operation| !operation.is_finite())
        {
            return Err(ProtocolError::InvalidChain {
                code: "non_finite".into(),
            });
        }
        if let Some(remote) = &self.remote {
            validate_remote_base_url(&remote.base_url)?;
            let synced_head_matches = remote
                .last_synced_len
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| self.chain.blocks.get(index))
                .map(|block| block.hash.as_str())
                == Some(remote.last_synced_head.as_str());
            if remote.chain_format_version
                != self
                    .chain
                    .format_version()
                    .map_err(|error| ProtocolError::InvalidChain {
                        code: error.code().to_string(),
                    })?
                || remote.chain_id.as_ref().map(ChainId::as_str)
                    != self
                        .chain
                        .chain_id()
                        .map_err(|error| ProtocolError::InvalidChain {
                            code: error.code().to_string(),
                        })?
                || remote.genesis_hash.as_str() != self.chain.blocks[0].hash
                || !synced_head_matches
            {
                return Err(ProtocolError::ManifestMismatch {
                    field: "remote_anchor",
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_info_declares_both_chain_generations() {
        let info = ApiInfoV2::new("0.1.0", "abc123");
        assert_eq!(info.api_version, 2);
        assert_eq!(info.supported_chain_formats, vec![1, 2]);
        assert!(info.capabilities.contains(&"signed_access_log".into()));
    }

    #[test]
    fn portable_legacy_workspace_is_honest_about_missing_chain_id() {
        let chain = Chain::new();
        let workspace = PortableWorkspaceV1 {
            format_version: PORTABLE_WORKSPACE_VERSION,
            id: "workspace-1".into(),
            name: "Legacy".into(),
            updated_ms: 1,
            remote: Some(RemoteProjectV1 {
                base_url: "https://cad.example".into(),
                project_id: ProjectSlug::new("legacy-project").unwrap(),
                chain_format_version: LEGACY_CHAIN_FORMAT_VERSION,
                chain_id: None,
                genesis_hash: HashHex::new(chain.blocks[0].hash.clone()).unwrap(),
                last_synced_len: 1,
                last_synced_head: HashHex::new(chain.head().hash.clone()).unwrap(),
                access: RemoteAccessV1::ReadOnly,
            }),
            chain,
            pending: vec![],
            recovery_ops: vec![],
        };
        workspace.validate().unwrap();
        let json = serde_json::to_string(&workspace).unwrap();
        let decoded: PortableWorkspaceV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, workspace);
    }

    #[test]
    fn portable_workspace_rejects_a_remote_anchor_outside_its_chain_prefix() {
        let chain = Chain::new();
        let mut workspace = PortableWorkspaceV1 {
            format_version: PORTABLE_WORKSPACE_VERSION,
            id: "workspace-1".into(),
            name: "Legacy".into(),
            updated_ms: 1,
            remote: Some(RemoteProjectV1 {
                base_url: "https://cad.example".into(),
                project_id: ProjectSlug::new("legacy-project").unwrap(),
                chain_format_version: LEGACY_CHAIN_FORMAT_VERSION,
                chain_id: None,
                genesis_hash: HashHex::new(chain.blocks[0].hash.clone()).unwrap(),
                last_synced_len: 1,
                last_synced_head: HashHex::new(chain.head().hash.clone()).unwrap(),
                access: RemoteAccessV1::ReadOnly,
            }),
            chain,
            pending: vec![],
            recovery_ops: vec![],
        };
        workspace.validate().unwrap();

        workspace.remote.as_mut().unwrap().last_synced_len = 0;
        assert_eq!(
            workspace.validate(),
            Err(ProtocolError::ManifestMismatch {
                field: "remote_anchor"
            })
        );

        workspace.remote.as_mut().unwrap().last_synced_len = 2;
        assert_eq!(
            workspace.validate(),
            Err(ProtocolError::ManifestMismatch {
                field: "remote_anchor"
            })
        );

        let remote = workspace.remote.as_mut().unwrap();
        remote.last_synced_len = 1;
        remote.last_synced_head = HashHex::zero();
        assert_eq!(
            workspace.validate(),
            Err(ProtocolError::ManifestMismatch {
                field: "remote_anchor"
            })
        );
    }

    #[test]
    fn portable_workspace_remote_url_is_empty_or_a_canonical_http_origin() {
        let chain = Chain::new();
        let mut workspace = PortableWorkspaceV1 {
            format_version: PORTABLE_WORKSPACE_VERSION,
            id: "workspace-1".into(),
            name: "Remote URL".into(),
            updated_ms: 1,
            remote: Some(RemoteProjectV1 {
                base_url: String::new(),
                project_id: ProjectSlug::new("remote-project").unwrap(),
                chain_format_version: LEGACY_CHAIN_FORMAT_VERSION,
                chain_id: None,
                genesis_hash: HashHex::new(chain.head().hash.clone()).unwrap(),
                last_synced_len: 1,
                last_synced_head: HashHex::new(chain.head().hash.clone()).unwrap(),
                access: RemoteAccessV1::ReadOnly,
            }),
            chain,
            pending: vec![],
            recovery_ops: vec![],
        };
        for valid in [
            "",
            "https://cad.example",
            "http://localhost:7878",
            "https://[::1]:8443",
        ] {
            workspace.remote.as_mut().unwrap().base_url = valid.into();
            workspace.validate().unwrap();
        }
        for invalid in [
            "cad.example",
            "ftp://cad.example",
            "https://user@cad.example",
            "https://cad.example/",
            "https://cad.example/path",
            "https://cad.example?query=1",
            "https://cad.example#fragment",
            "https://CAD.example",
            "https://cad.example:0",
            " https://cad.example",
            "https://cad.example\n",
        ] {
            workspace.remote.as_mut().unwrap().base_url = invalid.into();
            assert_eq!(
                workspace.validate(),
                Err(ProtocolError::InvalidRemoteBaseUrl),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn wire_hashes_fail_closed_on_noncanonical_json_strings() {
        let json = format!(
            r#"{{"base_len":1,"base_head":"{}","blocks":[]}}"#,
            "AB".repeat(32)
        );
        assert!(serde_json::from_str::<PushRequestV2>(&json).is_err());
    }
}
