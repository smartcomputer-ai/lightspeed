//! Selection of explicitly linked attachments for session results.
pub use harness::{Attachment, AttachmentSource, FileAttachment};
use std::collections::BTreeMap;
/// File links have a budget independent of native media admission.
pub const MAX_FILE_ATTACHMENTS: usize = 128;

/// Select referenced, recorded attachments in mention order. Conflicting identities
/// are unavailable rather than choosing whichever record happened to arrive last.
pub fn linked_attachments(
    text: &str,
    available: impl IntoIterator<Item = Attachment>,
) -> Vec<Attachment> {
    let mut by_handle: BTreeMap<String, Option<Attachment>> = BTreeMap::new();
    for attachment in available {
        let handle = attachment.handle().to_owned();
        by_handle
            .entry(handle)
            .and_modify(|previous| {
                let same = match (previous.as_ref(), &attachment) {
                    (Some(Attachment::Media(a)), Attachment::Media(b)) => {
                        a.content_ref == b.content_ref
                            && a.media_type == b.media_type
                            && a.kind == b.kind
                    }
                    (Some(Attachment::File(a)), Attachment::File(b)) => {
                        a.content_ref == b.content_ref
                    }
                    _ => false,
                };
                if !same {
                    *previous = None;
                }
            })
            .or_insert(Some(attachment));
    }
    let mut referenced: Vec<_> = by_handle
        .into_iter()
        .filter_map(|(handle, attachment)| {
            let attachment = attachment?;
            let at = text
                .match_indices(&handle)
                .find(|(at, _)| {
                    text.as_bytes()
                        .get(at + handle.len())
                        .is_none_or(|b| !b.is_ascii_hexdigit())
                })?
                .0;
            Some((at, attachment))
        })
        .collect();
    referenced.sort_by_key(|(at, _)| *at);
    let mut media_count = 0;
    let mut file_count = 0;
    referenced
        .into_iter()
        .filter_map(|(_, attachment)| {
            let (count, limit) = match &attachment {
                Attachment::Media(_) => (&mut media_count, harness::media::MAX_TOOL_MEDIA_ITEMS),
                Attachment::File(_) => (&mut file_count, MAX_FILE_ATTACHMENTS),
            };
            *count += 1;
            (*count <= limit).then_some(attachment)
        })
        .collect()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceArgs {
    path: crate::fs::FsPath,
    #[serde(default)]
    snapshot_ref: Option<harness::BlobRef>,
}

/// Origin is navigation metadata, independent of the immutable content identity.
fn reference_source(
    path: &crate::fs::FsPath,
    mounts: &[vfs::ResolvedWorkspaceAttachment],
) -> Option<AttachmentSource> {
    let path = vfs::VfsPath::parse(path.as_str()).ok()?;
    let components = path.components();
    let mount = mounts
        .iter()
        .filter(|mount| components.starts_with(&mount.path.components()))
        .max_by_key(|mount| mount.path.depth())?;
    let path = components[mount.path.depth()..].join("/");
    let (kind, id) = match &mount.target {
        vfs::ResolvedWorkspaceAttachmentTarget::AvailableWorkspace { workspace } => {
            ("vfs_workspace", workspace.workspace_id.to_string())
        }
        vfs::ResolvedWorkspaceAttachmentTarget::AvailableSnapshot { snapshot_ref } => {
            ("vfs_snapshot", snapshot_ref.to_string())
        }
        vfs::ResolvedWorkspaceAttachmentTarget::Unavailable { .. } => return None,
    };
    Some(AttachmentSource {
        kind: kind.into(),
        id,
        path,
    })
}

/// Resolve exactly one immutable manifest selection. Does not read file bytes.
pub(crate) async fn invoke_reference(
    ctx: &crate::fs::FsToolContext,
    mounts: &[vfs::ResolvedWorkspaceAttachment],
    arguments: serde_json::Value,
) -> crate::error::ToolResult<crate::runtime::ToolInvocationOutput> {
    use crate::fs::tools::{invalid_request, resolve_path};
    let args: ReferenceArgs = crate::runtime::decode_args(arguments)?;
    let (entry, name, source) = if let Some(snapshot_ref) = args.snapshot_ref {
        let path = vfs::VfsPath::parse(args.path.as_str())
            .map_err(|error| invalid_request(error.to_string()))?;
        let manifest = vfs::read_snapshot_manifest(ctx.blobs.as_ref(), &snapshot_ref)
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        let node = vfs::lookup_snapshot_path(&manifest, &path)
            .map_err(|error| invalid_request(error.to_string()))?;
        let vfs::VfsNode::File(file) = node else {
            return Err(invalid_request(
                "reference requires a file, not a directory",
            ));
        };
        (
            vfs::VfsEntry::File(file.clone()),
            path.as_str().to_owned(),
            Some(AttachmentSource {
                kind: "vfs_snapshot".into(),
                id: snapshot_ref.to_string(),
                path: path.as_str().trim_start_matches('/').into(),
            }),
        )
    } else {
        let path = resolve_path(ctx, &args.path)?;
        (
            ctx.fs.export_vfs(&path).await?,
            path.to_string(),
            reference_source(&path, mounts),
        )
    };
    let vfs::VfsEntry::File(file) = entry else {
        return Err(invalid_request(
            "reference requires a file, not a directory",
        ));
    };
    let stored = ctx.blobs.stat_blob(&file.blob_ref).await?;
    if stored.byte_len != file.size_bytes {
        return Err(invalid_request(
            "manifest file size differs from stored content",
        ));
    }
    let mut descriptor = FileAttachment::new(
        file.blob_ref,
        name.rsplit('/').next().unwrap_or(&name).into(),
        file.media_type,
    );
    descriptor.source = source;
    ctx.content_resolver
        .validate_attachment(&Attachment::File(descriptor.clone()))?;
    // Admission protects the result-to-event gap against collection of an old version.
    for blob in Attachment::File(descriptor.clone()).blob_refs() {
        ctx.blobs.retain_blob(&blob).await?;
    }
    let visible = format!(
        "File attachment: {}\nReference: {}\nUse [label]({}) to link this file version, or ![description]({}) to display it inline if it is an image.",
        descriptor.name, descriptor.handle, descriptor.handle, descriptor.handle
    );
    let content = crate::content::ContentDescriptor {
        content_ref: descriptor.content_ref.clone(),
        byte_len: stored.byte_len,
        name: Some(descriptor.name.clone()),
        media_type: descriptor.media_type.clone(),
        handle: Some(descriptor.handle.clone()),
        source: descriptor.source.clone(),
    };
    let mut result = crate::runtime::encode_output(&content, visible)?;
    result.attachments.push(Attachment::File(descriptor));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        BlobRef,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use std::sync::Arc;

    fn file(bytes: &[u8], name: &str) -> FileAttachment {
        FileAttachment::new(BlobRef::from_bytes(bytes), name.into(), None)
    }

    #[test]
    fn handles_identify_content_while_names_are_descriptive() {
        let a = file(b"same", "a.txt");
        let b = file(b"same", "b.txt");
        assert_eq!(a.handle, b.handle);
        assert_ne!(a.handle, file(b"different", "a.txt").handle);
        assert!(a.is_valid());
        assert!(Attachment::File(a).context_entry().is_none());
    }

    #[test]
    fn only_linked_known_unambiguous_attachments_travel() {
        let a = file(b"a", "a.txt");
        let b = file(b"b", "b.txt");
        let items = vec![Attachment::File(a.clone()), Attachment::File(b.clone())];
        assert!(linked_attachments("finished", items.clone()).is_empty());
        assert_eq!(
            linked_attachments(&format!("[b]({})", b.handle), items.clone()),
            vec![Attachment::File(b.clone())]
        );
        assert!(linked_attachments(&format!("{}f", a.handle), items.clone()).is_empty());
        let mut conflicting = b;
        conflicting.handle = a.handle.clone();
        assert!(
            linked_attachments(&a.handle, [items[0].clone(), Attachment::File(conflicting)])
                .is_empty()
        );
    }

    struct MetadataOnlyStore {
        inner: Arc<InMemoryBlobStore>,
        file: BlobRef,
    }

    #[async_trait::async_trait]
    impl BlobStore for MetadataOnlyStore {
        async fn put_bytes(
            &self,
            bytes: Vec<u8>,
        ) -> Result<BlobRef, harness::storage::BlobStoreError> {
            self.inner.put_bytes(bytes).await
        }
        async fn read_bytes(
            &self,
            reference: &BlobRef,
        ) -> Result<Vec<u8>, harness::storage::BlobStoreError> {
            assert_ne!(
                reference, &self.file,
                "reference creation must not read file contents"
            );
            self.inner.read_bytes(reference).await
        }
        async fn has_blob(
            &self,
            reference: &BlobRef,
        ) -> Result<bool, harness::storage::BlobStoreError> {
            self.inner.has_blob(reference).await
        }
        async fn stat_blob(
            &self,
            reference: &BlobRef,
        ) -> Result<harness::storage::BlobInfo, harness::storage::BlobStoreError> {
            self.inner.stat_blob(reference).await
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reference_exports_metadata_without_reading_file_bytes() {
        let store = Arc::new(InMemoryBlobStore::new());
        let content = store.put_bytes(b"report".to_vec()).await.unwrap();
        let mut manifest = vfs::VfsSnapshotManifest::empty();
        vfs::write_manifest_file(
            store.as_ref(),
            &mut manifest,
            &vfs::VfsPath::parse("/report.md").unwrap(),
            b"report".to_vec(),
            Some("text/markdown".into()),
            false,
        )
        .await
        .unwrap();
        let snapshot = vfs::commit_snapshot_manifest(store.as_ref(), None, manifest)
            .await
            .unwrap()
            .snapshot_ref;
        let store = Arc::new(MetadataOnlyStore {
            inner: store,
            file: content.clone(),
        });
        let fs = crate::fs::VfsSnapshotFileSystem::new(store.clone(), snapshot.clone())
            .await
            .unwrap();
        let ctx = crate::fs::FsToolContext::new(Arc::new(fs), store);
        let result = invoke_reference(&ctx, &[], serde_json::json!({"path":"/report.md"}))
            .await
            .unwrap();
        assert!(result.effects.is_empty());
        assert_eq!(result.attachments.len(), 1);
        assert_eq!(result.attachments[0].content_ref(), &content);
        assert!(result.attachments[0].context_entry().is_none());
        let explicit = invoke_reference(
            &ctx,
            &[],
            serde_json::json!({"path":"/report.md", "snapshot_ref": snapshot}),
        )
        .await
        .unwrap();
        assert_eq!(
            explicit.attachments[0].handle(),
            result.attachments[0].handle()
        );
        assert!(
            invoke_reference(&ctx, &[], serde_json::json!({"path":"/"}))
                .await
                .is_err()
        );
    }
}
