//! VFS/CAS orchestration over the provider-independent environment transfer protocol.
//! File payloads never enter model arguments or tool results.
use crate::error::{ToolError, ToolResult};
use async_trait::async_trait;
use environment_protocol::{
    data::{inventory::*, transfer::TransferOnExisting, transfer_session::*},
    shared::{ByteChunk, EnvironmentPath},
};
use harness::{
    BlobRef,
    storage::{BlobSource, BlobStore, BlobStoreError},
};
use std::{collections::BTreeMap, sync::Arc};

#[async_trait]
pub trait EnvironmentTransfer: Send + Sync {
    async fn request(&self, request: TransferRequest) -> ToolResult<TransferResponse>;
}
fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}
fn blob_error(error: impl std::fmt::Display) -> ToolError {
    invalid(error.to_string())
}
fn status(response: TransferResponse) -> ToolResult<TransferStatus> {
    match response {
        TransferResponse::Status(status) => Ok(status),
        _ => Err(invalid("unexpected transfer response")),
    }
}
async fn advance(
    remote: &dyn EnvironmentTransfer,
    id: &str,
    phase: TransferPhase,
) -> ToolResult<TransferStatus> {
    loop {
        let state = status(
            remote
                .request(TransferRequest::Advance {
                    operation_id: id.into(),
                })
                .await?,
        )?;
        if state.phase == phase || state.phase == TransferPhase::Complete {
            return Ok(state);
        }
        if state.phase == TransferPhase::Aborted {
            return Err(invalid("transfer aborted"));
        }
    }
}
fn flatten(path: String, entry: &vfs::VfsEntry, entries: &mut Vec<InventoryEntry>) {
    let content = match entry {
        vfs::VfsEntry::Directory(_) => InventoryContent::Directory,
        vfs::VfsEntry::File(file) => InventoryContent::File {
            size_bytes: file.size_bytes,
            executable: file.executable,
            digest: file.blob_ref.to_string(),
        },
    };
    entries.push(InventoryEntry {
        path: path.clone(),
        content,
    });
    if let vfs::VfsEntry::Directory(dir) = entry {
        for (name, child) in &dir.entries {
            flatten(
                if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                },
                child,
                entries,
            );
        }
    }
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

async fn materialize_selection(
    remote: &dyn EnvironmentTransfer,
    blobs: &dyn BlobStore,
    id: &str,
    entry: &vfs::VfsEntry,
    selection: TransferSelection,
) -> ToolResult<TransferStatus> {
    let result = async {
        let initial = status(
            remote
                .request(TransferRequest::Begin {
                    operation_id: id.into(),
                    selection,
                    limits: InventoryLimits::default(),
                })
                .await?,
        )?;
        if initial.phase == TransferPhase::Complete {
            return Ok(initial);
        }
        let mut entries = Vec::new();
        flatten(String::new(), entry, &mut entries);
        if entries.len() > InventoryLimits::default().max_entries as usize {
            return Err(invalid("VFS transfer entry quota exceeded"));
        }
        let source_refs: std::collections::BTreeSet<_> = entries
            .iter()
            .filter_map(|entry| match &entry.content {
                InventoryContent::File { digest, .. } => Some(digest),
                _ => None,
            })
            .collect();
        for digest in source_refs {
            blobs
                .retain_blob(&BlobRef::parse(digest.clone()).map_err(blob_error)?)
                .await
                .map_err(blob_error)?;
        }
        if initial.phase == TransferPhase::Scanning {
            advance(remote, id, TransferPhase::Inventory).await?;
        }
        for (index, page) in entries.chunks(MAX_INVENTORY_PAGE).enumerate() {
            remote
                .request(TransferRequest::Append {
                    operation_id: id.into(),
                    offset: (index * MAX_INVENTORY_PAGE) as u32,
                    entries: page.to_vec(),
                    last: (index + 1) * MAX_INVENTORY_PAGE >= entries.len(),
                })
                .await?;
        }
        let sizes: BTreeMap<_, _> = entries
            .iter()
            .filter_map(|e| match &e.content {
                InventoryContent::File {
                    digest, size_bytes, ..
                } => Some((digest.clone(), *size_bytes)),
                _ => None,
            })
            .collect();
        loop {
            // Missing pages shrink as uploads complete; always fetch the first page.
            let TransferResponse::Missing { digests, .. } = remote
                .request(TransferRequest::Missing {
                    operation_id: id.into(),
                    offset: 0,
                })
                .await?
            else {
                return Err(invalid("unexpected missing-content response"));
            };
            if digests.is_empty() {
                break;
            }
            for digest in digests {
                let blob_ref = BlobRef::parse(digest.clone()).map_err(blob_error)?;
                let size = *sizes
                    .get(&digest)
                    .ok_or_else(|| invalid("receiver requested undeclared content"))?;
                blobs.retain_blob(&blob_ref).await.map_err(blob_error)?;
                let mut offset = 0;
                loop {
                    let data = blobs
                        .read_blob_range(
                            &blob_ref,
                            offset,
                            MAX_CONTENT_CHUNK.min((size - offset) as usize),
                        )
                        .await
                        .map_err(blob_error)?;
                    if data.len() > MAX_CONTENT_CHUNK || (data.is_empty() && offset < size) {
                        return Err(invalid("CAS returned incomplete file content"));
                    }
                    let end = offset + data.len() as u64;
                    remote
                        .request(TransferRequest::Write {
                            operation_id: id.into(),
                            digest: digest.clone(),
                            offset,
                            data: ByteChunk::from(data),
                        })
                        .await?;
                    offset = end;
                    if offset == size {
                        break;
                    }
                }
            }
        }
        advance(remote, id, TransferPhase::Ready).await?;
        status(
            remote
                .request(TransferRequest::Commit {
                    operation_id: id.into(),
                })
                .await?,
        )
    }
    .await;
    // Transport failure is ambiguous: preserve the operation so the caller can inspect
    // its receipt. Explicit abort is available for cancellation/abandonment.
    result
}
struct CaptureSource<'a> {
    remote: &'a dyn EnvironmentTransfer,
    id: &'a str,
    digest: String,
    offset: u64,
    eof: bool,
}
#[async_trait]
impl BlobSource for CaptureSource<'_> {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
        if self.eof {
            return Ok(vec![]);
        }
        if max_bytes < MAX_CONTENT_CHUNK {
            return Err(BlobStoreError::Store {
                message: "capture source requires protocol chunk bound".into(),
            });
        }
        let response = self
            .remote
            .request(TransferRequest::Read {
                operation_id: self.id.into(),
                digest: self.digest.clone(),
                offset: self.offset,
            })
            .await
            .map_err(|e| BlobStoreError::Store {
                message: e.to_string(),
            })?;
        match response {
            TransferResponse::Chunk { data, eof } => {
                let bytes = data.into_inner();
                if bytes.len() > MAX_CONTENT_CHUNK || (bytes.is_empty() && !eof) {
                    return Err(BlobStoreError::Store {
                        message: "invalid capture chunk".into(),
                    });
                }
                self.offset += bytes.len() as u64;
                self.eof = eof;
                Ok(bytes)
            }
            _ => Err(BlobStoreError::Store {
                message: "unexpected capture response".into(),
            }),
        }
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
    let limits = if single_file {
        InventoryLimits {
            max_entries: 1,
            max_depth: 0,
            ..InventoryLimits::default()
        }
    } else {
        InventoryLimits::default()
    };
    let initial = status(
        remote
            .request(TransferRequest::Begin {
                operation_id: id.into(),
                selection: TransferSelection::Capture { source },
                limits,
            })
            .await?,
    )?;
    if initial.phase == TransferPhase::Scanning {
        advance(remote, id, TransferPhase::Ready).await?;
    }
    let mut entries = Vec::new();
    let mut offset = 0;
    loop {
        let TransferResponse::Inventory {
            entries: page,
            next_offset,
        } = remote
            .request(TransferRequest::Inventory {
                operation_id: id.into(),
                offset,
            })
            .await?
        else {
            return Err(invalid("unexpected inventory response"));
        };
        if page.len() > MAX_INVENTORY_PAGE
            || entries.len() + page.len() > InventoryLimits::default().max_entries as usize
        {
            return Err(invalid("capture inventory quota exceeded"));
        }
        entries.extend(page);
        match next_offset {
            Some(next) if next > offset => offset = next,
            Some(_) => return Err(invalid("inventory made no progress")),
            None => break,
        }
    }
    if single_file
        && !matches!(entries.as_slice(), [InventoryEntry { path, content: InventoryContent::File { .. } }] if path.is_empty())
    {
        return Err(invalid(
            "reference requires exactly one file, not a directory",
        ));
    }
    let mut manifest = vfs::VfsSnapshotManifest::empty();
    let mut refs = BTreeMap::new();
    for entry in &entries {
        let path = vfs::VfsPath::parse(if entry.path.is_empty() {
            "/selection".into()
        } else {
            format!("/selection/{}", entry.path)
        })
        .map_err(blob_error)?;
        match &entry.content {
            InventoryContent::Directory => {
                vfs::create_manifest_directory(&mut manifest, &path, false).map_err(blob_error)?
            }
            InventoryContent::File {
                digest,
                size_bytes,
                executable,
            } => {
                let blob_ref = BlobRef::parse(digest.clone()).map_err(blob_error)?;
                if !refs.contains_key(&blob_ref) {
                    if blobs.has_blob(&blob_ref).await.map_err(blob_error)? {
                        blobs.retain_blob(&blob_ref).await.map_err(blob_error)?;
                        if blobs
                            .stat_blob(&blob_ref)
                            .await
                            .map_err(blob_error)?
                            .byte_len
                            != *size_bytes
                        {
                            return Err(invalid("CAS size differs from capture"));
                        }
                    } else {
                        let mut source = CaptureSource {
                            remote,
                            id,
                            digest: digest.clone(),
                            offset: 0,
                            eof: false,
                        };
                        blobs
                            .put_stream(&blob_ref, *size_bytes, &mut source)
                            .await
                            .map_err(blob_error)?;
                    }
                    refs.insert(blob_ref.clone(), *size_bytes);
                }
                vfs::write_manifest_file_ref(
                    &mut manifest,
                    &path,
                    blob_ref,
                    *size_bytes,
                    None,
                    *executable,
                )
                .map_err(blob_error)?;
            }
        }
    }
    let state = status(
        remote
            .request(TransferRequest::Commit {
                operation_id: id.into(),
            })
            .await?,
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
        TransferSelection::WriteFile { destination },
    )
    .await?;
    guard.complete = true;
    Ok(crate::fs::tools::WriteFileResult {
        path: args.path,
        resolved_path,
        bytes_written,
        receipt: Some(receipt),
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
    pub receipt: TransferStatus,
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
        receipt: captured.status,
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
        {
            let remote = self.remote.clone();
            let operation_id = self.id.clone();
            handle.spawn(async move {
                let _ = remote
                    .request(TransferRequest::Abort { operation_id })
                    .await;
            });
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializeArgs {
    source_vfs_path: crate::fs::FsPath,
    destination_environment_path: EnvironmentPath,
    #[serde(default)]
    on_existing: TransferOnExisting,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureArgs {
    source_environment_path: EnvironmentPath,
    destination_vfs_path: crate::fs::FsPath,
    #[serde(default)]
    on_existing: TransferOnExisting,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct MaterializeResult {
    pub operation_id: String,
    pub destination: EnvironmentPath,
    pub receipt: TransferStatus,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct CaptureResult {
    pub operation_id: String,
    pub snapshot_ref: BlobRef,
    pub snapshot_path: String,
    pub destination: crate::fs::FsPath,
    pub published: bool,
    pub receipt: TransferStatus,
}

fn resolve_environment_path(
    ctx: &crate::environment::EnvironmentToolContext,
    path: &EnvironmentPath,
) -> ToolResult<EnvironmentPath> {
    let Some(cwd) = &ctx.process_cwd else {
        return Ok(path.clone());
    };
    let path =
        crate::environment::sources::absolute(std::path::Path::new(cwd.as_str()), path.as_str())
            .map_err(invalid)?;
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
    args.destination_environment_path =
        resolve_environment_path(ctx, &args.destination_environment_path)?;
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
        args.destination_environment_path.clone(),
        args.on_existing,
    )
    .await?;
    guard.complete = true;
    let message = format!(
        "Materialized {} entries ({} bytes); transferred {} bytes, reused {} bytes.",
        receipt.entries, receipt.bytes, receipt.transferred_bytes, receipt.reused_bytes
    );
    crate::runtime::encode_output(
        &MaterializeResult {
            operation_id: id,
            destination: args.destination_environment_path,
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
    args.source_environment_path = resolve_environment_path(ctx, &args.source_environment_path)?;
    let remote = ctx
        .transfer
        .as_deref()
        .ok_or_else(|| invalid("environment transfer unavailable"))?;
    let target = vfs
        .fs
        .prepare_vfs_capture(
            &args.destination_vfs_path,
            args.on_existing == TransferOnExisting::Replace,
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
    let captured = capture(
        remote,
        vfs.blobs.as_ref(),
        graph.as_deref(),
        &id,
        args.source_environment_path,
    )
    .await?;
    guard.complete = true;
    let publication = target.commit(captured.entry).await;
    let (published, message) = match publication {
        Ok(()) => (
            true,
            format!(
                "Captured {} entries into {}.",
                captured.status.entries, args.destination_vfs_path
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
            receipt: captured.status,
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
}
