use harness::{
    BlobRef, ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    CoreAgentIoError, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ToolCallStatus, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationResult,
    storage::{BlobStore, BlobStoreError, SessionStoreError},
};
use std::time::Duration;

use temporalio_sdk::{
    ApplicationFailure,
    activities::{ActivityContext, ActivityError},
};

pub(super) fn activity_error(
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ActivityError {
    ActivityError::application(ApplicationFailure::new(error))
}

/// A failure no retry heals: the same request gets the same answer on every attempt,
/// so the workflow should hear it now rather than wait behind endless retries.
pub(super) fn non_retryable_activity_error(
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ActivityError {
    ActivityError::application(ApplicationFailure::non_retryable(error))
}

/// A session store failure as an activity failure. Only a plain store failure can be
/// outlasted, and it is retried for as long as the outage lasts. Everything else is the
/// store's answer about this request.
pub(super) fn session_store_failure(error: SessionStoreError) -> ActivityError {
    match error {
        SessionStoreError::Store { .. } => activity_error(error),
        error if error.is_data_rejection() => ActivityError::from(
            ApplicationFailure::builder(error)
                .type_name(temporal_workflow::SESSION_DATA_REJECTED_ERROR_TYPE.to_owned())
                .non_retryable(true)
                .build(),
        ),
        error => non_retryable_activity_error(error),
    }
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
            heartbeat_ctx
                .record_heartbeat(())
                .await
                .expect("unit heartbeat serializes");
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
    // A rejection keeps the provider's message word for word: it is what the
    // run failure shows, and the operator's starting point for a repair.
    let context_limit = matches!(error, CoreAgentIoError::ContextLimit { .. });
    let (status, text) = match error {
        CoreAgentIoError::Rejected { message } | CoreAgentIoError::ContextLimit { message } => {
            (LlmGenerationStatus::Rejected, message)
        }
        error => (
            LlmGenerationStatus::Failed,
            format!(
                "core agent LLM generation failed\nrun_id={}\nturn_id={}\nerror={error}\n",
                request.run_id, request.turn_id
            ),
        ),
    };
    let failure_ref = write_error_blob(blobs, text).await?;
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status,
        failure_ref: Some(failure_ref),
        context_entries: Vec::new(),
        facts: LlmGenerationFacts {
            duration_ms: None,
            provider_response_id: None,
            finish: if context_limit {
                LlmFinish::ContextLimit
            } else {
                LlmFinish::Failed
            },
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
        usage: None,
        calls: 0,
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
            attachments: Vec::new(),
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
    request: &harness::ToolInvocationCallRequest,
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
        attachments: Vec::new(),
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

#[cfg(test)]
mod tests {
    use harness::SessionId;

    use super::*;

    fn failure(error: SessionStoreError) -> ApplicationFailure {
        match session_store_failure(error) {
            ActivityError::Application(failure) => *failure,
            other => panic!("expected an application failure, got {other:?}"),
        }
    }

    #[test]
    fn data_rejections_are_typed_and_final() {
        let session_id = SessionId::new("session");
        for error in [
            SessionStoreError::DataRejected {
                message: "invalid input syntax for type json".to_owned(),
            },
            SessionStoreError::MissingBlobs {
                session_id: session_id.clone(),
                blob_refs: vec![BlobRef::from_bytes(b"never stored")],
            },
        ] {
            let message = error.to_string();
            let failure = failure(error);
            assert!(failure.is_non_retryable());
            assert_eq!(
                failure.type_name(),
                Some(temporal_workflow::SESSION_DATA_REJECTED_ERROR_TYPE)
            );
            assert_eq!(failure.to_string(), message);
        }
    }

    #[test]
    fn store_answers_are_final_and_carry_no_rejection_type() {
        let session_id = SessionId::new("session");
        for error in [
            SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            },
            SessionStoreError::ExpectedHeadMismatch {
                session_id,
                expected: None,
                actual: None,
            },
        ] {
            let failure = failure(error);
            assert!(failure.is_non_retryable());
            assert_eq!(failure.type_name(), None);
        }
    }

    #[test]
    fn an_outage_keeps_retrying() {
        let failure = failure(SessionStoreError::Store {
            message: "connection refused".to_owned(),
        });
        assert!(!failure.is_non_retryable());
    }
}
