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
    ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND, ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
    CompactionPolicy, ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    ContextCompactionTask, ContextEntry, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus,
    LlmRequest, LlmUsage, ObservedToolCall, ProviderApiKind, ProviderNativeToolExecution,
    RemoteMcpApprovalPolicy, RemoteMcpExecution, RemoteMcpExposure, RemoteMcpToolSpec,
    TokenEstimate, TokenEstimateQuality, ToolCallId, ToolChoice, ToolKind, ToolName, ToolSpec,
    storage::BlobStore,
};
use llm_clients::{ApiResponse, anthropic::messages as am};
use serde_json::{Value, json};

use crate::{
    blob_io::{put_json, put_text, read_json, read_text},
    error::{LlmAdapterError, LlmAdapterResult},
    executor::{LlmCompactionAdapter, LlmGenerationAdapter},
    mcp::{
        MAX_NATIVE_MCP_TOOLS_PER_REQUEST, McpInventoryResolver, UnconfiguredMcpInventoryResolver,
    },
    params::{
        anthropic_messages_params, anthropic_thinking_from_effort,
        default_anthropic_thinking_display,
    },
    provider_keys::{ModelProviderResolver, NoStoredModelProviders, resolve_model_provider},
    result::{
        LlmGenerationExecution, debug_dump_request, partial_output_entries, store_debug_dumps,
        truncation_failure_text,
    },
    secrets::{
        REDACTED_SECRET_PLACEHOLDER, SecretResolveError, SecretResolver, UnconfiguredSecretResolver,
    },
};

const PROVIDER_KIND_TOOL_USE: &str = "anthropic.messages.tool_use";
use llm_clients::content::ANTHROPIC_THINKING_PROVIDER_KIND as PROVIDER_KIND_THINKING;
const PROVIDER_KIND_BLOCK: &str = "anthropic.messages.block";
const TOOL_SEARCH_TOOL_TYPE: &str = "tool_search_tool_bm25_20251119";
const TOOL_SEARCH_TOOL_NAME: &str = "tool_search_tool_bm25";
const MCP_TOOLSET_TYPE: &str = "mcp_toolset";
/// Client-seeded raw input message: a `ProviderOpaque` entry tagged with this
/// provider kind carries a complete Anthropic `{role, content}` message JSON
/// and lowers as that message instead of an assistant content block.
pub const ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND: &str = "anthropic.messages.input_message";
const MEDIA_TYPE_JSON: &str = "application/json";
const MEDIA_TYPE_TEXT: &str = "text/plain";

/// Anthropic requires `max_tokens` on every request (OpenAI's equivalents
/// are optional and omitted); used when the session sets no `output_limit`.
/// Thinking counts toward this cap, so a cap sized for a bare answer
/// truncates turns on models that reason before answering. 32K is within
/// every current model's output ceiling; the session's `maxOutputTokens` is
/// the knob for tighter bounds.
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_768;
/// Bound the provider's server-tool continuation loop. Anthropic explicitly
/// permits repeated `pause_turn` responses, so an unbounded adapter loop would
/// turn one durable activity into an uncontrolled sequence of billed calls.
const MAX_PAUSE_TURN_CONTINUATIONS: usize = 8;
/// Summary budget for summarization-based compaction when the task carries no
/// `target_tokens`.
const DEFAULT_COMPACTION_MAX_TOKENS: u64 = 2048;
/// Room above the summary budget for the thinking that precedes it. Models
/// that think by default (Claude Opus 5) would otherwise spend a tight
/// summary cap on reasoning and return a truncated or empty summary; the
/// instruction still bounds the summary itself.
const COMPACTION_THINKING_HEADROOM_TOKENS: u64 = 4096;
/// Preview for a thinking block whose summary the provider withheld (an
/// `omitted` display); clients hide this marker like other opaque state.
const OMITTED_THINKING_PREVIEW: &str = "reasoning state";
/// Preview for a `redacted_thinking` block: the provider flagged the
/// reasoning and returns only encrypted data.
const REDACTED_THINKING_PREVIEW: &str = "redacted thinking";

const COMPACTION_INSTRUCTION: &str = "Summarize the conversation above for context compaction. \
Capture the user's goals, decisions made, work completed, important tool results, and open \
questions. The summary will replace the prior conversation history, so include everything needed \
to continue seamlessly. Reply with the summary only.";

#[async_trait]
pub trait AnthropicMessagesApi: Send + Sync {
    /// `auth` overrides the client's transport-configured key for this
    /// request when stored provider credentials are used.
    async fn create(
        &self,
        request: am::CreateMessageRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
    ) -> Result<ApiResponse<am::Message>, llm_clients::LlmApiError>;
}

#[async_trait]
impl AnthropicMessagesApi for am::Client {
    async fn create(
        &self,
        request: am::CreateMessageRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
    ) -> Result<ApiResponse<am::Message>, llm_clients::LlmApiError> {
        am::Client::create_with_auth(self, request, auth).await
    }
}

#[derive(Clone)]
pub struct AnthropicMessagesLlmAdapter {
    client: Arc<dyn AnthropicMessagesApi>,
    blobs: Arc<dyn BlobStore>,
    /// Store the raw provider request and response of every generation as
    /// unrooted debug blobs. Off by default: each request carries the whole
    /// context, so the dumps grow quadratically with turn count.
    debug_dumps: bool,
    secrets: Arc<dyn SecretResolver>,
    provider_keys: Arc<dyn ModelProviderResolver>,
    inventory: Arc<dyn McpInventoryResolver>,
}

impl AnthropicMessagesLlmAdapter {
    pub fn new(client: Arc<dyn AnthropicMessagesApi>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            client,
            blobs,
            debug_dumps: false,
            secrets: Arc::new(UnconfiguredSecretResolver),
            provider_keys: Arc::new(NoStoredModelProviders),
            inventory: Arc::new(UnconfiguredMcpInventoryResolver),
        }
    }

    pub fn with_secret_resolver(mut self, secrets: Arc<dyn SecretResolver>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Enable or disable storing raw provider request/response dumps.
    pub fn with_debug_dumps(mut self, enabled: bool) -> Self {
        self.debug_dumps = enabled;
        self
    }

    pub fn with_provider_key_resolver(
        mut self,
        provider_keys: Arc<dyn ModelProviderResolver>,
    ) -> Self {
        self.provider_keys = provider_keys;
        self
    }

    pub fn with_mcp_inventory_resolver(mut self, inventory: Arc<dyn McpInventoryResolver>) -> Self {
        self.inventory = inventory;
        self
    }

    pub async fn materialize_create_request(
        &self,
        request: &LlmRequest,
    ) -> LlmAdapterResult<am::CreateMessageRequest> {
        materialize_create_request_with_inventory(
            self.blobs.as_ref(),
            self.inventory.as_ref(),
            request,
        )
        .await
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

        let mut catalog = crate::tool_catalog::ToolCatalog::resolve(
            self.blobs.as_ref(),
            &tools::runtime::ToolTarget::from(&request.request.model),
            &request.request.tools,
        )
        .await?;
        let provider_request = materialize_request_with_catalog(
            self.blobs.as_ref(),
            self.inventory.as_ref(),
            &request.request,
            &mut catalog,
        )
        .await?;
        let (mut send_request, mut redacted_request) =
            inject_remote_mcp_auth(self.secrets.as_ref(), &request.request, provider_request)
                .await?;
        let provider =
            resolve_model_provider(self.provider_keys.as_ref(), &request.request.model).await?;
        let mut request_dumps = Vec::new();
        let mut responses = Vec::new();
        loop {
            if let Some(dump) = debug_dump_request(self.debug_dumps, &redacted_request)? {
                request_dumps.push(dump);
            }
            let response = self
                .client
                .create(
                    send_request.clone(),
                    provider.as_ref().map(|provider| provider.as_request_auth()),
                )
                .await?;
            let paused = response.parsed.stop_reason == Some(am::StopReason::PauseTurn);
            if paused && responses.len() >= MAX_PAUSE_TURN_CONTINUATIONS {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: format!(
                        "Anthropic server-tool turn exceeded {} pause_turn continuations",
                        MAX_PAUSE_TURN_CONTINUATIONS
                    ),
                });
            }
            if paused {
                let paused_message = paused_assistant_message(&response.raw_json)?;
                send_request.messages.push(paused_message.clone());
                redacted_request.messages.push(paused_message);
            }
            responses.push(response);
            if !paused {
                break;
            }
        }
        let mut result = result_from_responses(self.blobs.as_ref(), &request, &responses).await?;
        catalog.normalize(&mut result);
        let request_dump = match request_dumps.len() {
            0 => None,
            1 => request_dumps.pop(),
            _ => Some(Value::Array(request_dumps)),
        };
        let raw_response_dump = if responses.len() == 1 {
            responses[0].raw_json.clone()
        } else {
            Value::Array(
                responses
                    .iter()
                    .map(|response| response.raw_json.clone())
                    .collect(),
            )
        };
        let debug_dumps = store_debug_dumps(
            self.blobs.as_ref(),
            request_dump,
            &raw_response_dump,
            &request,
            "anthropic_messages",
        )
        .await?;

        Ok(LlmGenerationExecution {
            result,
            debug_dumps,
        })
    }
}

fn paused_assistant_message(raw_response: &Value) -> LlmAdapterResult<am::MessageParam> {
    let blocks = raw_response
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: "Anthropic pause_turn response is missing a content array".to_owned(),
        })?
        .iter()
        .cloned()
        .map(am::ContentBlockParam::Raw)
        .collect();
    Ok(am::MessageParam::assistant(blocks))
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
        let provider =
            resolve_model_provider(self.provider_keys.as_ref(), &request.request.model).await?;
        let response = self
            .client
            .create(
                provider_request,
                provider.as_ref().map(|provider| provider.as_request_auth()),
            )
            .await?;
        result_from_compact_response(self.blobs.as_ref(), &request, &response).await
    }
}

pub async fn materialize_create_request(
    blobs: &dyn BlobStore,
    request: &LlmRequest,
) -> LlmAdapterResult<am::CreateMessageRequest> {
    materialize_create_request_with_inventory(blobs, &UnconfiguredMcpInventoryResolver, request)
        .await
}

async fn materialize_create_request_with_inventory(
    blobs: &dyn BlobStore,
    inventory: &dyn McpInventoryResolver,
    request: &LlmRequest,
) -> LlmAdapterResult<am::CreateMessageRequest> {
    let mut catalog = crate::tool_catalog::ToolCatalog::resolve(
        blobs,
        &tools::runtime::ToolTarget::from(&request.model),
        &request.tools,
    )
    .await?;
    materialize_request_with_catalog(blobs, inventory, request, &mut catalog).await
}

async fn materialize_request_with_catalog(
    blobs: &dyn BlobStore,
    inventory: &dyn McpInventoryResolver,
    request: &LlmRequest,
    catalog: &mut crate::tool_catalog::ToolCatalog,
) -> LlmAdapterResult<am::CreateMessageRequest> {
    if request.processing_tier.is_some() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "processing tier is not supported by Anthropic Messages".to_owned(),
        });
    }
    let mut params = anthropic_messages_params(request.params.as_ref())?;
    // Materialize the intent reasoning effort into provider params. Explicit
    // per-run provider params win: derived values never overwrite fields the
    // params body already sets.
    if let Some(effort) = request.reasoning_effort.as_deref() {
        let derived = anthropic_thinking_from_effort(effort)?;
        if params.thinking.is_none() && params.output_config.is_none() {
            params.thinking = Some(derived.thinking);
            params.output_config = derived.output_config;
        }
    }
    // Reasoning entries only carry text when the request asks for the
    // summary; current models omit it unless told otherwise.
    if let Some(thinking) = params.thinking.as_mut() {
        default_anthropic_thinking_display(thinking);
    }
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

    // Prompt-cache breakpoints: Anthropic caches only at explicit
    // `cache_control` markers, so the adapter places the standard layout on
    // every request — end of the system prompt, last non-deferred tool
    // definition, last block of the last message (a moving marker that keeps
    // the whole prefix warm turn after turn). Placement is a materialization
    // detail; nothing in the planned request or the session log changes.
    let cache_control = prompt_cache_control(params.prompt_cache_ttl.as_deref());
    let system = materialize_system(blobs, &request.context.entries, &cache_control).await?;
    let message_entries = request
        .context
        .entries
        .iter()
        .filter(|entry| !matches!(entry.kind, ContextEntryKind::Instructions))
        .cloned()
        .collect::<Vec<_>>();
    let mut messages = materialize_messages(blobs, &message_entries).await?;
    place_message_breakpoint(&mut messages, &cache_control);
    let (mut tools, mcp_servers) =
        materialize_tools(inventory, &request.model.model, catalog).await?;
    place_tool_breakpoint(&mut tools, &cache_control);

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
        tool_choice: anthropic_tool_choice(
            catalog.tool_choice(request.tool_choice.as_ref())?.as_ref(),
            request.parallel_tool_use,
        ),
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
    let summary_tokens = task
        .target_tokens
        .map(u64::from)
        .unwrap_or(DEFAULT_COMPACTION_MAX_TOKENS);
    Ok(am::CreateMessageRequest {
        model: task.model.model.clone(),
        max_tokens: summary_tokens + COMPACTION_THINKING_HEADROOM_TOKENS,
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

/// The system prompt as a single cached block; `None` when there are no
/// instructions (an empty system block would be rejected).
async fn materialize_system(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
    cache_control: &Value,
) -> LlmAdapterResult<Option<am::SystemContent>> {
    let mut parts = Vec::new();
    for entry in entries {
        if matches!(entry.kind, ContextEntryKind::Instructions) {
            let text = read_text(blobs, &entry.content.content_ref).await?;
            let text = text.trim();
            if !text.is_empty() {
                parts.push(text.to_owned());
            }
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(am::SystemContent::Blocks(vec![
        am::ContentBlockParam::Text(am::TextBlockParam {
            r#type: "text".to_owned(),
            text: parts.join("\n\n"),
            cache_control: Some(cache_control.clone()),
            extra: Default::default(),
        }),
    ])))
}

/// The `cache_control` marker for this request: ephemeral, with the
/// optional longer TTL from the params.
fn prompt_cache_control(ttl: Option<&str>) -> Value {
    match ttl {
        Some(ttl) => json!({ "type": "ephemeral", "ttl": ttl }),
        None => json!({ "type": "ephemeral" }),
    }
}

/// Mark the last non-deferred tool definition so the visible tool prefix is
/// cached. Adapter-owned raw tools may carry the marker; other raw
/// provider-native tools are left alone because their JSON is the operator's.
fn place_tool_breakpoint(tools: &mut [am::Tool], cache_control: &Value) {
    for tool in tools.iter_mut().rev() {
        if tool_is_deferred(tool) {
            continue;
        }
        match tool {
            am::Tool::Custom(definition) => {
                if definition.cache_control.is_none() {
                    definition.cache_control = Some(cache_control.clone());
                }
                return;
            }
            am::Tool::Raw(value) if adapter_owned_raw_tool(value) => {
                let object = value.as_object_mut().expect("adapter-owned tool object");
                object
                    .entry("cache_control".to_owned())
                    .or_insert_with(|| cache_control.clone());
                return;
            }
            am::Tool::Raw(_) => {}
        }
    }
}

fn tool_is_deferred(tool: &am::Tool) -> bool {
    let value = match tool {
        am::Tool::Custom(definition) => {
            return definition.extra.get("defer_loading") == Some(&Value::Bool(true));
        }
        am::Tool::Raw(value) => value,
    };
    value.get("defer_loading") == Some(&Value::Bool(true))
        || value
            .get("default_config")
            .and_then(|config| config.get("defer_loading"))
            == Some(&Value::Bool(true))
}

fn adapter_owned_raw_tool(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            MCP_TOOLSET_TYPE
                | TOOL_SEARCH_TOOL_TYPE
                | tools::web::search::ANTHROPIC_MESSAGES_WEB_SEARCH_TYPE
                | tools::web::fetch::ANTHROPIC_MESSAGES_WEB_FETCH_TYPE
        )
    )
}

/// Mark the last block of the last message that can carry `cache_control`
/// (thinking blocks cannot). Blocks that already carry a marker from
/// provider options keep theirs.
fn place_message_breakpoint(messages: &mut [am::MessageParam], cache_control: &Value) {
    let Some(message) = messages.last_mut() else {
        return;
    };
    let am::MessageParamContent::Blocks(blocks) = &mut message.content else {
        return;
    };
    for block in blocks.iter_mut().rev() {
        let slot = match block {
            am::ContentBlockParam::Text(block) => &mut block.cache_control,
            am::ContentBlockParam::Image(block) => &mut block.cache_control,
            am::ContentBlockParam::Document(block) => &mut block.cache_control,
            am::ContentBlockParam::ToolUse(block) => &mut block.cache_control,
            am::ContentBlockParam::ToolResult(block) => &mut block.cache_control,
            am::ContentBlockParam::Raw(value) => {
                if let Some(object) = value.as_object_mut()
                    // Anthropic requires cited text blocks (including opaque
                    // encrypted indexes) to be replayed unchanged.
                    && !object
                        .get("citations")
                        .and_then(Value::as_array)
                        .is_some_and(|citations| !citations.is_empty())
                    && object
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| {
                            matches!(
                                kind,
                                "text" | "image" | "document" | "tool_use" | "tool_result"
                            )
                        })
                {
                    object
                        .entry("cache_control")
                        .or_insert_with(|| cache_control.clone());
                    return;
                }
                continue;
            }
            am::ContentBlockParam::Thinking(_) | am::ContentBlockParam::RedactedThinking(_) => {
                continue;
            }
        };
        if slot.is_none() {
            *slot = Some(cache_control.clone());
        }
        return;
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
        if entry.content.provider_kind.as_deref()
            == Some(ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND)
        {
            for block in text_blocks(blobs, entry).await? {
                push_block(
                    &mut messages,
                    am::MessageRole::Assistant,
                    am::ContentBlockParam::Raw(block),
                )?;
            }
            continue;
        }
        let (role, blocks) = materialize_block(blobs, entry).await?;
        for block in blocks {
            push_block(&mut messages, role, block)?;
        }
    }
    Ok(messages)
}

async fn text_blocks(blobs: &dyn BlobStore, entry: &ContextEntry) -> LlmAdapterResult<Vec<Value>> {
    match read_json(blobs, &entry.content.content_ref).await? {
        Value::Array(blocks) => Ok(blocks),
        _ => Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "Anthropic cited text entry {} must carry a content block array",
                entry.entry_id
            ),
        }),
    }
}

/// Append a block to the trailing message of `role`, or open a new one.
/// Anthropic requires every `tool_result` block of a user message to come
/// before its other content, while a tool's media entries follow its result
/// in context; when parallel calls interleave results and media, a later
/// result is inserted after the last result already in the message, so
/// results stay first and media keeps call order behind them.
fn push_block(
    messages: &mut Vec<am::MessageParam>,
    role: am::MessageRole,
    block: am::ContentBlockParam,
) -> LlmAdapterResult<()> {
    match messages.last_mut() {
        Some(message) if message.role == role => match &mut message.content {
            am::MessageParamContent::Blocks(blocks) => {
                if matches!(block, am::ContentBlockParam::ToolResult(_)) {
                    let after_results = blocks
                        .iter()
                        .rposition(|existing| {
                            matches!(existing, am::ContentBlockParam::ToolResult(_))
                        })
                        .map_or(0, |index| index + 1);
                    blocks.insert(after_results, block);
                } else {
                    blocks.push(block);
                }
            }
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
        && entry.content.provider_kind.as_deref()
            == Some(ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND)
}

async fn materialize_input_message(
    blobs: &dyn BlobStore,
    entry: &ContextEntry,
) -> LlmAdapterResult<(am::MessageRole, Vec<am::ContentBlockParam>)> {
    let raw = read_json(blobs, &entry.content.content_ref).await?;
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

/// One context entry as the blocks of one role. Media entries (images, PDFs)
/// materialize as a text announcement naming the media handle followed by
/// the provider-native block, so the model can refer to what it was shown.
async fn materialize_block(
    blobs: &dyn BlobStore,
    entry: &ContextEntry,
) -> LlmAdapterResult<(am::MessageRole, Vec<am::ContentBlockParam>)> {
    match &entry.kind {
        ContextEntryKind::Message { role } => {
            let role = match role {
                ContextMessageRole::User => am::MessageRole::User,
                ContextMessageRole::Assistant => am::MessageRole::Assistant,
            };
            if let Some(mime) = crate::blob_io::image_media_type(entry.content.media_type.as_deref()) {
                let data = crate::blob_io::read_base64(blobs, &entry.content.content_ref).await?;
                return Ok((
                    role,
                    vec![
                        am::ContentBlockParam::text(crate::blob_io::media_announcement(entry)),
                        am::ContentBlockParam::image_base64(mime, data),
                    ],
                ));
            }
            if let Some(document) = crate::blob_io::document_entry(
                entry.content.media_type.as_deref(),
                entry.preview.as_deref(),
            ) {
                let blocks = if document.is_pdf {
                    let data = crate::blob_io::read_base64(blobs, &entry.content.content_ref).await?;
                    vec![
                        am::ContentBlockParam::text(crate::blob_io::media_announcement(entry)),
                        am::ContentBlockParam::document_base64(document.mime, data, document.name),
                    ]
                } else {
                    let text = read_text(blobs, &entry.content.content_ref).await?;
                    vec![am::ContentBlockParam::document_text(text, document.name)]
                };
                return Ok((role, blocks));
            }
            let text = crate::blob_io::read_message_text(blobs, &entry.content).await?;
            Ok((role, vec![am::ContentBlockParam::text(text)]))
        }
        ContextEntryKind::ToolResult { call_id, is_error } => {
            let output = read_text(blobs, &entry.content.content_ref).await?;
            Ok((
                am::MessageRole::User,
                vec![am::ContentBlockParam::ToolResult(am::ToolResultBlockParam {
                    r#type: "tool_result".to_owned(),
                    tool_use_id: call_id.as_str().to_owned(),
                    content: Some(Value::String(output)),
                    is_error: if *is_error { Some(true) } else { None },
                    cache_control: None,
                    extra: Default::default(),
                })],
            ))
        }
        ContextEntryKind::Instructions => Err(LlmAdapterError::InvalidProviderRequest {
            message: "instruction context entries must materialize as the system prompt".to_owned(),
        }),
        ContextEntryKind::Catalog { .. } => Ok((
            am::MessageRole::User,
            vec![am::ContentBlockParam::text(
                crate::catalog_prompts::stored_catalog_text(blobs, entry, &entry.content.content_ref)
                    .await?,
            )],
        )),
        ContextEntryKind::ToolCall { .. }
        | ContextEntryKind::ReasoningState
        | ContextEntryKind::ProviderOpaque => {
            if entry.content.media_type.as_deref() != Some(MEDIA_TYPE_JSON) {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: format!(
                        "Anthropic context entry {} must carry a raw JSON content block",
                        entry.entry_id
                    ),
                });
            }
            let raw = read_json(blobs, &entry.content.content_ref).await?;
            Ok((
                am::MessageRole::Assistant,
                vec![am::ContentBlockParam::Raw(raw)],
            ))
        }
        ContextEntryKind::McpApprovalResponse { .. } => {
            Err(LlmAdapterError::InvalidProviderRequest {
                message: "Anthropic provider-hosted MCP does not support approval responses; use native MCP execution or an OpenAI Responses model".to_owned(),
            })
        }
    }
}

async fn materialize_tools(
    inventory: &dyn McpInventoryResolver,
    model: &str,
    catalog: &mut crate::tool_catalog::ToolCatalog,
) -> LlmAdapterResult<(Vec<am::Tool>, Vec<Value>)> {
    let mut materialized = Vec::new();
    let mut mcp_toolsets = Vec::new();
    let mut mcp_servers = Vec::new();
    let mut has_deferred_mcp = false;
    let mut native_mcp_tool_count = 0usize;
    for tool in &catalog.tools {
        match &tool.kind {
            crate::tool_catalog::ResolvedToolKind::Function(function) => {
                let mut definition =
                    am::ToolDefinition::new(tool.name.as_str(), function.input_schema.clone());
                definition.description = function.description.clone();
                // Anthropic has no strict-mode switch; the input schema is the
                // only contract, so `strict` does not lower to anything.
                if let Some(options) = &function.provider_options {
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
            crate::tool_catalog::ResolvedToolKind::ProviderNative(native) => {
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
                        materialized.push(am::Tool::Raw(native.definition.clone()));
                    }
                }
            }
            crate::tool_catalog::ResolvedToolKind::RemoteMcp(remote_mcp) => {
                match (remote_mcp.execution, remote_mcp.exposure) {
                    (RemoteMcpExecution::Provider, _) => {
                        let (server, toolset) = materialize_remote_mcp_server(tool, remote_mcp)?;
                        has_deferred_mcp |= remote_mcp.defer_loading == Some(true);
                        mcp_servers.push(server);
                        mcp_toolsets.push(toolset);
                    }
                    (RemoteMcpExecution::Native, RemoteMcpExposure::Search) => {}
                    (RemoteMcpExecution::Native, RemoteMcpExposure::Inject) => {
                        let mut native =
                            inventory.list_tools(remote_mcp).await.map_err(|error| {
                                LlmAdapterError::McpInventory {
                                    server: remote_mcp.server_id.clone(),
                                    message: error.to_string(),
                                }
                            })?;
                        native.sort_by(|left, right| left.remote_name.cmp(&right.remote_name));
                        let advertised_count = native.len();
                        native.retain(|native_tool| {
                            let name = format!("{}__{}", tool.name, native_tool.remote_name);
                            crate::tool_catalog::valid_exposed_name(&name)
                        });
                        let omitted_count = advertised_count - native.len();
                        if omitted_count != 0 {
                            tracing::warn!(
                                server_id = %remote_mcp.server_id,
                                omitted_tool_count = omitted_count,
                                "omitted native MCP tools with provider-incompatible names"
                            );
                        }
                        if native_mcp_tool_count.saturating_add(native.len())
                            > MAX_NATIVE_MCP_TOOLS_PER_REQUEST
                        {
                            return Err(LlmAdapterError::McpInventory {
                            server: remote_mcp.server_id.clone(),
                            message: "native MCP inventory exceeds the per-request tool cap; author a Selected allowlist or switch the record to search exposure".to_owned(),
                        });
                        }
                        native_mcp_tool_count += native.len();
                        for native_tool in native {
                            let name = format!("{}__{}", tool.name, native_tool.remote_name);
                            catalog
                                .names
                                .insert(ToolName::new(name.clone()), Some(tool.id.clone()))?;
                            materialized.push(am::Tool::Custom(am::ToolDefinition {
                                name,
                                description: native_tool.description,
                                input_schema: native_tool.input_schema,
                                cache_control: None,
                                extra: Default::default(),
                            }));
                        }
                    }
                }
            }
        }
    }
    if has_deferred_mcp {
        ensure_anthropic_tool_search_model(model)?;
        catalog
            .names
            .insert(ToolName::new(TOOL_SEARCH_TOOL_NAME), None)?;
        materialized.push(am::Tool::Raw(json!({
            "type": TOOL_SEARCH_TOOL_TYPE,
            "name": TOOL_SEARCH_TOOL_NAME,
        })));
    }
    materialized.extend(mcp_toolsets.into_iter().map(am::Tool::Raw));
    Ok((materialized, mcp_servers))
}

fn materialize_remote_mcp_server(
    tool: &crate::tool_catalog::ResolvedTool,
    remote_mcp: &RemoteMcpToolSpec,
) -> LlmAdapterResult<(Value, Value)> {
    // Materialized requests never contain auth values; `inject_remote_mcp_auth`
    // adds `authorization_token` to the send request immediately before
    // provider I/O.
    if matches!(remote_mcp.approval, RemoteMcpApprovalPolicy::Always) {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "remote MCP tool {} requires approval, but the Anthropic MCP connector has no approval flow",
                tool.name
            ),
        });
    }

    let server = json!({
        "type": "url",
        "url": remote_mcp.server_url,
        "name": remote_mcp.server_label,
    });

    let mut toolset = json!({
        "type": MCP_TOOLSET_TYPE,
        "mcp_server_name": remote_mcp.server_label,
    });
    let object = toolset.as_object_mut().expect("mcp toolset object");
    let mut default_config = serde_json::Map::new();
    if let Some(allowed_tools) = &remote_mcp.allowed_tools {
        default_config.insert("enabled".to_owned(), Value::Bool(false));
        object.insert(
            "configs".to_owned(),
            Value::Object(
                allowed_tools
                    .iter()
                    .map(|name| (name.clone(), json!({ "enabled": true })))
                    .collect(),
            ),
        );
    }
    if remote_mcp.defer_loading == Some(true) {
        default_config.insert("defer_loading".to_owned(), Value::Bool(true));
    }
    if !default_config.is_empty() {
        object.insert("default_config".to_owned(), Value::Object(default_config));
    }
    Ok((server, toolset))
}

fn ensure_anthropic_tool_search_model(model: &str) -> LlmAdapterResult<()> {
    if anthropic_tool_search_model_support(model) != Some(false) {
        return Ok(());
    }
    Err(LlmAdapterError::UnsupportedModelFeature {
        model: model.to_owned(),
        feature: "Anthropic tool search",
        message: "tool_search_tool_bm25_20251119 requires Claude 4.5 or later".to_owned(),
    })
}

/// Return a definite answer for recognizable Claude model IDs. Unknown IDs
/// are left to the provider so Anthropic-compatible endpoints and future
/// naming schemes are not rejected speculatively.
fn anthropic_tool_search_model_support(model: &str) -> Option<bool> {
    let normalized = model.to_ascii_lowercase();
    if !normalized.contains("claude") {
        return None;
    }
    let versions = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter_map(|part| {
            let value = part.parse::<u16>().ok()?;
            (value < 100).then_some(value)
        })
        .collect::<Vec<_>>();
    let major = *versions.first()?;
    let minor = versions.get(1).copied().unwrap_or(0);
    Some(major > 4 || (major == 4 && minor >= 5))
}

/// Produce the request pair `generate` actually uses: the send request with
/// `authorization_token` resolved at the last moment, and the redacted request
/// that is persisted to blobs, preserving only the fact that auth was
/// configured.
async fn inject_remote_mcp_auth(
    secrets: &dyn SecretResolver,
    request: &LlmRequest,
    materialized: am::CreateMessageRequest,
) -> LlmAdapterResult<(am::CreateMessageRequest, am::CreateMessageRequest)> {
    let auth_specs: Vec<(&ToolSpec, &RemoteMcpToolSpec)> = request
        .tools
        .iter()
        .filter_map(|tool| match &tool.kind {
            ToolKind::RemoteMcp(remote_mcp)
                if remote_mcp.execution == RemoteMcpExecution::Provider
                    && remote_mcp.auth_ref.is_some() =>
            {
                Some((tool, remote_mcp))
            }
            _ => None,
        })
        .collect();
    if auth_specs.is_empty() {
        let redacted = materialized.clone();
        return Ok((materialized, redacted));
    }

    let mut send_request = materialized.clone();
    let mut redacted_request = materialized;
    for (tool, remote_mcp) in auth_specs {
        let auth_ref = remote_mcp.auth_ref.as_ref().expect("auth_ref present");
        let token = match secrets
            .resolve(auth_ref, Some(remote_mcp.server_url.as_str()))
            .await
        {
            Ok(token) => token,
            Err(SecretResolveError::CredentialAbsent { .. }) if !remote_mcp.auth_required => {
                continue;
            }
            Err(error) => {
                return Err(LlmAdapterError::SecretResolution {
                    tool: tool.name.to_string(),
                    message: error.to_string(),
                });
            }
        };
        set_remote_mcp_authorization_token(
            &mut send_request,
            &remote_mcp.server_label,
            token.expose(),
            tool,
        )?;
        set_remote_mcp_authorization_token(
            &mut redacted_request,
            &remote_mcp.server_label,
            REDACTED_SECRET_PLACEHOLDER,
            tool,
        )?;
    }
    Ok((send_request, redacted_request))
}

fn set_remote_mcp_authorization_token(
    request: &mut am::CreateMessageRequest,
    server_label: &str,
    value: &str,
    tool: &ToolSpec,
) -> LlmAdapterResult<()> {
    let entry = request
        .mcp_servers
        .as_mut()
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
        .find(|server| server.get("name").and_then(Value::as_str) == Some(server_label));
    let Some(entry) = entry else {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "materialized request is missing MCP server entry for {} (server name {server_label})",
                tool.name
            ),
        });
    };
    entry.insert(
        "authorization_token".to_string(),
        Value::String(value.to_owned()),
    );
    Ok(())
}

/// Materialize the provider tool choice. Anthropic carries the parallel
/// tool-use switch on `tool_choice`, so an intent with `parallel_tool_use`
/// but no explicit tool choice lowers to `auto` with the flag set.
fn anthropic_tool_choice(
    choice: Option<&ToolChoice>,
    parallel_tool_use: Option<bool>,
) -> Option<am::ToolChoice> {
    let choice = match (choice, parallel_tool_use) {
        (Some(choice), _) => choice,
        (None, Some(_)) => &ToolChoice::Auto,
        (None, None) => return None,
    };
    let mut materialized = match choice {
        ToolChoice::Auto => am::ToolChoice::auto(),
        ToolChoice::None => am::ToolChoice::none(),
        ToolChoice::RequiredAny => am::ToolChoice::any(),
        ToolChoice::Specific { tool_name } => am::ToolChoice::tool(tool_name.as_str()),
    };
    if !matches!(choice, ToolChoice::None) {
        materialized.disable_parallel_tool_use = parallel_tool_use.map(|parallel| !parallel);
    }
    Some(materialized)
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
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

pub async fn result_from_response(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    response: &ApiResponse<am::Message>,
) -> LlmAdapterResult<LlmGenerationResult> {
    if response.parsed.stop_reason == Some(am::StopReason::Refusal) {
        return refused_generation_result(blobs, request, response).await;
    }

    let mut context_entries = Vec::new();
    let mut tool_calls = Vec::new();
    // Consecutive text blocks form one assistant message; any other block
    // between them ends the run, so replay keeps the provider's block order.
    let mut text_run: Vec<Value> = Vec::new();

    for (index, block) in response.parsed.content.iter().enumerate() {
        let raw_block = raw_content_block(&response.raw_json, index, block)?;
        if block.r#type == "text" {
            text_run.push(raw_block);
            continue;
        }
        context_entries
            .extend(text_run_context_entries(blobs, std::mem::take(&mut text_run)).await?);
        match block.r#type.as_str() {
            "tool_use" => {
                let (entry, tool_call) = tool_use_context(blobs, block, raw_block, index).await?;
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
    context_entries.extend(text_run_context_entries(blobs, text_run).await?);

    let usage = response.parsed.usage.as_ref().map(llm_usage);
    let context_token_estimate =
        response
            .parsed
            .usage
            .as_ref()
            .and_then(prompt_tokens)
            .map(|tokens| TokenEstimate {
                tokens: u64_to_u32(tokens),
                quality: TokenEstimateQuality::ProviderCounted,
            });
    let finish = finish_reason(response.parsed.stop_reason, !tool_calls.is_empty());
    // A turn cut off at `max_tokens` fails, keeping the partial text the
    // user can see; tool calls from an unfinished turn have nothing to
    // replay against and are dropped with the unfinished thinking.
    let (status, failure_ref, context_entries, tool_calls) = if finish == LlmFinish::Length {
        let failure_ref = put_text(
            blobs,
            truncation_failure_text(
                request.run_id,
                request.turn_id,
                "Anthropic Messages",
                &response.parsed.id,
                Some(
                    request
                        .request
                        .output_limit
                        .map(u64::from)
                        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
                ),
                usage.as_ref().and_then(|usage| usage.output_tokens),
                usage.as_ref().and_then(|usage| usage.reasoning_tokens),
            ),
        )
        .await?;
        (
            LlmGenerationStatus::Failed,
            Some(failure_ref),
            partial_output_entries(blobs, context_entries).await?,
            Vec::new(),
        )
    } else {
        (
            LlmGenerationStatus::Succeeded,
            None,
            context_entries,
            tool_calls,
        )
    };
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status,
        failure_ref,
        context_entries,
        facts: LlmGenerationFacts {
            duration_ms: None,
            provider_response_id: Some(response.parsed.id.clone()),
            finish,
            usage,
            tool_calls,
            approval_requests: Vec::new(),
            context_token_estimate,
        },
    })
}

async fn result_from_responses(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    responses: &[ApiResponse<am::Message>],
) -> LlmAdapterResult<LlmGenerationResult> {
    let mut combined: Option<LlmGenerationResult> = None;
    for response in responses {
        let mut part = result_from_response(blobs, request, response).await?;
        if part.status == LlmGenerationStatus::Failed {
            if let Some(previous) = combined {
                let mut usage = previous.facts.usage;
                merge_llm_usage(&mut usage, part.facts.usage.as_ref());
                part.facts.usage = usage;
            }
            // A refused or truncated continuation is not a complete provider
            // turn. Keep its normal partial-output policy and do not commit
            // earlier server-tool blocks that cannot safely be replayed.
            return Ok(part);
        }
        match &mut combined {
            None => combined = Some(part),
            Some(total) => {
                total.context_entries.extend(part.context_entries);
                total.facts.tool_calls.extend(part.facts.tool_calls);
                total
                    .facts
                    .approval_requests
                    .extend(part.facts.approval_requests);
                merge_llm_usage(&mut total.facts.usage, part.facts.usage.as_ref());
                total.status = part.status;
                total.failure_ref = part.failure_ref;
                total.facts.provider_response_id = part.facts.provider_response_id;
                total.facts.finish = part.facts.finish;
                total.facts.context_token_estimate = part.facts.context_token_estimate;
            }
        }
    }
    combined.ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
        message: "Anthropic generation produced no responses".to_owned(),
    })
}

fn merge_llm_usage(total: &mut Option<LlmUsage>, next: Option<&LlmUsage>) {
    let Some(next) = next else {
        return;
    };
    let Some(total) = total.as_mut() else {
        *total = Some(next.clone());
        return;
    };
    fn add(total: &mut Option<u32>, next: Option<u32>) {
        if let Some(next) = next {
            *total = Some(total.unwrap_or_default().saturating_add(next));
        }
    }
    add(&mut total.input_tokens, next.input_tokens);
    add(&mut total.output_tokens, next.output_tokens);
    add(&mut total.reasoning_tokens, next.reasoning_tokens);
    add(&mut total.total_tokens, next.total_tokens);
    add(&mut total.cached_input_tokens, next.cached_input_tokens);
    add(
        &mut total.cache_write_input_tokens,
        next.cache_write_input_tokens,
    );
    add(
        &mut total.cache_miss_input_tokens,
        next.cache_miss_input_tokens,
    );
}

/// A `refusal` stop is terminal for the turn: the provider's safety
/// classifier declined the request (HTTP 200 with no or partial content).
/// The turn fails with the classifier's category and explanation instead of
/// completing as an empty answer, any partial content is dropped, and nothing
/// falls back to another model. The failure text follows the worker's
/// provider-error blob layout so clients render it the same way.
async fn refused_generation_result(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    response: &ApiResponse<am::Message>,
) -> LlmAdapterResult<LlmGenerationResult> {
    let details = response.parsed.stop_details.as_ref();
    let category = details
        .and_then(|details| details.category.as_deref())
        .unwrap_or("unspecified");
    let explanation = details
        .and_then(|details| details.explanation.as_deref())
        .unwrap_or("no explanation provided");
    let failure_ref = put_text(
        blobs,
        format!(
            "core agent LLM generation failed\nrun_id={}\nturn_id={}\n\
             error=Anthropic refused response {} (category: {category}): {explanation}\n",
            request.run_id, request.turn_id, response.parsed.id
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
            provider_response_id: Some(response.parsed.id.clone()),
            finish: LlmFinish::ContentFilter,
            usage: response.parsed.usage.as_ref().map(llm_usage),
            tool_calls: Vec::new(),
            approval_requests: Vec::new(),
            context_token_estimate: None,
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
        let block_types = response
            .parsed
            .content
            .iter()
            .map(|block| block.r#type.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "Anthropic Messages compaction response {} did not include summary text \
                 (stop_reason: {:?}, stop_details: {:?}, blocks: [{block_types}], \
                 output_tokens: {:?}, thinking_tokens: {:?})",
                response.parsed.id,
                response.parsed.stop_reason,
                response.parsed.stop_details,
                response
                    .parsed
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.output_tokens),
                response
                    .parsed
                    .usage
                    .as_ref()
                    .and_then(am::Usage::thinking_tokens),
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
            content: engine::ContentRef {
                content_ref,
                media_type: Some(MEDIA_TYPE_TEXT.to_owned()),
                provider_kind: Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND.to_owned()),
            },
            preview: Some(summary.to_owned()),
            origin: None,
            provenance_ref: None,
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

/// One assistant message owns the original consecutive text blocks. Display
/// text and citations are projected from this payload; replay uses it unchanged.
async fn text_run_context_entries(
    blobs: &dyn BlobStore,
    blocks: Vec<Value>,
) -> LlmAdapterResult<Vec<ContextEntryInput>> {
    let text = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<String>();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        },
        content: engine::ContentRef {
            content_ref: put_json(blobs, &Value::Array(blocks)).await?,
            media_type: Some(MEDIA_TYPE_JSON.to_owned()),
            provider_kind: Some(ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND.to_owned()),
        },
        preview: Some(text.chars().take(256).collect()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }])
}

async fn tool_use_context(
    blobs: &dyn BlobStore,
    block: &am::ContentBlock,
    raw_block: Value,
    index: usize,
) -> LlmAdapterResult<(ContextEntryInput, ObservedToolCall)> {
    let call_id = block.id.clone().unwrap_or_else(|| format!("toolu_{index}"));
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
        content: engine::ContentRef {
            content_ref: native_call_ref.clone(),
            media_type: Some(MEDIA_TYPE_JSON.to_owned()),
            provider_kind: Some(PROVIDER_KIND_TOOL_USE.to_owned()),
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    };
    let tool_call = ObservedToolCall {
        call_id,
        tool_id: Some(tool_name.clone()),
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
    let text = llm_clients::content::anthropic_thinking(&raw_block).unwrap_or_default();
    let preview = if text.trim().is_empty() {
        if block.r#type == "redacted_thinking" {
            REDACTED_THINKING_PREVIEW
        } else {
            OMITTED_THINKING_PREVIEW
        }
        .to_owned()
    } else {
        text.trim().chars().take(256).collect()
    };
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ReasoningState,
        content: engine::ContentRef {
            content_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_owned()),
            provider_kind: Some(PROVIDER_KIND_THINKING.to_owned()),
        },
        preview: Some(preview),
        origin: None,
        provenance_ref: None,
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
        content: engine::ContentRef {
            content_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_owned()),
            provider_kind: Some(provider_kind.to_owned()),
        },
        preview: Some(opaque_preview(block)),
        origin: None,
        provenance_ref: None,
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
        // Billed thinking tokens; a subset of `output_tokens`, reported
        // regardless of whether the summary text was returned.
        reasoning_tokens: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details.thinking_tokens)
            .map(u64_to_u32),
        total_tokens: match (input_tokens, output_tokens) {
            (Some(input), Some(output)) => Some(u64_to_u32(input + output)),
            _ => None,
        },
        cached_input_tokens: usage.cache_read_input_tokens.map(u64_to_u32),
        cache_write_input_tokens: usage.cache_creation_input_tokens.map(u64_to_u32),
        cache_miss_input_tokens: usage.input_tokens.map(u64_to_u32),
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
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use engine::{
        BlobRef, ContextEntryId, ContextEntrySource, ContextSnapshot, CoreAgentLlm,
        FunctionToolSpec, ModelSelection, ProviderParams, RunId, SessionId, ToolParallelism,
        TurnId, storage::InMemoryBlobStore,
    };
    use llm_clients::HeaderSnapshot;
    use serde_json::json;

    use super::*;

    struct StaticMcpInventory;

    #[async_trait]
    impl McpInventoryResolver for StaticMcpInventory {
        async fn list_tools(
            &self,
            _spec: &RemoteMcpToolSpec,
        ) -> Result<Vec<crate::NativeMcpTool>, crate::McpInventoryError> {
            Ok(vec![crate::NativeMcpTool {
                remote_name: "read".to_owned(),
                description: Some("Read".to_owned()),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }])
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_mcp_lowers_to_anthropic_custom_function_tools() {
        let blobs = InMemoryBlobStore::new();
        let (tools, servers) = materialize_tools(
            &StaticMcpInventory,
            "claude-opus-4-8",
            &mut crate::tool_catalog::ToolCatalog::resolve(
                &blobs,
                &tools::runtime::ToolTarget::api_kind(ProviderApiKind::AnthropicMessages),
                &[ToolSpec {
                    name: ToolName::try_new("mcp_docs").expect("name"),
                    kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                        server_id: "docs".to_owned(),
                        record_revision: 1,
                        server_label: "docs".to_owned(),
                        server_url: "https://example.com/mcp".to_owned(),
                        description_ref: None,
                        allowed_tools: None,
                        execution: RemoteMcpExecution::Native,
                        exposure: RemoteMcpExposure::Inject,
                        approval: RemoteMcpApprovalPolicy::Never,
                        defer_loading: None,
                        auth_ref: None,
                        auth_required: false,
                        allow_private_network: false,
                    }),
                    execution: Default::default(),
                    parallelism: ToolParallelism::ParallelSafe,
                }],
            )
            .await
            .expect("catalog"),
        )
        .await
        .expect("materialize native MCP");
        assert!(servers.is_empty());
        let value = serde_json::to_value(tools).expect("tools json");
        assert_eq!(value[0]["name"], "mcp_docs__read");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_lowers_anthropic_hosted_web_tools() {
        let blobs = InMemoryBlobStore::new();
        let mut config = tools::toolset::ToolsetConfig::empty();
        config.web.search = Some(tools::web::search::WebSearchToolConfig::new(
            vec!["example.com".to_owned()],
            Vec::new(),
        ));
        config.web.fetch = true;
        let resolved = tools::toolset::register_toolset(&config).expect("resolve hosted web tools");

        let mut request = intent_request(Vec::new());
        request.tools = resolved.tools.into_values().collect();

        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request)
                .await
                .expect("materialize hosted web tools"),
        )
        .expect("json");

        assert_eq!(value["tools"][0]["type"], "web_fetch_20250910");
        assert_eq!(value["tools"][0]["citations"]["enabled"], true);
        assert_eq!(value["tools"][1]["type"], "web_search_20250305");
        assert_eq!(value["tools"][1]["allowed_domains"], json!(["example.com"]));
        assert_eq!(
            value["tools"][1]["cache_control"],
            json!({ "type": "ephemeral" })
        );
    }
    use crate::executor::{LlmAdapterRegistry, LlmRuntime};
    use crate::params::{AnthropicMessagesParams, AnthropicThinkingConfig};

    struct FakeAnthropicMessagesApi {
        responses: Mutex<VecDeque<ApiResponse<am::Message>>>,
        seen: Mutex<Vec<am::CreateMessageRequest>>,
        seen_api_keys: Mutex<Vec<Option<String>>>,
    }

    fn observed_auth(auth: Option<llm_clients::RequestAuth<'_>>) -> Option<String> {
        auth.map(|auth| match auth {
            llm_clients::RequestAuth::None => "none".to_owned(),
            llm_clients::RequestAuth::ApiKey(value) => format!("api_key:{value}"),
            llm_clients::RequestAuth::Bearer(value) => format!("bearer:{value}"),
        })
    }

    #[async_trait]
    impl AnthropicMessagesApi for FakeAnthropicMessagesApi {
        async fn create(
            &self,
            request: am::CreateMessageRequest,
            auth: Option<llm_clients::RequestAuth<'_>>,
        ) -> Result<ApiResponse<am::Message>, llm_clients::LlmApiError> {
            self.seen.lock().expect("lock").push(request);
            self.seen_api_keys
                .lock()
                .expect("lock")
                .push(observed_auth(auth));
            self.responses
                .lock()
                .expect("lock")
                .pop_front()
                .ok_or_else(|| {
                    llm_clients::LlmApiError::Decode(llm_clients::DecodeError::new(
                        "fake Anthropic response queue exhausted",
                    ))
                })
        }
    }

    fn fake_api(raw_json: Value) -> Arc<FakeAnthropicMessagesApi> {
        fake_api_sequence(vec![raw_json])
    }

    fn fake_api_sequence(raw_responses: Vec<Value>) -> Arc<FakeAnthropicMessagesApi> {
        Arc::new(FakeAnthropicMessagesApi {
            responses: Mutex::new(
                raw_responses
                    .into_iter()
                    .map(|raw_json| ApiResponse {
                        parsed: serde_json::from_value(raw_json.clone()).expect("message"),
                        raw_json,
                        status: 200,
                        headers: HeaderSnapshot::default(),
                    })
                    .collect(),
            ),
            seen: Mutex::new(Vec::new()),
            seen_api_keys: Mutex::new(Vec::new()),
        })
    }

    async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
        blobs.insert_text(text).await
    }

    /// Cache markers are placement, not content: tests about lowering compare
    /// the content and assert the markers separately.
    fn without_cache_control(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .filter(|(key, _)| key != "cache_control")
                    .map(|(key, value)| (key, without_cache_control(value)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(without_cache_control).collect())
            }
            other => other,
        }
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
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
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

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_derives_thinking_from_reasoning_effort() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("max".to_string());

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        assert_eq!(value["output_config"], json!({ "effort": "max" }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_none_reasoning_effort_disables_thinking() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("none".to_string());

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        // Explicit: models that think by default would otherwise reason.
        assert_eq!(value["thinking"], json!({ "type": "disabled" }));
        assert!(value.get("output_config").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_without_effort_sends_no_thinking_config() {
        let blobs = InMemoryBlobStore::new();
        let request = intent_request(Vec::new());

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert!(value.get("thinking").is_none());
        assert_eq!(value["max_tokens"], json!(DEFAULT_MAX_OUTPUT_TOKENS));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_keeps_explicit_thinking_display() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.params = Some(anthropic_params(&AnthropicMessagesParams {
            thinking: Some(AnthropicThinkingConfig {
                r#type: "adaptive".to_string(),
                budget_tokens: None,
                display: Some("omitted".to_string()),
                extra: BTreeMap::new(),
            }),
            ..AnthropicMessagesParams::default()
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["thinking"],
            json!({ "type": "adaptive", "display": "omitted" })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_never_adds_display_to_disabled_thinking() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.params = Some(anthropic_params(&AnthropicMessagesParams {
            thinking: Some(AnthropicThinkingConfig {
                r#type: "disabled".to_string(),
                budget_tokens: None,
                display: None,
                extra: BTreeMap::new(),
            }),
            ..AnthropicMessagesParams::default()
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(value["thinking"], json!({ "type": "disabled" }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_params_thinking_wins_over_reasoning_effort() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("high".to_string());
        request.output_limit = Some(2048);
        request.params = Some(anthropic_params(&AnthropicMessagesParams {
            thinking: Some(AnthropicThinkingConfig {
                r#type: "enabled".to_string(),
                budget_tokens: Some(512),
                display: None,
                extra: BTreeMap::new(),
            }),
            ..AnthropicMessagesParams::default()
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        // Explicit params keep their mode and budget; the display default
        // still fills in so the reasoning entries carry text.
        assert_eq!(
            value["thinking"],
            json!({ "type": "enabled", "budget_tokens": 512, "display": "summarized" })
        );
        assert!(value.get("output_config").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_rejects_unknown_reasoning_effort() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("ultra".to_string());

        let error = materialize_create_request(&blobs, &request)
            .await
            .expect_err("unknown effort must fail");

        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
        assert!(error.to_string().contains("unknown reasoning effort"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_maps_parallel_tool_use_onto_tool_choice() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tool_choice = Some(ToolChoice::Auto);
        request.parallel_tool_use = Some(false);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["tool_choice"],
            json!({ "type": "auto", "disable_parallel_tool_use": true })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_parallel_tool_use_without_tool_choice_lowers_auto() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.parallel_tool_use = Some(true);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["tool_choice"],
            json!({ "type": "auto", "disable_parallel_tool_use": false })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_none_tool_choice_ignores_parallel_tool_use() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tool_choice = Some(ToolChoice::None);
        request.parallel_tool_use = Some(true);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(value["tool_choice"], json!({ "type": "none" }));
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
            content: engine::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
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
            content: item.content.clone(),
            preview: item.preview.clone(),
            origin: None,
            provenance_ref: item.provenance_ref.clone(),
            token_estimate: item.token_estimate.clone(),
            supersedes: None,
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
            content: engine::ContentRef::text(instructions_ref),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };
        let mut request = intent_request(vec![instructions_item, user_entry(2, input_ref)]);
        request.tools = vec![ToolSpec {
            name: ToolName::new("read_file"),
            execution: Default::default(),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: Some(description_ref),
                input_schema_ref: schema_ref,
                output_schema_ref: None,
                strict: Some(true),
                provider_options_ref: Some(provider_options_ref),
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }];
        request.tool_choice = Some(ToolChoice::Specific {
            tool_name: ToolName::new("read_file"),
        });
        request.parallel_tool_use = Some(false);
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
            prompt_cache_ttl: None,
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
                    "content": [{
                        "type": "text",
                        "text": "Read Cargo.toml",
                        "cache_control": { "type": "ephemeral" }
                    }]
                }],
                "system": [{
                    "type": "text",
                    "text": "Be precise.",
                    "cache_control": { "type": "ephemeral" }
                }],
                "metadata": { "user_id": "user-1" },
                "stop_sequences": ["<END>"],
                "stream": false,
                "temperature": 0.2,
                "thinking": { "type": "enabled", "budget_tokens": 1024, "display": "summarized" },
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
                content: engine::ContentRef {
                    content_ref: thinking_ref,
                    media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                    provider_kind: Some(PROVIDER_KIND_THINKING.to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
                supersedes: None,
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
                content: engine::ContentRef {
                    content_ref: assistant_ref,
                    media_type: Some("text/plain".to_owned()),
                    provider_kind: Some("anthropic.messages.text".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
                supersedes: None,
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
                content: engine::ContentRef {
                    content_ref: tool_use_ref,
                    media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                    provider_kind: Some(PROVIDER_KIND_TOOL_USE.to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
                supersedes: None,
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
                content: engine::ContentRef::text(tool_result_ref),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
                supersedes: None,
            },
            user_entry(6, followup_ref),
        ];
        let request = intent_request(entries);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            without_cache_control(value["messages"].clone()),
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
            content: engine::ContentRef {
                content_ref: raw_message_ref,
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                provider_kind: Some(ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND.to_owned()),
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };
        let request = intent_request(vec![raw_entry, user_entry(2, followup_ref)]);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            without_cache_control(value["messages"].clone()),
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
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "echo".to_string(),
                record_revision: 1,
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: None,
                allowed_tools: Some(vec!["echo".to_string()]),
                execution: RemoteMcpExecution::Provider,
                exposure: RemoteMcpExposure::Inject,
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: None,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            parallelism: ToolParallelism::ParallelSafe,
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
                "name": "echo"
            }])
        );
        assert_eq!(
            without_cache_control(value["tools"].clone()),
            json!([{
                "type": "mcp_toolset",
                "mcp_server_name": "echo",
                "default_config": { "enabled": false },
                "configs": { "echo": { "enabled": true } }
            }])
        );
        assert_eq!(
            value["tools"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
    }

    fn provider_mcp_tool(
        server_id: &str,
        allowed_tools: Option<Vec<&str>>,
        defer_loading: Option<bool>,
    ) -> ToolSpec {
        ToolSpec {
            name: ToolName::try_new(format!("mcp_{server_id}")).expect("tool name"),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: server_id.to_owned(),
                record_revision: 1,
                server_label: server_id.to_owned(),
                server_url: format!("https://{server_id}.example.com/mcp"),
                description_ref: None,
                allowed_tools: allowed_tools
                    .map(|tools| tools.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>()),
                execution: RemoteMcpExecution::Provider,
                exposure: RemoteMcpExposure::Inject,
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_provider_mcp_lowers_to_bm25_search_and_deferred_toolset() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tools = vec![provider_mcp_tool(
            "github",
            Some(vec!["search_issues", "read_issue"]),
            Some(true),
        )];

        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request)
                .await
                .expect("materialize"),
        )
        .expect("json");

        assert_eq!(
            without_cache_control(value["tools"].clone()),
            json!([
                {
                    "type": "tool_search_tool_bm25_20251119",
                    "name": "tool_search_tool_bm25"
                },
                {
                    "type": "mcp_toolset",
                    "mcp_server_name": "github",
                    "default_config": {
                        "enabled": false,
                        "defer_loading": true
                    },
                    "configs": {
                        "search_issues": { "enabled": true },
                        "read_issue": { "enabled": true }
                    }
                }
            ])
        );
        assert_eq!(
            value["tools"][0]["cache_control"],
            json!({ "type": "ephemeral" }),
            "the search tool is the final non-deferred tool"
        );
        assert!(
            value["tools"][1].get("cache_control").is_none(),
            "a deferred toolset cannot carry a cache breakpoint"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn multiple_provider_mcp_toolsets_emit_one_search_and_preserve_non_deferred_server() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tools = vec![
            provider_mcp_tool("github", None, Some(true)),
            provider_mcp_tool("linear", None, Some(true)),
            provider_mcp_tool("calendar", None, None),
        ];

        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request)
                .await
                .expect("materialize"),
        )
        .expect("json");
        let tools = value["tools"].as_array().expect("tools");
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool["type"] == TOOL_SEARCH_TOOL_TYPE)
                .count(),
            1
        );
        assert_eq!(tools[1]["default_config"]["defer_loading"], true);
        assert_eq!(tools[2]["default_config"]["defer_loading"], true);
        assert!(tools[3].get("default_config").is_none());
        assert_eq!(
            tools[3]["cache_control"],
            json!({ "type": "ephemeral" }),
            "the last non-deferred toolset owns the breakpoint"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_provider_mcp_rejects_known_unsupported_anthropic_model() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.model.model = "claude-3-7-sonnet-20250219".to_owned();
        request.tools = vec![provider_mcp_tool("github", None, Some(true))];

        let error = materialize_create_request(&blobs, &request)
            .await
            .expect_err("Claude 3.7 must reject tool search");
        assert!(matches!(
            error,
            LlmAdapterError::UnsupportedModelFeature {
                feature: "Anthropic tool search",
                ..
            }
        ));
    }

    #[test]
    fn anthropic_tool_search_model_gate_handles_current_old_and_unknown_ids() {
        assert_eq!(
            anthropic_tool_search_model_support("claude-sonnet-4-5-20250929"),
            Some(true)
        );
        assert_eq!(
            anthropic_tool_search_model_support("claude-opus-5"),
            Some(true)
        );
        assert_eq!(
            anthropic_tool_search_model_support("claude-3-7-sonnet-20250219"),
            Some(false)
        );
        assert_eq!(
            anthropic_tool_search_model_support("custom-anthropic-compatible"),
            None
        );
    }

    fn auth_remote_mcp_tool() -> ToolSpec {
        ToolSpec {
            name: ToolName::new("mcp_echo"),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "echo".to_string(),
                record_revision: 1,
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: None,
                allowed_tools: None,
                execution: RemoteMcpExecution::Provider,
                exposure: RemoteMcpExposure::Inject,
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: None,
                auth_ref: Some(engine::SecretRef {
                    namespace: "mcp_server".to_string(),
                    id: "echo".to_string(),
                }),
                auth_required: true,
                allow_private_network: false,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }
    }

    fn mcp_auth_generation_request() -> LlmGenerationRequest {
        LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: {
                let mut request = intent_request(Vec::new());
                request.tools = vec![auth_remote_mcp_tool()];
                request.output_limit = Some(256);
                request
            },
        }
    }

    fn optional_unbound_mcp_generation_request() -> LlmGenerationRequest {
        let mut request = mcp_auth_generation_request();
        let ToolKind::RemoteMcp(spec) = &mut request.request.tools[0].kind else {
            panic!("expected remote MCP tool");
        };
        spec.auth_required = false;
        request
    }

    fn completed_text_response_json() -> Value {
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "Done." }],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_omits_authorization_token_for_remote_mcp_auth_ref() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tools = vec![auth_remote_mcp_tool()];
        request.output_limit = Some(1024);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(value["mcp_servers"][0]["name"], json!("echo"));
        assert!(
            value["mcp_servers"][0].get("authorization_token").is_none(),
            "materialized requests must not carry auth values: {value}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_injects_remote_mcp_authorization_token_and_redacts_persisted_request() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs.clone())
            .with_debug_dumps(true)
            .with_secret_resolver(Arc::new(
                crate::secrets::StaticSecretResolver::new().with_secret(
                    "mcp_server",
                    "echo",
                    "token-xyz",
                ),
            ));

        let mut generation_request = mcp_auth_generation_request();
        let ToolKind::RemoteMcp(remote_mcp) = &mut generation_request.request.tools[0].kind else {
            panic!("expected remote MCP tool");
        };
        remote_mcp.defer_loading = Some(true);
        let execution = LlmGenerationAdapter::generate(&adapter, generation_request)
            .await
            .expect("generate");

        let sent = api.seen.lock().expect("lock").clone();
        assert_eq!(sent.len(), 1);
        let sent_json = serde_json::to_value(&sent[0]).expect("sent json");
        assert_eq!(
            sent_json["mcp_servers"][0]["authorization_token"],
            json!("token-xyz")
        );

        let dumps = execution.debug_dumps.expect("debug dumps are enabled");
        let stored = crate::blob_io::read_json(blobs.as_ref(), &dumps.provider_request_ref)
            .await
            .expect("stored provider request");
        assert_eq!(
            stored["mcp_servers"][0]["authorization_token"],
            json!("<redacted>")
        );
        assert_eq!(
            stored["tools"][0]["type"],
            json!("tool_search_tool_bm25_20251119")
        );
        assert_eq!(stored["tools"][1]["default_config"]["defer_loading"], true);
        assert!(
            !serde_json::to_string(&stored)
                .expect("stored string")
                .contains("token-xyz"),
            "persisted provider request must not contain the resolved token"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_continues_pause_turn_with_exact_assistant_blocks_and_merges_usage() {
        let paused_content = json!([
            {
                "type": "server_tool_use",
                "id": "srvtoolu_1",
                "name": "web_search",
                "input": { "query": "lightspeed agent runtime" }
            },
            {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_1",
                "content": [{
                    "type": "web_search_result",
                    "url": "https://example.com",
                    "encrypted_content": "opaque-result"
                }]
            }
        ]);
        let api = fake_api_sequence(vec![
            json!({
                "id": "msg_paused",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "stop_reason": "pause_turn",
                "content": paused_content.clone(),
                "usage": { "input_tokens": 10, "output_tokens": 2 }
            }),
            json!({
                "id": "msg_final",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "Found it." }],
                "usage": { "input_tokens": 20, "output_tokens": 3 }
            }),
        ]);
        let blobs = Arc::new(InMemoryBlobStore::new());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs);
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: intent_request(Vec::new()),
        };

        let execution = LlmGenerationAdapter::generate(&adapter, request)
            .await
            .expect("continue paused server-tool turn");

        let sent = api.seen.lock().expect("lock");
        assert_eq!(sent.len(), 2);
        let followup = serde_json::to_value(&sent[1]).expect("follow-up request");
        assert_eq!(followup["messages"][0]["role"], "assistant");
        assert_eq!(followup["messages"][0]["content"], paused_content);
        assert_eq!(execution.result.facts.finish, LlmFinish::Stop);
        assert_eq!(
            execution.result.facts.provider_response_id.as_deref(),
            Some("msg_final")
        );
        let usage = execution.result.facts.usage.expect("combined usage");
        assert_eq!(usage.input_tokens, Some(30));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(35));
        assert_eq!(execution.result.context_entries.len(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_omits_optional_unbound_remote_mcp_authorization() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs)
            .with_secret_resolver(Arc::new(crate::secrets::AbsentSecretResolver));

        LlmGenerationAdapter::generate(&adapter, optional_unbound_mcp_generation_request())
            .await
            .expect("optional unbound MCP auth should be omitted");

        let sent = api.seen.lock().expect("lock").clone();
        let sent_json = serde_json::to_value(&sent[0]).expect("sent json");
        assert!(
            sent_json["mcp_servers"][0]
                .get("authorization_token")
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_does_not_treat_a_missing_optional_mcp_server_as_unbound() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs)
            .with_secret_resolver(Arc::new(crate::secrets::StaticSecretResolver::new()));

        let error =
            LlmGenerationAdapter::generate(&adapter, optional_unbound_mcp_generation_request())
                .await
                .expect_err("missing optional MCP server must not silently downgrade");

        assert!(matches!(error, LlmAdapterError::SecretResolution { .. }));
        assert!(api.seen.lock().expect("lock").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_fails_before_provider_io_when_auth_ref_cannot_be_resolved() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs.clone());

        let error = LlmGenerationAdapter::generate(&adapter, mcp_auth_generation_request())
            .await
            .expect_err("unresolvable auth ref must fail generation");

        assert!(matches!(error, LlmAdapterError::SecretResolution { .. }));
        assert!(
            api.seen.lock().expect("lock").is_empty(),
            "no provider call may happen when auth resolution fails"
        );
    }

    fn generation_request() -> LlmGenerationRequest {
        LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: {
                let mut request = intent_request(Vec::new());
                request.output_limit = Some(256);
                request
            },
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_passes_stored_provider_key_to_the_client() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(
                crate::provider_keys::StaticProviderKeys::new().with_key("anthropic", "stored-key"),
            ));

        LlmGenerationAdapter::generate(&adapter, generation_request())
            .await
            .expect("generate");

        assert_eq!(
            api.seen_api_keys.lock().expect("lock").clone(),
            vec![Some("api_key:stored-key".to_owned())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_passes_stored_bearer_auth_to_the_client() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(
                crate::provider_keys::StaticProviderKeys::new()
                    .with_bearer("anthropic", "oauth-token"),
            ));

        LlmGenerationAdapter::generate(&adapter, generation_request())
            .await
            .expect("generate");

        assert_eq!(
            api.seen_api_keys.lock().expect("lock").clone(),
            vec![Some("bearer:oauth-token".to_owned())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_uses_client_key_when_no_stored_provider_key_exists() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api(completed_text_response_json());
        let adapter = AnthropicMessagesLlmAdapter::new(api.clone(), blobs);

        LlmGenerationAdapter::generate(&adapter, generation_request())
            .await
            .expect("generate");

        assert_eq!(api.seen_api_keys.lock().expect("lock").clone(), vec![None]);
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

        let mut mcp_approval = intent_request(Vec::new());
        mcp_approval.tools = vec![ToolSpec {
            name: ToolName::new("mcp_echo"),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "echo".to_string(),
                record_revision: 1,
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: None,
                allowed_tools: None,
                execution: RemoteMcpExecution::Provider,
                exposure: RemoteMcpExposure::Inject,
                approval: RemoteMcpApprovalPolicy::Always,
                defer_loading: None,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }];
        let error = materialize_create_request(&blobs, &mcp_approval)
            .await
            .expect_err("mcp approval must fail");
        assert!(error.to_string().contains("no approval flow"), "{error}");
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
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "output_tokens_details": { "thinking_tokens": 3 }
            }
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
                request.tools = vec![tools::definitions::register(
                    "env.read_file",
                    tools::definitions::BuiltinSettings {
                        presentation: tools::toolset::BuiltinToolPresentation::Canonical,
                        ..Default::default()
                    },
                    ToolParallelism::ParallelSafe,
                    Default::default(),
                )];
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
        assert_eq!(
            result
                .facts
                .usage
                .as_ref()
                .and_then(|usage| usage.reasoning_tokens),
            Some(3),
            "billed thinking tokens must surface as reasoning tokens"
        );
        assert_eq!(result.facts.tool_calls.len(), 1);
        assert_eq!(
            result.facts.tool_calls[0].tool_id,
            Some(ToolName::new("env.read_file"))
        );
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
        assert!(
            result
                .context_entries
                .iter()
                .find(|entry| matches!(&entry.kind, ContextEntryKind::ToolCall { .. }))
                .expect("tool-call context entry")
                .preview
                .is_none(),
            "tool-call arguments must remain CAS-backed instead of being copied into preview",
        );

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
            without_cache_control(followup_json["messages"].clone()),
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

    /// An `omitted` display returns thinking blocks with an empty summary;
    /// they replay unchanged but preview as opaque state, never as text.
    #[tokio::test(flavor = "current_thread")]
    async fn result_marks_omitted_thinking_as_opaque_reasoning_state() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "", "signature": "sig_1" },
                { "type": "text", "text": "42" }
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

        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::ReasoningState
        ));
        assert_eq!(
            result.context_entries[0].preview.as_deref(),
            Some("reasoning state")
        );
        let replayed = read_json(&blobs, &result.context_entries[0].content.content_ref)
            .await
            .expect("raw block");
        assert_eq!(replayed["signature"], "sig_1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_captures_server_tool_blocks_as_provider_opaque_context() {
        let blobs = InMemoryBlobStore::new();
        let server_tool_use = json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": { "query": "lightspeed agent runtime" }
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
            result.context_entries[0].content.provider_kind.as_deref(),
            Some(ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[1].content.provider_kind.as_deref(),
            Some(ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND)
        );
        let retained: Value = read_json(&blobs, &result.context_entries[0].content.content_ref)
            .await
            .expect("raw server tool use");
        assert_eq!(retained, server_tool_use);
        assert_eq!(result.facts.finish, LlmFinish::Stop);
    }

    fn assistant_response(content: Value, stop_reason: &str) -> ApiResponse<am::Message> {
        let raw_json = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "stop_reason": stop_reason,
            "content": content
        });
        ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("message"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        }
    }

    async fn replayed_content(blobs: &InMemoryBlobStore, result: &LlmGenerationResult) -> Value {
        let entries = result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(index, item))
            .collect();
        let replayed = materialize_create_request(blobs, &intent_request(entries))
            .await
            .expect("replay assistant output");
        let mut content =
            serde_json::to_value(replayed).expect("json")["messages"][0]["content"].clone();
        // The adapter's cache breakpoint lands on the last replayed block; it
        // is request policy, not part of the provider's retained blocks.
        for block in content.as_array_mut().into_iter().flatten() {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
        content
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_run_is_one_native_message_with_exact_blocks() {
        let blobs = InMemoryBlobStore::new();
        let blocks = json!([
            { "type": "text", "text": "The big one is " },
            {
                "type": "text",
                "text": "documented here",
                "citations": [{
                    "type": "web_search_result_location",
                    "url": "https://example.com/docs",
                    "title": "Lightspeed docs",
                    "encrypted_index": "opaque-index",
                    "cited_text": "documented here"
                }]
            },
            { "type": "text", "text": "." }
        ]);
        let response = assistant_response(blocks.clone(), "end_turn");

        let result = result_from_response(&blobs, &generation_request(), &response)
            .await
            .expect("result");

        assert_eq!(result.context_entries.len(), 1);
        let message = &result.context_entries[0];
        assert!(matches!(
            message.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ));
        let native = read_json(&blobs, &message.content.content_ref)
            .await
            .expect("native text blocks");
        assert_eq!(native, blocks);
        assert_eq!(
            llm_clients::content::anthropic_text_blocks(&native).as_deref(),
            Some("The big one is documented here.")
        );
        assert_eq!(replayed_content(&blobs, &result).await, blocks);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_tool_blocks_split_text_runs_and_keep_block_order() {
        let blobs = InMemoryBlobStore::new();
        let server_tool_use = json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": { "query": "lightspeed" }
        });
        let server_tool_result = json!({
            "type": "web_search_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": [{
                "type": "web_search_result",
                "url": "https://example.com",
                "title": "Example",
                "encrypted_content": "opaque-result"
            }]
        });
        let response = assistant_response(
            json!([
                { "type": "text", "text": "I'll look that up." },
                server_tool_use,
                server_tool_result,
                { "type": "text", "text": "Found it." }
            ]),
            "end_turn",
        );

        let result = result_from_response(&blobs, &generation_request(), &response)
            .await
            .expect("result");

        let kinds = result
            .context_entries
            .iter()
            .map(|entry| entry.kind.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant
                },
                ContextEntryKind::ProviderOpaque,
                ContextEntryKind::ProviderOpaque,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant
                },
            ]
        );
        assert_eq!(
            result.context_entries[0].preview.as_deref(),
            Some("I'll look that up.")
        );
        assert_eq!(
            result.context_entries[3].preview.as_deref(),
            Some("Found it.")
        );

        let replayed = replayed_content(&blobs, &result).await;
        assert_eq!(replayed[0]["text"], "I'll look that up.");
        assert_eq!(replayed[1], server_tool_use);
        assert_eq!(replayed[2], server_tool_result);
        assert_eq!(replayed[3]["text"], "Found it.");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_result_and_cited_blocks_replay_in_order() {
        let blobs = InMemoryBlobStore::new();
        let fetched_result = json!({
            "type": "web_fetch_tool_result",
            "tool_use_id": "srvtoolu_1",
            "content": {
                "type": "web_fetch_result",
                "url": "https://example.com/page",
                "content": { "type": "document", "source": { "type": "text", "data": "body" } }
            }
        });
        let cited_text = json!({
            "type": "text",
            "text": "A cited passage.",
            "citations": [{
                "type": "char_location",
                "document_index": 0,
                "document_title": "Example page",
                "start_char_index": 0,
                "end_char_index": 7,
                "cited_text": "A cited"
            }]
        });
        let response = assistant_response(
            json!([fetched_result.clone(), cited_text.clone()]),
            "end_turn",
        );

        let result = result_from_response(&blobs, &generation_request(), &response)
            .await
            .expect("result");

        let kinds = result
            .context_entries
            .iter()
            .map(|entry| entry.content.provider_kind.as_deref().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
                ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
            ]
        );
        assert_eq!(
            replayed_content(&blobs, &result).await,
            json!([fetched_result, cited_text])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn truncated_cited_text_replays_as_plain_text() {
        let blobs = InMemoryBlobStore::new();
        let response = assistant_response(
            json!([{
                "type": "text",
                "text": "Cut off mid",
                "citations": [{
                    "type": "web_search_result_location",
                    "url": "https://example.com/docs",
                    "title": "Lightspeed docs",
                    "encrypted_index": "opaque-index",
                    "cited_text": "Cut off"
                }]
            }]),
            "max_tokens",
        );

        let result = result_from_response(&blobs, &generation_request(), &response)
            .await
            .expect("result");

        assert_eq!(result.status, LlmGenerationStatus::Failed);
        assert_eq!(result.context_entries.len(), 1);
        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ));
        let replayed = replayed_content(&blobs, &result).await;
        assert_eq!(replayed[0]["type"], "text");
        assert_eq!(replayed[0]["text"], "Cut off mid");
        assert!(replayed[0].get("citations").is_none());
    }

    /// A `max_tokens` cut-off fails the turn but keeps the partial text; the
    /// unfinished thinking and the tool call (no result to replay against)
    /// are dropped, and the failure names the cap and the spend.
    #[tokio::test(flavor = "current_thread")]
    async fn result_fails_the_turn_on_truncation_but_keeps_partial_text() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "msg_cut",
            "type": "message",
            "role": "assistant",
            "stop_reason": "max_tokens",
            "content": [
                { "type": "thinking", "thinking": "Half a thought", "signature": "sig" },
                { "type": "text", "text": "The bicycle was" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "read_file",
                    "input": { "path": "Cargo.toml" }
                }
            ],
            "usage": {
                "input_tokens": 20,
                "output_tokens": 64,
                "output_tokens_details": { "thinking_tokens": 40 }
            }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("message"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(2),
            turn_id: TurnId::new(5),
            request: {
                let mut request = intent_request(Vec::new());
                request.output_limit = Some(64);
                request
            },
        };

        let result = result_from_response(&blobs, &request, &response)
            .await
            .expect("result");

        assert_eq!(result.status, LlmGenerationStatus::Failed);
        assert_eq!(result.facts.finish, LlmFinish::Length);
        assert!(result.facts.tool_calls.is_empty(), "no tool call may run");
        assert_eq!(
            result.context_entries.len(),
            1,
            "{:?}",
            result.context_entries
        );
        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ));
        assert_eq!(
            result.context_entries[0].preview.as_deref(),
            Some("The bicycle was")
        );
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(failure.contains("run_id=2"), "{failure}");
        assert!(failure.contains("turn_id=5"), "{failure}");
        assert!(
            failure
                .contains("cut off at max output tokens 64 after 64 output tokens (40 thinking)"),
            "{failure}"
        );
        assert!(failure.contains("partial output is kept"), "{failure}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_fails_the_turn_on_refusal() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "msg_refused",
            "type": "message",
            "role": "assistant",
            "stop_reason": "refusal",
            "stop_details": {
                "type": "refusal",
                "category": "cyber",
                "explanation": "This request triggered restrictions on violative cyber content."
            },
            "content": [{ "type": "text", "text": "partial" }],
            "usage": { "input_tokens": 20, "output_tokens": 0 }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("message"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(3),
            turn_id: TurnId::new(7),
            request: intent_request(Vec::new()),
        };

        let result = result_from_response(&blobs, &request, &response)
            .await
            .expect("result");

        assert_eq!(result.status, LlmGenerationStatus::Failed);
        assert_eq!(result.facts.finish, LlmFinish::ContentFilter);
        assert!(
            result.context_entries.is_empty(),
            "partial content must not land in the session log"
        );
        assert_eq!(
            result.facts.provider_response_id.as_deref(),
            Some("msg_refused")
        );
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(failure.contains("run_id=3"), "{failure}");
        assert!(failure.contains("turn_id=7"), "{failure}");
        assert!(failure.contains("(category: cyber)"), "{failure}");
        assert!(
            failure.contains("violative cyber content"),
            "the classifier explanation must reach the failure: {failure}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_maps_stop_reasons_to_finish() {
        for (stop_reason, expected) in [
            ("end_turn", LlmFinish::Stop),
            ("max_tokens", LlmFinish::Length),
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
            entry.content.provider_kind.as_deref(),
            Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
        );
        assert_eq!(
            blobs
                .read_text(&entry.content.content_ref)
                .await
                .expect("summary text"),
            "Summary: the user chose Postgres as the session store."
        );

        let seen = api.seen.lock().expect("seen");
        assert_eq!(seen.len(), 1);
        let request_json = serde_json::to_value(&seen[0]).expect("request json");
        assert_eq!(request_json["model"], "claude-opus-4-8");
        // The cap leaves room for thinking above the summary budget.
        assert_eq!(
            request_json["max_tokens"],
            128 + COMPACTION_THINKING_HEADROOM_TOKENS
        );
        let messages = request_json["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        let instruction = messages[1]["content"].as_str().expect("instruction text");
        assert!(instruction.contains("context compaction"), "{instruction}");
        assert!(instruction.contains("under 128 tokens"), "{instruction}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inserted_skill_text_lowers_as_an_ordinary_user_message() {
        let blobs = InMemoryBlobStore::new();
        let skill_text_ref = text_blob(&blobs, "# Deploy Review\n\nCheck rollout scope.").await;
        let entry = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::Runtime {
                label: "inserted-skill".to_string(),
            },
            content: engine::ContentRef {
                content_ref: skill_text_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let materialized = materialize_create_request(&blobs, &intent_request(vec![entry]))
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            without_cache_control(value["messages"].clone()),
            json!([{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "# Deploy Review\n\nCheck rollout scope."
                }]
            }])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn image_message_entry_materializes_as_image_block() {
        let blobs = InMemoryBlobStore::new();
        let image_bytes = vec![0xff, 0xd8, 0xff, 0xe0];
        let content_ref = blobs
            .put_bytes(image_bytes.clone())
            .await
            .expect("store image");
        let entry = ContextEntry {
            entry_id: ContextEntryId::new(1),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: Some("image/jpeg".to_owned()),
                provider_kind: None,
            },
            preview: Some("[image]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let (role, blocks) = materialize_block(&blobs, &entry)
            .await
            .expect("materialize image entry");

        assert_eq!(role, am::MessageRole::User);
        assert_eq!(blocks.len(), 2, "announcement then image");
        let announcement = serde_json::to_value(&blocks[0]).expect("serialize announcement");
        assert_eq!(announcement["type"], json!("text"));
        assert_eq!(
            announcement["text"],
            json!(format!(
                "[image · {} · image/jpeg]",
                engine::media::media_handle(&entry.content.content_ref)
            ))
        );
        let block = &blocks[1];
        let value = serde_json::to_value(block).expect("serialize block");
        assert_eq!(value["type"], json!("image"));
        assert_eq!(value["source"]["type"], json!("base64"));
        assert_eq!(value["source"]["media_type"], json!("image/jpeg"));
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(value["source"]["data"].as_str().expect("data"))
            .expect("valid base64");
        assert_eq!(decoded, image_bytes);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pdf_document_entry_materializes_as_document_block() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = blobs
            .put_bytes(b"%PDF-1.4 fake".to_vec())
            .await
            .expect("store pdf");
        let entry = ContextEntry {
            entry_id: ContextEntryId::new(1),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: Some("application/pdf".to_owned()),
                provider_kind: None,
            },
            preview: Some("[document: offer.pdf]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let (role, blocks) = materialize_block(&blobs, &entry)
            .await
            .expect("materialize pdf entry");

        assert_eq!(role, am::MessageRole::User);
        assert_eq!(blocks.len(), 2, "announcement then document");
        let announcement = serde_json::to_value(&blocks[0]).expect("serialize announcement");
        assert_eq!(
            announcement["text"],
            json!(format!(
                "[document: offer.pdf · {} · application/pdf]",
                engine::media::media_handle(&entry.content.content_ref)
            ))
        );
        let block = &blocks[1];
        let value = serde_json::to_value(block).expect("serialize block");
        assert_eq!(value["type"], json!("document"));
        assert_eq!(value["source"]["type"], json!("base64"));
        assert_eq!(value["source"]["media_type"], json!("application/pdf"));
        assert_eq!(value["title"], json!("offer.pdf"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn markdown_document_entry_materializes_as_text_document_block() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = blobs
            .put_bytes(b"# Notes\nhello".to_vec())
            .await
            .expect("store markdown");
        let entry = ContextEntry {
            entry_id: ContextEntryId::new(1),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: Some("text/markdown".to_owned()),
                provider_kind: None,
            },
            preview: Some("[document: notes.md]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let (role, blocks) = materialize_block(&blobs, &entry)
            .await
            .expect("materialize markdown entry");

        assert_eq!(role, am::MessageRole::User);
        assert_eq!(
            blocks.len(),
            1,
            "text documents carry no media announcement"
        );
        let block = &blocks[0];
        let value = serde_json::to_value(block).expect("serialize block");
        assert_eq!(value["type"], json!("document"));
        assert_eq!(value["source"]["type"], json!("text"));
        assert_eq!(value["source"]["media_type"], json!("text/plain"));
        assert_eq!(value["source"]["data"], json!("# Notes\nhello"));
        assert_eq!(value["title"], json!("notes.md"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plain_text_turn_without_document_preview_stays_a_text_block() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = blobs
            .put_bytes(b"just a normal message".to_vec())
            .await
            .expect("store text");
        let entry = ContextEntry {
            entry_id: ContextEntryId::new(1),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef::text(content_ref),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let (_, blocks) = materialize_block(&blobs, &entry)
            .await
            .expect("materialize text entry");

        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        let value = serde_json::to_value(block).expect("serialize block");
        assert_eq!(value["type"], json!("text"));
        assert_eq!(value["text"], json!("just a normal message"));
    }

    fn catalog_entry(id: u64, content_ref: BlobRef, supersedes: Option<u64>) -> ContextEntry {
        ContextEntry {
            key: Some(engine::ContextEntryKey::new("bot:directory")),
            entry_id: ContextEntryId::new(id),
            kind: ContextEntryKind::Catalog {
                title: "Bot directory".to_string(),
            },
            source: ContextEntrySource::ContextEdit,
            content: engine::ContentRef {
                content_ref,
                media_type: Some("text/markdown".to_string()),
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: supersedes.map(ContextEntryId::new),
        }
    }

    /// A catalog update must append, never rewrite: everything rendered
    /// before it is byte-identical, and only the successor carries the
    /// update header. This is what keeps the provider prefix cache warm on
    /// long-lived sessions.
    #[tokio::test(flavor = "current_thread")]
    async fn superseding_catalog_appends_without_moving_the_rendered_prefix() {
        let blobs = InMemoryBlobStore::new();
        let v1_ref = text_blob(&blobs, "- infra: accepts events addressed by you").await;
        let v2_ref = text_blob(
            &blobs,
            "- infra: accepts events addressed by you\n- comms: subscribes",
        )
        .await;
        let input_ref = text_blob(&blobs, "Who can I reach?").await;
        let user = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(2),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef {
                content_ref: input_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let before = materialize_create_request(
            &blobs,
            &intent_request(vec![catalog_entry(1, v1_ref.clone(), None), user.clone()]),
        )
        .await
        .expect("request before the update");
        let after = materialize_create_request(
            &blobs,
            &intent_request(vec![
                catalog_entry(1, v1_ref, None),
                user,
                catalog_entry(3, v2_ref, Some(1)),
            ]),
        )
        .await
        .expect("request after the update");

        // Same-role blocks merge into one message, so compare block lists. The
        // moving cache marker is placement, not content: it sits on the last
        // block of each request and is stripped before comparing.
        let before_blocks = without_cache_control(
            serde_json::to_value(&before.messages).expect("json"),
        )[0]["content"]
            .as_array()
            .cloned()
            .expect("blocks");
        let after_blocks = without_cache_control(
            serde_json::to_value(&after.messages).expect("json"),
        )[0]["content"]
            .as_array()
            .cloned()
            .expect("blocks");
        assert_eq!(after_blocks.len(), before_blocks.len() + 1);
        assert_eq!(&after_blocks[..before_blocks.len()], &before_blocks[..]);
        let successor = after_blocks.last().expect("successor")["text"]
            .as_str()
            .expect("text")
            .to_string();
        assert!(successor.starts_with(crate::catalog_prompts::CATALOG_UPDATE_HEADER));
        assert!(successor.contains("Bot directory:"));
        assert!(successor.ends_with("- comms: subscribes"));
        assert!(
            !before_blocks[0]["text"]
                .as_str()
                .expect("text")
                .contains("Updated catalog")
        );
    }

    /// Prompt caching on Anthropic exists only at explicit markers, so every
    /// request gets the three-breakpoint layout: end of the system prompt,
    /// last non-deferred tool definition, last block of the last message. The
    /// TTL param rides on each marker; a marker a tool brought through
    /// provider options is kept as is.
    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_places_prompt_cache_breakpoints() {
        let blobs = InMemoryBlobStore::new();
        let instructions_ref = text_blob(&blobs, "Be precise.").await;
        let input_ref = text_blob(&blobs, "Read Cargo.toml").await;
        let schema_ref = crate::blob_io::put_json(
            &blobs,
            &json!({ "type": "object", "properties": {}, "required": [] }),
        )
        .await
        .expect("schema");
        let instructions_item = ContextEntry {
            key: Some(engine::ContextEntryKey::new("instructions.000.test")),
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::Instructions,
            source: ContextEntrySource::ContextEdit,
            content: engine::ContentRef::text(instructions_ref),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };
        let tool = |name: &str| ToolSpec {
            name: ToolName::new(name),
            execution: Default::default(),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: None,
                input_schema_ref: schema_ref.clone(),
                output_schema_ref: None,
                strict: None,
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        };
        let mut request = intent_request(vec![instructions_item, user_entry(2, input_ref)]);
        request.tools = vec![tool("first"), tool("last")];
        request.params = Some(anthropic_params(&AnthropicMessagesParams {
            prompt_cache_ttl: Some("1h".to_string()),
            ..AnthropicMessagesParams::default()
        }));

        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request)
                .await
                .expect("materialize"),
        )
        .expect("json");

        let marker = json!({ "type": "ephemeral", "ttl": "1h" });
        assert_eq!(value["system"][0]["cache_control"], marker, "system prompt");
        assert_eq!(value["system"].as_array().map(Vec::len), Some(1));
        assert!(
            value["tools"][0].get("cache_control").is_none(),
            "only the last tool"
        );
        assert_eq!(value["tools"][1]["cache_control"], marker, "last tool");
        let blocks = value["messages"][0]["content"].as_array().expect("blocks");
        assert_eq!(blocks.last().expect("last block")["cache_control"], marker);
        assert_eq!(
            value["messages"].as_array().map(Vec::len),
            Some(1),
            "the marker moves with the last message; it never adds one"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_without_instructions_has_no_system_block() {
        let blobs = InMemoryBlobStore::new();
        let input_ref = text_blob(&blobs, "hi").await;
        let value = serde_json::to_value(
            materialize_create_request(&blobs, &intent_request(vec![user_entry(1, input_ref)]))
                .await
                .expect("materialize"),
        )
        .expect("json");
        assert!(value.get("system").is_none());
        assert_eq!(
            value["messages"][0]["content"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
    }

    #[test]
    fn anthropic_params_reject_unknown_prompt_cache_ttl() {
        let params = anthropic_params(&AnthropicMessagesParams {
            prompt_cache_ttl: Some("2h".to_string()),
            ..AnthropicMessagesParams::default()
        });
        let error = crate::params::anthropic_messages_params(Some(&params))
            .expect_err("2h is not a TTL Anthropic offers");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    /// A tool result followed by the media its tool produced: an image and a
    /// PDF, both tool-sourced user-role message entries.
    async fn tool_result_with_media(blobs: &InMemoryBlobStore) -> Vec<ContextEntry> {
        let tool_source = ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: None,
        };
        let result_ref = blobs
            .insert_text("[image 1 · media:x · image/png · 4 B]")
            .await;
        let image_ref = blobs
            .put_bytes(vec![0x89, 0x50, 0x4e, 0x47])
            .await
            .expect("store image");
        let pdf_ref = blobs
            .put_bytes(b"%PDF-1.4 fake".to_vec())
            .await
            .expect("store pdf");
        let make = |id: u64,
                    kind: ContextEntryKind,
                    content: engine::ContentRef,
                    preview: Option<&str>| {
            ContextEntry {
                entry_id: ContextEntryId::new(id),
                key: None,
                kind,
                source: tool_source.clone(),
                content,
                preview: preview.map(str::to_owned),
                origin: None,
                provenance_ref: None,
                token_estimate: None,
                supersedes: None,
            }
        };
        vec![
            make(
                1,
                ContextEntryKind::ToolResult {
                    call_id: ToolCallId::try_new("call_1").expect("call id"),
                    is_error: false,
                },
                engine::ContentRef::text(result_ref),
                None,
            ),
            make(
                2,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                engine::ContentRef {
                    content_ref: image_ref,
                    media_type: Some("image/png".to_owned()),
                    provider_kind: None,
                },
                Some("[image]"),
            ),
            make(
                3,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                engine::ContentRef {
                    content_ref: pdf_ref,
                    media_type: Some("application/pdf".to_owned()),
                    provider_kind: None,
                },
                Some("[document: report.pdf]"),
            ),
        ]
    }

    /// Two parallel calls commit result A, media A, result B, media B;
    /// Anthropic must receive both results first, then the media in call order.
    #[tokio::test(flavor = "current_thread")]
    async fn parallel_tool_results_stay_ahead_of_their_media() {
        let blobs = InMemoryBlobStore::new();
        let mut entries = tool_result_with_media(&blobs).await;
        let mut second = tool_result_with_media(&blobs).await;
        for (index, entry) in second.iter_mut().enumerate() {
            entry.entry_id = ContextEntryId::new(10 + index as u64);
            if let ContextEntryKind::ToolResult { call_id, .. } = &mut entry.kind {
                *call_id = ToolCallId::try_new("call_2").expect("call id");
            }
        }
        entries.extend(second);
        let messages = materialize_messages(&blobs, &entries)
            .await
            .expect("materialize");
        assert_eq!(messages.len(), 1, "{messages:?}");
        let value = serde_json::to_value(&messages[0]).expect("json");
        let blocks = value["content"].as_array().expect("blocks");
        let kinds = blocks
            .iter()
            .map(|block| block["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "tool_result",
                "tool_result",
                "text",
                "image",
                "text",
                "document",
                "text",
                "image",
                "text",
                "document"
            ]
        );
        assert_eq!(blocks[0]["tool_use_id"], json!("call_1"));
        assert_eq!(blocks[1]["tool_use_id"], json!("call_2"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_media_entries_follow_the_tool_result_in_one_user_message() {
        let blobs = InMemoryBlobStore::new();
        let entries = tool_result_with_media(&blobs).await;
        let messages = materialize_messages(&blobs, &entries)
            .await
            .expect("materialize");
        assert_eq!(messages.len(), 1, "one user message: {messages:?}");
        let value = serde_json::to_value(&messages[0]).expect("json");
        assert_eq!(value["role"], json!("user"));
        let blocks = value["content"].as_array().expect("blocks");
        let kinds = blocks
            .iter()
            .map(|block| block["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec!["tool_result", "text", "image", "text", "document"],
            "tool_result first, then announcement + block per asset"
        );
        assert_eq!(blocks[0]["tool_use_id"], json!("call_1"));
        assert!(
            blocks[1]["text"]
                .as_str()
                .expect("announcement")
                .starts_with("[image · media:")
        );
        assert!(
            blocks[3]["text"]
                .as_str()
                .expect("announcement")
                .starts_with("[document: report.pdf · media:")
        );
        assert_eq!(blocks[4]["title"], json!("report.pdf"));
    }
}
