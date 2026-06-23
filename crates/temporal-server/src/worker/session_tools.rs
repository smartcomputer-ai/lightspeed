use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    CoreAgentIoError, CoreAgentTools, ProviderApiKind, SessionId, ToolCallStatus,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    storage::{BlobStore, BlobStoreError},
};
use environment_registry::{
    EnvironmentRegistryError, SessionEnvironmentBindingRecord, SessionEnvironmentBindingStatus,
    SessionEnvironmentBindingStore,
};
use host_client::{HostClientError, HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    data::handshake::{InitializeParams, InitializedParams},
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport},
};
use messaging::OutboxStore;
use serde_json::Value;
use store_pg::PgStore;
use tools::{
    fleet::is_fleet_tool,
    fs::{FsPath, FsToolContext, MountedVfsFileSystem},
    host_protocol::RemoteHostConnection,
    limits::ToolLimits,
    messaging::{MessagingToolExecutor, is_messaging_tool},
    runtime::InlineToolRuntime,
    runtime::{ToolCatalog, ToolTarget},
    toolset::{EnvironmentToolsetConfig, ToolsetConfig, ToolsetEnvironment, resolve_toolset},
    web::fetch::WebFetchToolConfig,
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore, VfsWorkspaceStore};

use crate::{
    environment::{RuntimeEnvironment, SessionEnvironmentManager},
    fleet::{FleetChildRuntime, FleetService, FleetToolExecutor},
};

#[derive(Clone)]
pub struct SessionTools {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mount_store: Arc<dyn VfsMountStore>,
    environments: SessionEnvironmentManager,
    environment_bindings: Option<Arc<dyn SessionEnvironmentBindingStore>>,
    messaging: Option<MessagingToolExecutor>,
    fleet: Option<FleetToolExecutor>,
}

impl SessionTools {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
    ) -> Self {
        let environments = SessionEnvironmentManager::new(blobs.clone(), mount_store.clone());
        Self {
            blobs,
            workspace_store,
            mount_store,
            environments,
            environment_bindings: None,
            messaging: None,
            fleet: None,
        }
    }

    pub fn with_messaging_outbox(mut self, outbox: Arc<dyn OutboxStore>) -> Self {
        self.messaging = Some(MessagingToolExecutor::new(outbox));
        self
    }

    pub fn with_fleet_runtime(
        mut self,
        sessions: Arc<dyn engine::storage::SessionStore>,
        runtime: Arc<dyn FleetChildRuntime>,
    ) -> Self {
        let service = FleetService::new(sessions, runtime)
            .with_vfs_stores(self.workspace_store.clone(), self.mount_store.clone());
        self.fleet = Some(FleetToolExecutor::new(self.blobs.clone(), service));
        self
    }

    pub fn with_environment_bindings(
        mut self,
        bindings: Arc<dyn SessionEnvironmentBindingStore>,
    ) -> Self {
        self.environment_bindings = Some(bindings);
        self
    }

    pub fn with_environment(mut self, environment: RuntimeEnvironment) -> Self {
        self.environments.insert_environment(environment);
        self
    }

    pub fn from_pg_store(store: Arc<PgStore>) -> Self {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = store.clone();
        let mount_store: Arc<dyn VfsMountStore> = store.clone();
        let outbox: Arc<dyn OutboxStore> = store.clone();
        let environment_bindings: Arc<dyn SessionEnvironmentBindingStore> = store;
        Self::new(blobs, workspace_store, mount_store)
            .with_messaging_outbox(outbox)
            .with_environment_bindings(environment_bindings)
    }

    pub fn from_pg_store_with_fleet_runtime(
        store: Arc<PgStore>,
        runtime: Arc<dyn FleetChildRuntime>,
    ) -> Self {
        let sessions: Arc<dyn engine::storage::SessionStore> = store.clone();
        Self::from_pg_store(store).with_fleet_runtime(sessions, runtime)
    }

    async fn invoke_messaging_call(
        &self,
        session_id: &SessionId,
        run_id: engine::RunId,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let Some(executor) = &self.messaging else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "messaging tools are not configured on this runtime",
            )
            .await;
        };
        let arguments: Value = match self.blobs.read_bytes(&call.arguments_ref).await {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        format!("invalid JSON tool arguments: {error}"),
                    )
                    .await;
                }
            },
            Err(error) => {
                return failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("read tool arguments: {error}"),
                )
                .await;
            }
        };
        match executor
            .invoke(session_id, run_id, &call.tool_name, arguments)
            .await
        {
            Ok(output) => {
                let output_bytes = serde_json::to_vec(&output.output_json)
                    .map_err(|error| io_error(format!("encode tool output: {error}")))?;
                let output_ref = self
                    .blobs
                    .put_bytes(output_bytes)
                    .await
                    .map_err(map_blob_error)?;
                let visible_ref = self
                    .blobs
                    .put_bytes(output.model_visible_text.into_bytes())
                    .await
                    .map_err(map_blob_error)?;
                Ok(ToolInvocationResult {
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(output_ref),
                    model_visible_output_ref: Some(visible_ref),
                    error_ref: None,
                    effects: output.effects,
                })
            }
            Err(error) => {
                failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string()).await
            }
        }
    }

    async fn invoke_fleet_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let Some(executor) = &self.fleet else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "Fleet tools are not configured on this runtime",
            )
            .await;
        };
        executor
            .invoke(
                crate::fleet::FleetInvocationContext {
                    parent_session_id: request.session_id.clone(),
                    parent_run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    call_id: call.call_id.clone(),
                    observed_at_ms: now_unix_ms()?,
                },
                call,
            )
            .await
    }

    async fn environment_manager_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionEnvironmentManager, CoreAgentIoError> {
        let mut environments = self.environments.clone();
        let Some(bindings) = &self.environment_bindings else {
            return Ok(environments);
        };
        let bindings = bindings
            .list_bindings_for_session(session_id)
            .await
            .map_err(map_environment_registry_error)?;
        for binding in bindings {
            if binding.status != SessionEnvironmentBindingStatus::Ready {
                continue;
            }
            environments.insert_environment(self.runtime_environment_for_binding(binding).await?);
        }
        Ok(environments)
    }

    async fn runtime_environment_for_binding(
        &self,
        binding: SessionEnvironmentBindingRecord,
    ) -> Result<RuntimeEnvironment, CoreAgentIoError> {
        let mut client = connect_host_data_client(&binding.connection).await?;
        let response = client
            .initialize(&InitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "lightspeed-temporal-server".to_owned(),
                scope: binding.connection.scope.clone(),
                resume_connection_id: None,
            })
            .await
            .map_err(map_host_client_error)?;
        if response.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(io_error(format!(
                "unsupported host data protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                response.protocol_version
            )));
        }
        let cwd = response
            .default_cwd
            .as_deref()
            .or_else(|| binding.cwd.as_ref().map(|cwd| cwd.as_str()))
            .map(FsPath::new)
            .transpose()
            .map_err(|error| io_error(format!("invalid host data default cwd: {error}")))?;
        client
            .initialized(&InitializedParams {})
            .await
            .map_err(map_host_client_error)?;

        let mut connection = RemoteHostConnection::new(client, response.capabilities);
        if let Some(cwd) = cwd {
            connection = connection.with_cwd(cwd);
        }
        let (fs_context, environment_context) = connection.into_contexts(self.blobs.clone());
        crate::environment::runtime_environment_from_binding_record(&binding, environment_context)
            .map(|environment| environment.with_fs_context(fs_context))
            .map_err(io_error)
    }

    fn runtime_for_mounts(
        &self,
        mounts: Vec<VfsMountRecord>,
        environments: &SessionEnvironmentManager,
        active_env_target: Option<&engine::ToolExecutionTarget>,
    ) -> Result<InlineToolRuntime, CoreAgentIoError> {
        let catalog = workspace_catalog(environments.has_environments())?;
        let session_fs = if mounts.is_empty() {
            None
        } else {
            let fs = MountedVfsFileSystem::new(
                self.blobs.clone(),
                self.workspace_store.clone(),
                mounts.clone(),
            )
            .map_err(io_error)?;
            let cwd = mounted_vfs_cwd(fs.mounts())?;
            Some(FsToolContext::new(Arc::new(fs), self.blobs.clone()).with_cwd(cwd))
        };
        let targets = environments
            .tool_targets(session_fs, &mounts, active_env_target)
            .map_err(io_error)?;
        Ok(InlineToolRuntime::with_targets_and_blob_store(
            targets,
            self.blobs.clone(),
            ToolLimits::default(),
            catalog,
        ))
    }
}

async fn connect_host_data_client(
    connection: &HostConnectionSpec,
) -> Result<HostDataClient<host_client::WebSocketTransport>, CoreAgentIoError> {
    match &connection.transport {
        HostTransport::WebSocket => HostDataClient::connect(
            &connection.endpoint,
            WebSocketConnectOptions {
                user_agent: Some("lightspeed-temporal-server".to_owned()),
                ..WebSocketConnectOptions::default()
            },
        )
        .await
        .map_err(map_host_client_error),
        HostTransport::Http => Err(unsupported_host_data_transport("http")),
        HostTransport::Stdio => Err(unsupported_host_data_transport("stdio")),
        HostTransport::Ssh => Err(unsupported_host_data_transport("ssh")),
        HostTransport::Provider { provider_type } => Err(unsupported_host_data_transport(format!(
            "provider:{provider_type}"
        ))),
    }
}

fn unsupported_host_data_transport(transport: impl std::fmt::Display) -> CoreAgentIoError {
    io_error(format!(
        "host data transport is not supported by this worker: {transport}"
    ))
}

fn has_active_environment_fs(
    environments: &SessionEnvironmentManager,
    active_env_target: Option<&engine::ToolExecutionTarget>,
) -> bool {
    let Some(target) = active_env_target else {
        return false;
    };
    target.namespace == tools::targets::ENV_TARGET_NAMESPACE
        && environments
            .environment(&target.id)
            .is_some_and(|environment| environment.fs_context().is_some())
}

fn workspace_catalog(include_process_tools: bool) -> Result<ToolCatalog, CoreAgentIoError> {
    let mut catalog = ToolCatalog::new();
    for api_kind in [
        ProviderApiKind::OpenAiResponses,
        ProviderApiKind::AnthropicMessages,
        ProviderApiKind::OpenAiCompletions,
    ] {
        let target = ToolTarget::api_kind(api_kind);
        let mut config = ToolsetConfig::workspace();
        if include_process_tools {
            config.builtin.process = EnvironmentToolsetConfig::basic();
        }
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
        let has_generic_runtime_call = request
            .calls
            .iter()
            .any(|call| !is_messaging_tool(&call.tool_name) && !is_fleet_tool(&call.tool_name));
        if !has_generic_runtime_call {
            // Messaging/Fleet-only batches skip generic VFS/runtime setup entirely.
            let mut results = Vec::with_capacity(request.calls.len());
            for call in &request.calls {
                if is_messaging_tool(&call.tool_name) {
                    results.push(
                        self.invoke_messaging_call(&request.session_id, request.run_id, call)
                            .await?,
                    );
                } else {
                    results.push(self.invoke_fleet_call(&request, call).await?);
                }
            }
            return Ok(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results,
            });
        }

        let mounts = self
            .mount_store
            .list_mounts(&request.session_id)
            .await
            .map_err(map_catalog_error)?;
        let active_env_target = request
            .default_targets
            .get(tools::targets::ENV_TARGET_NAMESPACE);
        let environments = self
            .environment_manager_for_session(&request.session_id)
            .await?;
        let has_session_fs =
            !mounts.is_empty() || has_active_environment_fs(&environments, active_env_target);
        let runtime = self.runtime_for_mounts(mounts, &environments, active_env_target)?;

        let mut results = Vec::with_capacity(request.calls.len());
        for call in &request.calls {
            if is_messaging_tool(&call.tool_name) {
                results.push(
                    self.invoke_messaging_call(&request.session_id, request.run_id, call)
                        .await?,
                );
            } else if is_fleet_tool(&call.tool_name) {
                results.push(self.invoke_fleet_call(&request, call).await?);
            } else if !has_session_fs
                && call
                    .execution_target
                    .as_ref()
                    .is_some_and(|target| target.namespace == tools::targets::FS_TARGET_NAMESPACE)
            {
                results.push(
                    failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        "session has no VFS mounts configured",
                    )
                    .await?,
                );
            } else {
                results.push(runtime.invoke_call(call).await?);
            }
        }
        Ok(ToolInvocationBatchResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            results,
        })
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

fn map_environment_registry_error(error: EnvironmentRegistryError) -> CoreAgentIoError {
    io_error(format!("load session environment bindings: {error}"))
}

fn map_host_client_error(error: HostClientError) -> CoreAgentIoError {
    io_error(format!("host data-plane call failed: {error}"))
}

fn map_blob_error(error: BlobStoreError) -> CoreAgentIoError {
    io_error(format!("write tool error blob: {error}"))
}

fn now_unix_ms() -> Result<u64, CoreAgentIoError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| io_error(format!("system clock is before unix epoch: {error}")))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| io_error("current timestamp does not fit in u64 milliseconds"))
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use crate::environment::RuntimeEnvironment;
    use engine::{
        BlobRef, RunId, SessionId, ToolBatchId, ToolCallId, ToolName, TurnId,
        storage::{CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
    };
    use tools::environment::{
        EnvironmentToolContext,
        process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput, ProcessRequest,
            ProcessStatus, StreamOutput, WriteProcessStdinRequest,
        },
        projection::{
            EnvironmentCapabilities, EnvironmentKind, EnvironmentRecord, EnvironmentStatus,
        },
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

    #[derive(Default)]
    struct RecordingProcessExecutor {
        requests: Mutex<Vec<ProcessRequest>>,
    }

    #[derive(Default)]
    struct FakeFleetRuntime {
        started_runs: Mutex<Vec<(SessionId, String, engine::SubmissionId)>>,
    }

    #[async_trait]
    impl FleetChildRuntime for FakeFleetRuntime {
        async fn start_session(&self, _session_id: &SessionId) -> Result<(), api::AgentApiError> {
            Ok(())
        }

        async fn start_run(
            &self,
            session_id: &SessionId,
            input: String,
            submission_id: engine::SubmissionId,
        ) -> Result<String, api::AgentApiError> {
            self.started_runs.lock().expect("fleet lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }

        async fn read_session(
            &self,
            session_id: &SessionId,
        ) -> Result<api::SessionView, api::AgentApiError> {
            Ok(fleet_test_session(session_id, api::SessionStatus::Idle))
        }

        async fn read_session_events(
            &self,
            _session_id: &SessionId,
            _after: Option<u64>,
            _limit: u32,
        ) -> Result<api::SessionEventsReadResponse, api::AgentApiError> {
            Ok(api::SessionEventsReadResponse {
                events: Vec::new(),
                next_cursor: None,
                head_cursor: None,
                complete: true,
                gap: None,
            })
        }

        async fn list_session_environments(
            &self,
            _session_id: &SessionId,
        ) -> Result<api::SessionEnvironmentListResponse, api::AgentApiError> {
            Ok(api::SessionEnvironmentListResponse {
                active_env_id: None,
                environments: Vec::new(),
            })
        }

        async fn cancel_run(
            &self,
            _session_id: &SessionId,
            run_id: &str,
        ) -> Result<api::RunView, api::AgentApiError> {
            Ok(api::RunView {
                id: run_id.to_owned(),
                status: api::RunStatus::Cancelled,
                input: Vec::new(),
                items: Vec::new(),
                tool_batches: Vec::new(),
            })
        }

        async fn close_session(
            &self,
            session_id: &SessionId,
        ) -> Result<api::SessionView, api::AgentApiError> {
            Ok(fleet_test_session(session_id, api::SessionStatus::Closed))
        }
    }

    fn fleet_test_session(session_id: &SessionId, status: api::SessionStatus) -> api::SessionView {
        api::SessionView {
            id: session_id.as_str().to_owned(),
            status,
            cwd: None,
            config_revision: 0,
            config: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            runs: Vec::new(),
            active_context: api::ContextView::default(),
            active_tools: api::ActiveToolsView::default(),
            vfs_mounts: Vec::new(),
        }
    }

    #[async_trait]
    impl ProcessExecutor for RecordingProcessExecutor {
        async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            self.requests.lock().expect("process lock").push(request);
            Ok(ProcessOutput {
                status: ProcessStatus::Succeeded,
                handle: None,
                exit_code: Some(0),
                stdout: StreamOutput {
                    bytes: b"process ok".to_vec(),
                    truncated: false,
                },
                stderr: StreamOutput::default(),
            })
        }

        async fn write_stdin(
            &self,
            _request: WriteProcessStdinRequest,
        ) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not needed".to_owned(),
            })
        }
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

    fn test_environment(
        blobs: Arc<InMemoryBlobStore>,
        process: Arc<RecordingProcessExecutor>,
    ) -> RuntimeEnvironment {
        RuntimeEnvironment::new(
            EnvironmentRecord {
                env_id: "test".to_owned(),
                kind: EnvironmentKind::AttachedHost,
                capabilities: EnvironmentCapabilities {
                    fs_read: true,
                    fs_write: true,
                    process_exec: true,
                    process_stdin: true,
                    network: false,
                    persistent: false,
                },
                exec_target: Some(tools::targets::environment_target("test")),
                cwd: Some(FsPath::new("/workspace").expect("cwd")),
                status: EnvironmentStatus::Ready,
            },
            EnvironmentToolContext::new(Some(process), blobs)
                .with_process_cwd(FsPath::new("/workspace").expect("process cwd")),
        )
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
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref,
                    execution_target: Some(tools::targets::session_fs_target()),
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
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("Read"),
                    arguments_ref,
                    execution_target: Some(tools::targets::session_fs_target()),
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
    async fn session_tools_route_file_tools_to_vfs_and_process_tools_to_environment() {
        let (blobs, tools, session_id) = session_tools_with_readme_mount().await;
        let process = Arc::new(RecordingProcessExecutor::default());
        let tools = tools.with_environment(test_environment(blobs.clone(), process.clone()));
        let read_args = blobs
            .put_bytes(br#"{"path":"README.md","offset":1,"limit":10}"#.to_vec())
            .await
            .expect("read arguments");
        let process_args = blobs
            .put_bytes(br#"{"argv":["echo","hello"]}"#.to_vec())
            .await
            .expect("process arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_read"),
                        tool_name: ToolName::new("read_file"),
                        arguments_ref: read_args,
                        execution_target: Some(tools::targets::session_fs_target()),
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_process"),
                        tool_name: ToolName::new("exec_command"),
                        arguments_ref: process_args,
                        execution_target: Some(tools::targets::environment_target("test")),
                    },
                ],
            })
            .await
            .expect("invoke");

        assert_eq!(result.results.len(), 2);
        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        if result.results[1].status != ToolCallStatus::Succeeded {
            let error = blobs
                .read_text(result.results[1].error_ref.as_ref().expect("process error"))
                .await
                .expect("process error text");
            panic!("process tool failed: {error}");
        }
        let read_output = blobs
            .read_text(result.results[0].output_ref.as_ref().expect("read output"))
            .await
            .expect("read output text");
        assert!(read_output.contains("hello"));
        let process_visible = blobs
            .read_text(
                result.results[1]
                    .model_visible_output_ref
                    .as_ref()
                    .expect("process visible"),
            )
            .await
            .expect("process visible text");
        assert!(process_visible.contains("process ok"));
        let requests = process.requests.lock().expect("process lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].argv,
            vec!["echo".to_owned(), "hello".to_owned()]
        );
        assert_eq!(requests[0].cwd, Some(FsPath::new("/workspace").unwrap()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn messaging_tools_enqueue_outbox_rows_without_mounts() {
        use messaging::{InMemoryOutboxStore, OutboundPayload, ReadPendingOutbound};

        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let outbox = Arc::new(InMemoryOutboxStore::new());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_messaging_outbox(outbox.clone());
        let send_args = blobs
            .put_bytes(br#"{"text":"hello from the agent","reply_to":"4123"}"#.to_vec())
            .await
            .expect("arguments");
        let noop_args = blobs
            .put_bytes(br#"{"reason":"nothing to add"}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_send"),
                        tool_name: ToolName::new("message_send"),
                        arguments_ref: send_args,
                        execution_target: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_noop"),
                        tool_name: ToolName::new("message_noop"),
                        arguments_ref: noop_args,
                        execution_target: None,
                    },
                ],
            })
            .await
            .expect("invoke");

        assert_eq!(result.results.len(), 2);
        assert!(
            result
                .results
                .iter()
                .all(|call| call.status == ToolCallStatus::Succeeded)
        );
        let visible = blobs
            .read_text(
                result.results[0]
                    .model_visible_output_ref
                    .as_ref()
                    .expect("visible ref"),
            )
            .await
            .expect("visible text");
        assert!(visible.contains("Enqueued"));

        let pending = outbox
            .read_pending(ReadPendingOutbound {
                after_seq: 0,
                limit: 10,
            })
            .await
            .expect("read pending");
        assert_eq!(pending.len(), 1, "noop must not enqueue");
        assert_eq!(pending[0].run_id, Some(RunId::new(9)));
        assert_eq!(
            pending[0].payload,
            OutboundPayload::Send {
                text: "hello from the agent".to_owned(),
                reply_to: Some("4123".to_owned()),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_tools_spawn_without_generic_vfs_runtime_setup() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                agent_handle: crate::fleet::default_agent_handle(),
                created_at_ms: 1,
            })
            .await
            .expect("create parent");
        let mut state = engine::CoreAgentState::new();
        state.lifecycle.config = Some(crate::worker::default_session_config(
            engine::ModelSelection {
                api_kind: engine::ProviderApiKind::OpenAiResponses,
                provider_id: "test".to_owned(),
                model: "test-model".to_owned(),
            },
        ));
        let opening_events =
            engine::core_agent_clone_opening_events(&state, 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: parent.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("open parent");

        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let fleet_runtime = Arc::new(FakeFleetRuntime::default());
        let session_store: Arc<dyn SessionStore> = sessions;
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_fleet_runtime(session_store, fleet_runtime.clone());
        let arguments_ref = blobs
            .put_bytes(br#"{"input":"do child work"}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_spawn"),
                    tool_name: ToolName::new(::tools::fleet::AGENT_SPAWN_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let output_ref = result.results[0].output_ref.as_ref().expect("output");
        let output: ::tools::fleet::AgentSpawnOutput =
            serde_json::from_slice(&blobs.read_bytes(output_ref).await.expect("read output"))
                .expect("decode output");
        assert!(output.child_session_id.starts_with("agent_"));
        assert_eq!(
            fleet_runtime.started_runs.lock().expect("fleet lock")[0].1,
            "do child work"
        );
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
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref,
                    execution_target: Some(tools::targets::session_fs_target()),
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
                default_targets: Default::default(),
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
