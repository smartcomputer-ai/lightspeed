use super::*;

impl GatewayAgentApi {
    pub(super) async fn project_vfs_mounts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<VfsMountView>, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let mut views = Vec::with_capacity(mounts.len());
        for mount in mounts {
            views.push(self.vfs_mount_view(mount).await?);
        }
        Ok(views)
    }

    pub(super) async fn vfs_mount_view(
        &self,
        mount: VfsMountRecord,
    ) -> Result<VfsMountView, AgentApiError> {
        Ok(VfsMountView {
            mount_path: mount.mount_path.as_str().to_owned(),
            source: match mount.source {
                VfsMountSource::Snapshot { snapshot_ref } => VfsMountSourceView::Snapshot {
                    snapshot_ref: snapshot_ref.as_str().to_owned(),
                },
                VfsMountSource::Workspace { workspace_id } => {
                    let workspace = self
                        .store
                        .read_workspace(&workspace_id)
                        .await
                        .map_err(map_vfs_catalog_error)?;
                    VfsMountSourceView::Workspace {
                        workspace_id: workspace.workspace_id.as_str().to_owned(),
                        head_snapshot_ref: Some(workspace.head_snapshot_ref.as_str().to_owned()),
                        revision: Some(workspace.revision),
                    }
                }
            },
            access: api_vfs_mount_access(mount.access),
        })
    }

    pub(super) async fn create_vfs_workspace_record(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
        let _manifest = vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
            .await
            .map_err(map_vfs_read_error)?;
        self.record_vfs_snapshot_if_missing(
            snapshot_ref.clone(),
            VfsSnapshotSource::new("api_snapshot").with_subject("vfs/workspace/create"),
            params.display_name,
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
                base_snapshot_ref: Some(snapshot_ref.clone()),
                head_snapshot_ref: snapshot_ref,
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

    pub(super) async fn update_vfs_workspace_record(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
        vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
            .await
            .map_err(map_vfs_read_error)?;
        self.record_vfs_snapshot_if_missing(
            snapshot_ref.clone(),
            VfsSnapshotSource::new("api_workspace_update").with_subject("vfs/workspace/update"),
            params.display_name,
        )
        .await?;
        self.store
            .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                workspace_id,
                expected_revision: params.expected_revision,
                new_head_snapshot_ref: snapshot_ref,
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

    pub(super) async fn put_vfs_mount_record(
        &self,
        params: VfsMountPutParams,
    ) -> Result<(VfsMountRecord, SessionView), AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let mount_path = VfsPath::parse(&params.mount_path).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs mount path: {error}"))
        })?;
        let access = core_vfs_mount_access(params.access);
        let source = self
            .validate_vfs_mount_source(params.source, access)
            .await?;

        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "vfs mounts can only change while no run is active or queued",
            ));
        }

        let record = VfsMountRecord {
            session_id: session_id.clone(),
            mount_path,
            source,
            access,
        };
        let mut candidate_mounts = self
            .store
            .list_mounts(&session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        candidate_mounts.retain(|mount| mount.mount_path != record.mount_path);
        candidate_mounts.push(record.clone());
        self.validate_vfs_mount_table(candidate_mounts.clone())?;

        self.store
            .put_mount(record.clone())
            .await
            .map_err(map_vfs_catalog_error)?;
        let session = self
            .configure_vfs_host_tools(&session_id, &loaded, candidate_mounts)
            .await?;
        Ok((record, session))
    }

    pub(super) async fn delete_vfs_mount_record(
        &self,
        params: VfsMountDeleteParams,
    ) -> Result<(String, SessionView), AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let mount_path = VfsPath::parse(&params.mount_path).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs mount path: {error}"))
        })?;

        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "vfs mounts can only change while no run is active or queued",
            ));
        }

        let mut candidate_mounts = self
            .store
            .list_mounts(&session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let original_len = candidate_mounts.len();
        candidate_mounts.retain(|mount| mount.mount_path != mount_path);
        if candidate_mounts.len() == original_len {
            return Err(AgentApiError::not_found(format!(
                "vfs catalog mount not found: {session_id}:{mount_path}"
            )));
        }

        self.validate_vfs_mount_table(candidate_mounts.clone())?;
        self.store
            .remove_mount(&session_id, &mount_path)
            .await
            .map_err(map_vfs_catalog_error)?;
        let session = self
            .configure_vfs_host_tools(&session_id, &loaded, candidate_mounts)
            .await?;
        Ok((mount_path.as_str().to_owned(), session))
    }

    pub(super) async fn validate_vfs_mount_source(
        &self,
        source: VfsMountSourceInput,
        access: VfsMountAccess,
    ) -> Result<VfsMountSource, AgentApiError> {
        match source {
            VfsMountSourceInput::Snapshot { snapshot_ref } => {
                if access.is_writable() {
                    return Err(AgentApiError::invalid_request(
                        "snapshot vfs mounts must be read-only",
                    ));
                }
                let snapshot_ref = parse_blob_ref(&snapshot_ref)?;
                vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
                    .await
                    .map_err(map_vfs_read_error)?;
                self.record_vfs_snapshot_if_missing(
                    snapshot_ref.clone(),
                    VfsSnapshotSource::new("api_mount").with_subject("vfs/mount/put"),
                    None,
                )
                .await?;
                Ok(VfsMountSource::Snapshot { snapshot_ref })
            }
            VfsMountSourceInput::Workspace { workspace_id } => {
                let workspace_id = VfsWorkspaceId::try_new(workspace_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid vfs workspace id: {error}"))
                })?;
                let workspace = self
                    .store
                    .read_workspace(&workspace_id)
                    .await
                    .map_err(map_vfs_catalog_error)?;
                vfs::read_snapshot_manifest(self.store.as_ref(), &workspace.head_snapshot_ref)
                    .await
                    .map_err(map_vfs_read_error)?;
                Ok(VfsMountSource::Workspace { workspace_id })
            }
        }
    }

    pub(super) fn validate_vfs_mount_table(
        &self,
        mounts: Vec<VfsMountRecord>,
    ) -> Result<(), AgentApiError> {
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        MountedVfsFileSystem::new(blobs, workspace_store, mounts)
            .map(|_| ())
            .map_err(map_fs_error)
    }

    pub(super) async fn configure_vfs_host_tools(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
        mounts: Vec<VfsMountRecord>,
    ) -> Result<SessionView, AgentApiError> {
        let session_config = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is missing config: {session_id}"))
        })?;
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let fs = MountedVfsFileSystem::new(blobs.clone(), workspace_store, mounts)
            .map_err(map_fs_error)?;
        let cwd = mounted_vfs_cwd(fs.mounts())?;
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone()).with_cwd(cwd);
        let target = tools::runtime::ToolTarget::from(&session_config.model);
        let toolset = resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: Some(&ctx),
            },
            &ToolsetConfig::workspace(),
        )
        .map_err(|error| AgentApiError::internal(format!("build vfs host tools: {error}")))?;
        store_tool_documents(blobs.as_ref(), &toolset.documents).await?;

        let mut registry = loaded.state.tooling.registry.clone();
        for (tool_name, spec) in toolset.registry.tools {
            registry.tools.insert(tool_name, spec);
        }
        for (profile_id, tool_profile) in toolset.registry.profiles {
            registry.profiles.insert(profile_id, tool_profile);
        }

        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(session_id, CoreAgentCommand::SetToolRegistry { registry })
            .await?;
        self.submit_core_command(
            session_id,
            CoreAgentCommand::SetDefaultToolTarget {
                target: HostToolTargets::local_execution_target(),
            },
        )
        .await?;
        self.submit_core_command(
            session_id,
            CoreAgentCommand::SelectToolProfile {
                profile_id: toolset.profile_id.clone(),
            },
        )
        .await?;
        self.wait_for_vfs_tooling(session_id, &toolset.profile_id, baseline_failures)
            .await
    }

    pub(super) async fn wait_for_vfs_tooling(
        &self,
        session_id: &SessionId,
        profile_id: &engine::ToolProfileId,
        baseline_failures: usize,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for vfs host tools to configure: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            let selected = loaded.state.tooling.selected_profile_id.as_ref() == Some(profile_id);
            let target = loaded
                .state
                .tooling
                .routing
                .default_targets
                .get(tools::host::HOST_TARGET_NAMESPACE);
            if selected && target == Some(&HostToolTargets::local_execution_target()) {
                return self.project_session_by_id(session_id).await;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

pub(super) async fn commit_vfs_snapshot(
    store: &dyn BlobStore,
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
    let result = vfs::commit_snapshot_manifest(store, manifest)
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
        base_snapshot_ref: record
            .base_snapshot_ref
            .map(|blob_ref| blob_ref.as_str().to_owned()),
        head_snapshot_ref: record.head_snapshot_ref.as_str().to_owned(),
        revision: record.revision,
    }
}

pub(super) fn api_vfs_mount_access(access: VfsMountAccess) -> ApiVfsMountAccess {
    match access {
        VfsMountAccess::ReadOnly => ApiVfsMountAccess::ReadOnly,
        VfsMountAccess::ReadWrite => ApiVfsMountAccess::ReadWrite,
    }
}

pub(super) fn core_vfs_mount_access(access: ApiVfsMountAccess) -> VfsMountAccess {
    match access {
        ApiVfsMountAccess::ReadOnly => VfsMountAccess::ReadOnly,
        ApiVfsMountAccess::ReadWrite => VfsMountAccess::ReadWrite,
    }
}

pub(super) fn mounted_vfs_cwd(mounts: &[VfsMountRecord]) -> Result<FsPath, AgentApiError> {
    let cwd = if mounts
        .iter()
        .any(|mount| mount.mount_path.as_str() == "/workspace")
    {
        "/workspace"
    } else {
        "/"
    };
    FsPath::new(cwd).map_err(|error| AgentApiError::internal(error.to_string()))
}
pub(super) async fn store_tool_documents(
    blobs: &dyn BlobStore,
    documents: &[ToolDocument],
) -> Result<(), AgentApiError> {
    for document in documents {
        let blob_ref = blobs
            .put_bytes(document.blob_bytes())
            .await
            .map_err(map_blob_store_error)?;
        if blob_ref != document.blob_ref {
            return Err(AgentApiError::internal(format!(
                "tool document blob ref mismatch: expected {}, got {}",
                document.blob_ref, blob_ref
            )));
        }
    }
    Ok(())
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
pub(super) fn now_ms() -> Result<i64, AgentApiError> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AgentApiError::internal(format!("system clock is before epoch: {error}")))?
        .as_millis();
    i64::try_from(ms)
        .map_err(|_| AgentApiError::internal("current timestamp does not fit in i64 milliseconds"))
}
