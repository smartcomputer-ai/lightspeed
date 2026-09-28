//! Media admission: MIME allowlists, alias normalization, byte limits, and
//! the media/typing activity payloads the connector host serves.
//!
//! Attachments are provider-owned references until the conversation
//! workflow asks the connector to prepare one; the validation here runs at
//! admission so nothing unsupported or oversized ever reaches a workflow
//! history.

use api::{BotEventMedia, BotEventMediaKind, ChannelInboundMedia, ChannelMediaKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::delivery::ChannelRoute;

pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_PDF_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_TEXT_DOCUMENT_BYTES: u64 = 1024 * 1024;
pub const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
/// Attachments one provider message may carry.
pub const MAX_CHANNEL_MEDIA_PER_MESSAGE: usize = 8;

const IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];

const DOCUMENT_MIMES: &[&str] = &[
    "application/pdf",
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
];

const DOCUMENT_MIME_BY_EXTENSION: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("csv", "text/csv"),
    ("json", "application/json"),
];

const AUDIO_MIMES: &[&str] = &[
    "audio/mpeg",
    "audio/mp4",
    "audio/wav",
    "audio/webm",
    "audio/ogg",
    "audio/aac",
    "audio/amr",
    "audio/3gpp",
    "audio/3gpp2",
];

const AUDIO_MIME_BY_EXTENSION: &[(&str, &str)] = &[
    ("mp3", "audio/mpeg"),
    ("mpeg", "audio/mpeg"),
    ("mpga", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("mp4", "audio/mp4"),
    ("wav", "audio/wav"),
    ("wave", "audio/wav"),
    ("webm", "audio/webm"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("aac", "audio/aac"),
    ("amr", "audio/amr"),
    ("3gp", "audio/3gpp"),
    ("3gpp", "audio/3gpp"),
    ("3g2", "audio/3gpp2"),
    ("3gpp2", "audio/3gpp2"),
];

const AUDIO_MIME_ALIASES: &[(&str, &str)] = &[
    ("audio/mp3", "audio/mpeg"),
    ("audio/x-m4a", "audio/mp4"),
    ("audio/m4a", "audio/mp4"),
    ("audio/x-wav", "audio/wav"),
    ("audio/wave", "audio/wav"),
    ("audio/vnd.wave", "audio/wav"),
    ("audio/oga", "audio/ogg"),
    ("audio/opus", "audio/ogg"),
    ("audio/x-aac", "audio/aac"),
    ("audio/3gp", "audio/3gpp"),
    ("audio/3g2", "audio/3gpp2"),
];

fn lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| *value)
}

fn allowed(list: &'static [&'static str], mime: &str) -> Option<&'static str> {
    list.iter().copied().find(|candidate| *candidate == mime)
}

/// Lowercased media type without parameters (`image/jpeg; q=1` → `image/jpeg`).
fn clean_mime(value: Option<&str>) -> Option<String> {
    let lowered = value?.to_lowercase();
    let mime = lowered.split(';').next().unwrap_or_default().trim();
    (!mime.is_empty()).then(|| mime.to_owned())
}

/// Lowercased extension of a file name, when it has one that is not the
/// whole name.
fn extension_of(file_name: Option<&str>) -> Option<String> {
    let lowered = file_name?.to_lowercase();
    let extension = lowered.rsplit('.').next()?;
    (!extension.is_empty() && extension != lowered).then(|| extension.to_owned())
}

pub fn image_mime(reported_mime: Option<&str>) -> Option<&'static str> {
    let mime = clean_mime(reported_mime)?;
    allowed(IMAGE_MIMES, &mime)
}

/// The file extension wins over the reported type: providers report
/// `application/octet-stream` for most uploads.
pub fn document_mime(file_name: Option<&str>, reported_mime: Option<&str>) -> Option<&'static str> {
    if let Some(mime) =
        extension_of(file_name).and_then(|extension| lookup(DOCUMENT_MIME_BY_EXTENSION, &extension))
    {
        return Some(mime);
    }
    let mime = clean_mime(reported_mime)?;
    allowed(DOCUMENT_MIMES, &mime)
}

pub fn audio_mime(file_name: Option<&str>, reported_mime: Option<&str>) -> Option<&'static str> {
    if let Some(mime) =
        extension_of(file_name).and_then(|extension| lookup(AUDIO_MIME_BY_EXTENSION, &extension))
    {
        return Some(mime);
    }
    let reported = clean_mime(reported_mime)?;
    match lookup(AUDIO_MIME_ALIASES, &reported) {
        Some(normalized) => allowed(AUDIO_MIMES, normalized),
        None => allowed(AUDIO_MIMES, &reported),
    }
}

/// The admitted type for a media kind, or `None` when unsupported.
pub fn admitted_mime(
    kind: ChannelMediaKind,
    file_name: Option<&str>,
    reported_mime: &str,
) -> Option<&'static str> {
    match kind {
        ChannelMediaKind::Image => image_mime(Some(reported_mime)),
        ChannelMediaKind::Audio => audio_mime(file_name, Some(reported_mime)),
        ChannelMediaKind::Document => document_mime(file_name, Some(reported_mime)),
    }
}

pub fn media_byte_limit(kind: ChannelMediaKind, mime: &str) -> u64 {
    match kind {
        ChannelMediaKind::Image => MAX_IMAGE_BYTES,
        ChannelMediaKind::Audio => MAX_AUDIO_BYTES,
        ChannelMediaKind::Document => {
            if mime == "application/pdf" {
                MAX_PDF_BYTES
            } else {
                MAX_TEXT_DOCUMENT_BYTES
            }
        }
    }
}

pub fn media_kind_str(kind: ChannelMediaKind) -> &'static str {
    match kind {
        ChannelMediaKind::Image => "image",
        ChannelMediaKind::Audio => "audio",
        ChannelMediaKind::Document => "document",
    }
}

/// Validate one attachment reference and return it with its admitted MIME
/// type (aliases and extension-derived types normalized).
pub fn validate_inbound_media(media: &ChannelInboundMedia) -> Result<ChannelInboundMedia, String> {
    if media.file_id.is_empty() {
        return Err("media.fileId must be a non-empty string".to_owned());
    }
    if media.mime.is_empty() {
        return Err("media.mime must be a non-empty string".to_owned());
    }
    if media.name.as_deref().is_some_and(str::is_empty) {
        return Err("media.name must be a non-empty string".to_owned());
    }
    let kind = media.kind;
    let mime = admitted_mime(kind, media.name.as_deref(), &media.mime)
        .ok_or_else(|| format!("unsupported {} MIME", media_kind_str(kind)))?;
    let limit = media_byte_limit(kind, mime);
    if media.byte_size.is_some_and(|size| size > limit) {
        return Err(format!("media exceeds the {limit} byte limit"));
    }
    Ok(ChannelInboundMedia {
        mime: mime.to_owned(),
        ..media.clone()
    })
}

/// How an attachment is named in a rendering: `[image]`, `[document: notes.md]`.
pub fn media_label(kind: ChannelMediaKind, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("[{}: {name}]", media_kind_str(kind)),
        None => format!("[{}]", media_kind_str(kind)),
    }
}

pub fn bot_event_media_kind(kind: ChannelMediaKind) -> BotEventMediaKind {
    match kind {
        ChannelMediaKind::Image => BotEventMediaKind::Image,
        ChannelMediaKind::Audio => BotEventMediaKind::Audio,
        ChannelMediaKind::Document => BotEventMediaKind::Document,
    }
}

// ── Activity payloads ───────────────────────────────────────────────────────

/// `prepare_channel_media`: the connector downloads the provider file and
/// stores it in the universe's CAS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrepareChannelMediaInput {
    #[schemars(with = "String")]
    pub universe_id: Uuid,
    pub route: ChannelRoute,
    pub media: ChannelInboundMedia,
}

/// A prepared attachment: a CAS reference, never bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreparedMediaItem {
    pub blob_ref: String,
    pub kind: ChannelMediaKind,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl From<PreparedMediaItem> for BotEventMedia {
    fn from(item: PreparedMediaItem) -> Self {
        Self {
            blob_ref: item.blob_ref,
            kind: bot_event_media_kind(item.kind),
            text_ref: None,
            mime: item.mime,
            name: item.name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrepareChannelMediaResult {
    pub item: PreparedMediaItem,
}

/// `maintain_channel_typing`: keep the provider's typing indicator up for
/// the conversation until the activity is cancelled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintainChannelTypingInput {
    pub route: ChannelRoute,
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::ChannelAccountId;
    use api::ChannelProvider;

    fn media(kind: ChannelMediaKind, mime: &str, name: Option<&str>) -> ChannelInboundMedia {
        ChannelInboundMedia {
            file_id: "file-1".to_owned(),
            kind,
            mime: mime.to_owned(),
            name: name.map(str::to_owned),
            byte_size: None,
        }
    }

    #[test]
    fn normalizes_supported_document_and_audio_aliases() {
        assert_eq!(
            document_mime(Some("notes.MD"), Some("application/octet-stream")),
            Some("text/markdown")
        );
        assert_eq!(
            document_mime(None, Some("application/pdf; charset=binary")),
            Some("application/pdf")
        );
        assert_eq!(
            document_mime(Some("malware.exe"), Some("application/octet-stream")),
            None
        );
        assert_eq!(
            document_mime(Some("README"), Some("text/plain")),
            Some("text/plain")
        );
        assert_eq!(
            audio_mime(Some("voice.opus"), Some("application/octet-stream")),
            Some("audio/ogg")
        );
        assert_eq!(audio_mime(None, Some("audio/x-wav")), Some("audio/wav"));
        assert_eq!(
            audio_mime(None, Some("audio/x-m4a; codecs=aac")),
            Some("audio/mp4")
        );
        assert_eq!(audio_mime(None, Some("audio/flac")), None);
        assert_eq!(image_mime(Some("image/svg+xml")), None);
        assert_eq!(image_mime(Some("IMAGE/PNG")), Some("image/png"));
        assert_eq!(image_mime(None), None);
    }

    #[test]
    fn uses_media_specific_byte_limits() {
        assert_eq!(
            media_byte_limit(ChannelMediaKind::Image, "image/jpeg"),
            MAX_IMAGE_BYTES
        );
        assert_eq!(
            media_byte_limit(ChannelMediaKind::Audio, "audio/ogg"),
            MAX_AUDIO_BYTES
        );
        assert_eq!(
            media_byte_limit(ChannelMediaKind::Document, "text/plain"),
            MAX_TEXT_DOCUMENT_BYTES
        );
        assert_eq!(
            media_byte_limit(ChannelMediaKind::Document, "application/pdf"),
            MAX_PDF_BYTES
        );
        assert_eq!(MAX_TEXT_DOCUMENT_BYTES, 1024 * 1024);
    }

    #[test]
    fn validates_and_normalizes_inbound_media() {
        let normalized = validate_inbound_media(&media(
            ChannelMediaKind::Audio,
            "application/octet-stream",
            Some("voice.opus"),
        ))
        .unwrap();
        assert_eq!(normalized.mime, "audio/ogg");
        assert_eq!(normalized.name.as_deref(), Some("voice.opus"));

        let error = validate_inbound_media(&media(ChannelMediaKind::Image, "image/svg+xml", None))
            .unwrap_err();
        assert!(error.contains("unsupported image MIME"), "{error}");

        let oversized = ChannelInboundMedia {
            byte_size: Some(MAX_TEXT_DOCUMENT_BYTES + 1),
            ..media(ChannelMediaKind::Document, "text/plain", Some("big.txt"))
        };
        let error = validate_inbound_media(&oversized).unwrap_err();
        assert!(error.contains("byte limit"), "{error}");
        let pdf = ChannelInboundMedia {
            byte_size: Some(MAX_TEXT_DOCUMENT_BYTES + 1),
            ..media(
                ChannelMediaKind::Document,
                "application/pdf",
                Some("big.pdf"),
            )
        };
        assert!(validate_inbound_media(&pdf).is_ok());

        let no_file = ChannelInboundMedia {
            file_id: String::new(),
            ..media(ChannelMediaKind::Image, "image/jpeg", None)
        };
        assert!(
            validate_inbound_media(&no_file)
                .unwrap_err()
                .contains("fileId")
        );
        let empty_name = media(ChannelMediaKind::Image, "image/jpeg", Some(""));
        assert!(
            validate_inbound_media(&empty_name)
                .unwrap_err()
                .contains("media.name")
        );
    }

    #[test]
    fn labels_media_by_kind_and_name() {
        assert_eq!(media_label(ChannelMediaKind::Image, None), "[image]");
        assert_eq!(
            media_label(ChannelMediaKind::Document, Some("notes.md")),
            "[document: notes.md]"
        );
    }

    #[test]
    fn activity_payloads_use_camel_case_wire_names() {
        let input = PrepareChannelMediaInput {
            universe_id: Uuid::nil(),
            route: ChannelRoute {
                provider: ChannelProvider::new("telegram"),
                account_id: ChannelAccountId::new("primary"),
                chat_id: "123".to_owned(),
                thread_id: None,
            },
            media: media(ChannelMediaKind::Document, "text/plain", Some("note.txt")),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["universeId"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["route"]["accountId"], "primary");
        assert_eq!(json["media"]["fileId"], "file-1");
        assert!(json["route"].get("threadId").is_none());
        let back: PrepareChannelMediaInput = serde_json::from_value(json).unwrap();
        assert_eq!(back, input);

        let result = PrepareChannelMediaResult {
            item: PreparedMediaItem {
                blob_ref: "sha256:abc".to_owned(),
                kind: ChannelMediaKind::Audio,
                mime: "audio/ogg".to_owned(),
                name: None,
            },
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["item"]["blobRef"], "sha256:abc");
        assert_eq!(json["item"]["kind"], "audio");
        let event_media: BotEventMedia = result.item.into();
        assert_eq!(event_media.kind, BotEventMediaKind::Audio);
        assert_eq!(event_media.blob_ref, "sha256:abc");
    }
}
