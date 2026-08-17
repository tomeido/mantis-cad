use crate::ProtocolError;
use rand_core::RngCore;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn decode_lower_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    fn nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => unreachable!("alphabet checked above"),
        }
    }
    let mut output = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    Some(output)
}

macro_rules! canonical_hex_type {
    ($name:ident, $bytes:expr, $error:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
                let value = value.into();
                if decode_lower_hex::<$bytes>(&value).is_none() {
                    return Err(ProtocolError::$error);
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }

            #[allow(dead_code)]
            pub(crate) fn bytes(&self) -> [u8; $bytes] {
                decode_lower_hex::<$bytes>(&self.0).expect("validated canonical hex newtype")
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl FromStr for $name {
            type Err = ProtocolError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ProtocolError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

canonical_hex_type!(ChainId, 32, InvalidChainId);
canonical_hex_type!(HashHex, 32, InvalidHash);
canonical_hex_type!(PublicKeyHex, 32, InvalidPublicKey);
canonical_hex_type!(SignatureHex, 64, InvalidSignatureEncoding);

impl ChainId {
    /// Generate a cryptographically random 256-bit scoped chain id.
    pub fn generate() -> Result<Self, ProtocolError> {
        let mut bytes = [0_u8; 32];
        rand_core::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| ProtocolError::RandomUnavailable)?;
        Ok(Self(hex_encode(&bytes)))
    }
}

impl HashHex {
    pub fn zero() -> Self {
        Self("0".repeat(64))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectSlug(String);

impl ProjectSlug {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (3..=63).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase() || *byte == b'-');
        if !valid {
            return Err(ProtocolError::InvalidProjectSlug);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ProjectSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ProjectSlug {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ProjectSlug {
    type Err = ProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProjectSlug {
    type Error = ProtocolError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for ProjectSlug {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectSlug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hex_types_reject_uppercase_wrong_length_and_non_hex() {
        assert!(HashHex::new("ab".repeat(32)).is_ok());
        assert_eq!(
            HashHex::new("AB".repeat(32)),
            Err(ProtocolError::InvalidHash)
        );
        assert_eq!(
            PublicKeyHex::new("a".repeat(63)),
            Err(ProtocolError::InvalidPublicKey)
        );
        assert_eq!(
            SignatureHex::new("zz".repeat(64)),
            Err(ProtocolError::InvalidSignatureEncoding)
        );
        let json = format!("\"{}\"", "AB".repeat(32));
        assert!(serde_json::from_str::<ChainId>(&json).is_err());
    }

    #[test]
    fn project_slug_is_path_safe_and_canonical() {
        for valid in ["abc", "project-1", "a-very-long-project-2"] {
            assert_eq!(ProjectSlug::new(valid).unwrap().as_str(), valid);
        }
        for invalid in ["ab", "-abc", "abc-", "A-project", "a_b", "a/b", "한글"] {
            assert_eq!(
                ProjectSlug::new(invalid),
                Err(ProtocolError::InvalidProjectSlug)
            );
        }
    }
}
