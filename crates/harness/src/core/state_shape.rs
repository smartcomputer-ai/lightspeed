//! Guards on the JSON a stored checkpoint holds. The same JSON crosses every workflow
//! bootstrap, so a field it loses changes a session at its next rollover.
//!
//! [`populated_state`] names every field of every struct reachable from the state, with
//! no `..` anywhere, so a new field stops this file compiling until it has a value here.
//! That is the point: a new optional field would otherwise leave the committed copy
//! unchanged, and an old checkpoint would load with it empty.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::core::components::lifecycle::Status;
use crate::session::{SessionPosition, StoredSessionEntry};
use crate::*;

const REGENERATE_ENV: &str = "LIGHTSPEED_REGENERATE_CHECKPOINT_FIXTURE";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint_state.json")
}

fn blob(tag: &str) -> BlobRef {
    BlobRef::from_bytes(tag.as_bytes())
}

fn universe() -> Uuid {
    Uuid::from_u128(0x3f2c_1e9a_7b1d_4f55_9a8e_2c6c_1d9f_4a10)
}

fn content(tag: &str) -> ContentRef {
    ContentRef {
        content_ref: blob(tag),
        media_type: Some("text/plain".to_owned()),
        provider_kind: Some("openai.responses.message".to_owned()),
    }
}

fn token_estimate() -> TokenEstimate {
    TokenEstimate {
        tokens: 120,
        quality: TokenEstimateQuality::ProviderCounted,
    }
}

fn input_entry(tag: &str) -> ContextEntryInput {
    ContextEntryInput {
        origin: Some("channel".to_owned()),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: content(tag),
        preview: Some(tag.to_owned()),
        provenance_ref: Some(blob("provenance")),
        token_estimate: Some(token_estimate()),
    }
}

fn run_source() -> RunSource {
    RunSource::Input {
        input: vec![input_entry("run input")],
    }
}

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: "gpt-test".to_owned(),
    }
}

fn session_config() -> SessionConfig {
    SessionConfig {
        model: model(),
        generation: GenerationConfig {
            max_output_tokens: Some(4_096),
            reasoning_effort: Some("high".to_owned()),
            tool_choice: Some(ToolChoice::Specific {
                tool_id: ToolId::new("read_file"),
            }),
            parallel_tool_use: Some(true),
            processing_tier: Some(ModelProcessingTier::Fast),
        },
        limits: LimitsConfig {
            max_turns: Some(500),
        },
        context: ContextConfig {
            compaction: Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: Some(150_000),
                target_tokens: Some(60_000),
            }),
            input_limit_tokens: Some(200_000),
        },
        features: FeaturesConfig {
            vfs: Some(VfsFeature {
                version: 1,
                working_directory: Some("/work".to_owned()),
                workspaces: vec![
                    WorkspaceAttachment {
                        path: "/work".to_owned(),
                        target: WorkspaceAttachmentTarget::Workspace {
                            workspace_id: "workspace".to_owned(),
                        },
                        access: WorkspaceAccess::Edit,
                    },
                    WorkspaceAttachment {
                        path: "/snapshot".to_owned(),
                        target: WorkspaceAttachmentTarget::Snapshot {
                            snapshot_ref: blob("snapshot"),
                        },
                        access: WorkspaceAccess::Read,
                    },
                ],
                prompts: Some(VfsPromptsConfig {
                    roots: Some(vec!["/prompts".to_owned()]),
                }),
                skills: Some(VfsSkillsConfig {
                    roots: Some(vec!["/skills".to_owned()]),
                }),
            }),
            web: Some(WebFeature {
                version: 1,
                fetch: Some(WebFetchFeature {}),
                search: Some(WebSearchFeature {
                    allowed_domains: Some(vec!["example.com".to_owned()]),
                    blocked_domains: vec!["example.org".to_owned()],
                }),
            }),
            subagents: Some(SubagentsFeature {
                version: 1,
                agents: vec![SubagentAgentConfig {
                    profile_id: "researcher".to_owned(),
                }],
                limits: SubagentLimits {
                    max_depth: 2,
                    max_descendants: 8,
                    max_concurrent: 3,
                    run_timeout_ms: Some(60_000),
                },
            }),
            code_mode: Some(CodeModeFeature {
                version: 1,
                allowed_tools: Some(vec!["read_file".to_owned()]),
                limits: CodeModeLimits {
                    timeout_ms: 1,
                    max_memory_bytes: 2,
                    max_stack_bytes: 3,
                    max_source_bytes: 4,
                    max_catalog_bytes: 5,
                    max_request_bytes: 6,
                    max_result_bytes: 7,
                    max_output_bytes: 8,
                    max_tool_calls: 9,
                    max_outstanding_tool_calls: 10,
                },
            }),
            timers: Some(TimersFeature { version: 1 }),
            environments: Some(EnvironmentsFeature {
                version: 1,
                selection: true,
                prompts: Some(EnvironmentPromptsConfig {
                    roots: Some(vec!["/prompts".to_owned()]),
                }),
                skills: Some(EnvironmentSkillsConfig {
                    roots: Some(vec!["/skills".to_owned()]),
                }),
                environments: vec![EnvironmentAttachment {
                    environment_id: "env".to_owned(),
                    default: true,
                    access: EnvironmentAccess::Exec,
                    working_directory: Some("/home".to_owned()),
                }],
            }),
            mcp: Some(McpFeature {
                version: 1,
                servers: vec![McpServerAttachment {
                    server_id: "server".to_owned(),
                    tools: Some(vec!["search".to_owned()]),
                }],
            }),
        },
    }
}

fn usage() -> LlmUsage {
    LlmUsage {
        input_tokens: Some(1),
        output_tokens: Some(2),
        reasoning_tokens: Some(3),
        total_tokens: Some(4),
        cached_input_tokens: Some(5),
        cache_write_input_tokens: Some(6),
        cache_miss_input_tokens: Some(7),
    }
}

fn notify() -> RunTerminalNotifyIntent {
    RunTerminalNotifyIntent {
        holder_workflow_id: "holder".to_owned(),
        token: "token".to_owned(),
    }
}

fn observed_call(id: &str) -> ObservedToolCall {
    ObservedToolCall {
        call_id: ToolCallId::new(id),
        tool_id: Some(ToolId::new("read_file")),
        tool_name: ToolName::new("read_file"),
        provider_kind: Some("openai.responses.function_call".to_owned()),
        arguments_ref: blob("arguments"),
        native_call_ref: Some(blob("native call")),
    }
}

fn approval_subject() -> ApprovalSubject {
    ApprovalSubject::McpToolCall {
        server_id: "server".to_owned(),
        server_label: "Server".to_owned(),
        tool_name: "search".to_owned(),
        arguments_ref: blob("approval arguments"),
    }
}

fn provider_continuation() -> ApprovalContinuation {
    ApprovalContinuation::ProviderMcp {
        provider_request_id: "mcpr_1".to_owned(),
    }
}

fn approvals() -> BTreeMap<ApprovalId, ApprovalRecord> {
    [
        (provider_continuation(), Attribution::Local),
        (
            ApprovalContinuation::RuntimeMcp {
                batch_id: ToolBatchId::new(7),
                call_id: ToolCallId::new("call_pending"),
            },
            Attribution::Actor {
                id: "actor".to_owned(),
            },
        ),
        (
            provider_continuation(),
            Attribution::Key {
                prefix: "ls_abc".to_owned(),
            },
        ),
        (
            provider_continuation(),
            Attribution::Internal {
                component: "reaper".to_owned(),
                cause: "closed".to_owned(),
            },
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (continuation, decided_by))| {
        let approval_id = ApprovalId::new(format!("approval_{}", index + 1));
        let record = ApprovalRecord {
            request: ApprovalRequested {
                approval_id: approval_id.clone(),
                run_id: RunId::new(9),
                subject: approval_subject(),
                continuation,
            },
            requested_at_ms: 1_000,
            status: ApprovalStatus::Approved,
            note: Some("looks fine".to_owned()),
            decided_by: Some(decided_by),
        };
        (approval_id, record)
    })
    .collect()
}

fn turn_state(turn_id: u64, outcome: TurnOutcome) -> (TurnId, TurnState) {
    let turn_id = TurnId::new(turn_id);
    let state = TurnState {
        turn_id,
        run_id: RunId::new(9),
        status: TurnStatus::Completed,
        planned_request: Some(PlannedRequestState {
            request_fingerprint: "fingerprint".to_owned(),
            config_revision: 1,
            context_revision: 2,
            toolset_revision: 3,
        }),
        generation_status: Some(LlmGenerationStatus::Succeeded),
        facts: Some(LlmGenerationFacts {
            provider_response_id: Some("resp_1".to_owned()),
            finish: LlmFinish::ToolCalls,
            usage: Some(usage()),
            duration_ms: Some(900),
            tool_calls: vec![observed_call("call_observed")],
            approval_requests: vec![ObservedApprovalRequest {
                subject: approval_subject(),
                continuation: provider_continuation(),
            }],
            context_token_estimate: Some(token_estimate()),
        }),
        outcome: Some(outcome),
    };
    (turn_id, state)
}

fn attachments() -> Vec<Attachment> {
    vec![
        Attachment::Media(media::MediaDescriptor {
            handle: "media:0123456789ab".to_owned(),
            content_ref: blob("image"),
            media_type: "image/png".to_owned(),
            kind: media::MediaKind::Image,
            name: Some("chart.png".to_owned()),
        }),
        Attachment::File(FileAttachment {
            handle: "file:0123456789ab".to_owned(),
            content_ref: blob("file"),
            name: "notes.md".to_owned(),
            media_type: Some("text/markdown".to_owned()),
            source: Some(AttachmentSource {
                kind: "environment".to_owned(),
                id: "env".to_owned(),
                path: "/home/notes.md".to_owned(),
            }),
        }),
    ]
}

fn tool_result(call_id: &str) -> ToolCallResult {
    ToolCallResult {
        call_id: ToolCallId::new(call_id),
        status: ToolCallStatus::Succeeded,
        output_ref: Some(blob("output")),
        model_visible_context_entries: vec![input_entry("tool output")],
        error_ref: Some(blob("error")),
        effects: vec![ToolEffect {
            kind: "effect".to_owned(),
            data: BTreeMap::from([("key".to_owned(), "value".to_owned())]),
            refs: BTreeMap::from([("ref".to_owned(), blob("effect"))]),
        }],
        attachments: attachments(),
        duration_ms: Some(12),
        output_bytes: Some(34),
        truncated: true,
    }
}

fn await_spec() -> AwaitSpec {
    AwaitSpec {
        promise_ids: vec![PromiseId::from_number(1)],
        mode: AwaitMode::Any,
        deadline_at_ms: Some(9_000),
    }
}

fn active_run() -> ActiveRun {
    let batch_id = ToolBatchId::new(7);
    let finished_batch_id = ToolBatchId::new(6);
    ActiveRun {
        context_recovery: ContextRecoveryState {
            attempts: 1,
            recovery_attempted_turn_id: Some(TurnId::new(10)),
        },
        run_id: RunId::new(9),
        status: RunStatus::Parked,
        submission_id: Some(SubmissionId::new("submission-active")),
        source: run_source(),
        input_entry_ids: vec![ContextEntryId::new(1)],
        input_consumed_by_turn_id: Some(TurnId::new(10)),
        run_config: RunConfig {
            max_turns: Some(10),
            model_override: Some(model()),
            max_output_tokens: Some(100),
            reasoning_effort: Some("low".to_owned()),
            processing_tier: Some(ModelProcessingTier::Flex),
            tool_choice: Some(ToolChoice::RequiredAny),
            parallel_tool_use: Some(false),
        },
        input_limit_tokens: Some(200_000),
        config_revision: 3,
        first_seq: EventSeq::new(40),
        accepted_at_ms: 1_000,
        started_at_ms: Some(1_001),
        usage: Some(usage()),
        steering: vec![SteeringBatch {
            steering_id: SteeringId::new(2),
            submission_id: SubmissionId::new("submission-steer"),
            input: vec![input_entry("steer")],
            entry_ids: vec![ContextEntryId::new(2)],
            consumed_by_turn_id: Some(TurnId::new(11)),
        }],
        turns: BTreeMap::from([
            turn_state(
                10,
                TurnOutcome::FinalOutput {
                    output: Some(content("final")),
                },
            ),
            turn_state(11, TurnOutcome::ToolCallsQueued),
            turn_state(
                12,
                TurnOutcome::ContextLimit {
                    failure_ref: Some(blob("limit")),
                },
            ),
            turn_state(13, TurnOutcome::ApprovalsRequested),
            turn_state(
                14,
                TurnOutcome::Failed {
                    failure_ref: Some(blob("failed")),
                },
            ),
            turn_state(
                15,
                TurnOutcome::Rejected {
                    failure_ref: Some(blob("rejected")),
                },
            ),
            turn_state(16, TurnOutcome::Cancelled),
        ]),
        turn_count: 7,
        entry_guard_attempted: true,
        active_turn_id: Some(TurnId::new(16)),
        active_tool_batch_id: Some(batch_id),
        approvals: approvals(),
        parked_tool_batch: Some(ParkedToolBatch {
            batch_id,
            suspension: ToolBatchSuspension::AwaitTool {
                call_id: ToolCallId::new("call_pending"),
                spec: await_spec(),
            },
        }),
        tool_batches: BTreeMap::from([(
            batch_id,
            ActiveToolBatch {
                batch_id,
                run_id: RunId::new(9),
                turn_id: TurnId::new(16),
                calls: vec![ToolCallState {
                    call: observed_call("call_pending"),
                    status: ToolCallStatus::Pending,
                    result: Some(tool_result("call_pending")),
                }],
                promise_id_base: 1,
            },
        )]),
        completed_tool_batches: BTreeMap::from([(
            finished_batch_id,
            CompletedToolBatch {
                batch_id: finished_batch_id,
                run_id: RunId::new(9),
                turn_id: TurnId::new(15),
                results: vec![tool_result("call_finished")],
            },
        )]),
        notify_on_terminal: vec![notify()],
    }
}

fn accepted_run() -> AcceptedRun {
    AcceptedRun {
        run_id: RunId::new(10),
        submission_id: Some(SubmissionId::new("submission-queued")),
        source: run_source(),
        run_config: RunConfig::default(),
        input_limit_tokens: Some(200_000),
        config_revision: 3,
        first_seq: EventSeq::new(80),
        accepted_at_ms: 2_000,
        notify_on_terminal: vec![notify()],
    }
}

fn run_record() -> RunRecord {
    RunRecord {
        run_id: RunId::new(8),
        status: RunStatus::Failed,
        submission_id: Some(SubmissionId::new("submission-finished")),
        submission_digest: 77,
        source: run_source(),
        first_seq: EventSeq::new(20),
        terminal_seq: EventSeq::new(39),
        accepted_at_ms: 500,
        started_at_ms: Some(501),
        finished_at_ms: 600,
        usage: Some(usage()),
        output: Some(content("run output")),
        failure: Some(RunFailure {
            kind: RunFailureKind::ModelFailure,
            message_ref: Some(blob("failure message")),
        }),
        notify_on_terminal: vec![notify()],
    }
}

fn context_entries() -> Vec<ContextEntry> {
    let run_id = RunId::new(9);
    let turn_id = TurnId::new(10);
    let call_id = ToolCallId::new("call_finished");
    let kinds_and_sources = [
        (
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            },
            ContextEntrySource::AssistantOutput { run_id, turn_id },
        ),
        (
            ContextEntryKind::Instructions,
            ContextEntrySource::ContextEdit,
        ),
        (
            ContextEntryKind::Catalog {
                title: "Skills".to_owned(),
            },
            ContextEntrySource::Runtime {
                label: "catalog".to_owned(),
            },
        ),
        (
            ContextEntryKind::ToolCall {
                call_id: call_id.clone(),
                name: ToolName::new("read_file"),
            },
            ContextEntrySource::RunInput {
                run_id,
                input_index: 0,
            },
        ),
        (
            ContextEntryKind::ToolResult {
                call_id,
                is_error: true,
            },
            ContextEntrySource::Tool {
                run_id,
                turn_id,
                batch_id: Some(ToolBatchId::new(6)),
            },
        ),
        (
            ContextEntryKind::ReasoningState,
            ContextEntrySource::Reasoning { run_id, turn_id },
        ),
        (
            ContextEntryKind::ProviderOpaque,
            ContextEntrySource::Steering {
                run_id,
                steering_id: SteeringId::new(2),
                input_index: 1,
            },
        ),
        (
            ContextEntryKind::McpApprovalResponse {
                approval_request_id: "mcpr_1".to_owned(),
                approve: true,
            },
            ContextEntrySource::ApprovalDecision {
                run_id,
                approval_id: ApprovalId::new("approval_1"),
            },
        ),
    ];
    kinds_and_sources
        .into_iter()
        .enumerate()
        .map(|(index, (kind, source))| ContextEntry {
            origin: Some("channel".to_owned()),
            entry_id: ContextEntryId::new(index as u64 + 1),
            key: Some(ContextEntryKey::new(format!("key.{index}"))),
            kind,
            source,
            content: content("entry"),
            preview: Some("entry".to_owned()),
            provenance_ref: Some(blob("provenance")),
            token_estimate: Some(token_estimate()),
            supersedes: Some(ContextEntryId::new(100)),
        })
        .collect()
}

fn context_state() -> ContextState {
    ContextState {
        revision: 12,
        entries: context_entries(),
        compaction: ContextCompactionState {
            phase: ContextCompactionPhase::Pending(ContextCompactionPlan {
                run_id: Some(RunId::new(9)),
                covered_entry_ids: vec![ContextEntryId::new(1)],
                trigger: ContextCompactionTrigger::HighWatermark,
                estimated_tokens: Some(180_000),
            }),
            last_finished_revision: Some(10),
            last_manual_outcome: Some(ManualCompactionOutcome {
                revision: 9,
                status: ContextCompactionStatus::Failed,
                failure_ref: Some(blob("compaction failure")),
            }),
            failure_floor: Some(CompactionFailureFloor {
                tokens: Some(170_000),
                entries: 90,
            }),
        },
        last_generation: Some(ContextGenerationMetadata {
            model: model(),
            input_limit_tokens: Some(200_000),
        }),
        usage_observation: Some(ContextUsageObservation {
            context_revision: 12,
            tokens: 90_000,
        }),
    }
}

fn tool_spec(id: &str, kind: ToolKind) -> ToolSpec {
    ToolSpec {
        id: ToolId::new(id),
        kind,
        parallelism: ToolParallelism::ParallelSafe,
        execution: ToolExecutionSpec {
            class: ToolExecutionClass::Process,
            retry_safe: true,
        },
    }
}

fn function_tool(id: &str) -> ToolSpec {
    tool_spec(
        id,
        ToolKind::Function(FunctionToolSpec {
            description_ref: Some(blob("description")),
            input_schema_ref: blob("input schema"),
            output_schema_ref: Some(blob("output schema")),
            strict: Some(true),
            provider_options_ref: Some(blob("provider options")),
        }),
    )
}

fn tooling_state() -> ToolingState {
    let tools = [
        function_tool("read_file"),
        tool_spec(
            "agent_run",
            ToolKind::Builtin(BuiltinToolSpec {
                settings: json!({ "setting": true }),
            }),
        ),
        tool_spec(
            "web_search",
            ToolKind::ProviderNative(ProviderNativeToolSpec {
                api_kind: ProviderApiKind::OpenAiResponses,
                native_tool_ref: blob("native tool"),
                execution: ProviderNativeToolExecution::ProviderHosted,
            }),
        ),
        tool_spec(
            "mcp_server",
            ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "server".to_owned(),
                record_revision: 4,
                server_label: "Server".to_owned(),
                description_ref: Some(blob("server description")),
                allowed_tools: Some(vec!["search".to_owned()]),
                execution: RemoteMcpExecution::Runtime,
                exposure: RemoteMcpExposure::Search,
                approval: RemoteMcpApprovalPolicy::Always,
                defer_loading: Some(true),
            }),
        ),
    ];
    ToolingState {
        revision: 5,
        tools: tools
            .into_iter()
            .map(|tool| (tool.id.clone(), tool))
            .collect(),
    }
}

fn promise(number: u64, source: PromiseSource, scope: PromiseScope) -> (PromiseId, Promise) {
    let promise_id = PromiseId::from_number(number);
    let promise = Promise {
        promise_id: promise_id.clone(),
        source,
        scope,
        ownership: PromiseOwnership::Model,
        status: PromiseStatus::Resolved,
        payload_ref: Some(blob("payload")),
        error_ref: Some(blob("promise error")),
        deadline_at_ms: Some(9_000),
    };
    (promise_id, promise)
}

fn promises_state() -> PromiseComponentState {
    PromiseComponentState {
        promises: BTreeMap::from([
            promise(
                1,
                PromiseSource::Timer { fire_at_ms: 9_000 },
                PromiseScope::Run {
                    run_id: RunId::new(9),
                },
            ),
            promise(
                2,
                PromiseSource::Workflow {
                    producer_workflow_id: "producer".to_owned(),
                    producer_workflow_kind: "ProducerWorkflow".to_owned(),
                    invocation_id: "invocation".to_owned(),
                    completion_key: "reply".to_owned(),
                },
                PromiseScope::Session,
            ),
        ]),
        settled: vec![PromiseId::from_number(2)],
    }
}

fn endpoint() -> WorkflowEndpointRef {
    WorkflowEndpointRef {
        workflow_id: "controller".to_owned(),
        workflow_kind: "ControllerWorkflow".to_owned(),
    }
}

fn binding(
    id: &str,
    target: WorkflowToolTarget,
    completion: WorkflowToolCompletion,
) -> (WorkflowToolId, WorkflowToolBinding) {
    let workflow_tool_id = WorkflowToolId::new(id);
    let binding = WorkflowToolBinding {
        session_universe_id: universe(),
        definition: WorkflowToolDefinition {
            workflow_tool_id: workflow_tool_id.clone(),
            schema_revision: 1,
            semantic_type: "semantic".to_owned(),
            tool: function_tool("workflow_tool"),
        },
        target,
        completion,
        binding_fingerprint: "fingerprint".to_owned(),
    };
    (workflow_tool_id, binding)
}

fn promises_completion(key_source: WorkflowToolCompletionKeySource) -> WorkflowToolCompletion {
    WorkflowToolCompletion::Promises {
        reply_schema_ref: Some(blob("reply schema")),
        deadline_after_ms: Some(60_000),
        max_promises: 4,
        key_source,
    }
}

fn workflow_tool_bindings() -> BTreeMap<WorkflowToolId, WorkflowToolBinding> {
    let bound = || WorkflowToolTarget::Bound {
        receiver: endpoint(),
        dispatch: BoundWorkflowToolDispatch::Push,
    };
    BTreeMap::from([
        binding("accepted", bound(), WorkflowToolCompletion::Accepted),
        binding(
            "joined",
            WorkflowToolTarget::Start {
                start: WorkflowStartRef {
                    recipe_format: 1,
                    recipe_revision: 2,
                    recipe_ref: blob("recipe"),
                    recipe_fingerprint: "recipe fingerprint".to_owned(),
                },
            },
            WorkflowToolCompletion::Joined {
                reply_schema_ref: Some(blob("reply schema")),
                deadline_after_ms: Some(60_000),
            },
        ),
        binding(
            "keyed-reply",
            bound(),
            promises_completion(WorkflowToolCompletionKeySource::Reply),
        ),
        binding(
            "keyed-strings",
            bound(),
            promises_completion(WorkflowToolCompletionKeySource::StringArray {
                pointer: "/ids".to_owned(),
            }),
        ),
        binding(
            "keyed-fields",
            bound(),
            promises_completion(WorkflowToolCompletionKeySource::ArrayItemField {
                pointer: "/items".to_owned(),
                field: "id".to_owned(),
            }),
        ),
        binding(
            "keyed-indices",
            bound(),
            promises_completion(WorkflowToolCompletionKeySource::ArrayIndices {
                pointer: "/items".to_owned(),
                prefix: "item-".to_owned(),
            }),
        ),
    ])
}

fn invocation_id(number: u64) -> WorkflowToolInvocationId {
    WorkflowToolInvocationId::new(format!("wti:sha256:{number:064x}"))
}

fn invocation(number: u64) -> WorkflowToolInvocation {
    WorkflowToolInvocation {
        invocation_id: invocation_id(number),
        workflow_tool_id: WorkflowToolId::new("joined"),
        semantic_type: "semantic".to_owned(),
        schema_revision: 1,
        binding_fingerprint: "fingerprint".to_owned(),
        session_universe_id: universe(),
        session_id: SessionId::new("session"),
        run_id: RunId::new(9),
        turn_id: TurnId::new(16),
        tool_batch_id: ToolBatchId::new(7),
        tool_call_id: ToolCallId::new("call_pending"),
        arguments_ref: blob("arguments"),
        execution_context_ref: Some(blob("execution context")),
        completion_promises: Some(BTreeMap::from([(
            "reply".to_owned(),
            PromiseId::from_number(2),
        )])),
    }
}

fn workflow_tool_state() -> WorkflowToolState {
    WorkflowToolState {
        session_universe_id: Some(universe()),
        lifecycle_controller: Some(endpoint()),
        declaration_version: Some(1),
        declared_creation_fingerprint: Some("declared".to_owned()),
        bindings: workflow_tool_bindings(),
        system_binding_ids: BTreeSet::from([WorkflowToolId::new("accepted")]),
        bound_invocations: BTreeMap::from([(invocation_id(1), invocation(1))]),
        delivery_failures: BTreeMap::from([(invocation_id(1), blob("delivery failure"))]),
        start_requests: BTreeMap::from([(invocation_id(2), invocation(2))]),
        start_failures: BTreeMap::from([(invocation_id(2), blob("start failure"))]),
    }
}

fn code_call(request_id: &str, status: CodeToolCallStatus) -> (String, CodeToolCall) {
    let call = CodeToolCall {
        spec: CodeToolCallSpec {
            origin: CodeToolOrigin {
                execution_id: "execution".to_owned(),
                request_id: request_id.to_owned(),
            },
            binding_id: "read".to_owned(),
            tool_id: ToolId::new("read_file"),
            tool_name: ToolName::new("read_file"),
            arguments_ref: blob("code arguments"),
        },
        call_id: ToolCallId::new(format!("code_{request_id}")),
        promise_id_base: 3,
        status,
    };
    (request_id.to_owned(), call)
}

fn code_tool_state() -> CodeToolState {
    let calls = BTreeMap::from([
        code_call("pending", CodeToolCallStatus::Pending),
        code_call(
            "waiting",
            CodeToolCallStatus::Waiting {
                suspension: ToolBatchSuspension::JoinedWorkflowCalls {
                    calls: vec![JoinedWorkflowCall {
                        call_id: ToolCallId::new("code_waiting"),
                        invocation_id: invocation_id(1),
                        promise_id: PromiseId::from_number(2),
                    }],
                    spec: await_spec(),
                },
            },
        ),
        code_call(
            "completed",
            CodeToolCallStatus::Completed {
                result: CodeToolCallResult {
                    call_id: ToolCallId::new("code_completed"),
                    status: ToolCallStatus::Failed,
                    output_ref: Some(blob("code output")),
                    error_ref: Some(blob("code error")),
                    attachments: attachments(),
                    duration_ms: Some(12),
                    output_bytes: Some(34),
                    truncated: true,
                },
            },
        ),
    ]);
    CodeToolState {
        scopes: BTreeMap::from([(
            "execution".to_owned(),
            CodeToolScope {
                toolset_revision: 5,
                spec: CodeToolScopeSpec {
                    execution_id: "execution".to_owned(),
                    parent_invocation_id: invocation_id(1),
                    bindings: BTreeMap::from([(
                        "read".to_owned(),
                        CodeToolBinding {
                            tool_id: ToolId::new("read_file"),
                            tool_name: ToolName::new("read_file"),
                        },
                    )]),
                    max_calls: 10,
                    max_in_flight: 2,
                },
                closed: true,
                calls,
            },
        )]),
    }
}

/// States no real session reaches at once, such as a run that is parked with a pending
/// compaction: this exists to hold a value in every field, not to reduce.
fn populated_state() -> CoreAgentState {
    CoreAgentState {
        reduced_to: Some(SessionPosition {
            seq: EventSeq::new(90),
        }),
        id_cursors: IdCursors {
            last_run_id: 10,
            last_steering_id: 2,
            last_turn_id: 16,
            last_tool_batch_id: 7,
            last_context_entry_id: 8,
            last_promise_id: 2,
            last_approval_id: 4,
        },
        lifecycle: LifecycleState {
            status: Status::Open,
            config: Some(session_config()),
            config_revision: 3,
        },
        runs: RunsState {
            active: Some(active_run()),
            queued: vec![accepted_run()],
            finished: vec![run_record()],
            pruned_through: Some(RunId::new(2)),
        },
        context: context_state(),
        environment: EnvironmentState {
            active_environment_id: Some(EnvironmentId::new("env")),
        },
        tooling: tooling_state(),
        promises: promises_state(),
        workflow_tools: workflow_tool_state(),
        code_tools: code_tool_state(),
    }
}

/// Every place two JSON values differ, so a failure names the fields and not two walls of
/// text.
fn differences(path: String, committed: &Value, current: &Value, found: &mut Vec<String>) {
    match (committed, current) {
        (Value::Object(committed), Value::Object(current)) => {
            let keys: BTreeSet<_> = committed.keys().chain(current.keys()).collect();
            for key in keys {
                match (committed.get(key), current.get(key)) {
                    (Some(left), Some(right)) => {
                        differences(format!("{path}.{key}"), left, right, found);
                    }
                    (Some(_), None) => found.push(format!("{path}.{key}: removed")),
                    (None, _) => found.push(format!("{path}.{key}: added")),
                }
            }
        }
        (Value::Array(committed), Value::Array(current)) if committed.len() == current.len() => {
            for (index, (left, right)) in committed.iter().zip(current).enumerate() {
                differences(format!("{path}[{index}]"), left, right, found);
            }
        }
        _ if committed != current => found.push(format!("{path}: {committed} -> {current}")),
        _ => {}
    }
}

#[test]
fn stored_state_keeps_its_committed_shape() {
    let state = populated_state();
    let current = json!({
        "format_version": SESSION_CHECKPOINT_FORMAT_VERSION,
        "state": serde_json::to_value(&state).unwrap(),
    });
    let path = fixture_path();
    if std::env::var_os(REGENERATE_ENV).is_some() {
        let text = serde_json::to_string_pretty(&current).unwrap();
        std::fs::write(&path, text + "\n").unwrap();
    }
    let committed: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "read {}: {error}; run with {REGENERATE_ENV}=1",
                path.display()
            )
        }))
        .unwrap();

    let mut found = Vec::new();
    differences(String::new(), &committed, &current, &mut found);
    assert!(
        found.is_empty(),
        "the JSON a stored checkpoint holds changed:\n  {}\n\n\
         If an old checkpoint would now load with a value replay would set differently, bump \
         SESSION_CHECKPOINT_FORMAT_VERSION in core/components/state.rs: the reducer replays the \
         log instead, once, on each session's next load. Then, bumped or not, regenerate the \
         committed copy:\n  {REGENERATE_ENV}=1 cargo test -p harness --lib \
         stored_state_keeps_its_committed_shape",
        found.join("\n  "),
    );
    let decoded: CoreAgentState = serde_json::from_value(committed["state"].clone()).unwrap();
    assert_eq!(decoded, state, "the committed state decodes to itself");
}

/// A payload recorded before a field existed still decodes: the state and its components
/// fall back to their defaults, and a session record needs only its identities.
#[test]
fn state_and_session_record_decode_from_almost_nothing() {
    let state: CoreAgentState = serde_json::from_value(json!({})).unwrap();
    assert_eq!(state, CoreAgentState::new());
    let state: CoreAgentState = serde_json::from_value(json!({
        "id_cursors": {},
        "lifecycle": {},
        "runs": {},
        "context": {},
        "environment": {},
        "tooling": {},
        "promises": {},
        "workflow_tools": {},
        "code_tools": {},
    }))
    .unwrap();
    assert_eq!(state, CoreAgentState::new());

    let record: crate::storage::SessionRecord = serde_json::from_value(json!({
        "session_id": "session",
        "retention_root_session_id": "session",
    }))
    .unwrap();
    assert_eq!(record.session_id, SessionId::new("session"));
    assert!(!record.lifecycle_controlled);
}

fn commit(drive: &mut CoreAgentDrive, action: CoreAgentAction) {
    let CoreAgentAction::AppendEvents {
        expected_head,
        events,
    } = action
    else {
        panic!("expected an append, got {action:?}");
    };
    let mut seq = expected_head.map_or(0, |head| head.seq.as_u64());
    let entries = events
        .into_iter()
        .map(|event| {
            seq += 1;
            StoredSessionEntry {
                position: SessionPosition {
                    seq: EventSeq::new(seq),
                },
                observed_at_ms: event.observed_at_ms,
                joins: event.joins,
                event: event.event,
            }
        })
        .collect();
    drive.resume_appended(entries).unwrap();
}

/// The reducer's own round-trip check runs on every event of every test here, and only
/// sees what a test reaches. A holder waiting on a run lives in this one field, and a
/// checkpoint that lost it would never tell the holder the run ended.
#[test]
fn a_terminal_notification_survives_a_checkpoint_while_the_run_is_active() {
    let mut drive = CoreAgentDrive::from_replayed(SessionId::new("notify"), CoreAgentState::new());
    let action = drive
        .admit_command(
            CoreAgentCommand::OpenSession {
                config: SessionConfig {
                    model: model(),
                    generation: Default::default(),
                    limits: Default::default(),
                    context: Default::default(),
                    features: Default::default(),
                },
            },
            1,
        )
        .unwrap();
    commit(&mut drive, action);
    let action = drive
        .admit_command(
            CoreAgentCommand::RequestRun(RunRequestCommand {
                requested_by: None,
                notify_on_terminal: vec![notify()],
                submission_id: None,
                source: RunRequestSource::Input {
                    input: vec![input_entry("hello")],
                },
                run_config: RunConfig::default(),
                input_limit_tokens: None,
            }),
            2,
        )
        .unwrap();
    commit(&mut drive, action);
    while let action @ CoreAgentAction::AppendEvents { .. } =
        drive.next_action_unbounded(3).unwrap()
    {
        commit(&mut drive, action);
    }

    let notify_on_terminal = &drive
        .state()
        .runs
        .active
        .as_ref()
        .unwrap()
        .notify_on_terminal;
    assert_eq!(notify_on_terminal, &vec![notify()]);
}
