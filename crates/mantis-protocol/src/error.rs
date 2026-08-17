use std::fmt;

/// Validation errors with stable codes for HTTP and non-interactive clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidProjectSlug,
    InvalidChainId,
    InvalidHash,
    InvalidPublicKey,
    InvalidSignatureEncoding,
    InvalidRemoteBaseUrl,
    InvalidTitle,
    InvalidLabel,
    RandomUnavailable,
    CanonicalEncoding,
    UnsupportedVersion { kind: &'static str, found: u32 },
    InvalidChain { code: String },
    ManifestMismatch { field: &'static str },
    UntrustedOperator,
    BadCreateHash,
    BadCreateSignature,
    BootstrapBadCheckpoint,
    BootstrapUnauthorizedAuthor { block: u64 },
    AccessBadIndex { at: u64 },
    AccessUnsupportedVersion { at: u64, found: u32 },
    AccessInvalidAction { at: u64 },
    AccessBadPrevHash { at: u64 },
    AccessBadHash { at: u64 },
    AccessBadSignature { at: u64 },
    AccessProjectMismatch { at: u64 },
    AccessChainMismatch { at: u64 },
    AccessBootstrapMismatch { at: u64 },
    AccessUnauthorized { at: u64 },
    AccessUnknownMember { at: u64 },
    AccessNoop { at: u64 },
    AccessLastOwner { at: u64 },
}

impl ProtocolError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidProjectSlug => "invalid_project_slug",
            Self::InvalidChainId => "invalid_chain_id",
            Self::InvalidHash => "invalid_hash",
            Self::InvalidPublicKey => "invalid_public_key",
            Self::InvalidSignatureEncoding => "invalid_signature_encoding",
            Self::InvalidRemoteBaseUrl => "invalid_remote_base_url",
            Self::InvalidTitle => "invalid_title",
            Self::InvalidLabel => "invalid_label",
            Self::RandomUnavailable => "random_unavailable",
            Self::CanonicalEncoding => "canonical_encoding_failed",
            Self::UnsupportedVersion { .. } => "unsupported_version",
            Self::InvalidChain { .. } => "invalid_chain",
            Self::ManifestMismatch { .. } => "manifest_mismatch",
            Self::UntrustedOperator => "untrusted_operator",
            Self::BadCreateHash => "bad_create_hash",
            Self::BadCreateSignature => "bad_create_signature",
            Self::BootstrapBadCheckpoint => "bootstrap_bad_checkpoint",
            Self::BootstrapUnauthorizedAuthor { .. } => "bootstrap_unauthorized_author",
            Self::AccessBadIndex { .. } => "access_bad_index",
            Self::AccessUnsupportedVersion { .. } => "access_unsupported_version",
            Self::AccessInvalidAction { .. } => "access_invalid_action",
            Self::AccessBadPrevHash { .. } => "access_bad_prev_hash",
            Self::AccessBadHash { .. } => "access_bad_hash",
            Self::AccessBadSignature { .. } => "access_bad_signature",
            Self::AccessProjectMismatch { .. } => "access_project_mismatch",
            Self::AccessChainMismatch { .. } => "access_chain_mismatch",
            Self::AccessBootstrapMismatch { .. } => "access_bootstrap_mismatch",
            Self::AccessUnauthorized { .. } => "access_unauthorized",
            Self::AccessUnknownMember { .. } => "access_unknown_member",
            Self::AccessNoop { .. } => "access_noop",
            Self::AccessLastOwner { .. } => "access_last_owner",
        }
    }

    pub const fn access_record_index(&self) -> Option<u64> {
        match self {
            Self::AccessBadIndex { at }
            | Self::AccessUnsupportedVersion { at, .. }
            | Self::AccessInvalidAction { at }
            | Self::AccessBadPrevHash { at }
            | Self::AccessBadHash { at }
            | Self::AccessBadSignature { at }
            | Self::AccessProjectMismatch { at }
            | Self::AccessChainMismatch { at }
            | Self::AccessBootstrapMismatch { at }
            | Self::AccessUnauthorized { at }
            | Self::AccessUnknownMember { at }
            | Self::AccessNoop { at }
            | Self::AccessLastOwner { at } => Some(*at),
            _ => None,
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProjectSlug => write!(
                f,
                "project id must be 3-63 lowercase letters, digits, or internal hyphens"
            ),
            Self::InvalidChainId => write!(f, "chain id must be 64 lowercase hex characters"),
            Self::InvalidHash => write!(f, "hash must be 64 lowercase hex characters"),
            Self::InvalidPublicKey => {
                write!(f, "public key must be 64 lowercase hex characters")
            }
            Self::InvalidSignatureEncoding => {
                write!(f, "signature must be 128 lowercase hex characters")
            }
            Self::InvalidRemoteBaseUrl => write!(
                f,
                "remote base URL must be empty or a canonical http(s) origin without credentials, path, query, fragment, whitespace, or trailing slash"
            ),
            Self::InvalidTitle => write!(f, "title must be 1-120 non-control characters"),
            Self::InvalidLabel => write!(f, "label must be at most 80 non-control characters"),
            Self::RandomUnavailable => write!(f, "secure randomness is unavailable"),
            Self::CanonicalEncoding => write!(f, "canonical JSON encoding failed"),
            Self::UnsupportedVersion { kind, found } => {
                write!(f, "unsupported {kind} version {found}")
            }
            Self::InvalidChain { code } => write!(f, "invalid project chain: {code}"),
            Self::ManifestMismatch { field } => {
                write!(f, "project manifest does not match chain field {field}")
            }
            Self::UntrustedOperator => write!(f, "project creator is not a trusted operator"),
            Self::BadCreateHash => write!(f, "project-create hash does not match its payload"),
            Self::BadCreateSignature => write!(f, "project-create signature is invalid"),
            Self::BootstrapBadCheckpoint => {
                write!(
                    f,
                    "project-create initial checkpoint does not match the chain"
                )
            }
            Self::BootstrapUnauthorizedAuthor { block } => write!(
                f,
                "bootstrap block {block} was authored by a key never granted project access"
            ),
            Self::AccessBadIndex { at } => write!(f, "access record {at} has a bad index"),
            Self::AccessUnsupportedVersion { at, found } => {
                write!(f, "access record {at} uses unsupported version {found}")
            }
            Self::AccessInvalidAction { at } => {
                write!(f, "access record {at} contains invalid action data")
            }
            Self::AccessBadPrevHash { at } => {
                write!(f, "access record {at} does not link to its predecessor")
            }
            Self::AccessBadHash { at } => {
                write!(f, "access record {at} hash does not match its payload")
            }
            Self::AccessBadSignature { at } => {
                write!(f, "access record {at} signature is invalid")
            }
            Self::AccessProjectMismatch { at } => {
                write!(f, "access record {at} belongs to another project")
            }
            Self::AccessChainMismatch { at } => {
                write!(f, "access record {at} belongs to another chain")
            }
            Self::AccessBootstrapMismatch { at } => {
                write!(f, "access record {at} is not the required bootstrap grant")
            }
            Self::AccessUnauthorized { at } => {
                write!(f, "access record {at} was not signed by a current owner")
            }
            Self::AccessUnknownMember { at } => {
                write!(f, "access record {at} revokes an unknown member")
            }
            Self::AccessNoop { at } => write!(f, "access record {at} makes no state change"),
            Self::AccessLastOwner { at } => {
                write!(f, "access record {at} would remove the final owner")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}
