use crate::{HashHex, ProtocolError, PublicKeyHex, SignatureHex};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) fn canonical_json<T: Serialize>(value: &T) -> Result<String, ProtocolError> {
    serde_json::to_string(value).map_err(|_| ProtocolError::CanonicalEncoding)
}

pub(crate) fn canonical_hash<T: Serialize>(value: &T) -> Result<HashHex, ProtocolError> {
    let json = canonical_json(value)?;
    let digest = Sha256::digest(json.as_bytes());
    HashHex::new(crate::types::hex_encode(&digest))
}

pub(crate) fn verify_signature(
    public_key: &PublicKeyHex,
    hash: &HashHex,
    signature: &SignatureHex,
) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key.bytes()) else {
        return false;
    };
    let signature = Signature::from_bytes(&signature.bytes());
    verifying_key
        .verify_strict(&hash.bytes(), &signature)
        .is_ok()
}
