//! Reduced state crosses every continue-as-new, so it has to stay small however long
//! a run or a session lasts. These tests drive whole sessions through the public
//! drive API and hold the bounds, and each one replays its log to the live state.
use std::collections::BTreeMap;

use harness::session::{SessionPosition, StoredSessionEntry};
use harness::*;

/// One call per turn settles near 35 KB. The margin is for ids growing a digit.
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
        Some(OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND),
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
