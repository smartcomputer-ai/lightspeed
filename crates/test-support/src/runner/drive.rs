use std::sync::Arc;

use engine::{
    ApplyEvent, BlobRef, CoreAgentAction, CoreAgentDrive, CoreAgentDriveError, CoreAgentIoError,
    CoreAgentLlm, CoreAgentState, CoreAgentTools, CoreApplyEvent, EventSeq, LlmFinish,
    LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, SessionId,
    ToolCallStatus, ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    storage::{AppendSessionEvents, BlobStore, ReadSessionEvents},
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
        RunStatus, SessionConfig, ToolCallResult, ToolKind, ToolName, ToolParallelism, ToolProfile,
        ToolProfileId, ToolRegistry, ToolSpec, ToolTargetRequirement, TurnConfig, TurnEvent,
        UncommittedContextItem,
        storage::{CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
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

        let outcome = runner
            .drive_command(DriveCommand {
                session_id,
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

        assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert!(outcome.emitted_entries.iter().any(|entry| {
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
