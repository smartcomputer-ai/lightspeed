//! Compatibility adapters for small inline copies. All confinement, hashing and
//! publication use the same backend as paged transfers on Linux and macOS.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod session;
use super::*;
use environment_protocol::data::{inventory::*, transfer_session::*};
use sha2::{Digest, Sha256};
fn invalid(message: &str) -> EnvironmentProtocolError {
    EnvironmentProtocolError::new(EnvironmentProtocolErrorCode::InvalidRequest, message)
}
fn limits(value: TransferLimits) -> Result<InventoryLimits, EnvironmentProtocolError> {
    if value.max_entries == 0
        || value.max_entries > MAX_TRANSFER_ENTRIES
        || value.max_depth > MAX_TRANSFER_DEPTH
        || value.max_file_bytes > MAX_TRANSFER_BYTES
        || value.max_total_bytes > MAX_TRANSFER_BYTES
        || value.max_duration_ms == 0
        || value.max_duration_ms > MAX_TRANSFER_DURATION_MS
    {
        return Err(invalid("inline transfer limits exceed protocol ceilings"));
    }
    Ok(InventoryLimits {
        max_entries: value.max_entries,
        max_depth: value.max_depth,
        max_file_bytes: value.max_file_bytes,
        max_total_bytes: value.max_total_bytes,
        max_duration_ms: value.max_duration_ms,
        ..Default::default()
    })
}
async fn advance(
    fs: &LocalFileSystem,
    id: &str,
    phase: TransferPhase,
) -> Result<(), EnvironmentProtocolError> {
    loop {
        if let TransferResponse::Status(status) = fs
            .transfer(TransferRequest::Advance {
                operation_id: id.into(),
            })
            .await?
            && status.phase == phase
        {
            return Ok(());
        }
    }
}
pub(super) async fn capture(
    fs: &LocalFileSystem,
    params: CaptureParams,
) -> Result<CaptureResponse, EnvironmentProtocolError> {
    let id = format!("inline-{:032x}", rand::random::<u128>());
    let result = async {
        fs.transfer(TransferRequest::Begin {
            operation_id: id.clone(),
            selection: TransferSelection::Capture {
                source: params.source.clone(),
            },
            limits: limits(params.limits)?,
        })
        .await?;
        advance(fs, &id, TransferPhase::Ready).await?;
        let mut entries = Vec::new();
        let mut offset = 0;
        let mut bytes = 0;
        loop {
            let TransferResponse::Inventory {
                entries: page,
                next_offset,
            } = fs
                .transfer(TransferRequest::Inventory {
                    operation_id: id.clone(),
                    offset,
                })
                .await?
            else {
                return Err(invalid("unexpected inventory"));
            };
            for entry in page {
                let content = match entry.content {
                    InventoryContent::Directory => TransferContent::Directory,
                    InventoryContent::File {
                        digest,
                        size_bytes,
                        executable,
                    } => {
                        let mut data = Vec::with_capacity(size_bytes as usize);
                        loop {
                            let TransferResponse::Chunk { data: chunk, eof } = fs
                                .transfer(TransferRequest::Read {
                                    operation_id: id.clone(),
                                    digest: digest.clone(),
                                    offset: data.len() as u64,
                                })
                                .await?
                            else {
                                return Err(invalid("unexpected chunk"));
                            };
                            data.extend(chunk.into_inner());
                            if eof {
                                break;
                            }
                        }
                        bytes += size_bytes;
                        TransferContent::File {
                            data: ByteChunk::from(data),
                            executable,
                        }
                    }
                };
                entries.push(TransferEntry {
                    path: entry.path,
                    content,
                });
            }
            if let Some(next) = next_offset {
                offset = next;
            } else {
                break;
            }
        }
        fs.transfer(TransferRequest::Commit {
            operation_id: id.clone(),
        })
        .await?;
        Ok(CaptureResponse {
            source: params.source,
            entries,
            bytes,
        })
    }
    .await;
    if result.is_err() {
        let _ = fs
            .transfer(TransferRequest::Abort { operation_id: id })
            .await;
    }
    result
}
pub(super) async fn materialize(
    fs: &LocalFileSystem,
    params: MaterializeParams,
) -> Result<MaterializeResponse, EnvironmentProtocolError> {
    let quota = limits(params.limits)?;
    if params.entries.len() > quota.max_entries as usize {
        return Err(invalid("inline entry quota exceeded"));
    }
    let id = format!("inline-{:032x}", rand::random::<u128>());
    let result = async {
        let mut content = std::collections::BTreeMap::new();
        let mut entries = Vec::new();
        let mut bytes = 0u64;
        for entry in &params.entries {
            let kind = match &entry.content {
                TransferContent::Directory => InventoryContent::Directory,
                TransferContent::File { data, executable } => {
                    bytes += data.as_slice().len() as u64;
                    if bytes > quota.max_total_bytes {
                        return Err(invalid("inline byte quota exceeded"));
                    }
                    let digest = format!("sha256:{}", hex::encode(Sha256::digest(data.as_slice())));
                    content.insert(digest.clone(), data.as_slice());
                    InventoryContent::File {
                        digest,
                        size_bytes: data.as_slice().len() as u64,
                        executable: *executable,
                    }
                }
            };
            entries.push(InventoryEntry {
                path: entry.path.clone(),
                content: kind,
            });
        }
        if entries.is_empty() {
            return Err(invalid("empty inventory"));
        }
        fs.transfer(TransferRequest::Begin {
            operation_id: id.clone(),
            selection: TransferSelection::Materialize {
                destination: params.destination.clone(),
                on_existing: params.on_existing,
            },
            limits: quota,
        })
        .await?;
        advance(fs, &id, TransferPhase::Inventory).await?;
        for (index, page) in entries.chunks(MAX_INVENTORY_PAGE).enumerate() {
            fs.transfer(TransferRequest::Append {
                operation_id: id.clone(),
                offset: (index * MAX_INVENTORY_PAGE) as u32,
                entries: page.to_vec(),
                last: (index + 1) * MAX_INVENTORY_PAGE >= entries.len(),
            })
            .await?;
        }
        loop {
            let TransferResponse::Missing { digests, .. } = fs
                .transfer(TransferRequest::Missing {
                    operation_id: id.clone(),
                    offset: 0,
                })
                .await?
            else {
                return Err(invalid("unexpected missing response"));
            };
            if digests.is_empty() {
                break;
            }
            for digest in digests {
                let data = content[&digest];
                if data.is_empty() {
                    fs.transfer(TransferRequest::Write {
                        operation_id: id.clone(),
                        digest,
                        offset: 0,
                        data: ByteChunk::from(vec![]),
                    })
                    .await?;
                } else {
                    for (index, chunk) in data.chunks(MAX_CONTENT_CHUNK).enumerate() {
                        fs.transfer(TransferRequest::Write {
                            operation_id: id.clone(),
                            digest: digest.clone(),
                            offset: (index * MAX_CONTENT_CHUNK) as u64,
                            data: ByteChunk::from(chunk),
                        })
                        .await?;
                    }
                }
            }
        }
        advance(fs, &id, TransferPhase::Ready).await?;
        fs.transfer(TransferRequest::Commit {
            operation_id: id.clone(),
        })
        .await?;
        Ok(MaterializeResponse {
            destination: params.destination,
            entries: entries.len() as u32,
            bytes,
            retired_directory: None,
        })
    }
    .await;
    if result.is_err() {
        let _ = fs
            .transfer(TransferRequest::Abort { operation_id: id })
            .await;
    }
    result
}
