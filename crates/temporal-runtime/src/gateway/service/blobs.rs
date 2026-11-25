use super::*;
use api::{BlobComposeParams, MAX_BLOB_COMPOSE_PARTS};
use harness::storage::{BlobSource, MAX_BLOB_RANGE_BYTES};
use sha2::{Digest as _, Sha256};

pub(super) async fn put_blobs(
    store: &dyn BlobStore,
    params: BlobPutParams,
) -> Result<BlobPutResponse, AgentApiError> {
    let mut byte_lens = Vec::with_capacity(params.blobs.len());
    let mut blobs = Vec::with_capacity(params.blobs.len());
    for (index, blob) in params.blobs.into_iter().enumerate() {
        let bytes = decode_base64(&blob.bytes_base64, format!("blobs[{index}].bytesBase64"))?;
        byte_lens.push(u64::try_from(bytes.len()).map_err(|_| {
            AgentApiError::invalid_request(format!(
                "blobs[{index}] byte length does not fit in u64"
            ))
        })?);
        blobs.push(bytes);
    }
    let blob_refs = store.put_many(blobs).await.map_err(map_blob_store_error)?;
    Ok(BlobPutResponse {
        blobs: blob_refs
            .into_iter()
            .zip(byte_lens)
            .map(|(blob_ref, bytes)| BlobPutResult {
                blob_ref: blob_ref.as_str().to_owned(),
                bytes,
            })
            .collect(),
    })
}

pub(super) async fn has_blobs(
    store: &dyn BlobStore,
    params: BlobHasParams,
) -> Result<BlobHasResponse, AgentApiError> {
    let mut blobs = Vec::with_capacity(params.blob_refs.len());
    for blob_ref in params.blob_refs {
        let blob_ref = parse_blob_ref(&blob_ref)?;
        let exists = store
            .has_blob(&blob_ref)
            .await
            .map_err(map_blob_store_error)?;
        blobs.push(BlobHasItem {
            blob_ref: blob_ref.as_str().to_owned(),
            exists,
        });
    }
    Ok(BlobHasResponse { blobs })
}
/// Join part blobs into `params.blob_ref` through the store's verified
/// streaming write. Parts are read in bounded ranges, so nothing is held whole.
pub(super) async fn compose_blob(
    store: &dyn BlobStore,
    params: BlobComposeParams,
) -> Result<BlobPutResult, AgentApiError> {
    let target = parse_blob_ref(&params.blob_ref)?;
    if params.parts.is_empty() || params.parts.len() > MAX_BLOB_COMPOSE_PARTS {
        return Err(AgentApiError::invalid_request(format!(
            "parts must name between 1 and {MAX_BLOB_COMPOSE_PARTS} blobs"
        )));
    }
    let result = BlobPutResult {
        blob_ref: target.as_str().to_owned(),
        bytes: params.size_bytes,
    };
    if store
        .has_blob(&target)
        .await
        .map_err(map_blob_store_error)?
    {
        let existing = store
            .stat_blob(&target)
            .await
            .map_err(map_blob_read_error)?;
        if existing.byte_len != params.size_bytes {
            return Err(AgentApiError::invalid_request(format!(
                "{} already exists with {} bytes, not {}",
                target, existing.byte_len, params.size_bytes
            )));
        }
        store
            .retain_blob(&target)
            .await
            .map_err(map_blob_store_error)?;
        return Ok(result);
    }

    let mut parts = Vec::with_capacity(params.parts.len());
    let mut total = 0u64;
    for (index, part) in params.parts.iter().enumerate() {
        let part = parse_blob_ref(part)?;
        let info = store.stat_blob(&part).await.map_err(|error| match error {
            BlobStoreError::NotFound { .. } => {
                AgentApiError::invalid_request(format!("parts[{index}] {part} does not exist"))
            }
            error => map_blob_store_error(error),
        })?;
        total = total
            .checked_add(info.byte_len)
            .ok_or_else(|| AgentApiError::invalid_request("the parts' total length overflows"))?;
        parts.push((part, info.byte_len));
    }
    if total != params.size_bytes {
        return Err(AgentApiError::invalid_request(format!(
            "the parts hold {total} bytes, not sizeBytes {}",
            params.size_bytes
        )));
    }

    let mut source = ComposeSource {
        store,
        parts,
        index: 0,
        offset: 0,
        digest: Sha256::new(),
    };
    let stored = store.put_stream(&target, total, &mut source).await;
    if source.index == source.parts.len() {
        let digest = format!("sha256:{}", hex::encode(source.digest.finalize()));
        if digest != target.as_str() {
            return Err(AgentApiError::invalid_request(format!(
                "the parts join to {digest}, not {target}"
            )));
        }
    }
    stored.map_err(map_blob_store_error)?;
    Ok(result)
}

/// The parts of a composed blob, read in order in bounded ranges. It hashes
/// what it hands out so a digest mismatch can be reported as the caller's.
struct ComposeSource<'a> {
    store: &'a dyn BlobStore,
    parts: Vec<(BlobRef, u64)>,
    index: usize,
    offset: u64,
    digest: Sha256,
}

#[async_trait::async_trait]
impl BlobSource for ComposeSource<'_> {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
        while let Some((part, length)) = self.parts.get(self.index) {
            if self.offset == *length {
                self.index += 1;
                self.offset = 0;
                continue;
            }
            let count =
                (*length - self.offset).min(max_bytes.min(MAX_BLOB_RANGE_BYTES) as u64) as usize;
            let chunk = self.store.read_blob_range(part, self.offset, count).await?;
            if chunk.len() != count {
                return Err(BlobStoreError::Store {
                    message: format!("part {part} changed length while composing"),
                });
            }
            self.offset += count as u64;
            self.digest.update(&chunk);
            return Ok(chunk);
        }
        Ok(Vec::new())
    }
}

pub(super) fn decode_base64(value: &str, field: impl AsRef<str>) -> Result<Vec<u8>, AgentApiError> {
    BASE64.decode(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid base64 in {}: {error}", field.as_ref()))
    })
}
