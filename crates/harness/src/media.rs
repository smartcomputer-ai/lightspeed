//! Model-facing media: handles, admission, labels, and companion entries.
//!
//! Media the model has seen is named by a *handle* derived from its
//! content-addressed reference, `media:` plus the first twelve hex characters
//! of the SHA-256. The handle is a pure function of the bytes, so the same
//! image carries the same name in every session and no registry or counter is
//! needed. Every media context entry announces its handle in the visible text
//! that introduces it, and the model references media by writing the handle as
//! a URL (`![caption](media:3f9a2c1d4e7b)`), which consumers resolve against
//! the media entries of the session they render.
//!
//! Media produced by tools (MCP results, file reads, sub-agent hand-offs) is
//! admitted by the rules here and appended to the tool result as ordinary
//! user-role message entries, the same shape run input produces, so provider
//! lowering, compaction, and projection treat it uniformly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    BlobRef, ContentRef,
    core::{ContextEntryInput, ContextEntryKind, ContextMessageRole},
};

pub const MEDIA_HANDLE_PREFIX: &str = "media:";
pub const MEDIA_HANDLE_HEX_LEN: usize = 12;

/// Largest single asset a tool may hand the model; mirrors run-input admission.
pub const MAX_TOOL_MEDIA_BYTES: u64 = 10 * 1024 * 1024;
/// Most media entries one tool result may append; mirrors run-input admission.
pub const MAX_TOOL_MEDIA_ITEMS: usize = 8;

/// Image media types every supported provider accepts natively.
pub const IMAGE_MEDIA_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
pub const PDF_MEDIA_TYPE: &str = "application/pdf";

/// The model-facing name of a blob: `media:` + the first twelve hex characters
/// of its SHA-256.
pub fn media_handle(blob_ref: &BlobRef) -> String {
    let hex = blob_ref.as_str().rsplit(':').next().unwrap_or_default();
    format!(
        "{MEDIA_HANDLE_PREFIX}{}",
        &hex[..hex.len().min(MEDIA_HANDLE_HEX_LEN)]
    )
}

/// True when `handle` names `blob_ref`. Accepts the handle with or without
/// its `media:` prefix.
pub fn blob_ref_matches_handle(blob_ref: &BlobRef, handle: &str) -> bool {
    let hex = handle.strip_prefix(MEDIA_HANDLE_PREFIX).unwrap_or(handle);
    hex.len() == MEDIA_HANDLE_HEX_LEN && media_handle(blob_ref).ends_with(hex)
}

/// Every distinct media handle written in `text`, in first-seen order. A
/// handle is `media:` followed by exactly twelve lowercase hex characters
/// and not followed by another hex character.
pub fn find_media_handles(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(MEDIA_HANDLE_PREFIX) {
        let after = &rest[at + MEDIA_HANDLE_PREFIX.len()..];
        let hex_len = after
            .bytes()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        if hex_len == MEDIA_HANDLE_HEX_LEN
            && after[..hex_len]
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            let handle = format!("{MEDIA_HANDLE_PREFIX}{}", &after[..hex_len]);
            if !found.contains(&handle) {
                found.push(handle);
            }
        }
        rest = &after[hex_len.min(after.len())..];
    }
    found
}

/// What a media entry is to the providers: an image block or a document block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub enum MediaKind {
    Image,
    Document,
}

impl MediaKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Document => "document",
        }
    }
}

/// Why a tool-produced asset did not become a media entry. Never an error for
/// the run: the producer writes the reason into the visible text instead.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MediaRejection {
    #[error("{media_type} is not supported")]
    UnsupportedMediaType { media_type: String },
    #[error("{byte_len} bytes exceeds the {limit} byte limit")]
    TooLarge { byte_len: u64, limit: u64 },
    #[error("at most {limit} media items per result")]
    TooMany { limit: usize },
}

/// Classify a media type the providers accept natively as image or PDF.
pub fn media_kind(media_type: &str) -> Option<MediaKind> {
    let media_type = normalized_media_type(media_type);
    if IMAGE_MEDIA_TYPES.contains(&media_type.as_str()) {
        Some(MediaKind::Image)
    } else if media_type == PDF_MEDIA_TYPE {
        Some(MediaKind::Document)
    } else {
        None
    }
}

/// Lowercased media type without parameters (`image/PNG; q=1` → `image/png`).
pub fn normalized_media_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Admit one tool-produced asset: a supported type within the byte limit.
pub fn admit_tool_media(
    media_type: Option<&str>,
    byte_len: u64,
) -> Result<MediaKind, MediaRejection> {
    let media_type = media_type.unwrap_or("application/octet-stream");
    let kind = media_kind(media_type).ok_or_else(|| MediaRejection::UnsupportedMediaType {
        media_type: normalized_media_type(media_type),
    })?;
    if byte_len > MAX_TOOL_MEDIA_BYTES {
        return Err(MediaRejection::TooLarge {
            byte_len,
            limit: MAX_TOOL_MEDIA_BYTES,
        });
    }
    Ok(kind)
}

/// Recognize the media types this module admits from a file's leading bytes.
pub fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some(PDF_MEDIA_TYPE)
    } else {
        None
    }
}

/// The preview a media entry carries: `[image]`, `[image: photo.png]`,
/// `[document: report.pdf]`. Adapters recognize documents by this prefix.
pub fn media_preview(kind: MediaKind, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => format!("[{}: {name}]", kind.label()),
        None => format!("[{}]", kind.label()),
    }
}

/// The name a media entry's preview carries, if any.
pub fn media_preview_name(preview: Option<&str>) -> Option<String> {
    let preview = preview?.trim();
    let inner = preview.strip_prefix('[')?.strip_suffix(']')?;
    let (_, name) = inner.split_once(": ")?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The text that announces a media entry to the model, written immediately
/// before the provider-native block: `[image · media:3f9a2c1d4e7b · image/png]`
/// or `[document: report.pdf · media:9b21e04c77a1 · application/pdf]`.
pub fn media_announcement(content: &ContentRef, preview: Option<&str>) -> String {
    let media_type = content
        .media_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    let kind = media_kind(media_type).map_or("media", MediaKind::label);
    let name = media_preview_name(preview);
    let head = match name {
        Some(name) => format!("{kind}: {name}"),
        None => kind.to_owned(),
    };
    format!(
        "[{head} · {} · {media_type}]",
        media_handle(&content.content_ref)
    )
}

/// One line a tool writes into its visible text for an admitted asset:
/// `[image 1 · media:3f9a2c1d4e7b · image/png · 38 KiB]`.
pub fn tool_media_line(
    kind: MediaKind,
    index: usize,
    blob_ref: &BlobRef,
    media_type: &str,
    name: Option<&str>,
    byte_len: u64,
) -> String {
    let mut line = format!(
        "[{} {index} · {} · {media_type}",
        kind.label(),
        media_handle(blob_ref)
    );
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        line.push_str(" · ");
        line.push_str(name);
    }
    line.push_str(&format!(" · {}]", format_bytes(byte_len)));
    line
}

/// One line a tool writes for an asset it dropped:
/// `[image 2 omitted: image/svg+xml is not supported]`.
pub fn tool_media_omitted_line(label: &str, index: usize, rejection: &MediaRejection) -> String {
    format!("[{label} {index} omitted: {rejection}]")
}

pub fn format_bytes(byte_len: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    if byte_len >= MIB {
        format!("{:.1} MiB", byte_len as f64 / MIB as f64)
    } else if byte_len >= KIB {
        format!("{} KiB", byte_len.div_ceil(KIB))
    } else {
        format!("{byte_len} B")
    }
}

/// The user-role message entry that carries an admitted asset into context;
/// identical in shape to a run-input media entry.
pub fn media_context_entry(
    content_ref: BlobRef,
    media_type: &str,
    kind: MediaKind,
    name: Option<&str>,
) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: ContentRef {
            content_ref,
            media_type: Some(normalized_media_type(media_type)),
            provider_kind: None,
        },
        preview: Some(media_preview(kind, name)),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }
}

/// A media asset named for a consumer that did not see it enter context: the
/// parent of a sub-agent, an awaited promise's holder. Carries everything
/// needed to append a media entry without reading the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub struct MediaDescriptor {
    /// `media:` plus the first twelve hex characters of `content_ref`.
    pub handle: String,
    pub content_ref: BlobRef,
    pub media_type: String,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl MediaDescriptor {
    /// Describe an admitted media asset; `None` when its type is not one the
    /// providers accept natively.
    pub fn new(content_ref: BlobRef, media_type: &str, name: Option<&str>) -> Option<Self> {
        let kind = media_kind(media_type)?;
        Some(Self {
            handle: media_handle(&content_ref),
            content_ref,
            media_type: normalized_media_type(media_type),
            kind,
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    pub fn context_entry(&self) -> ContextEntryInput {
        media_context_entry(
            self.content_ref.clone(),
            &self.media_type,
            self.kind,
            self.name.as_deref(),
        )
    }
}

/// True for a message entry whose payload is provider-native media rather
/// than text: an admitted image or PDF.
pub fn is_media_content(content: &ContentRef) -> bool {
    content.media_type.as_deref().and_then(media_kind).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_is_the_first_twelve_hex_characters() {
        let blob_ref = BlobRef::from_bytes(b"payload");
        assert_eq!(media_handle(&blob_ref), "media:239f59ed55e7");
        assert!(blob_ref_matches_handle(&blob_ref, "media:239f59ed55e7"));
        assert!(blob_ref_matches_handle(&blob_ref, "239f59ed55e7"));
        assert!(!blob_ref_matches_handle(&blob_ref, "media:239f59ed55e"));
        assert!(!blob_ref_matches_handle(&blob_ref, "media:000000000000"));
    }

    #[test]
    fn handles_are_found_in_prose_once_each_and_only_when_exact() {
        let text = "See ![a](media:239f59ed55e7) and [b](media:aaaaaaaaaaaa) again \
                    media:239f59ed55e7; not media:239f59ed55e7f (too long) nor media:ABCDEFABCDEF";
        assert_eq!(
            find_media_handles(text),
            vec!["media:239f59ed55e7", "media:aaaaaaaaaaaa"]
        );
        assert!(find_media_handles("media:").is_empty());
    }

    #[test]
    fn admission_mirrors_run_input_rules() {
        assert_eq!(
            admit_tool_media(Some("image/png"), 10),
            Ok(MediaKind::Image)
        );
        assert_eq!(
            admit_tool_media(Some("Image/JPEG; q=1"), 10),
            Ok(MediaKind::Image)
        );
        assert_eq!(
            admit_tool_media(Some("application/pdf"), 10),
            Ok(MediaKind::Document)
        );
        assert_eq!(
            admit_tool_media(Some("image/svg+xml"), 10),
            Err(MediaRejection::UnsupportedMediaType {
                media_type: "image/svg+xml".into()
            })
        );
        assert_eq!(
            admit_tool_media(None, 10),
            Err(MediaRejection::UnsupportedMediaType {
                media_type: "application/octet-stream".into()
            })
        );
        assert_eq!(
            admit_tool_media(Some("image/png"), MAX_TOOL_MEDIA_BYTES + 1),
            Err(MediaRejection::TooLarge {
                byte_len: MAX_TOOL_MEDIA_BYTES + 1,
                limit: MAX_TOOL_MEDIA_BYTES
            })
        );
    }

    #[test]
    fn sniffing_recognizes_admitted_types_only() {
        assert_eq!(
            sniff_media_type(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(sniff_media_type(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff_media_type(b"GIF89a"), Some("image/gif"));
        assert_eq!(
            sniff_media_type(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_media_type(b"%PDF-1.7"), Some("application/pdf"));
        assert_eq!(sniff_media_type(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(sniff_media_type(b"plain text"), None);
        assert_eq!(sniff_media_type(b""), None);
    }

    #[test]
    fn labels_carry_handle_type_name_and_size() {
        let blob_ref = BlobRef::from_bytes(b"payload");
        assert_eq!(
            tool_media_line(MediaKind::Image, 1, &blob_ref, "image/png", None, 38_000),
            "[image 1 · media:239f59ed55e7 · image/png · 38 KiB]"
        );
        assert_eq!(
            tool_media_line(
                MediaKind::Document,
                2,
                &blob_ref,
                "application/pdf",
                Some("report.pdf"),
                3 * 1024 * 1024
            ),
            "[document 2 · media:239f59ed55e7 · application/pdf · report.pdf · 3.0 MiB]"
        );
        assert_eq!(
            tool_media_omitted_line(
                "image",
                3,
                &MediaRejection::UnsupportedMediaType {
                    media_type: "image/svg+xml".into()
                }
            ),
            "[image 3 omitted: image/svg+xml is not supported]"
        );
        assert_eq!(
            tool_media_omitted_line("audio", 1, &MediaRejection::TooMany { limit: 8 }),
            "[audio 1 omitted: at most 8 media items per result]"
        );
    }

    #[test]
    fn announcement_reads_the_entry_preview() {
        let entry = media_context_entry(
            BlobRef::from_bytes(b"payload"),
            "IMAGE/PNG",
            MediaKind::Image,
            Some(" photo.png "),
        );
        assert_eq!(entry.preview.as_deref(), Some("[image: photo.png]"));
        assert_eq!(entry.content.media_type.as_deref(), Some("image/png"));
        assert_eq!(
            media_announcement(&entry.content, entry.preview.as_deref()),
            "[image: photo.png · media:239f59ed55e7 · image/png]"
        );
        let unnamed = media_context_entry(
            BlobRef::from_bytes(b"payload"),
            "application/pdf",
            MediaKind::Document,
            None,
        );
        assert_eq!(
            media_announcement(&unnamed.content, unnamed.preview.as_deref()),
            "[document · media:239f59ed55e7 · application/pdf]"
        );
        assert!(is_media_content(&entry.content));
        assert!(!is_media_content(&ContentRef::text(BlobRef::from_bytes(
            b"x"
        ))));
    }
}
