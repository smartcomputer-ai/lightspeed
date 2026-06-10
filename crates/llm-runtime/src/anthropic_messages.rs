//! Anthropic Messages adapter.
//!
//! Lowers the engine's provider-neutral [`LlmRequest`] intent into native
//! Anthropic Messages API requests and maps responses back into context
//! entries and reducer facts, mirroring the OpenAI Responses adapter.
//!
//! Anthropic has no server-side compaction endpoint, so the standalone
//! compaction path runs a summarization request over the compactable context
//! and returns the summary as a user-visible replacement message.

use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND, ANTHROPIC_MESSAGES_MCP_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_MCP_TOOL_USE_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND, CompactionPolicy,
    ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    ContextCompactionTask, ContextEntry, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus,
    LlmRequest, LlmUsage, ObservedToolCall, ProviderApiKind, ProviderNativeToolExecution,
    RemoteMcpApprovalPolicy, RemoteMcpToolSpec, TokenEstimate, TokenEstimateQuality, ToolCallId,
    ToolChoice, ToolChoiceMode, ToolKind, ToolName, ToolSpec,
    storage::BlobStore,
};
use llm_clients::{ApiResponse, anthropic::messages as am};
use serde_json::{Value, json};

use crate::{
    blob_io::{put_json, put_text, read_json, read_text},
    error::{LlmAdapterError, LlmAdapterResult},
    executor::{LlmCompactionAdapter, LlmGenerationAdapter},
    params::anthropic_messages_params,
    result::LlmGenerationExecution,
};

const PROVIDER_KIND_TEXT: &str = "anthropic.messages.text";
const PROVIDER_KIND_TOOL_USE: &str = "anthropic.messages.tool_use";
const PROVIDER_KIND_THINKING: &str = "anthropic.messages.thinking";
const PROVIDER_KIND_BLOCK: &str = "anthropic.messages.block";
/// Client-seeded raw input message: a `ProviderOpaque` entry tagged with this
/// provider kind carries a complete Anthropic `{role, content}` message JSON
/// and lowers as that message instead of an assistant content block.
pub const ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND: &str =
    "anthropic.messages.input_message";
const MEDIA_TYPE_JSON: &str = "application/json";
const MEDIA_TYPE_TEXT: &str = "text/plain";

/// Anthropic requires `max_tokens`; used when the session sets no
/// `output_limit`.
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 4096;
/// Output budget for summarization-based compaction when the task carries no
/// `target_tokens`.
const DEFAULT_COMPACTION_MAX_TOKENS: u64 = 2048;

const COMPACTION_INSTRUCTION: &str = "Summarize the conversation above for context compaction. \
Capture the user's goals, decisions made, work completed, important tool results, and open \
questions. The summary will replace the prior conversation history, so include everything needed \
to continue seamlessly. Reply with the summary only.";

#[async_trait]
pub trait AnthropicMessagesApi: Send + Sync {
    async fn create(
        &self,
        request: am::CreateMessageRequest,
    ) -> Result<ApiResponse<am::Message>, llm_clients::LlmApiError>;
}

#[async_trait]
impl AnthropicMessagesApi for am::Client {
    async fn create(
        &self,
        request: am::CreateMessageRequest,
    ) -> Result<ApiResponse<am::Message>, llm_clients::LlmApiError> {
        am::Client::create(self, request).await
    }
}

#[derive(Clone)]
pub struct AnthropicMessagesLlmAdapter {
    client: Arc<dyn AnthropicMessagesApi>,
    blobs: Arc<dyn BlobStore>,
}

impl AnthropicMessagesLlmAdapter {
    pub fn new(client: Arc<dyn AnthropicMessagesApi>, blobs: Arc<dyn BlobStore>) -> Self {
        Self { client, blobs }
    }

    pub async fn materialize_create_request(
        &self,
        request: &LlmRequest,
    ) -> LlmAdapterResult<am::CreateMessageRequest> {
        materialize_create_request(self.blobs.as_ref(), request).await
    }

    pub async fn materialize_compact_request(
        &self,
        task: &ContextCompactionTask,
    ) -> LlmAdapterResult<am::CreateMessageRequest> {
        materialize_compact_request(self.blobs.as_ref(), task).await
    }
}

#[async_trait]
impl LlmGenerationAdapter for AnthropicMessagesLlmAdapter {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> LlmAdapterResult<LlmGenerationExecution> {
        if request.request.model.api_kind != ProviderApiKind::AnthropicMessages {
            return Err(LlmAdapterError::RequestKindMismatch {
                message: format!(
                    "expected AnthropicMessages request, got {:?}",
                    request.request.model.api_kind
                ),
            });
        }

        let provider_request = self.materialize_create_request(&request.request).await?;
        let provider_request_ref = put_json(self.blobs.as_ref(), &provider_request).await?;
        let response = self.client.create(provider_request).await?;
        let raw_response_ref = put_json(self.blobs.as_ref(), &response.raw_json).await?;
        let result = result_from_response(self.blobs.as_ref(), &request, &response).await?;

        Ok(LlmGenerationExecution {
            result,
            provider_request_ref,
            raw_response_ref,
        })
    }
}

#[async_trait]
impl LlmCompactionAdapter for AnthropicMessagesLlmAdapter {
    async fn compact_context(
        &self,
        request: ContextCompactionRequest,
    ) -> LlmAdapterResult<ContextCompactionResult> {
        if request.request.model.api_kind != ProviderApiKind::AnthropicMessages {
            return Err(LlmAdapterError::RequestKindMismatch {
                message: format!(
                    "expected AnthropicMessages compaction task, got {:?}",
                    request.request.model.api_kind
                ),
            });
        }
        let provider_request = self.materialize_compact_request(&request.request).await?;
        let _provider_request_ref = put_json(self.blobs.as_ref(), &provider_request).await?;
        let response = self.client.create(provider_request).await?;
        let _raw_response_ref = put_json(self.blobs.as_ref(), &response.raw_json).await?;
        result_from_compact_response(self.blobs.as_ref(), &request, &response).await
    }
}

pub async fn materialize_create_request(
    blobs: &dyn BlobStore,
    request: &LlmRequest,
) -> LlmAdapterResult<am::CreateMessageRequest> {
    let params = anthropic_messages_params(request.params.as_ref())?;
    if request.provider_response_id.is_some() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "Anthropic Messages has no provider response continuation; \
                      provider_response_id must be empty"
                .to_owned(),
        });
    }
    if matches!(
        request.compaction,
        Some(CompactionPolicy::ProviderTriggered { .. })
    ) {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "Anthropic Messages does not support provider-triggered compaction; \
                      use the provider-standalone compaction policy"
                .to_owned(),
        });
    }

    let max_tokens = request
        .output_limit
        .map(u64::from)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    if let Some(thinking) = params.thinking.as_ref()
        && let Some(budget_tokens) = thinking.budget_tokens
        && u64::from(budget_tokens) >= max_tokens
    {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "thinking budget_tokens {budget_tokens} must be below max output tokens {max_tokens}"
            ),
        });
    }

    let system = materialize_system(blobs, &request.context.entries).await?;
    let message_entries = request
        .context
        .entries
        .iter()
        .filter(|entry| !matches!(entry.kind, ContextEntryKind::Instructions))
        .cloned()
        .collect::<Vec<_>>();
    let messages = materialize_messages(blobs, &message_entries).await?;
    let (tools, mcp_servers) = materialize_tools(blobs, &request.tools).await?;

    Ok(am::CreateMessageRequest {
        model: request.model.model.clone(),
        max_tokens,
        messages,
        system,
        metadata: params.metadata.clone(),
        stop_sequences: non_empty(params.stop_sequences.clone()),
        stream: params.stream,
        temperature: optional_f64(params.temperature.as_ref(), "temperature")?,
        thinking: params.thinking.as_ref().map(|thinking| am::Thinking {
            r#type: thinking.r#type.clone(),
            budget_tokens: thinking.budget_tokens.map(u64::from),
            display: thinking.display.clone(),
            extra: thinking.extra.clone(),
        }),
        output_config: params.output_config.clone(),
        tool_choice: request.tool_choice.as_ref().map(anthropic_tool_choice),
        tools: non_empty(tools),
        top_k: params.top_k.map(u64::from),
        top_p: optional_f64(params.top_p.as_ref(), "top_p")?,
        service_tier: params.service_tier.clone(),
        container: params.container.clone(),
        mcp_servers: non_empty(mcp_servers).map(Value::from),
        extra: params.extra.clone(),
    })
}

pub async fn materialize_compact_request(
    blobs: &dyn BlobStore,
    task: &ContextCompactionTask,
) -> LlmAdapterResult<am::CreateMessageRequest> {
    let mut messages = materialize_messages(blobs, &task.context.entries).await?;
    messages.push(am::MessageParam::user(compaction_instruction(
        task.target_tokens,
    )));
    Ok(am::CreateMessageRequest {
        model: task.model.model.clone(),
        max_tokens: task
            .target_tokens
            .map(u64::from)
            .unwrap_or(DEFAULT_COMPACTION_MAX_TOKENS),
        messages,
        system: None,
        metadata: None,
        stop_sequences: None,
        stream: None,
        temperature: None,
        thinking: None,
        output_config: None,
        tool_choice: None,
        tools: None,
        top_k: None,
        top_p: None,
        service_tier: None,
        container: None,
        mcp_servers: None,
        extra: Default::default(),
    })
}

fn compaction_instruction(target_tokens: Option<u32>) -> String {
    match target_tokens {
        Some(target_tokens) => {
            format!("{COMPACTION_INSTRUCTION} Keep the summary under {target_tokens} tokens.")
        }
        None => COMPACTION_INSTRUCTION.to_owned(),
    }
}

async fn materialize_system(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
) -> LlmAdapterResult<Option<am::SystemContent>> {
    let mut parts = Vec::new();
    for entry in entries {
        if matches!(entry.kind, ContextEntryKind::Instructions) {
            let text = read_text(blobs, &entry.content_ref).await?;
            let text = text.trim();
            if !text.is_empty() {
                parts.push(text.to_owned());
            }
        }
    }
    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(am::SystemContent::Text(parts.join("\n\n"))))
    }
}

/// Anthropic groups assistant `tool_use`/`thinking` blocks and the following
/// `tool_result` blocks into role-alternating messages, so consecutive context
/// entries with the same effective role merge into one message.
async fn materialize_messages(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
) -> LlmAdapterResult<Vec<am::MessageParam>> {
    let mut messages: Vec<am::MessageParam> = Vec::new();
    for entry in entries {
        if is_raw_input_message(entry) {
            let (role, blocks) = materialize_input_message(blobs, entry).await?;
            for block in blocks {
                push_block(&mut messages, role, block)?;
            }
            continue;
        }
        let (role, block) = materialize_block(blobs, entry).await?;
        push_block(&mut messages, role, block)?;
    }
    Ok(messages)
}

fn push_block(
    messages: &mut Vec<am::MessageParam>,
    role: am::MessageRole,
    block: am::ContentBlockParam,
) -> LlmAdapterResult<()> {
    match messages.last_mut() {
        Some(message) if message.role == role => match &mut message.content {
            am::MessageParamContent::Blocks(blocks) => blocks.push(block),
            am::MessageParamContent::Text(_) => {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: "Anthropic message lowering produced unexpected text content"
                        .to_owned(),
                });
            }
        },
        _ => {
            messages.push(am::MessageParam {
                role,
                content: am::MessageParamContent::Blocks(vec![block]),
                extra: Default::default(),
            });
        }
    }
    Ok(())
}

fn is_raw_input_message(entry: &ContextEntry) -> bool {
    matches!(entry.kind, ContextEntryKind::ProviderOpaque)
        && entry.provider_kind.as_deref() == Some(ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND)
}

async fn materialize_input_message(
    blobs: &dyn BlobStore,
    entry: &ContextEntry,
) -> LlmAdapterResult<(am::MessageRole, Vec<am::ContentBlockParam>)> {
    let raw = read_json(blobs, &entry.content_ref).await?;
    let message: am::MessageParam =
        serde_json::from_value(raw).map_err(|error| LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "Anthropic raw input message entry {} is not a valid message: {error}",
                entry.entry_id
            ),
        })?;
    let blocks = match message.content {
        am::MessageParamContent::Text(text) => vec![am::ContentBlockParam::text(text)],
        am::MessageParamContent::Blocks(blocks) => blocks,
    };
    Ok((message.role, blocks))
}

async fn materialize_block(
    blobs: &dyn BlobStore,
    entry: &ContextEntry,
) -> LlmAdapterResult<(am::MessageRole, am::ContentBlockParam)> {
    match &entry.kind {
        ContextEntryKind::Message { role } => {
            let text = read_text(blobs, &entry.content_ref).await?;
            let role = match role {
                ContextMessageRole::User => am::MessageRole::User,
                ContextMessageRole::Assistant => am::MessageRole::Assistant,
            };
            Ok((role, am::ContentBlockParam::text(text)))
        }
        ContextEntryKind::ToolResult { call_id, is_error } => {
            let output = read_text(blobs, &entry.content_ref).await?;
            Ok((
                am::MessageRole::User,
                am::ContentBlockParam::ToolResult(am::ToolResultBlockParam {
                    r#type: "tool_result".to_owned(),
                    tool_use_id: call_id.as_str().to_owned(),
                    content: Some(Value::String(output)),
                    is_error: if *is_error { Some(true) } else { None },
                    cache_control: None,
                    extra: Default::default(),
                }),
            ))
        }
        ContextEntryKind::Instructions => Err(LlmAdapterError::InvalidProviderRequest {
            message: "instruction context entries must materialize as the system prompt"
                .to_owned(),
        }),
        ContextEntryKind::SkillCatalog => {
            let catalog = crate::skill_prompts::read_skill_catalog(blobs, &entry.content_ref).await?;
            Ok((
                am::MessageRole::User,
                am::ContentBlockParam::text(crate::skill_prompts::skill_catalog_text(&catalog)),
            ))
        }
        ContextEntryKind::SkillActivation { skill_id } => {
            let text = read_text(blobs, &entry.content_ref).await?;
            Ok((
                am::MessageRole::User,
                am::ContentBlockParam::text(crate::skill_prompts::skill_activation_text(
                    skill_id, text,
                )),
            ))
        }
        ContextEntryKind::ToolCall { .. }
        | ContextEntryKind::ReasoningState
        | ContextEntryKind::ProviderOpaque => {
            if entry.media_type.as_deref() != Some(MEDIA_TYPE_JSON) {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: format!(
                        "Anthropic context entry {} must carry a raw JSON content block",
                        entry.entry_id
                    ),
                });
            }
            let raw = read_json(blobs, &entry.content_ref).await?;
            Ok((am::MessageRole::Assistant, am::ContentBlockParam::Raw(raw)))
        }
    }
}

async fn materialize_tools(
    blobs: &dyn BlobStore,
    tools: &[ToolSpec],
) -> LlmAdapterResult<(Vec<am::Tool>, Vec<Value>)> {
    let mut materialized = Vec::new();
    let mut mcp_servers = Vec::new();
    for tool in tools {
        match &tool.kind {
            ToolKind::Function(function) => {
                let mut definition = am::ToolDefinition::new(
                    function.model_name.as_ref().unwrap_or(&tool.name).as_str(),
                    read_json(blobs, &function.input_schema_ref).await?,
                );
                definition.description = match &function.description_ref {
                    Some(blob_ref) => Some(read_text(blobs, blob_ref).await?),
                    None => None,
                };
                // Anthropic has no strict-mode switch; the input schema is the
                // only contract, so `strict` does not lower to anything.
                if let Some(provider_options_ref) = &function.provider_options_ref {
                    let options = read_json(blobs, provider_options_ref).await?;
                    let Some(options) = options.as_object() else {
                        return Err(LlmAdapterError::InvalidProviderRequest {
                            message: format!(
                                "provider options for tool {} must be a JSON object",
                                tool.name
                            ),
                        });
                    };
                    for (key, value) in options {
                        definition.extra.insert(key.clone(), value.clone());
                    }
                }
                materialized.push(am::Tool::Custom(definition));
            }
            ToolKind::ProviderNative(native) => {
                if native.api_kind != ProviderApiKind::AnthropicMessages {
                    return Err(LlmAdapterError::InvalidProviderRequest {
                        message: format!(
                            "provider-native tool {} targets {:?}, not AnthropicMessages",
                            tool.name, native.api_kind
                        ),
                    });
                }
                match native.execution {
                    ProviderNativeToolExecution::ProviderHosted
                    | ProviderNativeToolExecution::ClientEffect => {
                        materialized.push(am::Tool::Raw(
                            read_json(blobs, &native.native_tool_ref).await?,
                        ));
                    }
                }
            }
            ToolKind::RemoteMcp(remote_mcp) => {
                mcp_servers.push(materialize_remote_mcp_server(blobs, tool, remote_mcp).await?);
            }
        }
    }
    Ok((materialized, mcp_servers))
}

async fn materialize_remote_mcp_server(
    _blobs: &dyn BlobStore,
    tool: &ToolSpec,
    remote_mcp: &RemoteMcpToolSpec,
) -> LlmAdapterResult<Value> {
    if remote_mcp.auth_ref.is_some() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "remote MCP tool {} requires auth, but Anthropic Messages MCP auth injection is not implemented yet",
                tool.name
            ),
        });
    }
    if matches!(remote_mcp.approval, RemoteMcpApprovalPolicy::Always) {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "remote MCP tool {} requires approval, but the Anthropic MCP connector has no approval flow",
                tool.name
            ),
        });
    }

    let mut value = json!({
        "type": "url",
        "url": remote_mcp.server_url,
        "name": remote_mcp.server_label,
    });
    let object = value.as_object_mut().expect("mcp server object");
    if let Some(allowed_tools) = &remote_mcp.allowed_tools {
        object.insert(
            "tool_configuration".to_owned(),
            json!({ "allowed_tools": allowed_tools }),
        );
    }
    Ok(value)
}

fn anthropic_tool_choice(choice: &ToolChoice) -> am::ToolChoice {
    let mut materialized = match &choice.mode {
        ToolChoiceMode::Auto => am::ToolChoice::auto(),
        ToolChoiceMode::None => am::ToolChoice::none(),
        ToolChoiceMode::RequiredAny => am::ToolChoice::any(),
        ToolChoiceMode::Specific { tool_name } => am::ToolChoice::tool(tool_name.as_str()),
    };
    if !matches!(choice.mode, ToolChoiceMode::None) {
        materialized.disable_parallel_tool_use = choice.disable_parallel_tool_use;
    }
    materialized
}

fn optional_f64(value: Option<&Value>, name: &'static str) -> LlmAdapterResult<Option<f64>> {
    value
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
                    message: format!("{name} must be a JSON number"),
                })
        })
        .transpose()
}

fn non_empty<T>(entries: Vec<T>) -> Option<Vec<T>> {
    if entries.is_empty() { None } else { Some(entries) }
}

pub async fn result_from_response(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    response: &ApiResponse<am::Message>,
) -> LlmAdapterResult<LlmGenerationResult> {
    let mut context_entries = Vec::new();
    let mut tool_calls = Vec::new();

    for (index, block) in response.parsed.content.iter().enumerate() {
        let raw_block = raw_content_block(&response.raw_json, index, block)?;
        match block.r#type.as_str() {
            "text" => {
                if let Some(entry) = text_context_entry(blobs, block).await? {
                    context_entries.push(entry);
                }
            }
            "tool_use" => {
                let (entry, tool_call) =
                    tool_use_context(blobs, block, raw_block, index).await?;
                context_entries.push(entry);
                tool_calls.push(tool_call);
            }
            "thinking" | "redacted_thinking" => {
                context_entries.push(thinking_context_entry(blobs, block, raw_block).await?);
            }
            _ => {
                context_entries.push(opaque_context_entry(blobs, block, raw_block).await?);
            }
        }
    }

    let usage = response.parsed.usage.as_ref().map(llm_usage);
    let context_token_estimate = response
        .parsed
        .usage
        .as_ref()
        .and_then(prompt_tokens)
        .map(|tokens| TokenEstimate {
            tokens: u64_to_u32(tokens),
            quality: TokenEstimateQuality::ProviderCounted,
        });
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status: LlmGenerationStatus::Succeeded,
        failure_ref: None,
        context_entries,
        facts: LlmGenerationFacts {
            provider_response_id: Some(response.parsed.id.clone()),
            finish: finish_reason(response.parsed.stop_reason, !tool_calls.is_empty()),
            usage,
            tool_calls,
            context_token_estimate,
        },
    })
}

pub async fn result_from_compact_response(
    blobs: &dyn BlobStore,
    request: &ContextCompactionRequest,
    response: &ApiResponse<am::Message>,
) -> LlmAdapterResult<ContextCompactionResult> {
    let summary = response.parsed.output_text();
    let summary = summary.trim();
    if summary.is_empty() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "Anthropic Messages compaction response {} did not include summary text",
                response.parsed.id
            ),
        });
    }
    let content_ref = put_text(blobs, summary).await?;
    Ok(ContextCompactionResult {
        session_id: request.session_id.clone(),
        context_revision: request.request.context.context_revision,
        status: ContextCompactionStatus::Succeeded,
        failure_ref: None,
        context_entries: vec![ContextEntryInput {
            // The summary replaces compactable history as a user-role message
            // so the next request still starts with a user turn.
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content_ref,
            media_type: Some(MEDIA_TYPE_TEXT.to_owned()),
            preview: Some(summary.to_owned()),
            provider_kind: Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND.to_owned()),
            provider_item_id: Some(response.parsed.id.clone()),
            token_estimate: None,
        }],
    })
}

fn raw_content_block(
    raw_response: &Value,
    index: usize,
    block: &am::ContentBlock,
) -> LlmAdapterResult<Value> {
    if let Some(raw_block) = raw_response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.get(index))
    {
        return Ok(raw_block.clone());
    }
    serde_json::to_value(block).map_err(|error| LlmAdapterError::InvalidProviderRequest {
        message: format!("failed to encode Anthropic content block: {error}"),
    })
}

async fn text_context_entry(
    blobs: &dyn BlobStore,
    block: &am::ContentBlock,
) -> LlmAdapterResult<Option<ContextEntryInput>> {
    let Some(text) = block.text.as_deref().filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let content_ref = put_text(blobs, text).await?;
    Ok(Some(ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        },
        content_ref,
        media_type: Some(MEDIA_TYPE_TEXT.to_owned()),
        preview: Some(text.to_owned()),
        provider_kind: Some(PROVIDER_KIND_TEXT.to_owned()),
        provider_item_id: None,
        token_estimate: None,
    }))
}

async fn tool_use_context(
    blobs: &dyn BlobStore,
    block: &am::ContentBlock,
    raw_block: Value,
    index: usize,
) -> LlmAdapterResult<(ContextEntryInput, ObservedToolCall)> {
    let call_id = block
        .id
        .clone()
        .unwrap_or_else(|| format!("toolu_{index}"));
    let call_id = ToolCallId::try_new(call_id.clone()).map_err(|error| {
        LlmAdapterError::InvalidProviderRequest {
            message: format!("invalid Anthropic tool call id {call_id:?}: {error}"),
        }
    })?;
    let name = block
        .name
        .as_deref()
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: "Anthropic tool_use block is missing name".to_owned(),
        })?;
    let tool_name = ToolName::try_new(name.to_owned()).map_err(|error| {
        LlmAdapterError::InvalidProviderRequest {
            message: format!("invalid Anthropic tool name {name:?}: {error}"),
        }
    })?;
    let arguments = block.input.clone().unwrap_or_else(|| json!({}));
    let arguments_ref = put_json(blobs, &arguments).await?;
    let native_call_ref = put_json(blobs, &raw_block).await?;

    let context_entry = ContextEntryInput {
        kind: ContextEntryKind::ToolCall {
            call_id: call_id.clone(),
            name: tool_name.clone(),
        },
        content_ref: native_call_ref.clone(),
        media_type: Some(MEDIA_TYPE_JSON.to_owned()),
        preview: Some(format!("{tool_name}({arguments})")),
        provider_kind: Some(PROVIDER_KIND_TOOL_USE.to_owned()),
        provider_item_id: block.id.clone(),
        token_estimate: None,
    };
    let tool_call = ObservedToolCall {
        call_id,
        tool_name,
        provider_kind: Some(PROVIDER_KIND_TOOL_USE.to_owned()),
        arguments_ref,
        native_call_ref: Some(native_call_ref),
    };
    Ok((context_entry, tool_call))
}

async fn thinking_context_entry(
    blobs: &dyn BlobStore,
    block: &am::ContentBlock,
    raw_block: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let content_ref = put_json(blobs, &raw_block).await?;
    let preview = block
        .thinking
        .as_deref()
        .filter(|thinking| !thinking.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "redacted thinking".to_owned());
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ReasoningState,
        content_ref,
        media_type: Some(MEDIA_TYPE_JSON.to_owned()),
        preview: Some(preview),
        provider_kind: Some(PROVIDER_KIND_THINKING.to_owned()),
        provider_item_id: None,
        token_estimate: None,
    })
}

/// Server-side tool blocks (web search, code execution, MCP connector) and
/// any future block types are preserved verbatim so the next request replays
/// the assistant turn exactly as the provider produced it.
async fn opaque_context_entry(
    blobs: &dyn BlobStore,
    block: &am::ContentBlock,
    raw_block: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let provider_kind = match block.r#type.as_str() {
        "server_tool_use" => ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND,
        "mcp_tool_use" => ANTHROPIC_MESSAGES_MCP_TOOL_USE_PROVIDER_KIND,
        "mcp_tool_result" => ANTHROPIC_MESSAGES_MCP_TOOL_RESULT_PROVIDER_KIND,
        kind if kind.ends_with("_tool_result") => {
            ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND
        }
        _ => PROVIDER_KIND_BLOCK,
    };
    let content_ref = put_json(blobs, &raw_block).await?;
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content_ref,
        media_type: Some(MEDIA_TYPE_JSON.to_owned()),
        preview: Some(opaque_preview(block)),
        provider_kind: Some(provider_kind.to_owned()),
        provider_item_id: block.id.clone(),
        token_estimate: None,
    })
}

fn opaque_preview(block: &am::ContentBlock) -> String {
    match (block.r#type.as_str(), block.name.as_deref()) {
        ("server_tool_use", Some(name)) => {
            format!("Anthropic Messages server tool call: {name}")
        }
        ("mcp_tool_use", Some(name)) => format!("Anthropic Messages MCP tool call: {name}"),
        ("mcp_tool_result", _) => "Anthropic Messages MCP tool result".to_owned(),
        (kind, _) => format!("Anthropic Messages {kind} block"),
    }
}

fn finish_reason(stop_reason: Option<am::StopReason>, has_tool_calls: bool) -> LlmFinish {
    match stop_reason {
        Some(am::StopReason::ToolUse) => LlmFinish::ToolCalls,
        Some(am::StopReason::EndTurn | am::StopReason::StopSequence) => LlmFinish::Stop,
        Some(am::StopReason::MaxTokens) => LlmFinish::Length,
        Some(am::StopReason::Refusal) => LlmFinish::ContentFilter,
        Some(am::StopReason::ModelContextWindow) => LlmFinish::ContextLimit,
        Some(am::StopReason::PauseTurn | am::StopReason::Unknown) => LlmFinish::Unknown,
        None if has_tool_calls => LlmFinish::ToolCalls,
        None => LlmFinish::Unknown,
    }
}

fn llm_usage(usage: &am::Usage) -> LlmUsage {
    let input_tokens = prompt_tokens(usage);
    let output_tokens = usage.output_tokens;
    LlmUsage {
        input_tokens: input_tokens.map(u64_to_u32),
        output_tokens: output_tokens.map(u64_to_u32),
        reasoning_tokens: None,
        total_tokens: match (input_tokens, output_tokens) {
            (Some(input), Some(output)) => Some(u64_to_u32(input + output)),
            _ => None,
        },
    }
}

/// Anthropic reports cache reads/writes separately from `input_tokens`; the
/// full prompt size is the sum of all three.
fn prompt_tokens(usage: &am::Usage) -> Option<u64> {
    let mut total = usage.input_tokens?;
    total += usage.cache_creation_input_tokens.unwrap_or(0);
    total += usage.cache_read_input_tokens.unwrap_or(0);
    Some(total)
}

fn u64_to_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use engine::{
        BlobRef, ContextEntryId, ContextEntrySource, ContextSnapshot, CoreAgentLlm,
        FunctionToolSpec, ModelSelection, ProviderParams, RunId, SessionId, ToolParallelism,
        TurnId, storage::InMemoryBlobStore,
    };
    use llm_clients::HeaderSnapshot;
    use serde_json::json;

    use super::*;
    use crate::executor::{LlmAdapterRegistry, LlmRuntime};
    use crate::params::{AnthropicMessagesParams, AnthropicThinkingConfig};

    struct FakeAnthropicMessagesApi {
        response: ApiResponse<am::Message>,
        seen: Mutex<Vec<am::CreateMessageRequest>>,
    }

    #[async_trait]
    impl AnthropicMessagesApi for FakeAnthropicMessagesApi {
        async fn create(
            &self,
            request: am::CreateMessageRequest,
        ) -> Result<ApiResponse<am::Message>, llm_clients::LlmApiError> {
            self.seen.lock().expect("lock").push(request);
            Ok(self.response.clone())
        }
    }

    fn fake_api(raw_json: Value) -> Arc<FakeAnthropicMessagesApi> {
        Arc::new(FakeAnthropicMessagesApi {
            response: ApiResponse {
                parsed: serde_json::from_value(raw_json.clone()).expect("message"),
                raw_json,
                status: 200,
                headers: HeaderSnapshot::default(),
            },
            seen: Mutex::new(Vec::new()),
        })
    }

    async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
        blobs.insert_text(text).await
    }

    fn model() -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::AnthropicMessages,
            provider_id: "anthropic".to_string(),
            model: "claude-opus-4-8".to_string(),
        }
    }

    fn intent_request(entries: Vec<ContextEntry>) -> LlmRequest {
        LlmRequest {
            model: model(),
            request_fingerprint: "sha256:test".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::AnthropicMessages,
                context_revision: 0,
                entries,
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: None,
            provider_response_id: None,
            compaction: None,
            params: None,
        }
    }

    fn anthropic_params(params: &AnthropicMessagesParams) -> ProviderParams {
        ProviderParams::new(
            ProviderApiKind::AnthropicMessages,
            serde_json::to_value(params).expect("serialize params"),
        )
    }

    fn user_entry(entry_id: u64, content_ref: BlobRef) -> ContextEntry {
        ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(entry_id),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn retained_context_entry(index: usize, item: &ContextEntryInput) -> ContextEntry {
        ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(index as u64 + 1),
            kind: item.kind.clone(),
            source: match item.kind {
                ContextEntryKind::ReasoningState => ContextEntrySource::Reasoning {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
                _ => ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
            },
            content_ref: item.content_ref.clone(),
            media_type: item.media_type.clone(),
            preview: item.preview.clone(),
            provider_kind: item.provider_kind.clone(),
            provider_item_id: item.provider_item_id.clone(),
            token_estimate: item.token_estimate.clone(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_maps_context_tools_and_params() {
        let blobs = InMemoryBlobStore::new();
        let instructions_ref = text_blob(&blobs, "Be precise.").await;
        let input_ref = text_blob(&blobs, "Read Cargo.toml").await;
        let description_ref = text_blob(&blobs, "Read a file").await;
        let schema_ref = crate::blob_io::put_json(
            &blobs,
            &json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        )
        .await
        .expect("schema");
        let provider_options_ref =
            crate::blob_io::put_json(&blobs, &json!({ "cache_control": { "type": "ephemeral" } }))
                .await
                .expect("provider options");
        let instructions_item = ContextEntry {
            key: Some(engine::ContextEntryKey::new("instructions.000.test")),
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::Instructions,
            source: ContextEntrySource::ContextEdit,
            content_ref: instructions_ref,
            media_type: Some("text/plain".to_owned()),
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        };
        let mut request = intent_request(vec![instructions_item, user_entry(2, input_ref)]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("read_file"),
            kind: ToolKind::Function(FunctionToolSpec {
                model_name: None,
                description_ref: Some(description_ref),
                input_schema_ref: schema_ref,
                output_schema_ref: None,
                strict: Some(true),
                provider_options_ref: Some(provider_options_ref),
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: Default::default(),
        }];
        request.tool_choice = Some(ToolChoice {
            mode: ToolChoiceMode::Specific {
                tool_name: ToolName::new("read_file"),
            },
            disable_parallel_tool_use: Some(true),
        });
        request.output_limit = Some(2048);
        request.params = Some(anthropic_params(&AnthropicMessagesParams {
            thinking: Some(AnthropicThinkingConfig {
                r#type: "enabled".to_string(),
                budget_tokens: Some(1024),
                display: None,
                extra: BTreeMap::new(),
            }),
            output_config: Some(json!({ "effort": "high" })),
            metadata: Some(json!({ "user_id": "user-1" })),
            stop_sequences: vec!["<END>".to_string()],
            stream: Some(false),
            temperature: Some(json!(0.2)),
            top_k: Some(16),
            top_p: Some(json!(0.9)),
            service_tier: Some("auto".to_string()),
            container: None,
            extra: BTreeMap::from([("betas".to_string(), json!(["context-1m"]))]),
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value,
            json!({
                "model": "claude-opus-4-8",
                "max_tokens": 2048,
                "messages": [{
                    "role": "user",
                    "content": [{ "type": "text", "text": "Read Cargo.toml" }]
                }],
                "system": "Be precise.",
                "metadata": { "user_id": "user-1" },
                "stop_sequences": ["<END>"],
                "stream": false,
                "temperature": 0.2,
                "thinking": { "type": "enabled", "budget_tokens": 1024 },
                "output_config": { "effort": "high" },
                "tool_choice": {
                    "type": "tool",
                    "name": "read_file",
                    "disable_parallel_tool_use": true
                },
                "tools": [{
                    "name": "read_file",
                    "description": "Read a file",
                    "input_schema": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    },
                    "cache_control": { "type": "ephemeral" }
                }],
                "top_k": 16,
                "top_p": 0.9,
                "service_tier": "auto",
                "betas": ["context-1m"]
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_groups_assistant_blocks_and_tool_results() {
        let blobs = InMemoryBlobStore::new();
        let user_ref = text_blob(&blobs, "What is in Cargo.toml?").await;
        let thinking_ref = crate::blob_io::put_json(
            &blobs,
            &json!({ "type": "thinking", "thinking": "Let me look.", "signature": "sig" }),
        )
        .await
        .expect("thinking blob");
        let assistant_ref = text_blob(&blobs, "I'll read it.").await;
        let tool_use_ref = crate::blob_io::put_json(
            &blobs,
            &json!({
                "type": "tool_use",
                "id": "toolu_1",
                "name": "read_file",
                "input": { "path": "Cargo.toml" }
            }),
        )
        .await
        .expect("tool use blob");
        let tool_result_ref = text_blob(&blobs, "[workspace]").await;
        let followup_ref = text_blob(&blobs, "Thanks!").await;

        let entries = vec![
            user_entry(1, user_ref),
            ContextEntry {
                key: None,
                entry_id: ContextEntryId::new(2),
                kind: ContextEntryKind::ReasoningState,
                source: ContextEntrySource::Reasoning {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
                content_ref: thinking_ref,
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                preview: None,
                provider_kind: Some(PROVIDER_KIND_THINKING.to_owned()),
                provider_item_id: None,
                token_estimate: None,
            },
            ContextEntry {
                key: None,
                entry_id: ContextEntryId::new(3),
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                source: ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
                content_ref: assistant_ref,
                media_type: Some("text/plain".to_owned()),
                preview: None,
                provider_kind: Some(PROVIDER_KIND_TEXT.to_owned()),
                provider_item_id: None,
                token_estimate: None,
            },
            ContextEntry {
                key: None,
                entry_id: ContextEntryId::new(4),
                kind: ContextEntryKind::ToolCall {
                    call_id: engine::ToolCallId::new("toolu_1"),
                    name: ToolName::new("read_file"),
                },
                source: ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
                content_ref: tool_use_ref,
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                preview: None,
                provider_kind: Some(PROVIDER_KIND_TOOL_USE.to_owned()),
                provider_item_id: Some("toolu_1".to_owned()),
                token_estimate: None,
            },
            ContextEntry {
                key: None,
                entry_id: ContextEntryId::new(5),
                kind: ContextEntryKind::ToolResult {
                    call_id: engine::ToolCallId::new("toolu_1"),
                    is_error: false,
                },
                source: ContextEntrySource::Tool {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                    batch_id: None,
                },
                content_ref: tool_result_ref,
                media_type: Some("text/plain".to_owned()),
                preview: None,
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            },
            user_entry(6, followup_ref),
        ];
        let request = intent_request(entries);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["messages"],
            json!([
                {
                    "role": "user",
                    "content": [{ "type": "text", "text": "What is in Cargo.toml?" }]
                },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": "Let me look.", "signature": "sig" },
                        { "type": "text", "text": "I'll read it." },
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "read_file",
                            "input": { "path": "Cargo.toml" }
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_1",
                            "content": "[workspace]"
                        },
                        { "type": "text", "text": "Thanks!" }
                    ]
                }
            ])
        );
        assert_eq!(value["max_tokens"], json!(DEFAULT_MAX_OUTPUT_TOKENS));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_lowers_raw_input_messages() {
        let blobs = InMemoryBlobStore::new();
        let raw_message_ref = crate::blob_io::put_json(
            &blobs,
            &json!({ "role": "user", "content": "Remember the marker ZEPHYR-42." }),
        )
        .await
        .expect("raw message blob");
        let followup_ref = text_blob(&blobs, "What marker?").await;
        let raw_entry = ContextEntry {
            key: Some(engine::ContextEntryKey::new("client.anthropic.raw.note")),
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::ProviderOpaque,
            source: ContextEntrySource::ContextEdit,
            content_ref: raw_message_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_owned()),
            preview: None,
            provider_kind: Some(ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND.to_owned()),
            provider_item_id: None,
            token_estimate: None,
        };
        let request = intent_request(vec![raw_entry, user_entry(2, followup_ref)]);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["messages"],
            json!([{
                "role": "user",
                "content": [
                    { "type": "text", "text": "Remember the marker ZEPHYR-42." },
                    { "type": "text", "text": "What marker?" }
                ]
            }])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_lowers_remote_mcp_tool_as_mcp_server() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tools = vec![ToolSpec {
            name: ToolName::new("mcp_echo"),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: None,
                allowed_tools: Some(vec!["echo".to_string()]),
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: None,
                auth_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: Default::default(),
        }];

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["mcp_servers"],
            json!([{
                "type": "url",
                "url": "https://echo.example.com/mcp",
                "name": "echo",
                "tool_configuration": { "allowed_tools": ["echo"] }
            }])
        );
        assert_eq!(value.get("tools"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_rejects_unsupported_intents() {
        let blobs = InMemoryBlobStore::new();

        let mut continuation = intent_request(Vec::new());
        continuation.provider_response_id = Some("msg_prev".to_string());
        let error = materialize_create_request(&blobs, &continuation)
            .await
            .expect_err("continuation must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));

        let mut provider_triggered = intent_request(Vec::new());
        provider_triggered.compaction = Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens: Some(1000),
        });
        let error = materialize_create_request(&blobs, &provider_triggered)
            .await
            .expect_err("provider-triggered compaction must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));

        let mut oversized_thinking = intent_request(Vec::new());
        oversized_thinking.output_limit = Some(1024);
        oversized_thinking.params = Some(anthropic_params(&AnthropicMessagesParams {
            thinking: Some(AnthropicThinkingConfig {
                r#type: "enabled".to_string(),
                budget_tokens: Some(2048),
                display: None,
                extra: BTreeMap::new(),
            }),
            ..AnthropicMessagesParams::default()
        }));
        let error = materialize_create_request(&blobs, &oversized_thinking)
            .await
            .expect_err("thinking budget above max_tokens must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));

        let mut mcp_auth = intent_request(Vec::new());
        mcp_auth.tools = vec![ToolSpec {
            name: ToolName::new("mcp_echo"),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: None,
                allowed_tools: None,
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: None,
                auth_ref: Some(engine::SecretRef {
                    namespace: "auth_grant".to_string(),
                    id: "grant_123".to_string(),
                }),
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: Default::default(),
        }];
        let error = materialize_create_request(&blobs, &mcp_auth)
            .await
            .expect_err("mcp auth must fail");
        assert!(
            error
                .to_string()
                .contains("Anthropic Messages MCP auth injection is not implemented yet"),
            "{error}"
        );

        let mut mcp_approval = intent_request(Vec::new());
        mcp_approval.tools = vec![ToolSpec {
            name: ToolName::new("mcp_echo"),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: None,
                allowed_tools: None,
                approval: RemoteMcpApprovalPolicy::Always,
                defer_loading: None,
                auth_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: Default::default(),
        }];
        let error = materialize_create_request(&blobs, &mcp_approval)
            .await
            .expect_err("mcp approval must fail");
        assert!(
            error.to_string().contains("no approval flow"),
            "{error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn llm_runtime_returns_generation_result_for_anthropic_message() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let input_ref = text_blob(&blobs, "Use the tool").await;
        let raw_json = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "I'll inspect it." },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "read_file",
                    "input": { "path": "Cargo.toml" }
                }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        });
        let api = fake_api(raw_json);
        let adapter = Arc::new(AnthropicMessagesLlmAdapter::new(api.clone(), blobs.clone()));
        let registry = LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::AnthropicMessages, adapter);
        let executor = LlmRuntime::new(registry);
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: {
                let mut request = intent_request(vec![user_entry(1, input_ref)]);
                request.output_limit = Some(256);
                request
            },
        };

        let result = CoreAgentLlm::generate(&executor, request)
            .await
            .expect("generate");

        assert_eq!(result.status, LlmGenerationStatus::Succeeded);
        assert_eq!(result.facts.provider_response_id.as_deref(), Some("msg_1"));
        assert_eq!(result.facts.finish, LlmFinish::ToolCalls);
        assert_eq!(
            result
                .facts
                .usage
                .as_ref()
                .and_then(|usage| usage.total_tokens),
            Some(15)
        );
        assert_eq!(result.facts.tool_calls.len(), 1);
        assert_eq!(
            result.facts.tool_calls[0].tool_name,
            ToolName::new("read_file")
        );
        assert_eq!(
            blobs
                .read_text(&result.facts.tool_calls[0].arguments_ref)
                .await
                .expect("arguments"),
            "{\"path\":\"Cargo.toml\"}"
        );
        assert_eq!(result.context_entries.len(), 2);

        let retained_entries = result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(index, item))
            .collect::<Vec<_>>();
        let followup_request = intent_request(retained_entries);
        let followup = materialize_create_request(blobs.as_ref(), &followup_request)
            .await
            .expect("followup request");
        let followup_json = serde_json::to_value(followup).expect("followup json");
        assert_eq!(
            followup_json["messages"],
            json!([{
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "I'll inspect it." },
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "read_file",
                        "input": { "path": "Cargo.toml" }
                    }
                ]
            }])
        );
        assert_eq!(api.seen.lock().expect("lock").len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_preserves_thinking_blocks_for_replay() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [
                { "type": "thinking", "thinking": "Reading first.", "signature": "sig_1" },
                { "type": "redacted_thinking", "data": "opaque" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "read_file",
                    "input": { "path": "Cargo.toml" }
                }
            ]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("message"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: intent_request(Vec::new()),
        };

        let result = result_from_response(&blobs, &request, &response)
            .await
            .expect("result");

        assert_eq!(result.context_entries.len(), 3);
        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::ReasoningState
        ));
        assert!(matches!(
            result.context_entries[1].kind,
            ContextEntryKind::ReasoningState
        ));
        assert_eq!(
            result.context_entries[1].preview.as_deref(),
            Some("redacted thinking")
        );

        let retained_entries = result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(index, item))
            .collect::<Vec<_>>();
        let followup = materialize_create_request(&blobs, &intent_request(retained_entries))
            .await
            .expect("followup request");
        let followup_json = serde_json::to_value(followup).expect("followup json");
        assert_eq!(followup_json["messages"][0]["role"], "assistant");
        assert_eq!(
            followup_json["messages"][0]["content"][0]["type"],
            "thinking"
        );
        assert_eq!(
            followup_json["messages"][0]["content"][1]["type"],
            "redacted_thinking"
        );
        assert_eq!(
            followup_json["messages"][0]["content"][2]["type"],
            "tool_use"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_captures_server_tool_blocks_as_provider_opaque_context() {
        let blobs = InMemoryBlobStore::new();
        let server_tool_use = json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": { "query": "forge agent runtime" }
        });
        let web_search_result = json!({
            "type": "web_search_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": [{ "type": "web_search_result", "url": "https://example.com" }]
        });
        let raw_json = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [server_tool_use.clone(), web_search_result.clone(), {
                "type": "text",
                "text": "Found it."
            }]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("message"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: intent_request(Vec::new()),
        };

        let result = result_from_response(&blobs, &request, &response)
            .await
            .expect("result");

        assert_eq!(result.context_entries.len(), 3);
        assert!(result.facts.tool_calls.is_empty());
        assert_eq!(
            result.context_entries[0].provider_kind.as_deref(),
            Some(ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[1].provider_kind.as_deref(),
            Some(ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND)
        );
        let retained: Value = read_json(&blobs, &result.context_entries[0].content_ref)
            .await
            .expect("raw server tool use");
        assert_eq!(retained, server_tool_use);
        assert_eq!(result.facts.finish, LlmFinish::Stop);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_maps_stop_reasons_to_finish() {
        for (stop_reason, expected) in [
            ("end_turn", LlmFinish::Stop),
            ("max_tokens", LlmFinish::Length),
            ("refusal", LlmFinish::ContentFilter),
            ("model_context_window", LlmFinish::ContextLimit),
            ("pause_turn", LlmFinish::Unknown),
        ] {
            let blobs = InMemoryBlobStore::new();
            let raw_json = json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "stop_reason": stop_reason,
                "content": [{ "type": "text", "text": "hi" }]
            });
            let response = ApiResponse {
                parsed: serde_json::from_value(raw_json.clone()).expect("message"),
                raw_json,
                status: 200,
                headers: HeaderSnapshot::default(),
            };
            let request = LlmGenerationRequest {
                session_id: SessionId::new("session-a"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                request: intent_request(Vec::new()),
            };

            let result = result_from_response(&blobs, &request, &response)
                .await
                .expect("result");

            assert_eq!(result.facts.finish, expected, "stop_reason {stop_reason}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn llm_runtime_runs_anthropic_summarization_compaction() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let input_ref = text_blob(&blobs, "We chose Postgres as the session store.").await;
        let raw_json = json!({
            "id": "msg_summary",
            "type": "message",
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{
                "type": "text",
                "text": "Summary: the user chose Postgres as the session store."
            }],
            "usage": { "input_tokens": 30, "output_tokens": 12 }
        });
        let api = fake_api(raw_json);
        let adapter = Arc::new(AnthropicMessagesLlmAdapter::new(api.clone(), blobs.clone()));
        let registry = LlmAdapterRegistry::new()
            .with_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter);
        let executor = LlmRuntime::new(registry);
        let request = ContextCompactionRequest {
            session_id: SessionId::new("session-a"),
            request: ContextCompactionTask {
                model: model(),
                request_fingerprint: "sha256:compact".to_string(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::AnthropicMessages,
                    context_revision: 7,
                    entries: vec![user_entry(1, input_ref)],
                    token_estimate: None,
                },
                target_tokens: Some(128),
                params: None,
            },
        };

        let result = CoreAgentLlm::compact_context(&executor, request)
            .await
            .expect("compact context");

        assert_eq!(result.status, ContextCompactionStatus::Succeeded);
        assert_eq!(result.context_revision, 7);
        assert_eq!(result.context_entries.len(), 1);
        let entry = &result.context_entries[0];
        assert!(matches!(
            entry.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(
            entry.provider_kind.as_deref(),
            Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
        );
        assert_eq!(entry.provider_item_id.as_deref(), Some("msg_summary"));
        assert_eq!(
            blobs
                .read_text(&entry.content_ref)
                .await
                .expect("summary text"),
            "Summary: the user chose Postgres as the session store."
        );

        let seen = api.seen.lock().expect("seen");
        assert_eq!(seen.len(), 1);
        let request_json = serde_json::to_value(&seen[0]).expect("request json");
        assert_eq!(request_json["model"], "claude-opus-4-8");
        assert_eq!(request_json["max_tokens"], 128);
        let messages = request_json["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        let instruction = messages[1]["content"].as_str().expect("instruction text");
        assert!(instruction.contains("context compaction"), "{instruction}");
        assert!(instruction.contains("under 128 tokens"), "{instruction}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skill_context_lowers_as_user_messages() {
        let blobs = InMemoryBlobStore::new();
        let activation_ref = text_blob(&blobs, "# Deploy Review\n\nCheck rollout scope.").await;
        let entry = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::SkillActivation {
                skill_id: engine::SkillId::new("skill:deploy-review"),
            },
            source: ContextEntrySource::Runtime {
                label: "skills.activation".to_string(),
            },
            content_ref: activation_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        };

        let materialized = materialize_create_request(&blobs, &intent_request(vec![entry]))
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["messages"],
            json!([{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "Forge loaded skill (skill:deploy-review):\n\n# Deploy Review\n\nCheck rollout scope."
                }]
            }])
        );
    }
}
