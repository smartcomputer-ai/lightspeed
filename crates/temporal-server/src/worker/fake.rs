use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError, CoreAgentLlm,
    CoreAgentTools, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ObservedToolCall, ToolCallId, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationResult, ToolKind, ToolName, ToolTargetRequirement,
    storage::BlobStore,
};
use serde_json::Value;

use crate::worker::FAKE_TOOL_NAME;

#[derive(Clone)]
pub struct FakeLlm {
    blobs: Arc<dyn BlobStore>,
}

impl FakeLlm {
    pub fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
        tool_name: ToolName,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let arguments = serde_json::json!({
            "text": format!("echo from run {} turn {}", request.run_id, request.turn_id)
        });
        let argument_bytes = serde_json::to_vec(&arguments).map_err(io_error)?;
        let arguments_ref = self
            .blobs
            .put_bytes(argument_bytes)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("agent_call_{}", request.turn_id.as_u64()));
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
                provider_kind: Some("fake".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-tool-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fake".to_owned()),
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
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let text = format!("Fake agent completed run {}.", request.run_id);
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
                preview: Some("fake final answer".to_owned()),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-final-{}", request.turn_id.as_u64())),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
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
        if request_has_tool_result(&request) {
            return self.final_result(&request).await;
        }
        match invocable_fake_tool(&request) {
            Some(tool_name) => self.tool_call_result(&request, tool_name).await,
            None => self.final_result(&request).await,
        }
    }
}

#[derive(Clone)]
pub struct FakeTools {
    blobs: Arc<dyn BlobStore>,
}

impl FakeTools {
    pub fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }
}

#[async_trait]
impl CoreAgentTools for FakeTools {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
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
            let output = format!("{}: {text}", call.tool_name);
            let output_ref = self
                .blobs
                .put_bytes(output.into_bytes())
                .await
                .map_err(io_error)?;
            results.push(ToolInvocationResult {
                call_id: call.call_id.clone(),
                status: ToolCallStatus::Succeeded,
                output_ref: Some(output_ref.clone()),
                model_visible_output_ref: Some(output_ref),
                error_ref: None,
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
}

fn request_has_tool_result(request: &LlmGenerationRequest) -> bool {
    request
        .request
        .context
        .entries
        .iter()
        .any(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }))
}

/// Picks a tool the fake model can call from the planned request toolset,
/// preferring the canonical fake echo tool when it is registered. Returns
/// `None` when the session has no client-invocable function tool, in which
/// case the fake model answers directly.
fn invocable_fake_tool(request: &LlmGenerationRequest) -> Option<ToolName> {
    let tools = &request.request.tools;
    if let Some(tool) = tools
        .iter()
        .find(|tool| tool.name.as_str() == FAKE_TOOL_NAME)
    {
        return Some(tool.name.clone());
    }
    tools
        .iter()
        .find(|tool| {
            matches!(tool.kind, ToolKind::Function(_))
                && matches!(
                    tool.target_requirement,
                    ToolTargetRequirement::None | ToolTargetRequirement::Optional { .. }
                )
        })
        .map(|tool| tool.name.clone())
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}
