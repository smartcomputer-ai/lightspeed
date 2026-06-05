use super::*;

pub(super) async fn put_blob(
    store: &dyn BlobStore,
    params: BlobPutParams,
) -> Result<BlobPutResponse, AgentApiError> {
    let bytes = decode_base64(&params.bytes_base64, "bytesBase64")?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| AgentApiError::invalid_request("blob byte length does not fit in u64"))?;
    let blob_ref = store.put_bytes(bytes).await.map_err(map_blob_store_error)?;
    Ok(BlobPutResponse {
        blob_ref: blob_ref.as_str().to_owned(),
        bytes: byte_len,
    })
}

pub(super) async fn put_blobs(
    store: &dyn BlobStore,
    params: BlobPutManyParams,
) -> Result<BlobPutManyResponse, AgentApiError> {
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
    Ok(BlobPutManyResponse {
        blobs: blob_refs
            .into_iter()
            .zip(byte_lens)
            .map(|(blob_ref, bytes)| BlobPutResponse {
                blob_ref: blob_ref.as_str().to_owned(),
                bytes,
            })
            .collect(),
    })
}

pub(super) async fn get_blob(
    store: &dyn BlobStore,
    params: BlobGetParams,
) -> Result<BlobGetResponse, AgentApiError> {
    let blob_ref = parse_blob_ref(&params.blob_ref)?;
    let bytes = store
        .read_bytes(&blob_ref)
        .await
        .map_err(map_blob_read_error)?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| AgentApiError::internal("blob byte length does not fit in u64"))?;
    Ok(BlobGetResponse {
        blob_ref: blob_ref.as_str().to_owned(),
        bytes_base64: BASE64.encode(bytes),
        bytes: byte_len,
    })
}

pub(super) async fn has_blobs(
    store: &dyn BlobStore,
    params: BlobHasManyParams,
) -> Result<BlobHasManyResponse, AgentApiError> {
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
    Ok(BlobHasManyResponse { blobs })
}
pub(super) fn decode_base64(value: &str, field: impl AsRef<str>) -> Result<Vec<u8>, AgentApiError> {
    BASE64.decode(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid base64 in {}: {error}", field.as_ref()))
    })
}
