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
/// As in compaction's estimate of the same entry.
const TEXT_BYTES_PER_TOKEN: u64 = 4;
/// No provider reports a window this small, so shorter text skips the model listing.
const REPORTED_LIMIT_MIN_TOKENS: u64 = 16 * 1024;

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
                let text = read_input_text(store, &blob_ref, "text input").await?;
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

/// Reads text input. Bytes that do not decode are the caller's own input and no
/// retry changes them; a store failure says nothing about the input and may heal.
async fn read_input_text(
    store: &dyn BlobStore,
    blob_ref: &BlobRef,
    what: &str,
) -> Result<String, AgentApiError> {
    let bytes = store
        .read_bytes(blob_ref)
        .await
        .map_err(map_input_blob_store_error)?;
    String::from_utf8(bytes)
        .map_err(|_| AgentApiError::invalid_request(format!("{what} is not valid UTF-8")))
}

/// Refuses input holding a text entry larger than the model's whole window: once in
/// context it would fail this run and every later one until someone replaces it. Each
/// entry is judged alone, because compaction can still rescue several smaller ones. A
/// limit the document declares covers only the document's model, and `reported_limit` is
/// awaited only for large text, so ordinary input never pays for discovery. With no known
/// limit nothing is refused.
pub(super) async fn refuse_oversized_text(
    store: &dyn BlobStore,
    input: &[ContextEntryInput],
    config: &harness::SessionConfig,
    model: &harness::ModelSelection,
    reported_limit: impl Future<Output = Option<u32>> + Send,
) -> Result<(), AgentApiError> {
    let mut largest = 0u64;
    for entry in input {
        let media_type = entry.content.media_type.as_deref();
        if media_type.and_then(harness::media::media_kind).is_some() {
            continue;
        }
        let info = store
            .stat_blob(&entry.content.content_ref)
            .await
            .map_err(map_input_blob_store_error)?;
        largest = largest.max(info.byte_len.div_ceil(TEXT_BYTES_PER_TOKEN));
    }
    let declared_limit = (*model == config.model)
        .then_some(config.context.input_limit_tokens)
        .flatten();
    let limit = match declared_limit {
        Some(limit) => Some(limit),
        None if largest >= REPORTED_LIMIT_MIN_TOKENS => reported_limit.await,
        None => None,
    };
    match limit {
        Some(limit) if largest > u64::from(limit) => Err(AgentApiError::invalid_request(format!(
            "a text input of about {largest} tokens cannot fit this model's window of {limit} tokens: shorten it or split it into smaller files"
        ))),
        _ => Ok(()),
    }
}

async fn media_message_input(
    store: &dyn BlobStore,
    blob_ref: &str,
    mime: &str,
    kind: MediaKind,
    name: Option<&str>,
) -> Result<ContextEntryInput, AgentApiError> {
    if let Some(name) = name {
        reject_nul("media name", name)?;
        if name.trim().chars().count() > api::MAX_MEDIA_NAME_CHARS {
            return Err(AgentApiError::invalid_request(format!(
                "media name is longer than {} characters",
                api::MAX_MEDIA_NAME_CHARS
            )));
        }
    }
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
        let document = match name {
            Some(name) if !name.trim().is_empty() => format!("document \"{}\"", name.trim()),
            _ => "document".to_owned(),
        };
        read_input_text(store, &blob_ref, &document).await?;
    }
    let preview = match name {
        Some(name) if !name.trim().is_empty() => format!("[{label}: {}]", name.trim()),
        _ => format!("[{label}]"),
    };
    Ok(ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: harness::ContentRef {
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
            let text = read_input_text(store, &blob_ref, "text input").await?;
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
            reject_nul("catalog title", title)?;
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

/// A client-owned catalog entry; the harness supersedes rather than replaces
/// it on change, so the previous version keeps the rendered prefix stable.
pub(super) fn catalog_input(title: String, content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Catalog {
            title: title.clone(),
        },
        content: harness::ContentRef {
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
        content: harness::ContentRef::text(content_ref),
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

/// Postgres cannot store U+0000. The session log replaces it, but a caller's own field
/// is better refused here, where the caller hears why.
pub(super) fn reject_nul(field: &str, value: &str) -> Result<(), AgentApiError> {
    if value.contains('\0') {
        return Err(AgentApiError::invalid_request(format!(
            "{field} cannot contain a NUL character"
        )));
    }
    Ok(())
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
    if let Some(origin) = origin {
        reject_nul("origin", origin)?;
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

#[cfg(test)]
mod tests {
    use harness::storage::InMemoryBlobStore;

    use super::*;

    /// Reads fail as a store outage would.
    struct FailingReads(InMemoryBlobStore);

    #[async_trait::async_trait]
    impl BlobStore for FailingReads {
        async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
            self.0.put_bytes(bytes).await
        }

        async fn read_bytes(&self, _blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
            Err(BlobStoreError::Store {
                message: "connection reset".to_owned(),
            })
        }

        async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
            self.0.has_blob(blob_ref).await
        }

        async fn stat_blob(
            &self,
            blob_ref: &BlobRef,
        ) -> Result<harness::storage::BlobInfo, BlobStoreError> {
            self.0.stat_blob(blob_ref).await
        }
    }

    fn csv(blob_ref: &BlobRef) -> InputItem {
        InputItem::Media {
            origin: None,
            blob_ref: blob_ref.to_string(),
            mime: "text/csv".to_owned(),
            kind: MediaKind::Document,
            name: Some("report.csv".to_owned()),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_document_that_does_not_decode_is_refused_by_name() {
        let store = InMemoryBlobStore::default();
        let windows_1252 = store.put_bytes(vec![b'a', 0xE9, b'b']).await.unwrap();
        let error = run_input_from_api(&store, &[csv(&windows_1252)])
            .await
            .unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        assert!(error.message.contains("report.csv"), "{}", error.message);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_store_failure_on_input_is_not_the_callers_fault() {
        let store = FailingReads(InMemoryBlobStore::default());
        let stored = store.put_bytes(b"a,b".to_vec()).await.unwrap();
        for item in [
            csv(&stored),
            InputItem::TextRef {
                origin: None,
                provenance_ref: None,
                blob_ref: stored.to_string(),
            },
        ] {
            let error = run_input_from_api(&store, &[item]).await.unwrap_err();
            assert_eq!(error.kind, AgentApiErrorKind::Internal);
        }
    }
}
