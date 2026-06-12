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
