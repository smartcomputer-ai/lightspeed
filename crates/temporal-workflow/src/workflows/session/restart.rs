//! Taking over an open session whose last execution stopped. Everything comes from the
//! log, and nothing here was persisted for this purpose. What the log cannot say is
//! whether work in flight took effect: a generation may have been paid for, and every
//! call of a batch is logged as started before any of them runs. So the interrupted
//! run fails instead of continuing, and no call that is unsafe to repeat runs again.

use super::*;
use harness::{CodeToolCallStatus, CodeToolOrigin, RunId, ToolCallStatus};

/// How many of the latest ended runs notify their holders again. Terminal notifications
/// leave at the next pass of the session loop, so only the last few can have been lost.
const RESENT_RUN_TERMINALS: usize = 8;

const INTERRUPTED_RUN_MESSAGE: &str = "the session's workflow stopped while this run was in \
     progress, and the session was restarted from its log. The run did not finish, and a tool \
     call or model request in flight when it stopped may or may not have taken effect.";

const INTERRUPTED_CODE_TOOL_CALL_MESSAGE: &str = "the session's workflow stopped while this call \
     was pending, and the session was restarted from its log. The call may or may not have taken \
     effect, and it is not safe to repeat automatically.";

/// Settle what the stopped execution left behind, then open for work. A failure ends
/// this execution as well, and asking for another restart tries again.
pub(super) async fn recover_interrupted_work(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    // Before the failure below, whose own notification goes out with its append.
    ctx.state_mut(AgentSessionWorkflow::queue_recent_run_terminals)?;
    let failed_code_tool_calls = fail_unsafe_code_tool_calls(ctx).await?;
    let interrupted = ctx.state(|state| interrupted_run(&state.core_state));
    if let Some(run_id) = interrupted {
        let message_ref = activity_calls::put_refusal_blob(ctx, INTERRUPTED_RUN_MESSAGE).await;
        let mut drive = drive_from_state(ctx)?;
        let command = CoreAgentCommand::FailRun {
            run_id,
            message_ref,
        };
        match admit_and_append_command(ctx, &mut drive, command, None).await? {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                anyhow::bail!("failing the interrupted run: {}", failure.message)
            }
        }
    }
    ctx.state_mut(AgentSessionWorkflow::queue_unanswered_invocations)?;
    ctx.state_mut(|state| state.ready = true);
    observability::log_restarted(
        ctx,
        interrupted.map(|run_id| run_id.as_u64()),
        failed_code_tool_calls,
    );
    Ok(())
}

/// The active run, unless it is parked. A parked run is waiting on an approval, a
/// promise or a workflow it does not run itself, with nothing of its own in flight, so
/// it can go on waiting on the new execution.
fn interrupted_run(state: &CoreAgentState) -> Option<RunId> {
    let run = state.runs.active.as_ref()?;
    let parked = run.status == RunStatus::Parked
        || (run.status == RunStatus::Running && run.parked_tool_batch.is_some());
    (!parked).then_some(run.run_id)
}

/// Pending code tool calls that the dispatcher would run again but must not. Nothing
/// logs whether a pending call had started. Calls safe to repeat stay pending and go
/// out again, and a call waiting on a promise has already run.
fn unsafe_pending_code_tool_calls(state: &CoreAgentState) -> Vec<CodeToolOrigin> {
    state
        .code_tools
        .scopes
        .values()
        .flat_map(|scope| scope.calls.values())
        .filter(|call| matches!(call.status, CodeToolCallStatus::Pending))
        .filter(|call| {
            !tool_batches::call_execution_spec(state, Some(&call.spec.tool_id)).retry_safe
        })
        .map(|call| call.spec.origin.clone())
        .collect()
}

async fn fail_unsafe_code_tool_calls(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<usize> {
    let origins = ctx.state(|state| unsafe_pending_code_tool_calls(&state.core_state));
    if origins.is_empty() {
        return Ok(0);
    }
    let error_ref =
        tool_batches::put_boundary_error_blob(ctx, INTERRUPTED_CODE_TOOL_CALL_MESSAGE).await;
    let count = origins.len();
    let mut drive = drive_from_state(ctx)?;
    for origin in origins {
        let command = CoreAgentCommand::CompleteCodeToolCall {
            result: tool_batches::boundary_call_result(
                harness::code_tool_call_id(&origin),
                ToolCallStatus::Failed,
                error_ref.clone(),
            ),
            origin,
        };
        // A call that cannot take a result has no parent left to dispatch for, so
        // this must not stop the session from coming back.
        if let CommandAdmissionResult::Rejected(failure) =
            admit_and_append_command(ctx, &mut drive, command, None).await?
        {
            record_warning(ctx, format!("restart: {}", failure.message));
        }
    }
    Ok(count)
}

impl AgentSessionWorkflow {
    fn restart_identity(&self) -> anyhow::Result<(uuid::Uuid, SessionId)> {
        let universe_id = self
            .universe_id
            .ok_or_else(|| anyhow::anyhow!("initialized session is missing universe id"))?;
        let session_id = self
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("initialized session is missing session id"))?;
        Ok((universe_id, session_id))
    }

    /// The queue of terminal notifications died with the execution, and state does not
    /// record which went out. Receivers know a repeat by its emission id.
    fn queue_recent_run_terminals(&mut self) -> anyhow::Result<()> {
        let (universe_id, session_id) = self.restart_identity()?;
        let finished = &self.core_state.runs.finished;
        let recent = &finished[finished.len().saturating_sub(RESENT_RUN_TERMINALS)..];
        let emissions = recent.iter().flat_map(|record| {
            session_state::run_terminal_emissions(
                universe_id,
                &session_id,
                record.terminal_seq,
                record,
            )
        });
        self.pending_emissions.extend(emissions);
        Ok(())
    }

    /// Pushed invocations a run or the session still waits on. Each may never have
    /// left the old execution, and nothing else would send it. An invocation of a run
    /// that has ended is not asked for again. State does not keep where an invocation
    /// was logged, so these name the head of the log.
    fn queue_unanswered_invocations(&mut self) -> anyhow::Result<()> {
        let (universe_id, session_id) = self.restart_identity()?;
        let Some(log_seq) = self.core_state.reduced_to.as_ref().map(|head| head.seq) else {
            return Ok(());
        };
        let holder_workflow_id = crate::compose_workflow_id(universe_id, &session_id);
        let tools = &self.core_state.workflow_tools;
        let emissions = tools
            .bound_invocations
            .values()
            .filter(|invocation| invocation_is_awaited(&self.core_state, invocation))
            .filter_map(|invocation| {
                let harness::WorkflowToolTarget::Bound {
                    receiver,
                    dispatch: harness::BoundWorkflowToolDispatch::Push,
                } = &tools.bindings.get(&invocation.workflow_tool_id)?.target
                else {
                    return None;
                };
                Some(PendingEmission::immediate(
                    receiver.workflow_id.clone(),
                    EmissionEnvelope::tool_invocation(
                        universe_id,
                        session_id.clone(),
                        log_seq,
                        invocation.clone(),
                        holder_workflow_id.clone(),
                    ),
                ))
            })
            .collect::<Vec<_>>();
        self.pending_emissions.extend(emissions);
        Ok(())
    }
}

fn invocation_is_awaited(
    state: &CoreAgentState,
    invocation: &harness::WorkflowToolInvocation,
) -> bool {
    let active_run = state.runs.active.as_ref().map(|run| run.run_id);
    invocation
        .completion_promises
        .iter()
        .flat_map(|promises| promises.values())
        .filter_map(|promise_id| state.promises.promises.get(promise_id))
        .any(|promise| {
            promise.status == harness::PromiseStatus::Pending
                && match promise.scope {
                    harness::PromiseScope::Session => true,
                    harness::PromiseScope::Run { run_id } => Some(run_id) == active_run,
                }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        ApprovalSubject, BlobRef, CoreAgentCodec, FunctionToolSpec, LlmFinish, LlmGenerationFacts,
        LlmGenerationResult, LlmGenerationStatus, ModelSelection, ObservedToolCall,
        ProviderApiKind, RemoteMcpApprovalPolicy, RemoteMcpExecution, RemoteMcpExposure,
        RemoteMcpToolSpec, RunConfig, RunRequestCommand, RunRequestSource, RunTerminalNotifyIntent,
        RuntimeMcpApprovalRequest, SessionConfig, ToolCallId, ToolId, ToolInvocationResult,
        ToolKind, ToolName, ToolParallelism, ToolSpec, apply_event, session::StoredSessionEntry,
    };

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiCompletions,
                provider_id: "p".to_owned(),
                model: "m".to_owned(),
            },
            generation: Default::default(),
            limits: Default::default(),
            context: harness::ContextConfig {
                input_limit_tokens: None,
                compaction: None,
            },
            features: Default::default(),
        }
    }

    /// A session log and the drive of the execution writing it. Once frozen, `replayed`
    /// gives what a new execution loads, so nothing in memory reaches it.
    struct Session {
        drive: CoreAgentDrive,
        log: Vec<StoredSessionEntry>,
        now: u64,
    }

    impl Session {
        fn opened(tools: Vec<ToolSpec>) -> Self {
            let drive = CoreAgentDrive::from_replayed(SessionId::new("s"), CoreAgentState::new());
            let mut session = Self {
                drive,
                log: Vec::new(),
                now: 10,
            };
            session.admit(CoreAgentCommand::OpenSession { config: config() });
            let tools = tools
                .into_iter()
                .map(|tool| (tool.id.clone(), tool))
                .collect();
            session.admit(CoreAgentCommand::ReplaceTools {
                expected_revision: Some(session.drive.state().tooling.revision),
                tools,
            });
            session
        }

        fn tick(&mut self) -> u64 {
            self.now += 1;
            self.now
        }

        fn commit(&mut self, action: CoreAgentAction) {
            let CoreAgentAction::AppendEvents { events, .. } = action else {
                panic!("expected an append, got {action:?}");
            };
            let entries = events
                .into_iter()
                .map(|event| {
                    let entry = StoredSessionEntry {
                        position: SessionPosition {
                            seq: harness::EventSeq::new(self.log.len() as u64 + 1),
                        },
                        observed_at_ms: event.observed_at_ms,
                        joins: event.joins,
                        event: event.event,
                    };
                    self.log.push(entry.clone());
                    entry
                })
                .collect();
            self.drive.resume_appended(entries).expect("apply");
        }

        fn admit(&mut self, command: CoreAgentCommand) {
            let now = self.tick();
            let action = self.drive.admit_command(command, now).expect("admit");
            self.commit(action);
        }

        fn request_run(&mut self, input: &[u8], submission: &str) {
            self.admit(run_command(input, submission, Vec::new()));
        }

        fn until_generation(&mut self) -> harness::LlmGenerationRequest {
            for _ in 0..100 {
                let now = self.tick();
                match self.drive.next_action_unbounded(now).expect("next") {
                    CoreAgentAction::GenerateLlm { request } => return request,
                    action => self.commit(action),
                }
            }
            panic!("no generation planned");
        }

        fn until_batch(&mut self) -> harness::ToolInvocationBatchRequest {
            for _ in 0..100 {
                let now = self.tick();
                match self.drive.next_action_unbounded(now).expect("next") {
                    CoreAgentAction::InvokeTools { request } => return request,
                    action => self.commit(action),
                }
            }
            panic!("no tool batch planned");
        }

        fn finish_turn_with_calls(
            &mut self,
            request: &harness::LlmGenerationRequest,
            calls: &[(&str, &str)],
        ) {
            let now = self.tick();
            let action = self
                .drive
                .resume_generation(tool_call_generation(request, calls), now)
                .expect("resume generation");
            self.commit(action);
        }

        /// What the restart does: reduce the log alone, then fail the interrupted run.
        fn restarted(&self) -> Self {
            let mut state = CoreAgentState::new();
            for stored in &self.log {
                let entry = CoreAgentCodec.decode_entry(stored).expect("decode");
                apply_event(&mut state, &entry).expect("apply");
            }
            Self {
                drive: CoreAgentDrive::from_replayed(SessionId::new("s"), state),
                log: self.log.clone(),
                now: self.now + 1_000,
            }
        }

        fn fail_interrupted(&mut self) {
            let run_id = interrupted_run(self.drive.state()).expect("an interrupted run");
            self.admit(CoreAgentCommand::FailRun {
                run_id,
                message_ref: BlobRef::from_bytes(b"interrupted"),
            });
        }

        /// Commit until the drive stops, and name any work it asks the runtime for.
        fn settle(&mut self) -> Option<String> {
            for _ in 0..200 {
                let now = self.tick();
                match self.drive.next_action_unbounded(now).expect("next") {
                    action @ CoreAgentAction::AppendEvents { .. } => self.commit(action),
                    CoreAgentAction::GenerateLlm { request } => {
                        return Some(format!("generate run {}", request.run_id));
                    }
                    CoreAgentAction::InvokeTools { request } => {
                        return Some(format!("invoke {:?}", request.calls[0].call_id));
                    }
                    CoreAgentAction::CompactContext { .. } => return Some("compact".to_owned()),
                    CoreAgentAction::Idle | CoreAgentAction::Closed => return None,
                    other => panic!("unexpected {other:?}"),
                }
            }
            panic!("the drive did not settle");
        }

        fn context_kinds(&self) -> Vec<ContextEntryKind> {
            let entries = &self.drive.state().context.entries;
            entries.iter().map(|entry| entry.kind.clone()).collect()
        }
    }

    fn user_entry(bytes: &[u8]) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content: harness::ContentRef::text(BlobRef::from_bytes(bytes)),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    fn run_command(
        input: &[u8],
        submission: &str,
        notify_on_terminal: Vec<RunTerminalNotifyIntent>,
    ) -> CoreAgentCommand {
        CoreAgentCommand::RequestRun(RunRequestCommand {
            requested_by: None,
            notify_on_terminal,
            submission_id: Some(SubmissionId::new(submission)),
            source: RunRequestSource::Input {
                input: vec![user_entry(input)],
            },
            run_config: RunConfig::default(),
            input_limit_tokens: None,
        })
    }

    fn function_tool(name: &str) -> ToolSpec {
        ToolSpec {
            id: ToolId::new(name),
            execution: Default::default(),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: None,
                input_schema_ref: BlobRef::from_bytes(br#"{"type":"object"}"#),
                output_schema_ref: None,
                strict: None,
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::Exclusive,
        }
    }

    fn runtime_mcp_tool(name: &str) -> ToolSpec {
        ToolSpec {
            id: ToolId::new(name),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "mail".to_owned(),
                record_revision: 1,
                server_label: "mail".to_owned(),
                description_ref: None,
                allowed_tools: None,
                execution: RemoteMcpExecution::Runtime,
                exposure: RemoteMcpExposure::Inject,
                approval: RemoteMcpApprovalPolicy::Always,
                defer_loading: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }
    }

    fn tool_call_generation(
        request: &harness::LlmGenerationRequest,
        calls: &[(&str, &str)],
    ) -> LlmGenerationResult {
        let observed = calls
            .iter()
            .map(|(call_id, tool)| ObservedToolCall {
                call_id: ToolCallId::new(*call_id),
                tool_id: Some(ToolId::new(*tool)),
                tool_name: ToolName::new(*tool),
                provider_kind: None,
                arguments_ref: BlobRef::from_bytes(call_id.as_bytes()),
                native_call_ref: None,
            })
            .collect::<Vec<_>>();
        let context_entries = observed
            .iter()
            .map(|call| ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call.call_id.clone(),
                    name: call.tool_name.clone(),
                },
                content: harness::ContentRef::text(BlobRef::from_bytes(
                    call.call_id.as_str().as_bytes(),
                )),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            })
            .collect();
        LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries,
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some("resp".to_owned()),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: observed,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        }
    }

    fn tool_result(call_id: &ToolCallId) -> ToolInvocationResult {
        let output = BlobRef::from_bytes(call_id.as_str().as_bytes());
        ToolInvocationResult {
            attachments: Vec::new(),
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output.clone()),
            model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
                call_id,
                ToolCallStatus::Succeeded,
                output,
            )],
            error_ref: None,
            effects: Vec::new(),
        }
    }

    fn tool_results(session: &Session) -> Vec<ToolCallId> {
        session
            .context_kinds()
            .into_iter()
            .filter_map(|kind| match kind {
                ContextEntryKind::ToolResult { call_id, .. } => Some(call_id),
                _ => None,
            })
            .collect()
    }

    fn last_run_status(session: &Session) -> RunStatus {
        session
            .drive
            .state()
            .runs
            .finished
            .last()
            .expect("an ended run")
            .status
    }

    #[test]
    fn a_generation_in_flight_fails_its_run_instead_of_being_paid_for_twice() {
        let mut session = Session::opened(Vec::new());
        session.request_run(b"hi", "sub-1");
        let lost = session.until_generation();

        // Left to itself the log asks for the identical request again.
        let mut replayed = session.restarted();
        let now = replayed.tick();
        let CoreAgentAction::GenerateLlm { request } =
            replayed.drive.next_action_unbounded(now).expect("next")
        else {
            panic!("the log should plan the generation again");
        };
        assert_eq!(request, lost);

        let mut restarted = session.restarted();
        restarted.fail_interrupted();
        assert_eq!(restarted.settle(), None);
        assert!(restarted.drive.state().runs.active.is_none());
        assert_eq!(last_run_status(&restarted), RunStatus::Failed);
    }

    #[test]
    fn calls_left_open_get_results_and_none_runs_again() {
        let mut session = Session::opened(vec![function_tool("shell")]);
        session.request_run(b"hi", "sub-1");
        let generation = session.until_generation();
        session.finish_turn_with_calls(&generation, &[("c1", "shell"), ("c2", "shell")]);
        let batch = session.until_batch();
        let now = session.tick();
        let action = session
            .drive
            .resume_tool_call(batch.batch_id, tool_result(&ToolCallId::new("c1")), now)
            .expect("first result");
        session.commit(action);

        // Left to itself the log runs c2 again, though it may have run already.
        let mut replayed = session.restarted();
        let now = replayed.tick();
        let CoreAgentAction::InvokeTools { request } =
            replayed.drive.next_action_unbounded(now).expect("next")
        else {
            panic!("the log should dispatch the open call again");
        };
        assert_eq!(request.calls[0].call_id, ToolCallId::new("c2"));

        let mut restarted = session.restarted();
        restarted.fail_interrupted();
        assert_eq!(restarted.settle(), None);
        assert_eq!(last_run_status(&restarted), RunStatus::Failed);
        assert_eq!(
            tool_results(&restarted),
            vec![ToolCallId::new("c1"), ToolCallId::new("c2")]
        );

        // The next request is well formed, so the session serves the next run.
        restarted.request_run(b"next", "sub-2");
        assert!(restarted.settle().expect("work").starts_with("generate"));
    }

    #[test]
    fn a_finished_tool_call_turn_with_no_batch_yet_still_gets_its_results() {
        let mut session = Session::opened(vec![function_tool("shell")]);
        session.request_run(b"hi", "sub-1");
        let generation = session.until_generation();
        session.finish_turn_with_calls(&generation, &[("c1", "shell")]);

        let mut restarted = session.restarted();
        restarted.fail_interrupted();
        assert_eq!(restarted.settle(), None);
        assert_eq!(tool_results(&restarted), vec![ToolCallId::new("c1")]);
        restarted.request_run(b"next", "sub-2");
        assert!(restarted.settle().expect("work").starts_with("generate"));
    }

    #[test]
    fn a_queued_run_starts_after_the_interrupted_one_and_a_retry_starts_nothing() {
        let mut session = Session::opened(Vec::new());
        session.request_run(b"first", "sub-1");
        let interrupted = session.until_generation();
        session.request_run(b"second", "sub-2");

        let mut restarted = session.restarted();
        restarted.fail_interrupted();
        assert!(restarted.settle().expect("work").starts_with("generate"));
        let active = restarted
            .drive
            .state()
            .runs
            .active
            .as_ref()
            .expect("active");
        assert_ne!(active.run_id, interrupted.run_id);
        assert_eq!(active.submission_id, Some(SubmissionId::new("sub-2")));

        // A caller retrying either submission gets the run it already has.
        for (input, submission) in [(b"first".as_slice(), "sub-1"), (b"second", "sub-2")] {
            let now = restarted.tick();
            let retried = restarted
                .drive
                .admit_command(run_command(input, submission, Vec::new()), now)
                .expect("retry");
            assert!(!matches!(retried, CoreAgentAction::AppendEvents { .. }));
        }
    }

    #[test]
    fn a_run_parked_on_an_approval_stays_parked_and_asks_for_no_work() {
        let mut session = Session::opened(vec![runtime_mcp_tool("mcp_mail")]);
        session.request_run(b"hi", "sub-1");
        let generation = session.until_generation();
        session.finish_turn_with_calls(&generation, &[("c1", "mcp_mail")]);
        let batch = session.until_batch();
        let subject = ApprovalSubject::McpToolCall {
            server_id: "mail".to_owned(),
            server_label: "mail".to_owned(),
            tool_name: "send".to_owned(),
            arguments_ref: batch.calls[0].arguments_ref.clone(),
        };
        let now = session.tick();
        let action = session
            .drive
            .request_runtime_mcp_approvals(
                batch.batch_id,
                vec![RuntimeMcpApprovalRequest {
                    call_id: batch.calls[0].call_id.clone(),
                    subject,
                }],
                now,
            )
            .expect("park");
        session.commit(action);

        let mut restarted = session.restarted();
        assert_eq!(interrupted_run(restarted.drive.state()), None);
        assert_eq!(restarted.settle(), None);
        let active = restarted
            .drive
            .state()
            .runs
            .active
            .as_ref()
            .expect("active");
        assert_eq!(active.status, RunStatus::Parked);
    }

    #[test]
    fn only_a_run_with_nothing_parked_counts_as_interrupted() {
        let mut session = Session::opened(Vec::new());
        session.request_run(b"hi", "sub-1");
        session.until_generation();
        let parked = harness::ParkedToolBatch {
            batch_id: harness::ToolBatchId::new(1),
            suspension: harness::ToolBatchSuspension::AwaitTool {
                call_id: ToolCallId::new("c1"),
                spec: harness::AwaitSpec {
                    promise_ids: Vec::new(),
                    mode: harness::AwaitMode::All,
                    deadline_at_ms: None,
                },
            },
        };
        let mut state = session.drive.state().clone();
        let run_id = state.runs.active.as_ref().expect("active").run_id;
        assert_eq!(interrupted_run(&state), Some(run_id));

        // Waiting on its own promise, a joined workflow, or a script.
        let active = state.runs.active.as_mut().expect("active");
        active.parked_tool_batch = Some(parked);
        assert_eq!(interrupted_run(&state), None);

        // A cancel was under way: nothing is left to wait for.
        state.runs.active.as_mut().expect("active").status = RunStatus::Cancelling;
        assert_eq!(interrupted_run(&state), Some(run_id));

        state.runs.active = None;
        assert_eq!(interrupted_run(&state), None);
    }

    #[test]
    fn only_a_pending_code_tool_call_unsafe_to_repeat_is_failed() {
        let mut state = CoreAgentState::new();
        let call = |request_id: &str, tool: &str, status: CodeToolCallStatus| {
            let origin = CodeToolOrigin {
                execution_id: "scope".into(),
                request_id: request_id.into(),
            };
            harness::CodeToolCall {
                call_id: harness::code_tool_call_id(&origin),
                spec: harness::CodeToolCallSpec {
                    origin,
                    binding_id: tool.into(),
                    tool_id: ToolId::new(tool),
                    tool_name: ToolName::new(tool),
                    arguments_ref: BlobRef::from_bytes(b"{}"),
                },
                promise_id_base: 1,
                status,
            }
        };
        let mut read = function_tool("read");
        read.execution.retry_safe = true;
        state.tooling.tools.insert(read.id.clone(), read);
        let shell = function_tool("shell");
        state.tooling.tools.insert(shell.id.clone(), shell);
        let waiting = CodeToolCallStatus::Waiting {
            suspension: harness::ToolBatchSuspension::AwaitTool {
                call_id: ToolCallId::new("c"),
                spec: harness::AwaitSpec {
                    promise_ids: Vec::new(),
                    mode: harness::AwaitMode::All,
                    deadline_at_ms: None,
                },
            },
        };
        let calls = [
            call("a-safe", "read", CodeToolCallStatus::Pending),
            call("b-unsafe", "shell", CodeToolCallStatus::Pending),
            call("c-waiting", "shell", waiting),
            call("d-unknown-tool", "gone", CodeToolCallStatus::Pending),
        ];
        state.code_tools.scopes.insert(
            "scope".into(),
            harness::CodeToolScope {
                toolset_revision: 0,
                spec: harness::CodeToolScopeSpec {
                    execution_id: "scope".into(),
                    parent_invocation_id: harness::WorkflowToolInvocationId::new(format!(
                        "wti:sha256:{}",
                        "0".repeat(64)
                    )),
                    bindings: BTreeMap::new(),
                    max_calls: 10,
                    max_in_flight: 3,
                },
                closed: false,
                calls: calls
                    .into_iter()
                    .map(|call| (call.spec.origin.request_id.clone(), call))
                    .collect(),
            },
        );

        let failed = unsafe_pending_code_tool_calls(&state)
            .into_iter()
            .map(|origin| origin.request_id)
            .collect::<Vec<_>>();
        assert_eq!(failed, ["b-unsafe", "d-unknown-tool"]);
    }

    fn restarting_workflow(session: &Session) -> AgentSessionWorkflow {
        AgentSessionWorkflow {
            universe_id: Some(uuid::Uuid::from_u128(1)),
            session_id: Some(SessionId::new("s")),
            core_state: session.drive.state().clone(),
            ..Default::default()
        }
    }

    #[test]
    fn the_latest_ended_runs_notify_again_from_what_the_log_kept() {
        let mut session = Session::opened(Vec::new());
        let runs = RESENT_RUN_TERMINALS + 2;
        for index in 0..runs {
            let notify = vec![RunTerminalNotifyIntent {
                holder_workflow_id: "holder".to_owned(),
                token: format!("token-{index}"),
            }];
            session.admit(run_command(b"hi", &format!("sub-{index}"), notify));
            session.until_generation();
            session.fail_interrupted();
            assert_eq!(session.settle(), None);
        }

        let mut workflow = restarting_workflow(&session.restarted());
        workflow.queue_recent_run_terminals().expect("queue");

        assert_eq!(workflow.pending_emissions.len(), RESENT_RUN_TERMINALS);
        let records = &workflow.core_state.runs.finished;
        for (pending, record) in workflow
            .pending_emissions
            .iter()
            .zip(&records[runs - RESENT_RUN_TERMINALS..])
        {
            assert_eq!(pending.receiver_workflow_id, "holder");
            assert_eq!(pending.next_attempt_at_ms, 0);
            assert!(matches!(
                &pending.envelope.producer,
                harness::EmissionProducer::Session { log_seq, .. } if *log_seq == record.terminal_seq
            ));
            // The id is all a receiver dedupes on, and it does not depend on the seq.
            let original = EmissionEnvelope::run_terminal(
                uuid::Uuid::from_u128(1),
                SessionId::new("s"),
                harness::EventSeq::new(1),
                record.notify_on_terminal[0].token.clone(),
                record.run_id,
                record.status,
                None,
                None,
            );
            assert_eq!(pending.envelope.emission_id, original.emission_id);
        }
    }

    fn pushed_invocation(
        workflow: &mut AgentSessionWorkflow,
        dispatch: harness::BoundWorkflowToolDispatch,
        id: char,
        promise: harness::Promise,
    ) {
        let tool_id = harness::WorkflowToolId::new(format!("tool_{id}"));
        let definition = harness::WorkflowToolDefinition {
            workflow_tool_id: tool_id.clone(),
            schema_revision: 1,
            semantic_type: "test.tool.v1".to_owned(),
            tool: function_tool(&format!("tool_{id}")),
        };
        let binding = harness::WorkflowToolBinding::admit(
            uuid::Uuid::from_u128(1),
            definition,
            harness::WorkflowToolTarget::Bound {
                receiver: harness::WorkflowEndpointRef {
                    workflow_id: format!("receiver_{id}"),
                    workflow_kind: "plugin".to_owned(),
                },
                dispatch,
            },
            // A pull receiver is never waited on by the binding's own terms.
            match dispatch {
                harness::BoundWorkflowToolDispatch::Push => {
                    harness::WorkflowToolCompletion::Joined {
                        reply_schema_ref: None,
                        deadline_after_ms: Some(60_000),
                    }
                }
                harness::BoundWorkflowToolDispatch::Pull => {
                    harness::WorkflowToolCompletion::Accepted
                }
            },
        )
        .expect("binding");
        let invocation_id = harness::WorkflowToolInvocationId::new(format!(
            "wti:sha256:{}",
            String::from(id).repeat(64)
        ));
        let invocation = harness::WorkflowToolInvocation {
            invocation_id: invocation_id.clone(),
            workflow_tool_id: tool_id.clone(),
            semantic_type: "test.tool.v1".to_owned(),
            schema_revision: 1,
            binding_fingerprint: binding.binding_fingerprint.clone(),
            session_universe_id: uuid::Uuid::from_u128(1),
            session_id: SessionId::new("s"),
            run_id: RunId::new(1),
            turn_id: harness::TurnId::new(1),
            tool_batch_id: harness::ToolBatchId::new(1),
            tool_call_id: ToolCallId::new("c"),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            execution_context_ref: None,
            completion_promises: Some(BTreeMap::from([(
                harness::REPLY_COMPLETION_KEY.to_owned(),
                promise.promise_id.clone(),
            )])),
        };
        let core = &mut workflow.core_state;
        core.workflow_tools.bindings.insert(tool_id, binding);
        core.workflow_tools
            .bound_invocations
            .insert(invocation_id, invocation);
        core.promises
            .promises
            .insert(promise.promise_id.clone(), promise);
    }

    fn promise(
        id: u64,
        status: harness::PromiseStatus,
        scope: harness::PromiseScope,
    ) -> harness::Promise {
        harness::Promise {
            promise_id: harness::PromiseId::from_number(id),
            source: harness::PromiseSource::Workflow {
                producer_workflow_id: "universe/receiver".to_owned(),
                producer_workflow_kind: "plugin".to_owned(),
                invocation_id: "wti".to_owned(),
                completion_key: harness::REPLY_COMPLETION_KEY.to_owned(),
            },
            scope,
            ownership: harness::PromiseOwnership::Runtime,
            status,
            payload_ref: None,
            error_ref: None,
            deadline_at_ms: None,
        }
    }

    #[test]
    fn a_pushed_invocation_goes_out_again_only_while_something_waits_on_it() {
        use harness::{BoundWorkflowToolDispatch as Dispatch, PromiseScope, PromiseStatus};
        let mut session = Session::opened(Vec::new());
        session.request_run(b"hi", "sub-1");
        session.until_generation();
        let active = session.drive.state().runs.active.as_ref().expect("active");
        let active_run = active.run_id;
        let ended_run = RunId::new(active_run.as_u64() + 7);
        let mut workflow = restarting_workflow(&session);

        let run_scope = PromiseScope::Run { run_id: active_run };
        let cases = [
            (
                'a',
                Dispatch::Push,
                PromiseStatus::Pending,
                run_scope.clone(),
            ),
            (
                'b',
                Dispatch::Push,
                PromiseStatus::Pending,
                PromiseScope::Session,
            ),
            ('c', Dispatch::Push, PromiseStatus::Resolved, run_scope),
            (
                'd',
                Dispatch::Push,
                PromiseStatus::Pending,
                PromiseScope::Run { run_id: ended_run },
            ),
            (
                'e',
                Dispatch::Pull,
                PromiseStatus::Pending,
                PromiseScope::Session,
            ),
        ];
        for (number, (id, dispatch, status, scope)) in (1..).zip(cases) {
            let promise = promise(number, status, scope);
            pushed_invocation(&mut workflow, dispatch, id, promise);
        }

        workflow.queue_unanswered_invocations().expect("queue");

        let receivers = workflow
            .pending_emissions
            .iter()
            .map(|pending| pending.receiver_workflow_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(receivers, ["receiver_a", "receiver_b"]);
    }

    #[test]
    fn a_force_closed_session_does_not_come_back() {
        let mut session = Session::opened(Vec::new());
        session.request_run(b"hi", "sub-1");
        session.until_generation();
        let mut recovered = session.restarted();
        recovered.admit(CoreAgentCommand::CloseSession { force: true });
        assert_eq!(
            recovered.drive.state().lifecycle.status,
            CoreAgentStatus::Closed
        );
        assert!(recovered.drive.state().runs.active.is_none());
        let now = recovered.tick();
        let reopened = recovered
            .drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, now);
        assert!(reopened.is_err());
    }
}
