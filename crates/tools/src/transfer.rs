//! VFS/CAS orchestration over the provider-independent environment transfer protocol.
//! File payloads never enter model arguments or tool results.
use crate::error::{ToolError, ToolResult};
use async_trait::async_trait;
use engine::{
    BlobRef,
    storage::{BlobSource, BlobStore, BlobStoreError},
};
use environment_protocol::{
    data::{inventory::*, transfer::TransferOnExisting, transfer_session::*},
    shared::{ByteChunk, EnvironmentPath},
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
    let result = async {
        let initial = status(
            remote
                .request(TransferRequest::Begin {
                    operation_id: id.into(),
                    selection: TransferSelection::Materialize {
                        destination,
                        on_existing,
                    },
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
    graph: Option<&dyn engine::storage::BlobGraphStore>,
    id: &str,
    source: EnvironmentPath,
) -> ToolResult<CapturedSelection> {
    let initial = status(
        remote
            .request(TransferRequest::Begin {
                operation_id: id.into(),
                selection: TransferSelection::Capture { source },
                limits: InventoryLimits::default(),
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
    let snapshot = vfs::commit_snapshot_manifest(blobs, graph, manifest)
        .await
        .map_err(blob_error)?;
    Ok(CapturedSelection {
        entry,
        snapshot_ref: snapshot.snapshot_ref,
        status: state,
    })
}

pub type SharedEnvironmentTransfer = Arc<dyn EnvironmentTransfer>;

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
    crate::runtime::encode_output(
        &serde_json::json!({"operation_id":id,"destination":args.destination_environment_path,"receipt":receipt}),
        format!(
            "Materialized {} entries ({} bytes); transferred {} bytes, reused {} bytes.",
            receipt.entries, receipt.bytes, receipt.transferred_bytes, receipt.reused_bytes
        ),
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
        &serde_json::json!({"operation_id":id,"snapshot_ref":captured.snapshot_ref,"snapshot_path":"/selection","destination":args.destination_vfs_path,"published":published,"receipt":captured.status}),
        message,
    )?;
    persist_capture_result(vfs.blobs.as_ref(), graph.as_deref(), &output).await?;
    Ok(output)
}

async fn persist_capture_result(
    blobs: &dyn BlobStore,
    graph: Option<&dyn engine::storage::BlobGraphStore>,
    output: &crate::runtime::ToolInvocationOutput,
) -> ToolResult<()> {
    // The session retains the tool output. Its containment edge must retain the
    // snapshot too, especially when concurrent workspace edits prevent publication.
    let bytes = serde_json::to_vec(&output.output_json).map_err(blob_error)?;
    let output_ref = blobs.put_bytes(bytes).await.map_err(blob_error)?;
    engine::storage::record_contains_edges(
        graph,
        &output_ref,
        engine::storage::collect_blob_refs(&output.output_json),
    )
    .await
    .map_err(blob_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn unpublished_capture_is_retained_through_its_tool_result() {
        let store = engine::storage::InMemoryBlobStore::new();
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
        assert!(edges.contains(&engine::storage::BlobEdge::contains(
            output_ref,
            snapshot.snapshot_ref.clone(),
        )));
        assert!(edges.contains(&engine::storage::BlobEdge::contains(
            snapshot.snapshot_ref,
            file,
        )));
    }
}
