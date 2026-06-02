use std::sync::Arc;

use engine::{
    ApplyEvent, BlobRef, CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentDriveError,
    CoreAgentIoError, CoreAgentLlm, CoreAgentState, CoreAgentTools, CoreApplyEvent, EventSeq,
    LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus,
    SessionId, SkillCatalogContext, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationResult,
    storage::{AppendSessionEvents, BlobStore, ReadSessionEvents},
};
use tools::skills::{
    SkillCatalogSnapshot, SkillToolResultActivationInput, conventional_vfs_skill_root_specs,
    prepare_skill_catalog_publication, resolve_mounted_vfs_skill_roots,
    skill_activation_from_tool_result,
};

use super::{
    error::RunnerError,
    protocol::{DEFAULT_MAX_STEPS, DriveCommand, DriveOutcome, DriveSession, RunnerStores},
};
use crate::RunnerQuiescence;

const DEFAULT_READ_PAGE_SIZE: usize = 256;

pub struct SessionRunner {
    stores: RunnerStores,
    llm: Arc<dyn CoreAgentLlm>,
    tools: Option<Arc<dyn CoreAgentTools>>,
    apply: CoreApplyEvent,
    read_page_size: usize,
}

impl SessionRunner {
    /// Creates a runner for an existing logical session store.
    ///
    /// The runner does not create session records. Hosts/substrates must call
    /// `SessionStore::create_session` before driving `CoreAgentCommand::OpenSession`.
    pub fn new(stores: RunnerStores, llm: Arc<dyn CoreAgentLlm>) -> Self {
        Self {
            stores,
            llm,
            tools: None,
            apply: CoreApplyEvent,
            read_page_size: DEFAULT_READ_PAGE_SIZE,
        }
    }

    pub fn with_tools(mut self, tools: Arc<dyn CoreAgentTools>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub async fn drive_command(&self, request: DriveCommand) -> Result<DriveOutcome, RunnerError> {
        let max_steps = resolve_max_steps(request.max_steps)?;
        let mut drive = self.load_drive(&request.session_id).await?;
        let mut emitted_entries = Vec::new();

        if should_refresh_skill_catalog_before_admitting(drive.state(), &request.command) {
            self.refresh_skill_catalog_before_run(
                &mut drive,
                request.observed_at_ms,
                &mut emitted_entries,
            )
            .await?;
        }

        let action = match drive.admit_command(request.command, request.observed_at_ms) {
            Ok(action) => action,
            Err(CoreAgentDriveError::Command(engine::CommandError::Rejected(rejection))) => {
                let quiescence = classify_quiescence(drive.state());
                return Ok(DriveOutcome {
                    session_id: request.session_id,
                    accepted: false,
                    rejection: Some(rejection),
                    head: drive.head().cloned(),
                    emitted_entries,
                    state: drive.state().clone(),
                    quiescence,
                });
            }
            Err(error) => return Err(error.into()),
        };

        let quiescence = self
            .fulfill_until_quiescent(
                &mut drive,
                action,
                request.observed_at_ms,
                max_steps,
                &mut emitted_entries,
            )
            .await?;

        Ok(DriveOutcome {
            session_id: request.session_id,
            accepted: true,
            rejection: None,
            head: drive.head().cloned(),
            emitted_entries,
            state: drive.state().clone(),
            quiescence,
        })
    }

    async fn refresh_skill_catalog_before_run(
        &self,
        drive: &mut CoreAgentDrive,
        observed_at_ms: u64,
        emitted_entries: &mut Vec<engine::CoreAgentEntry>,
    ) -> Result<(), RunnerError> {
        let Some(command) = self
            .refresh_skill_catalog_command(drive.session_id(), drive.state())
            .await?
        else {
            return Ok(());
        };
        let action = drive.admit_command(command, observed_at_ms)?;
        match action {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                let appended = self
                    .stores
                    .sessions
                    .append(AppendSessionEvents {
                        session_id: drive.session_id().clone(),
                        expected_head,
                        events,
                    })
                    .await?;
                let entries = drive.resume_appended(appended.entries)?;
                emitted_entries.extend(entries);
                Ok(())
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => Ok(()),
            other => Err(RunnerError::InvalidRequest {
                message: format!("skill catalog refresh emitted unexpected action: {other:?}"),
            }),
        }
    }

    async fn refresh_skill_catalog_command(
        &self,
        session_id: &SessionId,
        state: &CoreAgentState,
    ) -> Result<Option<CoreAgentCommand>, RunnerError> {
        let Some(workspace_store) = self.stores.vfs_workspace_store.as_ref() else {
            return Ok(None);
        };
        let Some(mount_store) = self.stores.vfs_mount_store.as_ref() else {
            return Ok(None);
        };

        let mounts = mount_store.list_mounts(session_id).await.map_err(|error| {
            RunnerError::InvalidRequest {
                message: format!("load VFS mounts for skill catalog refresh: {error}"),
            }
        })?;
        let specs = conventional_vfs_skill_root_specs(&mounts);
        if specs.is_empty() {
            return Ok(clear_catalog_command(state.skills.catalog.as_ref()));
        }

        let resolved = resolve_mounted_vfs_skill_roots(
            self.stores.blobs.clone(),
            workspace_store.clone(),
            mounts,
            specs,
        )
        .await
        .map_err(|error| RunnerError::InvalidRequest {
            message: format!("resolve VFS skill roots: {error}"),
        })?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(|error| RunnerError::InvalidRequest {
                message: format!("filter VFS skill roots: {error}"),
            })?;
        if inputs.is_empty() {
            return Ok(clear_catalog_command(state.skills.catalog.as_ref()));
        }

        let publication =
            prepare_skill_catalog_publication(self.stores.blobs.as_ref(), state, None, &inputs)
                .await
                .map_err(|error| RunnerError::InvalidRequest {
                    message: format!("prepare skill catalog publication: {error}"),
                })?;
        Ok(publication.command)
    }

    async fn append_skill_activation_command(
        &self,
        drive: &mut CoreAgentDrive,
        observed_at_ms: u64,
        command: CoreAgentCommand,
        emitted_entries: &mut Vec<engine::CoreAgentEntry>,
    ) -> Result<(), RunnerError> {
        let action = drive.admit_command(command, observed_at_ms)?;
        match action {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                let appended = self
                    .stores
                    .sessions
                    .append(AppendSessionEvents {
                        session_id: drive.session_id().clone(),
                        expected_head,
                        events,
                    })
                    .await?;
                let entries = drive.resume_appended(appended.entries)?;
                emitted_entries.extend(entries);
                Ok(())
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => Ok(()),
            other => Err(RunnerError::InvalidRequest {
                message: format!("skill activation refresh emitted unexpected action: {other:?}"),
            }),
        }
    }

    async fn skill_activation_command_for_active_tool_batch(
        &self,
        state: &CoreAgentState,
    ) -> Result<Option<CoreAgentCommand>, RunnerError> {
        let Some(catalog_context) = state.skills.catalog.as_ref() else {
            return Ok(None);
        };
        let Some(active_run) = state.runs.active.as_ref() else {
            return Ok(None);
        };
        let Some(batch_id) = active_run.active_tool_batch_id else {
            return Ok(None);
        };
        let Some(batch) = active_run.tool_batches.get(&batch_id) else {
            return Ok(None);
        };

        let catalog_bytes = self
            .stores
            .blobs
            .read_bytes(&catalog_context.catalog_ref)
            .await?;
        let catalog =
            serde_json::from_slice::<SkillCatalogSnapshot>(&catalog_bytes).map_err(|error| {
                RunnerError::InvalidRequest {
                    message: format!("decode active skill catalog: {error}"),
                }
            })?;

        let mut activations = state.skills.activations.clone();
        for call_state in &batch.calls {
            let Some(result) = call_state.result.as_ref() else {
                continue;
            };
            let Some(output_ref) = result.output_ref.as_ref() else {
                continue;
            };
            let output_bytes = self.stores.blobs.read_bytes(output_ref).await?;
            let output_json = serde_json::from_slice(&output_bytes).map_err(|error| {
                RunnerError::InvalidRequest {
                    message: format!("decode tool output {}: {error}", result.call_id),
                }
            })?;
            let Some(activation) =
                skill_activation_from_tool_result(SkillToolResultActivationInput {
                    catalog_ref: &catalog_context.catalog_ref,
                    catalog: &catalog,
                    current_activations: &activations,
                    call_id: &result.call_id,
                    tool_name: &call_state.call.tool_name,
                    status: result.status,
                    execution_target: call_state.execution_target.as_ref(),
                    output_json: &output_json,
                })
            else {
                continue;
            };
            activations.push(activation);
        }

        if activations == state.skills.activations {
            Ok(None)
        } else {
            Ok(Some(CoreAgentCommand::SetSkillActivations { activations }))
        }
    }

    pub async fn drive_until_quiescent(
        &self,
        request: DriveSession,
    ) -> Result<DriveOutcome, RunnerError> {
        let max_steps = resolve_max_steps(request.max_steps)?;
        let mut drive = self.load_drive(&request.session_id).await?;
        let mut emitted_entries = Vec::new();
        let action = drive.next_action(request.observed_at_ms, max_steps)?;
        let quiescence = self
            .fulfill_until_quiescent(
                &mut drive,
                action,
                request.observed_at_ms,
                max_steps,
                &mut emitted_entries,
            )
            .await?;

        Ok(DriveOutcome {
            session_id: request.session_id,
            accepted: true,
            rejection: None,
            head: drive.head().cloned(),
            emitted_entries,
            state: drive.state().clone(),
            quiescence,
        })
    }

    pub async fn load_state(&self, session_id: &SessionId) -> Result<CoreAgentState, RunnerError> {
        let mut state = CoreAgentState::new();
        let mut after: Option<EventSeq> = None;
        let codec = engine::CoreAgentCodec;
        loop {
            let page = self
                .stores
                .sessions
                .read_after(ReadSessionEvents {
                    session_id: session_id.clone(),
                    after,
                    limit: self.read_page_size,
                })
                .await?;
            for entry in page.entries.iter().map(|entry| codec.decode_entry(entry)) {
                let entry = entry?;
                self.apply.apply(&mut state, &entry)?;
            }
            if page.complete {
                return Ok(state);
            }
            after = page.next_after;
        }
    }

    async fn load_drive(&self, session_id: &SessionId) -> Result<CoreAgentDrive, RunnerError> {
        let state = self.load_state(session_id).await?;
        let head = state.reduced_to.clone();
        Ok(CoreAgentDrive::from_replayed(
            session_id.clone(),
            state,
            head,
        ))
    }

    async fn fulfill_until_quiescent(
        &self,
        drive: &mut CoreAgentDrive,
        mut action: CoreAgentAction,
        observed_at_ms: u64,
        max_steps: usize,
        emitted_entries: &mut Vec<engine::CoreAgentEntry>,
    ) -> Result<RunnerQuiescence, RunnerError> {
        loop {
            match action {
                CoreAgentAction::AppendEvents {
                    expected_head,
                    events,
                } => {
                    let pending_skill_activation_command =
                        if active_tool_batch_has_results(drive.state()) {
                            self.skill_activation_command_for_active_tool_batch(drive.state())
                                .await?
                        } else {
                            None
                        };
                    let appended = self
                        .stores
                        .sessions
                        .append(AppendSessionEvents {
                            session_id: drive.session_id().clone(),
                            expected_head,
                            events,
                        })
                        .await?;
                    let entries = drive.resume_appended(appended.entries)?;
                    emitted_entries.extend(entries);
                    if let Some(command) = pending_skill_activation_command {
                        self.append_skill_activation_command(
                            drive,
                            observed_at_ms,
                            command,
                            emitted_entries,
                        )
                        .await?;
                    }
                    action = drive.next_action(observed_at_ms, max_steps)?;
                }
                CoreAgentAction::GenerateLlm { request } => {
                    let result = match self.llm.generate(request.clone()).await {
                        Ok(result) => result,
                        Err(error) => {
                            failed_generation_result_from_error(
                                self.stores.blobs.as_ref(),
                                request,
                                error,
                            )
                            .await?
                        }
                    };
                    action = drive.resume_generation(result, observed_at_ms)?;
                }
                CoreAgentAction::InvokeTools { request } => {
                    let result = match self.tools.as_deref() {
                        Some(tools) => match tools.invoke_batch(request.clone()).await {
                            Ok(result) => result,
                            Err(error) => {
                                failed_tool_batch_result(
                                    self.stores.blobs.as_ref(),
                                    &request,
                                    error.to_string(),
                                )
                                .await?
                            }
                        },
                        None => {
                            failed_tool_batch_result(
                                self.stores.blobs.as_ref(),
                                &request,
                                "test-support tool runtime unavailable",
                            )
                            .await?
                        }
                    };
                    action = drive.resume_tool_batch(result, observed_at_ms)?;
                }
                CoreAgentAction::Idle => return Ok(RunnerQuiescence::Idle),
                CoreAgentAction::Closed => return Ok(RunnerQuiescence::Closed),
                CoreAgentAction::StepLimitReached => {
                    return Ok(RunnerQuiescence::IterationLimitReached);
                }
            }
        }
    }
}

fn should_refresh_skill_catalog_before_admitting(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    matches!(command, CoreAgentCommand::RequestRun { .. })
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
}

fn clear_catalog_command(active_catalog: Option<&SkillCatalogContext>) -> Option<CoreAgentCommand> {
    active_catalog.map(|_| CoreAgentCommand::SetSkillCatalog { catalog: None })
}

fn active_tool_batch_has_results(state: &CoreAgentState) -> bool {
    let Some(active_run) = state.runs.active.as_ref() else {
        return false;
    };
    let Some(batch_id) = active_run.active_tool_batch_id else {
        return false;
    };
    active_run
        .tool_batches
        .get(&batch_id)
        .is_some_and(|batch| batch.calls.iter().any(|call| call.result.is_some()))
}

fn resolve_max_steps(max_steps: Option<u32>) -> Result<usize, RunnerError> {
    let max_steps = max_steps.unwrap_or(DEFAULT_MAX_STEPS);
    if max_steps == 0 {
        return Err(RunnerError::InvalidRequest {
            message: "max_steps must be greater than zero".to_owned(),
        });
    }
    Ok(max_steps as usize)
}

fn classify_quiescence(state: &CoreAgentState) -> RunnerQuiescence {
    match engine::classify_core_agent_action(state) {
        CoreAgentAction::Closed => RunnerQuiescence::Closed,
        _ => RunnerQuiescence::Idle,
    }
}

async fn failed_generation_result_from_error(
    blobs: &dyn BlobStore,
    request: LlmGenerationRequest,
    error: CoreAgentIoError,
) -> Result<LlmGenerationResult, engine::storage::BlobStoreError> {
    let failure_ref = write_error_blob(
        blobs,
        format!(
            "core agent LLM generation failed\nrun_id={}\nturn_id={}\nerror={error}\n",
            request.run_id, request.turn_id
        ),
    )
    .await?;
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status: LlmGenerationStatus::Failed,
        failure_ref: Some(failure_ref),
        context_items: Vec::new(),
        facts: LlmGenerationFacts {
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
            context_token_estimate: None,
            compaction: None,
        },
    })
}

async fn failed_tool_batch_result(
    blobs: &dyn BlobStore,
    request: &ToolInvocationBatchRequest,
    error: impl AsRef<str>,
) -> Result<ToolInvocationBatchResult, engine::storage::BlobStoreError> {
    let mut results = Vec::with_capacity(request.calls.len());
    for call in &request.calls {
        let error_ref = write_error_blob(
            blobs,
            format!(
                "{}\nrun_id={}\nturn_id={}\nbatch_id={}\ncall_id={}\ntool_name={}\n",
                error.as_ref(),
                request.run_id,
                request.turn_id,
                request.batch_id,
                call.call_id,
                call.tool_name
            ),
        )
        .await?;
        results.push(ToolInvocationResult {
            call_id: call.call_id.clone(),
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

async fn write_error_blob(
    blobs: &dyn BlobStore,
    message: impl Into<String>,
) -> Result<BlobRef, engine::storage::BlobStoreError> {
    blobs.put_bytes(message.into().into_bytes()).await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use engine::{
        AgentHandle, ContextConfig, ContextItemKind, ContextItemSource, ContextMessageRole,
        CoreAgentCommand, CoreAgentEventKind, FunctionToolSpec, LlmFinish, ModelProviderOptions,
        ModelSelection, ObservedToolCall, ProviderApiKind, ProviderRequestDefaults, RunConfig,
        RunStatus, SessionConfig, SessionId, ToolCallResult, ToolExecutionTarget, ToolKind,
        ToolName, ToolParallelism, ToolProfile, ToolProfileId, ToolRegistry, ToolSpec,
        ToolTargetRequirement, TurnConfig, TurnEvent, UncommittedContextItem,
        storage::{
            BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore,
        },
    };
    use tools::skills::{SkillCatalogSnapshot, SkillLocation};
    use tools::{
        host::{
            HostToolContext, InlineHostToolRuntime,
            fs::{FsPath, MountedVfsFileSystem},
            profiles::{HostToolPreset, resolve_host_profile},
        },
        runtime::ToolTarget,
    };
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, VfsCatalogError, VfsMountAccess, VfsMountRecord, VfsMountSource, VfsMountStore,
        VfsPath, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore, create_inline_snapshot,
    };

    use super::*;

    #[derive(Debug)]
    struct FailOnceLlm {
        calls: Mutex<u32>,
    }

    #[async_trait]
    impl CoreAgentLlm for FailOnceLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            let call = {
                let mut calls = self.calls.lock().expect("calls lock");
                *calls += 1;
                *calls
            };
            if call == 1 {
                return Err(CoreAgentIoError::Failed {
                    message: "temporary provider failure".to_owned(),
                });
            }
            Ok(final_output_result(&request))
        }
    }

    #[derive(Debug)]
    struct ToolThenFinalLlm {
        calls: Mutex<u32>,
    }

    struct ReadSkillThenFinalLlm {
        calls: Mutex<u32>,
        blobs: Arc<dyn BlobStore>,
    }

    #[derive(Default)]
    struct TestVfsCatalog {
        mounts: Mutex<BTreeMap<SessionId, Vec<VfsMountRecord>>>,
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
    }

    #[async_trait]
    impl VfsMountStore for TestVfsCatalog {
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
            session_id: &SessionId,
            mount_path: &VfsPath,
        ) -> Result<(), VfsCatalogError> {
            let mut mounts = self.mounts.lock().expect("mount lock");
            let Some(session_mounts) = mounts.get_mut(session_id) else {
                return Ok(());
            };
            session_mounts.retain(|mount| &mount.mount_path != mount_path);
            Ok(())
        }
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestVfsCatalog {
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
            if request
                .expected_revision
                .is_some_and(|revision| revision != workspace.revision)
            {
                return Err(VfsCatalogError::RevisionConflict {
                    workspace_id: request.workspace_id,
                    expected_revision: request.expected_revision.unwrap_or_default(),
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
    impl CoreAgentLlm for ToolThenFinalLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            let call = {
                let mut calls = self.calls.lock().expect("calls lock");
                *calls += 1;
                *calls
            };
            if call == 1 {
                return Ok(LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_items: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-tool".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: vec![ObservedToolCall {
                            call_id: engine::ToolCallId::new("call-1"),
                            tool_name: ToolName::new("test_tool"),
                            provider_kind: None,
                            arguments_ref: BlobRef::from_bytes(br#"{}"#),
                            native_call_ref: None,
                        }],
                        context_token_estimate: None,
                        compaction: None,
                    },
                });
            }
            Ok(final_output_result(&request))
        }
    }

    #[async_trait]
    impl CoreAgentLlm for ReadSkillThenFinalLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            let call = {
                let mut calls = self.calls.lock().expect("calls lock");
                *calls += 1;
                *calls
            };
            if call == 1 {
                let arguments_ref = self
                    .blobs
                    .put_bytes(
                        br#"{"path":"/skills/system/deploy-review/SKILL.md","offset":null,"limit":null}"#
                            .to_vec(),
                    )
                    .await
                    .map_err(|error| CoreAgentIoError::Failed {
                        message: error.to_string(),
                    })?;
                return Ok(LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_items: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-read-skill".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: vec![ObservedToolCall {
                            call_id: engine::ToolCallId::new("call-read-skill"),
                            tool_name: ToolName::new("read_file"),
                            provider_kind: None,
                            arguments_ref,
                            native_call_ref: None,
                        }],
                        context_token_estimate: None,
                        compaction: None,
                    },
                });
            }
            Ok(final_output_result(&request))
        }
    }

    fn final_output_result(request: &LlmGenerationRequest) -> LlmGenerationResult {
        LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_items: vec![UncommittedContextItem {
                kind: ContextItemKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                source: ContextItemSource::AssistantOutput {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                },
                native_item_ref: BlobRef::from_bytes(b"assistant output"),
                media_type: None,
                preview: None,
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some("resp-1".to_owned()),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                context_token_estimate: None,
                compaction: None,
            },
        }
    }

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
                options: ModelProviderOptions::None,
            },
            run: run_config(),
            turn: TurnConfig {
                max_output_tokens: None,
                provider_request_defaults: ProviderRequestDefaults::None,
            },
            context: ContextConfig {
                instructions_ref: None,
                max_context_tokens: None,
                target_context_tokens: None,
                reserve_output_tokens: None,
                compaction_enabled: false,
            },
        }
    }

    fn run_config() -> RunConfig {
        RunConfig {
            max_turns: None,
            max_tool_rounds: None,
            model_override: None,
            max_output_tokens: None,
            provider_request_defaults: None,
        }
    }

    async fn runner_with(llm: Arc<dyn CoreAgentLlm>) -> (SessionRunner, engine::SessionId) {
        let sessions = Arc::new(InMemorySessionStore::new());
        let stores = RunnerStores::new(sessions.clone(), Arc::new(InMemoryBlobStore::new()));
        let session_id = engine::SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        (SessionRunner::new(stores, llm), session_id)
    }

    fn tool_registry() -> ToolRegistry {
        let tool_name = ToolName::new("test_tool");
        let profile_id = ToolProfileId::new("test_profile");
        ToolRegistry {
            tools: BTreeMap::from([(
                tool_name.clone(),
                ToolSpec {
                    name: tool_name.clone(),
                    kind: ToolKind::Function(FunctionToolSpec {
                        model_name: None,
                        description_ref: None,
                        input_schema_ref: BlobRef::from_bytes(br#"{}"#),
                        output_schema_ref: None,
                        strict: None,
                        provider_options_ref: None,
                    }),
                    parallelism: ToolParallelism::ParallelSafe,
                    target_requirement: ToolTargetRequirement::None,
                },
            )]),
            profiles: BTreeMap::from([(
                profile_id.clone(),
                ToolProfile {
                    profile_id,
                    visible_tools: vec![tool_name],
                    tool_choice: None,
                },
            )]),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_run_refreshes_conventional_vfs_skill_catalog_before_planning() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores = RunnerStores::new(sessions.clone(), blobs.clone())
            .with_vfs_catalog(vfs.clone(), vfs.clone());
        let session_id = SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    "deploy-review/SKILL.md",
                    b"---\nname: deploy-review\ndescription: Use when reviewing deploys.\n---\n\nBody\n"
                        .to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create snapshot");
        vfs.put_mount(VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse("/skills/system").unwrap(),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot.snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        })
        .await
        .expect("mount skills");
        let runner = SessionRunner::new(
            stores,
            Arc::new(ToolThenFinalLlm {
                calls: Mutex::new(0),
            }),
        );
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession { config: config() },
                max_steps: None,
            })
            .await
            .expect("open session");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input_ref: BlobRef::from_bytes(b"input"),
                    run_config: run_config(),
                },
                max_steps: Some(64),
            })
            .await
            .expect("drive request");

        let catalog_ref = outcome
            .state
            .skills
            .catalog
            .as_ref()
            .expect("skill catalog")
            .catalog_ref
            .clone();
        let catalog: SkillCatalogSnapshot =
            serde_json::from_slice(&blobs.read_bytes(&catalog_ref).await.expect("read catalog"))
                .expect("decode catalog");

        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "deploy-review");
        assert!(matches!(
            &catalog.skills[0].location,
            SkillLocation::MountedSnapshot {
                source_snapshot_ref,
                source_mount_path,
                skill_doc_path,
                ..
            } if source_snapshot_ref == &snapshot.snapshot_ref
                && source_mount_path.as_str() == "/skills/system"
                && skill_doc_path.as_str() == "/skills/system/deploy-review/SKILL.md"
        ));
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Skill(engine::SkillEvent::CatalogSet { catalog: Some(_) })
            )
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_file_of_cataloged_skill_doc_records_tool_result_activation() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let blob_store: Arc<dyn BlobStore> = blobs.clone();
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores = RunnerStores::new(sessions.clone(), blob_store.clone())
            .with_vfs_catalog(vfs.clone(), vfs.clone());
        let session_id = SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let snapshot = create_inline_snapshot(
            blob_store.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    "deploy-review/SKILL.md",
                    b"---\nname: deploy-review\ndescription: Use when reviewing deploys.\n---\n\nBody\n"
                        .to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create snapshot");
        vfs.put_mount(VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse("/skills/system").unwrap(),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot.snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        })
        .await
        .expect("mount skills");

        let mounted_fs = MountedVfsFileSystem::new(
            blob_store.clone(),
            vfs.clone(),
            vfs.list_mounts(&session_id).await.expect("list mounts"),
        )
        .expect("mounted fs");
        let ctx = HostToolContext::new(Arc::new(mounted_fs), None, blob_store.clone())
            .with_cwd(FsPath::root());
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiResponses);
        let profile =
            resolve_host_profile(&ctx, &target, HostToolPreset::DirectFs).expect("host profile");
        let registry = profile.registry.clone();
        let profile_id = profile.profile_id.clone();
        let tools = InlineHostToolRuntime::new(ctx, profile.catalog);
        let runner = SessionRunner::new(
            stores,
            Arc::new(ReadSkillThenFinalLlm {
                calls: Mutex::new(0),
                blobs: blob_store,
            }),
        )
        .with_tools(Arc::new(tools));

        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession { config: config() },
                max_steps: None,
            })
            .await
            .expect("open session");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 11,
                command: CoreAgentCommand::SetToolRegistry { registry },
                max_steps: None,
            })
            .await
            .expect("set registry");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 12,
                command: CoreAgentCommand::SelectToolProfile { profile_id },
                max_steps: None,
            })
            .await
            .expect("select profile");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 13,
                command: CoreAgentCommand::SetDefaultToolTarget {
                    target: ToolExecutionTarget::new("host", "local"),
                },
                max_steps: None,
            })
            .await
            .expect("set default target");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input_ref: BlobRef::from_bytes(b"input"),
                    run_config: run_config(),
                },
                max_steps: Some(96),
            })
            .await
            .expect("drive request");

        assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Skill(engine::SkillEvent::ActivationsSet {
                    activations
                }) if activations.iter().any(|activation| {
                    matches!(
                        &activation.source,
                        engine::SkillActivationSource::ToolResult { call_id }
                            if call_id.as_str() == "call-read-skill"
                    )
                })
            )
        }));
        assert!(outcome.state.skills.activations.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn llm_io_error_is_recorded_and_drive_can_continue() {
        let (runner, session_id) = runner_with(Arc::new(FailOnceLlm {
            calls: Mutex::new(0),
        }))
        .await;
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession { config: config() },
                max_steps: None,
            })
            .await
            .expect("open session");

        let failed = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input_ref: BlobRef::from_bytes(b"input"),
                    run_config: run_config(),
                },
                max_steps: Some(32),
            })
            .await
            .expect("drive request");

        assert_eq!(failed.quiescence, RunnerQuiescence::Idle);
        assert_eq!(failed.state.runs.completed[0].status, RunStatus::Failed);
        assert!(failed.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Turn(TurnEvent::Completed {
                    outcome: engine::TurnOutcome::Failed {
                        failure_ref: Some(_)
                    },
                    ..
                })
            )
        }));

        let completed = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 30,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input_ref: BlobRef::from_bytes(b"input-2"),
                    run_config: run_config(),
                },
                max_steps: Some(32),
            })
            .await
            .expect("drive follow-up request");

        assert_eq!(completed.quiescence, RunnerQuiescence::Idle);
        assert_eq!(
            completed.state.runs.completed[1].status,
            RunStatus::Completed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_tool_runtime_is_recorded_and_drive_can_continue() {
        let (runner, session_id) = runner_with(Arc::new(ToolThenFinalLlm {
            calls: Mutex::new(0),
        }))
        .await;
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession { config: config() },
                max_steps: None,
            })
            .await
            .expect("open session");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 11,
                command: CoreAgentCommand::SetToolRegistry {
                    registry: tool_registry(),
                },
                max_steps: None,
            })
            .await
            .expect("set registry");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 12,
                command: CoreAgentCommand::SelectToolProfile {
                    profile_id: ToolProfileId::new("test_profile"),
                },
                max_steps: None,
            })
            .await
            .expect("select profile");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input_ref: BlobRef::from_bytes(b"input"),
                    run_config: run_config(),
                },
                max_steps: Some(64),
            })
            .await
            .expect("drive request");

        assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Tool(engine::ToolEvent::CallCompleted {
                    result: ToolCallResult {
                        status: ToolCallStatus::Failed,
                        error_ref: Some(_),
                        ..
                    },
                    ..
                })
            )
        }));
    }
}
