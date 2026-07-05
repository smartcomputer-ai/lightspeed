use super::*;

/// P71 G3: images and documents, bounded per run.
const ALLOWED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
/// P72 G1: bounded audio blobs are accepted at admission, then rewritten by
/// workflow preprocessing before core planning.
const ALLOWED_AUDIO_MIMES: &[&str] = &[
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
const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
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
        match item {
            InputItem::Text { text } => {
                let text = text.trim();
                if !text.is_empty() {
                    let content_ref = store
                        .put_bytes(text.as_bytes().to_vec())
                        .await
                        .map_err(map_blob_store_error)?;
                    entries.push(user_message_input(content_ref));
                }
            }
            InputItem::TextRef { blob_ref } => {
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
    let raw_mime = mime.trim().to_ascii_lowercase();
    let mime = if matches!(kind, MediaKind::Audio) {
        normalize_audio_mime(&raw_mime)
    } else {
        raw_mime
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
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
            if !ALLOWED_AUDIO_MIMES.contains(&mime.as_str()) {
                return Err(AgentApiError::unsupported_audio_mime(format!(
                    "unsupported audio mime type {mime}; allowed: {}",
                    ALLOWED_AUDIO_MIMES.join(", ")
                )));
            }
            ("audio", MAX_AUDIO_BYTES)
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
        return if matches!(kind, MediaKind::Audio) {
            Err(AgentApiError::audio_blob_too_large(format!(
                "{label} blob is {} bytes; the limit is {max_bytes} bytes",
                info.byte_len
            )))
        } else {
            Err(AgentApiError::invalid_request(format!(
                "{label} blob is {} bytes; the limit is {max_bytes} bytes",
                info.byte_len
            )))
        };
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
        content_ref: blob_ref,
        media_type: Some(mime),
        preview: Some(preview),
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    })
}

pub(super) async fn context_entry_input_from_api(
    store: &dyn BlobStore,
    item: &InputItem,
) -> Result<ContextEntryInput, AgentApiError> {
    match item {
        InputItem::Text { text } => {
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
        InputItem::TextRef { blob_ref } => {
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
            blob_ref,
            mime,
            kind,
            name,
        } => media_message_input(store, blob_ref, mime, *kind, name.as_deref()).await,
    }
}

fn empty_context_append_item_error() -> AgentApiError {
    AgentApiError::invalid_request("session/context/append items must contain non-empty text")
}

pub(super) fn user_message_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}
pub(super) fn empty_run_input_error() -> AgentApiError {
    AgentApiError::invalid_request(
        "session/runs/start input must contain at least one non-empty item",
    )
}

fn normalize_audio_mime(mime: &str) -> String {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "audio/mp3" => "audio/mpeg",
        "audio/x-m4a" | "audio/m4a" => "audio/mp4",
        "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "audio/wav",
        "audio/oga" | "audio/opus" => "audio/ogg",
        "audio/x-aac" => "audio/aac",
        "audio/3gp" => "audio/3gpp",
        "audio/3gpp2" | "audio/3g2" => "audio/3gpp2",
        other => other,
    }
    .to_owned()
}
