use super::*;

impl GatewayAgentApi {
    pub(super) async fn validate_workspace_link_targets(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<(), AgentApiError> {
        let Some(vfs) = features.vfs.as_ref() else {
            return Ok(());
        };
        if vfs.workspace_links.is_empty() {
            return Ok(());
        }
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let resolved = vfs::resolve_workspace_links(blobs, workspace_store, &vfs.workspace_links)
            .await
            .map_err(map_vfs_catalog_error)?;
        if let Some(link) = resolved.iter().find(|link| !link.is_available()) {
            return Err(AgentApiError::invalid_request(format!(
                "workspace link target at {} is unavailable: {}",
                link.path,
                link.unavailable_reason().unwrap_or("unknown reason")
            )));
        }
        Ok(())
    }

    pub(super) async fn create_vfs_workspace_record(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let (snapshot_ref, head_totals) = match params.snapshot_ref {
            Some(snapshot_ref) => {
                let snapshot_ref = parse_blob_ref(&snapshot_ref)?;
                let manifest = vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
                    .await
                    .map_err(map_vfs_read_error)?;
                (snapshot_ref, manifest.totals)
            }
            None => {
                let result = vfs::commit_snapshot_manifest(
                    self.store.as_ref(),
                    Some(self.store.as_ref()),
                    vfs::VfsSnapshotManifest::empty(),
                )
                .await
                .map_err(map_vfs_commit_error)?;
                (result.snapshot_ref, result.manifest.totals)
            }
        };
        self.record_vfs_snapshot_if_missing(
            snapshot_ref.clone(),
            VfsSnapshotSource::new("api_snapshot").with_subject("vfs/workspaces/create"),
            None,
        )
        .await?;

        let workspace_id = match params.workspace_id {
            Some(workspace_id) => VfsWorkspaceId::try_new(workspace_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid vfs workspace id: {error}"))
            })?,
            None => self.allocate_vfs_workspace_id(),
        };
        self.store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id,
                display_name: params.display_name,
                base_snapshot_ref: Some(snapshot_ref.clone()),
                head_snapshot_ref: snapshot_ref,
                head_totals,
                created_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)
    }

    pub(super) async fn read_vfs_workspace_record(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        self.store
            .read_workspace(&workspace_id)
            .await
            .map_err(map_vfs_catalog_error)
    }

    pub(super) async fn list_vfs_workspace_records(
        &self,
    ) -> Result<Vec<VfsWorkspaceRecord>, AgentApiError> {
        self.store
            .list_workspaces()
            .await
            .map_err(map_vfs_catalog_error)
    }

    pub(super) async fn update_vfs_workspace_record(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
        let manifest = vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
            .await
            .map_err(map_vfs_read_error)?;
        self.record_vfs_snapshot_if_missing(
            snapshot_ref.clone(),
            VfsSnapshotSource::new("api_workspace_update").with_subject("vfs/workspaces/update"),
            None,
        )
        .await?;
        self.store
            .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                workspace_id,
                expected_revision: params.expected_revision,
                display_name: params.display_name,
                new_head_snapshot_ref: snapshot_ref,
                new_head_totals: manifest.totals,
                updated_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)
    }

    pub(super) async fn delete_vfs_workspace_record(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        self.store
            .delete_workspace(&workspace_id)
            .await
            .map_err(map_vfs_catalog_error)
    }

    pub(super) async fn record_vfs_snapshot(
        &self,
        snapshot_ref: BlobRef,
        source: VfsSnapshotSource,
        display_name: Option<String>,
    ) -> Result<(), AgentApiError> {
        self.store
            .record_snapshot(VfsSnapshotRecord {
                snapshot_ref,
                source,
                display_name,
                created_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)
    }

    pub(super) async fn record_vfs_snapshot_if_missing(
        &self,
        snapshot_ref: BlobRef,
        source: VfsSnapshotSource,
        display_name: Option<String>,
    ) -> Result<(), AgentApiError> {
        match self.store.read_snapshot(&snapshot_ref).await {
            Ok(_) => Ok(()),
            Err(VfsCatalogError::NotFound { .. }) => {
                self.record_vfs_snapshot(snapshot_ref, source, display_name)
                    .await
            }
            Err(error) => Err(map_vfs_catalog_error(error)),
        }
    }

    pub(super) fn allocate_vfs_workspace_id(&self) -> VfsWorkspaceId {
        VfsWorkspaceId::new(format!("workspace_{}", uuid::Uuid::new_v4().simple()))
    }
}

pub(super) async fn commit_vfs_snapshot(
    store: &dyn BlobStore,
    blob_graph: Option<&dyn engine::storage::BlobGraphStore>,
    params: VfsSnapshotCommitParams,
) -> Result<VfsSnapshotCommitResponse, AgentApiError> {
    let manifest: vfs::VfsSnapshotManifest =
        serde_json::from_value(params.manifest).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs snapshot manifest: {error}"))
        })?;
    manifest
        .validate()
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
    validate_vfs_manifest_blob_refs(store, &manifest).await?;
    let totals = manifest.totals.clone();
    let result = vfs::commit_snapshot_manifest(store, blob_graph, manifest)
        .await
        .map_err(map_vfs_commit_error)?;
    Ok(VfsSnapshotCommitResponse {
        snapshot_ref: result.snapshot_ref.as_str().to_owned(),
        files: totals.files,
        bytes: totals.bytes,
    })
}

pub(super) async fn read_vfs_snapshot(
    store: &dyn BlobStore,
    params: VfsSnapshotReadParams,
) -> Result<VfsSnapshotReadResponse, AgentApiError> {
    let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
    let manifest = vfs::read_snapshot_manifest(store, &snapshot_ref)
        .await
        .map_err(map_vfs_read_error)?;
    let manifest_value = serde_json::to_value(&manifest)
        .map_err(|error| AgentApiError::internal(format!("failed to encode manifest: {error}")))?;
    Ok(VfsSnapshotReadResponse {
        snapshot_ref: snapshot_ref.as_str().to_owned(),
        files: manifest.totals.files,
        bytes: manifest.totals.bytes,
        manifest: manifest_value,
    })
}

pub(super) fn vfs_workspace_view(record: VfsWorkspaceRecord) -> VfsWorkspaceView {
    VfsWorkspaceView {
        workspace_id: record.workspace_id.as_str().to_owned(),
        display_name: record.display_name,
        base_snapshot_ref: record
            .base_snapshot_ref
            .map(|blob_ref| blob_ref.as_str().to_owned()),
        head_snapshot_ref: record.head_snapshot_ref.as_str().to_owned(),
        files: record.head_totals.files,
        bytes: record.head_totals.bytes,
        revision: record.revision,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) async fn validate_vfs_manifest_blob_refs(
    store: &dyn BlobStore,
    manifest: &vfs::VfsSnapshotManifest,
) -> Result<(), AgentApiError> {
    let mut refs = BTreeMap::new();
    collect_vfs_manifest_blob_refs(&manifest.root, &mut refs)?;
    for (blob_ref, expected_bytes) in refs {
        let info = store
            .stat_blob(&blob_ref)
            .await
            .map_err(map_vfs_manifest_blob_error)?;
        if info.byte_len != expected_bytes {
            return Err(AgentApiError::invalid_request(format!(
                "vfs manifest file size for {blob_ref} is {expected_bytes}, but stored blob size is {}",
                info.byte_len
            )));
        }
    }
    Ok(())
}

pub(super) fn collect_vfs_manifest_blob_refs(
    directory: &vfs::VfsDirectory,
    refs: &mut BTreeMap<BlobRef, u64>,
) -> Result<(), AgentApiError> {
    for entry in directory.entries.values() {
        match entry {
            vfs::VfsEntry::File(file) => {
                if let Some(existing) = refs.insert(file.blob_ref.clone(), file.size_bytes)
                    && existing != file.size_bytes
                {
                    return Err(AgentApiError::invalid_request(format!(
                        "vfs manifest references blob {} with conflicting sizes: {existing} and {}",
                        file.blob_ref, file.size_bytes
                    )));
                }
            }
            vfs::VfsEntry::Directory(directory) => {
                collect_vfs_manifest_blob_refs(directory, refs)?;
            }
        }
    }
    Ok(())
}
