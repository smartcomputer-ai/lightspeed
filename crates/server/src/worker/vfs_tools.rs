use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    CoreAgentIoError, CoreAgentTools, ProviderApiKind, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationResult,
    storage::{BlobStore, BlobStoreError},
};
use store_pg::PgStore;
use tools::{
    host::{
        HostToolContext, InlineHostToolRuntime,
        fs::{FsPath, MountedVfsFileSystem},
        profiles::{HostToolPreset, resolve_host_profile},
    },
    runtime::ToolTarget,
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore, VfsWorkspaceStore};

#[derive(Clone)]
pub struct SessionMountedVfsTools {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mount_store: Arc<dyn VfsMountStore>,
}

impl SessionMountedVfsTools {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
    ) -> Self {
        Self {
            blobs,
            workspace_store,
            mount_store,
        }
    }

    pub fn from_pg_store(store: Arc<PgStore>) -> Self {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = store.clone();
        let mount_store: Arc<dyn VfsMountStore> = store;
        Self::new(blobs, workspace_store, mount_store)
    }

    fn runtime_for_mounts(
        &self,
        mounts: Vec<VfsMountRecord>,
    ) -> Result<InlineHostToolRuntime, CoreAgentIoError> {
        let fs =
            MountedVfsFileSystem::new(self.blobs.clone(), self.workspace_store.clone(), mounts)
                .map_err(io_error)?;
        let cwd = mounted_vfs_cwd(fs.mounts())?;
        let ctx = HostToolContext::new(Arc::new(fs), None, self.blobs.clone()).with_cwd(cwd);
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiResponses);
        let profile = resolve_host_profile(&ctx, &target, HostToolPreset::DirectFs)
            .map_err(|error| io_error(format!("build mounted vfs tool profile: {error}")))?;
        Ok(InlineHostToolRuntime::new(ctx, profile.catalog))
    }
}

#[async_trait]
impl CoreAgentTools for SessionMountedVfsTools {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
        let mounts = self
            .mount_store
            .list_mounts(&request.session_id)
            .await
            .map_err(map_catalog_error)?;
        if mounts.is_empty() {
            return failed_batch(
                self.blobs.as_ref(),
                request,
                "session has no VFS mounts configured",
            )
            .await;
        }
        let runtime = self.runtime_for_mounts(mounts)?;
        runtime.invoke_batch(request).await
    }
}

fn mounted_vfs_cwd(mounts: &[VfsMountRecord]) -> Result<FsPath, CoreAgentIoError> {
    let cwd = if mounts
        .iter()
        .any(|mount| mount.mount_path.as_str() == "/workspace")
    {
        "/workspace"
    } else {
        "/"
    };
    FsPath::new(cwd).map_err(io_error)
}

async fn failed_batch(
    blobs: &dyn BlobStore,
    request: ToolInvocationBatchRequest,
    message: impl Into<String>,
) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
    let message = message.into();
    let mut results = Vec::with_capacity(request.calls.len());
    for call in request.calls {
        let error_ref = blobs
            .put_bytes(message.clone().into_bytes())
            .await
            .map_err(map_blob_error)?;
        results.push(ToolInvocationResult {
            call_id: call.call_id,
            status: ToolCallStatus::Failed,
            output_ref: None,
            model_visible_output_ref: Some(error_ref.clone()),
            error_ref: Some(error_ref),
            effects: Vec::new(),
        });
    }
    Ok(ToolInvocationBatchResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        batch_id: request.batch_id,
        results,
    })
}

fn map_catalog_error(error: VfsCatalogError) -> CoreAgentIoError {
    io_error(format!("load VFS mounts: {error}"))
}

fn map_blob_error(error: BlobStoreError) -> CoreAgentIoError {
    io_error(format!("write tool error blob: {error}"))
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use engine::{
        BlobRef, RunId, SessionId, ToolBatchId, ToolCallId, ToolExecutionTarget, ToolName, TurnId,
        storage::InMemoryBlobStore,
    };
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, VfsMountAccess, VfsMountSource, VfsPath, VfsWorkspaceId, VfsWorkspaceRecord,
        create_inline_snapshot,
    };

    use super::*;

    #[derive(Default)]
    struct TestCatalog {
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
        mounts: Mutex<BTreeMap<SessionId, Vec<VfsMountRecord>>>,
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestCatalog {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let workspace = VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            };
            self.workspaces
                .lock()
                .expect("workspace lock")
                .insert(workspace.workspace_id.clone(), workspace.clone());
            Ok(workspace)
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }

        async fn compare_and_set_head(
            &self,
            request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            let workspace = workspaces.get_mut(&request.workspace_id).ok_or_else(|| {
                VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                }
            })?;
            if let Some(expected_revision) = request.expected_revision
                && workspace.revision != expected_revision
            {
                return Err(VfsCatalogError::RevisionConflict {
                    workspace_id: request.workspace_id,
                    expected_revision,
                    actual_revision: workspace.revision,
                });
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.revision += 1;
            workspace.updated_at_ms = request.updated_at_ms;
            Ok(workspace.clone())
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .remove(workspace_id)
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }
    }

    #[async_trait]
    impl VfsMountStore for TestCatalog {
        async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
            self.mounts
                .lock()
                .expect("mount lock")
                .entry(record.session_id.clone())
                .or_default()
                .push(record);
            Ok(())
        }

        async fn list_mounts(
            &self,
            session_id: &SessionId,
        ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
            Ok(self
                .mounts
                .lock()
                .expect("mount lock")
                .get(session_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn remove_mount(
            &self,
            _session_id: &SessionId,
            _mount_path: &VfsPath,
        ) -> Result<(), VfsCatalogError> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mounted_vfs_tools_read_session_workspace_mount() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let session_id = SessionId::new("session_1");
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new("README.md", b"hello\n".to_vec()).expect("inline file"),
            ]),
        )
        .await
        .expect("snapshot");
        let workspace_id = VfsWorkspaceId::new("workspace_1");
        catalog
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                base_snapshot_ref: Some(snapshot.snapshot_ref.clone()),
                head_snapshot_ref: snapshot.snapshot_ref,
                created_at_ms: 1,
            })
            .await
            .expect("workspace");
        catalog
            .put_mount(VfsMountRecord {
                session_id: session_id.clone(),
                mount_path: VfsPath::parse("/workspace").expect("mount path"),
                source: VfsMountSource::Workspace { workspace_id },
                access: VfsMountAccess::ReadWrite,
            })
            .await
            .expect("mount");
        let tools = SessionMountedVfsTools::new(blobs.clone(), catalog.clone(), catalog);
        let arguments_ref = blobs
            .put_bytes(br#"{"path":"README.md","offset":1,"limit":10}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref,
                    execution_target: Some(ToolExecutionTarget::new("host", "local")),
                }],
            })
            .await
            .expect("invoke");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let output = blobs
            .read_text(result.results[0].output_ref.as_ref().expect("output ref"))
            .await
            .expect("output");
        assert!(output.contains("hello"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mounted_vfs_tools_fail_clearly_without_mounts() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionMountedVfsTools::new(blobs.clone(), catalog.clone(), catalog);
        let arguments_ref = BlobRef::from_bytes(b"{}");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref,
                    execution_target: Some(ToolExecutionTarget::new("host", "local")),
                }],
            })
            .await
            .expect("invoke");

        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        let error = blobs
            .read_text(result.results[0].error_ref.as_ref().expect("error ref"))
            .await
            .expect("error");
        assert!(error.contains("no VFS mounts"));
    }
}
