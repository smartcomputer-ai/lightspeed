use std::sync::Arc;

use engine::{
    ApplyEvent, BlobRef, ContextCompactionRequest, ContextCompactionResult,
    ContextCompactionStatus, CoreAgentAction, CoreAgentCommand, CoreAgentDrive,
    CoreAgentDriveError, CoreAgentIoError, CoreAgentLlm, CoreAgentState, CoreAgentTools,
    CoreApplyEvent, EventSeq, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, SKILL_CATALOG_CONTEXT_KEY, SessionId, ToolCallStatus,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    storage::{AppendSessionEvents, BlobStore, ReadSessionEvents},
};
use tools::{
    prompts::{
        PromptAssemblyLimits, conventional_vfs_prompt_root_specs,
        prepare_prompt_instructions_publication, resolve_mounted_vfs_prompt_roots,
    },
    skills::{
        conventional_vfs_skill_root_specs, prepare_skill_catalog_publication,
        resolve_mounted_vfs_skill_roots,
    },
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

    async fn refresh_prompt_instructions_before_run(
        &self,
        drive: &mut CoreAgentDrive,
        observed_at_ms: u64,
        emitted_entries: &mut Vec<engine::CoreAgentEntry>,
    ) -> Result<(), RunnerError> {
        let Some(command) = self
            .refresh_prompt_instructions_command(drive.session_id(), drive.state())
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
                message: format!(
                    "prompt instructions refresh emitted unexpected action: {other:?}"
                ),
            }),
        }
    }

    async fn refresh_prompt_instructions_command(
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
                message: format!("load VFS mounts for prompt instructions refresh: {error}"),
            }
        })?;
        let specs = conventional_vfs_prompt_root_specs(&mounts);
        if specs.is_empty() {
            let publication = prepare_prompt_instructions_publication(
                self.stores.blobs.as_ref(),
                state,
                &[],
                PromptAssemblyLimits::default(),
            )
            .await
            .map_err(|error| RunnerError::InvalidRequest {
                message: format!("prepare prompt instructions publication: {error}"),
            })?;
            return Ok(publication.command);
        }

        let resolved = resolve_mounted_vfs_prompt_roots(
            self.stores.blobs.clone(),
            workspace_store.clone(),
            mounts,
            specs,
        )
        .await
        .map_err(|error| RunnerError::InvalidRequest {
            message: format!("resolve VFS prompt roots: {error}"),
        })?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(|error| RunnerError::InvalidRequest {
                message: format!("filter VFS prompt roots: {error}"),
            })?;
        let publication = prepare_prompt_instructions_publication(
            self.stores.blobs.as_ref(),
            state,
            &inputs,
            PromptAssemblyLimits::default(),
        )
        .await
        .map_err(|error| RunnerError::InvalidRequest {
            message: format!("prepare prompt instructions publication: {error}"),
        })?;
        Ok(publication.command)
    }

    pub async fn drive_command(&self, request: DriveCommand) -> Result<DriveOutcome, RunnerError> {
        let max_steps = resolve_max_steps(request.max_steps)?;
        engine::storage::ensure_engine_blobs(self.stores.blobs.as_ref()).await?;
        let mut drive = self.load_drive(&request.session_id).await?;
        let mut emitted_entries = Vec::new();

        if should_refresh_run_context_before_admitting(drive.state(), &request.command) {
            self.refresh_prompt_instructions_before_run(
                &mut drive,
                request.observed_at_ms,
                &mut emitted_entries,
            )
            .await?;
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
            return Ok(clear_catalog_command(
                active_skill_catalog_ref(state).as_ref(),
            ));
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
            return Ok(clear_catalog_command(
                active_skill_catalog_ref(state).as_ref(),
            ));
        }

        let publication =
            prepare_skill_catalog_publication(self.stores.blobs.as_ref(), state, None, &inputs)
                .await
                .map_err(|error| RunnerError::InvalidRequest {
                    message: format!("prepare skill catalog publication: {error}"),
                })?;
        Ok(publication.command)
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
                CoreAgentAction::CompactContext { request } => {
                    let result = match self.llm.compact_context(request.clone()).await {
                        Ok(result) => result,
                        Err(error) => {
                            failed_context_compaction_result_from_error(
                                self.stores.blobs.as_ref(),
                                request,
                                error,
                            )
                            .await?
                        }
                    };
                    action = drive.resume_context_compaction(result, observed_at_ms)?;
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

fn should_refresh_run_context_before_admitting(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    matches!(command, CoreAgentCommand::RequestRun { .. })
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
}

fn clear_catalog_command(active_catalog_ref: Option<&BlobRef>) -> Option<CoreAgentCommand> {
    active_catalog_ref.map(|_| CoreAgentCommand::RemoveContext {
        key: engine::ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY),
    })
}

fn active_skill_catalog_ref(state: &CoreAgentState) -> Option<BlobRef> {
    state
        .context
        .entries
        .iter()
        .find(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|key| key.as_str() == SKILL_CATALOG_CONTEXT_KEY)
                && matches!(entry.kind, engine::ContextEntryKind::SkillCatalog)
        })
        .map(|entry| entry.content_ref.clone())
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
        context_entries: Vec::new(),
        facts: LlmGenerationFacts {
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
            context_token_estimate: None,
        },
    })
}

async fn failed_context_compaction_result_from_error(
    blobs: &dyn BlobStore,
    request: ContextCompactionRequest,
    error: CoreAgentIoError,
) -> Result<ContextCompactionResult, engine::storage::BlobStoreError> {
    let context_revision = compaction_request_context_revision(&request);
    let failure_ref = write_error_blob(
        blobs,
        format!(
            "core agent context compaction failed\nsession_id={}\ncontext_revision={}\nerror={error}\n",
            request.session_id, context_revision
        ),
    )
    .await?;
    Ok(ContextCompactionResult {
        session_id: request.session_id,
        context_revision,
        status: ContextCompactionStatus::Failed,
        failure_ref: Some(failure_ref),
        context_entries: Vec::new(),
    })
}

fn compaction_request_context_revision(request: &ContextCompactionRequest) -> u64 {
    request.request.context.context_revision
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
        AgentHandle, CompactionPolicy, ContextCompactionRequest, ContextCompactionResult,
        ContextCompactionStatus, ContextConfig, ContextEntryInput, ContextEntryKey,
        ContextEntryKind, ContextMessageRole, CoreAgentCommand, CoreAgentEventKind,
        FunctionToolSpec, LlmFinish, ModelSelection, ObservedToolCall,
        ProviderApiKind, RunConfig, RunStatus,
        SessionConfig, SessionId, ToolCallResult, ToolExecutionTarget, ToolKind, ToolName,
        ToolParallelism, ToolSpec, ToolTargetRequirement, TurnConfig, TurnEvent,
        storage::{
            BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore,
        },
    };
    use tools::prompts::{
        PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX, PROMPT_INSTRUCTIONS_PROVIDER_KIND,
        PromptInstructionsReport,
    };
    use tools::skills::{SkillCatalogSnapshot, SkillLocation};
    use tools::{
        host::{
            HostToolContext, InlineHostToolRuntime,
            fs::{FileSystem, FsPath, MountedVfsFileSystem},
            tools::ReadFileResult,
        },
        runtime::ToolTarget,
        toolset::{ToolsetConfig, ToolsetEnvironment, resolve_toolset},
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

    struct FailCompactionLlm;

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

    #[async_trait]
    impl CoreAgentLlm for FailCompactionLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            Ok(final_output_result(&request))
        }

        async fn compact_context(
            &self,
            _request: ContextCompactionRequest,
        ) -> Result<ContextCompactionResult, CoreAgentIoError> {
            Err(CoreAgentIoError::Failed {
                message: "compact endpoint unavailable".to_owned(),
            })
        }
    }

    #[derive(Debug)]
    struct ToolThenFinalLlm {
        calls: Mutex<u32>,
    }

    struct ReadFileThenFinalLlm {
        calls: Mutex<u32>,
        blobs: Arc<dyn BlobStore>,
        path: String,
        offset: Option<usize>,
        limit: Option<usize>,
        call_id: String,
    }

    #[derive(Default)]
    struct CaptureFinalLlm {
        requests: Mutex<Vec<LlmGenerationRequest>>,
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
                    context_entries: Vec::new(),
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
                    },
                });
            }
            Ok(final_output_result(&request))
        }
    }

    #[async_trait]
    impl CoreAgentLlm for ReadFileThenFinalLlm {
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
                let arguments = serde_json::json!({
                    "path": self.path,
                    "offset": self.offset,
                    "limit": self.limit,
                });
                let arguments_ref = self
                    .blobs
                    .put_bytes(serde_json::to_vec(&arguments).map_err(|error| {
                        CoreAgentIoError::Failed {
                            message: error.to_string(),
                        }
                    })?)
                    .await
                    .map_err(|error| CoreAgentIoError::Failed {
                        message: error.to_string(),
                    })?;
                return Ok(LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-read-skill".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: vec![ObservedToolCall {
                            call_id: engine::ToolCallId::new(self.call_id.clone()),
                            tool_name: ToolName::new("read_file"),
                            provider_kind: None,
                            arguments_ref,
                            native_call_ref: None,
                        }],
                        context_token_estimate: None,
                    },
                });
            }
            Ok(final_output_result(&request))
        }
    }

    #[async_trait]
    impl CoreAgentLlm for CaptureFinalLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            self.requests
                .lock()
                .expect("request lock")
                .push(request.clone());
            Ok(final_output_result(&request))
        }
    }

    fn final_output_result(request: &LlmGenerationRequest) -> LlmGenerationResult {
        LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                content_ref: BlobRef::from_bytes(b"assistant output"),
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
            },
        }
    }

    fn user_input(content_ref: BlobRef) -> Vec<ContextEntryInput> {
        vec![ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }]
    }

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            run: run_config(),
            turn: TurnConfig {
                max_output_tokens: None,
                tool_choice: None,
                provider_params: None,
            },
            context: ContextConfig { compaction: None },
            tools: Default::default(),
        }
    }

    fn standalone_compaction_config() -> SessionConfig {
        let mut config = config();
        config.context.compaction = Some(CompactionPolicy::ProviderStandalone {
            compact_threshold_tokens: None,
            target_tokens: Some(128),
        });
        config
    }

    fn run_config() -> RunConfig {
        RunConfig {
            max_turns: None,
            max_tool_rounds: None,
            model_override: None,
            max_output_tokens: None,
            provider_params: None,
            tool_choice: None,
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

    fn tool_set() -> BTreeMap<ToolName, ToolSpec> {
        let tool_name = ToolName::new("test_tool");
        BTreeMap::from([(
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
        )])
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compact_context_provider_error_finishes_failed_compaction() {
        let (runner, session_id) = runner_with(Arc::new(FailCompactionLlm)).await;
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession {
                    config: standalone_compaction_config(),
                },
                max_steps: None,
            })
            .await
            .expect("open session");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: CoreAgentCommand::UpsertContext {
                    key: ContextEntryKey::new("client.native"),
                    entry: ContextEntryInput {
                        kind: ContextEntryKind::ProviderOpaque,
                        content_ref: BlobRef::from_bytes(br#"{"type":"input"}"#),
                        media_type: Some("application/json".to_owned()),
                        preview: None,
                        provider_kind: None,
                        provider_item_id: None,
                        token_estimate: None,
                    },
                },
                max_steps: None,
            })
            .await
            .expect("upsert context");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 30,
                command: CoreAgentCommand::CompactContext,
                max_steps: Some(64),
            })
            .await
            .expect("compact context");

        assert!(outcome.accepted);
        assert!(!outcome.state.context.pending_compaction);
        assert!(outcome.emitted_entries.iter().any(|entry| matches!(
            &entry.event.kind,
            CoreAgentEventKind::Context(engine::ContextEvent::CompactionFinished {
                status: ContextCompactionStatus::Failed,
                failure_ref: Some(_),
                ..
            })
        )));
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
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
                max_steps: Some(64),
            })
            .await
            .expect("drive request");

        let catalog_ref = active_skill_catalog_ref(&outcome.state).expect("skill catalog");
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
                CoreAgentEventKind::Context(engine::ContextEvent::EntriesApplied { entries, .. })
                    if entries.iter().any(|entry| {
                        matches!(entry.kind, ContextEntryKind::SkillCatalog)
                    })
            )
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_run_refreshes_conventional_vfs_prompt_instructions_before_planning() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores = RunnerStores::new(sessions.clone(), blobs.clone())
            .with_vfs_catalog(vfs.clone(), vfs.clone());
        let session_id = SessionId::new("session-prompts");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let initial_snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    ".forge/prompts/instructions.md",
                    b"Keep replies concise.\n".to_vec(),
                )
                .unwrap(),
                InlineFile::new(
                    ".forge/prompts/instructions.d/010-style.md",
                    b"Prefer concrete file references.\n".to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create initial prompt snapshot");
        let workspace_id = VfsWorkspaceId::new("workspace-prompts");
        vfs.create_workspace(CreateVfsWorkspaceRecord {
            workspace_id: workspace_id.clone(),
            base_snapshot_ref: Some(initial_snapshot.snapshot_ref.clone()),
            head_snapshot_ref: initial_snapshot.snapshot_ref,
            created_at_ms: 1,
        })
        .await
        .expect("create workspace");
        vfs.put_mount(VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse("/workspace").unwrap(),
            source: VfsMountSource::Workspace {
                workspace_id: workspace_id.clone(),
            },
            access: VfsMountAccess::ReadWrite,
        })
        .await
        .expect("mount workspace");
        let llm = Arc::new(CaptureFinalLlm::default());
        let runner = SessionRunner::new(stores, llm.clone());
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession { config: config() },
                max_steps: None,
            })
            .await
            .expect("open session");

        let first_outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"first input")),
                    run_config: run_config(),
                },
                max_steps: Some(64),
            })
            .await
            .expect("drive first request");

        let first_prompts = tools::prompts::active_prompt_instruction_entries(&first_outcome.state);
        assert_eq!(first_prompts.len(), 2);
        assert_prompt_entry_metadata(&first_prompts);
        assert_eq!(
            prompt_entry_texts(blobs.as_ref(), &first_prompts).await,
            vec![
                "Keep replies concise.\n".to_owned(),
                "Prefer concrete file references.\n".to_owned(),
            ]
        );
        let first_report_ref = prompt_report_ref_from_entries(&first_prompts);
        let first_report: PromptInstructionsReport = serde_json::from_slice(
            &blobs
                .read_bytes(&first_report_ref)
                .await
                .expect("read first prompt report"),
        )
        .expect("decode first prompt report");
        let mut first_report_paths = first_report
            .sources
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>();
        first_report_paths.sort_unstable();
        assert_eq!(
            first_report_paths,
            vec![
                "/workspace/.forge/prompts/instructions.d/010-style.md",
                "/workspace/.forge/prompts/instructions.md",
            ]
        );
        assert!(first_report.sources.iter().all(|source| source.published));
        assert!(first_outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Context(engine::ContextEvent::KeyPrefixReplaced {
                    key_prefix,
                    entries,
                    ..
                }) if key_prefix.as_str() == PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
                    && entries.len() == 2
            )
        }));

        let first_prompt_refs = prompt_content_refs(&first_prompts);
        {
            let requests = llm.requests.lock().expect("requests lock");
            assert_eq!(requests.len(), 1);
            assert_eq!(
                prompt_content_refs(&prompt_instruction_entries_in_request(&requests[0])),
                first_prompt_refs
            );
            assert_prompts_precede_user_message(&requests[0]);
        }

        let updated_snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    ".forge/prompts/instructions.d/020-focus.md",
                    b"Mention tradeoffs explicitly.\n".to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create updated prompt snapshot");
        let workspace = vfs
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        vfs.compare_and_set_head(CompareAndSetVfsWorkspaceHead {
            workspace_id: workspace_id.clone(),
            expected_revision: Some(workspace.revision),
            new_head_snapshot_ref: updated_snapshot.snapshot_ref,
            updated_at_ms: 30,
        })
        .await
        .expect("update workspace head");

        let second_outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 40,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"second input")),
                    run_config: run_config(),
                },
                max_steps: Some(64),
            })
            .await
            .expect("drive second request");

        let second_prompts =
            tools::prompts::active_prompt_instruction_entries(&second_outcome.state);
        assert_eq!(second_prompts.len(), 1);
        assert_prompt_entry_metadata(&second_prompts);
        assert_eq!(
            prompt_entry_texts(blobs.as_ref(), &second_prompts).await,
            vec!["Mention tradeoffs explicitly.\n".to_owned()]
        );
        assert!(second_outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Context(engine::ContextEvent::KeyPrefixReplaced {
                    key_prefix,
                    entries,
                    ..
                }) if key_prefix.as_str() == PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
                    && entries.len() == 1
            )
        }));
        let second_prompt_refs = prompt_content_refs(&second_prompts);
        {
            let requests = llm.requests.lock().expect("requests lock");
            assert_eq!(requests.len(), 2);
            assert_eq!(
                prompt_content_refs(&prompt_instruction_entries_in_request(&requests[1])),
                second_prompt_refs
            );
            assert_prompts_precede_user_message(&requests[1]);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_file_of_cataloged_skill_doc_does_not_record_activation() {
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
        let toolset = resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");
        let tool_set = toolset.tools.clone();
        let tools = InlineHostToolRuntime::new(ctx, toolset.catalog);
        let runner = SessionRunner::new(
            stores,
            Arc::new(ReadFileThenFinalLlm {
                calls: Mutex::new(0),
                blobs: blob_store,
                path: "/skills/system/deploy-review/SKILL.md".to_owned(),
                offset: None,
                limit: None,
                call_id: "call-read-skill".to_owned(),
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
                command: CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(0),
                    tools: tool_set,
                },
                max_steps: None,
            })
            .await
            .expect("replace tools");
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
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
                max_steps: Some(96),
            })
            .await
            .expect("drive request");

        assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert!(outcome.emitted_entries.iter().all(|entry| {
            !matches!(
                &entry.event.kind,
                CoreAgentEventKind::Context(engine::ContextEvent::EntriesApplied { entries, .. })
                    if entries.iter().any(|entry| {
                        matches!(entry.kind, ContextEntryKind::SkillActivation { .. })
                    })
            )
        }));
        assert!(
            outcome
                .state
                .context
                .entries
                .iter()
                .all(|entry| { !matches!(entry.kind, ContextEntryKind::SkillActivation { .. }) })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_skill_read_output_stays_pinned_after_workspace_changes() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let blob_store: Arc<dyn BlobStore> = blobs.clone();
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores = RunnerStores::new(sessions.clone(), blob_store.clone())
            .with_vfs_catalog(vfs.clone(), vfs.clone());
        let session_id = SessionId::new("session-workspace");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let original_snapshot = create_inline_snapshot(
            blob_store.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    "deploy-review/SKILL.md",
                    b"---\nname: deploy-review\ndescription: Use when reviewing deploys.\n---\n\nOriginal body\n"
                        .to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create original snapshot");
        let workspace_id = VfsWorkspaceId::new("workspace-skills");
        vfs.create_workspace(CreateVfsWorkspaceRecord {
            workspace_id: workspace_id.clone(),
            base_snapshot_ref: Some(original_snapshot.snapshot_ref.clone()),
            head_snapshot_ref: original_snapshot.snapshot_ref.clone(),
            created_at_ms: 1,
        })
        .await
        .expect("create workspace");
        vfs.put_mount(VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse("/skills/system").unwrap(),
            source: VfsMountSource::Workspace {
                workspace_id: workspace_id.clone(),
            },
            access: VfsMountAccess::ReadWrite,
        })
        .await
        .expect("mount skills workspace");

        let mounted_fs = MountedVfsFileSystem::new(
            blob_store.clone(),
            vfs.clone(),
            vfs.list_mounts(&session_id).await.expect("list mounts"),
        )
        .expect("mounted fs");
        let ctx = HostToolContext::new(Arc::new(mounted_fs), None, blob_store.clone())
            .with_cwd(FsPath::root());
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiResponses);
        let toolset = resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");
        let tool_set = toolset.tools.clone();
        let tools = InlineHostToolRuntime::new(ctx, toolset.catalog);
        let runner = SessionRunner::new(
            stores,
            Arc::new(ReadFileThenFinalLlm {
                calls: Mutex::new(0),
                blobs: blob_store.clone(),
                path: "/skills/system/deploy-review/SKILL.md".to_owned(),
                offset: None,
                limit: None,
                call_id: "call-read-skill".to_owned(),
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
                command: CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(0),
                    tools: tool_set,
                },
                max_steps: None,
            })
            .await
            .expect("replace tools");
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
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
                max_steps: Some(96),
            })
            .await
            .expect("drive request");
        let output_ref = tool_output_ref(&outcome, "call-read-skill");
        let loaded_skill = read_file_result(blobs.as_ref(), &output_ref).await;
        assert!(loaded_skill.text.contains("Original body"));

        let updated_snapshot = create_inline_snapshot(
            blob_store.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    "deploy-review/SKILL.md",
                    b"---\nname: deploy-review\ndescription: Use when reviewing deploys.\n---\n\nUpdated body\n"
                        .to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create updated snapshot");
        let workspace = vfs
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        vfs.compare_and_set_head(CompareAndSetVfsWorkspaceHead {
            workspace_id: workspace_id.clone(),
            expected_revision: Some(workspace.revision),
            new_head_snapshot_ref: updated_snapshot.snapshot_ref,
            updated_at_ms: 30,
        })
        .await
        .expect("update workspace head");

        let current_fs = MountedVfsFileSystem::new(
            blob_store.clone(),
            vfs.clone(),
            vfs.list_mounts(&session_id).await.expect("list mounts"),
        )
        .expect("current mounted fs");
        let current_skill = current_fs
            .read_file_text(&FsPath::new("/skills/system/deploy-review/SKILL.md").unwrap())
            .await
            .expect("read current skill");
        assert!(current_skill.contains("Updated body"));

        let pinned_skill = read_file_result(blobs.as_ref(), &output_ref).await;
        assert!(pinned_skill.text.contains("Original body"));
        assert!(!pinned_skill.text.contains("Updated body"));
    }

    fn tool_output_ref(outcome: &DriveOutcome, call_id: &str) -> BlobRef {
        outcome
            .emitted_entries
            .iter()
            .find_map(|entry| match &entry.event.kind {
                CoreAgentEventKind::Tool(engine::ToolEvent::CallCompleted { result, .. })
                    if result.call_id.as_str() == call_id =>
                {
                    result.output_ref.clone()
                }
                _ => None,
            })
            .expect("tool output ref")
    }

    async fn read_file_result(blobs: &dyn BlobStore, output_ref: &BlobRef) -> ReadFileResult {
        let bytes = blobs.read_bytes(output_ref).await.expect("read output");
        serde_json::from_slice(&bytes).expect("decode read_file result")
    }

    fn assert_prompt_entry_metadata(entries: &[&engine::ContextEntry]) {
        for entry in entries {
            assert!(matches!(entry.kind, ContextEntryKind::Instructions));
            assert!(entry.key.as_ref().is_some_and(|key| {
                key.as_str()
                    .starts_with(PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX)
            }));
            assert_eq!(
                entry.provider_kind.as_deref(),
                Some(PROMPT_INSTRUCTIONS_PROVIDER_KIND)
            );
            assert!(
                entry
                    .provider_item_id
                    .as_deref()
                    .is_some_and(|value| BlobRef::parse(value.to_owned()).is_ok())
            );
        }
    }

    fn prompt_content_refs(entries: &[&engine::ContextEntry]) -> Vec<BlobRef> {
        let mut refs = entries
            .iter()
            .map(|entry| entry.content_ref.clone())
            .collect::<Vec<_>>();
        refs.sort();
        refs
    }

    fn prompt_report_ref_from_entries(entries: &[&engine::ContextEntry]) -> BlobRef {
        let first = entries
            .first()
            .and_then(|entry| entry.provider_item_id.as_deref())
            .expect("prompt report ref");
        let report_ref = BlobRef::parse(first.to_owned()).expect("valid prompt report ref");
        for entry in entries {
            assert_eq!(entry.provider_item_id.as_deref(), Some(first));
        }
        report_ref
    }

    async fn prompt_entry_texts(
        blobs: &dyn BlobStore,
        entries: &[&engine::ContextEntry],
    ) -> Vec<String> {
        let mut texts = Vec::with_capacity(entries.len());
        for entry in entries {
            let bytes = blobs
                .read_bytes(&entry.content_ref)
                .await
                .expect("read prompt source");
            texts.push(String::from_utf8(bytes).expect("prompt source utf8"));
        }
        texts.sort();
        texts
    }

    fn prompt_instruction_entries_in_request(
        request: &LlmGenerationRequest,
    ) -> Vec<&engine::ContextEntry> {
        request_context_entries(request)
            .iter()
            .filter(|entry| is_prompt_instruction_entry(entry))
            .collect()
    }

    fn request_context_entries(request: &LlmGenerationRequest) -> &[engine::ContextEntry] {
        &request.request.context.entries
    }

    fn is_prompt_instruction_entry(entry: &engine::ContextEntry) -> bool {
        matches!(entry.kind, ContextEntryKind::Instructions)
            && entry.key.as_ref().is_some_and(|key| {
                key.as_str()
                    .starts_with(PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX)
            })
            && entry.provider_kind.as_deref() == Some(PROMPT_INSTRUCTIONS_PROVIDER_KIND)
    }

    fn assert_prompts_precede_user_message(request: &LlmGenerationRequest) {
        let entries = request_context_entries(request);
        let user_position = entries
            .iter()
            .position(|entry| {
                matches!(
                    entry.kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::User
                    }
                )
            })
            .expect("user message in request context");
        for (index, entry) in entries.iter().enumerate() {
            if is_prompt_instruction_entry(entry) {
                assert!(index < user_position);
            }
        }
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
                    input: user_input(BlobRef::from_bytes(b"input")),
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
                    input: user_input(BlobRef::from_bytes(b"input-2")),
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
                command: CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(0),
                    tools: tool_set(),
                },
                max_steps: None,
            })
            .await
            .expect("replace tools");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 20,
                command: CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
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
