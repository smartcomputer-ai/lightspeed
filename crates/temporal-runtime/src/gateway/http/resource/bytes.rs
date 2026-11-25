//! Raw blob transfer through bounded storage reads and a verified upload spool.
use super::*;
use futures_util::StreamExt as _;
use harness::{
    BlobRef,
    storage::{BlobSource, BlobStore, BlobStoreError},
};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

const CHUNK_BYTES: usize = 256 * 1024;

pub(super) async fn download_operation(
    service: Arc<crate::gateway::GatewayAgentApi>,
    operation: &str,
    params: Value,
    headers: &HeaderMap,
) -> Result<Response, Failure> {
    let blob_ref = match operation {
        api::METHOD_BLOBS_READ => {
            service
                .authorize_method(api::METHOD_BLOBS_READ, None)
                .await?;
            let params: api::BlobReadParams =
                serde_json::from_value(params).map_err(invalid_json)?;
            BlobRef::parse(&params.blob_ref)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?
        }
        api::METHOD_VFS_WORKSPACES_FILES_READ => {
            service
                .workspace_file_blob_ref(serde_json::from_value(params).map_err(invalid_json)?)
                .await?
        }
        _ => return Err(AgentApiError::internal("unknown binary download binding").into()),
    };
    download(
        service.store().clone(),
        blob_ref,
        headers.get(header::RANGE),
    )
    .await
}

/// One satisfiable byte range of a blob, `start..end` (end exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ByteRange {
    Whole,
    Part { start: u64, end: u64 },
    Unsatisfiable,
}

/// Interpret a `Range` header for a blob of `length` bytes. Only one
/// `bytes=` range is honoured (`start-`, `start-end` or `-suffix`); anything
/// else is ignored and the whole blob is sent, as HTTP allows.
fn byte_range(value: Option<&HeaderValue>, length: u64) -> ByteRange {
    let Some(spec) = value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("bytes="))
    else {
        return ByteRange::Whole;
    };
    if spec.contains(',') {
        return ByteRange::Whole;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return ByteRange::Whole;
    };
    let digits = |text: &str| -> Option<u64> {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let (start, end) = match (first.is_empty(), last.is_empty()) {
        (true, false) => {
            let Some(suffix) = digits(last) else {
                return ByteRange::Whole;
            };
            if suffix == 0 {
                return ByteRange::Unsatisfiable;
            }
            (length.saturating_sub(suffix), length)
        }
        (false, _) => {
            let Some(start) = digits(first) else {
                return ByteRange::Whole;
            };
            let end = if last.is_empty() {
                length
            } else {
                match digits(last) {
                    Some(last) if last >= start => last.saturating_add(1).min(length),
                    _ => return ByteRange::Whole,
                }
            };
            (start, end)
        }
        (true, true) => return ByteRange::Whole,
    };
    if start >= length {
        return ByteRange::Unsatisfiable;
    }
    if start == 0 && end == length {
        return ByteRange::Whole;
    }
    ByteRange::Part { start, end }
}

async fn download(
    store: Arc<dyn BlobStore>,
    blob_ref: BlobRef,
    range: Option<&HeaderValue>,
) -> Result<Response, Failure> {
    let info = store.stat_blob(&blob_ref).await.map_err(blob_error)?;
    let length = info.byte_len;
    if length == 0 && blob_ref != BlobRef::from_bytes(&[]) {
        return Err(AgentApiError::internal("empty blob digest mismatch").into());
    }
    // stat_blob enforces universe ownership before response headers are sent.
    let (start, end) = match byte_range(range, length) {
        ByteRange::Whole => (0, length),
        ByteRange::Part { start, end } => (start, end),
        ByteRange::Unsatisfiable => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{length}")).expect("range header"),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return Ok(response);
        }
    };
    let partial = start != 0 || end != length;
    // A whole transfer verifies the digest at its end; a range cannot, so the
    // client checks the reassembled file.
    let stream = futures_util::stream::try_unfold(
        (store, blob_ref, start, Sha256::new()),
        move |(store, blob_ref, offset, mut digest)| async move {
            if offset == end {
                return Ok(None);
            }
            let count = (end - offset).min(CHUNK_BYTES as u64) as usize;
            let chunk = store.read_blob_range(&blob_ref, offset, count).await?;
            if chunk.len() != count {
                return Err(BlobStoreError::Store {
                    message: "blob length changed during transfer".to_owned(),
                });
            }
            if !partial {
                digest.update(&chunk);
            }
            let offset = offset + chunk.len() as u64;
            if !partial && offset == length && !digest_matches(digest.clone(), &blob_ref) {
                return Err(BlobStoreError::Store {
                    message: "blob digest mismatch during transfer".to_owned(),
                });
            }
            Ok(Some((
                Bytes::from(chunk),
                (store, blob_ref, offset, digest),
            )))
        },
    );
    let mut response = Body::from_stream(stream).into_response();
    if partial {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{length}", end - 1))
                .expect("range header"),
        );
    }
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&(end - start).to_string()).expect("byte length header"),
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn upload(
    state: Arc<GatewayState>,
    request: Request,
    max_body: usize,
) -> Result<Response, Failure> {
    let (mut parts, body) = request.into_parts();
    let mut authentication_headers = parts.headers.clone();
    authentication_headers.remove(UNIVERSE_HEADER);
    let admitted = state.api_admission().admit(&authentication_headers).await?;
    validate_percent_encoding(parts.uri.path())?;
    let Path(path) = Path::<HashMap<String, String>>::from_request_parts(&mut parts, &())
        .await
        .map_err(|error| Failure::status(StatusCode::BAD_REQUEST, error.body_text()))?;
    let store = state.deployment_store();
    let context = context_for_path(
        admitted,
        store.pool(),
        &parts.headers,
        api::METHOD_BLOBS_UPLOAD,
        path.get("universe").map(String::as_str),
    )
    .await?;
    if parts.uri.query().is_some_and(|query| !query.is_empty()) {
        return Err(
            AgentApiError::invalid_request("binary uploads take no query parameters").into(),
        );
    }
    if !is_media_type(&parts.headers, "application/octet-stream") {
        return Err(Failure::status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/octet-stream",
        ));
    }
    ensure_identity_encoding(&parts.headers)?;
    let mut lengths = parts.headers.get_all(header::CONTENT_LENGTH).iter();
    let length = lengths
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| AgentApiError::invalid_request("binary upload requires Content-Length"))?;
    if lengths.next().is_some() {
        return Err(AgentApiError::invalid_request("duplicate Content-Length").into());
    }
    if length > max_body as u64 {
        return Err(Failure::status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "binary upload exceeds the request limit",
        ));
    }
    let expected = BlobRef::parse(
        path.get("blobRef")
            .ok_or_else(|| AgentApiError::invalid_request("missing blob reference"))?,
    )
    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
    request_context::with_request_context(context.clone(), async {
        let service = state.api_for_request(&context).await?;
        service
            .authorize_method(api::METHOD_BLOBS_UPLOAD, None)
            .await?;
        let mut source = spool(body, length, &expected, None).await?;
        service
            .store()
            .put_stream(&expected, length, &mut source)
            .await
            .map_err(blob_error)?;
        success(
            api::BlobPutResult {
                blob_ref: expected.as_str().to_owned(),
                bytes: length,
            },
            HttpSuccess::Ok,
            api::METHOD_BLOBS_UPLOAD,
        )
    })
    .await
}

struct Spool {
    // Dropping a request future removes the spool, including on client abort.
    _temporary: tempfile::NamedTempFile,
    file: tokio::fs::File,
}
#[async_trait::async_trait]
impl BlobSource for Spool {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
        let mut bytes = vec![0u8; max_bytes.min(CHUNK_BYTES)];
        let count = self
            .file
            .read(&mut bytes)
            .await
            .map_err(|error| BlobStoreError::Store {
                message: format!("read upload spool: {error}"),
            })?;
        bytes.truncate(count);
        Ok(bytes)
    }
}

async fn spool(
    body: Body,
    length: u64,
    expected: &BlobRef,
    directory: Option<&std::path::Path>,
) -> Result<Spool, Failure> {
    let temporary = match directory {
        Some(directory) => tempfile::NamedTempFile::new_in(directory),
        None => tempfile::NamedTempFile::new(),
    }
    .map_err(io_error)?;
    let mut file = tokio::fs::File::from_std(temporary.reopen().map_err(io_error)?);
    let mut stream = body.into_data_stream();
    let mut received = 0u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| AgentApiError::invalid_request("upload body was interrupted"))?;
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| AgentApiError::invalid_request("upload length overflow"))?;
        if received > length {
            return Err(AgentApiError::invalid_request("upload exceeds Content-Length").into());
        }
        digest.update(&chunk);
        file.write_all(&chunk).await.map_err(io_error)?;
    }
    if received != length || !digest_matches(digest, expected) {
        return Err(
            AgentApiError::invalid_request("upload length or digest does not match").into(),
        );
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(io_error)?;
    Ok(Spool {
        _temporary: temporary,
        file,
    })
}
fn digest_matches(digest: Sha256, expected: &BlobRef) -> bool {
    let hex: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    expected.as_str().strip_prefix("sha256:") == Some(hex.as_str())
}
fn io_error(error: std::io::Error) -> Failure {
    AgentApiError::internal(format!("upload spool: {error}")).into()
}
fn blob_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { .. } => AgentApiError::not_found("blob not found"),
        other => AgentApiError::internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::storage::BlobInfo;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RangeStore {
        bytes: Vec<u8>,
        reference: BlobRef,
        reads: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl BlobStore for RangeStore {
        async fn put_bytes(&self, _: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
            panic!("download never writes");
        }
        async fn read_bytes(&self, _: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
            panic!("HTTP download must not buffer the complete blob");
        }
        async fn has_blob(&self, reference: &BlobRef) -> Result<bool, BlobStoreError> {
            Ok(reference == &self.reference)
        }
        async fn stat_blob(&self, reference: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
            if reference != &self.reference {
                return Err(BlobStoreError::NotFound {
                    blob_ref: reference.clone(),
                });
            }
            Ok(BlobInfo {
                blob_ref: reference.clone(),
                byte_len: self.bytes.len() as u64,
            })
        }
        async fn read_blob_range(
            &self,
            reference: &BlobRef,
            offset: u64,
            max_bytes: usize,
        ) -> Result<Vec<u8>, BlobStoreError> {
            assert_eq!(reference, &self.reference);
            assert!(max_bytes <= CHUNK_BYTES);
            self.reads.fetch_add(1, Ordering::SeqCst);
            let offset = offset as usize;
            Ok(self.bytes[offset..(offset + max_bytes).min(self.bytes.len())].to_vec())
        }
    }
    fn store() -> Arc<RangeStore> {
        let bytes = vec![42; CHUNK_BYTES * 3 + 19];
        Arc::new(RangeStore {
            reference: BlobRef::from_bytes(&bytes),
            bytes,
            reads: AtomicUsize::new(0),
        })
    }
    #[tokio::test(flavor = "current_thread")]
    async fn download_reads_only_when_polled_and_stops_after_drop() {
        let store = store();
        let response = download(store.clone(), store.reference.clone(), None)
            .await
            .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(store.reads.load(Ordering::SeqCst), 0);
        let mut stream = response.into_body().into_data_stream();
        assert_eq!(stream.next().await.unwrap().unwrap().len(), CHUNK_BYTES);
        assert_eq!(store.reads.load(Ordering::SeqCst), 1);
        drop(stream);
        tokio::task::yield_now().await;
        assert_eq!(store.reads.load(Ordering::SeqCst), 1);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn download_transfers_exact_bytes_and_refuses_unowned_references_before_headers() {
        let store = store();
        let response = download(store.clone(), store.reference.clone(), None)
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), store.bytes.len())
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), &store.bytes);
        assert_eq!(store.reads.load(Ordering::SeqCst), 4);
        let error = download(store, BlobRef::from_bytes(b"not owned"), None)
            .await
            .unwrap_err();
        assert_eq!(error.error.kind, AgentApiErrorKind::NotFound);
    }
    #[test]
    fn byte_range_honours_one_range_and_ignores_the_rest() {
        let header = |value: &str| HeaderValue::from_str(value).unwrap();
        let range = |value: &str| byte_range(Some(&header(value)), 100);
        assert_eq!(byte_range(None, 100), ByteRange::Whole);
        assert_eq!(
            range("bytes=10-"),
            ByteRange::Part {
                start: 10,
                end: 100
            }
        );
        assert_eq!(range("bytes=10-19"), ByteRange::Part { start: 10, end: 20 });
        assert_eq!(
            range("bytes=90-500"),
            ByteRange::Part {
                start: 90,
                end: 100
            }
        );
        assert_eq!(
            range("bytes=-30"),
            ByteRange::Part {
                start: 70,
                end: 100
            }
        );
        assert_eq!(range("bytes=0-"), ByteRange::Whole);
        assert_eq!(range("bytes=-500"), ByteRange::Whole);
        assert_eq!(range("bytes=100-"), ByteRange::Unsatisfiable);
        assert_eq!(range("bytes=-0"), ByteRange::Unsatisfiable);
        assert_eq!(range("bytes=0-1,5-6"), ByteRange::Whole);
        assert_eq!(range("bytes=20-10"), ByteRange::Whole);
        assert_eq!(range("bytes=a-"), ByteRange::Whole);
        assert_eq!(range("bytes=+1-"), ByteRange::Whole);
        assert_eq!(range("items=0-1"), ByteRange::Whole);
        assert_eq!(
            byte_range(Some(&header("bytes=0-")), 0),
            ByteRange::Unsatisfiable
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ranged_download_sends_only_the_requested_bytes() {
        let mut bytes = vec![0u8; CHUNK_BYTES + 100];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let store = Arc::new(RangeStore {
            reference: BlobRef::from_bytes(&bytes),
            bytes,
            reads: AtomicUsize::new(0),
        });
        let start = CHUNK_BYTES as u64 - 10;
        let header = HeaderValue::from_str(&format!("bytes={start}-")).unwrap();
        let response = download(store.clone(), store.reference.clone(), Some(&header))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let total = store.bytes.len();
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes {start}-{}/{total}", total - 1).as_str()
        );
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "110");
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        let body = axum::body::to_bytes(response.into_body(), total)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), &store.bytes[start as usize..]);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn unsatisfiable_range_reports_the_length() {
        let store = store();
        let header = HeaderValue::from_static("bytes=999999999-");
        let response = download(store.clone(), store.reference.clone(), Some(&header))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes */{}", store.bytes.len()).as_str()
        );
        assert_eq!(store.reads.load(Ordering::SeqCst), 0);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn upload_spool_verifies_digest_and_length_and_supplies_bounded_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = vec![17; CHUNK_BYTES + 12];
        let expected = BlobRef::from_bytes(&bytes);
        let mut source = spool(
            Body::from(bytes.clone()),
            bytes.len() as u64,
            &expected,
            Some(directory.path()),
        )
        .await
        .unwrap();
        assert_eq!(
            source.read_chunk(CHUNK_BYTES * 10).await.unwrap().len(),
            CHUNK_BYTES
        );
        assert_eq!(source.read_chunk(CHUNK_BYTES).await.unwrap().len(), 12);
        assert!(source.read_chunk(CHUNK_BYTES).await.unwrap().is_empty());
        drop(source);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        for (body, length) in [
            (b"bad".to_vec(), 3),
            (b"okay".to_vec(), 3),
            (b"ok".to_vec(), 3),
        ] {
            let error = match spool(
                Body::from(body),
                length,
                &BlobRef::from_bytes(b"yes"),
                Some(directory.path()),
            )
            .await
            {
                Ok(_) => panic!("invalid upload admitted"),
                Err(error) => error,
            };
            assert_eq!(error.error.kind, AgentApiErrorKind::InvalidRequest);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_upload_removes_its_partial_spool() {
        let directory = tempfile::tempdir().unwrap();
        let first = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(Bytes::from_static(b"part"))
        });
        let stream = first.chain(futures_util::stream::pending());
        let expected = BlobRef::from_bytes(b"partial");
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(30),
            spool(
                Body::from_stream(stream),
                7,
                &expected,
                Some(directory.path()),
            ),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
