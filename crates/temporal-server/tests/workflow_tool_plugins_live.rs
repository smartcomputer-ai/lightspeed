//! P100b B5-B7 live proof: workflow plugins run in their own Temporal worker
//! and are reached only through data — an opaque bound endpoint or a
//! CAS-backed start recipe. The session worker registers no plugin workflow
//! type (`worker_with_activities` is used unchanged); the plugin workflows
//! below are registered on a second worker with its own task queue.
//!
//! Run serially against the local stack:
//!
//! ```bash
//! source dev/env.sh
//! cargo test -p temporal-server --test workflow_tool_plugins_live -- --ignored --test-threads=1
//! ```

mod support;

use std::{sync::Arc, time::Duration};

use api::{
    AgentApiService, ContextEntryKindView, ContextMessageRoleView, InputItem, RunStartParams,
    RunStartSource, RunTerminalNotificationInput, SessionReadParams,
};
use async_trait::async_trait;
use engine::{
    BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError,
    CoreAgentLlm, CoreAgentTools, EmissionBody, EmissionEnvelope, EmissionProducer,
    FunctionToolSpec, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ObservedToolCall, PromiseResolution, SessionId, ToolCallId, ToolKind,
    ToolName, ToolParallelism, ToolSpec, WorkflowEndpointRef, WorkflowStartRef,
    WorkflowToolCompletion, WorkflowToolDeclaration, WorkflowToolDefinition, WorkflowToolTarget,
    storage::BlobStore,
};
use support::live::{
    LIVE_TEST_LOCK, final_assistant_text, live_universe_id, live_workflow_handle,
    require_storage_live_env, wait_for_terminal_run,
};
use temporal_server::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, SessionTools, WorkerActivities, core_runtime, worker_with_activities},
};
use temporal_workflow::{
    AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET,
    WORKFLOW_TOOL_RECOVERY_QUERY, WorkflowToolRecipeV1, WorkflowToolRecoveryResult,
    WorkflowToolStartArgs, connect_temporal, workflow_tool_recipe_fingerprint,
};
use temporalio_client::{Client, WorkflowStartOptions, WorkflowTerminateOptions};
use temporalio_common::worker::WorkerTaskTypes;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, Worker, WorkerOptions, WorkflowContext,
    WorkflowContextView, WorkflowResult,
};
use tools::concurrency::AWAIT_TOOL_NAME;

const REQUEST_APPROVAL_TOOL: &str = "request_approval";
const LAUNCH_JOB_TOOL: &str = "launch_job";
const MESSAGE_SEND_TOOL: &str = "message_send";

// ---------------------------------------------------------------------------
// Minimal generic plugin workflows (registered ONLY on the plugin worker).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoundPluginArgs {
    universe_id: uuid::Uuid,
    /// When false, the plugin records invocations but never replies — used
    /// to prove producer authorization by letting the test inject forged
    /// and authorized resolutions itself.
    auto_reply: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoundPluginSnapshot {
    invocations: u32,
    duplicate_invocations: u32,
    replies_sent: u32,
    cancellations: Vec<String>,
}

/// Bound request/reply receiver: consumes pushed `ToolInvocation` envelopes
/// through the fixed `deliver_emission` signal and resolves each keyed
/// completion promise with a `SourceResolution` emission back to the
/// producing session — sent twice to prove duplicate delivery is a no-op.
#[workflow(name = "TestBoundPluginWorkflow")]
#[derive(Default)]
pub struct TestBoundPluginWorkflow {
    inbox: Vec<EmissionEnvelope>,
    seen_invocations: std::collections::BTreeSet<String>,
    snapshot: BoundPluginSnapshot,
}

#[workflow_methods]
impl TestBoundPluginWorkflow {
    #[run]
    pub async fn run(ctx: &mut WorkflowContext<Self>, args: BoundPluginArgs) -> WorkflowResult<()> {
        loop {
            ctx.wait_condition(|state| !state.inbox.is_empty()).await;
            let envelopes = ctx.state_mut(|state| std::mem::take(&mut state.inbox));
            for envelope in envelopes {
                match envelope.body {
                    EmissionBody::ToolInvocation { invocation } => {
                        // Receiver dedup is invocation-id keying of the
                        // plugin's own state: a duplicate push is a no-op.
                        let fresh = ctx.state_mut(|state| {
                            let fresh = state
                                .seen_invocations
                                .insert(invocation.invocation_id.as_str().to_owned());
                            if fresh {
                                state.snapshot.invocations += 1;
                            } else {
                                state.snapshot.duplicate_invocations += 1;
                            }
                            fresh
                        });
                        if !fresh || !args.auto_reply {
                            continue;
                        }
                        let EmissionProducer::Session { session_id, .. } = &envelope.producer
                        else {
                            continue;
                        };
                        let holder =
                            temporal_workflow::compose_workflow_id(args.universe_id, session_id);
                        for (_key, promise_id) in invocation.completion_promises.iter().flatten() {
                            let reply = EmissionEnvelope::source_resolution(
                                args.universe_id,
                                ctx.workflow_id().to_owned(),
                                promise_id.clone(),
                                PromiseResolution::Resolved { payload_ref: None },
                            );
                            // Deliver twice: the second copy must be an
                            // end-to-end no-op at the holder.
                            for _ in 0..2 {
                                let _ = ctx
                                    .external_workflow(holder.clone(), None)
                                    .signal(AgentSessionWorkflow::deliver_emission, reply.clone())
                                    .await;
                                ctx.state_mut(|state| state.snapshot.replies_sent += 1);
                            }
                        }
                    }
                    EmissionBody::InvocationCancellation {
                        invocation_id,
                        completion_key,
                        ..
                    } => {
                        ctx.state_mut(|state| {
                            state
                                .snapshot
                                .cancellations
                                .push(format!("{invocation_id}:{completion_key}"));
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        envelope: EmissionEnvelope,
    ) {
        self.inbox.push(envelope);
    }

    #[query(name = "plugin_snapshot")]
    pub fn plugin_snapshot(&self, _ctx: &WorkflowContextView) -> BoundPluginSnapshot {
        self.snapshot.clone()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelfReceiverControllerSnapshot {
    /// Channels-shaped durable outbox: invocation id is the idempotency key.
    outbox: std::collections::BTreeMap<String, engine::WorkflowToolInvocation>,
    duplicate_invocations: u32,
    replies_sent: u32,
    terminal_tokens: Vec<String>,
    terminal_after_enqueue_and_reply: bool,
    continue_as_new_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelfReceiverControllerArgs {
    universe_id: uuid::Uuid,
    auto_reply: bool,
    continue_after_enqueue: bool,
    reply_payload_ref: Option<BlobRef>,
    snapshot: SelfReceiverControllerSnapshot,
}

/// Channels-shaped lifecycle controller and promise-bearing tool receiver.
/// Pushed invocations are processed directly from the workflow inbox while
/// the managed run is active; run-terminal handling is a separate branch and
/// is never a prerequisite for enqueue or reply.
#[workflow(name = "TestSelfReceiverControllerWorkflow")]
#[derive(Default)]
pub struct TestSelfReceiverControllerWorkflow {
    inbox: Vec<EmissionEnvelope>,
    snapshot: SelfReceiverControllerSnapshot,
}

#[workflow_methods]
impl TestSelfReceiverControllerWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: SelfReceiverControllerArgs,
    ) -> WorkflowResult<()> {
        ctx.state_mut(|state| state.snapshot = args.snapshot.clone());
        loop {
            ctx.wait_condition(|state| !state.inbox.is_empty()).await;
            let envelopes = ctx.state_mut(|state| std::mem::take(&mut state.inbox));
            for envelope in envelopes {
                match envelope.body {
                    EmissionBody::ToolInvocation { invocation } => {
                        let invocation_id = invocation.invocation_id.as_str().to_owned();
                        let fresh = ctx.state_mut(|state| {
                            if let std::collections::btree_map::Entry::Vacant(e) =
                                state.snapshot.outbox.entry(invocation_id)
                            {
                                e.insert(invocation.clone());
                                true
                            } else {
                                state.snapshot.duplicate_invocations += 1;
                                false
                            }
                        });
                        if !fresh || !args.auto_reply {
                            continue;
                        }
                        let EmissionProducer::Session { session_id, .. } = &envelope.producer
                        else {
                            continue;
                        };
                        let holder =
                            temporal_workflow::compose_workflow_id(args.universe_id, session_id);
                        for (_key, promise_id) in invocation.completion_promises.iter().flatten() {
                            let reply = EmissionEnvelope::source_resolution(
                                args.universe_id,
                                ctx.workflow_id().to_owned(),
                                promise_id.clone(),
                                PromiseResolution::Resolved {
                                    payload_ref: args.reply_payload_ref.clone(),
                                },
                            );
                            // First-terminal-writer semantics make the second
                            // resolution an end-to-end no-op.
                            for _ in 0..2 {
                                let _ = ctx
                                    .external_workflow(holder.clone(), None)
                                    .signal(AgentSessionWorkflow::deliver_emission, reply.clone())
                                    .await;
                                ctx.state_mut(|state| state.snapshot.replies_sent += 1);
                            }
                        }
                    }
                    EmissionBody::RunTerminal { token, .. } => {
                        ctx.state_mut(|state| {
                            state.snapshot.terminal_after_enqueue_and_reply =
                                !state.snapshot.outbox.is_empty()
                                    && (!args.auto_reply || state.snapshot.replies_sent > 0);
                            state.snapshot.terminal_tokens.push(token);
                        });
                    }
                    _ => {}
                }
            }

            if args.continue_after_enqueue
                && ctx.state(|state| {
                    !state.snapshot.outbox.is_empty() && state.snapshot.continue_as_new_count == 0
                })
            {
                let mut next = args.clone();
                ctx.state_mut(|state| state.snapshot.continue_as_new_count += 1);
                next.snapshot = ctx.state(|state| state.snapshot.clone());
                ctx.continue_as_new(&next, ContinueAsNewOptions::default())?;
            }
        }
    }

    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        envelope: EmissionEnvelope,
    ) {
        self.inbox.push(envelope);
    }

    #[query(name = "self_receiver_snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> SelfReceiverControllerSnapshot {
        self.snapshot.clone()
    }
}

/// Start-on-call plugin: started by the generic adapter from a CAS recipe,
/// resolves every keyed completion promise back to the holder, exposes the
/// fixed recovery query, then completes.
#[workflow(name = "TestStartPluginWorkflow")]
#[derive(Default)]
pub struct TestStartPluginWorkflow {
    recovery: WorkflowToolRecoveryResult,
}

#[workflow_methods]
impl TestStartPluginWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: WorkflowToolStartArgs,
    ) -> WorkflowResult<()> {
        if ctx.workflow_id() != args.execution_id {
            return Err(anyhow::anyhow!(
                "started execution id mismatch: workflow_id={} execution_id={}",
                ctx.workflow_id(),
                args.execution_id
            )
            .into());
        }
        for (key, promise_id) in args.invocation.completion_promises.iter().flatten() {
            let resolution = PromiseResolution::Resolved { payload_ref: None };
            ctx.state_mut(|state| {
                state
                    .recovery
                    .resolutions
                    .insert(key.clone(), resolution.clone());
            });
            let envelope = EmissionEnvelope::source_resolution(
                args.universe_id,
                args.execution_id.clone(),
                promise_id.clone(),
                resolution,
            );
            let _ = ctx
                .external_workflow(args.holder_workflow_id.clone(), None)
                .signal(AgentSessionWorkflow::deliver_emission, envelope)
                .await;
        }
        // Keep the started execution alive after its semantic reply. Joined
        // completion must follow the reply, not workflow termination.
        ctx.timer(Duration::from_secs(60)).await;
        Ok(())
    }

    #[query(name = WORKFLOW_TOOL_RECOVERY_QUERY)]
    pub fn recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        self.recovery.clone()
    }
}

/// Start-on-call plugin that never signals its result: the holder's slow
/// recovery poll must recover the keyed resolutions through the fixed query
/// after the execution completes.
#[workflow(name = "TestSilentStartPluginWorkflow")]
#[derive(Default)]
pub struct TestSilentStartPluginWorkflow {
    recovery: WorkflowToolRecoveryResult,
}

#[workflow_methods]
impl TestSilentStartPluginWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: WorkflowToolStartArgs,
    ) -> WorkflowResult<()> {
        for (key, _promise_id) in args.invocation.completion_promises.iter().flatten() {
            ctx.state_mut(|state| {
                state.recovery.resolutions.insert(
                    key.clone(),
                    PromiseResolution::Resolved { payload_ref: None },
                );
            });
        }
        Ok(())
    }

    #[query(name = WORKFLOW_TOOL_RECOVERY_QUERY)]
    pub fn recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        self.recovery.clone()
    }
}

/// A bound receiver that has already completed by the time the session
/// binds to it: push delivery must exhaust and fail terminally.
#[workflow(name = "TestClosedPluginWorkflow")]
#[derive(Default)]
pub struct TestClosedPluginWorkflow {}

#[workflow_methods]
impl TestClosedPluginWorkflow {
    #[run]
    pub async fn run(
        _ctx: &mut WorkflowContext<Self>,
        _args: BoundPluginArgs,
    ) -> WorkflowResult<()> {
        Ok(())
    }
}

/// Start-on-call plugin that works for a while before resolving. Used to
/// exercise holder continue-as-new during a parked completion (the rebuilt
/// start work re-issues the deterministic start and `AlreadyStarted` is
/// success) and owned-execution cancellation after run-terminal auto-cancel.
#[workflow(name = "TestSlowStartPluginWorkflow")]
#[derive(Default)]
pub struct TestSlowStartPluginWorkflow {
    recovery: WorkflowToolRecoveryResult,
}

#[workflow_methods]
impl TestSlowStartPluginWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: WorkflowToolStartArgs,
    ) -> WorkflowResult<()> {
        // Plugin workflows own cooperative cancellation: race domain work
        // against the cancellation request and end as cancelled.
        {
            use futures_util::FutureExt;
            let work = ctx.timer(Duration::from_secs(20)).fuse();
            let cancelled = ctx.cancelled();
            futures_util::pin_mut!(work, cancelled);
            futures_util::select! {
                _ = work => {}
                _ = cancelled => {
                    return Err(temporalio_sdk::WorkflowTermination::Cancelled);
                }
            }
        }
        for (key, promise_id) in args.invocation.completion_promises.iter().flatten() {
            let resolution = PromiseResolution::Resolved { payload_ref: None };
            ctx.state_mut(|state| {
                state
                    .recovery
                    .resolutions
                    .insert(key.clone(), resolution.clone());
            });
            let envelope = EmissionEnvelope::source_resolution(
                args.universe_id,
                args.execution_id.clone(),
                promise_id.clone(),
                resolution,
            );
            let _ = ctx
                .external_workflow(args.holder_workflow_id.clone(), None)
                .signal(AgentSessionWorkflow::deliver_emission, envelope)
                .await;
        }
        Ok(())
    }

    /// Accept (and ignore) per-key cancellation facts.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        _envelope: EmissionEnvelope,
    ) {
    }

    #[query(name = WORKFLOW_TOOL_RECOVERY_QUERY)]
    pub fn recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        self.recovery.clone()
    }
}

// ---------------------------------------------------------------------------
// Scripted session LLM: `CALL <tool>` -> tool call -> await its keyed
// promise -> final text carrying the await result. `CALL_NOWAIT <tool>`
// finishes the run right after the acknowledgement, leaving the run-scoped
// keyed promise to auto-cancel at run terminal.
// ---------------------------------------------------------------------------

struct WorkflowToolScriptedLlm {
    blobs: Arc<dyn BlobStore>,
}

impl WorkflowToolScriptedLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn latest_text_for_kind(
        &self,
        request: &LlmGenerationRequest,
        matches_kind: impl Fn(&ContextEntryKind) -> bool,
    ) -> Result<Option<String>, CoreAgentIoError> {
        for entry in request.request.context.entries.iter().rev() {
            if matches_kind(&entry.kind) {
                return self
                    .blobs
                    .read_text(&entry.content_ref)
                    .await
                    .map(Some)
                    .map_err(io_error);
            }
        }
        Ok(None)
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == tool_name)
        {
            return Err(CoreAgentIoError::Failed {
                message: format!("scripted workflow-tool test expected {tool_name} in the toolset"),
            });
        }
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("{tool_name}_call_{}", request.turn_id.as_u64()));
        let tool_name = ToolName::new(tool_name);
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content_ref: arguments_ref.clone(),
                media_type: Some("application/json".to_owned()),
                preview: Some(format!("{tool_name}({arguments})")),
                provider_kind: Some("workflow-tool-script".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!(
                    "workflow-tool-{}-{}",
                    tool_name,
                    request.turn_id.as_u64()
                )),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("workflow-tool-script".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
                context_token_estimate: None,
            },
        })
    }

    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
        text: String,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let output_ref = self
            .blobs
            .put_bytes(text.into_bytes())
            .await
            .map_err(io_error)?;
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                content_ref: output_ref,
                media_type: Some("text/plain".to_owned()),
                preview: Some("workflow tool scripted final".to_owned()),
                provider_kind: Some("workflow-tool-script".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!(
                    "workflow-tool-final-{}",
                    request.turn_id.as_u64()
                )),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

/// Extract the first keyed completion promise id from a workflow-tool
/// acknowledgement.
fn parse_completion_promise(text: &str) -> Option<String> {
    let start = text.find("wtp:sha256:")?;
    let hex = &text[start + "wtp:sha256:".len()..];
    let hex: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
    (hex.len() == 64).then(|| format!("wtp:sha256:{hex}"))
}

#[async_trait]
impl CoreAgentLlm for WorkflowToolScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let user_text = self
            .latest_text_for_kind(&request, |kind| {
                matches!(
                    kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::User
                    }
                )
            })
            .await?
            .unwrap_or_default();
        // Scan newest-first for the entry that decides this turn: a tool
        // result answers the pending exchange; a `CALL`/`CALL_NOWAIT` user
        // message starts one. Older runs' entries are skipped rather than
        // misread as fresh input.
        for entry in request.request.context.entries.iter().rev() {
            match &entry.kind {
                ContextEntryKind::ToolResult { .. } => {
                    let tool_result = self
                        .blobs
                        .read_text(&entry.content_ref)
                        .await
                        .map_err(io_error)?;
                    if tool_result.contains("\"accepted\":true")
                        && !user_text.starts_with("CALL_NOWAIT ")
                        && let Some(promise_id) = parse_completion_promise(&tool_result)
                    {
                        // A short await timeout ends the run while the keyed
                        // promise is still pending (an await timeout never
                        // changes the underlying promise).
                        let timeout_ms: u64 = if user_text.starts_with("CALL_SHORTWAIT ") {
                            5_000
                        } else {
                            120_000
                        };
                        let arguments = serde_json::json!({
                            "promises": [promise_id],
                            "mode": "all",
                            "timeout_ms": timeout_ms
                        });
                        return self
                            .tool_call_result(&request, AWAIT_TOOL_NAME, arguments)
                            .await;
                    }
                    return self
                        .final_result(&request, format!("workflow tool test done: {tool_result}"))
                        .await;
                }
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                } => {
                    let text = self
                        .blobs
                        .read_text(&entry.content_ref)
                        .await
                        .map_err(io_error)?;
                    if let Some(tool_name) = text
                        .strip_prefix("CALL ")
                        .or_else(|| text.strip_prefix("CALL_NOWAIT "))
                        .or_else(|| text.strip_prefix("CALL_SHORTWAIT "))
                    {
                        return self
                            .tool_call_result(
                                &request,
                                tool_name.trim(),
                                serde_json::json!({ "detail": "live plugin proof" }),
                            )
                            .await;
                    }
                }
                _ => {}
            }
        }
        self.final_result(&request, "scripted run completed".to_owned())
            .await
    }
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Harness: session worker (unchanged registration) + plugin worker.
// ---------------------------------------------------------------------------

async fn tool_definition(
    blobs: &Arc<dyn BlobStore>,
    tool_id: &str,
    tool_name: &str,
    semantic_type: &str,
) -> anyhow::Result<WorkflowToolDefinition> {
    let input_schema_ref = blobs
        .put_bytes(serde_json::to_vec(
            &serde_json::json!({ "type": "object" }),
        )?)
        .await?;
    Ok(WorkflowToolDefinition {
        tool_id: engine::WorkflowToolId::new(tool_id),
        revision: 1,
        semantic_type: semantic_type.to_owned(),
        tool: ToolSpec {
            name: ToolName::new(tool_name),
            execution: Default::default(),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: None,
                input_schema_ref,
                output_schema_ref: None,
                strict: None,
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        },
    })
}

fn reply_completion() -> WorkflowToolCompletion {
    promises_completion(None, None)
}

fn promises_completion(
    deadline_after_ms: Option<u64>,
    reply_schema_ref: Option<engine::BlobRef>,
) -> WorkflowToolCompletion {
    WorkflowToolCompletion::Promises {
        reply_schema_ref,
        deadline_after_ms,
        max_promises: 1,
        key_source: engine::WorkflowToolCompletionKeySource::Reply,
    }
}

fn joined_completion(
    deadline_after_ms: u64,
    reply_schema_ref: Option<engine::BlobRef>,
) -> WorkflowToolCompletion {
    WorkflowToolCompletion::Joined {
        reply_schema_ref,
        deadline_after_ms,
    }
}

/// Run one scenario with two workers: the ordinary session worker (the
/// production registration list, untouched — that is the stable-worker
/// proof) and a plugin worker registering only the test plugin workflows on
/// its own task queue.
async fn run_with_plugin_worker<F, Fut>(run_client: F) -> anyhow::Result<()>
where
    F: FnOnce(Client, Arc<GatewayAgentApi>, Arc<dyn BlobStore>, SessionId, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    run_with_plugin_worker_opts(None, run_client).await
}

async fn run_with_plugin_worker_opts<F, Fut>(
    continue_as_new_history_threshold: Option<u32>,
    run_client: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Client, Arc<GatewayAgentApi>, Arc<dyn BlobStore>, SessionId, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let session_queue = format!("lightspeed-agent-live-{}", uuid::Uuid::new_v4().simple());
    let plugin_queue = format!("lightspeed-plugin-live-{}", uuid::Uuid::new_v4().simple());
    let session_id = SessionId::new(format!("session_live_{}", uuid::Uuid::new_v4().simple()));
    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());

    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();

    let mut api_builder = GatewayAgentApi::builder(client.clone(), store.clone())
        .with_task_queue(session_queue.clone())
        .with_default_model(temporal_server::default_model_from_env());
    if let Some(threshold) = continue_as_new_history_threshold {
        api_builder = api_builder.with_continue_as_new_history_threshold(threshold);
    }
    let api = Arc::new(api_builder.build());

    let llm = Arc::new(WorkflowToolScriptedLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store.clone(), llm, tools)
            .with_workflow_tool_executions(client.clone()),
    );
    let mut session_worker =
        worker_with_activities(&runtime, client.clone(), session_queue.clone(), activities)?;
    let shutdown_session_worker = session_worker.shutdown_handle();
    let session_worker_future = session_worker.run();
    tokio::pin!(session_worker_future);

    let plugin_worker_options = WorkerOptions::new(plugin_queue.clone())
        .register_workflow::<TestBoundPluginWorkflow>()
        .register_workflow::<TestSelfReceiverControllerWorkflow>()
        .register_workflow::<TestStartPluginWorkflow>()
        .register_workflow::<TestSilentStartPluginWorkflow>()
        .register_workflow::<TestClosedPluginWorkflow>()
        .register_workflow::<TestSlowStartPluginWorkflow>()
        .task_types(WorkerTaskTypes::workflow_only())
        .build();
    let mut plugin_worker = Worker::new(&runtime, client.clone(), plugin_worker_options)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let shutdown_plugin_worker = plugin_worker.shutdown_handle();
    let plugin_worker_future = plugin_worker.run();
    tokio::pin!(plugin_worker_future);

    let client_future = run_client(client.clone(), api, blobs, session_id, plugin_queue);
    tokio::pin!(client_future);

    let client_result = tokio::select! {
        worker_result = session_worker_future.as_mut() => {
            return match worker_result {
                Ok(()) => Err(anyhow::anyhow!("session worker stopped before the live test completed")),
                Err(error) => Err(error.context("session worker failed")),
            };
        }
        worker_result = plugin_worker_future.as_mut() => {
            return match worker_result {
                Ok(()) => Err(anyhow::anyhow!("plugin worker stopped before the live test completed")),
                Err(error) => Err(error.context("plugin worker failed")),
            };
        }
        client_result = client_future.as_mut() => client_result,
    };

    shutdown_session_worker();
    shutdown_plugin_worker();
    tokio::time::timeout(Duration::from_secs(10), session_worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("session worker did not shut down within 10 seconds"))??;
    tokio::time::timeout(Duration::from_secs(10), plugin_worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("plugin worker did not shut down within 10 seconds"))??;
    client_result
}

use std::future::Future;

async fn start_managed_session_and_run(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    declarations: Vec<WorkflowToolDeclaration>,
    call_tool: &str,
) -> anyhow::Result<api::RunView> {
    api.start_managed_session_for_workflow_with_profile(
        session_id,
        false,
        None,
        engine::ManagedSessionWorkflowTools::v1(None, declarations),
    )
    .await
    .map_err(|error| anyhow::anyhow!("start managed session: {error:?}"))?;
    eprintln!("managed session started: {session_id}");
    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    text: format!("CALL {call_tool}"),
                }],
            },
            config: None,
        })
        .await
        .map_err(|error| anyhow::anyhow!("start run: {error:?}"))?;
    wait_for_terminal_run_slow(api, session_id, &run.result.run.id).await
}

/// Delivery retries, recovery polls, and start backoff make some scenarios
/// legitimately slower than the default helper timeout.
async fn wait_for_terminal_run_slow(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<api::RunView> {
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(120) {
            anyhow::bail!("timed out waiting for run {run_id} to finish");
        }
        if let Ok(run) = wait_for_terminal_run(api, session_id, run_id).await {
            return Ok(run);
        }
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Bound request/reply end to end: push delivery mid-run, keyed promise
/// resolved by the plugin, duplicate reply a no-op — with zero
/// plugin-specific code in the session worker.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_bound_request_reply_resolves_via_plugin_worker() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let receiver_id = format!("plugin/approvals-{}", uuid::Uuid::new_v4().simple());
        client
            .start_workflow(
                TestBoundPluginWorkflow::run,
                BoundPluginArgs {
                    universe_id,
                    auto_reply: true,
                },
                WorkflowStartOptions::new(plugin_queue, receiver_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start bound plugin: {error}"))?;

        let definition = tool_definition(
            &blobs,
            "approve",
            REQUEST_APPROVAL_TOOL,
            "lightspeed.test.approval.v1",
        )
        .await?;
        let run = start_managed_session_and_run(
            &api,
            &session_id,
            vec![WorkflowToolDeclaration::new(
                definition,
                WorkflowToolTarget::Bound {
                    receiver: WorkflowEndpointRef {
                        workflow_id: receiver_id.clone(),
                        workflow_kind: "test_approvals".to_owned(),
                    },
                    dispatch: engine::BoundWorkflowToolDispatch::Push,
                },
                reply_completion(),
            )],
            REQUEST_APPROVAL_TOOL,
        )
        .await?;

        if run.status != api::RunStatus::Completed {
            eprintln!("run entries: {:#?}", run.entries);
            eprintln!("tool batches: {:#?}", run.tool_batches);
        }
        assert_eq!(run.status, api::RunStatus::Completed);
        let output = final_assistant_text(&run).expect("assistant output");
        assert!(
            output.contains("resolved"),
            "await must observe the plugin's keyed resolution: {output}"
        );
        let await_calls = run
            .tool_batches
            .iter()
            .flat_map(|batch| &batch.calls)
            .filter(|call| call.tool_name == AWAIT_TOOL_NAME)
            .collect::<Vec<_>>();
        assert_eq!(await_calls.len(), 1, "expected one explicit await call");
        let await_call = await_calls[0];
        let await_value: serde_json::Value = serde_json::from_str(
            await_call
                .output
                .as_deref()
                .expect("await call must expose its materialized output"),
        )?;
        assert_eq!(await_value["outcome"], "terminal");
        assert_eq!(await_value["results"][0]["status"], "resolved");
        assert_eq!(
            run.entries
                .iter()
                .filter(|entry| matches!(
                    &entry.kind,
                    ContextEntryKindView::ToolResult { call_id, .. }
                        if call_id == &await_call.call_id
                ))
                .count(),
            1,
            "workflow Promise await must project exactly one ToolResult"
        );
        assert_eq!(
            run.entries
                .iter()
                .filter(|entry| matches!(
                    entry.kind,
                    ContextEntryKindView::Message {
                        role: ContextMessageRoleView::User
                    }
                ))
                .count(),
            1,
            "workflow Promise output must not be inserted as a user message"
        );

        let handle = client.get_workflow_handle::<TestBoundPluginWorkflow>(receiver_id);
        let snapshot = handle
            .query(
                TestBoundPluginWorkflow::plugin_snapshot,
                (),
                temporalio_client::WorkflowQueryOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("query plugin: {error}"))?;
        assert_eq!(snapshot.invocations, 1, "one pushed invocation");
        assert_eq!(snapshot.replies_sent, 2, "duplicate reply was sent");
        let _ = handle.terminate(WorkflowTerminateOptions::default()).await;
        Ok(())
    })
    .await
}

/// Channels-shaped Joined proof: one workflow is both lifecycle controller
/// and pushed bound receiver. It durably deduplicates an outbox enqueue,
/// resolves the runtime-owned reply while the run is active, continues as
/// new, and then consumes that run's terminal notification. The model sees
/// one ordinary result on the original call and never calls `await`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_controller_self_receiver_resolves_before_run_terminal() -> anyhow::Result<()>
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let controller_id = format!("channels/session-{}", uuid::Uuid::new_v4().simple());
        let receipt_ref = blobs.put_bytes(br#"{"status":"sent"}"#.to_vec()).await?;
        let controller = WorkflowEndpointRef {
            workflow_id: controller_id.clone(),
            workflow_kind: "channels.session".to_owned(),
        };
        client
            .start_workflow(
                TestSelfReceiverControllerWorkflow::run,
                SelfReceiverControllerArgs {
                    universe_id,
                    auto_reply: true,
                    continue_after_enqueue: true,
                    reply_payload_ref: Some(receipt_ref),
                    snapshot: SelfReceiverControllerSnapshot::default(),
                },
                WorkflowStartOptions::new(plugin_queue, controller_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start Channels controller: {error}"))?;

        let definition = tool_definition(
            &blobs,
            "message-send",
            MESSAGE_SEND_TOOL,
            "channels.message.send.v1",
        )
        .await?;
        api.start_managed_session_for_workflow_with_profile(
            &session_id,
            false,
            None,
            engine::ManagedSessionWorkflowTools::v1(
                Some(controller.clone()),
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Bound {
                        receiver: controller,
                        dispatch: engine::BoundWorkflowToolDispatch::Push,
                    },
                    joined_completion(30_000, None),
                )],
            ),
        )
        .await
        .map_err(|error| anyhow::anyhow!("start managed Channels session: {error:?}"))?;

        let terminal_token = format!("channel-terminal-{}", uuid::Uuid::new_v4().simple());
        let started = api
            .start_run(RunStartParams {
                notify_on_terminal: Some(RunTerminalNotificationInput {
                    token: terminal_token.clone(),
                }),
                submission_id: None,
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input {
                    items: vec![InputItem::Text {
                        text: format!("CALL {MESSAGE_SEND_TOOL}"),
                    }],
                },
                config: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("start managed Channels run: {error:?}"))?;
        let run = wait_for_terminal_run_slow(&api, &session_id, &started.result.run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        let output = final_assistant_text(&run).expect("assistant output");
        assert!(
            output.contains("sent"),
            "controller must return the semantic receipt mid-run: {output}"
        );
        let calls = run
            .tool_batches
            .iter()
            .flat_map(|batch| &batch.calls)
            .collect::<Vec<_>>();
        assert!(
            calls.iter().all(|call| call.tool_name != AWAIT_TOOL_NAME),
            "Joined must not produce an explicit await call: {calls:#?}"
        );
        let joined_call = calls
            .iter()
            .find(|call| call.tool_name == MESSAGE_SEND_TOOL)
            .expect("original Joined call");
        assert!(
            !joined_call
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("wtp:sha256:"),
            "Joined result must not expose the runtime Promise acknowledgement"
        );

        let handle =
            client.get_workflow_handle::<TestSelfReceiverControllerWorkflow>(controller_id.clone());
        let snapshot = {
            let started = std::time::Instant::now();
            loop {
                if started.elapsed() > Duration::from_secs(30) {
                    anyhow::bail!("controller never consumed the run-terminal notification");
                }
                let snapshot = handle
                    .query(
                        TestSelfReceiverControllerWorkflow::snapshot,
                        (),
                        temporalio_client::WorkflowQueryOptions::default(),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("query Channels controller: {error}"))?;
                if snapshot
                    .terminal_tokens
                    .iter()
                    .any(|token| token == &terminal_token)
                {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        assert_eq!(snapshot.outbox.len(), 1, "one idempotent outbox row");
        assert_eq!(snapshot.replies_sent, 2, "duplicate resolution was emitted");
        assert_eq!(
            snapshot.continue_as_new_count, 1,
            "controller continued as new"
        );
        assert!(
            snapshot.terminal_after_enqueue_and_reply,
            "terminal handling must happen after enqueue and Promise reply"
        );

        // Re-delivery after continue-as-new/replay converges on the durable
        // invocation-id outbox key and emits no additional resolution.
        let invocation = snapshot
            .outbox
            .values()
            .next()
            .expect("outbox invocation")
            .clone();
        handle
            .signal(
                TestSelfReceiverControllerWorkflow::deliver_emission,
                EmissionEnvelope::tool_invocation(
                    universe_id,
                    session_id.clone(),
                    engine::EventSeq::new(999),
                    invocation,
                ),
                temporalio_client::WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("redeliver Channels invocation: {error}"))?;
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(20) {
                anyhow::bail!("controller never deduplicated the replayed invocation");
            }
            let snapshot = handle
                .query(
                    TestSelfReceiverControllerWorkflow::snapshot,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query deduplicated invocation: {error}"))?;
            if snapshot.duplicate_invocations == 1 {
                assert_eq!(snapshot.outbox.len(), 1);
                assert_eq!(snapshot.replies_sent, 2);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let _ = handle.terminate(WorkflowTerminateOptions::default()).await;
        Ok(())
    })
    .await
}

/// Pushed Accepted is independent of caller lifetime: the model gets durable
/// acceptance, the run reaches terminal, and a receiver that starts afterward
/// still consumes the already-admitted invocation.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_pushed_accepted_delivers_after_run_terminal() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let receiver_id = format!("plugin/accepted-{}", uuid::Uuid::new_v4().simple());
        let definition = tool_definition(
            &blobs,
            "accepted-message",
            MESSAGE_SEND_TOOL,
            "channels.message.accepted.v1",
        )
        .await?;
        let run = start_managed_session_and_run(
            &api,
            &session_id,
            vec![WorkflowToolDeclaration::new(
                definition,
                WorkflowToolTarget::Bound {
                    receiver: WorkflowEndpointRef {
                        workflow_id: receiver_id.clone(),
                        workflow_kind: "test_accepted_receiver".to_owned(),
                    },
                    dispatch: engine::BoundWorkflowToolDispatch::Push,
                },
                WorkflowToolCompletion::Accepted,
            )],
            MESSAGE_SEND_TOOL,
        )
        .await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        assert!(
            run.tool_batches
                .iter()
                .flat_map(|batch| &batch.calls)
                .all(|call| call.tool_name != AWAIT_TOOL_NAME),
            "Accepted must not keep the run alive through await"
        );

        client
            .start_workflow(
                TestBoundPluginWorkflow::run,
                BoundPluginArgs {
                    universe_id,
                    auto_reply: false,
                },
                WorkflowStartOptions::new(plugin_queue, receiver_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start late Accepted receiver: {error}"))?;
        let handle = client.get_workflow_handle::<TestBoundPluginWorkflow>(receiver_id);
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(30) {
                anyhow::bail!("Accepted invocation was abandoned at run terminal");
            }
            let snapshot = handle
                .query(
                    TestBoundPluginWorkflow::plugin_snapshot,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query late Accepted receiver: {error}"))?;
            if snapshot.invocations == 1 {
                assert!(snapshot.cancellations.is_empty());
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = handle.terminate(WorkflowTerminateOptions::default()).await;
        Ok(())
    })
    .await
}

/// The self-receiver deadline is an executable backstop, not documentation:
/// a controller that accepts the invocation but never resolves it cannot
/// park its own managed run forever.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_controller_self_receiver_deadline_breaks_stalled_reply() -> anyhow::Result<()>
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let controller_id = format!("channels/stalled-{}", uuid::Uuid::new_v4().simple());
        let controller = WorkflowEndpointRef {
            workflow_id: controller_id.clone(),
            workflow_kind: "channels.session".to_owned(),
        };
        client
            .start_workflow(
                TestSelfReceiverControllerWorkflow::run,
                SelfReceiverControllerArgs {
                    universe_id,
                    auto_reply: false,
                    continue_after_enqueue: false,
                    reply_payload_ref: None,
                    snapshot: SelfReceiverControllerSnapshot::default(),
                },
                WorkflowStartOptions::new(plugin_queue, controller_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start stalled controller: {error}"))?;

        let definition = tool_definition(
            &blobs,
            "message-send",
            MESSAGE_SEND_TOOL,
            "channels.message.send.v1",
        )
        .await?;
        api.start_managed_session_for_workflow_with_profile(
            &session_id,
            false,
            None,
            engine::ManagedSessionWorkflowTools::v1(
                Some(controller.clone()),
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Bound {
                        receiver: controller,
                        dispatch: engine::BoundWorkflowToolDispatch::Push,
                    },
                    joined_completion(5_000, None),
                )],
            ),
        )
        .await
        .map_err(|error| anyhow::anyhow!("start stalled managed session: {error:?}"))?;

        let terminal_token = format!("stalled-terminal-{}", uuid::Uuid::new_v4().simple());
        let started = api
            .start_run(RunStartParams {
                notify_on_terminal: Some(RunTerminalNotificationInput {
                    token: terminal_token.clone(),
                }),
                submission_id: None,
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input {
                    items: vec![InputItem::Text {
                        text: format!("CALL {MESSAGE_SEND_TOOL}"),
                    }],
                },
                config: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("start stalled managed run: {error:?}"))?;
        let run = wait_for_terminal_run_slow(&api, &session_id, &started.result.run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        let joined_call = run
            .tool_batches
            .iter()
            .flat_map(|batch| &batch.calls)
            .find(|call| call.tool_name == MESSAGE_SEND_TOOL)
            .expect("stalled original Joined call");
        assert!(
            joined_call.is_error,
            "hard deadline must fail the stalled original Joined call: {joined_call:#?}"
        );
        assert!(
            run.tool_batches
                .iter()
                .flat_map(|batch| &batch.calls)
                .all(|call| call.tool_name != AWAIT_TOOL_NAME),
            "Joined deadline failure must resume the original call without await"
        );

        let handle =
            client.get_workflow_handle::<TestSelfReceiverControllerWorkflow>(controller_id);
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(30) {
                anyhow::bail!("stalled controller never received terminal notification");
            }
            let snapshot = handle
                .query(
                    TestSelfReceiverControllerWorkflow::snapshot,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query stalled controller: {error}"))?;
            if snapshot
                .terminal_tokens
                .iter()
                .any(|token| token == &terminal_token)
            {
                assert_eq!(snapshot.outbox.len(), 1);
                assert_eq!(snapshot.replies_sent, 0);
                assert!(snapshot.terminal_after_enqueue_and_reply);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = handle.terminate(WorkflowTerminateOptions::default()).await;
        Ok(())
    })
    .await
}

/// Producer authorization: a forged resolution from the wrong workflow id is
/// dropped; the exact stored producer resolves the promise.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_reply_requires_exact_stored_producer() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let receiver_id = format!("plugin/approvals-{}", uuid::Uuid::new_v4().simple());
        client
            .start_workflow(
                TestBoundPluginWorkflow::run,
                BoundPluginArgs {
                    universe_id,
                    auto_reply: false,
                },
                WorkflowStartOptions::new(plugin_queue, receiver_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start bound plugin: {error}"))?;

        let definition = tool_definition(
            &blobs,
            "approve",
            REQUEST_APPROVAL_TOOL,
            "lightspeed.test.approval.v1",
        )
        .await?;
        api.start_managed_session_for_workflow_with_profile(
            &session_id,
            false,
            None,
            engine::ManagedSessionWorkflowTools::v1(
                None,
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Bound {
                        receiver: WorkflowEndpointRef {
                            workflow_id: receiver_id.clone(),
                            workflow_kind: "test_approvals".to_owned(),
                        },
                        dispatch: engine::BoundWorkflowToolDispatch::Push,
                    },
                    reply_completion(),
                )],
            ),
        )
        .await
        .map_err(|error| anyhow::anyhow!("start managed session: {error:?}"))?;
        let run = api
            .start_run(RunStartParams {
                notify_on_terminal: None,
                submission_id: None,
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input {
                    items: vec![InputItem::Text {
                        text: format!("CALL {REQUEST_APPROVAL_TOOL}"),
                    }],
                },
                config: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("start run: {error:?}"))?;

        // Wait until the plugin has received the pushed invocation, then
        // read the keyed promise id from the session workflow status.
        let plugin_handle =
            client.get_workflow_handle::<TestBoundPluginWorkflow>(receiver_id.clone());
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(30) {
                anyhow::bail!("plugin never received the pushed invocation");
            }
            let snapshot = plugin_handle
                .query(
                    TestBoundPluginWorkflow::plugin_snapshot,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query plugin: {error}"))?;
            if snapshot.invocations >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let session_handle = live_workflow_handle(&client, &session_id)?;
        let status = session_handle
            .query(
                AgentSessionWorkflow::status,
                (),
                temporalio_client::WorkflowQueryOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("query session: {error}"))?;
        anyhow::ensure!(status.initialized, "session must be initialized");

        // The awaited promise id is deterministic, but recover it from the
        // session log projection instead: read events and find the emitted
        // completion promise.
        let events = api
            .read_session_events(api::SessionEventsReadParams {
                session_id: session_id.as_str().to_owned(),
                after: None,
                limit: Some(200),
                wait_ms: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("read events: {error:?}"))?;
        let promise_id = events
            .result
            .events
            .iter()
            .find_map(|event| match &event.kind {
                api::SessionEventKindView::WorkflowToolEmitted {
                    completion_promises: Some(promises),
                    ..
                } => promises.values().next().cloned(),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("no emitted completion promise in events"))?;

        // Forged producer: wrong workflow id. Must be dropped.
        let forged = EmissionEnvelope::source_resolution(
            universe_id,
            "plugin/imposter".to_owned(),
            engine::PromiseId::new(promise_id.clone()),
            PromiseResolution::Resolved { payload_ref: None },
        );
        session_handle
            .signal(
                AgentSessionWorkflow::deliver_emission,
                forged,
                temporalio_client::WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("signal forged reply: {error}"))?;
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(20) {
                anyhow::bail!("session never recorded the unauthorized-producer drop");
            }
            let status = session_handle
                .query(
                    AgentSessionWorkflow::status,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query session: {error}"))?;
            if status
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("unauthorized producer"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Authorized producer: exact stored receiver id. Resolves the run.
        let authorized = EmissionEnvelope::source_resolution(
            universe_id,
            receiver_id.clone(),
            engine::PromiseId::new(promise_id),
            PromiseResolution::Resolved { payload_ref: None },
        );
        session_handle
            .signal(
                AgentSessionWorkflow::deliver_emission,
                authorized.clone(),
                temporalio_client::WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("signal authorized reply: {error}"))?;

        let run = wait_for_terminal_run_slow(&api, &session_id, &run.result.run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);

        // Stale reply: the promise is already terminal, so a re-delivered
        // authorized resolution is an end-to-end no-op — the session stays
        // healthy and queryable.
        session_handle
            .signal(
                AgentSessionWorkflow::deliver_emission,
                authorized,
                temporalio_client::WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("signal stale reply: {error}"))?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status = session_handle
            .query(
                AgentSessionWorkflow::status,
                (),
                temporalio_client::WorkflowQueryOptions::default(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("query session after stale reply: {error}"))?;
        anyhow::ensure!(status.initialized, "session survives a stale reply");

        // Duplicate push: the receiver keys its state by invocation id, so a
        // re-delivered invocation envelope is a plugin-side no-op.
        let synthetic_invocation = engine::WorkflowToolInvocation {
            invocation_id: engine::WorkflowToolInvocationId::new(format!(
                "wti:sha256:{}",
                "b".repeat(64)
            )),
            tool_id: engine::WorkflowToolId::new("approve"),
            semantic_type: "lightspeed.test.approval.v1".to_owned(),
            schema_revision: 1,
            binding_fingerprint: format!("wtb:sha256:{}", "c".repeat(64)),
            session_universe_id: universe_id,
            session_id: session_id.clone(),
            run_id: engine::RunId::new(1),
            turn_id: engine::TurnId::new(1),
            tool_batch_id: engine::ToolBatchId::new(1),
            tool_call_id: engine::ToolCallId::new("synthetic-call"),
            arguments_ref: engine::BlobRef::from_bytes(b"{}"),
            execution_context_ref: None,
            completion_promises: None,
        };
        let duplicate_push = EmissionEnvelope::tool_invocation(
            universe_id,
            session_id.clone(),
            engine::EventSeq::new(999),
            synthetic_invocation,
        );
        for _ in 0..2 {
            plugin_handle
                .signal(
                    TestBoundPluginWorkflow::deliver_emission,
                    duplicate_push.clone(),
                    temporalio_client::WorkflowSignalOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("signal duplicate push: {error}"))?;
        }
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(20) {
                anyhow::bail!("plugin never processed the duplicate push");
            }
            let snapshot = plugin_handle
                .query(
                    TestBoundPluginWorkflow::plugin_snapshot,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query plugin: {error}"))?;
            if snapshot.duplicate_invocations >= 1 {
                assert_eq!(snapshot.invocations, 2, "one real + one fresh synthetic");
                assert_eq!(
                    snapshot.duplicate_invocations, 1,
                    "second copy deduplicated"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let _ = plugin_handle
            .terminate(WorkflowTerminateOptions::default())
            .await;
        Ok(())
    })
    .await
}

/// Start + Joined: the generic adapter starts a plugin workflow type the
/// session worker has never registered, from a CAS recipe. Its early semantic
/// reply completes the original call while the deterministic execution is
/// still running, without an explicit model `await`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_start_on_call_resolves_via_plugin_worker() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let recipe = serde_json::to_vec(&WorkflowToolRecipeV1 {
            workflow_type: "TestStartPluginWorkflow".to_owned(),
            task_queue: plugin_queue,
        })?;
        let recipe_fingerprint = workflow_tool_recipe_fingerprint(&recipe);
        let recipe_ref = blobs.put_bytes(recipe).await?;

        let definition = tool_definition(
            &blobs,
            "launch",
            LAUNCH_JOB_TOOL,
            "lightspeed.test.launch.v1",
        )
        .await?;
        let run = start_managed_session_and_run(
            &api,
            &session_id,
            vec![WorkflowToolDeclaration::new(
                definition,
                WorkflowToolTarget::Start {
                    start: WorkflowStartRef {
                        recipe_format: 1,
                        revision: 1,
                        recipe_ref,
                        recipe_fingerprint,
                    },
                },
                joined_completion(30_000, None),
            )],
            LAUNCH_JOB_TOOL,
        )
        .await?;

        assert_eq!(run.status, api::RunStatus::Completed);
        assert!(
            run.tool_batches
                .iter()
                .flat_map(|batch| &batch.calls)
                .all(|call| call.tool_name != AWAIT_TOOL_NAME),
            "Start + Joined must complete without an explicit await"
        );
        let events = api
            .read_session_events(api::SessionEventsReadParams {
                session_id: session_id.as_str().to_owned(),
                after: None,
                limit: Some(500),
                wait_ms: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("read Start + Joined events: {error:?}"))?;
        let execution_id = events
            .result
            .events
            .iter()
            .find_map(|event| match &event.kind {
                api::SessionEventKindView::WorkflowToolStartRequested { execution_id, .. } => {
                    Some(execution_id.clone())
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("Start + Joined execution event was not projected"))?;
        let handle = client.get_workflow_handle::<TestStartPluginWorkflow>(execution_id);
        use temporalio_common::protos::temporal::api::enums::v1::WorkflowExecutionStatus;
        let description = handle
            .describe(temporalio_client::WorkflowDescribeOptions::default())
            .await
            .map_err(|error| anyhow::anyhow!("describe early-reply execution: {error}"))?;
        assert_eq!(
            description.status(),
            WorkflowExecutionStatus::Running,
            "semantic reply must complete the Joined call before execution termination"
        );
        let _ = handle.terminate(WorkflowTerminateOptions::default()).await;
        Ok(())
    })
    .await
}

/// Recovery backstop: the started execution completes without ever
/// signalling its result; the holder's slow recovery poll recovers the keyed
/// resolution through the fixed recovery query.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_start_recovery_query_resolves_silent_execution() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|_client, api, blobs, session_id, plugin_queue| async move {
        let recipe = serde_json::to_vec(&WorkflowToolRecipeV1 {
            workflow_type: "TestSilentStartPluginWorkflow".to_owned(),
            task_queue: plugin_queue,
        })?;
        let recipe_fingerprint = workflow_tool_recipe_fingerprint(&recipe);
        let recipe_ref = blobs.put_bytes(recipe).await?;

        let definition = tool_definition(
            &blobs,
            "launch",
            LAUNCH_JOB_TOOL,
            "lightspeed.test.launch.v1",
        )
        .await?;
        let run = start_managed_session_and_run(
            &api,
            &session_id,
            vec![WorkflowToolDeclaration::new(
                definition,
                WorkflowToolTarget::Start {
                    start: WorkflowStartRef {
                        recipe_format: 1,
                        revision: 1,
                        recipe_ref,
                        recipe_fingerprint,
                    },
                },
                reply_completion(),
            )],
            LAUNCH_JOB_TOOL,
        )
        .await?;

        assert_eq!(run.status, api::RunStatus::Completed);
        let output = final_assistant_text(&run).expect("assistant output");
        assert!(
            output.contains("resolved"),
            "recovery query must resolve the silent execution's promise: {output}"
        );
        Ok(())
    })
    .await
}

/// Absent controller self-receiver: bounded push retry exhausts, terminal
/// `DeliveryFailed` fails the keyed promise, and the awaiting run completes
/// with the failure instead of wedging.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_dead_receiver_fails_promise_terminally() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(
        |_client, api, blobs, session_id, _plugin_queue| async move {
            let absent_controller = WorkflowEndpointRef {
                workflow_id: format!("channels/never-started-{}", uuid::Uuid::new_v4().simple()),
                workflow_kind: "channels.session".to_owned(),
            };
            let definition = tool_definition(
                &blobs,
                "approve",
                REQUEST_APPROVAL_TOOL,
                "lightspeed.test.approval.v1",
            )
            .await?;
            api.start_managed_session_for_workflow_with_profile(
                &session_id,
                false,
                None,
                engine::ManagedSessionWorkflowTools::v1(
                    Some(absent_controller.clone()),
                    vec![WorkflowToolDeclaration::new(
                        definition,
                        WorkflowToolTarget::Bound {
                            receiver: absent_controller,
                            dispatch: engine::BoundWorkflowToolDispatch::Push,
                        },
                        promises_completion(Some(30_000), None),
                    )],
                ),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start absent-controller session: {error:?}"))?;
            let started = api
                .start_run(RunStartParams {
                    notify_on_terminal: None,
                    submission_id: None,
                    session_id: session_id.as_str().to_owned(),
                    source: RunStartSource::Input {
                        items: vec![InputItem::Text {
                            text: format!("CALL {REQUEST_APPROVAL_TOOL}"),
                        }],
                    },
                    config: None,
                })
                .await
                .map_err(|error| anyhow::anyhow!("start absent-controller run: {error:?}"))?;
            let run = wait_for_terminal_run_slow(&api, &session_id, &started.result.run.id).await?;

            assert_eq!(run.status, api::RunStatus::Completed);
            let output = final_assistant_text(&run).expect("assistant output");
            assert!(
                output.contains("failed"),
                "await must observe the terminal delivery failure: {output}"
            );

            let events = api
                .read_session_events(api::SessionEventsReadParams {
                    session_id: session_id.as_str().to_owned(),
                    after: None,
                    limit: Some(200),
                    wait_ms: None,
                })
                .await
                .map_err(|error| anyhow::anyhow!("read events: {error:?}"))?;
            assert!(
                events.result.events.iter().any(|event| matches!(
                    event.kind,
                    api::SessionEventKindView::WorkflowToolDeliveryFailed { .. }
                )),
                "terminal DeliveryFailed must be projected"
            );
            Ok(())
        },
    )
    .await
}

fn parse_execution_id(text: &str) -> Option<String> {
    let start = text.find("wtx:sha256:")?;
    let hex = &text[start + "wtx:sha256:".len()..];
    let hex: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
    (hex.len() == 64).then(|| format!("wtx:sha256:{hex}"))
}

/// All keyed completion promises emitted so far, in session-log order.
async fn emitted_promises(
    api: &GatewayAgentApi,
    session_id: &SessionId,
) -> anyhow::Result<Vec<String>> {
    let events = api
        .read_session_events(api::SessionEventsReadParams {
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: None,
        })
        .await
        .map_err(|error| anyhow::anyhow!("read events: {error:?}"))?;
    Ok(events
        .result
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            api::SessionEventKindView::WorkflowToolEmitted {
                completion_promises: Some(promises),
                ..
            } => promises.values().next().cloned(),
            _ => None,
        })
        .collect())
}

/// Reply-schema gate: a schema-invalid reply payload fails the keyed
/// promise with a stable CAS-backed error instead of resolving it; a valid
/// payload resolves. The receiver cannot bypass validation.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_reply_schema_gates_resolutions() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let receiver_id = format!("plugin/approvals-{}", uuid::Uuid::new_v4().simple());
        client
            .start_workflow(
                TestBoundPluginWorkflow::run,
                BoundPluginArgs {
                    universe_id,
                    auto_reply: false,
                },
                WorkflowStartOptions::new(plugin_queue, receiver_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start bound plugin: {error}"))?;

        let reply_schema_ref = blobs
            .put_bytes(serde_json::to_vec(&serde_json::json!({
                "type": "object",
                "properties": { "ok": { "type": "boolean" } },
                "required": ["ok"],
                "additionalProperties": false
            }))?)
            .await?;
        let definition = tool_definition(
            &blobs,
            "approve",
            REQUEST_APPROVAL_TOOL,
            "lightspeed.test.approval.v1",
        )
        .await?;
        api.start_managed_session_for_workflow_with_profile(
            &session_id,
            false,
            None,
            engine::ManagedSessionWorkflowTools::v1(
                None,
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Bound {
                        receiver: WorkflowEndpointRef {
                            workflow_id: receiver_id.clone(),
                            workflow_kind: "test_approvals".to_owned(),
                        },
                        dispatch: engine::BoundWorkflowToolDispatch::Push,
                    },
                    promises_completion(None, Some(reply_schema_ref)),
                )],
            ),
        )
        .await
        .map_err(|error| anyhow::anyhow!("start managed session: {error:?}"))?;

        let session_handle = live_workflow_handle(&client, &session_id)?;
        let resolve_run = |payload: Vec<u8>, expect: &'static str| {
            let api = api.clone();
            let blobs = blobs.clone();
            let session_id = session_id.clone();
            let session_handle = &session_handle;
            let receiver_id = receiver_id.clone();
            async move {
                let run = api
                    .start_run(RunStartParams {
                        notify_on_terminal: None,
                        submission_id: None,
                        session_id: session_id.as_str().to_owned(),
                        source: RunStartSource::Input {
                            items: vec![InputItem::Text {
                                text: format!("CALL {REQUEST_APPROVAL_TOOL}"),
                            }],
                        },
                        config: None,
                    })
                    .await
                    .map_err(|error| anyhow::anyhow!("start run: {error:?}"))?;
                let expected_promises = run
                    .result
                    .run
                    .id
                    .trim_start_matches("run_")
                    .parse::<usize>()?;
                let started = std::time::Instant::now();
                let promise_id = loop {
                    if started.elapsed() > Duration::from_secs(30) {
                        anyhow::bail!("emitted promise never appeared for {expect}");
                    }
                    let promises = emitted_promises(&api, &session_id).await?;
                    if promises.len() >= expected_promises {
                        break promises[expected_promises - 1].clone();
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                };
                let payload_ref = blobs.put_bytes(payload).await?;
                let reply = EmissionEnvelope::source_resolution(
                    live_universe_id()?,
                    receiver_id,
                    engine::PromiseId::new(promise_id),
                    PromiseResolution::Resolved {
                        payload_ref: Some(payload_ref),
                    },
                );
                session_handle
                    .signal(
                        AgentSessionWorkflow::deliver_emission,
                        reply,
                        temporalio_client::WorkflowSignalOptions::default(),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("signal reply: {error}"))?;
                let run = wait_for_terminal_run_slow(&api, &session_id, &run.result.run.id).await?;
                assert_eq!(run.status, api::RunStatus::Completed);
                let output = final_assistant_text(&run)
                    .expect("assistant output")
                    .to_owned();
                assert!(
                    output.contains(expect),
                    "expected `{expect}` in await outcome: {output}"
                );
                anyhow::Ok(())
            }
        };

        // Schema-invalid payload -> the keyed promise fails.
        resolve_run(b"\"not an object\"".to_vec(), "failed").await?;
        // Valid payload -> the keyed promise resolves.
        resolve_run(br#"{"ok":true}"#.to_vec(), "resolved").await?;

        let _ = client
            .get_workflow_handle::<TestBoundPluginWorkflow>(receiver_id)
            .terminate(WorkflowTerminateOptions::default())
            .await;
        Ok(())
    })
    .await
}

/// Receiver closed: the bound receiver completed before the invocation, so
/// push delivery exhausts and fails the keyed promise terminally.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_closed_receiver_fails_delivery_terminally() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let receiver_id = format!("plugin/closed-{}", uuid::Uuid::new_v4().simple());
        client
            .start_workflow(
                TestClosedPluginWorkflow::run,
                BoundPluginArgs {
                    universe_id,
                    auto_reply: false,
                },
                WorkflowStartOptions::new(plugin_queue, receiver_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start closed plugin: {error}"))?;
        // Wait until the receiver has actually completed.
        let handle = client.get_workflow_handle::<TestClosedPluginWorkflow>(receiver_id.clone());
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(20) {
                anyhow::bail!("closed plugin never completed");
            }
            let description = handle
                .describe(temporalio_client::WorkflowDescribeOptions::default())
                .await
                .map_err(|error| anyhow::anyhow!("describe closed plugin: {error}"))?;
            use temporalio_common::protos::temporal::api::enums::v1::WorkflowExecutionStatus;
            if description.status() == WorkflowExecutionStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let definition = tool_definition(
            &blobs,
            "approve",
            REQUEST_APPROVAL_TOOL,
            "lightspeed.test.approval.v1",
        )
        .await?;
        let run = start_managed_session_and_run(
            &api,
            &session_id,
            vec![WorkflowToolDeclaration::new(
                definition,
                WorkflowToolTarget::Bound {
                    receiver: WorkflowEndpointRef {
                        workflow_id: receiver_id,
                        workflow_kind: "test_approvals".to_owned(),
                    },
                    dispatch: engine::BoundWorkflowToolDispatch::Push,
                },
                reply_completion(),
            )],
            REQUEST_APPROVAL_TOOL,
        )
        .await?;

        assert_eq!(run.status, api::RunStatus::Completed);
        let output = final_assistant_text(&run).expect("assistant output");
        assert!(
            output.contains("failed"),
            "await must observe the terminal delivery failure: {output}"
        );
        Ok(())
    })
    .await
}

/// Holder continue-as-new during a parked Joined call: pending start work and
/// the original-call mapping are rebuilt from durable state, the deterministic
/// start is re-issued, and the hard-deadline reply still completes that call.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_start_survives_continue_as_new() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker_opts(
        Some(30),
        |_client, api, blobs, session_id, plugin_queue| async move {
            let recipe = serde_json::to_vec(&WorkflowToolRecipeV1 {
                workflow_type: "TestSlowStartPluginWorkflow".to_owned(),
                task_queue: plugin_queue,
            })?;
            let recipe_fingerprint = workflow_tool_recipe_fingerprint(&recipe);
            let recipe_ref = blobs.put_bytes(recipe).await?;
            let definition = tool_definition(
                &blobs,
                "launch",
                LAUNCH_JOB_TOOL,
                "lightspeed.test.launch.v1",
            )
            .await?;
            let run = start_managed_session_and_run(
                &api,
                &session_id,
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Start {
                        start: WorkflowStartRef {
                            recipe_format: 1,
                            revision: 1,
                            recipe_ref,
                            recipe_fingerprint,
                        },
                    },
                    joined_completion(60_000, None),
                )],
                LAUNCH_JOB_TOOL,
            )
            .await?;

            assert_eq!(run.status, api::RunStatus::Completed);
            let joined_call = run
                .tool_batches
                .iter()
                .flat_map(|batch| &batch.calls)
                .find(|call| call.tool_name == LAUNCH_JOB_TOOL)
                .expect("original Start + Joined call");
            assert!(
                run.tool_batches
                    .iter()
                    .flat_map(|batch| &batch.calls)
                    .all(|call| call.tool_name != AWAIT_TOOL_NAME),
                "Joined continue-as-new proof must not call await"
            );
            let session = api
                .read_session(SessionReadParams {
                    session_id: session_id.as_str().to_owned(),
                })
                .await
                .map_err(|error| anyhow::anyhow!("read continued Joined session: {error:?}"))?;
            let completion = &session
                .result
                .session
                .management
                .as_ref()
                .and_then(|management| management.tools.first())
                .ok_or_else(|| anyhow::anyhow!("continued Joined declaration was not projected"))?
                .completion;
            assert_eq!(
                completion.clone(),
                api::WorkflowToolCompletionInput::Joined {
                    reply_schema_ref: None,
                    deadline_after_ms: 60_000,
                }
            );

            let events = api
                .read_session_events(api::SessionEventsReadParams {
                    session_id: session_id.as_str().to_owned(),
                    after: None,
                    limit: Some(500),
                    wait_ms: None,
                })
                .await
                .map_err(|error| anyhow::anyhow!("read continued Joined events: {error:?}"))?;
            let (event_call_id, promise_id) = events
                .result
                .events
                .iter()
                .find_map(|event| match &event.kind {
                    api::SessionEventKindView::WorkflowToolStartRequested {
                        call_id,
                        completion_promises,
                        ..
                    } => completion_promises
                        .get(engine::REPLY_COMPLETION_KEY)
                        .map(|promise_id| (call_id.clone(), promise_id.clone())),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("continued Joined start event was not projected"))?;
            assert_eq!(event_call_id, joined_call.call_id);
            assert!(events.result.events.iter().any(|event| matches!(
                &event.kind,
                api::SessionEventKindView::PromiseResolved {
                    promise_id: resolved,
                    ..
                } if resolved == &promise_id
            )));
            Ok(())
        },
    )
    .await
}

/// Run-terminal auto-cancel of a run-scoped keyed promise sends the
/// best-effort per-key cancellation fact to the bound receiver.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_run_terminal_auto_cancel_notifies_bound_receiver() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let universe_id = live_universe_id()?;
        let receiver_id = format!("plugin/approvals-{}", uuid::Uuid::new_v4().simple());
        client
            .start_workflow(
                TestBoundPluginWorkflow::run,
                BoundPluginArgs {
                    universe_id,
                    auto_reply: false,
                },
                WorkflowStartOptions::new(plugin_queue, receiver_id.clone()).build(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("start bound plugin: {error}"))?;

        let definition = tool_definition(
            &blobs,
            "approve",
            REQUEST_APPROVAL_TOOL,
            "lightspeed.test.approval.v1",
        )
        .await?;
        api.start_managed_session_for_workflow_with_profile(
            &session_id,
            false,
            None,
            engine::ManagedSessionWorkflowTools::v1(
                None,
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Bound {
                        receiver: WorkflowEndpointRef {
                            workflow_id: receiver_id.clone(),
                            workflow_kind: "test_approvals".to_owned(),
                        },
                        dispatch: engine::BoundWorkflowToolDispatch::Push,
                    },
                    reply_completion(),
                )],
            ),
        )
        .await
        .map_err(|error| anyhow::anyhow!("start managed session: {error:?}"))?;
        let run = api
            .start_run(RunStartParams {
                notify_on_terminal: None,
                submission_id: None,
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input {
                    items: vec![InputItem::Text {
                        text: format!("CALL_NOWAIT {REQUEST_APPROVAL_TOOL}"),
                    }],
                },
                config: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("start run: {error:?}"))?;
        let run = wait_for_terminal_run_slow(&api, &session_id, &run.result.run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);

        let plugin_handle = client.get_workflow_handle::<TestBoundPluginWorkflow>(receiver_id);
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(30) {
                anyhow::bail!("plugin never received the per-key cancellation fact");
            }
            let snapshot = plugin_handle
                .query(
                    TestBoundPluginWorkflow::plugin_snapshot,
                    (),
                    temporalio_client::WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("query plugin: {error}"))?;
            if snapshot.cancellations.len() == 1 {
                assert!(
                    snapshot.cancellations[0].ends_with(":reply"),
                    "cancellation names the completion key: {:?}",
                    snapshot.cancellations
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = plugin_handle
            .terminate(WorkflowTerminateOptions::default())
            .await;
        Ok(())
    })
    .await
}

/// When run-terminal auto-cancel turns the last pending keyed promise of a
/// session-owned started execution terminal, the exact execution is
/// cancelled.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_auto_cancel_cancels_started_execution() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(|client, api, blobs, session_id, plugin_queue| async move {
        let recipe = serde_json::to_vec(&WorkflowToolRecipeV1 {
            workflow_type: "TestSlowStartPluginWorkflow".to_owned(),
            task_queue: plugin_queue,
        })?;
        let recipe_fingerprint = workflow_tool_recipe_fingerprint(&recipe);
        let recipe_ref = blobs.put_bytes(recipe).await?;
        let definition =
            tool_definition(&blobs, "launch", LAUNCH_JOB_TOOL, "lightspeed.test.launch.v1")
                .await?;
        api.start_managed_session_for_workflow_with_profile(
            &session_id,
            false,
            None,
            engine::ManagedSessionWorkflowTools::v1(
                None,
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Start {
                        start: WorkflowStartRef {
                            recipe_format: 1,
                            revision: 1,
                            recipe_ref,
                            recipe_fingerprint,
                        },
                    },
                    reply_completion(),
                )],
            ),
        )
        .await
        .map_err(|error| anyhow::anyhow!("start managed session: {error:?}"))?;
        // A short await parks on the keyed promise long enough for the
        // deterministic start to be issued, then times out; the run
        // terminal auto-cancels the still-pending promise while the slow
        // execution is running.
        let run = api
            .start_run(RunStartParams {
        notify_on_terminal: None,
                submission_id: None,
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input {
                    items: vec![InputItem::Text {
                        text: format!("CALL_SHORTWAIT {LAUNCH_JOB_TOOL}"),
                    }],
                },
                config: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("start run: {error:?}"))?;
        let run = wait_for_terminal_run_slow(&api, &session_id, &run.result.run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        let execution_id = run
            .entries
            .iter()
            .filter_map(|entry| entry.text.as_deref().and_then(parse_execution_id))
            .next()
            .ok_or_else(|| anyhow::anyhow!("no execution id in run entries"))?;

        // The run terminal auto-cancels the run-scoped keyed promise; with
        // its last key terminal, the owned execution must be cancelled.
        let handle = client
            .get_workflow_handle::<temporalio_client::UntypedWorkflow>(execution_id);
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(60) {
                anyhow::bail!("started execution was never cancelled");
            }
            use temporalio_common::protos::temporal::api::enums::v1::WorkflowExecutionStatus;
            let status = match handle
                .describe(temporalio_client::WorkflowDescribeOptions::default())
                .await
            {
                Ok(description) => description.status(),
                // The deterministic start may not be visible yet.
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            if status == WorkflowExecutionStatus::Canceled {
                break;
            }
            if status != WorkflowExecutionStatus::Running {
                let session_error = live_workflow_handle(&client, &session_id)?
                    .query(
                        AgentSessionWorkflow::status,
                        (),
                        temporalio_client::WorkflowQueryOptions::default(),
                    )
                    .await
                    .ok()
                    .and_then(|status| status.last_error);
                anyhow::bail!(
                    "expected running-then-cancelled, got {status:?}; session last_error: {session_error:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok(())
    })
    .await
}

/// Hard promise deadline: a start recipe onto a task queue nobody serves
/// leaves the execution unscheduled; the binding's trusted relative
/// deadline fails the keyed promise instead of parking the run forever.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Temporal + Postgres env"]
async fn workflow_tool_start_deadline_fails_unserved_queue() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_plugin_worker(
        |_client, api, blobs, session_id, _plugin_queue| async move {
            let recipe = serde_json::to_vec(&WorkflowToolRecipeV1 {
                workflow_type: "TestStartPluginWorkflow".to_owned(),
                task_queue: format!("lightspeed-plugin-absent-{}", uuid::Uuid::new_v4().simple()),
            })?;
            let recipe_fingerprint = workflow_tool_recipe_fingerprint(&recipe);
            let recipe_ref = blobs.put_bytes(recipe).await?;
            let definition = tool_definition(
                &blobs,
                "launch",
                LAUNCH_JOB_TOOL,
                "lightspeed.test.launch.v1",
            )
            .await?;
            let run = start_managed_session_and_run(
                &api,
                &session_id,
                vec![WorkflowToolDeclaration::new(
                    definition,
                    WorkflowToolTarget::Start {
                        start: WorkflowStartRef {
                            recipe_format: 1,
                            revision: 1,
                            recipe_ref,
                            recipe_fingerprint,
                        },
                    },
                    promises_completion(Some(10_000), None),
                )],
                LAUNCH_JOB_TOOL,
            )
            .await?;

            assert_eq!(run.status, api::RunStatus::Completed);
            let output = final_assistant_text(&run).expect("assistant output");
            assert!(
                output.contains("failed"),
                "hard deadline must fail the unserved start's promise: {output}"
            );
            Ok(())
        },
    )
    .await
}
