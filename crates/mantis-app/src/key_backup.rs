//! Password-encrypted, portable signing identity backups.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use mantis_chain::Identity;
use serde::{Deserialize, Serialize};

const FORMAT_VERSION: u32 = 1;
const KDF_MEMORY_KIB: u32 = 19_456;
const KDF_ITERATIONS: u32 = 2;
const KDF_LANES: u32 = 1;
const AAD_PREFIX: &str = "MantisCAD identity backup v1";

#[derive(Serialize, Deserialize)]
struct PlainIdentityV1 {
    name: String,
    secret_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedIdentityV1 {
    format_version: u32,
    kdf: String,
    cipher: String,
    public_key: String,
    salt_b64: String,
    nonce_b64: String,
    ciphertext_b64: String,
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    if password.chars().count() < 8 {
        return Err("backup password must be at least 8 characters".into());
    }
    let params = Params::new(KDF_MEMORY_KIB, KDF_ITERATIONS, KDF_LANES, Some(32))
        .map_err(|e| format!("invalid Argon2 parameters: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("key derivation failed: {e}"))?;
    Ok(key)
}

fn associated_data(public_key: &str) -> String {
    format!("{AAD_PREFIX}|{public_key}")
}

pub fn export(identity: &Identity, password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 24];
    getrandom::getrandom(&mut salt).map_err(|e| format!("random salt failed: {e}"))?;
    getrandom::getrandom(&mut nonce).map_err(|e| format!("random nonce failed: {e}"))?;
    let public_key = identity.public_hex();
    let key = derive_key(password, &salt)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let plaintext = serde_json::to_vec(&PlainIdentityV1 {
        name: identity.name.clone(),
        secret_hex: identity.secret_hex(),
    })
    .map_err(|e| format!("cannot serialize identity: {e}"))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: associated_data(&public_key).as_bytes(),
            },
        )
        .map_err(|_| "identity encryption failed".to_string())?;
    serde_json::to_string_pretty(&EncryptedIdentityV1 {
        format_version: FORMAT_VERSION,
        kdf: "argon2id-v19-m19456-t2-p1".into(),
        cipher: "xchacha20poly1305".into(),
        public_key,
        salt_b64: BASE64.encode(salt),
        nonce_b64: BASE64.encode(nonce),
        ciphertext_b64: BASE64.encode(ciphertext),
    })
    .map_err(|e| format!("cannot serialize backup: {e}"))
}

pub fn import(json: &str, password: &str) -> Result<Identity, String> {
    let envelope: EncryptedIdentityV1 =
        serde_json::from_str(json).map_err(|e| format!("invalid .mantis-key JSON: {e}"))?;
    if envelope.format_version != FORMAT_VERSION {
        return Err(format!(
            "unsupported key backup format {}",
            envelope.format_version
        ));
    }
    if envelope.kdf != "argon2id-v19-m19456-t2-p1" || envelope.cipher != "xchacha20poly1305" {
        return Err("unsupported key backup encryption parameters".into());
    }
    let salt = BASE64
        .decode(&envelope.salt_b64)
        .map_err(|_| "backup salt is not valid base64".to_string())?;
    if salt.len() != 16 {
        return Err("backup salt has the wrong length".into());
    }
    let nonce = BASE64
        .decode(&envelope.nonce_b64)
        .map_err(|_| "backup nonce is not valid base64".to_string())?;
    let nonce: [u8; 24] = nonce
        .try_into()
        .map_err(|_| "backup nonce has the wrong length".to_string())?;
    let ciphertext = BASE64
        .decode(&envelope.ciphertext_b64)
        .map_err(|_| "backup ciphertext is not valid base64".to_string())?;
    let key = derive_key(password, &salt)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: associated_data(&envelope.public_key).as_bytes(),
            },
        )
        .map_err(|_| "wrong password or tampered .mantis-key file".to_string())?;
    let plain: PlainIdentityV1 = serde_json::from_slice(&plaintext)
        .map_err(|_| "decrypted key backup is invalid".to_string())?;
    let identity = Identity::from_secret_hex(&plain.name, &plain.secret_hex)
        .map_err(|e| format!("decrypted signing key is invalid: {e}"))?;
    if identity.public_hex() != envelope.public_key {
        return Err("key backup public-key check failed".into());
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_identity_round_trip() {
        let identity = Identity::generate("alice");
        let public = identity.public_hex();
        let backup = export(&identity, "correct horse battery staple").unwrap();
        assert!(!backup.contains(&identity.secret_hex()));
        let restored = import(&backup, "correct horse battery staple").unwrap();
        assert_eq!(restored.name, "alice");
        assert_eq!(restored.public_hex(), public);
    }

    #[test]
    fn wrong_password_and_tampering_are_rejected() {
        let identity = Identity::generate("alice");
        let backup = export(&identity, "correct horse battery staple").unwrap();
        assert!(import(&backup, "a different valid password").is_err());
        let tampered = backup.replace(&identity.public_hex(), &"00".repeat(32));
        assert!(import(&tampered, "correct horse battery staple").is_err());
    }

    #[test]
    fn short_password_is_rejected() {
        let identity = Identity::generate("alice");
        assert!(export(&identity, "short").is_err());
    }
}
