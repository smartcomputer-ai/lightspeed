use std::{collections::BTreeMap, sync::Arc};

use engine::{
    BlobRef, ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentDriveError, CoreAgentIoError,
    CoreAgentLlm, CoreAgentState, CoreAgentTools, EventSeq, LlmFinish, LlmGenerationFacts,
    LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, SessionId, ToolBatchOutcome,
    ToolCallStatus, ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    storage::{AppendSessionEvents, BlobStore, ReadSessionEvents},
};
use tools::{
    catalog::{SKILL_CATALOG_CONTEXT_KEY, VFS_CATALOG_CONTEXT_KEY, clear_catalog_command},
    environment::projection::{prepare_vfs_catalog_publication, vfs_catalog_from_workspace_links},
    prompts::{
        PromptAssemblyLimits, configured_vfs_prompt_root_specs,
        prepare_prompt_instructions_publication,
        prepare_prompt_instructions_publication_with_warnings, resolve_linked_vfs_prompt_roots,
    },
    skills::{
        configured_vfs_skill_root_specs, prepare_skill_catalog_publication_with_warnings,
        resolve_linked_vfs_skill_roots,
    },
};

use super::{
    error::RunnerError,
    protocol::{DEFAULT_MAX_STEPS, DriveCommand, DriveOutcome, DriveSession, RunnerStores},
};
use crate::RunnerQuiescence;

const DEFAULT_READ_PAGE_SIZE: usize = 256;
// Mirrors the hosted product fallback so prompt-refresh tests exercise the
// same effective-context policy without making the substrate-neutral runner
// depend on the Temporal workflow crate.
const TEST_PRODUCT_DEFAULT_INSTRUCTIONS: &[u8] = b"You are Lightspeed, a concise personal assistant. Use available tools when useful, then answer plainly.";

pub struct SessionRunner {
    stores: RunnerStores,
    llm: Arc<dyn CoreAgentLlm>,
    tools: Option<Arc<dyn CoreAgentTools>>,
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
        self.admit_refresh_command(
            drive,
            observed_at_ms,
            emitted_entries,
            command,
            "prompt instructions refresh",
        )
        .await
    }

    async fn admit_refresh_command(
        &self,
        drive: &mut CoreAgentDrive,
        observed_at_ms: u64,
        emitted_entries: &mut Vec<engine::CoreAgentEntry>,
        command: CoreAgentCommand,
        label: &'static str,
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
                message: format!("{label} emitted unexpected action: {other:?}"),
            }),
        }
    }

    async fn refresh_prompt_instructions_command(
        &self,
        _session_id: &SessionId,
        state: &CoreAgentState,
    ) -> Result<Option<CoreAgentCommand>, RunnerError> {
        let prompt_config = state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .and_then(|vfs| vfs.prompts.as_ref());
        let links = if prompt_config.is_some() {
            self.resolve_workspace_links(state).await?
        } else {
            Vec::new()
        };
        let specs = match prompt_config {
            Some(config) => configured_vfs_prompt_root_specs(&links, config.roots.as_deref())
                .map_err(|error| RunnerError::InvalidRequest {
                    message: format!("configure VFS prompt roots: {error}"),
                })?,
            None => Vec::new(),
        };
        let desired_source = if specs.is_empty() {
            let publication = prepare_prompt_instructions_publication(
                self.stores.blobs.as_ref(),
                None,
                &[],
                PromptAssemblyLimits::default(),
            )
            .await
            .map_err(|error| RunnerError::InvalidRequest {
                message: format!("prepare prompt instructions publication: {error}"),
            })?;
            publication.desired
        } else {
            let workspace_store = self.stores.vfs_workspace_store.as_ref().ok_or_else(|| {
                RunnerError::InvalidRequest {
                    message: "VFS prompt sourcing requires a workspace store".to_owned(),
                }
            })?;
            let resolved = resolve_linked_vfs_prompt_roots(
                self.stores.blobs.clone(),
                workspace_store.clone(),
                links,
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
            let publication = prepare_prompt_instructions_publication_with_warnings(
                self.stores.blobs.as_ref(),
                None,
                &inputs,
                PromptAssemblyLimits::default(),
                resolved.warnings().to_vec(),
            )
            .await
            .map_err(|error| RunnerError::InvalidRequest {
                message: format!("prepare prompt instructions publication: {error}"),
            })?;
            publication.desired
        };

        let desired =
            effective_prompt_instruction_inputs(self.stores.blobs.as_ref(), state, desired_source)
                .await?;
        if active_instruction_inputs(state) == desired {
            return Ok(None);
        }
        Ok(Some(CoreAgentCommand::ReplaceContextPrefix {
            expected_revision: Some(state.context.revision),
            key_prefix: engine::ContextEntryKey::new("instructions"),
            entries: desired,
        }))
    }

    pub async fn drive_command(&self, request: DriveCommand) -> Result<DriveOutcome, RunnerError> {
        let max_steps = resolve_max_steps(request.max_steps)?;
        engine::storage::ensure_engine_blobs(self.stores.blobs.as_ref()).await?;
        let mut drive = self.load_drive(&request.session_id).await?;
        let mut emitted_entries = Vec::new();

        if should_refresh_run_context_before_admitting(drive.state(), &request.command) {
            self.refresh_environment_projection_before_run(
                &mut drive,
                request.observed_at_ms,
                &mut emitted_entries,
            )
            .await?;
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

    async fn refresh_environment_projection_before_run(
        &self,
        drive: &mut CoreAgentDrive,
        observed_at_ms: u64,
        emitted_entries: &mut Vec<engine::CoreAgentEntry>,
    ) -> Result<(), RunnerError> {
        let commands = self
            .refresh_environment_projection_commands(drive.session_id(), drive.state())
            .await?;
        for command in commands {
            self.admit_refresh_command(
                drive,
                observed_at_ms,
                emitted_entries,
                command,
                "environment projection refresh",
            )
            .await?;
        }
        Ok(())
    }

    async fn refresh_environment_projection_commands(
        &self,
        _session_id: &SessionId,
        state: &CoreAgentState,
    ) -> Result<Vec<CoreAgentCommand>, RunnerError> {
        let features = state
            .lifecycle
            .config
            .as_ref()
            .map(|config| &config.features);
        let vfs_catalog_enabled = features.is_some_and(|features| features.vfs.is_some());
        let links = if vfs_catalog_enabled {
            self.resolve_workspace_links(state).await?
        } else {
            Vec::new()
        };
        if !vfs_catalog_enabled {
            return Ok(state
                .context
                .entries
                .iter()
                .any(|entry| {
                    entry
                        .key
                        .as_ref()
                        .is_some_and(|key| key.as_str() == VFS_CATALOG_CONTEXT_KEY)
                })
                .then(|| CoreAgentCommand::RemoveContext {
                    expected_revision: None,
                    key: engine::ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY),
                })
                .into_iter()
                .collect());
        }
        let catalog = vfs_catalog_from_workspace_links(&links).map_err(|error| {
            RunnerError::InvalidRequest {
                message: format!("prepare VFS catalog: {error}"),
            }
        })?;
        let publication = prepare_vfs_catalog_publication(
            self.stores.blobs.as_ref(),
            None,
            engine::current_catalog_inputs(state)
                .get(&engine::ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY)),
            catalog,
        )
        .await
        .map_err(|error| RunnerError::InvalidRequest {
            message: format!("prepare VFS catalog publication: {error}"),
        })?;
        Ok(publication.command.into_iter().collect())
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
        self.admit_refresh_command(
            drive,
            observed_at_ms,
            emitted_entries,
            command,
            "skill catalog refresh",
        )
        .await
    }

    async fn refresh_skill_catalog_command(
        &self,
        _session_id: &SessionId,
        state: &CoreAgentState,
    ) -> Result<Option<CoreAgentCommand>, RunnerError> {
        let catalogs = engine::current_catalog_inputs(state);
        let current = catalogs.get(&engine::ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY));
        if current.is_some_and(|entry| entry.origin.as_deref() != Some("runtime.vfs.skills")) {
            return Ok(None);
        }
        let skills_config = state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .and_then(|vfs| vfs.skills.as_ref());
        let Some(skills_config) = skills_config else {
            return Ok(clear_catalog_command(current, SKILL_CATALOG_CONTEXT_KEY));
        };
        let Some(workspace_store) = self.stores.vfs_workspace_store.as_ref() else {
            return Ok(clear_catalog_command(current, SKILL_CATALOG_CONTEXT_KEY));
        };
        let links = self.resolve_workspace_links(state).await?;
        let specs = configured_vfs_skill_root_specs(&links, skills_config.roots.as_deref())
            .map_err(|error| RunnerError::InvalidRequest {
                message: format!("configure VFS skill roots: {error}"),
            })?;
        if specs.is_empty() {
            return Ok(clear_catalog_command(current, SKILL_CATALOG_CONTEXT_KEY));
        }

        let resolved = resolve_linked_vfs_skill_roots(
            self.stores.blobs.clone(),
            workspace_store.clone(),
            links,
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
        if inputs.is_empty() && resolved.warnings().is_empty() {
            return Ok(clear_catalog_command(current, SKILL_CATALOG_CONTEXT_KEY));
        }

        let publication = prepare_skill_catalog_publication_with_warnings(
            self.stores.blobs.as_ref(),
            None,
            current,
            &inputs,
            resolved.warnings().to_vec(),
        )
        .await
        .map_err(|error| RunnerError::InvalidRequest {
            message: format!("prepare skill catalog publication: {error}"),
        })?;
        Ok(publication.command)
    }

    async fn resolve_workspace_links(
        &self,
        state: &CoreAgentState,
    ) -> Result<Vec<vfs::ResolvedWorkspaceLink>, RunnerError> {
        let declarations = state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .map(|vfs| vfs.workspace_links.as_slice())
            .unwrap_or_default();
        if declarations.is_empty() {
            return Ok(Vec::new());
        }
        let workspace_store = self.stores.vfs_workspace_store.as_ref().ok_or_else(|| {
            RunnerError::InvalidRequest {
                message: "workspace links require a VFS workspace store".to_owned(),
            }
        })?;
        vfs::resolve_workspace_links(
            self.stores.blobs.clone(),
            workspace_store.clone(),
            declarations,
        )
        .await
        .map_err(|error| RunnerError::InvalidRequest {
            message: format!("resolve workspace links: {error}"),
        })
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
                engine::apply_event(&mut state, &entry)?;
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
                    let outcome = match self.tools.as_deref() {
                        Some(tools) => match tools.invoke_batch(request.clone()).await {
                            Ok(outcome) => outcome,
                            Err(error) => ToolBatchOutcome::completed(
                                failed_tool_batch_result(
                                    self.stores.blobs.as_ref(),
                                    &request,
                                    error.to_string(),
                                )
                                .await?,
                            ),
                        },
                        None => ToolBatchOutcome::completed(
                            failed_tool_batch_result(
                                self.stores.blobs.as_ref(),
                                &request,
                                "test-support tool runtime unavailable",
                            )
                            .await?,
                        ),
                    };
                    action = drive.resume_tool_batch_outcome(outcome, observed_at_ms)?;
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
    matches!(command, CoreAgentCommand::RequestRun(_))
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
}

async fn effective_prompt_instruction_inputs(
    blobs: &dyn BlobStore,
    state: &CoreAgentState,
    source_entries: BTreeMap<engine::ContextEntryKey, engine::ContextEntryInput>,
) -> Result<BTreeMap<engine::ContextEntryKey, engine::ContextEntryInput>, RunnerError> {
    let mut desired = active_instruction_inputs(state);
    desired.retain(|key, _| {
        !context_key_is_in_prefix(key, tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX)
    });
    desired.extend(source_entries);
    desired.remove(&engine::ContextEntryKey::new("instructions.000.default"));
    if desired.is_empty() {
        let content_ref = blobs
            .put_bytes(TEST_PRODUCT_DEFAULT_INSTRUCTIONS.to_vec())
            .await?;
        desired.insert(
            engine::ContextEntryKey::new("instructions.000.default"),
            engine::ContextEntryInput {
                kind: engine::ContextEntryKind::Instructions,
                content: engine::ContentRef::text(content_ref),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            },
        );
    }
    Ok(desired)
}

fn active_instruction_inputs(
    state: &CoreAgentState,
) -> BTreeMap<engine::ContextEntryKey, engine::ContextEntryInput> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, engine::ContextEntryKind::Instructions))
        .filter_map(|entry| {
            let key = entry.key.clone()?;
            if !context_key_is_in_prefix(&key, "instructions") {
                return None;
            }
            Some((
                key,
                engine::ContextEntryInput {
                    kind: entry.kind.clone(),
                    content: entry.content.clone(),
                    preview: entry.preview.clone(),
                    origin: entry.origin.clone(),
                    provenance_ref: entry.provenance_ref.clone(),
                    token_estimate: entry.token_estimate.clone(),
                },
            ))
        })
        .collect()
}

fn context_key_is_in_prefix(key: &engine::ContextEntryKey, prefix: &str) -> bool {
    key.as_str() == prefix
        || key
            .as_str()
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('.'))
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
            duration_ms: None,
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
            approval_requests: Vec::new(),
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
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Failed,
            output_ref: None,
            model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
                &call.call_id,
                ToolCallStatus::Failed,
                error_ref.clone(),
            )],
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
        CompactionPolicy, ContextCompactionRequest, ContextCompactionResult,
        ContextCompactionStatus, ContextConfig, ContextEntryInput, ContextEntryKey,
        ContextEntryKind, ContextMessageRole, CoreAgentCommand, CoreAgentEvent, FunctionToolSpec,
        LlmFinish, ModelSelection, ObservedToolCall, ProviderApiKind, RunConfig, RunStatus,
        SessionConfig, SessionId, ToolCallResult, ToolKind, ToolName, ToolParallelism, ToolSpec,
        TurnEvent, WorkspaceLink, WorkspaceLinkAccess, WorkspaceLinkTarget,
        storage::{
            BlobStore, CreateForkedSession, CreateSession, InMemoryBlobStore, InMemorySessionStore,
            SessionStore,
        },
    };
    use tools::prompts::{
        PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX, PROMPT_INSTRUCTIONS_PROVIDER_KIND,
        PromptInstructionsReport,
    };
    use tools::skills::{SkillCatalogSnapshot, SkillLocation};
    use tools::{
        fs::tools::ReadFileResult,
        fs::{FsPath, FsToolContext, LinkedVfsFileSystem},
        runtime::InlineToolRuntime,
        toolset::{ToolsetConfig, register_toolset},
    };
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, ResolvedWorkspaceLink, ResolvedWorkspaceLinkTarget, VfsCatalogError, VfsPath,
        VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore, create_inline_snapshot,
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
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestVfsCatalog {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let workspace = VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                display_name: record.display_name,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                head_totals: record.head_totals,
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

        async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
            Ok(self
                .workspaces
                .lock()
                .expect("workspace lock")
                .values()
                .cloned()
                .collect())
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
            if let Some(display_name) = request.display_name {
                workspace.display_name = Some(display_name);
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.head_totals = request.new_head_totals;
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
                        duration_ms: None,
                        provider_response_id: Some("resp-tool".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        approval_requests: Vec::new(),
                        tool_calls: vec![ObservedToolCall {
                            call_id: engine::ToolCallId::new("call-1"),
                            tool_id: Some(ToolName::new("test_tool")),
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
                        duration_ms: None,
                        provider_response_id: Some("resp-read-skill".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        approval_requests: Vec::new(),
                        tool_calls: vec![ObservedToolCall {
                            call_id: engine::ToolCallId::new(self.call_id.clone()),
                            tool_id: Some(ToolName::new("vfs.read_file")),
                            tool_name: ToolName::new("vfs_read_file"),
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
                content: engine::ContentRef {
                    content_ref: BlobRef::from_bytes(b"assistant output"),
                    media_type: None,
                    provider_kind: None,
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some("resp-1".to_owned()),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        }
    }

    fn user_input(content_ref: BlobRef) -> Vec<ContextEntryInput> {
        vec![ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }]
    }

    fn request_run_command(content_ref: BlobRef) -> CoreAgentCommand {
        CoreAgentCommand::RequestRun(engine::RunRequestCommand {
            notify_on_terminal: Vec::new(),
            submission_id: None,
            source: engine::RunRequestSource::Input {
                input: user_input(content_ref),
            },
            run_config: run_config(),
        })
    }

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            generation: Default::default(),
            limits: Default::default(),
            context: ContextConfig { compaction: None },
            features: Default::default(),
        }
    }

    fn vfs_config(prompts: bool, skills: bool) -> SessionConfig {
        let mut config = config();
        config.features.vfs = Some(engine::VfsFeature {
            prompts: prompts.then_some(engine::VfsPromptsConfig::default()),
            skills: skills.then_some(engine::VfsSkillsConfig {
                roots: Some(vec!["/skills/system".into()]),
            }),
            ..engine::VfsFeature::default()
        });
        config
    }

    fn vfs_config_with_links(
        prompts: bool,
        skills: bool,
        workspace_links: Vec<WorkspaceLink>,
    ) -> SessionConfig {
        let mut config = vfs_config(prompts, skills);
        config.features.vfs.as_mut().unwrap().workspace_links = workspace_links;
        config
    }

    fn snapshot_link(path: &str, snapshot_ref: &BlobRef) -> WorkspaceLink {
        WorkspaceLink {
            path: path.to_owned(),
            target: WorkspaceLinkTarget::Snapshot {
                snapshot_ref: snapshot_ref.to_string(),
            },
            access: WorkspaceLinkAccess::ReadOnly,
        }
    }

    fn workspace_link(path: &str, workspace_id: &VfsWorkspaceId) -> WorkspaceLink {
        WorkspaceLink {
            path: path.to_owned(),
            target: WorkspaceLinkTarget::Workspace {
                workspace_id: workspace_id.to_string(),
            },
            access: WorkspaceLinkAccess::ReadWrite,
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
            reasoning_effort: None,
            processing_tier: None,
            tool_choice: None,
            parallel_tool_use: None,
        }
    }

    async fn runner_with(llm: Arc<dyn CoreAgentLlm>) -> (SessionRunner, engine::SessionId) {
        let sessions = Arc::new(InMemorySessionStore::new());
        let stores = RunnerStores::new(sessions.clone(), Arc::new(InMemoryBlobStore::new()));
        let session_id = engine::SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
                execution: Default::default(),
                kind: ToolKind::Function(FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: BlobRef::from_bytes(br#"{}"#),
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
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
                    expected_revision: None,
                    key: ContextEntryKey::new("client.native"),
                    entry: ContextEntryInput {
                        kind: ContextEntryKind::ProviderOpaque,
                        content: engine::ContentRef {
                            content_ref: BlobRef::from_bytes(br#"{"type":"input"}"#),
                            media_type: Some("application/json".to_owned()),
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
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
            &entry.event,
            CoreAgentEvent::Context(engine::ContextEvent::CompactionFinished {
                status: ContextCompactionStatus::Failed,
                failure_ref: Some(_),
                ..
            })
        )));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_run_refreshes_environment_projection_before_planning() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores =
            RunnerStores::new(sessions.clone(), blobs.clone()).with_vfs_catalog(vfs.clone());
        let session_id = SessionId::new("session-environment-projection");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            None,
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
            ]),
        )
        .await
        .expect("create snapshot");
        let link = snapshot_link("/workspace", &snapshot.snapshot_ref);
        let llm = Arc::new(CaptureFinalLlm::default());
        let runner = SessionRunner::new(stores, llm.clone());

        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession {
                    config: vfs_config_with_links(false, false, vec![link]),
                },
                max_steps: None,
            })
            .await
            .expect("open session");
        let outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: request_run_command(BlobRef::from_bytes(b"input")),
                max_steps: Some(64),
            })
            .await
            .expect("drive request");

        let vfs_entry = outcome
            .state
            .context
            .entries
            .iter()
            .find(|entry| {
                entry
                    .key
                    .as_ref()
                    .is_some_and(|key| key.as_str() == VFS_CATALOG_CONTEXT_KEY)
            })
            .expect("VFS catalog context entry");
        let vfs_catalog: tools::environment::projection::VfsCatalog = serde_json::from_slice(
            &blobs
                .read_bytes(
                    vfs_entry
                        .provenance_ref
                        .as_ref()
                        .expect("VFS snapshot provenance"),
                )
                .await
                .unwrap(),
        )
        .expect("decode VFS catalog");
        assert_eq!(vfs_catalog.routes.len(), 1);
        assert_eq!(vfs_catalog.routes[0].path.as_str(), "/workspace");

        let requests = llm.requests.lock().expect("requests lock");
        let planned = &requests[0].request.context.entries;
        assert!(planned.iter().any(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|key| key.as_str() == VFS_CATALOG_CONTEXT_KEY)
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forked_session_rehydrates_inherited_opened_event_and_runs() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let stores = RunnerStores::new(sessions.clone(), blobs.clone());
        let source_id = SessionId::new("source-session");
        let child_id = SessionId::new("child-session");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: source_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let llm = Arc::new(CaptureFinalLlm::default());
        let runner = SessionRunner::new(stores, llm.clone());
        let session_config = config();
        runner
            .drive_command(DriveCommand {
                session_id: source_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession {
                    config: session_config.clone(),
                },
                max_steps: None,
            })
            .await
            .expect("open source");

        let source_state = runner
            .load_state(&source_id)
            .await
            .expect("load source state");
        let fork_seq = engine::storage::largest_safe_fork_seq_from_state(&source_state);
        assert_eq!(fork_seq, engine::EventSeq::new(1));
        sessions
            .create_forked_session(CreateForkedSession {
                source_session_id: source_id,
                session_id: child_id.clone(),
                source_seq: fork_seq,
                created_at_ms: 20,
            })
            .await
            .expect("create fork");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id: child_id,
                observed_at_ms: 30,
                command: request_run_command(BlobRef::from_bytes(b"child input")),
                max_steps: Some(64),
            })
            .await
            .expect("run child");

        assert!(outcome.accepted);
        assert_eq!(outcome.state.lifecycle.config, Some(session_config));
        assert!(outcome.state.runs.active.is_none());
        assert_eq!(outcome.state.runs.completed.len(), 1);
        assert!(!outcome.state.context.entries.iter().any(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|key| key.as_str() == VFS_CATALOG_CONTEXT_KEY)
        }));
        assert_eq!(
            outcome
                .emitted_entries
                .first()
                .expect("child emitted entries")
                .position
                .seq,
            engine::EventSeq::new(2)
        );
        assert_eq!(llm.requests.lock().expect("requests").len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disabled_vfs_skills_clear_only_the_runtime_skill_catalog_without_storage() {
        let stores = RunnerStores::new(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryBlobStore::new()),
        );
        let runner = SessionRunner::new(stores, Arc::new(CaptureFinalLlm::default()));
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(config());
        let session_id = SessionId::new("disabled-skills");
        assert!(
            runner
                .refresh_skill_catalog_command(&session_id, &state)
                .await
                .unwrap()
                .is_none()
        );
        state.context.entries.push(engine::ContextEntry {
            entry_id: engine::ContextEntryId::new(1),
            key: Some(engine::ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY)),
            origin: Some("runtime.vfs.skills".into()),
            kind: ContextEntryKind::Catalog {
                title: "Skills".into(),
            },
            source: engine::ContextEntrySource::ContextEdit,
            content: engine::ContentRef::text(BlobRef::from_bytes(b"menu")),
            preview: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        });
        for vfs in [
            None,
            Some(engine::VfsFeature {
                tools: Some(engine::VfsToolSurface::Edit),
                prompts: Some(Default::default()),
                ..Default::default()
            }),
        ] {
            state.lifecycle.config.as_mut().unwrap().features.vfs = vfs;
            let command = runner
                .refresh_skill_catalog_command(&session_id, &state)
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(command, CoreAgentCommand::RemoveContext { key, .. } if key.as_str() == SKILL_CATALOG_CONTEXT_KEY)
            );
        }
        state.context.entries[0].origin = Some("controller".into());
        assert!(
            runner
                .refresh_skill_catalog_command(&session_id, &state)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_run_refreshes_default_vfs_skill_catalog_before_planning() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores =
            RunnerStores::new(sessions.clone(), blobs.clone()).with_vfs_catalog(vfs.clone());
        let session_id = SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let snapshot = create_inline_snapshot(blobs.as_ref(), None, CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    ".agents/skills/deploy-review/SKILL.md",
                    b"---\nname: deploy-review\ndescription: Use when reviewing deploys.\n---\n\nBody\n"
                        .to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("create snapshot");
        let link = snapshot_link("/skills/system", &snapshot.snapshot_ref);
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
                command: CoreAgentCommand::OpenSession {
                    config: {
                        let mut config = vfs_config_with_links(false, true, vec![link]);
                        config.features.vfs.as_mut().unwrap().skills = Some(Default::default());
                        config
                    },
                },
                max_steps: None,
            })
            .await
            .expect("open session");

        let outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: request_run_command(BlobRef::from_bytes(b"input")),
                max_steps: Some(64),
            })
            .await
            .expect("drive request");

        let catalog_ref = engine::current_context_entry(
            &outcome.state,
            &engine::ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY),
        )
        .expect("skill catalog")
        .provenance_ref
        .as_ref()
        .expect("structured skill source");
        let catalog: SkillCatalogSnapshot =
            serde_json::from_slice(&blobs.read_bytes(catalog_ref).await.expect("read catalog"))
                .expect("decode catalog");

        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "deploy-review");
        assert!(matches!(
            &catalog.skills[0].location,
            SkillLocation::LinkedSnapshot {
                source_snapshot_ref,
                source_link_path,
                skill_doc_path,
                ..
            } if source_snapshot_ref == &snapshot.snapshot_ref
                && source_link_path.as_str() == "/skills/system"
                && skill_doc_path.as_str() == "/skills/system/.agents/skills/deploy-review/SKILL.md"
        ));
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event,
                CoreAgentEvent::Context(engine::ContextEvent::EntriesApplied { entries, .. })
                    if entries.iter().any(|entry| {
                        matches!(entry.kind, ContextEntryKind::Catalog { .. }) && entry.key.as_ref().is_some_and(|key| key.as_str() == SKILL_CATALOG_CONTEXT_KEY)
                    })
            )
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_run_refreshes_conventional_vfs_prompt_instructions_before_planning() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores =
            RunnerStores::new(sessions.clone(), blobs.clone()).with_vfs_catalog(vfs.clone());
        let session_id = SessionId::new("session-prompts");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let initial_snapshot = create_inline_snapshot(
            blobs.as_ref(),
            None,
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    ".lightspeed/prompts/instructions.md",
                    b"Keep replies concise.\n".to_vec(),
                )
                .unwrap(),
                InlineFile::new(
                    ".lightspeed/prompts/instructions.d/010-style.md",
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
            display_name: None,
            base_snapshot_ref: Some(initial_snapshot.snapshot_ref.clone()),
            head_snapshot_ref: initial_snapshot.snapshot_ref,
            head_totals: initial_snapshot.manifest.totals.clone(),
            created_at_ms: 1,
        })
        .await
        .expect("create workspace");
        let link = workspace_link("/workspace", &workspace_id);
        let llm = Arc::new(CaptureFinalLlm::default());
        let runner = SessionRunner::new(stores, llm.clone());
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession {
                    config: vfs_config_with_links(true, false, vec![link]),
                },
                max_steps: None,
            })
            .await
            .expect("open session");

        let first_outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: request_run_command(BlobRef::from_bytes(b"first input")),
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
                "/workspace/.lightspeed/prompts/instructions.d/010-style.md",
                "/workspace/.lightspeed/prompts/instructions.md",
            ]
        );
        assert!(first_report.sources.iter().all(|source| source.published));
        assert!(first_outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event,
                CoreAgentEvent::Context(engine::ContextEvent::KeyPrefixReplaced {
                    key_prefix,
                    entries,
                    ..
                }) if key_prefix.as_str() == "instructions"
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
            None,
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    ".lightspeed/prompts/instructions.d/020-focus.md",
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
            display_name: None,
            new_head_snapshot_ref: updated_snapshot.snapshot_ref,
            new_head_totals: updated_snapshot.manifest.totals.clone(),
            updated_at_ms: 30,
        })
        .await
        .expect("update workspace head");

        let second_outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 40,
                command: request_run_command(BlobRef::from_bytes(b"second input")),
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
                &entry.event,
                CoreAgentEvent::Context(engine::ContextEvent::KeyPrefixReplaced {
                    key_prefix,
                    entries,
                    ..
                }) if key_prefix.as_str() == "instructions"
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

        let empty_snapshot = create_inline_snapshot(
            blobs.as_ref(),
            None,
            CreateInlineSnapshotRequest::new(Vec::new()),
        )
        .await
        .expect("create empty prompt snapshot");
        let workspace = vfs
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        vfs.compare_and_set_head(CompareAndSetVfsWorkspaceHead {
            workspace_id,
            expected_revision: Some(workspace.revision),
            display_name: None,
            new_head_snapshot_ref: empty_snapshot.snapshot_ref,
            new_head_totals: empty_snapshot.manifest.totals,
            updated_at_ms: 50,
        })
        .await
        .expect("clear workspace prompts");

        let third_outcome = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 60,
                command: request_run_command(BlobRef::from_bytes(b"third input")),
                max_steps: Some(64),
            })
            .await
            .expect("drive third request");

        assert!(tools::prompts::active_prompt_instruction_entries(&third_outcome.state).is_empty());
        let default_entry = third_outcome
            .state
            .context
            .entries
            .iter()
            .find(|entry| {
                entry
                    .key
                    .as_ref()
                    .is_some_and(|key| key.as_str() == "instructions.000.default")
            })
            .expect("restored default instructions");
        assert!(matches!(default_entry.kind, ContextEntryKind::Instructions));
        assert!(third_outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event,
                CoreAgentEvent::Context(engine::ContextEvent::KeyPrefixReplaced {
                    key_prefix,
                    entries,
                    ..
                }) if key_prefix.as_str() == "instructions"
                    && entries.len() == 1
                    && entries[0].key.as_ref().is_some_and(|key| {
                        key.as_str() == "instructions.000.default"
                    })
            )
        }));
        let requests = llm.requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 3);
        assert!(prompt_instruction_entries_in_request(&requests[2]).is_empty());
        assert!(request_context_entries(&requests[2]).iter().any(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|key| key.as_str() == "instructions.000.default")
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_file_of_cataloged_skill_doc_records_an_ordinary_tool_result() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let blob_store: Arc<dyn BlobStore> = blobs.clone();
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores =
            RunnerStores::new(sessions.clone(), blob_store.clone()).with_vfs_catalog(vfs.clone());
        let session_id = SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let snapshot = create_inline_snapshot(blob_store.as_ref(), None, CreateInlineSnapshotRequest::new(vec![
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
        let linked_fs = LinkedVfsFileSystem::new(
            blob_store.clone(),
            vfs.clone(),
            vec![ResolvedWorkspaceLink {
                path: VfsPath::parse("/skills/system").unwrap(),
                target: ResolvedWorkspaceLinkTarget::AvailableSnapshot {
                    snapshot_ref: snapshot.snapshot_ref.clone(),
                },
                access: WorkspaceLinkAccess::ReadOnly,
            }],
        )
        .expect("linked fs");
        let ctx =
            FsToolContext::new(Arc::new(linked_fs), blob_store.clone()).with_cwd(FsPath::root());
        let toolset = register_toolset(&ToolsetConfig::workspace()).expect("toolset");
        let tool_set = toolset.tools.clone();
        let tools = InlineToolRuntime::with_vfs_filesystem(ctx, tools::runtime::ToolCatalog::new());
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

        let outcome = runner
            .drive_command(DriveCommand {
                session_id,
                observed_at_ms: 20,
                command: request_run_command(BlobRef::from_bytes(b"input")),
                max_steps: Some(96),
            })
            .await
            .expect("drive request");

        assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert!(outcome.state.context.entries.iter().any(|entry| {
            matches!(&entry.kind, ContextEntryKind::ToolResult { call_id, is_error: false }
                if call_id.as_str() == "call-read-skill")
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_skill_read_output_stays_pinned_after_workspace_changes() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let blob_store: Arc<dyn BlobStore> = blobs.clone();
        let vfs = Arc::new(TestVfsCatalog::default());
        let stores =
            RunnerStores::new(sessions.clone(), blob_store.clone()).with_vfs_catalog(vfs.clone());
        let session_id = SessionId::new("session-workspace");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let original_snapshot = create_inline_snapshot(blob_store.as_ref(), None, CreateInlineSnapshotRequest::new(vec![
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
            display_name: None,
            base_snapshot_ref: Some(original_snapshot.snapshot_ref.clone()),
            head_snapshot_ref: original_snapshot.snapshot_ref.clone(),
            head_totals: original_snapshot.manifest.totals.clone(),
            created_at_ms: 1,
        })
        .await
        .expect("create workspace");
        let linked_fs = LinkedVfsFileSystem::new(
            blob_store.clone(),
            vfs.clone(),
            vec![ResolvedWorkspaceLink {
                path: VfsPath::parse("/skills/system").unwrap(),
                target: ResolvedWorkspaceLinkTarget::AvailableWorkspace {
                    workspace: vfs.read_workspace(&workspace_id).await.unwrap(),
                },
                access: WorkspaceLinkAccess::ReadWrite,
            }],
        )
        .expect("linked fs");
        let ctx =
            FsToolContext::new(Arc::new(linked_fs), blob_store.clone()).with_cwd(FsPath::root());
        let toolset = register_toolset(&ToolsetConfig::workspace()).expect("toolset");
        let tool_set = toolset.tools.clone();
        let tools = InlineToolRuntime::with_vfs_filesystem(ctx, tools::runtime::ToolCatalog::new());
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

        let outcome = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 20,
                command: request_run_command(BlobRef::from_bytes(b"input")),
                max_steps: Some(96),
            })
            .await
            .expect("drive request");
        let output_ref = tool_output_ref(&outcome, "call-read-skill");
        let loaded_skill = read_file_result(blobs.as_ref(), &output_ref).await;
        assert!(loaded_skill.text.contains("Original body"));

        let updated_snapshot = create_inline_snapshot(blob_store.as_ref(), None, CreateInlineSnapshotRequest::new(vec![
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
            display_name: None,
            new_head_snapshot_ref: updated_snapshot.snapshot_ref,
            new_head_totals: updated_snapshot.manifest.totals.clone(),
            updated_at_ms: 30,
        })
        .await
        .expect("update workspace head");

        let current_fs = LinkedVfsFileSystem::new(
            blob_store.clone(),
            vfs.clone(),
            vec![ResolvedWorkspaceLink {
                path: VfsPath::parse("/skills/system").unwrap(),
                target: ResolvedWorkspaceLinkTarget::AvailableWorkspace {
                    workspace: vfs.read_workspace(&workspace_id).await.unwrap(),
                },
                access: WorkspaceLinkAccess::ReadWrite,
            }],
        )
        .expect("current linked fs");
        let current_skill = tools::fs::tools::invoke_read_file(
            &FsToolContext::new(Arc::new(current_fs), blob_store.clone()).with_cwd(FsPath::root()),
            tools::fs::tools::ReadFileArgs {
                path: FsPath::new("/skills/system/deploy-review/SKILL.md").unwrap(),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("ordinary read of current skill");
        assert!(current_skill.text.contains("Updated body"));

        let pinned_skill = read_file_result(blobs.as_ref(), &output_ref).await;
        assert!(pinned_skill.text.contains("Original body"));
        assert!(!pinned_skill.text.contains("Updated body"));
        let replayed = runner
            .load_state(&session_id)
            .await
            .expect("replay after workspace edit");
        assert_eq!(replayed, outcome.state);
        let recorded = replayed
            .context
            .entries
            .iter()
            .find(|entry| {
                matches!(&entry.kind, ContextEntryKind::ToolResult { call_id, is_error: false }
                    if call_id.as_str() == "call-read-skill")
            })
            .expect("recorded ordinary read");
        // The model-visible rendering is stored separately from the structured output.
        let recorded_text = blobs
            .read_text(&recorded.content.content_ref)
            .await
            .expect("recorded model-visible text");
        assert!(recorded_text.contains("Original body"));
        assert!(!recorded_text.contains("Updated body"));
    }

    fn tool_output_ref(outcome: &DriveOutcome, call_id: &str) -> BlobRef {
        outcome
            .emitted_entries
            .iter()
            .find_map(|entry| match &entry.event {
                CoreAgentEvent::Tool(engine::ToolEvent::CallCompleted { result, .. })
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
                entry.content.provider_kind.as_deref(),
                Some(PROMPT_INSTRUCTIONS_PROVIDER_KIND)
            );
            assert!(entry.provenance_ref.is_some());
        }
    }

    fn prompt_content_refs(entries: &[&engine::ContextEntry]) -> Vec<BlobRef> {
        let mut refs = entries
            .iter()
            .map(|entry| entry.content.content_ref.clone())
            .collect::<Vec<_>>();
        refs.sort();
        refs
    }

    fn prompt_report_ref_from_entries(entries: &[&engine::ContextEntry]) -> BlobRef {
        let first = entries
            .first()
            .and_then(|entry| entry.provenance_ref.as_ref())
            .expect("prompt report ref");
        let report_ref = first.clone();
        for entry in entries {
            assert_eq!(entry.provenance_ref.as_ref(), Some(first));
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
                .read_bytes(&entry.content.content_ref)
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
            && entry.content.provider_kind.as_deref() == Some(PROMPT_INSTRUCTIONS_PROVIDER_KIND)
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
                command: request_run_command(BlobRef::from_bytes(b"input")),
                max_steps: Some(32),
            })
            .await
            .expect("drive request");

        assert_eq!(failed.quiescence, RunnerQuiescence::Idle);
        assert_eq!(failed.state.runs.completed[0].status, RunStatus::Failed);
        assert!(failed.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event,
                CoreAgentEvent::Turn(TurnEvent::Completed {
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
                command: request_run_command(BlobRef::from_bytes(b"input-2")),
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
                command: request_run_command(BlobRef::from_bytes(b"input")),
                max_steps: Some(64),
            })
            .await
            .expect("drive request");

        assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event,
                CoreAgentEvent::Tool(engine::ToolEvent::CallCompleted {
                    result: ToolCallResult {
                        duration_ms: None,
                        output_bytes: None,
                        truncated: false,
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
