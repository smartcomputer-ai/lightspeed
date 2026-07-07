use super::*;

pub(super) async fn call_llm_generate(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: LlmGenerationRequest,
) -> anyhow::Result<engine::LlmGenerationResult> {
    ctx.start_activity(
        WorkflowActivities::llm_generate,
        LlmGenerateActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

pub(super) async fn call_context_compact(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: engine::ContextCompactionRequest,
) -> anyhow::Result<engine::ContextCompactionResult> {
    ctx.start_activity(
        WorkflowActivities::context_compact,
        crate::ContextCompactActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

pub(super) async fn call_tool_invoke_batch(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: ToolInvocationBatchRequest,
) -> anyhow::Result<engine::ToolBatchOutcome> {
    ctx.start_activity(
        WorkflowActivities::tool_invoke_batch,
        ToolInvokeBatchActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}
