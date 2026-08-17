//! Shared, versioned contracts for MantisCAD projects, access control, sync,
//! and portable workspaces.
//!
//! This crate contains no networking or persistence. Server, GUI, CLI, and AI
//! agents all use the same serde shapes and validation rules. Cryptographic
//! records sign the raw 32-byte SHA-256 digest of a domain-separated canonical
//! JSON payload, matching the block-signature convention in `mantis-chain`.

mod access;
mod crypto;
mod error;
mod project;
mod types;
mod wire;

pub use access::{
    AccessActionV1, AccessLedgerV1, AccessMemberV1, AccessRecordV1, AccessStateV1, ProjectRoleV1,
};
pub use error::ProtocolError;
pub use project::{ProjectBootstrapV1, ProjectCreateV1, ProjectManifestV1};
pub use types::{ChainId, HashHex, ProjectSlug, PublicKeyHex, SignatureHex};
pub use wire::{
    ApiInfoV2, BlocksPageV2, ChainStateV1, ErrorDetailV1, ErrorResponseV1, PortableWorkspaceV1,
    ProjectInfoV2, ProjectSummaryV1, PushRequestV2, PushResponseV2, RemoteAccessV1,
    RemoteProjectV1,
};

pub const PROJECT_MANIFEST_VERSION: u32 = 1;
pub const PROJECT_CREATE_VERSION: u32 = 1;
pub const ACCESS_RECORD_VERSION: u32 = 1;
pub const PORTABLE_WORKSPACE_VERSION: u32 = 1;
pub const API_VERSION: u32 = 2;
