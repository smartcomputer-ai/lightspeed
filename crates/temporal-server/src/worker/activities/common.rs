use engine::{
    BlobRef, ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    CoreAgentIoError, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ToolCallStatus, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationResult,
    storage::{BlobStore, BlobStoreError},
};
use temporalio_sdk::activities::ActivityError;

pub(super) fn activity_error(error: impl Into<anyhow::Error>) -> ActivityError {
    ActivityError::from(error.into())
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
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
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

async fn write_error_blob(
    blobs: &dyn BlobStore,
    message: impl Into<String>,
) -> Result<BlobRef, BlobStoreError> {
    blobs.put_bytes(message.into().into_bytes()).await
}
