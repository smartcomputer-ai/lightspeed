//! Reduced state crosses every continue-as-new, so it has to stay small however long
//! a run or a session lasts. These tests drive whole sessions through the public
//! drive API and hold the bounds, and each one replays its log to the live state.
use std::collections::BTreeMap;

use harness::session::{SessionPosition, StoredSessionEntry};
use harness::*;

/// One call per turn settles near 24 KB with these turns, and near 35 KB when each also
/// keeps a reasoning entry. What matters is that it stops growing.
const ONE_CALL_PER_TURN_STATE_BYTES: usize = 45_000;

struct Session {
    drive: CoreAgentDrive,
    log: Vec<StoredSessionEntry>,
    now: u64,
}

impl Session {
    fn open() -> Self {
        let mut session = Self {
            drive: CoreAgentDrive::from_replayed(
                SessionId::new("3f2c1e9a-7b1d-4f55-9a8e-2c6c1d9f4a10"),
                CoreAgentState::new(),
            ),
            log: Vec::new(),
            now: 1_780_000_000_000,
        };
        session.admit(CoreAgentCommand::OpenSession { config: config() });
        session.admit(CoreAgentCommand::ReplaceTools {
            expected_revision: None,
            tools: BTreeMap::from([(ToolName::new("read_file"), function_tool("read_file"))]),
        });
        session
    }

    fn tick(&mut self) -> u64 {
        self.now += 7;
        self.now
    }

    fn commit(&mut self, action: CoreAgentAction) {
        let CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } = action
        else {
            panic!("expected an append, got {action:?}");
        };
        let mut head = expected_head;
        let entries = events
            .into_iter()
            .map(|event| {
                let seq = head.as_ref().map_or(1, |p| p.seq.as_u64() + 1);
                let position = SessionPosition {
                    seq: EventSeq::new(seq),
                };
                head = Some(position.clone());
                StoredSessionEntry {
                    position,
                    observed_at_ms: event.observed_at_ms,
                    joins: event.joins,
                    event: event.event,
                }
            })
            .collect::<Vec<_>>();
        self.log.extend(entries.iter().cloned());
        self.drive.resume_appended(entries).expect("resume");
    }

    fn admit(&mut self, command: CoreAgentCommand) {
        let now = self.tick();
        match self.drive.admit_command(command, now).expect("admit") {
            action @ CoreAgentAction::AppendEvents { .. } => self.commit(action),
            CoreAgentAction::Idle | CoreAgentAction::Closed => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Everything the planner wants appended, up to the first thing it needs done.
    fn pump(&mut self) -> CoreAgentAction {
        loop {
            let now = self.tick();
            match self.drive.next_action_unbounded(now).expect("next action") {
                action @ CoreAgentAction::AppendEvents { .. } => self.commit(action),
                other => return other,
            }
        }
    }

    fn state(&self) -> &CoreAgentState {
        self.drive.state()
    }

    fn active_run(&self) -> &ActiveRun {
        self.state().runs.active.as_ref().expect("active run")
    }

    fn state_bytes(&self) -> usize {
        serde_json::to_vec(self.state()).expect("state").len()
    }

    /// Whatever pruning did live, the log has to reproduce.
    fn assert_replays(&self) {
        let mut replayed = CoreAgentState::new();
        for entry in &self.log {
            let decoded = CoreAgentCodec.decode_entry(entry).expect("decode");
            apply_event(&mut replayed, &decoded).expect("replay");
        }
        assert_eq!(&replayed, self.state());
    }

    fn generate(&mut self, result: LlmGenerationResult) {
        let now = self.tick();
        let action = self.drive.resume_generation(result, now).expect("generate");
        self.commit(action);
    }

    fn compact(&mut self, request: &ContextCompactionRequest) {
        let now = self.tick();
        let action = self
            .drive
            .resume_context_compaction(compaction_result(request), now)
            .expect("compact");
        self.commit(action);
    }

    /// One chat run, start to finish.
    fn chat_run(&mut self, index: u64) {
        self.admit(request_run(index));
        loop {
            match self.pump() {
                CoreAgentAction::GenerateLlm { request } => self.generate(final_turn(&request)),
                CoreAgentAction::CompactContext { request } => self.compact(&request),
                CoreAgentAction::Idle => return,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// One run of `turns` tool turns with a compaction every 25, stopping with the
    /// next generation asked for. `inspect` looks at the session after each turn.
    fn tool_turns(
        &mut self,
        turns: u64,
        mut inspect: impl FnMut(&Self, u64),
    ) -> LlmGenerationRequest {
        let mut done = 0;
        loop {
            match self.pump() {
                CoreAgentAction::GenerateLlm { request } => {
                    if done == turns {
                        return request;
                    }
                    done += 1;
                    self.generate(tool_turn(&request));
                    if done % 25 == 0 {
                        self.admit(CoreAgentCommand::CompactContext);
                    }
                    inspect(self, done);
                }
                CoreAgentAction::CompactContext { request } => self.compact(&request),
                CoreAgentAction::InvokeTools { request } => {
                    let now = self.tick();
                    let action = self
                        .drive
                        .resume_tool_batch(tool_results(&request), now)
                        .expect("tools");
                    self.commit(action);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

fn blob(tag: impl AsRef<[u8]>) -> BlobRef {
    BlobRef::from_bytes(tag.as_ref())
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
        context: ContextConfig {
            reported_input_limit_tokens: None,
            input_limit_tokens: Some(200_000),
            compaction: Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: None,
                target_tokens: None,
            }),
        },
        features: Default::default(),
    }
}

fn function_tool(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        execution: Default::default(),
        kind: ToolKind::Function(FunctionToolSpec {
            description_ref: None,
            input_schema_ref: blob(br#"{"type":"object"}"#),
            output_schema_ref: None,
            strict: None,
            provider_options_ref: None,
        }),
        parallelism: ToolParallelism::ParallelSafe,
    }
}

fn usage() -> LlmUsage {
    LlmUsage {
        input_tokens: Some(48_211),
        output_tokens: Some(912),
        reasoning_tokens: Some(640),
        total_tokens: Some(49_123),
        cached_input_tokens: Some(41_000),
        cache_write_input_tokens: None,
        cache_miss_input_tokens: Some(7_211),
    }
}

fn message(
    role: ContextMessageRole,
    tag: String,
    provider_kind: Option<&str>,
) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message { role },
        content: ContentRef {
            content_ref: blob(tag),
            media_type: Some("text/plain".to_owned()),
            provider_kind: provider_kind.map(str::to_owned),
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }
}

fn request_run(index: u64) -> CoreAgentCommand {
    CoreAgentCommand::RequestRun(RunRequestCommand {
        requested_by: None,
        notify_on_terminal: Vec::new(),
        submission_id: Some(submission(index)),
        source: RunRequestSource::Input {
            input: vec![message(
                ContextMessageRole::User,
                format!("user-{index}"),
                None,
            )],
        },
        run_config: RunConfig::default(),
    })
}

fn submission(index: u64) -> SubmissionId {
    SubmissionId::new(format!("bd6f1b2c-1b1e-4a7e-9b1a-{index:012}"))
}

fn facts(finish: LlmFinish) -> LlmGenerationFacts {
    LlmGenerationFacts {
        provider_response_id: Some("resp_0123456789abcdef".to_owned()),
        finish,
        usage: Some(usage()),
        duration_ms: Some(8_123),
        tool_calls: Vec::new(),
        approval_requests: Vec::new(),
        context_token_estimate: Some(TokenEstimate {
            tokens: 48_000,
            quality: TokenEstimateQuality::ProviderCounted,
        }),
    }
}

fn generation(request: &LlmGenerationRequest, facts: LlmGenerationFacts) -> LlmGenerationResult {
    LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status: LlmGenerationStatus::Succeeded,
        failure_ref: None,
        context_entries: Vec::new(),
        facts,
    }
}

fn final_turn(request: &LlmGenerationRequest) -> LlmGenerationResult {
    let mut result = generation(request, facts(LlmFinish::Stop));
    result.context_entries.push(message(
        ContextMessageRole::Assistant,
        format!("assistant-{}", request.turn_id),
        Some("openai.responses.message"),
    ));
    result
}

fn call_of(request: &LlmGenerationRequest, tool: &str) -> ObservedToolCall {
    let call = format!("call_{:020}", request.turn_id.as_u64());
    ObservedToolCall {
        call_id: ToolCallId::new(&call),
        tool_id: Some(ToolName::new(tool)),
        tool_name: ToolName::new(tool),
        provider_kind: Some("openai.responses.function_call".to_owned()),
        arguments_ref: blob(format!("args-{call}")),
        native_call_ref: Some(blob(format!("native-{call}"))),
    }
}

/// A turn whose only output is one call of `tool`.
fn call_turn(request: &LlmGenerationRequest, tool: &str) -> LlmGenerationResult {
    let call = call_of(request, tool);
    let mut facts = facts(LlmFinish::ToolCalls);
    facts.tool_calls = vec![call.clone()];
    let mut result = generation(request, facts);
    result.context_entries.push(ContextEntryInput {
        kind: ContextEntryKind::ToolCall {
            call_id: call.call_id.clone(),
            name: call.tool_name.clone(),
        },
        content: ContentRef {
            content_ref: blob(call.call_id.as_str()),
            media_type: Some("application/json".to_owned()),
            provider_kind: call.provider_kind.clone(),
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    });
    result
}

fn tool_turn(request: &LlmGenerationRequest) -> LlmGenerationResult {
    call_turn(request, "read_file")
}

fn tool_results(request: &ToolInvocationBatchRequest) -> ToolInvocationBatchResult {
    ToolInvocationBatchResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        batch_id: request.batch_id,
        results: request
            .calls
            .iter()
            .map(|call| {
                let output = blob(format!("output-{}", call.call_id));
                ToolInvocationResult {
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(output.clone()),
                    model_visible_context_entries: vec![
                        ToolInvocationResult::tool_result_context_entry(
                            &call.call_id,
                            ToolCallStatus::Succeeded,
                            output,
                        ),
                    ],
                    error_ref: None,
                    effects: Vec::new(),
                    attachments: Vec::new(),
                    duration_ms: Some(42),
                    output_bytes: Some(4_096),
                    truncated: false,
                }
            })
            .collect(),
    }
}

fn compaction_result(request: &ContextCompactionRequest) -> ContextCompactionResult {
    ContextCompactionResult {
        usage: Some(usage()),
        calls: 1,
        session_id: request.session_id.clone(),
        context_revision: request.request.context.context_revision,
        status: ContextCompactionStatus::Succeeded,
        failure_ref: None,
        context_entries: vec![message(
            ContextMessageRole::User,
            format!("summary-{}", request.request.context.context_revision),
            None,
        )],
    }
}

#[test]
fn a_run_of_thousands_of_turns_stays_small_and_replays() {
    let mut session = Session::open();
    session.admit(request_run(1));
    session.tool_turns(5_000, |session, turn| {
        if turn % 500 == 0 {
            let bytes = session.state_bytes();
            assert!(
                bytes < ONE_CALL_PER_TURN_STATE_BYTES,
                "{bytes} B at turn {turn}"
            );
        }
    });
    let run = session.active_run();
    assert_eq!(run.turn_count, 5_001);
    assert_eq!(run.turns.len(), 2, "the settled turn and the one starting");
    assert_eq!(run.completed_tool_batches.len(), 1);
    session.assert_replays();
}

#[test]
fn a_long_run_still_finishes_with_what_its_last_turn_produced() {
    let mut session = Session::open();
    session.admit(request_run(1));
    let request = session.tool_turns(300, |_, _| {});
    session.generate(final_turn(&request));
    assert!(matches!(session.pump(), CoreAgentAction::Idle));
    let record = session.state().runs.completed.last().expect("record");
    assert_eq!(record.status, RunStatus::Completed);
    assert_eq!(
        record.output.as_ref().map(|output| &output.content_ref),
        Some(&blob(format!("assistant-{}", request.turn_id)))
    );
    session.assert_replays();
}

#[test]
fn a_session_of_many_runs_keeps_a_window_of_records_and_recognizes_retries_within_it() {
    let mut session = Session::open();
    let runs = MAX_RETAINED_TERMINAL_RUNS as u64 + 44;
    for index in 1..=runs {
        session.chat_run(index);
    }
    let state = session.state();
    assert_eq!(state.runs.completed.len(), MAX_RETAINED_TERMINAL_RUNS);
    assert_eq!(state.runs.pruned_through, Some(RunId::new(44)));
    assert!(state.runs.is_pruned(RunId::new(44)));
    assert!(!state.runs.is_pruned(RunId::new(45)));
    session.assert_replays();

    // Within the window a retry is the same run. Past it, it is a new one.
    session.admit(request_run(45));
    assert_eq!(session.state().id_cursors.last_run_id, runs);
    session.admit(request_run(44));
    assert_eq!(session.state().id_cursors.last_run_id, runs + 1);
}

#[test]
fn a_queued_run_cancelled_behind_an_active_one_never_loses_the_newest_record() {
    let mut session = Session::open();
    // Run 1 holds the session while every later run is cancelled in the queue, so run
    // ids in the window end up out of order.
    session.admit(request_run(1));
    let CoreAgentAction::GenerateLlm { request } = session.pump() else {
        panic!("expected a generation");
    };
    let queued = MAX_RETAINED_TERMINAL_RUNS as u64 + 3;
    for index in 2..=queued {
        session.admit(request_run(index));
        session.admit(CoreAgentCommand::CancelRun {
            run_id: RunId::new(index),
            requested_by: None,
        });
    }
    assert_eq!(session.state().runs.pruned_through, Some(RunId::new(3)));

    session.generate(final_turn(&request));
    assert!(matches!(session.pump(), CoreAgentAction::Idle));
    let state = session.state();
    assert_eq!(state.runs.completed.len(), MAX_RETAINED_TERMINAL_RUNS);
    assert_eq!(state.runs.completed.last().unwrap().run_id, RunId::new(1));
    assert_eq!(state.runs.pruned_through, Some(RunId::new(4)));
    assert!(!state.runs.is_pruned(RunId::new(1)));
    assert!(state.runs.is_pruned(RunId::new(2)));
    session.assert_replays();
}

/// A decision is answered by the context entry it adds, so its record has no more
/// to do once the next turn has run. The approval still waited on is another matter.
#[test]
fn provider_approvals_across_many_turns_leave_only_what_is_waited_on() {
    let mut session = Session::open();
    session.admit(request_run(1));
    let mut sizes = Vec::new();
    for turn in 1..=500u64 {
        let CoreAgentAction::GenerateLlm { request } = session.pump() else {
            panic!("expected a generation at turn {turn}");
        };
        let provider_request_id = format!("mcpr_{turn}");
        let mut facts = facts(LlmFinish::Stop);
        facts.approval_requests = vec![ObservedApprovalRequest {
            subject: ApprovalSubject::McpToolCall {
                server_id: "mail".to_owned(),
                server_label: "mail".to_owned(),
                tool_name: "send".to_owned(),
                arguments_ref: blob(format!("args-{turn}")),
            },
            continuation: ApprovalContinuation::OpenAiMcp {
                provider_request_id: provider_request_id.clone(),
            },
        }];
        session.generate(generation(&request, facts));
        assert!(matches!(session.pump(), CoreAgentAction::Idle), "parked");

        let run = session.active_run();
        assert_eq!(run.status, RunStatus::Parked);
        assert_eq!(run.approvals.len(), 1, "older decisions are gone");
        let approval_id = run
            .pending_approvals()
            .next()
            .unwrap()
            .request
            .approval_id
            .clone();
        let run_id = run.run_id;
        session.admit(CoreAgentCommand::DecideApproval(ApprovalDecisionCommand {
            approval_id,
            run_id,
            decision: ApprovalDecision::Approved,
            note: None,
            decided_by: None,
            response: Some(ContextEntryInput {
                kind: ContextEntryKind::McpApprovalResponse {
                    approval_request_id: provider_request_id,
                    approve: true,
                },
                content: ContentRef {
                    content_ref: blob(format!("response-{turn}")),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("openai.responses.mcp_approval_response".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }),
        }));
        if turn % 25 == 0 {
            session.admit(CoreAgentCommand::CompactContext);
            let CoreAgentAction::CompactContext { request } = session.pump() else {
                panic!("expected a compaction");
            };
            session.compact(&request);
        }
        if turn % 100 == 0 {
            sizes.push(session.state_bytes());
        }
    }
    assert!(
        sizes
            .iter()
            .all(|bytes| *bytes < ONE_CALL_PER_TURN_STATE_BYTES),
        "{sizes:?}"
    );
    session.assert_replays();
}

#[test]
fn a_decision_after_its_record_is_pruned_finds_nothing_to_decide() {
    let mut session = Session::open();
    session.admit(request_run(1));
    let CoreAgentAction::GenerateLlm { request } = session.pump() else {
        panic!("expected a generation");
    };
    let mut approval_facts = facts(LlmFinish::Stop);
    approval_facts.approval_requests = vec![ObservedApprovalRequest {
        subject: ApprovalSubject::McpToolCall {
            server_id: "mail".to_owned(),
            server_label: "mail".to_owned(),
            tool_name: "send".to_owned(),
            arguments_ref: blob("args"),
        },
        continuation: ApprovalContinuation::OpenAiMcp {
            provider_request_id: "mcpr_1".to_owned(),
        },
    }];
    session.generate(generation(&request, approval_facts));
    assert!(matches!(session.pump(), CoreAgentAction::Idle));
    let approval_id = session
        .active_run()
        .pending_approvals()
        .next()
        .unwrap()
        .request
        .approval_id
        .clone();
    let decide = |approval_id: ApprovalId| {
        CoreAgentCommand::DecideApproval(ApprovalDecisionCommand {
            approval_id,
            run_id: RunId::new(1),
            decision: ApprovalDecision::Approved,
            note: None,
            decided_by: None,
            response: Some(ContextEntryInput {
                kind: ContextEntryKind::McpApprovalResponse {
                    approval_request_id: "mcpr_1".to_owned(),
                    approve: true,
                },
                content: ContentRef {
                    content_ref: blob("response"),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("openai.responses.mcp_approval_response".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }),
        })
    };
    session.admit(decide(approval_id.clone()));
    let CoreAgentAction::GenerateLlm { request } = session.pump() else {
        panic!("expected the next turn");
    };
    session.generate(tool_turn(&request));

    let now = session.tick();
    let refused = session
        .drive
        .admit_command(decide(approval_id), now)
        .expect_err("the record is gone");
    let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = refused else {
        panic!("expected a rejection, got {refused:?}");
    };
    assert_eq!(rejection.kind, CommandRejectionKind::UnknownReference);
}

#[test]
fn steering_keeps_a_window_and_never_drops_what_has_not_reached_a_turn() {
    let steer = |run_id, index: u64| CoreAgentCommand::RequestRunSteering {
        run_id,
        input: vec![message(
            ContextMessageRole::User,
            format!("steer-{index}"),
            None,
        )],
        requested_by: None,
    };
    let mut session = Session::open();
    session.admit(request_run(1));
    let run_id = RunId::new(1);
    let window = MAX_RETAINED_STEERING_BATCHES as u64;
    let mut index = 0;
    session.tool_turns(window * 2, |_, _| {});
    // Steering lands before each turn, and the window fills with consumed batches.
    for _ in 0..window * 2 {
        index += 1;
        session.admit(steer(run_id, index));
        let CoreAgentAction::GenerateLlm { request } = session.pump() else {
            panic!("expected a generation");
        };
        session.generate(tool_turn(&request));
        let CoreAgentAction::InvokeTools { request } = session.pump() else {
            panic!("expected a tool batch");
        };
        let now = session.tick();
        let action = session
            .drive
            .resume_tool_batch(tool_results(&request), now)
            .expect("tools");
        session.commit(action);
    }
    let run = session.active_run();
    assert_eq!(run.steering.len(), MAX_RETAINED_STEERING_BATCHES);
    assert_eq!(
        run.steering.first().unwrap().steering_id.as_u64(),
        window + 1,
        "the oldest consumed batches went first"
    );
    // The count stays level from here on, so a caller waiting on a steer it sent reads
    // the newest id.
    assert_eq!(run.latest_steering_id().unwrap().as_u64(), index);

    // A batch accepted but not yet carried by a turn outlives any window.
    let CoreAgentAction::GenerateLlm { request } = session.pump() else {
        panic!("expected a generation");
    };
    let unconsumed = window as usize + 8;
    for _ in 0..unconsumed {
        index += 1;
        session.admit(steer(run_id, index));
    }
    session.generate(tool_turn(&request));
    let run = session.active_run();
    assert!(
        run.steering
            .iter()
            .filter(|steering| steering.consumed_by_turn_id.is_none())
            .count()
            == unconsumed
    );
    session.assert_replays();
}

const UNIVERSE: u128 = 7;
const CODE_TOOL: &str = "code_execute";
const NOTIFY_TOOL: &str = "notify";
const SUBMIT_TOOL: &str = "job_submit";

fn system_tool(
    name: &str,
    target: WorkflowToolTarget,
    completion: WorkflowToolCompletion,
) -> CoreAgentCommand {
    CoreAgentCommand::AdmitSystemWorkflowTool {
        session_universe_id: uuid::Uuid::from_u128(UNIVERSE),
        declaration: WorkflowToolDeclaration::new(
            WorkflowToolDefinition {
                tool_id: WorkflowToolId::new(name),
                revision: 1,
                semantic_type: format!("test.{name}.v1"),
                tool: function_tool(name),
            },
            target,
            completion,
        ),
    }
}

fn start_target(name: &str) -> WorkflowToolTarget {
    WorkflowToolTarget::Start {
        start: WorkflowStartRef {
            recipe_format: 1,
            revision: 1,
            recipe_ref: blob(name),
            recipe_fingerprint: format!("wtr:sha256:{name}"),
        },
    }
}

/// A session with one workflow tool of each completion shape.
fn open_with_workflow_tools() -> Session {
    let mut session = Session::open();
    session.admit(system_tool(
        CODE_TOOL,
        start_target(CODE_TOOL),
        WorkflowToolCompletion::Joined {
            reply_schema_ref: None,
            deadline_after_ms: 660_000,
        },
    ));
    session.admit(system_tool(
        NOTIFY_TOOL,
        WorkflowToolTarget::Bound {
            receiver: WorkflowEndpointRef {
                workflow_id: "receiver".to_owned(),
                workflow_kind: "test.receiver".to_owned(),
            },
            dispatch: BoundWorkflowToolDispatch::Push,
        },
        WorkflowToolCompletion::Accepted,
    ));
    session.admit(system_tool(
        SUBMIT_TOOL,
        start_target(SUBMIT_TOOL),
        WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms: None,
            max_promises: 1,
            key_source: WorkflowToolCompletionKeySource::Reply,
        },
    ));
    session.admit(CoreAgentCommand::ReplaceTools {
        expected_revision: Some(1),
        tools: [CODE_TOOL, NOTIFY_TOOL, SUBMIT_TOOL, "read_file"]
            .into_iter()
            .map(|name| (ToolName::new(name), function_tool(name)))
            .collect(),
    });
    session
}

/// What the worker returns for a successful workflow tool call of this batch.
fn invoked(
    request: &ToolInvocationBatchRequest,
    now: u64,
) -> (
    ToolInvocationBatchResult,
    WorkflowToolInvocationId,
    PromiseId,
) {
    let call = &request.calls[0];
    let binding = &call.workflow_tool.as_ref().expect("workflow tool").binding;
    let invocation_id = WorkflowToolInvocationId::for_call(
        binding.session_universe_id,
        &request.session_id,
        request.run_id,
        request.turn_id,
        request.batch_id,
        &call.call_id,
        &binding.binding_fingerprint,
    );
    let promise_id = PromiseId::from_number(request.promise_id_base);
    let invocation = WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: binding.definition.tool_id.clone(),
        semantic_type: binding.definition.semantic_type.clone(),
        schema_revision: binding.definition.revision,
        binding_fingerprint: binding.binding_fingerprint.clone(),
        session_universe_id: binding.session_universe_id,
        session_id: request.session_id.clone(),
        run_id: request.run_id,
        turn_id: request.turn_id,
        tool_batch_id: request.batch_id,
        tool_call_id: call.call_id.clone(),
        arguments_ref: call.arguments_ref.clone(),
        execution_context_ref: None,
        completion_promises: binding
            .completion
            .is_promise_bearing()
            .then(|| BTreeMap::from([(REPLY_COMPLETION_KEY.to_owned(), promise_id.clone())])),
    };
    let mut effect = workflow_tool_emit_effect(&invocation);
    if matches!(binding.completion, WorkflowToolCompletion::Joined { .. }) {
        effect = with_completion_deadline(effect, Some(now + 1_000_000_000));
    }
    let mut results = tool_results(request);
    results.results[0].effects = vec![effect];
    if binding.completion.exposes_model_owned_promises() {
        results.results[0].output_ref = Some(blob(promise_id.as_str()));
    }
    (results, invocation_id, promise_id)
}

impl Session {
    fn next_batch(&mut self) -> ToolInvocationBatchRequest {
        match self.pump() {
            CoreAgentAction::InvokeTools { request } => request,
            other => panic!("expected a tool batch, got {other:?}"),
        }
    }

    fn next_generation(&mut self) -> LlmGenerationRequest {
        loop {
            match self.pump() {
                CoreAgentAction::GenerateLlm { request } => return request,
                CoreAgentAction::CompactContext { request } => self.compact(&request),
                other => panic!("expected a generation, got {other:?}"),
            }
        }
    }

    fn answer(&mut self, results: ToolInvocationBatchResult) {
        let now = self.tick();
        let action = self.drive.resume_tool_batch(results, now).expect("results");
        self.commit(action);
    }

    /// Starts a joined call and leaves the run parked on its reply.
    fn start_joined_call(
        &mut self,
    ) -> (
        ToolInvocationBatchRequest,
        WorkflowToolInvocationId,
        PromiseId,
    ) {
        let request = self.next_generation();
        self.generate(call_turn(&request, CODE_TOOL));
        let request = self.next_batch();
        let (results, invocation_id, promise_id) = invoked(&request, self.now);
        self.answer(results);
        assert!(matches!(self.pump(), CoreAgentAction::Idle), "parked");
        (request, invocation_id, promise_id)
    }

    fn resolve(&mut self, promise_id: &PromiseId) {
        self.admit(CoreAgentCommand::ResolvePromise {
            promise_id: promise_id.clone(),
            resolution: PromiseResolution::Resolved {
                payload_ref: Some(blob("done")),
            },
        });
    }

    fn resume_joined(&mut self, request: &ToolInvocationBatchRequest) {
        let now = self.tick();
        self.admit(CoreAgentCommand::ResumeToolBatch(ResumeToolBatchCommand {
            run_id: request.run_id,
            batch_id: request.batch_id,
            claim: WakeReason::Terminal,
            claim_observed_at_ms: now,
            output: ToolBatchResumeOutput::JoinedWorkflowCalls {
                additional_context: Vec::new(),
            },
        }));
    }

    fn open_scope(&mut self, execution_id: &str, parent: WorkflowToolInvocationId) {
        self.admit(CoreAgentCommand::OpenCodeToolScope {
            scope: CodeToolScopeSpec {
                execution_id: execution_id.to_owned(),
                parent_invocation_id: parent,
                bindings: BTreeMap::from([(
                    "read_file".to_owned(),
                    CodeToolBinding {
                        tool_id: ToolName::new("read_file"),
                        tool_name: ToolName::new("read_file"),
                    },
                )]),
                max_calls: 16,
                max_in_flight: 8,
            },
        });
    }

    /// One tool call made from the script of this scope.
    fn script_call(&mut self, execution_id: &str) {
        let origin = CodeToolOrigin {
            execution_id: execution_id.to_owned(),
            request_id: "1".to_owned(),
        };
        self.admit(CoreAgentCommand::AdmitCodeToolCall {
            call: CodeToolCallSpec {
                origin: origin.clone(),
                binding_id: "read_file".to_owned(),
                tool_id: ToolName::new("read_file"),
                tool_name: ToolName::new("read_file"),
                arguments_ref: blob("{}"),
            },
        });
        let request =
            code_tool_request(self.drive.session_id(), self.state(), execution_id, "1").unwrap();
        let result = tool_results(&request).results.remove(0);
        self.admit(CoreAgentCommand::CompleteCodeToolCall { origin, result });
    }

    fn close_scope(&mut self, execution_id: &str) {
        self.admit(CoreAgentCommand::CloseCodeToolScope {
            execution_id: execution_id.to_owned(),
        });
    }

    /// One whole `code_execute`: a joined call, a scope, a call from the script, the
    /// scope closing, the reply and the batch resuming.
    fn code_execute_turn(&mut self, turn: u64) -> WorkflowToolInvocationId {
        let (request, invocation_id, promise_id) = self.start_joined_call();
        let execution_id = format!("execution-{turn}");
        self.open_scope(&execution_id, invocation_id.clone());
        self.script_call(&execution_id);
        self.close_scope(&execution_id);
        self.resolve(&promise_id);
        self.resume_joined(&request);
        invocation_id
    }

    fn finish_with_final_turn(&mut self) {
        let request = self.next_generation();
        self.generate(final_turn(&request));
        assert!(matches!(self.pump(), CoreAgentAction::Idle));
    }
}

#[test]
fn a_code_mode_run_of_thousands_of_turns_stays_small_and_replays() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    for turn in 1..=5_000u64 {
        session.code_execute_turn(turn);
        if turn % 25 == 0 {
            session.admit(CoreAgentCommand::CompactContext);
        }
        if turn % 500 == 0 {
            let state = session.state();
            assert!(state.workflow_tools.start_requests.len() <= 1);
            assert!(state.code_tools.scopes.len() <= 1);
            let bytes = session.state_bytes();
            // The one-call figure plus a full window of terminal promises, at about
            // 520 B each.
            assert!(bytes < 200_000, "{bytes} B at turn {turn}");
        }
    }
    session.assert_replays();
}

#[test]
fn a_run_only_counts_invocations_still_in_flight() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    for turn in 1..=(MAX_IN_FLIGHT_WORKFLOW_TOOL_INVOCATIONS as u64 + 8) {
        let request = session.next_generation();
        session.generate(call_turn(&request, CODE_TOOL));
        let request = session.next_batch();
        let runtime = request.calls[0].workflow_tool.as_ref().unwrap();
        assert_eq!(runtime.in_flight_invocation_count, 0, "turn {turn}");
        let (results, _, promise_id) = invoked(&request, session.now);
        session.answer(results);
        session.resolve(&promise_id);
        session.resume_joined(&request);
    }
    session.finish_with_final_turn();
    assert_eq!(
        session.state().runs.completed.last().unwrap().status,
        RunStatus::Completed
    );
    session.assert_replays();
}

/// Promise-bearing calls left pending while the run moves on, then the next one sees
/// how many there are.
#[test]
fn a_pending_invocation_counts_and_stays_until_its_promise_settles() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    let mut promises = Vec::new();
    let mut invocations = Vec::new();
    for pending in 0..3 {
        let request = session.next_generation();
        session.generate(call_turn(&request, SUBMIT_TOOL));
        let request = session.next_batch();
        let runtime = request.calls[0].workflow_tool.as_ref().unwrap();
        assert_eq!(runtime.in_flight_invocation_count, pending);
        let (results, invocation_id, promise_id) = invoked(&request, session.now);
        session.answer(results);
        invocations.push(invocation_id);
        promises.push(promise_id);
    }
    let held = |session: &Session, id: &WorkflowToolInvocationId| {
        session
            .state()
            .workflow_tools
            .start_requests
            .contains_key(id)
    };
    assert!(invocations.iter().all(|id| held(&session, id)));

    // Settling is not the boundary, because the host reads the record right after it.
    session.resolve(&promises[0]);
    assert!(held(&session, &invocations[0]));
    let request = session.next_generation();
    session.generate(tool_turn(&request));
    let request = session.next_batch();
    assert_eq!(request.calls[0].workflow_tool, None);
    session.answer(tool_results(&request));
    assert!(!held(&session, &invocations[0]), "gone at the boundary");
    assert!(held(&session, &invocations[1]) && held(&session, &invocations[2]));

    // A failure report for what has gone is no longer anybody's concern.
    let head = session.log.len();
    for command in [
        CoreAgentCommand::FailWorkflowToolStart {
            invocation_id: invocations[0].clone(),
            error_ref: blob("late"),
        },
        CoreAgentCommand::FailWorkflowToolDelivery {
            invocation_id: invocations[0].clone(),
            error_ref: blob("late"),
        },
    ] {
        session.admit(command);
    }
    assert_eq!(session.log.len(), head, "no events");
    session.assert_replays();
}

#[test]
fn a_notify_invocation_is_finished_as_soon_as_its_batch_is() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    for _ in 0..(MAX_IN_FLIGHT_WORKFLOW_TOOL_INVOCATIONS + 2) {
        let request = session.next_generation();
        session.generate(call_turn(&request, NOTIFY_TOOL));
        let request = session.next_batch();
        let (results, invocation_id, _) = invoked(&request, session.now);
        session.answer(results);
        assert!(
            session
                .state()
                .workflow_tools
                .emissions
                .contains_key(&invocation_id)
        );
        let request = session.next_generation();
        assert!(
            !session
                .state()
                .workflow_tools
                .emissions
                .contains_key(&invocation_id)
        );
        session.generate(tool_turn(&request));
        let request = session.next_batch();
        session.answer(tool_results(&request));
    }
    session.assert_replays();
}

/// A scope stops its parent going while open. Once it closes, they go together, so
/// no scope in state ever lacks its parent.
#[test]
fn an_open_scope_holds_its_parent_and_a_closed_one_goes_with_it() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    let (request, parent, promise_id) = session.start_joined_call();
    session.open_scope("execution", parent.clone());
    session.resolve(&promise_id);
    session.resume_joined(&request);

    // The batch has completed and its reply is in, but the runner has not closed.
    let request = session.next_generation();
    let state = session.state();
    assert!(state.workflow_tools.start_requests.contains_key(&parent));
    assert!(code_tool_parent(state, &state.code_tools.scopes["execution"].spec).is_ok());

    session.generate(tool_turn(&request));
    let request = session.next_batch();
    session.close_scope("execution");
    session.answer(tool_results(&request));
    session.next_generation();
    let state = session.state();
    assert!(!state.workflow_tools.start_requests.contains_key(&parent));
    assert!(!state.code_tools.scopes.contains_key("execution"));

    // The runner closing again finds nothing, and that is fine.
    let head = session.log.len();
    session.close_scope("execution");
    assert_eq!(session.log.len(), head, "no events");
    session.assert_replays();
}

impl Session {
    /// One tool turn whose call mints a timer promise and leaves it pending.
    fn timer_turn(&mut self) -> PromiseId {
        let request = self.next_generation();
        self.generate(tool_turn(&request));
        let request = self.next_batch();
        let promise_id = PromiseId::from_number(request.promise_id_base);
        let mut results = tool_results(&request);
        results.results[0].effects = vec![promise_create_effect(
            &promise_id,
            &PromiseSource::Timer {
                fire_at_ms: u64::MAX,
            },
            None,
        )];
        self.answer(results);
        promise_id
    }

    /// One tool turn that does nothing else, so a pruning boundary passes.
    fn boundary_turn(&mut self) {
        let request = self.next_generation();
        self.generate(tool_turn(&request));
        let request = self.next_batch();
        self.answer(tool_results(&request));
    }
}

#[test]
fn terminal_promises_keep_a_window_and_old_ids_become_harmless() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    // One long pending promise from the very start.
    let survivor = session.timer_turn();

    let extra = 40;
    let mut first_settled = None;
    for _ in 0..(MAX_RETAINED_TERMINAL_PROMISES + extra) {
        let promise_id = session.timer_turn();
        first_settled.get_or_insert(promise_id.clone());
        session.resolve(&promise_id);
    }
    // The last settled promise waits for the next boundary.
    session.boundary_turn();

    let state = session.state();
    let promises = &state.promises.promises;
    assert_eq!(
        promises.values().filter(|p| p.status.is_terminal()).count(),
        MAX_RETAINED_TERMINAL_PROMISES
    );
    assert_eq!(promises[&survivor].status, PromiseStatus::Pending);
    let first_settled = first_settled.unwrap();
    assert!(!promises.contains_key(&first_settled));
    assert!(
        state.id_cursors.last_promise_id >= (MAX_RETAINED_TERMINAL_PROMISES + extra) as u64,
        "the cursor counts every promise ever minted"
    );

    // A late resolution of a pruned id is a no-op. A never minted one is a mistake.
    let head = session.log.len();
    session.resolve(&first_settled);
    assert_eq!(session.log.len(), head, "no events");
    let now = session.tick();
    let never_minted = session.drive.admit_command(
        CoreAgentCommand::ResolvePromise {
            promise_id: PromiseId::from_number(state_cursor(&session) + 1),
            resolution: PromiseResolution::Cancelled,
        },
        now,
    );
    assert!(matches!(
        never_minted,
        Err(CoreAgentDriveError::Command(CommandError::Rejected(rejection)))
            if rejection.kind == CommandRejectionKind::UnknownReference
    ));

    // A model cancelling a pruned id is told nothing went wrong by the reducer.
    let request = session.next_generation();
    session.generate(tool_turn(&request));
    let request = session.next_batch();
    let mut results = tool_results(&request);
    results.results[0].effects = vec![promise_cancel_effect(&first_settled)];
    session.answer(results);
    session.assert_replays();
}

/// A job submitted early can settle long after hundreds of newer promises did, and the
/// model has not read it yet, so the window counts from when a promise settled.
#[test]
fn a_promise_settling_late_stays_awaitable_for_a_whole_window() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    let long_job = session.timer_turn();
    for _ in 0..MAX_RETAINED_TERMINAL_PROMISES + 8 {
        let promise_id = session.timer_turn();
        session.resolve(&promise_id);
    }
    session.resolve(&long_job);
    session.boundary_turn();
    assert_eq!(
        session.state().promises.promises[&long_job].status,
        PromiseStatus::Resolved
    );

    for _ in 0..MAX_RETAINED_TERMINAL_PROMISES {
        let promise_id = session.timer_turn();
        session.resolve(&promise_id);
    }
    session.boundary_turn();
    let state = session.state();
    assert!(!state.promises.promises.contains_key(&long_job));
    assert_eq!(state.promises.settled.len(), MAX_RETAINED_TERMINAL_PROMISES);
    session.assert_replays();
}

fn state_cursor(session: &Session) -> u64 {
    session.state().id_cursors.last_promise_id
}

/// Force recovery in the middle of a script: the outcome of the unfinished call is
/// recorded and readable, because the parent's own promise is only cancelled in the
/// same append, after the terminal event, and pruning waits for the next boundary.
#[test]
fn a_force_cancelled_script_still_reports_its_interrupted_call() {
    let mut session = open_with_workflow_tools();
    session.admit(request_run(1));
    let (_, parent, promise_id) = session.start_joined_call();
    session.open_scope("execution", parent.clone());
    let origin = CodeToolOrigin {
        execution_id: "execution".to_owned(),
        request_id: "1".to_owned(),
    };
    session.admit(CoreAgentCommand::AdmitCodeToolCall {
        call: CodeToolCallSpec {
            origin: origin.clone(),
            binding_id: "read_file".to_owned(),
            tool_id: ToolName::new("read_file"),
            tool_name: ToolName::new("read_file"),
            arguments_ref: blob("{}"),
        },
    });
    session.admit(CoreAgentCommand::ForceCancelRun {
        run_id: RunId::new(1),
    });
    assert!(matches!(session.pump(), CoreAgentAction::Idle));

    let state = session.state();
    assert_eq!(
        state.promises.promises[&promise_id].status,
        PromiseStatus::Cancelled
    );
    assert!(state.workflow_tools.start_requests.contains_key(&parent));
    let call = code_tool_call(state, &origin).expect("still readable");
    assert!(call.status.is_terminal());

    // The next run's first boundary clears it.
    session.admit(request_run(2));
    session.finish_with_final_turn();
    let state = session.state();
    assert!(!state.workflow_tools.start_requests.contains_key(&parent));
    assert!(code_tool_call(state, &origin).is_none());
    session.assert_replays();
}

/// Pruning only moves where a record is read from, so what the log rebuilds has to be
/// the record state kept, for every way a run can end.
#[test]
fn a_record_rebuilt_from_the_log_is_the_one_state_kept() {
    let mut session = open_with_workflow_tools();
    // Completes after some tool turns.
    session.admit(request_run(1));
    let request = session.tool_turns(20, |_, _| {});
    session.generate(final_turn(&request));
    assert!(matches!(session.pump(), CoreAgentAction::Idle));
    // Fails on the provider.
    session.admit(request_run(2));
    let request = session.next_generation();
    let mut failed = generation(&request, facts(LlmFinish::Stop));
    failed.status = LlmGenerationStatus::Failed;
    failed.failure_ref = Some(blob("provider down"));
    session.generate(failed);
    assert!(matches!(session.pump(), CoreAgentAction::Idle));
    // Is cancelled while it runs, then in the queue behind it, then forced.
    session.admit(request_run(3));
    session.next_generation();
    session.admit(request_run(4));
    session.admit(CoreAgentCommand::CancelRun {
        run_id: RunId::new(4),
        requested_by: None,
    });
    session.admit(CoreAgentCommand::ForceCancelRun {
        run_id: RunId::new(3),
    });
    assert!(matches!(session.pump(), CoreAgentAction::Idle));

    let state = session.state();
    assert_eq!(state.runs.completed.len(), 4);
    let decoded = session
        .log
        .iter()
        .map(|entry| CoreAgentCodec.decode_entry(entry).expect("decode"))
        .collect::<Vec<_>>();
    for kept in &state.runs.completed {
        let entries = decoded
            .iter()
            .filter(|entry| entry.joins.run_id == Some(kept.run_id))
            .cloned()
            .collect::<Vec<_>>();
        let rebuilt = run_record_from_entries(&entries).expect("terminal");
        assert_eq!(
            rebuilt,
            RunRecord {
                usage: None,
                ..kept.clone()
            }
        );
        assert_eq!(
            run_usage_from_entries(&entries),
            kept.usage,
            "{}",
            kept.run_id
        );
    }
}
