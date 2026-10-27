//! VFS/CAS orchestration over the provider-independent environment transfer protocol.
//! File payloads never enter model arguments or tool results.
use crate::error::{ToolError, ToolResult};
use async_trait::async_trait;
use environment_client::ClientError;
use environment_protocol::{
    bytes::ByteChunk,
    data::{
        fs::{TreeContent, TreeFilter, WriteCondition},
        handshake::TransferCeilings,
        transfer::{
            AppendTransferInventoryParams, BeginTransferParams, InventoryEntry,
            ListTransferMissingParams, ListTransferMissingResponse, ReadTransferContentParams,
            ReadTransferContentResponse, ReadTransferInventoryParams,
            ReadTransferInventoryResponse, SpecialFilePolicy, SymlinkPolicy, TransferLimits,
            TransferOnExisting, TransferOperationParams, TransferPhase, TransferSelection,
            TransferStatus, WriteTransferContentParams, WriteTransferContentResponse,
        },
    },
    ids::TransferOperationId,
    paths::{ContentDigest, EnvironmentPath, TreePath},
};
use futures_util::TryStreamExt as _;
use harness::{
    BlobRef,
    storage::{BlobSource, BlobStore, BlobStoreError, MAX_BLOB_RANGE_BYTES},
};
use std::{collections::BTreeMap, sync::Arc};

/// Most entries one tool-driven transfer moves, within the endpoint's own
/// ceiling.
const MAX_TRANSFER_ENTRIES: u32 = 100_000;
/// Digests one operation moves at once over the multiplexed client. Each
/// digest's chunks still go in offset order, as the daemon requires; only
/// different digests overlap.
const CONTENT_CONCURRENCY: usize = 4;

/// The `transfer/*` methods of one environment. [`DataClient`] implements
/// it over the data plane; tests and in-process environments implement it
/// directly and report their errors as [`ClientError::Protocol`].
///
/// [`DataClient`]: environment_client::DataClient
#[async_trait]
pub trait EnvironmentTransfer: Send + Sync {
    /// The endpoint's advertised transfer ceilings.
    fn transfer_ceilings(&self) -> TransferCeilings;
    async fn begin(&self, params: &BeginTransferParams) -> Result<TransferStatus, ClientError>;
    async fn advance(
        &self,
        params: &TransferOperationParams,
    ) -> Result<TransferStatus, ClientError>;
    async fn get_status(
        &self,
        params: &TransferOperationParams,
    ) -> Result<TransferStatus, ClientError>;
    async fn read_inventory(
        &self,
        params: &ReadTransferInventoryParams,
    ) -> Result<ReadTransferInventoryResponse, ClientError>;
    async fn append(
        &self,
        params: &AppendTransferInventoryParams,
    ) -> Result<TransferStatus, ClientError>;
    async fn list_missing(
        &self,
        params: &ListTransferMissingParams,
    ) -> Result<ListTransferMissingResponse, ClientError>;
    async fn read(
        &self,
        params: &ReadTransferContentParams,
    ) -> Result<ReadTransferContentResponse, ClientError>;
    async fn write(
        &self,
        params: &WriteTransferContentParams,
    ) -> Result<WriteTransferContentResponse, ClientError>;
    async fn commit(&self, params: &TransferOperationParams)
    -> Result<TransferStatus, ClientError>;
    async fn abort(&self, params: &TransferOperationParams) -> Result<TransferStatus, ClientError>;
}

/// What a finished transfer moved, as the model sees it.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TransferReceipt {
    /// Entries in the transferred tree, the selected root included.
    pub entry_count: u32,
    /// File bytes in the transferred tree.
    pub total_bytes: u64,
    /// Content bytes moved over the wire.
    pub transferred_bytes: u64,
    /// Content bytes satisfied from content already at the destination.
    pub reused_bytes: u64,
    /// Entries a capture left out, such as names that are not UTF-8.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_entry_count: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl From<&TransferStatus> for TransferReceipt {
    fn from(status: &TransferStatus) -> Self {
        Self {
            entry_count: status.entry_count,
            total_bytes: status.total_bytes,
            transferred_bytes: status.transferred_bytes,
            reused_bytes: status.reused_bytes,
            skipped_entry_count: status.skipped_entry_count,
        }
    }
}

/// What a materialize does with an existing destination, as the model asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExisting {
    #[default]
    Replace,
    Error,
}

impl From<OnExisting> for TransferOnExisting {
    fn from(on_existing: OnExisting) -> Self {
        match on_existing {
            OnExisting::Replace => Self::Replace,
            OnExisting::Error => Self::Error,
        }
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}
fn blob_error(error: impl std::fmt::Display) -> ToolError {
    invalid(error.to_string())
}
fn transfer_error(error: ClientError) -> ToolError {
    invalid(format!("environment transfer failed: {error}"))
}

fn operation_id(id: &str) -> ToolResult<TransferOperationId> {
    TransferOperationId::new(id).map_err(|error| invalid(error.to_string()))
}

fn operation(id: &TransferOperationId) -> TransferOperationParams {
    TransferOperationParams {
        operation_id: id.clone(),
    }
}

/// The status, unless the operation failed or was aborted.
fn live(status: TransferStatus) -> ToolResult<TransferStatus> {
    match status.phase {
        TransferPhase::Failed => Err(invalid(format!(
            "environment transfer failed: {}",
            status
                .error
                .as_ref()
                .map_or_else(|| "no reason given".to_owned(), ToString::to_string)
        ))),
        TransferPhase::Aborted => Err(invalid("environment transfer was aborted")),
        _ => Ok(status),
    }
}

/// Caller quotas: the endpoint's ceilings, with the entry count bounded by
/// what one tool call may move.
fn limits(ceilings: &TransferCeilings, single_file: bool) -> TransferLimits {
    TransferLimits {
        max_entries: if single_file {
            1
        } else {
            ceilings.max_entries.min(MAX_TRANSFER_ENTRIES)
        },
        max_depth: if single_file { 0 } else { ceilings.max_depth },
        max_file_bytes: ceilings.max_file_bytes,
        max_total_bytes: ceilings.max_total_bytes,
        max_manifest_bytes: ceilings.max_manifest_bytes,
        max_duration_ms: ceilings.max_duration_ms,
    }
}

fn digest_of(blob_ref: &BlobRef) -> ToolResult<ContentDigest> {
    ContentDigest::parse(blob_ref.as_str())
        .map_err(|_| invalid("CAS reference is not a sha256 digest"))
}

fn blob_of(digest: &ContentDigest) -> ToolResult<BlobRef> {
    BlobRef::parse(digest.to_string()).map_err(blob_error)
}

fn flatten(
    path: TreePath,
    entry: &vfs::VfsEntry,
    entries: &mut Vec<InventoryEntry>,
) -> ToolResult<()> {
    let content = match entry {
        vfs::VfsEntry::Directory(_) => TreeContent::Directory {},
        vfs::VfsEntry::File(file) => TreeContent::File {
            size_bytes: file.size_bytes,
            executable: file.executable,
            digest: Some(digest_of(&file.blob_ref)?),
        },
    };
    entries.push(InventoryEntry {
        path: path.clone(),
        content,
    });
    if let vfs::VfsEntry::Directory(dir) = entry {
        for (name, child) in &dir.entries {
            let child_path = if path.is_root() {
                TreePath::new(name.clone())
            } else {
                path.join(name)
            }
            .map_err(|error| invalid(error.to_string()))?;
            flatten(child_path, child, entries)?;
        }
    }
    Ok(())
}

/// Resolve a workspace once before calling this method. Retries keep that immutable source.
pub async fn materialize(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    id: &str,
    entry: &vfs::VfsEntry,
    destination: EnvironmentPath,
    on_existing: TransferOnExisting,
) -> ToolResult<TransferStatus> {
    materialize_selection(
        remote,
        blobs,
        id,
        entry,
        TransferSelection::Materialize {
            destination,
            on_existing,
        },
    )
    .await
}

/// Drive a materialize or writeFile operation to completion, resuming from
/// whatever phase a retried `transfer/begin` reports. A transport failure
/// leaves the operation in place so a retry can inspect and resume it;
/// explicit abort is available for cancellation.
async fn materialize_selection(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    id: &str,
    entry: &vfs::VfsEntry,
    selection: TransferSelection,
) -> ToolResult<TransferStatus> {
    let ceilings = remote.transfer_ceilings();
    let id = operation_id(id)?;
    let single_file = matches!(selection, TransferSelection::WriteFile { .. });
    let quotas = limits(&ceilings, single_file);
    let mut entries = Vec::new();
    flatten(TreePath::root(), entry, &mut entries)?;
    if entries.len() as u64 > u64::from(quotas.max_entries) {
        return Err(invalid("VFS transfer entry quota exceeded"));
    }
    let sizes: BTreeMap<ContentDigest, u64> = entries
        .iter()
        .filter_map(|entry| match &entry.content {
            TreeContent::File {
                digest: Some(digest),
                size_bytes,
                ..
            } => Some((*digest, *size_bytes)),
            _ => None,
        })
        .collect();
    for digest in sizes.keys() {
        blobs
            .retain_blob(&blob_of(digest)?)
            .await
            .map_err(blob_error)?;
    }
    let mut state = live(
        remote
            .begin(&BeginTransferParams {
                operation_id: id.clone(),
                selection,
                limits: quotas,
            })
            .await
            .map_err(transfer_error)?,
    )?;
    let page_size = ceilings.max_inventory_page_entries.max(1) as usize;
    let mut stalled_content = false;
    loop {
        state = match state.phase {
            TransferPhase::Complete => return Ok(state),
            TransferPhase::Inventory => {
                let appended = state.entry_count as usize;
                let rest = entries.get(appended..).unwrap_or_default();
                if rest.is_empty() {
                    return Err(invalid("transfer inventory is complete but not sealed"));
                }
                let mut status = state;
                for (page_index, page) in rest.chunks(page_size).enumerate() {
                    let index = appended + page_index * page_size;
                    status = live(
                        remote
                            .append(&AppendTransferInventoryParams {
                                operation_id: id.clone(),
                                index: index as u32,
                                entries: page.to_vec(),
                                last: index + page.len() == entries.len(),
                            })
                            .await
                            .map_err(transfer_error)?,
                    )?;
                }
                status
            }
            TransferPhase::Content => {
                let missing = remote
                    .list_missing(&ListTransferMissingParams {
                        operation_id: id.clone(),
                        limit: ceilings.max_inventory_page_entries.max(1),
                    })
                    .await
                    .map_err(transfer_error)?;
                if missing.digests.is_empty() {
                    let next = live(
                        remote
                            .advance(&operation(&id))
                            .await
                            .map_err(transfer_error)?,
                    )?;
                    if next.phase == TransferPhase::Content {
                        if stalled_content {
                            return Err(invalid("environment transfer made no progress"));
                        }
                        stalled_content = true;
                    }
                    next
                } else {
                    futures_util::stream::iter(missing.digests.iter().map(Ok))
                        .try_for_each_concurrent(CONTENT_CONCURRENCY, |digest| {
                            upload(remote, blobs, &id, &ceilings, digest, &sizes)
                        })
                        .await?;
                    state
                }
            }
            TransferPhase::Scanning | TransferPhase::Staging => live(
                remote
                    .advance(&operation(&id))
                    .await
                    .map_err(transfer_error)?,
            )?,
            TransferPhase::Ready => live(
                remote
                    .commit(&operation(&id))
                    .await
                    .map_err(transfer_error)?,
            )?,
            TransferPhase::Failed | TransferPhase::Aborted => live(state)?,
        };
    }
}

async fn upload(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    id: &TransferOperationId,
    ceilings: &TransferCeilings,
    digest: &ContentDigest,
    sizes: &BTreeMap<ContentDigest, u64>,
) -> ToolResult<()> {
    let size = *sizes
        .get(digest)
        .ok_or_else(|| invalid("receiver requested undeclared content"))?;
    let blob_ref = blob_of(digest)?;
    blobs.retain_blob(&blob_ref).await.map_err(blob_error)?;
    let chunk = ceilings.max_content_chunk_bytes.max(1);
    let mut offset = 0;
    loop {
        let want = chunk.min(size - offset);
        // One transfer chunk may exceed the store's range bound: fill it
        // from consecutive ranges.
        let mut data = Vec::with_capacity(want as usize);
        while (data.len() as u64) < want {
            let remaining = want - data.len() as u64;
            let range = remaining.min(MAX_BLOB_RANGE_BYTES as u64);
            let bytes = blobs
                .read_blob_range(&blob_ref, offset + data.len() as u64, range as usize)
                .await
                .map_err(blob_error)?;
            if bytes.len() as u64 > range || bytes.is_empty() {
                return Err(invalid("CAS returned incomplete file content"));
            }
            data.extend_from_slice(&bytes);
        }
        if data.is_empty() && offset < size {
            return Err(invalid("CAS returned incomplete file content"));
        }
        let written = remote
            .write(&WriteTransferContentParams {
                operation_id: id.clone(),
                digest: *digest,
                offset,
                data: ByteChunk::from(data),
            })
            .await
            .map_err(transfer_error)?;
        offset = written.next_offset;
        if offset >= size {
            return Ok(());
        }
    }
}

struct CaptureSource<'a> {
    remote: &'a dyn EnvironmentTransfer,
    id: &'a TransferOperationId,
    digest: ContentDigest,
    chunk_bytes: u64,
    offset: u64,
    eof: bool,
}
#[async_trait]
impl BlobSource for CaptureSource<'_> {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
        if self.eof {
            return Ok(vec![]);
        }
        let want = (max_bytes as u64).min(self.chunk_bytes).max(1);
        let response = self
            .remote
            .read(&ReadTransferContentParams {
                operation_id: self.id.clone(),
                digest: self.digest,
                offset: self.offset,
                max_bytes: want,
            })
            .await
            .map_err(|e| BlobStoreError::Store {
                message: e.to_string(),
            })?;
        let bytes = response.data.into_inner();
        if bytes.len() as u64 > want || (bytes.is_empty() && response.truncated) {
            return Err(BlobStoreError::Store {
                message: "invalid capture chunk".into(),
            });
        }
        self.offset += bytes.len() as u64;
        self.eof = !response.truncated;
        Ok(bytes)
    }
}
#[derive(Clone, Debug)]
pub struct CapturedSelection {
    pub entry: vfs::VfsEntry,
    pub snapshot_ref: BlobRef,
    pub status: TransferStatus,
}
/// Capture raw content, deduplicate against CAS, then publish an immutable snapshot.
/// The selected node is stored at `/selection`, including when it is a single file.
pub async fn capture(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    graph: Option<&dyn harness::storage::BlobGraphStore>,
    id: &str,
    source: EnvironmentPath,
) -> ToolResult<CapturedSelection> {
    let captured = capture_selection(remote, blobs, id, source, false).await?;
    let snapshot = vfs::commit_snapshot_manifest(blobs, graph, captured.manifest)
        .await
        .map_err(blob_error)?;
    Ok(CapturedSelection {
        entry: captured.entry,
        snapshot_ref: snapshot.snapshot_ref,
        status: captured.status,
    })
}

struct CapturedContent {
    entry: vfs::VfsEntry,
    manifest: vfs::VfsSnapshotManifest,
    status: TransferStatus,
}

async fn capture_selection(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    id: &str,
    source: EnvironmentPath,
    single_file: bool,
) -> ToolResult<CapturedContent> {
    let ceilings = remote.transfer_ceilings();
    let id = operation_id(id)?;
    let quotas = limits(&ceilings, single_file);
    let mut state = live(
        remote
            .begin(&BeginTransferParams {
                operation_id: id.clone(),
                selection: TransferSelection::Capture {
                    source,
                    filter: TreeFilter {
                        include_hidden: true,
                        include_ignored: true,
                        ..TreeFilter::default()
                    },
                    symlinks: SymlinkPolicy::ExpandInside,
                    special_files: SpecialFilePolicy::Reject,
                },
                limits: quotas,
            })
            .await
            .map_err(transfer_error)?,
    )?;
    while state.phase == TransferPhase::Scanning {
        state = live(
            remote
                .advance(&operation(&id))
                .await
                .map_err(transfer_error)?,
        )?;
    }
    // A retried capture that already completed pages the inventory its
    // receipt retained and never revisits the live source.
    if !matches!(
        state.phase,
        TransferPhase::Inventory | TransferPhase::Complete
    ) {
        return Err(invalid(format!(
            "capture is in phase {:?}, not inventory",
            state.phase
        )));
    }
    let page_size = ceilings.max_inventory_page_entries.max(1);
    let mut entries = Vec::new();
    let mut index = 0;
    loop {
        let page = remote
            .read_inventory(&ReadTransferInventoryParams {
                operation_id: id.clone(),
                index,
                limit: page_size,
            })
            .await
            .map_err(transfer_error)?;
        if page.entries.len() > page_size as usize
            || entries.len() + page.entries.len() > quotas.max_entries as usize
        {
            return Err(invalid("capture inventory quota exceeded"));
        }
        entries.extend(page.entries);
        match page.next_index {
            Some(next) if next > index => index = next,
            Some(_) => return Err(invalid("inventory made no progress")),
            None => break,
        }
    }
    if single_file
        && !matches!(entries.as_slice(), [InventoryEntry { path, content: TreeContent::File { .. } }] if path.is_root())
    {
        return Err(invalid(
            "reference requires exactly one file, not a directory",
        ));
    }
    // Each distinct content once, fetched a few digests at a time.
    let mut refs: BTreeMap<BlobRef, (ContentDigest, u64)> = BTreeMap::new();
    for entry in &entries {
        if let TreeContent::File {
            digest, size_bytes, ..
        } = &entry.content
        {
            let digest = digest
                .as_ref()
                .ok_or_else(|| invalid("capture inventory omitted a file digest"))?;
            refs.entry(blob_of(digest)?)
                .or_insert((*digest, *size_bytes));
        }
    }
    futures_util::stream::iter(refs.iter().map(Ok))
        .try_for_each_concurrent(CONTENT_CONCURRENCY, |(blob_ref, (digest, size_bytes))| {
            fetch_content(
                remote,
                blobs,
                &id,
                &ceilings,
                blob_ref,
                *digest,
                *size_bytes,
            )
        })
        .await?;
    let mut manifest = vfs::VfsSnapshotManifest::empty();
    for entry in &entries {
        let path = vfs::VfsPath::parse(if entry.path.is_root() {
            "/selection".into()
        } else {
            format!("/selection/{}", entry.path)
        })
        .map_err(blob_error)?;
        match &entry.content {
            TreeContent::Directory {} => {
                vfs::create_manifest_directory(&mut manifest, &path, false).map_err(blob_error)?
            }
            TreeContent::File {
                digest,
                size_bytes,
                executable,
            } => {
                let digest = digest
                    .as_ref()
                    .ok_or_else(|| invalid("capture inventory omitted a file digest"))?;
                vfs::write_manifest_file_ref(
                    &mut manifest,
                    &path,
                    blob_of(digest)?,
                    *size_bytes,
                    None,
                    *executable,
                )
                .map_err(blob_error)?;
            }
        }
    }
    let state = live(
        remote
            .commit(&operation(&id))
            .await
            .map_err(transfer_error)?,
    )?;
    for blob_ref in refs.keys() {
        blobs.retain_blob(blob_ref).await.map_err(blob_error)?;
    }
    let entry = manifest
        .root
        .entries
        .get("selection")
        .cloned()
        .ok_or_else(|| invalid("capture omitted selected root"))?;
    Ok(CapturedContent {
        entry,
        manifest,
        status: state,
    })
}

/// Make one captured content available in CAS: reuse a blob already
/// stored, otherwise stream it from the environment in order.
async fn fetch_content(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    id: &TransferOperationId,
    ceilings: &TransferCeilings,
    blob_ref: &BlobRef,
    digest: ContentDigest,
    size_bytes: u64,
) -> ToolResult<()> {
    if blobs.has_blob(blob_ref).await.map_err(blob_error)? {
        blobs.retain_blob(blob_ref).await.map_err(blob_error)?;
        if blobs
            .stat_blob(blob_ref)
            .await
            .map_err(blob_error)?
            .byte_len
            != size_bytes
        {
            return Err(invalid("CAS size differs from capture"));
        }
        return Ok(());
    }
    let mut source = CaptureSource {
        remote,
        id,
        digest,
        chunk_bytes: ceilings.max_content_chunk_bytes,
        offset: 0,
        eof: false,
    };
    blobs
        .put_stream(blob_ref, size_bytes, &mut source)
        .await
        .map_err(blob_error)
        .map(drop)
}

pub type SharedEnvironmentTransfer = Arc<dyn EnvironmentTransfer>;

/// Materialize a single immutable blob using bounded provider transfer chunks.
pub(crate) async fn invoke_write_reference(
    ctx: &crate::environment::EnvironmentToolContext,
    args: crate::fs::tools::WriteFileArgs,
) -> ToolResult<crate::fs::tools::WriteFileResult> {
    args.validate()?;
    let filesystem = ctx
        .filesystem
        .as_ref()
        .ok_or_else(|| invalid("environment filesystem unavailable"))?;
    let resolved_path = crate::fs::tools::resolve_path(filesystem, &args.path)?;
    if !filesystem.fs.access_policy().can_write_path(&resolved_path) {
        return Err(crate::fs::FsError::PermissionDenied {
            path: resolved_path,
        }
        .into());
    }
    let reference = filesystem
        .content_resolver
        .resolve(args.content_ref.as_ref().unwrap())
        .await?;
    let bytes_written = usize::try_from(reference.byte_len)
        .map_err(|_| invalid("file size exceeds platform limits"))?;
    // `write_file` creates missing parents on every route; a transfer's
    // writeFile selection, like `fs/writeFile`, needs an existing parent.
    if let Some(parent) = resolved_path.parent()
        && !parent.is_root()
    {
        filesystem
            .fs
            .create_directory(&parent, crate::fs::CreateDirectoryOptions::recursive())
            .await?;
    }
    let remote = ctx
        .transfer
        .as_ref()
        .ok_or_else(|| invalid("environment transfer unavailable"))?;
    let destination = EnvironmentPath::new(resolved_path.as_str()).map_err(blob_error)?;
    let id = ctx
        .operation_id
        .clone()
        .unwrap_or_else(|| format!("write-ref-{}", uuid::Uuid::new_v4().simple()));
    // A completed receipt must survive source/destination changes on retry.
    // The transfer protocol checks the complete selection and operation identity.
    let mut guard = TransferGuard {
        remote: remote.clone(),
        id: id.clone(),
        complete: false,
    };
    let receipt = materialize_selection(
        remote.as_ref(),
        ctx.blobs.as_ref(),
        &id,
        &vfs::VfsEntry::File(vfs::VfsFile {
            blob_ref: reference.content_ref,
            size_bytes: reference.byte_len,
            media_type: reference.media_type,
            executable: false,
        }),
        TransferSelection::WriteFile {
            destination,
            create: WriteCondition::Always,
            if_match: None,
            executable: false,
        },
    )
    .await?;
    guard.complete = true;
    Ok(crate::fs::tools::WriteFileResult {
        path: args.path,
        resolved_path,
        bytes_written,
        receipt: Some(TransferReceipt::from(&receipt)),
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentReferenceArgs {
    path: crate::fs::FsPath,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct EnvironmentReferenceResult {
    #[serde(flatten)]
    pub content: crate::content::ContentDescriptor,
    pub receipt: TransferReceipt,
}

/// Capture one environment file into CAS without a VFS attachment or publication.
pub(crate) async fn invoke_environment_reference(
    ctx: &crate::environment::EnvironmentToolContext,
    arguments: serde_json::Value,
) -> ToolResult<crate::runtime::ToolInvocationOutput> {
    let args: EnvironmentReferenceArgs = crate::runtime::decode_args(arguments)?;
    let filesystem = ctx
        .filesystem
        .as_ref()
        .ok_or_else(|| invalid("environment filesystem unavailable"))?;
    let path = crate::fs::tools::resolve_path(filesystem, &args.path)?;
    if !filesystem.fs.access_policy().can_read_path(&path) {
        return Err(crate::fs::FsError::PermissionDenied { path }.into());
    }
    let source = EnvironmentPath::new(path.as_str()).map_err(blob_error)?;
    let remote = ctx
        .transfer
        .as_ref()
        .ok_or_else(|| invalid("environment transfer unavailable"))?;
    let id = ctx
        .operation_id
        .clone()
        .unwrap_or_else(|| format!("env-reference-{}", uuid::Uuid::new_v4().simple()));
    let mut guard = TransferGuard {
        remote: remote.clone(),
        id: id.clone(),
        complete: false,
    };
    let captured =
        capture_selection(remote.as_ref(), ctx.blobs.as_ref(), &id, source, true).await?;
    guard.complete = true;
    let vfs::VfsEntry::File(file) = captured.entry else {
        return Err(invalid("reference requires a file"));
    };
    let name = path
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or(path.as_str())
        .to_owned();
    let mut attachment =
        harness::FileAttachment::new(file.blob_ref.clone(), name.clone(), file.media_type.clone());
    attachment.source = ctx
        .environment_id
        .as_ref()
        .map(|environment| harness::AttachmentSource {
            kind: "environment".into(),
            id: environment.clone(),
            path: path.to_string(),
        });
    filesystem
        .content_resolver
        .validate_attachment(&harness::Attachment::File(attachment.clone()))?;
    let output = EnvironmentReferenceResult {
        content: crate::content::ContentDescriptor {
            content_ref: file.blob_ref,
            byte_len: file.size_bytes,
            media_type: file.media_type,
            name: Some(name),
            handle: Some(attachment.handle.clone()),
            source: attachment.source.clone(),
        },
        receipt: TransferReceipt::from(&captured.status),
    };
    let mut result = crate::runtime::encode_output(
        &output,
        format!(
            "Captured immutable file: {}\nReference: {}",
            attachment.name, attachment.handle
        ),
    )?;
    result
        .attachments
        .push(harness::Attachment::File(attachment));
    Ok(result)
}

struct TransferGuard {
    remote: Arc<dyn EnvironmentTransfer>,
    id: String,
    complete: bool,
}
impl Drop for TransferGuard {
    fn drop(&mut self) {
        if !self.complete
            && let Ok(handle) = tokio::runtime::Handle::try_current()
            && let Ok(operation_id) = TransferOperationId::new(self.id.clone())
        {
            let remote = self.remote.clone();
            handle.spawn(async move {
                let _ = remote
                    .abort(&TransferOperationParams { operation_id })
                    .await;
            });
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializeArgs {
    source_vfs_path: crate::fs::FsPath,
    destination_environment_path: String,
    #[serde(default)]
    on_existing: OnExisting,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureArgs {
    source_environment_path: String,
    destination_vfs_path: crate::fs::FsPath,
    #[serde(default)]
    on_existing: OnExisting,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct MaterializeResult {
    pub operation_id: String,
    pub destination: String,
    pub receipt: TransferReceipt,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct CaptureResult {
    pub operation_id: String,
    pub snapshot_ref: BlobRef,
    pub snapshot_path: String,
    pub destination: crate::fs::FsPath,
    pub published: bool,
    pub receipt: TransferReceipt,
}

/// An absolute environment path; a relative one resolves against the
/// process working directory.
fn resolve_environment_path(
    ctx: &crate::environment::EnvironmentToolContext,
    path: &str,
) -> ToolResult<EnvironmentPath> {
    let base = ctx.process_cwd.as_ref().map_or("/", |cwd| cwd.as_str());
    let path =
        crate::environment::sources::absolute(std::path::Path::new(base), path).map_err(invalid)?;
    EnvironmentPath::new(path.to_string_lossy()).map_err(|e| invalid(e.to_string()))
}

pub async fn invoke_materialize(
    vfs: &crate::fs::FsToolContext,
    ctx: &crate::environment::EnvironmentToolContext,
    operation_id: Option<&str>,
    arguments: serde_json::Value,
) -> ToolResult<crate::runtime::ToolInvocationOutput> {
    let mut args: MaterializeArgs = crate::runtime::decode_args(arguments)?;
    args.source_vfs_path = crate::fs::tools::resolve_path(vfs, &args.source_vfs_path)?;
    let destination = resolve_environment_path(ctx, &args.destination_environment_path)?;
    let remote = ctx
        .transfer
        .as_deref()
        .ok_or_else(|| invalid("environment transfer unavailable"))?;
    let entry = vfs.fs.export_vfs(&args.source_vfs_path).await?;
    let id = operation_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("materialize-{}", uuid::Uuid::new_v4().simple()));
    let mut guard = TransferGuard {
        remote: ctx.transfer.as_ref().unwrap().clone(),
        id: id.clone(),
        complete: false,
    };
    let receipt = materialize(
        remote,
        vfs.blobs.as_ref(),
        &id,
        &entry,
        destination.clone(),
        args.on_existing.into(),
    )
    .await?;
    guard.complete = true;
    let receipt = TransferReceipt::from(&receipt);
    let message = format!(
        "Materialized {} entries ({} bytes); transferred {} bytes, reused {} bytes.",
        receipt.entry_count, receipt.total_bytes, receipt.transferred_bytes, receipt.reused_bytes
    );
    crate::runtime::encode_output(
        &MaterializeResult {
            operation_id: id,
            destination: destination.as_str().to_owned(),
            receipt,
        },
        message,
    )
}
pub async fn invoke_capture(
    vfs: &crate::fs::FsToolContext,
    ctx: &crate::environment::EnvironmentToolContext,
    operation_id: Option<&str>,
    arguments: serde_json::Value,
) -> ToolResult<crate::runtime::ToolInvocationOutput> {
    let mut args: CaptureArgs = crate::runtime::decode_args(arguments)?;
    args.destination_vfs_path = crate::fs::tools::resolve_path(vfs, &args.destination_vfs_path)?;
    let source = resolve_environment_path(ctx, &args.source_environment_path)?;
    let remote = ctx
        .transfer
        .as_deref()
        .ok_or_else(|| invalid("environment transfer unavailable"))?;
    let target = vfs
        .fs
        .prepare_vfs_capture(
            &args.destination_vfs_path,
            args.on_existing == OnExisting::Replace,
        )
        .await?;
    let graph = target.blob_graph();
    let id = operation_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("capture-{}", uuid::Uuid::new_v4().simple()));
    let mut guard = TransferGuard {
        remote: ctx.transfer.as_ref().unwrap().clone(),
        id: id.clone(),
        complete: false,
    };
    let captured = capture(remote, vfs.blobs.as_ref(), graph.as_deref(), &id, source).await?;
    guard.complete = true;
    let publication = target.commit(captured.entry).await;
    let (published, message) = match publication {
        Ok(()) => (
            true,
            format!(
                "Captured {} entries into {}.",
                captured.status.entry_count, args.destination_vfs_path
            ),
        ),
        Err(error) => (
            false,
            format!(
                "Capture saved as {} at /selection, but workspace publication failed: {error}",
                captured.snapshot_ref
            ),
        ),
    };
    let output = crate::runtime::encode_output(
        &CaptureResult {
            operation_id: id,
            snapshot_ref: captured.snapshot_ref,
            snapshot_path: "/selection".into(),
            destination: args.destination_vfs_path,
            published,
            receipt: TransferReceipt::from(&captured.status),
        },
        message,
    )?;
    persist_capture_result(vfs.blobs.as_ref(), graph.as_deref(), &output).await?;
    Ok(output)
}

async fn persist_capture_result(
    blobs: &dyn BlobStore,
    graph: Option<&dyn harness::storage::BlobGraphStore>,
    output: &crate::runtime::ToolInvocationOutput,
) -> ToolResult<()> {
    // The session retains the tool output. Its containment edge must retain the
    // snapshot too, especially when concurrent workspace edits prevent publication.
    let bytes = serde_json::to_vec(&output.output_json).map_err(blob_error)?;
    let output_ref = blobs.put_bytes(bytes).await.map_err(blob_error)?;
    harness::storage::record_contains_edges(
        graph,
        &output_ref,
        harness::storage::collect_blob_refs(&output.output_json),
    )
    .await
    .map_err(blob_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn unpublished_capture_is_retained_through_its_tool_result() {
        let store = harness::storage::InMemoryBlobStore::new();
        let file = store.put_bytes(b"captured bytes".to_vec()).await.unwrap();
        let mut manifest = vfs::VfsSnapshotManifest::empty();
        vfs::write_manifest_file_ref(
            &mut manifest,
            &vfs::VfsPath::parse("/selection").unwrap(),
            file.clone(),
            14,
            None,
            false,
        )
        .unwrap();
        let snapshot = vfs::commit_snapshot_manifest(&store, Some(&store), manifest)
            .await
            .unwrap();
        let output = crate::runtime::encode_output(
            &serde_json::json!({"published": false, "snapshot_ref": snapshot.snapshot_ref}),
            "Workspace changed; capture saved.",
        )
        .unwrap();
        persist_capture_result(&store, Some(&store), &output)
            .await
            .unwrap();
        let output_ref = BlobRef::from_bytes(&serde_json::to_vec(&output.output_json).unwrap());
        let edges = store.edges();
        assert!(edges.contains(&harness::storage::BlobEdge::contains(
            output_ref,
            snapshot.snapshot_ref.clone(),
        )));
        assert!(edges.contains(&harness::storage::BlobEdge::contains(
            snapshot.snapshot_ref,
            file,
        )));
    }

    #[test]
    fn vfs_trees_flatten_into_tree_paths_with_digests() {
        let file = vfs::VfsFile {
            blob_ref: BlobRef::from_bytes(b"x"),
            size_bytes: 1,
            media_type: None,
            executable: true,
        };
        let mut nested = vfs::VfsDirectory::default();
        nested
            .entries
            .insert("lib.rs".into(), vfs::VfsEntry::File(file.clone()));
        let mut root = vfs::VfsDirectory::default();
        root.entries
            .insert("src".into(), vfs::VfsEntry::Directory(nested));
        let mut entries = Vec::new();
        flatten(
            TreePath::root(),
            &vfs::VfsEntry::Directory(root),
            &mut entries,
        )
        .unwrap();
        let paths = entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![".", "src", "src/lib.rs"]);
        assert!(matches!(
            &entries[2].content,
            TreeContent::File { digest: Some(digest), executable: true, .. }
                if digest.to_string() == file.blob_ref.as_str()
        ));
    }
}
