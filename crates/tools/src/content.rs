//! Shared resolution of immutable content references in the bound blob store.
//!
//! Recorded aliases provide identity and descriptive metadata, not authorization.
//! A full hash can address any blob in the runtime's bound universe.

use std::sync::Arc;

use harness::{
    Attachment, AttachmentSource, BlobRef,
    storage::{BlobGraphStore, BlobStore},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ToolResult;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ContentError {
    #[error(
        "invalid content reference: {reference}; expected a full sha256 reference or a recorded media:/file: handle"
    )]
    InvalidReference { reference: String },
    #[error("unknown content handle: {handle}")]
    UnknownHandle { handle: String },
    #[error("ambiguous content handle: {handle}")]
    AmbiguousHandle { handle: String },
    #[error("content offset {offset} exceeds blob size {byte_len}")]
    InvalidOffset { offset: u64, byte_len: u64 },
    #[error("{operation} requires {actual_bytes} bytes, exceeding the {max_bytes} byte limit")]
    LimitExceeded {
        operation: String,
        actual_bytes: u64,
        max_bytes: u64,
    },
    #[error(
        "content range at byte offset {offset} is not valid UTF-8 (valid prefix: {valid_up_to} bytes); adjust the byte range or read format bytes"
    )]
    InvalidUtf8 { offset: u64, valid_up_to: usize },
    #[error("content is not a complete JSON value: {message}")]
    InvalidJson { message: String },
    #[error("content cannot be presented as native media: {message}")]
    InvalidMedia { message: String },
}

/// An immutable identity plus verified size and optional descriptive metadata.
/// A handle is returned only when recorded or explicitly admitted as an attachment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContentDescriptor {
    pub content_ref: BlobRef,
    pub byte_len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Navigation/provenance metadata; never permission to fetch another source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AttachmentSource>,
}

/// Accept owned result envelopes without rewriting arbitrary external payloads.
/// Supplied sizes and handles are not trusted as availability or admission facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContentReferenceDescriptor {
    #[serde(alias = "blobRef")]
    pub content_ref: String,
    #[serde(
        default,
        alias = "mediaType",
        alias = "mimeType",
        skip_serializing_if = "Option::is_none"
    )]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AttachmentSource>,
}

/// A full SHA-256 ref, a recorded short handle, or an owned content descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ContentReference {
    Reference(String),
    Descriptor(ContentReferenceDescriptor),
}

impl From<BlobRef> for ContentReference {
    fn from(value: BlobRef) -> Self {
        Self::Reference(value.to_string())
    }
}

impl From<ContentDescriptor> for ContentReference {
    fn from(value: ContentDescriptor) -> Self {
        Self::Descriptor(ContentReferenceDescriptor {
            content_ref: value.content_ref.to_string(),
            media_type: value.media_type,
            name: value.name,
            source: value.source,
        })
    }
}

/// One host-side boundary for tools consuming content. Snapshots are supplied
/// from durable session records, including completed code calls and compacted
/// context; the resolver itself does not restore sessions or enumerate storage.
#[derive(Clone)]
pub struct ContentResolver {
    blobs: Arc<dyn BlobStore>,
    graph: Option<Arc<dyn BlobGraphStore>>,
    attachments: Vec<Attachment>,
}

impl ContentResolver {
    pub fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            blobs,
            graph: None,
            attachments: Vec::new(),
        }
    }

    pub fn with_blob_graph(mut self, graph: Arc<dyn BlobGraphStore>) -> Self {
        self.graph = Some(graph);
        self
    }

    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn store(&self) -> &Arc<dyn BlobStore> {
        &self.blobs
    }

    pub fn blob_graph(&self) -> Option<&dyn BlobGraphStore> {
        self.graph.as_deref()
    }

    /// Do not publish a short handle already bound to a different recorded asset.
    /// Callers can still use full hashes when a truncated handle collides.
    pub fn validate_attachment(&self, attachment: &Attachment) -> ToolResult<()> {
        match self.attachment_for_handle(attachment.handle()) {
            Ok(recorded) if same_alias_identity(recorded, attachment) => Ok(()),
            Err(ContentError::UnknownHandle { .. }) => Ok(()),
            _ => Err(ContentError::AmbiguousHandle {
                handle: attachment.handle().to_owned(),
            }
            .into()),
        }
    }

    pub async fn resolve(&self, reference: &ContentReference) -> ToolResult<ContentDescriptor> {
        let (value, supplied) = match reference {
            ContentReference::Reference(value) => (value.as_str(), None),
            ContentReference::Descriptor(value) => (value.content_ref.as_str(), Some(value)),
        };
        if let Some(supplied) = supplied {
            validate_content_metadata(
                supplied.name.as_deref(),
                supplied.media_type.as_deref(),
                supplied.source.as_ref(),
            )?;
        }
        let (content_ref, selected) = if value.starts_with("sha256:") {
            let reference = BlobRef::parse(value).map_err(|_| ContentError::InvalidReference {
                reference: value.to_owned(),
            })?;
            (reference, None)
        } else if valid_handle(value) {
            let attachment = self.attachment_for_handle(value)?;
            (attachment.content_ref().clone(), Some(attachment))
        } else {
            return Err(ContentError::InvalidReference {
                reference: value.to_owned(),
            }
            .into());
        };
        let info = self.blobs.stat_blob(&content_ref).await?;
        let attachment = selected.or_else(|| {
            self.attachments.iter().rev().find(|attachment| {
                attachment.content_ref() == &content_ref
                    && self.attachment_for_handle(attachment.handle()).is_ok()
            })
        });
        let (name, media_type, source) = match attachment {
            Some(Attachment::Media(media)) => {
                (media.name.clone(), Some(media.media_type.clone()), None)
            }
            Some(Attachment::File(file)) => (
                Some(file.name.clone()),
                file.media_type.clone(),
                file.source.clone(),
            ),
            None => (None, None, None),
        };
        let descriptor = ContentDescriptor {
            content_ref,
            byte_len: info.byte_len,
            media_type: supplied
                .and_then(|value| value.media_type.clone())
                .or(media_type),
            name: supplied.and_then(|value| value.name.clone()).or(name),
            handle: attachment.map(|value| value.handle().to_owned()),
            source: supplied.and_then(|value| value.source.clone()).or(source),
        };
        // Reused old content must survive the gap until its new durable owner
        // and containment edges are committed by the tool completion path.
        self.blobs.retain_blob(&descriptor.content_ref).await?;
        Ok(descriptor)
    }

    fn attachment_for_handle(&self, handle: &str) -> Result<&Attachment, ContentError> {
        let mut matches = self
            .attachments
            .iter()
            .filter(|item| item.handle() == handle);
        let first = matches.next().ok_or_else(|| ContentError::UnknownHandle {
            handle: handle.to_owned(),
        })?;
        if matches.any(|other| !same_alias_identity(first, other)) {
            return Err(ContentError::AmbiguousHandle {
                handle: handle.to_owned(),
            });
        }
        Ok(first)
    }
}

/// Bound caller-supplied descriptions independently of the immutable byte size.
/// Provenance is copied as data and never used to fetch or authorize content.
pub(crate) fn validate_content_metadata(
    name: Option<&str>,
    media_type: Option<&str>,
    source: Option<&AttachmentSource>,
) -> ToolResult<()> {
    for (label, value, limit, allow_empty) in [
        ("name", name, 1024, false),
        ("media_type", media_type, 256, false),
        (
            "source.kind",
            source.map(|source| source.kind.as_str()),
            128,
            false,
        ),
        (
            "source.id",
            source.map(|source| source.id.as_str()),
            8192,
            false,
        ),
        (
            "source.path",
            source.map(|source| source.path.as_str()),
            4096,
            true,
        ),
    ] {
        if let Some(value) = value
            && ((!allow_empty && value.trim().is_empty()) || value.len() > limit)
        {
            return Err(crate::ToolError::InvalidRequest {
                message: format!(
                    "{label} must be at most {limit} bytes{}",
                    if allow_empty { "" } else { " and nonblank" }
                ),
            });
        }
    }
    Ok(())
}

fn valid_handle(value: &str) -> bool {
    let (hex, len) = if let Some(hex) = value.strip_prefix("media:") {
        (hex, 12)
    } else if let Some(hex) = value.strip_prefix("file:") {
        (hex, 24)
    } else {
        return false;
    };
    hex.len() == len
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn same_alias_identity(left: &Attachment, right: &Attachment) -> bool {
    match (left, right) {
        (Attachment::Media(left), Attachment::Media(right)) => {
            left.content_ref == right.content_ref
                && left.media_type == right.media_type
                && left.kind == right.kind
        }
        (Attachment::File(left), Attachment::File(right)) => left.content_ref == right.content_ref,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        FileAttachment,
        storage::{BlobStoreError, InMemoryBlobStore},
    };
    use serde_json::json;

    #[tokio::test(flavor = "current_thread")]
    async fn full_refs_need_no_session_record_and_verify_supplied_size_and_handle() {
        let store = Arc::new(InMemoryBlobStore::new());
        let reference = store.put_bytes(b"hello".to_vec()).await.unwrap();
        let resolver = ContentResolver::new(store);
        let input = serde_json::from_value(json!({
            "content_ref":reference, "byte_len":9000, "name":"hello.txt", "handle":"file:000000000000000000000000"
        })).unwrap();
        let descriptor = resolver.resolve(&input).await.unwrap();
        assert_eq!(descriptor.content_ref, reference);
        assert_eq!(descriptor.byte_len, 5);
        assert_eq!(descriptor.name.as_deref(), Some("hello.txt"));
        assert_eq!(descriptor.handle, None);
        assert!(matches!(
            resolver
                .resolve(&ContentReference::from(BlobRef::from_bytes(b"absent")))
                .await,
            Err(crate::ToolError::BlobStore(BlobStoreError::NotFound { .. }))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recorded_aliases_resolve_but_ambiguous_or_unrecorded_aliases_fail() {
        let store = Arc::new(InMemoryBlobStore::new());
        let a = store.put_bytes(b"one".to_vec()).await.unwrap();
        let b = store.put_bytes(b"two".to_vec()).await.unwrap();
        let attachment =
            FileAttachment::new(a.clone(), "one.txt".into(), Some("text/plain".into()));
        let input = ContentReference::Reference(attachment.handle.clone());
        let resolver = ContentResolver::new(store.clone());
        assert!(matches!(
            resolver.resolve(&input).await,
            Err(crate::ToolError::Content(
                ContentError::UnknownHandle { .. }
            ))
        ));
        let resolver = resolver.with_attachments(vec![Attachment::File(attachment.clone())]);
        assert_eq!(resolver.resolve(&input).await.unwrap().content_ref, a);
        let mut conflicting = FileAttachment::new(b, "two.txt".into(), None);
        conflicting.handle = attachment.handle.clone();
        let resolver = ContentResolver::new(store).with_attachments(vec![
            Attachment::File(attachment),
            Attachment::File(conflicting),
        ]);
        assert!(matches!(
            resolver.resolve(&input).await,
            Err(crate::ToolError::Content(
                ContentError::AmbiguousHandle { .. }
            ))
        ));
        assert_eq!(resolver.resolve(&a.into()).await.unwrap().handle, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn supplied_provenance_is_bounded_descriptive_data() {
        let store = Arc::new(InMemoryBlobStore::new());
        let reference = store.put_bytes(b"body".to_vec()).await.unwrap();
        let resolver = ContentResolver::new(store);
        let source = AttachmentSource {
            kind: "web_fetch".into(),
            id: "https://example.org/page".into(),
            path: String::new(),
        };
        let descriptor = resolver
            .resolve(
                &serde_json::from_value(json!({
                    "content_ref":reference,"source":source,"media_type":"text/html"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(descriptor.source, Some(source));
        for extra in [
            json!({"name":"a".repeat(1025)}),
            json!({"media_type":"a".repeat(257)}),
            json!({"source":{"kind":"web_fetch","id":"a".repeat(8193),"path":""}}),
        ] {
            let mut value = json!({"content_ref":reference});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(matches!(
                resolver
                    .resolve(&serde_json::from_value(value).unwrap())
                    .await,
                Err(crate::ToolError::InvalidRequest { .. })
            ));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mcp_job_envelopes_are_accepted_without_admitting_claimed_handles() {
        let store = Arc::new(InMemoryBlobStore::new());
        let reference = store.put_bytes(b"data".to_vec()).await.unwrap();
        let resolver = ContentResolver::new(store);
        let input = serde_json::from_value(json!({
            "type":"resource", "blobRef":reference,"byteLen":123,"mediaType":"text/plain","other":"ignored"
        })).unwrap();
        let output = resolver.resolve(&input).await.unwrap();
        assert_eq!(output.content_ref, reference);
        assert_eq!(output.byte_len, 4);
        assert_eq!(output.media_type.as_deref(), Some("text/plain"));
        let mcp_input = serde_json::from_value(
            json!({"type":"image","blobRef":reference,"mimeType":"image/png"}),
        )
        .unwrap();
        assert_eq!(
            resolver
                .resolve(&mcp_input)
                .await
                .unwrap()
                .media_type
                .as_deref(),
            Some("image/png")
        );
        assert!(
            serde_json::from_value::<ContentReference>(json!({
                "content_ref":reference, "blobRef":reference
            }))
            .is_err()
        );
        for value in [
            "/tmp/file",
            "https://example.org/a",
            "file:0000",
            "sha256:ABCD",
        ] {
            assert!(matches!(
                resolver
                    .resolve(&ContentReference::Reference(value.into()))
                    .await,
                Err(crate::ToolError::Content(
                    ContentError::InvalidReference { .. }
                ))
            ));
        }
    }
}
