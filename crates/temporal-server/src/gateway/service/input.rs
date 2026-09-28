use super::*;

/// Images and documents, bounded per run.
const ALLOWED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
/// PDF is the only document type both providers accept natively; the text
/// MIMEs are inlined as text by the llm-runtime adapters.
const PDF_MIME: &str = "application/pdf";
const TEXT_DOCUMENT_MIMES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
];
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 10 * 1024 * 1024;
/// Text documents land in model context verbatim; keep them small.
const MAX_TEXT_DOCUMENT_BYTES: u64 = 1024 * 1024;
const MAX_MEDIA_ITEMS_PER_RUN: usize = 8;

pub(super) async fn run_input_from_api(
    store: &dyn BlobStore,
    input: &[InputItem],
) -> Result<Vec<ContextEntryInput>, AgentApiError> {
    let mut entries = Vec::new();
    let mut media_items = 0usize;
    for item in input {
        let origin = input_origin_from_api(item)?;
        let provenance_ref = input_provenance_from_api(store, item).await?;
        let first = entries.len();
        match item {
            InputItem::Text { text, .. } => {
                let text = text.trim();
                if !text.is_empty() {
                    let content_ref = store
                        .put_bytes(text.as_bytes().to_vec())
                        .await
                        .map_err(map_blob_store_error)?;
                    entries.push(user_message_input(content_ref));
                }
            }
            InputItem::TextRef { blob_ref, .. } => {
                let blob_ref = parse_blob_ref(blob_ref)?;
                let text = store
                    .read_text(&blob_ref)
                    .await
                    .map_err(map_input_blob_store_error)?;
                let text = text.trim();
                if !text.is_empty() {
                    entries.push(user_message_input(blob_ref));
                }
            }
            InputItem::Media {
                origin: _,
                blob_ref,
                mime,
                kind,
                name,
            } => {
                media_items += 1;
                if media_items > MAX_MEDIA_ITEMS_PER_RUN {
                    return Err(AgentApiError::invalid_request(format!(
                        "run input accepts at most {MAX_MEDIA_ITEMS_PER_RUN} media items"
                    )));
                }
                entries.push(
                    media_message_input(store, blob_ref, mime, *kind, name.as_deref()).await?,
                );
            }
            InputItem::Catalog { .. } => {
                return Err(AgentApiError::invalid_request(
                    "catalog items are context, not conversation: publish them with session/context/append",
                ));
            }
        }
        for entry in &mut entries[first..] {
            entry.origin = origin.clone();
            entry.provenance_ref = provenance_ref.clone();
        }
    }

    if entries.is_empty() {
        return Err(empty_run_input_error());
    }
    Ok(entries)
}

async fn media_message_input(
    store: &dyn BlobStore,
    blob_ref: &str,
    mime: &str,
    kind: MediaKind,
    name: Option<&str>,
) -> Result<ContextEntryInput, AgentApiError> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let (label, max_bytes) = match kind {
        MediaKind::Image => {
            if !ALLOWED_IMAGE_MIMES.contains(&mime.as_str()) {
                return Err(AgentApiError::invalid_request(format!(
                    "unsupported image mime type {mime}; allowed: {}",
                    ALLOWED_IMAGE_MIMES.join(", ")
                )));
            }
            ("image", MAX_IMAGE_BYTES)
        }
        MediaKind::Audio => {
            return Err(AgentApiError::invalid_request(
                "audio must be transcribed with transcriptions/start before session admission; submit text or a text reference",
            ));
        }
        MediaKind::Document if mime == PDF_MIME => ("document", MAX_PDF_BYTES),
        MediaKind::Document if TEXT_DOCUMENT_MIMES.contains(&mime.as_str()) => {
            ("document", MAX_TEXT_DOCUMENT_BYTES)
        }
        MediaKind::Document => {
            return Err(AgentApiError::invalid_request(format!(
                "unsupported document mime type {mime}; allowed: {PDF_MIME}, {}",
                TEXT_DOCUMENT_MIMES.join(", ")
            )));
        }
    };
    let blob_ref = parse_blob_ref(blob_ref)?;
    let info = store
        .stat_blob(&blob_ref)
        .await
        .map_err(map_input_blob_store_error)?;
    if info.byte_len > max_bytes {
        return Err(AgentApiError::invalid_request(format!(
            "{label} blob is {} bytes; the limit is {max_bytes} bytes",
            info.byte_len
        )));
    }
    if matches!(kind, MediaKind::Document) && mime != PDF_MIME {
        // Text documents reach the model as text; reject undecodable bytes
        // here instead of failing the run later in the adapter.
        store
            .read_text(&blob_ref)
            .await
            .map_err(map_input_blob_store_error)?;
    }
    let preview = match name {
        Some(name) if !name.trim().is_empty() => format!("[{label}: {}]", name.trim()),
        _ => format!("[{label}]"),
    };
    Ok(ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: engine::ContentRef {
            content_ref: blob_ref,
            media_type: Some(mime),
            provider_kind: None,
        },
        preview: Some(preview),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    })
}

pub(super) async fn context_entry_input_from_api(
    store: &dyn BlobStore,
    item: &InputItem,
) -> Result<ContextEntryInput, AgentApiError> {
    let origin = input_origin_from_api(item)?;
    let mut entry = match item {
        InputItem::Text { text, .. } => {
            let text = text.trim();
            if text.is_empty() {
                return Err(empty_context_append_item_error());
            }
            let content_ref = store
                .put_bytes(text.as_bytes().to_vec())
                .await
                .map_err(map_blob_store_error)?;
            Ok(user_message_input(content_ref))
        }
        InputItem::TextRef { blob_ref, .. } => {
            let blob_ref = parse_blob_ref(blob_ref)?;
            let text = store
                .read_text(&blob_ref)
                .await
                .map_err(map_input_blob_store_error)?;
            if text.trim().is_empty() {
                return Err(empty_context_append_item_error());
            }
            Ok(user_message_input(blob_ref))
        }
        InputItem::Media {
            origin: _,
            blob_ref,
            mime,
            kind,
            name,
        } => media_message_input(store, blob_ref, mime, *kind, name.as_deref()).await,
        InputItem::Catalog { title, text } => {
            let title = title.trim();
            if title.is_empty() {
                return Err(AgentApiError::invalid_request(
                    "session/context/append catalog items need a title",
                ));
            }
            let text = text.trim();
            if text.is_empty() {
                return Err(empty_context_append_item_error());
            }
            let content_ref = store
                .put_bytes(text.as_bytes().to_vec())
                .await
                .map_err(map_blob_store_error)?;
            Ok(catalog_input(title.to_owned(), content_ref))
        }
    }?;
    entry.origin = origin;
    entry.provenance_ref = input_provenance_from_api(store, item).await?;
    Ok(entry)
}

fn empty_context_append_item_error() -> AgentApiError {
    AgentApiError::invalid_request("session/context/append items must contain non-empty text")
}

/// A client-owned catalog entry; the engine supersedes rather than replaces
/// it on change, so the previous version keeps the rendered prefix stable.
pub(super) fn catalog_input(title: String, content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Catalog {
            title: title.clone(),
        },
        content: engine::ContentRef {
            content_ref,
            media_type: Some("text/markdown".to_owned()),
            provider_kind: None,
        },
        preview: Some(title),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }
}

pub(super) fn user_message_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: engine::ContentRef::text(content_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }
}
pub(super) fn empty_run_input_error() -> AgentApiError {
    AgentApiError::invalid_request(
        "session/runs/start input must contain at least one non-empty item",
    )
}

fn input_origin_from_api(item: &InputItem) -> Result<Option<String>, AgentApiError> {
    let origin = match item {
        InputItem::Text { origin, .. }
        | InputItem::TextRef { origin, .. }
        | InputItem::Media { origin, .. } => origin,
        InputItem::Catalog { .. } => return Ok(None),
    };
    if let Some(origin) = origin
        && (origin.trim().is_empty() || origin.len() > 200)
    {
        return Err(AgentApiError::invalid_request(
            "origin must contain 1–200 bytes and cannot be blank",
        ));
    }
    Ok(origin.clone())
}

async fn input_provenance_from_api(
    store: &dyn BlobStore,
    item: &InputItem,
) -> Result<Option<BlobRef>, AgentApiError> {
    let reference = match item {
        InputItem::Text { provenance_ref, .. } | InputItem::TextRef { provenance_ref, .. } => {
            provenance_ref
        }
        _ => return Ok(None),
    };
    let Some(reference) = reference else {
        return Ok(None);
    };
    let reference = parse_blob_ref(reference)?;
    store
        .stat_blob(&reference)
        .await
        .map_err(map_input_blob_store_error)?;
    Ok(Some(reference))
}
