use crate::{
    crypto::{canonical_hash, canonical_json, verify_signature},
    project::validate_title,
    ChainId, HashHex, ProjectManifestV1, ProjectSlug, ProtocolError, PublicKeyHex, SignatureHex,
    ACCESS_RECORD_VERSION,
};
use mantis_chain::Identity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const ACCESS_DOMAIN: &str = "mantis.project.access.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRoleV1 {
    Owner,
    Writer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccessActionV1 {
    Grant {
        public_key: PublicKeyHex,
        role: ProjectRoleV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Revoke {
        public_key: PublicKeyHex,
    },
    Rename {
        title: String,
    },
    Archive,
    Unarchive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRecordV1 {
    pub schema_version: u32,
    pub index: u64,
    pub project_id: ProjectSlug,
    /// `None` only for an access log attached to a legacy-v1 chain.
    pub chain_id: Option<ChainId>,
    /// Always binds the log to the exact chain, including legacy chains.
    pub genesis_hash: HashHex,
    pub prev_hash: HashHex,
    pub timestamp_ms: u64,
    pub actor_pk: PublicKeyHex,
    pub action: AccessActionV1,
    pub hash: HashHex,
    pub sig: SignatureHex,
}

#[derive(Serialize)]
struct AccessSignable<'a> {
    domain: &'static str,
    schema_version: u32,
    index: u64,
    project_id: &'a ProjectSlug,
    chain_id: &'a Option<ChainId>,
    genesis_hash: &'a HashHex,
    prev_hash: &'a HashHex,
    timestamp_ms: u64,
    actor_pk: &'a PublicKeyHex,
    action: &'a AccessActionV1,
}

impl AccessRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new_signed(
        index: u64,
        manifest: &ProjectManifestV1,
        prev_hash: HashHex,
        timestamp_ms: u64,
        action: AccessActionV1,
        actor: &Identity,
    ) -> Result<Self, ProtocolError> {
        validate_action_content(&action)?;
        let mut record = Self {
            schema_version: ACCESS_RECORD_VERSION,
            index,
            project_id: manifest.project_id.clone(),
            chain_id: manifest.chain_id.clone(),
            genesis_hash: manifest.genesis_hash.clone(),
            prev_hash,
            timestamp_ms,
            actor_pk: PublicKeyHex::new(actor.public_hex())?,
            action,
            hash: HashHex::zero(),
            sig: SignatureHex::new("0".repeat(128))?,
        };
        record.hash = record.compute_hash()?;
        record.sig = SignatureHex::new(actor.sign_hash_hex(record.hash.as_str()))?;
        Ok(record)
    }

    fn signable(&self) -> AccessSignable<'_> {
        AccessSignable {
            domain: ACCESS_DOMAIN,
            schema_version: self.schema_version,
            index: self.index,
            project_id: &self.project_id,
            chain_id: &self.chain_id,
            genesis_hash: &self.genesis_hash,
            prev_hash: &self.prev_hash,
            timestamp_ms: self.timestamp_ms,
            actor_pk: &self.actor_pk,
            action: &self.action,
        }
    }

    pub fn signable_json(&self) -> Result<String, ProtocolError> {
        canonical_json(&self.signable())
    }

    pub fn compute_hash(&self) -> Result<HashHex, ProtocolError> {
        canonical_hash(&self.signable())
    }

    pub fn verify_signature(&self) -> bool {
        verify_signature(&self.actor_pk, &self.hash, &self.sig)
    }
}

fn validate_label(label: &Option<String>) -> Result<(), ProtocolError> {
    if let Some(label) = label {
        if label.is_empty()
            || label.trim() != label
            || label.chars().count() > 80
            || label.chars().any(char::is_control)
        {
            return Err(ProtocolError::InvalidLabel);
        }
    }
    Ok(())
}

fn validate_action_content(action: &AccessActionV1) -> Result<(), ProtocolError> {
    match action {
        AccessActionV1::Grant { label, .. } => validate_label(label),
        AccessActionV1::Rename { title } => validate_title(title),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessMemberV1 {
    pub public_key: PublicKeyHex,
    pub role: ProjectRoleV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub updated_at_ms: u64,
    pub updated_by: PublicKeyHex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessStateV1 {
    pub len: u64,
    pub head: HashHex,
    pub members: Vec<AccessMemberV1>,
    pub title: String,
    pub archived: bool,
}

/// Fully replayed ACL and transparent project-administration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessLedgerV1 {
    manifest: ProjectManifestV1,
    records: Vec<AccessRecordV1>,
    members: BTreeMap<PublicKeyHex, AccessMemberV1>,
    effective_title: String,
    effective_archived: bool,
}

impl AccessLedgerV1 {
    pub fn replay(
        manifest: &ProjectManifestV1,
        records: &[AccessRecordV1],
    ) -> Result<Self, ProtocolError> {
        if manifest.schema_version != crate::PROJECT_MANIFEST_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                kind: "project manifest",
                found: manifest.schema_version,
            });
        }
        validate_title(&manifest.title)?;
        if records.is_empty() {
            return Err(ProtocolError::AccessBootstrapMismatch { at: 0 });
        }

        let mut ledger = Self {
            manifest: manifest.clone(),
            records: Vec::with_capacity(records.len()),
            members: BTreeMap::new(),
            effective_title: manifest.title.clone(),
            effective_archived: manifest.archived,
        };

        for record in records {
            ledger.verify_next(record)?;
            ledger.apply_next(record)?;
            ledger.records.push(record.clone());
        }
        Ok(ledger)
    }

    fn verify_next(&self, record: &AccessRecordV1) -> Result<(), ProtocolError> {
        let expected_index = self.records.len() as u64;
        let expected_prev = self
            .records
            .last()
            .map(|previous| previous.hash.clone())
            .unwrap_or_else(HashHex::zero);
        self.verify_record_crypto(record, expected_index, &expected_prev)
    }

    fn verify_record_crypto(
        &self,
        record: &AccessRecordV1,
        expected_index: u64,
        expected_prev: &HashHex,
    ) -> Result<(), ProtocolError> {
        if record.schema_version != ACCESS_RECORD_VERSION {
            return Err(ProtocolError::AccessUnsupportedVersion {
                at: expected_index,
                found: record.schema_version,
            });
        }
        if record.index != expected_index {
            return Err(ProtocolError::AccessBadIndex { at: expected_index });
        }
        if record.project_id != self.manifest.project_id {
            return Err(ProtocolError::AccessProjectMismatch { at: expected_index });
        }
        if record.chain_id != self.manifest.chain_id
            || record.genesis_hash != self.manifest.genesis_hash
        {
            return Err(ProtocolError::AccessChainMismatch { at: expected_index });
        }
        if record.prev_hash != *expected_prev {
            return Err(ProtocolError::AccessBadPrevHash { at: expected_index });
        }
        validate_action_content(&record.action)
            .map_err(|_| ProtocolError::AccessInvalidAction { at: expected_index })?;
        if record.hash != record.compute_hash()? {
            return Err(ProtocolError::AccessBadHash { at: expected_index });
        }
        if !record.verify_signature() {
            return Err(ProtocolError::AccessBadSignature { at: expected_index });
        }
        Ok(())
    }

    /// Verify only the incoming hash-chain links, action encoding, hashes, and
    /// signatures against this trusted ledger head.
    ///
    /// This deliberately does not replay the accepted prefix or evaluate ACL
    /// authorization. A server can use it to prove possession of the claimed
    /// actor keys before consuming their rate-limit tokens, then call
    /// [`AccessLedgerV1::try_extend`] for the atomic authorization update.
    pub fn verify_extension_crypto(
        &self,
        records: &[AccessRecordV1],
    ) -> Result<usize, ProtocolError> {
        let mut expected_index = self.records.len() as u64;
        let mut expected_prev = self
            .records
            .last()
            .map(|record| record.hash.clone())
            .unwrap_or_else(HashHex::zero);
        for record in records {
            self.verify_record_crypto(record, expected_index, &expected_prev)?;
            expected_index = expected_index
                .checked_add(1)
                .ok_or(ProtocolError::AccessBadIndex { at: expected_index })?;
            expected_prev = record.hash.clone();
        }
        Ok(records.len())
    }

    fn apply_next(&mut self, record: &AccessRecordV1) -> Result<(), ProtocolError> {
        let at = record.index;
        if at == 0 {
            let exact_bootstrap = record.actor_pk == self.manifest.created_by
                && matches!(
                    &record.action,
                    AccessActionV1::Grant {
                        public_key,
                        role: ProjectRoleV1::Owner,
                        label: None,
                    } if *public_key == self.manifest.initial_owner
                );
            if !exact_bootstrap {
                return Err(ProtocolError::AccessBootstrapMismatch { at });
            }
        } else if self.role(&record.actor_pk) != Some(ProjectRoleV1::Owner) {
            return Err(ProtocolError::AccessUnauthorized { at });
        }

        match &record.action {
            AccessActionV1::Grant {
                public_key,
                role,
                label,
            } => {
                if let Some(existing) = self.members.get(public_key) {
                    if existing.role == *role && existing.label == *label {
                        return Err(ProtocolError::AccessNoop { at });
                    }
                    if existing.role == ProjectRoleV1::Owner
                        && *role != ProjectRoleV1::Owner
                        && self.owner_count() == 1
                    {
                        return Err(ProtocolError::AccessLastOwner { at });
                    }
                }
                self.members.insert(
                    public_key.clone(),
                    AccessMemberV1 {
                        public_key: public_key.clone(),
                        role: *role,
                        label: label.clone(),
                        updated_at_ms: record.timestamp_ms,
                        updated_by: record.actor_pk.clone(),
                    },
                );
            }
            AccessActionV1::Revoke { public_key } => {
                let Some(existing) = self.members.get(public_key) else {
                    return Err(ProtocolError::AccessUnknownMember { at });
                };
                if existing.role == ProjectRoleV1::Owner && self.owner_count() == 1 {
                    return Err(ProtocolError::AccessLastOwner { at });
                }
                self.members.remove(public_key);
            }
            AccessActionV1::Rename { title } => {
                if *title == self.effective_title {
                    return Err(ProtocolError::AccessNoop { at });
                }
                self.effective_title.clone_from(title);
            }
            AccessActionV1::Archive => {
                if self.effective_archived {
                    return Err(ProtocolError::AccessNoop { at });
                }
                self.effective_archived = true;
            }
            AccessActionV1::Unarchive => {
                if !self.effective_archived {
                    return Err(ProtocolError::AccessNoop { at });
                }
                self.effective_archived = false;
            }
        }
        Ok(())
    }

    /// Atomically append access records. On any failure this ledger is
    /// unchanged, mirroring `Chain::try_extend` semantics.
    pub fn try_extend(&mut self, records: &[AccessRecordV1]) -> Result<usize, ProtocolError> {
        let original_len = self.records.len();
        let original_members = self.members.clone();
        let original_title = self.effective_title.clone();
        let original_archived = self.effective_archived;
        for record in records {
            let result = self
                .verify_next(record)
                .and_then(|()| self.apply_next(record));
            if let Err(error) = result {
                self.records.truncate(original_len);
                self.members = original_members;
                self.effective_title = original_title;
                self.effective_archived = original_archived;
                return Err(error);
            }
            self.records.push(record.clone());
        }
        Ok(records.len())
    }

    pub fn records(&self) -> &[AccessRecordV1] {
        &self.records
    }

    pub fn members(&self) -> &BTreeMap<PublicKeyHex, AccessMemberV1> {
        &self.members
    }

    pub fn role(&self, public_key: &PublicKeyHex) -> Option<ProjectRoleV1> {
        self.members.get(public_key).map(|member| member.role)
    }

    pub fn can_write(&self, public_key: &PublicKeyHex) -> bool {
        self.role(public_key).is_some()
    }

    pub fn owner_count(&self) -> usize {
        self.members
            .values()
            .filter(|member| member.role == ProjectRoleV1::Owner)
            .count()
    }

    pub fn effective_title(&self) -> &str {
        &self.effective_title
    }

    pub fn effective_archived(&self) -> bool {
        self.effective_archived
    }

    pub fn state(&self) -> AccessStateV1 {
        AccessStateV1 {
            len: self.records.len() as u64,
            head: self
                .records
                .last()
                .map(|record| record.hash.clone())
                .unwrap_or_else(HashHex::zero),
            members: self.members.values().cloned().collect(),
            title: self.effective_title.clone(),
            archived: self.effective_archived,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChainId, ProjectBootstrapV1, ProjectSlug};

    fn fixture() -> (ProjectBootstrapV1, Identity, Identity, Identity) {
        let operator = Identity::from_secret_hex("operator", &"11".repeat(32)).unwrap();
        let owner = Identity::from_secret_hex("owner", &"22".repeat(32)).unwrap();
        let writer = Identity::from_secret_hex("writer", &"33".repeat(32)).unwrap();
        let owner_pk = PublicKeyHex::new(owner.public_hex()).unwrap();
        let bootstrap = ProjectBootstrapV1::new_signed(
            ProjectSlug::new("sample-project").unwrap(),
            "Sample",
            ChainId::new("aa".repeat(32)).unwrap(),
            owner_pk,
            10,
            &operator,
        )
        .unwrap();
        (bootstrap, operator, owner, writer)
    }

    fn next_record(
        ledger: &AccessLedgerV1,
        manifest: &ProjectManifestV1,
        action: AccessActionV1,
        actor: &Identity,
    ) -> AccessRecordV1 {
        AccessRecordV1::new_signed(
            ledger.records().len() as u64,
            manifest,
            ledger.state().head,
            20 + ledger.records().len() as u64,
            action,
            actor,
        )
        .unwrap()
    }

    #[test]
    fn bootstrap_replay_and_owner_writer_grants_are_deterministic() {
        let (bootstrap, _, owner, writer) = fixture();
        let mut ledger =
            AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        assert_eq!(ledger.owner_count(), 1);
        assert_eq!(ledger.effective_title(), "Sample");
        let writer_pk = PublicKeyHex::new(writer.public_hex()).unwrap();
        let record = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Grant {
                public_key: writer_pk.clone(),
                role: ProjectRoleV1::Writer,
                label: Some("Automation".into()),
            },
            &owner,
        );
        ledger.try_extend(&[record]).unwrap();
        assert_eq!(ledger.role(&writer_pk), Some(ProjectRoleV1::Writer));
        assert!(ledger.can_write(&writer_pk));
        assert_eq!(ledger.state().members.len(), 2);
    }

    #[test]
    fn writer_cannot_manage_access_and_failure_is_atomic() {
        let (bootstrap, _, owner, writer) = fixture();
        let mut ledger =
            AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let writer_pk = PublicKeyHex::new(writer.public_hex()).unwrap();
        let grant = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Grant {
                public_key: writer_pk.clone(),
                role: ProjectRoleV1::Writer,
                label: None,
            },
            &owner,
        );
        ledger.try_extend(&[grant]).unwrap();
        let before = ledger.clone();
        let unauthorized = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Archive,
            &writer,
        );
        assert_eq!(
            ledger.try_extend(&[unauthorized]),
            Err(ProtocolError::AccessUnauthorized { at: 2 })
        );
        assert_eq!(ledger, before);
    }

    #[test]
    fn last_owner_cannot_be_revoked_or_demoted() {
        let (bootstrap, _, owner, _) = fixture();
        let mut ledger =
            AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let owner_pk = PublicKeyHex::new(owner.public_hex()).unwrap();

        let revoke = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Revoke {
                public_key: owner_pk.clone(),
            },
            &owner,
        );
        assert_eq!(
            ledger.try_extend(&[revoke]),
            Err(ProtocolError::AccessLastOwner { at: 1 })
        );

        let demote = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Grant {
                public_key: owner_pk,
                role: ProjectRoleV1::Writer,
                label: None,
            },
            &owner,
        );
        assert_eq!(
            ledger.try_extend(&[demote]),
            Err(ProtocolError::AccessLastOwner { at: 1 })
        );
    }

    #[test]
    fn access_log_is_bound_to_project_chain_and_previous_hash() {
        let (bootstrap, _, owner, _) = fixture();
        let ledger = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let mut record = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Rename {
                title: "Renamed".into(),
            },
            &owner,
        );
        record.chain_id = Some(ChainId::new("bb".repeat(32)).unwrap());
        assert_eq!(
            ledger.clone().try_extend(&[record]),
            Err(ProtocolError::AccessChainMismatch { at: 1 })
        );

        let mut record = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Archive,
            &owner,
        );
        record.prev_hash = HashHex::new("cc".repeat(32)).unwrap();
        assert_eq!(
            ledger.clone().try_extend(&[record]),
            Err(ProtocolError::AccessBadPrevHash { at: 1 })
        );
    }

    #[test]
    fn tampered_access_payload_and_signature_are_rejected() {
        let (bootstrap, _, owner, _) = fixture();
        let ledger = AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let record = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Rename {
                title: "Renamed".into(),
            },
            &owner,
        );

        let mut bad_hash = record.clone();
        bad_hash.action = AccessActionV1::Archive;
        assert_eq!(
            ledger.clone().try_extend(&[bad_hash]),
            Err(ProtocolError::AccessBadHash { at: 1 })
        );

        let mut bad_signature = record;
        bad_signature.sig = SignatureHex::new("00".repeat(64)).unwrap();
        assert_eq!(
            ledger.clone().try_extend(&[bad_signature]),
            Err(ProtocolError::AccessBadSignature { at: 1 })
        );
    }

    #[test]
    fn trusted_extension_does_not_replay_the_accepted_prefix() {
        let (bootstrap, _, owner, _) = fixture();
        let mut ledger =
            AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        // Deliberately violate the trusted-prefix invariant after replay. A
        // full replay detects this, while the online extension path only
        // checks the new record against the retained head and derived state.
        ledger.records[0].hash = HashHex::zero();
        assert!(AccessLedgerV1::replay(&bootstrap.manifest, ledger.records()).is_err());
        let rename = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Rename {
                title: "Incremental".into(),
            },
            &owner,
        );
        assert_eq!(
            ledger
                .verify_extension_crypto(std::slice::from_ref(&rename))
                .unwrap(),
            1
        );
        assert_eq!(ledger.try_extend(&[rename]).unwrap(), 1);
        assert_eq!(ledger.effective_title(), "Incremental");
    }

    #[test]
    fn replay_errors_expose_stable_code_and_record_context() {
        let (bootstrap, _, _, _) = fixture();
        let mut unsupported = bootstrap.access_log[0].clone();
        unsupported.schema_version = 99;
        let error = AccessLedgerV1::replay(&bootstrap.manifest, &[unsupported]).unwrap_err();
        assert_eq!(error.code(), "access_unsupported_version");
        assert_eq!(error.access_record_index(), Some(0));

        let mut invalid_action = bootstrap.access_log[0].clone();
        invalid_action.action = AccessActionV1::Rename { title: "\n".into() };
        let error = AccessLedgerV1::replay(&bootstrap.manifest, &[invalid_action]).unwrap_err();
        assert_eq!(error.code(), "access_invalid_action");
        assert_eq!(error.access_record_index(), Some(0));
    }

    #[test]
    fn migrated_legacy_manifest_binds_acl_by_genesis_hash() {
        let operator = Identity::from_secret_hex("operator", &"11".repeat(32)).unwrap();
        let owner = Identity::from_secret_hex("owner", &"22".repeat(32)).unwrap();
        let chain = mantis_chain::Chain::new();
        let manifest = ProjectManifestV1 {
            schema_version: crate::PROJECT_MANIFEST_VERSION,
            project_id: ProjectSlug::new("legacy-project").unwrap(),
            title: "Legacy Project".into(),
            chain_id: None,
            genesis_hash: HashHex::new(chain.blocks[0].hash.clone()).unwrap(),
            chain_format_version: mantis_chain::LEGACY_CHAIN_FORMAT_VERSION,
            created_at_ms: 1,
            created_by: PublicKeyHex::new(operator.public_hex()).unwrap(),
            initial_owner: PublicKeyHex::new(owner.public_hex()).unwrap(),
            archived: false,
        };
        manifest.validate_chain(&chain).unwrap();
        let first = AccessRecordV1::new_signed(
            0,
            &manifest,
            HashHex::zero(),
            1,
            AccessActionV1::Grant {
                public_key: manifest.initial_owner.clone(),
                role: ProjectRoleV1::Owner,
                label: None,
            },
            &operator,
        )
        .unwrap();
        let ledger = AccessLedgerV1::replay(&manifest, &[first]).unwrap();
        assert_eq!(ledger.owner_count(), 1);
        assert_eq!(ledger.records()[0].chain_id, None);
        assert_eq!(ledger.records()[0].genesis_hash, manifest.genesis_hash);
    }

    #[test]
    fn rename_archive_and_unarchive_are_replayed() {
        let (bootstrap, _, owner, _) = fixture();
        let mut ledger =
            AccessLedgerV1::replay(&bootstrap.manifest, &bootstrap.access_log).unwrap();
        let rename = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Rename {
                title: "Renamed".into(),
            },
            &owner,
        );
        ledger.try_extend(&[rename]).unwrap();
        let archive = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Archive,
            &owner,
        );
        ledger.try_extend(&[archive]).unwrap();
        assert_eq!(ledger.effective_title(), "Renamed");
        assert!(ledger.effective_archived());
        let unarchive = next_record(
            &ledger,
            &bootstrap.manifest,
            AccessActionV1::Unarchive,
            &owner,
        );
        ledger.try_extend(&[unarchive]).unwrap();
        assert!(!ledger.effective_archived());
    }
}
