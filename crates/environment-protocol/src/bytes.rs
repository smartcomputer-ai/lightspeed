//! Byte encodings.
//!
//! Payload bytes are padded standard base64 ([`ByteChunk`]). Fixed-size
//! cryptographic values are canonical lowercase hex, decoded byte by byte
//! so malformed input fails cleanly and never panics.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

/// JSON Schema pattern of padded standard base64.
pub const BASE64_PATTERN: &str = "^([A-Za-z0-9+/]{4})*([A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$";

/// Payload bytes, always named `data` on the wire, as padded standard
/// base64.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ByteChunk(
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            pattern(BASE64_PATTERN),
            extend("contentEncoding" = "base64")
        )
    )]
    Vec<u8>,
);

impl ByteChunk {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ByteChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ByteChunk({} bytes)", self.0.len())
    }
}

impl From<Vec<u8>> for ByteChunk {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<&[u8]> for ByteChunk {
    fn from(value: &[u8]) -> Self {
        Self(value.to_vec())
    }
}

impl Serialize for ByteChunk {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64_STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for ByteChunk {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        BASE64_STANDARD
            .decode(encoded)
            .map(Self)
            .map_err(D::Error::custom)
    }
}

/// A secret value carried on the wire, such as a `secretEnv` value, a
/// registration key or a stream token. It serializes as a plain string;
/// `Debug` is redacted so structured logging never prints it.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(with = "String"))]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString(<redacted>)")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum HexError {
    #[error("hex value has {actual} digits; expected {expected}")]
    Length { expected: usize, actual: usize },

    #[error("hex value has an odd number of digits")]
    OddLength,

    #[error("byte {index} is not a lowercase hex digit")]
    Digit { index: usize },
}

/// Lowercase hex of `bytes`.
pub fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    text
}

/// Decode canonical lowercase hex of any even length.
pub fn decode_hex(value: &str) -> Result<Vec<u8>, HexError> {
    let digits = value.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return Err(HexError::OddLength);
    }
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .map(|(pair, [high, low])| {
            let high = hex_digit(*high).ok_or(HexError::Digit { index: pair * 2 })?;
            let low = hex_digit(*low).ok_or(HexError::Digit {
                index: pair * 2 + 1,
            })?;
            Ok((high << 4) | low)
        })
        .collect()
}

/// Decode canonical lowercase hex of exactly `N` bytes.
pub fn decode_hex_array<const N: usize>(value: &str) -> Result<[u8; N], HexError> {
    if value.len() != N * 2 {
        return Err(HexError::Length {
            expected: N * 2,
            actual: value.len(),
        });
    }
    let bytes = decode_hex(value)?;
    let mut array = [0u8; N];
    array.copy_from_slice(&bytes);
    Ok(array)
}

fn hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

/// A fixed-size value that travels as canonical lowercase hex.
macro_rules! hex_value {
    ($(#[$doc:meta])* $name:ident, $bytes:literal, $pattern:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub struct $name(
            #[cfg_attr(
                feature = "schema",
                schemars(
                    with = "String",
                    pattern($pattern)
                )
            )]
            [u8; $bytes],
        );

        impl $name {
            pub const BYTES: usize = $bytes;

            pub fn from_bytes(bytes: [u8; $bytes]) -> Self {
                Self(bytes)
            }

            pub fn as_bytes(&self) -> &[u8; $bytes] {
                &self.0
            }

            pub fn parse(value: &str) -> Result<Self, $crate::bytes::HexError> {
                $crate::bytes::decode_hex_array::<$bytes>(value).map(Self)
            }

            pub fn to_hex(&self) -> String {
                $crate::bytes::encode_hex(&self.0)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "{}({})", stringify!($name), self.to_hex())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.to_hex())
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::bytes::HexError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_hex())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <String as serde::Deserialize>::deserialize(deserializer)?;
                Self::parse(&value).map_err(<D::Error as serde::de::Error>::custom)
            }
        }
    };
}

pub(crate) use hex_value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_chunks_are_padded_standard_base64() {
        let value = serde_json::to_value(ByteChunk::from(b"hello".as_slice())).unwrap();
        assert_eq!(value, serde_json::json!("aGVsbG8="));
        assert!(serde_json::from_value::<ByteChunk>(serde_json::json!("aGVsbG8")).is_err());
    }

    #[test]
    fn hex_decoding_is_canonical_and_never_panics() {
        assert_eq!(decode_hex("0001abff").unwrap(), [0, 1, 0xab, 0xff]);
        assert_eq!(encode_hex(&[0, 1, 0xab, 0xff]), "0001abff");
        for bad in ["abc", "zz", "AB", "+1", "a\u{e9}b", "aaaaaaaaaaaaaaa\u{e9}"] {
            assert!(decode_hex(bad).is_err(), "{bad:?}");
        }
        assert!(matches!(
            decode_hex_array::<2>("00"),
            Err(HexError::Length { .. })
        ));
    }
}
