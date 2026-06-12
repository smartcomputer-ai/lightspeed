use super::*;

/// G3 first cut: images only, bounded per run.
const ALLOWED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
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
    match kind {
        MediaKind::Image => {}
        MediaKind::Audio => {
            return Err(AgentApiError::invalid_request(
                "audio media input is not supported yet (P71 G6 transcription)",
            ));
        }
        MediaKind::Document => {
            return Err(AgentApiError::invalid_request(
                "document media input is not supported yet",
            ));
        }
    }
    let mime = mime.trim().to_ascii_lowercase();
    if !ALLOWED_IMAGE_MIMES.contains(&mime.as_str()) {
        return Err(AgentApiError::invalid_request(format!(
            "unsupported image mime type {mime}; allowed: {}",
            ALLOWED_IMAGE_MIMES.join(", ")
        )));
    }
    let blob_ref = parse_blob_ref(blob_ref)?;
    let info = store
        .stat_blob(&blob_ref)
        .await
        .map_err(map_input_blob_store_error)?;
    if info.byte_len > MAX_IMAGE_BYTES {
        return Err(AgentApiError::invalid_request(format!(
            "image blob is {} bytes; the limit is {MAX_IMAGE_BYTES} bytes",
            info.byte_len
        )));
    }
    let preview = match name {
        Some(name) if !name.trim().is_empty() => format!("[image: {}]", name.trim()),
        _ => "[image]".to_owned(),
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
        InputItem::Media { .. } => Err(AgentApiError::invalid_request(
            "media items are not supported in context/append yet; room media \
             arrives as placeholder text in the P71 G3 first cut",
        )),
    }
}

fn empty_context_append_item_error() -> AgentApiError {
    AgentApiError::invalid_request("context/append items must contain non-empty text")
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
    AgentApiError::invalid_request("run/start input must contain at least one non-empty text item")
}
