use crate::{
    crypto::{canonical_hash, canonical_json, verify_signature},
    AccessActionV1, AccessLedgerV1, AccessRecordV1, ChainId, HashHex, ProjectRoleV1, ProjectSlug,
    ProtocolError, PublicKeyHex, SignatureHex, PROJECT_CREATE_VERSION, PROJECT_MANIFEST_VERSION,
};
use mantis_chain::{Chain, Identity, LEGACY_CHAIN_FORMAT_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const CREATE_DOMAIN: &str = "mantis.project.create.v1";

pub(crate) fn validate_title(title: &str) -> Result<(), ProtocolError> {
    if title.is_empty()
        || title.trim() != title
        || title.chars().count() > 120
        || title.chars().any(char::is_control)
    {
        return Err(ProtocolError::InvalidTitle);
    }
    Ok(())
}

/// Immutable bootstrap metadata. Rename/archive state is replayed from the
/// signed access log so administrative changes remain transparent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifestV1 {
    pub schema_version: u32,
    pub project_id: ProjectSlug,
    pub title: String,
    /// `None` is reserved for an honestly migrated legacy-v1 chain.
    pub chain_id: Option<ChainId>,
    pub genesis_hash: HashHex,
    pub chain_format_version: u32,
    pub created_at_ms: u64,
    /// Trusted deployment operator that signed the bootstrap request.
    pub created_by: PublicKeyHex,
    pub initial_owner: PublicKeyHex,
    pub archived: bool,
}

impl ProjectManifestV1 {
    pub fn validate_chain(&self, chain: &Chain) -> Result<(), ProtocolError> {
        if self.schema_version != PROJECT_MANIFEST_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                kind: "project manifest",
                found: self.schema_version,
            });
        }
        validate_title(&self.title)?;
        chain
            .validate()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        let format_version =
            chain
                .format_version()
                .map_err(|error| ProtocolError::InvalidChain {
                    code: error.code().to_string(),
                })?;
        if self.chain_format_version != format_version {
            return Err(ProtocolError::ManifestMismatch {
                field: "chain_format_version",
            });
        }
        let chain_id = chain
            .chain_id()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        if self.chain_id.as_ref().map(ChainId::as_str) != chain_id {
            return Err(ProtocolError::ManifestMismatch { field: "chain_id" });
        }
        if self.genesis_hash.as_str() != chain.blocks[0].hash {
            return Err(ProtocolError::ManifestMismatch {
                field: "genesis_hash",
            });
        }
        Ok(())
    }
}

/// Operator-signed request authorizing a scoped-v2 project or an explicit
/// legacy-v1 migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCreateV1 {
    pub schema_version: u32,
    pub project_id: ProjectSlug,
    pub title: String,
    /// `Some` for normal scoped-v2 creation; `None` only for the explicit
    /// signed legacy migration path.
    pub chain_id: Option<ChainId>,
    /// Operator-approved chain checkpoint. The head commits to the complete
    /// prefix through `initial_len`, preventing a copied create proof from
    /// authorizing a different initial history.
    pub initial_len: u64,
    pub initial_head: HashHex,
    pub created_at_ms: u64,
    pub initial_owner: PublicKeyHex,
    pub operator_pk: PublicKeyHex,
    pub hash: HashHex,
    pub sig: SignatureHex,
}

#[derive(Serialize)]
struct CreateSignable<'a> {
    domain: &'static str,
    schema_version: u32,
    project_id: &'a ProjectSlug,
    title: &'a str,
    chain_id: &'a Option<ChainId>,
    initial_len: u64,
    initial_head: &'a HashHex,
    created_at_ms: u64,
    initial_owner: &'a PublicKeyHex,
    operator_pk: &'a PublicKeyHex,
}

impl ProjectCreateV1 {
    pub fn new_signed(
        project_id: ProjectSlug,
        title: impl Into<String>,
        chain_id: ChainId,
        initial_owner: PublicKeyHex,
        created_at_ms: u64,
        operator: &Identity,
    ) -> Result<Self, ProtocolError> {
        let chain =
            Chain::new_scoped(chain_id.as_str()).map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        Self::new_signed_for_chain(
            project_id,
            title,
            &chain,
            initial_owner,
            created_at_ms,
            operator,
        )
    }

    /// Sign a create proof for an already validated chain checkpoint. This is
    /// the operator-approved import path for scoped or legacy workspaces.
    pub fn new_signed_for_chain(
        project_id: ProjectSlug,
        title: impl Into<String>,
        chain: &Chain,
        initial_owner: PublicKeyHex,
        created_at_ms: u64,
        operator: &Identity,
    ) -> Result<Self, ProtocolError> {
        chain
            .validate()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        let chain_id = chain
            .chain_id()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?
            .map(ChainId::new)
            .transpose()?;
        let title = title.into();
        validate_title(&title)?;
        let operator_pk = PublicKeyHex::new(operator.public_hex())?;
        let mut request = Self {
            schema_version: PROJECT_CREATE_VERSION,
            project_id,
            title,
            chain_id,
            initial_len: u64::try_from(chain.len())
                .map_err(|_| ProtocolError::BootstrapBadCheckpoint)?,
            initial_head: HashHex::new(chain.head().hash.clone())?,
            created_at_ms,
            initial_owner,
            operator_pk,
            hash: HashHex::zero(),
            sig: SignatureHex::new("0".repeat(128))?,
        };
        request.hash = request.compute_hash()?;
        request.sig = SignatureHex::new(operator.sign_hash_hex(request.hash.as_str()))?;
        Ok(request)
    }

    fn signable(&self) -> CreateSignable<'_> {
        CreateSignable {
            domain: CREATE_DOMAIN,
            schema_version: self.schema_version,
            project_id: &self.project_id,
            title: &self.title,
            chain_id: &self.chain_id,
            initial_len: self.initial_len,
            initial_head: &self.initial_head,
            created_at_ms: self.created_at_ms,
            initial_owner: &self.initial_owner,
            operator_pk: &self.operator_pk,
        }
    }

    pub fn signable_json(&self) -> Result<String, ProtocolError> {
        canonical_json(&self.signable())
    }

    pub fn compute_hash(&self) -> Result<HashHex, ProtocolError> {
        canonical_hash(&self.signable())
    }

    pub fn verify(&self, allowed_operators: &BTreeSet<PublicKeyHex>) -> Result<(), ProtocolError> {
        if self.schema_version != PROJECT_CREATE_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                kind: "project create",
                found: self.schema_version,
            });
        }
        validate_title(&self.title)?;
        if self.initial_len == 0 {
            return Err(ProtocolError::BootstrapBadCheckpoint);
        }
        if !allowed_operators.contains(&self.operator_pk) {
            return Err(ProtocolError::UntrustedOperator);
        }
        if self.hash != self.compute_hash()? {
            return Err(ProtocolError::BadCreateHash);
        }
        if !verify_signature(&self.operator_pk, &self.hash, &self.sig) {
            return Err(ProtocolError::BadCreateSignature);
        }
        Ok(())
    }

    pub fn to_manifest(&self, chain: &Chain) -> Result<ProjectManifestV1, ProtocolError> {
        chain
            .validate()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        let chain_format_version =
            chain
                .format_version()
                .map_err(|error| ProtocolError::InvalidChain {
                    code: error.code().to_string(),
                })?;
        let actual_chain_id = chain
            .chain_id()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        if self.chain_id.as_ref().map(ChainId::as_str) != actual_chain_id {
            return Err(ProtocolError::ManifestMismatch { field: "chain_id" });
        }
        self.validate_initial_checkpoint(chain)?;
        let manifest = ProjectManifestV1 {
            schema_version: PROJECT_MANIFEST_VERSION,
            project_id: self.project_id.clone(),
            title: self.title.clone(),
            chain_id: self.chain_id.clone(),
            genesis_hash: HashHex::new(chain.blocks[0].hash.clone())?,
            chain_format_version,
            created_at_ms: self.created_at_ms,
            created_by: self.operator_pk.clone(),
            initial_owner: self.initial_owner.clone(),
            archived: false,
        };
        manifest.validate_chain(chain)?;
        Ok(manifest)
    }

    pub fn validate_initial_checkpoint(&self, chain: &Chain) -> Result<(), ProtocolError> {
        let index = self
            .initial_len
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or(ProtocolError::BootstrapBadCheckpoint)?;
        if chain.blocks.get(index).map(|block| block.hash.as_str())
            != Some(self.initial_head.as_str())
        {
            return Err(ProtocolError::BootstrapBadCheckpoint);
        }
        Ok(())
    }
}

/// Complete deterministic result of provisioning or migrating a project. The
/// caller supplies time and, for v2, randomness (`chain_id`). The signed
/// initial checkpoint and historical grants reject unapproved imported
/// authors; choosing between later competing histories authored by previously
/// granted keys still relies on a retained audit/head anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectBootstrapV1 {
    pub create: ProjectCreateV1,
    pub manifest: ProjectManifestV1,
    pub chain: Chain,
    pub access_log: Vec<AccessRecordV1>,
}

impl ProjectBootstrapV1 {
    pub fn new_signed(
        project_id: ProjectSlug,
        title: impl Into<String>,
        chain_id: ChainId,
        initial_owner: PublicKeyHex,
        created_at_ms: u64,
        operator: &Identity,
    ) -> Result<Self, ProtocolError> {
        let chain =
            Chain::new_scoped(chain_id.as_str()).map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        let create = ProjectCreateV1::new_signed_for_chain(
            project_id,
            title,
            &chain,
            initial_owner.clone(),
            created_at_ms,
            operator,
        )?;
        Self::finish_signed(create, chain, initial_owner, created_at_ms, operator)
    }

    /// Sign and wrap a pre-existing legacy-v1 chain for the one-project
    /// migration path. Scoped-v2 chains are rejected here so callers cannot
    /// accidentally erase a real chain id.
    pub fn new_legacy_signed(
        project_id: ProjectSlug,
        title: impl Into<String>,
        chain: Chain,
        initial_owner: PublicKeyHex,
        created_at_ms: u64,
        operator: &Identity,
    ) -> Result<Self, ProtocolError> {
        chain
            .validate()
            .map_err(|error| ProtocolError::InvalidChain {
                code: error.code().to_string(),
            })?;
        if chain.format_version().ok() != Some(LEGACY_CHAIN_FORMAT_VERSION)
            || chain.chain_id().ok().flatten().is_some()
        {
            return Err(ProtocolError::ManifestMismatch { field: "chain_id" });
        }
        let create = ProjectCreateV1::new_signed_for_chain(
            project_id,
            title,
            &chain,
            initial_owner.clone(),
            created_at_ms,
            operator,
        )?;
        Self::finish_signed(create, chain, initial_owner, created_at_ms, operator)
    }

    fn finish_signed(
        create: ProjectCreateV1,
        chain: Chain,
        initial_owner: PublicKeyHex,
        created_at_ms: u64,
        operator: &Identity,
    ) -> Result<Self, ProtocolError> {
        let manifest = create.to_manifest(&chain)?;
        let first_access = AccessRecordV1::new_signed(
            0,
            &manifest,
            HashHex::zero(),
            created_at_ms,
            AccessActionV1::Grant {
                public_key: initial_owner,
                role: ProjectRoleV1::Owner,
                label: None,
            },
            operator,
        )?;
        Ok(Self {
            create,
            manifest,
            chain,
            access_log: vec![first_access],
        })
    }

    pub fn verify(&self, allowed_operators: &BTreeSet<PublicKeyHex>) -> Result<(), ProtocolError> {
        self.create.verify(allowed_operators)?;
        let expected_manifest = self.create.to_manifest(&self.chain)?;
        if self.manifest != expected_manifest {
            return Err(ProtocolError::ManifestMismatch { field: "bootstrap" });
        }
        AccessLedgerV1::replay(&self.manifest, &self.access_log)?;
        let granted_keys = self
            .access_log
            .iter()
            .filter_map(|record| match &record.action {
                AccessActionV1::Grant { public_key, .. } => Some(public_key),
                _ => None,
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let checkpoint = usize::try_from(self.create.initial_len)
            .map_err(|_| ProtocolError::BootstrapBadCheckpoint)?;
        for block in &self.chain.blocks[checkpoint..] {
            let author = PublicKeyHex::new(block.author_pk.clone())
                .map_err(|_| ProtocolError::BootstrapUnauthorizedAuthor { block: block.index })?;
            if !granted_keys.contains(&author) {
                return Err(ProtocolError::BootstrapUnauthorizedAuthor { block: block.index });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mantis_graph::{GraphOp, NodeId};

    fn identities() -> (Identity, Identity) {
        (
            Identity::from_secret_hex("operator", &"11".repeat(32)).unwrap(),
            Identity::from_secret_hex("owner", &"22".repeat(32)).unwrap(),
        )
    }

    #[test]
    fn create_signable_json_is_domain_separated_and_field_ordered() {
        let (operator, owner) = identities();
        let request = ProjectCreateV1::new_signed(
            ProjectSlug::new("signed-project").unwrap(),
            "Signed Project",
            ChainId::new("aa".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            42,
            &operator,
        )
        .unwrap();
        assert_eq!(
            request.signable_json().unwrap(),
            format!(
                "{{\"domain\":\"mantis.project.create.v1\",\"schema_version\":1,\
                 \"project_id\":\"signed-project\",\"title\":\"Signed Project\",\
                 \"chain_id\":\"{}\",\"initial_len\":1,\"initial_head\":\"{}\",\
                 \"created_at_ms\":42,\
                 \"initial_owner\":\"{}\",\"operator_pk\":\"{}\"}}",
                "aa".repeat(32),
                request.initial_head,
                owner.public_hex(),
                operator.public_hex(),
            )
        );
    }

    #[test]
    fn bootstrap_verifies_only_for_configured_operator_and_detects_tampering() {
        let (operator, owner) = identities();
        let bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("signed-project").unwrap(),
            "Signed Project",
            ChainId::new("aa".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            42,
            &operator,
        )
        .unwrap();
        let allowed = BTreeSet::from([PublicKeyHex::new(operator.public_hex()).unwrap()]);
        bootstrap.verify(&allowed).unwrap();
        assert_eq!(
            bootstrap.verify(&BTreeSet::new()),
            Err(ProtocolError::UntrustedOperator)
        );

        let mut tampered = bootstrap.clone();
        tampered.create.title = "Rewritten".into();
        assert_eq!(tampered.verify(&allowed), Err(ProtocolError::BadCreateHash));

        let mut bad_signature = bootstrap;
        bad_signature.create.sig = SignatureHex::new("00".repeat(64)).unwrap();
        assert_eq!(
            bad_signature.verify(&allowed),
            Err(ProtocolError::BadCreateSignature)
        );
    }

    #[test]
    fn legacy_bootstrap_is_signed_bound_and_cannot_hide_a_scoped_id() {
        let (operator, owner) = identities();
        let legacy = Chain::new();
        let bootstrap = ProjectBootstrapV1::new_legacy_signed(
            ProjectSlug::new("legacy-project").unwrap(),
            "Legacy Project",
            legacy,
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            42,
            &operator,
        )
        .unwrap();
        assert_eq!(bootstrap.create.chain_id, None);
        assert_eq!(bootstrap.create.initial_len, bootstrap.chain.len() as u64);
        assert_eq!(
            bootstrap.create.initial_head.as_str(),
            bootstrap.chain.head().hash
        );
        assert_eq!(bootstrap.manifest.chain_id, None);
        assert_eq!(
            bootstrap.manifest.chain_format_version,
            LEGACY_CHAIN_FORMAT_VERSION
        );
        let allowed = BTreeSet::from([PublicKeyHex::new(operator.public_hex()).unwrap()]);
        bootstrap.verify(&allowed).unwrap();

        let scoped = Chain::new_scoped(&"aa".repeat(32)).unwrap();
        assert_eq!(
            ProjectBootstrapV1::new_legacy_signed(
                ProjectSlug::new("wrong-project").unwrap(),
                "Wrong Project",
                scoped,
                PublicKeyHex::new(owner.public_hex()).unwrap(),
                43,
                &operator,
            ),
            Err(ProtocolError::ManifestMismatch { field: "chain_id" })
        );
    }

    #[test]
    fn copied_create_proof_and_acl_cannot_authorize_an_ungranted_fork() {
        let (operator, owner) = identities();
        let mut bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("fork-proof").unwrap(),
            "Fork Proof",
            ChainId::new("ab".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            42,
            &operator,
        )
        .unwrap();
        let attacker = Identity::from_secret_hex("attacker", &"33".repeat(32)).unwrap();
        bootstrap
            .chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "unauthorized fork",
                &attacker,
                43,
            )
            .unwrap();
        let allowed = BTreeSet::from([PublicKeyHex::new(operator.public_hex()).unwrap()]);
        assert_eq!(
            bootstrap.verify(&allowed),
            Err(ProtocolError::BootstrapUnauthorizedAuthor { block: 1 })
        );
    }

    #[test]
    fn restore_accepts_history_from_a_writer_granted_then_revoked() {
        let (operator, owner) = identities();
        let writer = Identity::from_secret_hex("writer", &"44".repeat(32)).unwrap();
        let mut bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("restored-history").unwrap(),
            "Restored History",
            ChainId::new("cd".repeat(32)).unwrap(),
            PublicKeyHex::new(owner.public_hex()).unwrap(),
            42,
            &operator,
        )
        .unwrap();
        let writer_key = PublicKeyHex::new(writer.public_hex()).unwrap();
        let grant = AccessRecordV1::new_signed(
            1,
            &bootstrap.manifest,
            bootstrap.access_log[0].hash.clone(),
            43,
            AccessActionV1::Grant {
                public_key: writer_key.clone(),
                role: ProjectRoleV1::Writer,
                label: None,
            },
            &owner,
        )
        .unwrap();
        let revoke = AccessRecordV1::new_signed(
            2,
            &bootstrap.manifest,
            grant.hash.clone(),
            45,
            AccessActionV1::Revoke {
                public_key: writer_key,
            },
            &owner,
        )
        .unwrap();
        bootstrap.access_log.extend([grant, revoke]);
        bootstrap
            .chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "historical writer edit",
                &writer,
                44,
            )
            .unwrap();

        let allowed = BTreeSet::from([PublicKeyHex::new(operator.public_hex()).unwrap()]);
        bootstrap.verify(&allowed).unwrap();
        let replayed = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        assert!(!replayed.can_write(&PublicKeyHex::new(writer.public_hex()).unwrap()));
    }

    #[test]
    fn scoped_workspace_import_signs_its_full_initial_checkpoint() {
        let (operator, owner) = identities();
        let historical_author =
            Identity::from_secret_hex("historical author", &"55".repeat(32)).unwrap();
        let mut chain = Chain::new_scoped(&"de".repeat(32)).unwrap();
        chain
            .append(
                vec![GraphOp::AddNode {
                    id: NodeId(1),
                    type_name: "number_slider".into(),
                    pos: (0.0, 0.0),
                }],
                "pre-existing workspace edit",
                &historical_author,
                100,
            )
            .unwrap();
        let owner_key = PublicKeyHex::new(owner.public_hex()).unwrap();
        let create = ProjectCreateV1::new_signed_for_chain(
            ProjectSlug::new("workspace-import").unwrap(),
            "Workspace Import",
            &chain,
            owner_key.clone(),
            200,
            &operator,
        )
        .unwrap();
        assert_eq!(create.initial_len, chain.len() as u64);
        assert_eq!(create.initial_head.as_str(), chain.head().hash);
        let bootstrap =
            ProjectBootstrapV1::finish_signed(create, chain, owner_key, 200, &operator).unwrap();
        bootstrap
            .verify(&BTreeSet::from([
                PublicKeyHex::new(operator.public_hex()).unwrap()
            ]))
            .unwrap();
    }
}
