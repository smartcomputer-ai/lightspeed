//! Content-addressed blob references.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::fmt;
use thiserror::Error;

const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;
const SHA256_REF_LEN: usize = SHA256_PREFIX.len() + SHA256_HEX_LEN;

/// The newtype-struct name [`BlobRef`] serializes under. JSON ignores it, so the
/// wire stays a bare string, and the typed walk in `storage` recognises a ref
/// by it without any ref-shaped string ever counting as one.
pub(crate) const BLOB_REF_NEWTYPE: &str = "lightspeed::BlobRef";

// A content-addressed blob reference: `sha256:<64 lowercase hex>`.
// Every value holds a canonical ref: construction and decode both validate, so a
// malformed ref can neither be written nor read. A decode failure is the point for
// this type. Leniency covers missing fields, unknown fields and shape change, not an
// invalid value of a validated id. Code taking refs from outside drops or refuses
// what fails to decode at that edge.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "contract",
    derive(schemars::JsonSchema),
    schemars(transparent)
)]
pub struct BlobRef(String);

impl Serialize for BlobRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(BLOB_REF_NEWTYPE, &self.0)
    }
}

impl<'de> Deserialize<'de> for BlobRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Durable content identity and encoding. The payload stays in CAS; consumers
/// project it outside the harness rather than assuming every output is text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub struct ContentRef {
    pub content_ref: BlobRef,
    pub media_type: Option<String>,
    pub provider_kind: Option<String>,
}

impl ContentRef {
    pub fn text(content_ref: BlobRef) -> Self {
        Self {
            content_ref,
            media_type: Some("text/plain".to_owned()),
            provider_kind: None,
        }
    }
}

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

    #[test]
    fn blob_ref_decode_validates_like_parse() {
        for value in [
            "not-a-ref",
            "sha256:1234",
            "sha256:\u{e9}\u{e9}\u{e9}",
            &format!("sha256:{}", "A".repeat(64)),
            &format!("SHA256:{}", "a".repeat(64)),
            &format!("sha256:{}\n", "a".repeat(64)),
        ] {
            assert!(BlobRef::parse(value).is_err());
            assert!(
                serde_json::from_value::<BlobRef>(serde_json::json!(value)).is_err(),
                "{value:?} decoded"
            );
        }
        assert!(serde_json::from_value::<BlobRef>(serde_json::json!(1)).is_err());
    }

    #[test]
    fn blob_ref_stays_a_bare_string_on_the_wire_and_as_a_map_key() {
        let blob_ref = BlobRef::from_bytes(b"payload");
        assert_eq!(
            serde_json::to_value(&blob_ref).unwrap(),
            serde_json::Value::String(blob_ref.as_str().to_owned())
        );
        let map = std::collections::BTreeMap::from([(blob_ref.clone(), 1)]);
        let encoded = serde_json::to_value(&map).unwrap();
        assert_eq!(encoded[blob_ref.as_str()], 1);
        assert_eq!(
            serde_json::from_value::<std::collections::BTreeMap<BlobRef, u32>>(encoded).unwrap(),
            map
        );
    }
}
