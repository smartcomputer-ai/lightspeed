use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use harness::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError, CoreAgentLlm,
    CoreAgentTools, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ObservedToolCall, ToolBatchOutcome, ToolCallId, ToolCallStatus,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult, ToolKind,
    ToolName, storage::BlobStore,
};
use serde_json::Value;

use crate::worker::FAKE_TOOL_NAME;

/// Marker in fake tool arguments that makes [`FakeTools`] return a terminal
/// failed result for that call instead of an echo.
pub const FAKE_TOOL_FAILURE_MARKER: &str = "fail this call";

/// The tiny provider-suggested delay scripted transient failures carry, so
/// retry-heavy live tests run in seconds without touching the production
/// retry policy constants.
pub const FAKE_TRANSIENT_RETRY_AFTER: Duration = Duration::from_millis(50);

/// Shared counters for the fake runtime, so live tests can assert how many
/// provider/tool calls the hosted runtime actually made and how many were
/// abandoned by activity cancellation.
#[derive(Clone, Default)]
pub struct FakeRuntimeCounters {
    generations_started: Arc<AtomicUsize>,
    generations_completed: Arc<AtomicUsize>,
    generations_abandoned: Arc<AtomicUsize>,
    tool_calls_started: Arc<AtomicUsize>,
    tool_calls_completed: Arc<AtomicUsize>,
    tool_calls_abandoned: Arc<AtomicUsize>,
}

impl FakeRuntimeCounters {
    pub fn generations_started(&self) -> usize {
        self.generations_started.load(Ordering::SeqCst)
    }

    pub fn generations_completed(&self) -> usize {
        self.generations_completed.load(Ordering::SeqCst)
    }

    /// Generate calls whose future was dropped before completing: the
    /// worker abandoned an in-flight provider call on activity cancellation.
    pub fn generations_abandoned(&self) -> usize {
        self.generations_abandoned.load(Ordering::SeqCst)
    }

    pub fn tool_calls_started(&self) -> usize {
        self.tool_calls_started.load(Ordering::SeqCst)
    }

    pub fn tool_calls_completed(&self) -> usize {
        self.tool_calls_completed.load(Ordering::SeqCst)
    }

    pub fn tool_calls_abandoned(&self) -> usize {
        self.tool_calls_abandoned.load(Ordering::SeqCst)
    }
}

/// Drop guard: counts an in-flight operation as abandoned unless it was
/// explicitly marked completed.
struct InflightGuard {
    completed: bool,
    completed_counter: Arc<AtomicUsize>,
    abandoned_counter: Arc<AtomicUsize>,
}

impl InflightGuard {
    fn start(
        started: &Arc<AtomicUsize>,
        completed: &Arc<AtomicUsize>,
        abandoned: &Arc<AtomicUsize>,
    ) -> Self {
        started.fetch_add(1, Ordering::SeqCst);
        Self {
            completed: false,
            completed_counter: completed.clone(),
            abandoned_counter: abandoned.clone(),
        }
    }

    fn complete(mut self) {
        self.completed = true;
        self.completed_counter.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.abandoned_counter.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Clone)]
pub struct FakeLlm {
    blobs: Arc<dyn BlobStore>,
    tool_rounds_before_final: usize,
    parallel_tool_calls: usize,
    failing_parallel_call: Option<usize>,
    transient_failures_remaining: Arc<AtomicUsize>,
    generation_delay: Duration,
    stalled: Arc<AtomicBool>,
    counters: FakeRuntimeCounters,
}

impl FakeLlm {
    pub fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            blobs,
            tool_rounds_before_final: 1,
            parallel_tool_calls: 1,
            failing_parallel_call: None,
            transient_failures_remaining: Arc::new(AtomicUsize::new(0)),
            generation_delay: Duration::ZERO,
            stalled: Arc::new(AtomicBool::new(false)),
            counters: FakeRuntimeCounters::default(),
        }
    }

    /// While `stalled` is set, every generate call hangs forever (it counts
    /// as started and, once the activity abandons it, as abandoned). Live
    /// tests use this to drive provider activities into Temporal's timeouts
    /// and then clear the switch to let the session recover.
    pub fn with_stall_switch(mut self, stalled: Arc<AtomicBool>) -> Self {
        self.stalled = stalled;
        self
    }

    /// Sleep this long inside every generate call before producing the
    /// result, so live tests can cancel or steer a run mid-generation.
    pub fn with_generation_delay(mut self, delay: Duration) -> Self {
        self.generation_delay = delay;
        self
    }

    /// Share counters with the test (and with a [`FakeTools`] built through
    /// [`FakeTools::with_counters`]).
    pub fn with_counters(mut self, counters: FakeRuntimeCounters) -> Self {
        self.counters = counters;
        self
    }

    pub fn counters(&self) -> FakeRuntimeCounters {
        self.counters.clone()
    }

    pub fn with_tool_rounds(mut self, tool_rounds_before_final: usize) -> Self {
        self.tool_rounds_before_final = tool_rounds_before_final;
        self
    }

    /// Emit this many tool calls in one tool-call turn, so hosted runs
    /// exercise a multi-call batch instead of one call per turn.
    pub fn with_parallel_tool_calls(mut self, parallel_tool_calls: usize) -> Self {
        self.parallel_tool_calls = parallel_tool_calls.max(1);
        self
    }

    /// Mark one of the parallel calls with [`FAKE_TOOL_FAILURE_MARKER`] so
    /// its execution fails terminally while its siblings succeed.
    pub fn with_failing_parallel_call(mut self, index: usize) -> Self {
        self.failing_parallel_call = Some(index);
        self
    }

    /// Fail the next `count` generate calls with a transient
    /// [`CoreAgentIoError::Retryable`] carrying
    /// [`FAKE_TRANSIENT_RETRY_AFTER`], then behave normally. The counter is
    /// shared across clones, so successive Temporal activity attempts consume
    /// it in order. Use `usize::MAX` to stay transient past any bounded
    /// attempt budget.
    pub fn with_transient_failures(self, count: usize) -> Self {
        self.transient_failures_remaining
            .store(count, Ordering::SeqCst);
        self
    }

    fn take_scripted_transient_failure(&self) -> bool {
        self.transient_failures_remaining
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
        tool_id: ToolName,
        tool_name: ToolName,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mut context_entries = Vec::with_capacity(self.parallel_tool_calls);
        let mut tool_calls = Vec::with_capacity(self.parallel_tool_calls);
        for index in 0..self.parallel_tool_calls {
            let marker = if self.failing_parallel_call == Some(index) {
                format!(" {FAKE_TOOL_FAILURE_MARKER}")
            } else {
                String::new()
            };
            let arguments = serde_json::json!({
                "text": format!(
                    "echo from run {} turn {} call {index}{marker}",
                    request.run_id, request.turn_id
                )
            });
            let argument_bytes = serde_json::to_vec(&arguments).map_err(io_error)?;
            let arguments_ref = self
                .blobs
                .put_bytes(argument_bytes)
                .await
                .map_err(io_error)?;
            let call_id =
                ToolCallId::new(format!("agent_call_{}_{index}", request.turn_id.as_u64()));
            context_entries.push(ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content: harness::ContentRef {
                    content_ref: arguments_ref.clone(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("fake".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            });
            tool_calls.push(ObservedToolCall {
                call_id,
                tool_id: Some(tool_id.clone()),
                tool_name: tool_name.clone(),
                provider_kind: Some("fake".to_owned()),
                arguments_ref,
                native_call_ref: None,
            });
        }
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries,
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("fake-tool-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }

    /// The final answer names the run and echoes every steering message the
    /// request context carries, so live tests can prove steering reached the
    /// model at the next turn boundary.
    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mut text = format!("Fake agent completed run {}.", request.run_id);
        for entry in &request.request.context.entries {
            if !matches!(entry.source, harness::ContextEntrySource::Steering { .. }) {
                continue;
            }
            let steering = self
                .blobs
                .read_text(&entry.content.content_ref)
                .await
                .map_err(io_error)?;
            text.push_str(&format!(" Steering received: {steering}."));
        }
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
                content: harness::ContentRef {
                    content_ref: output_ref,
                    media_type: Some("text/plain".to_owned()),
                    provider_kind: Some("fake".to_owned()),
                },
                preview: Some("fake final answer".to_owned()),
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("fake-final-{}", request.turn_id.as_u64())),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[async_trait]
impl CoreAgentLlm for FakeLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if self.take_scripted_transient_failure() {
            return Err(CoreAgentIoError::Retryable {
                message: "scripted transient provider failure".to_owned(),
                retry_after: Some(FAKE_TRANSIENT_RETRY_AFTER),
            });
        }
        let guard = InflightGuard::start(
            &self.counters.generations_started,
            &self.counters.generations_completed,
            &self.counters.generations_abandoned,
        );
        if self.stalled.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if !self.generation_delay.is_zero() {
            tokio::time::sleep(self.generation_delay).await;
        }
        let result = if tool_result_count(&request) >= self.tool_rounds_before_final {
            self.final_result(&request).await
        } else {
            match invocable_fake_tool(&request)? {
                Some((tool_id, tool_name)) => {
                    self.tool_call_result(&request, tool_id, tool_name).await
                }
                None => self.final_result(&request).await,
            }
        };
        guard.complete();
        result
    }
}

#[derive(Clone)]
pub struct FakeTools {
    blobs: Arc<dyn BlobStore>,
    call_delay: Duration,
    counters: FakeRuntimeCounters,
}

impl FakeTools {
    pub fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            blobs,
            call_delay: Duration::ZERO,
            counters: FakeRuntimeCounters::default(),
        }
    }

    /// Sleep this long inside every tool batch/call before producing results,
    /// so live tests can cancel a run while its tools execute.
    pub fn with_call_delay(mut self, delay: Duration) -> Self {
        self.call_delay = delay;
        self
    }

    pub fn with_counters(mut self, counters: FakeRuntimeCounters) -> Self {
        self.counters = counters;
        self
    }
}

#[async_trait]
impl CoreAgentTools for FakeTools {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let guard = InflightGuard::start(
            &self.counters.tool_calls_started,
            &self.counters.tool_calls_completed,
            &self.counters.tool_calls_abandoned,
        );
        if !self.call_delay.is_zero() {
            tokio::time::sleep(self.call_delay).await;
        }
        let mut results = Vec::with_capacity(request.calls.len());
        for call in &request.calls {
            let args = self
                .blobs
                .read_text(&call.arguments_ref)
                .await
                .map_err(io_error)?;
            let text = serde_json::from_str::<Value>(&args)
                .ok()
                .and_then(|value| {
                    value
                        .get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or(args);
            if text.contains(FAKE_TOOL_FAILURE_MARKER) {
                let error_ref = self
                    .blobs
                    .put_bytes(format!("{}: scripted failure", call.tool_name).into_bytes())
                    .await
                    .map_err(io_error)?;
                results.push(ToolInvocationResult {
                    attachments: Vec::new(),
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Failed,
                    output_ref: None,
                    model_visible_context_entries: vec![
                        ToolInvocationResult::tool_result_context_entry(
                            &call.call_id,
                            ToolCallStatus::Failed,
                            error_ref.clone(),
                        ),
                    ],
                    error_ref: Some(error_ref),
                    effects: Vec::new(),
                });
                continue;
            }
            let output = format!("{}: {text}", call.tool_name);
            let output_bytes = output.len() as u64;
            let output_ref = self
                .blobs
                .put_bytes(output.into_bytes())
                .await
                .map_err(io_error)?;
            results.push(ToolInvocationResult {
                attachments: Vec::new(),
                duration_ms: None,
                output_bytes: Some(output_bytes),
                truncated: false,
                call_id: call.call_id.clone(),
                status: ToolCallStatus::Succeeded,
                output_ref: Some(output_ref.clone()),
                model_visible_context_entries: vec![
                    ToolInvocationResult::tool_result_context_entry(
                        &call.call_id,
                        ToolCallStatus::Succeeded,
                        output_ref,
                    ),
                ],
                error_ref: None,
                effects: Vec::new(),
            });
        }
        guard.complete();
        Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            results,
        }))
    }
}

fn tool_result_count(request: &LlmGenerationRequest) -> usize {
    request
        .request
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }))
        .count()
}

/// Picks a tool the fake model can call from the planned request toolset,
/// preferring the canonical fake echo tool when it is registered. Returns
/// `None` when the session has no client-invocable function tool, in which
/// case the fake model answers directly.
fn invocable_fake_tool(
    request: &LlmGenerationRequest,
) -> Result<Option<(ToolName, ToolName)>, CoreAgentIoError> {
    let tools = &request.request.tools;
    if let Some(tool) = tools
        .iter()
        .find(|tool| tool.name.as_str() == FAKE_TOOL_NAME)
    {
        return Ok(Some((tool.name.clone(), tool.name.clone())));
    }
    let target = tools::runtime::ToolTarget::from(&request.request.model);
    for tool in tools {
        match &tool.kind {
            ToolKind::Function(_) => return Ok(Some((tool.name.clone(), tool.name.clone()))),
            ToolKind::Builtin(spec) => {
                let definitions =
                    tools::definitions::resolve(&tool.name, spec, &target).map_err(io_error)?;
                if let Some(definition) = definitions.into_iter().find(|definition| {
                    matches!(
                        definition.definition,
                        tools::definitions::Definition::Function(_)
                    )
                }) {
                    return Ok(Some((tool.name.clone(), definition.name)));
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}
