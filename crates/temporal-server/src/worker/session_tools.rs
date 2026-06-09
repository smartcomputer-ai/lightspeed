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
    },
    runtime::{ToolCatalog, ToolTarget},
    toolset::{ToolsetConfig, ToolsetEnvironment, resolve_toolset},
    web::fetch::WebFetchToolConfig,
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore, VfsWorkspaceStore};

#[derive(Clone)]
pub struct SessionTools {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mount_store: Arc<dyn VfsMountStore>,
}

impl SessionTools {
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
        let catalog = workspace_catalog()?;
        Ok(InlineHostToolRuntime::new(ctx, catalog))
    }

    fn targetless_runtime(&self) -> Result<InlineHostToolRuntime, CoreAgentIoError> {
        let catalog = workspace_catalog()?;
        Ok(InlineHostToolRuntime::with_targets_and_blob_store(
            tools::host::HostToolTargets::new(),
            self.blobs.clone(),
            tools::host::context::HostToolLimits::default(),
            catalog,
        ))
    }
}

fn workspace_catalog() -> Result<ToolCatalog, CoreAgentIoError> {
    let mut catalog = ToolCatalog::new();
    for api_kind in [
        ProviderApiKind::OpenAiResponses,
        ProviderApiKind::AnthropicMessages,
        ProviderApiKind::OpenAiCompletions,
    ] {
        let target = ToolTarget::api_kind(api_kind);
        let mut config = ToolsetConfig::workspace();
        config.web_fetch = WebFetchToolConfig::enabled();
        let toolset = resolve_toolset(ToolsetEnvironment { target: &target }, &config)
            .map_err(|error| io_error(format!("build mounted vfs tool catalog: {error}")))?;
        for binding in toolset.catalog.bindings() {
            catalog.insert(binding.clone());
        }
    }
    Ok(catalog)
}

#[async_trait]
impl CoreAgentTools for SessionTools {
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
            let runtime = self.targetless_runtime()?;
            return invoke_without_mounts(self.blobs.as_ref(), runtime, request).await;
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

async fn invoke_without_mounts(
    blobs: &dyn BlobStore,
    runtime: InlineHostToolRuntime,
    request: ToolInvocationBatchRequest,
) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
    let mut results = Vec::with_capacity(request.calls.len());
    for call in request.calls {
        if call.execution_target.is_none() {
            results.push(runtime.invoke_call(&call).await?);
        } else {
            results.push(
                failed_result(blobs, call.call_id, "session has no VFS mounts configured").await?,
            );
        }
    }
    Ok(ToolInvocationBatchResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        batch_id: request.batch_id,
        results,
    })
}

async fn failed_result(
    blobs: &dyn BlobStore,
    call_id: engine::ToolCallId,
    message: impl Into<String>,
) -> Result<ToolInvocationResult, CoreAgentIoError> {
    let error_ref = blobs
        .put_bytes(message.into().into_bytes())
        .await
        .map_err(map_blob_error)?;
    Ok(ToolInvocationResult {
        call_id,
        status: ToolCallStatus::Failed,
        output_ref: None,
        model_visible_output_ref: Some(error_ref.clone()),
        error_ref: Some(error_ref),
        effects: Vec::new(),
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

    async fn session_tools_with_readme_mount() -> (Arc<InMemoryBlobStore>, SessionTools, SessionId)
    {
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
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
        (blobs, tools, session_id)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_read_session_workspace_mount() {
        let (blobs, tools, session_id) = session_tools_with_readme_mount().await;
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
    async fn session_tools_accept_claude_style_read_tool() {
        let (blobs, tools, session_id) = session_tools_with_readme_mount().await;
        let arguments_ref = blobs
            .put_bytes(br#"{"file_path":"README.md","offset":1,"limit":10}"#.to_vec())
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
                    tool_name: ToolName::new("Read"),
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
    async fn session_tools_fail_host_tool_without_mounts() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
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

    #[tokio::test(flavor = "current_thread")]
    async fn targetless_web_fetch_runs_without_mounts() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
        let arguments_ref = blobs
            .put_bytes(br#"{"url":"http://127.0.0.1:1/","max_chars":1000}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("web_fetch"),
                    arguments_ref,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke");

        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        let error = blobs
            .read_text(result.results[0].error_ref.as_ref().expect("error ref"))
            .await
            .expect("error");
        assert!(error.contains("non-public"));
        assert!(!error.contains("no VFS mounts"));
    }
}
