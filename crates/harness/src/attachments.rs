//! Portable immutable assets associated with tool results.
//! The harness records descriptors; adapters own admission and presentation.

use crate::{BlobRef, ContextEntryInput, media::MediaDescriptor};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Attachment {
    Media(MediaDescriptor),
    File(FileAttachment),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub struct FileAttachment {
    pub handle: String,
    pub content_ref: BlobRef,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AttachmentSource>,
}

/// Descriptive origin only; never an authority for resolving attachment bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub struct AttachmentSource {
    pub kind: String,
    pub id: String,
    pub path: String,
}

impl FileAttachment {
    pub fn new(content_ref: BlobRef, name: String, media_type: Option<String>) -> Self {
        Self {
            handle: format!("file:{}", &content_ref.as_str()[7..31]),
            content_ref,
            name,
            media_type,
            source: None,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.handle == format!("file:{}", &self.content_ref.as_str()[7..31])
            && !self.name.is_empty()
    }
}

impl Attachment {
    pub fn handle(&self) -> &str {
        match self {
            Self::Media(media) => &media.handle,
            Self::File(file) => &file.handle,
        }
    }

    pub fn content_ref(&self) -> &BlobRef {
        match self {
            Self::Media(media) => &media.content_ref,
            Self::File(file) => &file.content_ref,
        }
    }

    /// File links are metadata; only admitted media adds native model input.
    pub fn context_entry(&self) -> Option<ContextEntryInput> {
        match self {
            Self::Media(media) => Some(media.context_entry()),
            Self::File(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_content_ref_fails_decode_instead_of_panicking_validation() {
        for content_ref in [
            "short",
            "sha256:\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
        ] {
            let attachment = serde_json::json!({
                "kind": "file",
                "data": { "handle": "file:x", "content_ref": content_ref, "name": "n" },
            });
            assert!(serde_json::from_value::<Attachment>(attachment).is_err());
        }
    }

    #[test]
    fn a_decoded_file_attachment_validates_its_handle() {
        let file = FileAttachment::new(BlobRef::from_bytes(b"file"), "n".into(), None);
        let decoded: FileAttachment =
            serde_json::from_value(serde_json::to_value(&file).unwrap()).unwrap();
        assert!(decoded.is_valid());
    }
}
