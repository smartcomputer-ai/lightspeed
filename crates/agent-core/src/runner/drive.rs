use std::sync::Arc;

use crate::{
    AdmitCommand, ApplyEvent, CoreAdmitCommand, CoreAgentCodec, CoreAgentLlm, CoreAgentState,
    CoreAgentTools, CoreAgentWorkflow, CoreApplyEvent, CorePlanner, EventSeq, WorkflowEventBuffer,
    core_agent::workflow::classify_quiescence,
    runner::{
        error::RunnerError,
        protocol::{DEFAULT_MAX_STEPS, DriveCommand, DriveOutcome, DriveSession, RunnerStores},
    },
    storage::ReadSessionEvents,
};

const DEFAULT_READ_PAGE_SIZE: usize = 256;

pub struct SessionRunner {
    stores: RunnerStores,
    llm: Arc<dyn CoreAgentLlm>,
    tools: Option<Arc<dyn CoreAgentTools>>,
    admit: CoreAdmitCommand,
    apply: CoreApplyEvent,
    planner: CorePlanner,
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
            admit: CoreAdmitCommand,
            apply: CoreApplyEvent,
            planner: CorePlanner::core(),
            read_page_size: DEFAULT_READ_PAGE_SIZE,
        }
    }

    pub fn with_tools(mut self, tools: Arc<dyn CoreAgentTools>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub async fn drive_command(&self, request: DriveCommand) -> Result<DriveOutcome, RunnerError> {
        let max_steps = resolve_max_steps(request.max_steps)?;
        let mut state = self.load_state(&request.session_id).await?;
        let mut emitted_entries = Vec::new();
        let mut event_buffer = WorkflowEventBuffer::new(state.reduced_to.clone());

        let proposals = match self.admit.admit(&state, request.command) {
            Ok(proposals) => proposals,
            Err(crate::CommandError::Rejected(rejection)) => {
                let quiescence = classify_quiescence(&state);
                return Ok(DriveOutcome {
                    session_id: request.session_id,
                    accepted: false,
                    rejection: Some(rejection),
                    head: state.reduced_to.clone(),
                    emitted_entries,
                    state,
                    quiescence,
                });
            }
            Err(error) => return Err(error.into()),
        };

        event_buffer.stage_and_apply(&self.apply, &mut state, proposals, request.observed_at_ms)?;

        let workflow = self.workflow(&request.session_id);
        let quiescence = workflow
            .drive_until_quiescent(
                &mut state,
                &mut event_buffer,
                request.observed_at_ms,
                max_steps,
                &mut emitted_entries,
            )
            .await?;

        Ok(DriveOutcome {
            session_id: request.session_id,
            accepted: true,
            rejection: None,
            head: state.reduced_to.clone(),
            emitted_entries,
            state,
            quiescence,
        })
    }

    pub async fn drive_until_quiescent(
        &self,
        request: DriveSession,
    ) -> Result<DriveOutcome, RunnerError> {
        let max_steps = resolve_max_steps(request.max_steps)?;
        let mut state = self.load_state(&request.session_id).await?;
        let mut emitted_entries = Vec::new();
        let mut event_buffer = WorkflowEventBuffer::new(state.reduced_to.clone());
        let workflow = self.workflow(&request.session_id);
        let quiescence = workflow
            .drive_until_quiescent(
                &mut state,
                &mut event_buffer,
                request.observed_at_ms,
                max_steps,
                &mut emitted_entries,
            )
            .await?;

        Ok(DriveOutcome {
            session_id: request.session_id,
            accepted: true,
            rejection: None,
            head: state.reduced_to.clone(),
            emitted_entries,
            state,
            quiescence,
        })
    }

    pub async fn load_state(
        &self,
        session_id: &crate::SessionId,
    ) -> Result<CoreAgentState, RunnerError> {
        let mut state = CoreAgentState::new();
        let mut after: Option<EventSeq> = None;
        let codec = CoreAgentCodec;
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

    fn workflow<'a>(&'a self, session_id: &'a crate::SessionId) -> CoreAgentWorkflow<'a> {
        CoreAgentWorkflow::new(
            session_id,
            self.stores.sessions.as_ref(),
            self.stores.blobs.as_ref(),
            &self.apply,
            &self.planner,
            self.llm.as_ref(),
            self.tools.as_deref(),
        )
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

#[cfg(test)]
mod p53_tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;

    use super::*;
    use crate::{
        AgentHandle, BlobRef, ContextConfig, ContextItemKind, ContextItemSource,
        ContextMessageRole, CoreAgentCommand, CoreAgentEventKind, CoreAgentIoError, CoreAgentLlm,
        CoreAgentTools, DriveCommand, FunctionToolSpec, LlmFinish, LlmGenerationFacts,
        LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, ModelProviderOptions,
        ModelSelection, ObservedToolCall, ProviderApiKind, ProviderRequestDefaults, RunConfig,
        RunStatus, RunnerQuiescence, RunnerStores, SessionConfig, ToolCallResult, ToolCallStatus,
        ToolEvent, ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolKind, ToolName,
        ToolParallelism, ToolProfile, ToolProfileId, ToolRegistry, ToolSpec, ToolTargetRequirement,
        TurnConfig, TurnEvent, TurnOutcome, UncommittedContextItem,
        storage::{CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
    };

    #[derive(Clone, Debug)]
    struct FinalOutputLlm;

    #[async_trait]
    impl CoreAgentLlm for FinalOutputLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            Ok(final_output_result(&request))
        }
    }

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
                            call_id: crate::ToolCallId::new("call-1"),
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

    #[derive(Clone, Debug)]
    struct FailingTools;

    #[async_trait]
    impl CoreAgentTools for FailingTools {
        async fn invoke_batch(
            &self,
            _request: ToolInvocationBatchRequest,
        ) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
            Err(CoreAgentIoError::Failed {
                message: "tool runtime unavailable".to_owned(),
            })
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
            tool_profile_id: None,
        }
    }

    fn run_config() -> RunConfig {
        RunConfig {
            max_turns: None,
            max_tool_rounds: None,
            model_override: None,
        }
    }

    async fn runner() -> (SessionRunner, crate::SessionId) {
        runner_with(Arc::new(FinalOutputLlm), None).await
    }

    async fn runner_with(
        llm: Arc<dyn CoreAgentLlm>,
        tools: Option<Arc<dyn CoreAgentTools>>,
    ) -> (SessionRunner, crate::SessionId) {
        let sessions = Arc::new(InMemorySessionStore::new());
        let stores = RunnerStores::new(sessions.clone(), Arc::new(InMemoryBlobStore::new()));
        let session_id = crate::SessionId::new("session-a");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let mut runner = SessionRunner::new(stores, llm);
        if let Some(tools) = tools {
            runner = runner.with_tools(tools);
        }
        (runner, session_id)
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
    async fn inline_llm_result_drives_to_completed_run_without_effect_events() {
        let (runner, session_id) = runner().await;
        let open = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenSession { config: config() },
                max_steps: None,
            })
            .await
            .expect("open session");
        assert!(open.accepted);

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
        assert!(outcome.state.runs.active.is_none());
        assert_eq!(outcome.state.runs.completed.len(), 1);
        assert_eq!(outcome.state.runs.completed[0].status, RunStatus::Completed);
        assert_eq!(
            outcome.state.runs.completed[0].output_ref,
            Some(BlobRef::from_bytes(b"assistant output"))
        );
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                entry.event.kind,
                CoreAgentEventKind::Turn(TurnEvent::GenerationCompleted { .. })
            )
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn llm_io_error_is_recorded_and_drive_can_retry() {
        let (runner, session_id) = runner_with(
            Arc::new(FailOnceLlm {
                calls: Mutex::new(0),
            }),
            None,
        )
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
                    outcome: TurnOutcome::Failed {
                        failure_ref: Some(_)
                    },
                    ..
                })
            )
        }));
        assert!(outcome.emitted_entries.iter().any(|entry| {
            matches!(
                &entry.event.kind,
                CoreAgentEventKind::Run(crate::RunEvent::Completed { .. })
            )
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_io_error_is_recorded_and_drive_can_continue() {
        let (runner, session_id) = runner_with(
            Arc::new(ToolThenFinalLlm {
                calls: Mutex::new(0),
            }),
            Some(Arc::new(FailingTools)),
        )
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
                CoreAgentEventKind::Tool(ToolEvent::CallCompleted {
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
