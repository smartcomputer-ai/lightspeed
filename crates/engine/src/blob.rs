//! Content-addressed blob references.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use thiserror::Error;

const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;
const SHA256_REF_LEN: usize = SHA256_PREFIX.len() + SHA256_HEX_LEN;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobRef(String);

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BlobRefError {
    #[error("blob ref must use sha256:<64 lowercase hex> format: {value}")]
    InvalidFormat { value: String },
}

impl BlobRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, BlobRefError> {
        let value = value.into();
        if is_canonical_sha256_ref(&value) {
            Ok(Self(value))
        } else {
            Err(BlobRefError::InvalidFormat { value })
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(format!("{SHA256_PREFIX}{}", hex::encode(digest)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn new_unchecked_for_tests(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl Default for BlobRef {
    fn default() -> Self {
        Self::from_bytes(&[])
    }
}

impl fmt::Display for BlobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn is_canonical_sha256_ref(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(SHA256_PREFIX) else {
        return false;
    };
    hex.len() == SHA256_HEX_LEN
        && value.len() == SHA256_REF_LEN
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_ref_round_trips_as_json_string() {
        let blob_ref = BlobRef::from_bytes(b"payload");

        let encoded = serde_json::to_string(&blob_ref).expect("serialize blob ref");
        assert_eq!(
            encoded,
            "\"sha256:239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5\""
        );
        let decoded: BlobRef = serde_json::from_str(&encoded).expect("decode blob ref");
        assert_eq!(decoded, blob_ref);
    }

    #[test]
    fn blob_ref_parse_rejects_non_canonical_values() {
        assert!(BlobRef::parse("blob://payload").is_err());
        assert!(BlobRef::parse("sha256:ABCDEF").is_err());
        assert!(BlobRef::parse("sha256:1234").is_err());
    }
}
