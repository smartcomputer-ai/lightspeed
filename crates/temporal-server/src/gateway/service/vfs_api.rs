use super::*;

impl GatewayAgentApi {
    /// A workspace edit may retain its current files. New files need the
    /// caller's own upload even when the manifest itself was uploaded raw.
    pub(super) async fn authorize_vfs_manifest(
        &self,
        manifest: &vfs::VfsSnapshotManifest,
        source_workspace_id: Option<&str>,
    ) -> Result<(), AgentApiError> {
        let mut supplied = vfs::manifest_blob_refs(manifest);
        if let Some(workspace_id) = source_workspace_id {
            // Retaining a workspace's files is reading them.
            self.authorize_method(
                METHOD_VFS_WORKSPACES_READ,
                Some(ResourceRef::Workspace(workspace_id.to_owned())),
            )
            .await?;
            let workspace = self
                .read_vfs_workspace_record(VfsWorkspaceReadParams {
                    workspace_id: workspace_id.to_owned(),
                })
                .await?;
            let source =
                vfs::read_snapshot_manifest(self.store.as_ref(), &workspace.head_snapshot_ref)
                    .await
                    .map_err(map_vfs_read_error)?;
            let admitted = vfs::manifest_blob_refs(&source);
            supplied.retain(|blob_ref| !admitted.contains(blob_ref));
        }
        self.authorize_supplied_refs(None, supplied).await
    }

    pub(super) async fn validate_workspace_attachment_targets(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<(), AgentApiError> {
        self.preparation_service()
            .validate_workspace_attachment_targets(features)
            .await
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
        let created_at_ms = now_ms()?;
        self.create_owned_resource(
            ResourceRef::Workspace(workspace_id.as_str().to_owned()),
            params.access,
            async {
                self.store
                    .create_workspace(CreateVfsWorkspaceRecord {
                        workspace_id,
                        display_name: params.display_name,
                        base_snapshot_ref: Some(snapshot_ref.clone()),
                        head_snapshot_ref: snapshot_ref,
                        head_totals,
                        created_at_ms,
                    })
                    .await
                    .map_err(map_vfs_catalog_error)
            },
            |_| true,
        )
        .await
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

    /// The workspaces the caller may see, with their access summaries.
    pub(super) async fn list_vfs_workspace_views(
        &self,
    ) -> Result<Vec<VfsWorkspaceView>, AgentApiError> {
        Ok(self
            .store
            .list_workspaces_for(&self.operational_reader()?)
            .await
            .map_err(map_vfs_catalog_error)?
            .into_iter()
            .map(|(record, access)| vfs_workspace_view(record, access))
            .collect())
    }

    /// The view of a workspace with its access summary.
    pub(super) async fn vfs_workspace_view(
        &self,
        record: VfsWorkspaceRecord,
    ) -> Result<VfsWorkspaceView, AgentApiError> {
        let access = self
            .access_summary(&ResourceRef::Workspace(
                record.workspace_id.as_str().to_owned(),
            ))
            .await?;
        Ok(vfs_workspace_view(record, access))
    }

    /// A snapshot read through a workspace must be that workspace's head or
    /// base; without one, only a snapshot the caller committed or uploaded.
    pub(super) async fn authorize_vfs_snapshot_read(
        &self,
        params: &VfsSnapshotReadParams,
    ) -> Result<(), AgentApiError> {
        let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
        let Some(workspace_id) = &params.workspace_id else {
            return self.authorize_blob_read(None, &snapshot_ref).await;
        };
        let workspace = self
            .read_vfs_workspace_record(VfsWorkspaceReadParams {
                workspace_id: workspace_id.clone(),
            })
            .await?;
        if workspace.head_snapshot_ref != snapshot_ref
            && workspace.base_snapshot_ref.as_ref() != Some(&snapshot_ref)
        {
            return Err(AgentApiError::not_found(format!(
                "snapshot {} is neither the head nor the base of workspace {workspace_id}",
                snapshot_ref.as_str()
            )));
        }
        Ok(())
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

pub(super) fn vfs_workspace_view(
    record: VfsWorkspaceRecord,
    access: access::ResourceAccessSummary,
) -> VfsWorkspaceView {
    VfsWorkspaceView {
        access,
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
