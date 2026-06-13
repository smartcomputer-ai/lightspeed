use engine::{BlobRef, storage::BlobStore};
use serde::Serialize;
use serde_json::Value;

use crate::error::{LlmAdapterError, LlmAdapterResult};

/// Image media types the adapters materialize as provider-native image
/// parts. Mirrors the gateway's admission allowlist.
pub fn image_media_type(media_type: Option<&str>) -> Option<&str> {
    match media_type {
        Some(media_type @ ("image/jpeg" | "image/png" | "image/webp" | "image/gif")) => {
            Some(media_type)
        }
        _ => None,
    }
}

/// A user-message entry carrying an inbound document (P71 G3).
///
/// PDFs are unambiguous by media type. Text-based documents share
/// `text/plain`-family media types with ordinary text turns, so they are
/// recognized by the `[document...]` preview marker the gateway writes at
/// admission.
pub struct DocumentEntry {
    pub mime: String,
    /// Parsed from the `[document: <name>]` preview, when present.
    pub name: Option<String>,
    pub is_pdf: bool,
}

pub fn document_entry(media_type: Option<&str>, preview: Option<&str>) -> Option<DocumentEntry> {
    let mime = media_type?;
    let is_pdf = mime == "application/pdf";
    let is_text_document = matches!(
        mime,
        "text/plain" | "text/markdown" | "text/csv" | "application/json"
    ) && preview.is_some_and(|preview| preview.starts_with("[document"));
    if !is_pdf && !is_text_document {
        return None;
    }
    let name = preview
        .and_then(|preview| preview.strip_prefix("[document: "))
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::to_owned);
    Some(DocumentEntry {
        mime: mime.to_owned(),
        name,
        is_pdf,
    })
}

pub async fn read_base64(blobs: &dyn BlobStore, blob_ref: &BlobRef) -> LlmAdapterResult<String> {
    use base64::Engine as _;
    let bytes = blobs.read_bytes(blob_ref).await?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub async fn read_text(blobs: &dyn BlobStore, blob_ref: &BlobRef) -> LlmAdapterResult<String> {
    let bytes = blobs.read_bytes(blob_ref).await?;
    String::from_utf8(bytes).map_err(|error| LlmAdapterError::InvalidUtf8 {
        blob_ref: blob_ref.clone(),
        message: error.to_string(),
    })
}

pub async fn read_json(blobs: &dyn BlobStore, blob_ref: &BlobRef) -> LlmAdapterResult<Value> {
    let bytes = blobs.read_bytes(blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| LlmAdapterError::InvalidJson {
        blob_ref: blob_ref.clone(),
        message: error.to_string(),
    })
}

pub async fn put_bytes(blobs: &dyn BlobStore, bytes: Vec<u8>) -> LlmAdapterResult<BlobRef> {
    blobs.put_bytes(bytes).await.map_err(Into::into)
}

pub async fn put_text(blobs: &dyn BlobStore, text: impl Into<String>) -> LlmAdapterResult<BlobRef> {
    put_bytes(blobs, text.into().into_bytes()).await
}

pub async fn put_json<T>(blobs: &dyn BlobStore, value: &T) -> LlmAdapterResult<BlobRef>
where
    T: Serialize + ?Sized,
{
    let bytes =
        serde_json::to_vec(value).map_err(|error| LlmAdapterError::InvalidProviderRequest {
            message: format!("failed to encode JSON blob: {error}"),
        })?;
    put_bytes(blobs, bytes).await
}
