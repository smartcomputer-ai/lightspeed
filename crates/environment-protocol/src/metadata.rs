//! Bounds on descriptive values: display names, correlation metadata and
//! short human-readable texts.
//!
//! These values are descriptive: they never select, authenticate or gate.
//! The same bounds apply wherever Lightspeed accepts them (daemon
//! registration, provider observations and templates, environment and
//! session metadata, profile documents), so a session and the environment it
//! ran in accept the same keys. On the wire they are validated on decode by
//! [`DisplayName`], [`Metadata`] and [`DescriptiveText`].

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::limits::MAX_DESCRIPTIVE_TEXT_BYTES;

pub const MAX_DISPLAY_NAME_BYTES: u32 = 128;
pub const MAX_METADATA_ENTRIES: u32 = 32;
pub const MAX_METADATA_KEY_BYTES: u32 = 64;
pub const MAX_METADATA_VALUE_BYTES: u32 = 256;
/// Keys under this prefix are written by Lightspeed only.
pub const RESERVED_METADATA_PREFIX: &str = "lightspeed.";

/// JSON Schema pattern of [`DescriptiveText`]: no control characters except
/// line feed and tab.
pub const DESCRIPTIVE_TEXT_PATTERN: &str = "^[^\\u0000-\\u0008\\u000B-\\u001F\\u007F-\\u009F]+$";

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MetadataError {
    #[error("display name must be 1..={MAX_DISPLAY_NAME_BYTES} bytes without control characters")]
    DisplayName,

    #[error("metadata has more than {MAX_METADATA_ENTRIES} entries")]
    TooManyEntries,

    #[error("metadata key {key:?} must be 1..={MAX_METADATA_KEY_BYTES} bytes")]
    KeyBytes { key: String },

    #[error("metadata value for {key:?} must be 1..={MAX_METADATA_VALUE_BYTES} bytes")]
    ValueBytes { key: String },

    #[error("metadata entry {key:?} contains a control character")]
    ControlCharacter { key: String },

    #[error("metadata key {key:?} uses the reserved {RESERVED_METADATA_PREFIX} prefix")]
    ReservedPrefix { key: String },

    #[error(
        "text must be 1..={MAX_DESCRIPTIVE_TEXT_BYTES} bytes without control characters other than line feed and tab"
    )]
    DescriptiveText,
}

pub fn validate_display_name(name: &str) -> Result<(), MetadataError> {
    if name.is_empty()
        || name.len() > MAX_DISPLAY_NAME_BYTES as usize
        || name.chars().any(char::is_control)
    {
        return Err(MetadataError::DisplayName);
    }
    Ok(())
}

/// A metadata map a caller supplies: at most [`MAX_METADATA_ENTRIES`]
/// bounded entries and no key under [`RESERVED_METADATA_PREFIX`].
pub fn validate_caller_metadata(metadata: &BTreeMap<String, String>) -> Result<(), MetadataError> {
    if metadata.len() > MAX_METADATA_ENTRIES as usize {
        return Err(MetadataError::TooManyEntries);
    }
    for (key, value) in metadata {
        validate_metadata_entry(key, value)?;
        if key.starts_with(RESERVED_METADATA_PREFIX) {
            return Err(MetadataError::ReservedPrefix { key: key.clone() });
        }
    }
    Ok(())
}

/// A stored metadata map: caller keys obey [`validate_caller_metadata`]'s
/// count, and Lightspeed's own reserved keys are bounded per entry but not
/// counted.
pub fn validate_stored_metadata(metadata: &BTreeMap<String, String>) -> Result<(), MetadataError> {
    let caller_entries = metadata
        .keys()
        .filter(|key| !key.starts_with(RESERVED_METADATA_PREFIX))
        .count();
    if caller_entries > MAX_METADATA_ENTRIES as usize {
        return Err(MetadataError::TooManyEntries);
    }
    for (key, value) in metadata {
        validate_metadata_entry(key, value)?;
    }
    Ok(())
}

/// Key and value bytes and control characters of one entry.
pub fn validate_metadata_entry(key: &str, value: &str) -> Result<(), MetadataError> {
    if key.is_empty() || key.len() > MAX_METADATA_KEY_BYTES as usize {
        return Err(MetadataError::KeyBytes {
            key: key.to_owned(),
        });
    }
    if value.is_empty() || value.len() > MAX_METADATA_VALUE_BYTES as usize {
        return Err(MetadataError::ValueBytes {
            key: key.to_owned(),
        });
    }
    if key.chars().any(char::is_control) || value.chars().any(char::is_control) {
        return Err(MetadataError::ControlCharacter {
            key: key.to_owned(),
        });
    }
    Ok(())
}

/// Check [`DescriptiveText`]'s rule.
pub fn validate_descriptive_text(text: &str) -> Result<(), MetadataError> {
    if text.is_empty()
        || text.len() > MAX_DESCRIPTIVE_TEXT_BYTES as usize
        || text.chars().any(is_disallowed_text_control)
    {
        return Err(MetadataError::DescriptiveText);
    }
    Ok(())
}

fn is_disallowed_text_control(character: char) -> bool {
    character.is_control() && character != '\n' && character != '\t'
}

/// A display name: 1 to 128 bytes without control characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DisplayName(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 128), pattern(crate::ids::NO_CONTROL_PATTERN))
    )]
    String,
);

impl DisplayName {
    pub fn new(name: impl Into<String>) -> Result<Self, MetadataError> {
        let name = name.into();
        validate_display_name(&name)?;
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// A short human-readable text: 1 to 1024 bytes without control characters
/// other than line feed and tab. Never parsed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DescriptiveText(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 1024), pattern(DESCRIPTIVE_TEXT_PATTERN))
    )]
    String,
);

impl DescriptiveText {
    pub fn new(text: impl Into<String>) -> Result<Self, MetadataError> {
        let text = text.into();
        validate_descriptive_text(&text)?;
        Ok(Self(text))
    }

    /// Build a valid text from any string, for senders: disallowed control
    /// characters become spaces and the text is cut to the bound on a
    /// character boundary. `None` when nothing is left.
    pub fn lossy(text: &str) -> Option<Self> {
        let mut cleaned: String = text
            .chars()
            .map(|character| {
                if is_disallowed_text_control(character) {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let max = MAX_DESCRIPTIVE_TEXT_BYTES as usize;
        if cleaned.len() > max {
            let mut end = max;
            while !cleaned.is_char_boundary(end) {
                end -= 1;
            }
            cleaned.truncate(end);
        }
        (!cleaned.is_empty()).then_some(Self(cleaned))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

macro_rules! text_impls {
    ($name:ident, $error:ty) => {
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = $error;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

text_impls!(DisplayName, MetadataError);
text_impls!(DescriptiveText, MetadataError);

/// Caller-supplied metadata on the wire: at most [`MAX_METADATA_ENTRIES`]
/// bounded entries and no key under [`RESERVED_METADATA_PREFIX`]
/// ([`validate_caller_metadata`]), checked on decode. Serialized as a JSON
/// object of strings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata(BTreeMap<String, String>);

impl Metadata {
    pub fn new(entries: BTreeMap<String, String>) -> Result<Self, MetadataError> {
        validate_caller_metadata(&entries)?;
        Ok(Self(entries))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    pub fn into_map(self) -> BTreeMap<String, String> {
        self.0
    }
}

impl TryFrom<BTreeMap<String, String>> for Metadata {
    type Error = MetadataError;

    fn try_from(entries: BTreeMap<String, String>) -> Result<Self, Self::Error> {
        Self::new(entries)
    }
}

impl Serialize for Metadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Metadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = BTreeMap::<String, String>::deserialize(deserializer)?;
        Self::new(entries).map_err(D::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Metadata {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Metadata".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Descriptive key-value metadata: at most 32 entries, keys 1 to 64 bytes outside the reserved `lightspeed.` prefix, values 1 to 256 bytes, no control characters.",
            "type": "object",
            "maxProperties": MAX_METADATA_ENTRIES,
            "propertyNames": {
                "minLength": 1,
                "maxLength": MAX_METADATA_KEY_BYTES,
                "pattern": crate::ids::NO_CONTROL_PATTERN,
                "not": {"pattern": "^lightspeed\\."},
            },
            "additionalProperties": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_METADATA_VALUE_BYTES,
                "pattern": crate::ids::NO_CONTROL_PATTERN,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_metadata_is_bounded_and_keeps_out_of_the_reserved_prefix() {
        let mut metadata = BTreeMap::new();
        metadata.insert("harbor.trialId".to_owned(), "trial-1".to_owned());
        assert_eq!(validate_caller_metadata(&metadata), Ok(()));
        metadata.insert("lightspeed.x".to_owned(), "y".to_owned());
        assert_eq!(
            validate_caller_metadata(&metadata),
            Err(MetadataError::ReservedPrefix {
                key: "lightspeed.x".to_owned()
            })
        );
        assert_eq!(validate_stored_metadata(&metadata), Ok(()));
        metadata.clear();
        metadata.insert(
            "k".to_owned(),
            "a".repeat(MAX_METADATA_VALUE_BYTES as usize + 1),
        );
        assert!(matches!(
            validate_caller_metadata(&metadata),
            Err(MetadataError::ValueBytes { .. })
        ));
        assert_eq!(
            validate_display_name("bad\nname"),
            Err(MetadataError::DisplayName)
        );
        assert_eq!(validate_display_name(""), Err(MetadataError::DisplayName));
    }

    #[test]
    fn stored_metadata_does_not_count_reserved_keys() {
        let mut metadata: BTreeMap<String, String> = (0..MAX_METADATA_ENTRIES as usize)
            .map(|index| (format!("k{index}"), "v".to_owned()))
            .collect();
        metadata.insert("lightspeed.envd.version".to_owned(), "0.5.0".to_owned());
        assert_eq!(validate_stored_metadata(&metadata), Ok(()));
        assert!(validate_caller_metadata(&metadata).is_err());
    }

    #[test]
    fn wire_values_validate_on_decode() {
        assert!(serde_json::from_str::<DisplayName>("\"runner 7\"").is_ok());
        assert!(serde_json::from_str::<DisplayName>("\"\"").is_err());
        assert!(serde_json::from_str::<Metadata>(r#"{"lightspeed.x": "y"}"#).is_err());
        assert!(serde_json::from_str::<Metadata>(r#"{"k": "v"}"#).is_ok());
        assert!(serde_json::from_str::<DescriptiveText>("\"line one\\nline two\"").is_ok());
        assert!(serde_json::from_str::<DescriptiveText>("\"bell\\u0007\"").is_err());
        let long = "x".repeat(MAX_DESCRIPTIVE_TEXT_BYTES as usize + 1);
        assert!(DescriptiveText::new(long.clone()).is_err());
        let cut = DescriptiveText::lossy(&format!("a\u{7}{long}")).unwrap();
        assert_eq!(cut.as_str().len(), MAX_DESCRIPTIVE_TEXT_BYTES as usize);
        assert!(cut.as_str().starts_with("a "));
        assert!(DescriptiveText::lossy("").is_none());
    }
}
