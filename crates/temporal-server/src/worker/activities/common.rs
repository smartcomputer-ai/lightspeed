use engine::{
    BlobRef, ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    CoreAgentIoError, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ToolCallStatus, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationResult,
    storage::{BlobStore, BlobStoreError},
};
use std::time::Duration;

use temporalio_sdk::{
    ApplicationFailure,
    activities::{ActivityContext, ActivityError},
};

pub(super) fn activity_error(error: impl Into<anyhow::Error>) -> ActivityError {
    ActivityError::from(error.into())
}

/// Run `work` as a cancellable, heartbeating activity body. The
/// activity heartbeats on a ticker so Temporal can deliver a workflow-side
/// cancellation (`TryCancel` from the session workflow); when it arrives the
/// in-flight work is dropped — the provider request or tool execution is
/// abandoned — and the activity completes as cancelled. Dropping the future
/// is the abort: provider clients and tool runtimes must not hold work alive
/// past their future.
pub(super) async fn cancellable<T>(
    ctx: &ActivityContext,
    work: impl Future<Output = Result<T, ActivityError>>,
) -> Result<T, ActivityError> {
    let heartbeat_ctx = ctx.clone();
    let heartbeat = async move {
        let mut ticker =
            tokio::time::interval(temporal_workflow::ACTIVITY_CANCELLATION_HEARTBEAT_INTERVAL);
        loop {
            ticker.tick().await;
            heartbeat_ctx.record_heartbeat(Vec::new());
        }
    };
    tokio::pin!(work);
    tokio::pin!(heartbeat);
    tokio::select! {
        result = &mut work => result,
        _ = &mut heartbeat => unreachable!("heartbeat ticker never completes"),
        _ = ctx.cancelled() => {
            tracing::info!("activity cancelled by workflow; abandoning in-flight work");
            Err(ActivityError::cancelled())
        }
    }
}

/// Longest transient provider message carried in the typed activity failure;
/// details payloads stay bounded even for pathological provider errors.
const MAX_TRANSIENT_FAILURE_MESSAGE_BYTES: usize = 8 * 1024;

/// Builds the typed retryable `llm_provider_transient` application failure
/// for a transient provider error. Temporal owns the durable backoff;
/// a provider-suggested delay is honored via `next_retry_delay`, clamped to
/// the policy's maximum interval so it cannot schedule past the bounded total
/// budget.
pub(super) fn transient_provider_failure(
    operation: &'static str,
    attempt: u32,
    message: String,
    retry_after: Option<Duration>,
) -> ActivityError {
    let mut message = message;
    if message.len() > MAX_TRANSIENT_FAILURE_MESSAGE_BYTES {
        let mut end = MAX_TRANSIENT_FAILURE_MESSAGE_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    let next_retry_delay =
        retry_after.map(|delay| delay.min(temporal_workflow::LLM_RETRY_MAX_INTERVAL));
    tracing::warn!(
        operation,
        attempt,
        next_retry_delay_ms = next_retry_delay.map(|delay| delay.as_millis() as u64),
        message = %message,
        "transient LLM provider failure; Temporal retries within the bounded policy"
    );
    let failure = ApplicationFailure::builder(anyhow::anyhow!(
        "{operation} hit a transient provider failure (attempt {attempt}): {message}"
    ))
    .type_name(temporal_workflow::LLM_PROVIDER_TRANSIENT_ERROR_TYPE.to_owned())
    .maybe_next_retry_delay(next_retry_delay)
    .details(temporal_workflow::LlmTransientFailureDetails {
        version: temporal_workflow::LLM_TRANSIENT_FAILURE_DETAILS_VERSION,
        message,
        attempt,
        retry_after_ms: next_retry_delay.map(|delay| delay.as_millis() as u64),
    })
    .build();
    ActivityError::from(failure)
}

pub(super) async fn failed_generation_result_from_error(
    blobs: &dyn BlobStore,
    request: LlmGenerationRequest,
    error: CoreAgentIoError,
) -> Result<LlmGenerationResult, BlobStoreError> {
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

pub(super) async fn failed_context_compaction_result_from_error(
    blobs: &dyn BlobStore,
    request: ContextCompactionRequest,
    error: CoreAgentIoError,
) -> Result<ContextCompactionResult, BlobStoreError> {
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

pub(super) async fn failed_tool_batch_result(
    blobs: &dyn BlobStore,
    request: &ToolInvocationBatchRequest,
    error: impl AsRef<str>,
) -> Result<ToolInvocationBatchResult, BlobStoreError> {
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

pub(super) async fn failed_tool_call_result(
    blobs: &dyn BlobStore,
    request: &engine::ToolInvocationCallRequest,
    error: impl AsRef<str>,
) -> Result<ToolInvocationResult, BlobStoreError> {
    let error_ref = write_error_blob(
        blobs,
        format!(
            "{}\nrun_id={}\nturn_id={}\nbatch_id={}\ncall_id={}\ntool_name={}\n",
            error.as_ref(),
            request.run_id,
            request.turn_id,
            request.batch_id,
            request.call.call_id,
            request.call.tool_name
        ),
    )
    .await?;
    Ok(ToolInvocationResult {
        duration_ms: None,
        output_bytes: None,
        truncated: false,
        call_id: request.call.call_id.clone(),
        status: ToolCallStatus::Failed,
        output_ref: None,
        model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
            &request.call.call_id,
            ToolCallStatus::Failed,
            error_ref.clone(),
        )],
        error_ref: Some(error_ref),
        effects: Vec::new(),
    })
}

pub(super) async fn write_error_blob(
    blobs: &dyn BlobStore,
    message: impl Into<String>,
) -> Result<BlobRef, BlobStoreError> {
    blobs.put_bytes(message.into().into_bytes()).await
}
