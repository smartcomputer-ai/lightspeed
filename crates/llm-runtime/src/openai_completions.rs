//! OpenAI Chat Completions adapter.
//!
//! Chat Completions has a message-oriented wire format distinct from the
//! Responses API. This adapter lowers engine context directly into native
//! messages and preserves native tool-call objects for exact replay.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use engine::{
    CompactionPolicy, ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus,
    ContextCompactionTask, ContextEntry, ContextEntryInput, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, LlmRequest, LlmUsage, OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND,
    ObservedToolCall, ProviderApiKind, RemoteMcpExecution, RemoteMcpExposure, TokenEstimate,
    TokenEstimateQuality, ToolCallId, ToolChoice, ToolKind, ToolName, storage::BlobStore,
};
use llm_clients::{ApiResponse, openai::completions as oai_c};
use serde_json::{Value, json};

use crate::{
    blob_io::{put_json, put_text, read_json, read_text},
    error::{LlmAdapterError, LlmAdapterResult},
    executor::{LlmCompactionAdapter, LlmGenerationAdapter},
    mcp::{
        MAX_NATIVE_MCP_TOOLS_PER_REQUEST, McpInventoryResolver, UnconfiguredMcpInventoryResolver,
    },
    params::{openai_completions_params, validate_openai_reasoning_effort},
    provider_keys::{ModelProviderResolver, NoStoredModelProviders, resolve_model_provider},
    result::{
        LlmGenerationExecution, debug_dump_request, partial_output_entries, store_debug_dumps,
        truncation_failure_text,
    },
};

pub use llm_clients::content::{
    OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND, OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND,
};
pub const OPENAI_COMPLETIONS_TOOL_CALL_PROVIDER_KIND: &str = "openai.completions.tool_call";

const MEDIA_TYPE_JSON: &str = "application/json";
const MEDIA_TYPE_TEXT: &str = "text/plain";
const DEFAULT_COMPACTION_MAX_TOKENS: u64 = 2048;
const COMPACTION_INSTRUCTION: &str = "Summarize the conversation above for context compaction. \
Capture the user's goals, decisions made, work completed, important tool results, and open \
questions. The summary will replace the prior conversation history, so include everything needed \
to continue seamlessly. Reply with the summary only.";

/// Wire-level differences within the nominally OpenAI-compatible Chat
/// Completions family. This deliberately derives from the selected provider,
/// not its endpoint, so transport configuration remains outside durable model
/// and request state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionDialect {
    OpenAi,
    DeepSeek,
    OpenRouter,
    Compatible,
}

impl CompletionDialect {
    fn for_provider(provider_id: &str) -> Self {
        match provider_id.to_ascii_lowercase().as_str() {
            "openai" => Self::OpenAi,
            "deepseek" => Self::DeepSeek,
            "openrouter" => Self::OpenRouter,
            _ => Self::Compatible,
        }
    }

    fn instruction_role(self) -> &'static str {
        match self {
            Self::OpenAi | Self::OpenRouter => "developer",
            Self::DeepSeek | Self::Compatible => "system",
        }
    }

    fn uses_max_completion_tokens(self) -> bool {
        matches!(self, Self::OpenAi | Self::OpenRouter)
    }
}

#[async_trait]
pub trait OpenAiCompletionsApi: Send + Sync {
    async fn create(
        &self,
        request: oai_c::CreateCompletionRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai_c::Completion>, llm_clients::LlmApiError>;
}

#[async_trait]
impl OpenAiCompletionsApi for oai_c::Client {
    async fn create(
        &self,
        request: oai_c::CreateCompletionRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai_c::Completion>, llm_clients::LlmApiError> {
        oai_c::Client::create_with_transport(self, request, auth, endpoint).await
    }
}

#[derive(Clone)]
pub struct OpenAiCompletionsLlmAdapter {
    client: Arc<dyn OpenAiCompletionsApi>,
    blobs: Arc<dyn BlobStore>,
    /// Store the raw provider request and response of every generation as
    /// unrooted debug blobs. Off by default: each request carries the whole
    /// context, so the dumps grow quadratically with turn count.
    debug_dumps: bool,
    provider_keys: Arc<dyn ModelProviderResolver>,
    inventory: Arc<dyn McpInventoryResolver>,
}

impl OpenAiCompletionsLlmAdapter {
    pub fn new(client: Arc<dyn OpenAiCompletionsApi>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            client,
            blobs,
            debug_dumps: false,
            provider_keys: Arc::new(NoStoredModelProviders),
            inventory: Arc::new(UnconfiguredMcpInventoryResolver),
        }
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
    ) -> LlmAdapterResult<oai_c::CreateCompletionRequest> {
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
    ) -> LlmAdapterResult<oai_c::CreateCompletionRequest> {
        materialize_compact_request(self.blobs.as_ref(), task).await
    }
}

#[async_trait]
impl LlmGenerationAdapter for OpenAiCompletionsLlmAdapter {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> LlmAdapterResult<LlmGenerationExecution> {
        if request.request.model.api_kind != ProviderApiKind::OpenAiCompletions {
            return Err(LlmAdapterError::RequestKindMismatch {
                message: format!(
                    "expected OpenAiCompletions request, got {:?}",
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
        let mut provider_request = materialize_request_with_catalog(
            self.blobs.as_ref(),
            self.inventory.as_ref(),
            &request.request,
            &mut catalog,
        )
        .await?;
        // Route every turn of a session to the same prompt cache.
        provider_request.prompt_cache_key =
            Some(crate::prompt_cache::prompt_cache_key(&request.session_id));
        let provider =
            resolve_model_provider(self.provider_keys.as_ref(), &request.request.model).await?;
        let request_dump = debug_dump_request(self.debug_dumps, &provider_request)?;
        let response = self
            .client
            .create(
                provider_request,
                provider.as_ref().map(|provider| provider.as_request_auth()),
                provider
                    .as_ref()
                    .and_then(|provider| provider.endpoint.as_ref())
                    .map(|endpoint| &endpoint.transport),
            )
            .await?;
        reject_failure_finish(&response)?;
        let mut result = result_from_response(self.blobs.as_ref(), &request, &response).await?;
        catalog.normalize(&mut result);
        let debug_dumps = store_debug_dumps(
            self.blobs.as_ref(),
            request_dump,
            &response.raw_json,
            &request,
            "openai_completions",
        )
        .await?;
        Ok(LlmGenerationExecution {
            result,
            debug_dumps,
        })
    }
}

#[async_trait]
impl LlmCompactionAdapter for OpenAiCompletionsLlmAdapter {
    async fn compact_context(
        &self,
        request: ContextCompactionRequest,
    ) -> LlmAdapterResult<ContextCompactionResult> {
        if request.request.model.api_kind != ProviderApiKind::OpenAiCompletions {
            return Err(LlmAdapterError::RequestKindMismatch {
                message: format!(
                    "expected OpenAiCompletions compaction task, got {:?}",
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
                provider
                    .as_ref()
                    .and_then(|provider| provider.endpoint.as_ref())
                    .map(|endpoint| &endpoint.transport),
            )
            .await?;
        reject_failure_finish(&response)?;
        result_from_compact_response(self.blobs.as_ref(), &request, &response).await
    }
}

pub async fn materialize_create_request(
    blobs: &dyn BlobStore,
    request: &LlmRequest,
) -> LlmAdapterResult<oai_c::CreateCompletionRequest> {
    materialize_create_request_with_inventory(blobs, &UnconfiguredMcpInventoryResolver, request)
        .await
}

async fn materialize_create_request_with_inventory(
    blobs: &dyn BlobStore,
    inventory: &dyn McpInventoryResolver,
    request: &LlmRequest,
) -> LlmAdapterResult<oai_c::CreateCompletionRequest> {
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
) -> LlmAdapterResult<oai_c::CreateCompletionRequest> {
    if request.provider_response_id.is_some() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "Chat Completions has no provider response continuation; provider_response_id must be empty"
                .to_owned(),
        });
    }
    if matches!(
        request.compaction,
        Some(CompactionPolicy::ProviderTriggered { .. })
    ) {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "Chat Completions does not support provider-triggered compaction".to_owned(),
        });
    }

    let dialect = CompletionDialect::for_provider(&request.model.provider_id);
    let params = openai_completions_params(request.params.as_ref())?;
    validate_dialect_capabilities(request, &params, dialect)?;
    let mut extra = params.extra;
    let service_tier = crate::params::take_openai_service_tier(&mut extra, params.service_tier)?
        .or(crate::params::openai_processing_service_tier(
            &request.model.provider_id,
            request.processing_tier,
        )?);
    let reasoning_effort = materialize_reasoning_effort(
        request.reasoning_effort.as_deref(),
        &request.model.model,
        dialect,
        &mut extra,
    )?;
    let parallel_tool_calls = params.parallel_tool_calls.or(request.parallel_tool_use);

    let tools = materialize_tools(inventory, catalog).await?;
    let (max_tokens, max_completion_tokens) = if dialect.uses_max_completion_tokens() {
        (None, request.output_limit.map(u64::from))
    } else {
        (request.output_limit.map(u64::from), None)
    };

    Ok(oai_c::CreateCompletionRequest {
        model: request.model.model.clone(),
        messages: materialize_messages(blobs, &request.context.entries, dialect).await?,
        tools,
        tool_choice: materialize_tool_choice(
            catalog.tool_choice(request.tool_choice.as_ref())?.as_ref(),
            dialect,
            &extra,
        )?,
        response_format: params.response_format,
        temperature: optional_f64(params.temperature.as_ref(), "temperature")?,
        top_p: optional_f64(params.top_p.as_ref(), "top_p")?,
        max_tokens,
        max_completion_tokens,
        stop: params.stop,
        parallel_tool_calls,
        store: params.store,
        stream: Some(false),
        service_tier,
        stream_options: None,
        metadata: non_empty_map(params.metadata),
        reasoning_effort,
        extra,
        prompt_cache_key: None,
    })
}

pub async fn materialize_compact_request(
    blobs: &dyn BlobStore,
    task: &ContextCompactionTask,
) -> LlmAdapterResult<oai_c::CreateCompletionRequest> {
    let dialect = CompletionDialect::for_provider(&task.model.provider_id);
    let mut messages = materialize_messages(blobs, &task.context.entries, dialect).await?;
    messages.push(oai_c::CompletionMessage::user(compaction_instruction(
        task.target_tokens,
    )));
    let output_limit = Some(
        task.target_tokens
            .map(u64::from)
            .unwrap_or(DEFAULT_COMPACTION_MAX_TOKENS),
    );
    let (max_tokens, max_completion_tokens) = if dialect.uses_max_completion_tokens() {
        (None, output_limit)
    } else {
        (output_limit, None)
    };
    let mut extra = BTreeMap::new();
    if dialect == CompletionDialect::DeepSeek {
        extra.insert("thinking".to_owned(), json!({"type":"disabled"}));
    }
    Ok(oai_c::CreateCompletionRequest {
        model: task.model.model.clone(),
        messages,
        max_tokens,
        max_completion_tokens,
        stream: Some(false),
        extra,
        ..Default::default()
    })
}

fn compaction_instruction(target_tokens: Option<u32>) -> String {
    match target_tokens {
        Some(tokens) => format!("{COMPACTION_INSTRUCTION} Keep the summary under {tokens} tokens."),
        None => COMPACTION_INSTRUCTION.to_owned(),
    }
}

async fn materialize_messages(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
    dialect: CompletionDialect,
) -> LlmAdapterResult<Vec<oai_c::CompletionMessage>> {
    let mut messages = Vec::new();
    let mut last_assistant_source: Option<ContextEntrySource> = None;

    for entry in entries {
        match &entry.kind {
            ContextEntryKind::ToolCall { .. } => {
                require_completions_provider_kind(entry)?;
                let raw = read_json(blobs, &entry.content.content_ref).await?;
                let tool_call: oai_c::CompletionToolCall =
                    serde_json::from_value(raw).map_err(|error| {
                        LlmAdapterError::InvalidProviderRequest {
                            message: format!(
                                "Chat Completions tool-call entry {} is invalid: {error}",
                                entry.entry_id
                            ),
                        }
                    })?;
                let can_fold = messages
                    .last()
                    .is_some_and(|message: &oai_c::CompletionMessage| message.role == "assistant")
                    && last_assistant_source.as_ref() == Some(&entry.source);
                if can_fold {
                    messages
                        .last_mut()
                        .expect("assistant present")
                        .tool_calls
                        .get_or_insert_with(Vec::new)
                        .push(tool_call);
                } else {
                    let mut message = assistant_shell(dialect);
                    message.tool_calls = Some(vec![tool_call]);
                    messages.push(message);
                }
                last_assistant_source = Some(entry.source.clone());
            }
            ContextEntryKind::ReasoningState
                if entry.content.provider_kind.as_deref()
                    == Some(OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND) =>
            {
                require_completions_provider_kind(entry)?;
                if entry.content.media_type.as_deref() != Some(MEDIA_TYPE_JSON) {
                    return Err(LlmAdapterError::InvalidProviderRequest {
                        message: format!(
                            "Chat Completions native entry {} must contain JSON",
                            entry.entry_id
                        ),
                    });
                }
                let raw = read_json(blobs, &entry.content.content_ref).await?;
                let Some(state) = raw.as_object() else {
                    return Err(LlmAdapterError::InvalidProviderRequest {
                        message: format!(
                            "Chat Completions reasoning entry {} must be a JSON object",
                            entry.entry_id
                        ),
                    });
                };
                let can_fold = messages
                    .last()
                    .is_some_and(|message: &oai_c::CompletionMessage| message.role == "assistant")
                    && last_assistant_source.as_ref() == Some(&entry.source);
                if can_fold {
                    messages
                        .last_mut()
                        .expect("assistant present")
                        .extra
                        .extend(state.clone());
                } else {
                    let mut message = assistant_shell(dialect);
                    message.extra.extend(state.clone());
                    messages.push(message);
                }
                last_assistant_source = Some(entry.source.clone());
            }
            ContextEntryKind::ReasoningState | ContextEntryKind::ProviderOpaque => {
                require_completions_provider_kind(entry)?;
                if entry.content.media_type.as_deref() != Some(MEDIA_TYPE_JSON) {
                    return Err(LlmAdapterError::InvalidProviderRequest {
                        message: format!(
                            "Chat Completions native entry {} must contain JSON",
                            entry.entry_id
                        ),
                    });
                }
                let raw = read_json(blobs, &entry.content.content_ref).await?;
                let message: oai_c::CompletionMessage =
                    serde_json::from_value(raw).map_err(|error| {
                        LlmAdapterError::InvalidProviderRequest {
                            message: format!(
                                "Chat Completions native entry {} is not a message: {error}",
                                entry.entry_id
                            ),
                        }
                    })?;
                last_assistant_source = (message.role == "assistant").then(|| entry.source.clone());
                messages.push(message);
            }
            _ => {
                reject_foreign_provider_kind(entry)?;
                let message = materialize_message(blobs, entry, dialect).await?;
                let assistant = message.role == "assistant";
                push_message(&mut messages, message);
                last_assistant_source = assistant.then(|| entry.source.clone());
            }
        }
    }
    Ok(messages)
}

fn reject_foreign_provider_kind(entry: &ContextEntry) -> LlmAdapterResult<()> {
    let Some(kind) = entry.content.provider_kind.as_deref() else {
        return Ok(());
    };
    if (kind.starts_with("openai.") || kind.starts_with("anthropic."))
        && !kind.starts_with("openai.completions.")
    {
        return Err(LlmAdapterError::RequestKindMismatch {
            message: format!(
                "context entry {} has provider kind {kind:?}, expected openai.completions.*",
                entry.entry_id
            ),
        });
    }
    Ok(())
}

fn require_completions_provider_kind(entry: &ContextEntry) -> LlmAdapterResult<()> {
    if entry
        .content
        .provider_kind
        .as_deref()
        .is_some_and(|kind| kind.starts_with("openai.completions."))
    {
        Ok(())
    } else {
        Err(LlmAdapterError::RequestKindMismatch {
            message: format!(
                "context entry {} has provider kind {:?}, expected openai.completions.*",
                entry.entry_id, entry.content.provider_kind
            ),
        })
    }
}

fn push_message(
    messages: &mut Vec<oai_c::CompletionMessage>,
    mut message: oai_c::CompletionMessage,
) {
    if message.role == "user"
        && let Some(previous) = messages.last_mut()
        && previous.role == "user"
    {
        let mut parts = take_parts(previous.content.take());
        parts.extend(take_parts(message.content.take()));
        previous.content = Some(oai_c::CompletionMessageContent::Parts(parts));
        return;
    }
    messages.push(message);
}

fn take_parts(content: Option<oai_c::CompletionMessageContent>) -> Vec<oai_c::CompletionContent> {
    match content {
        Some(oai_c::CompletionMessageContent::Text(text)) => vec![text_part(text)],
        Some(oai_c::CompletionMessageContent::Parts(parts)) => parts,
        None => Vec::new(),
    }
}

async fn materialize_message(
    blobs: &dyn BlobStore,
    entry: &ContextEntry,
    dialect: CompletionDialect,
) -> LlmAdapterResult<oai_c::CompletionMessage> {
    match &entry.kind {
        ContextEntryKind::Message { role } => {
            let role = match role {
                ContextMessageRole::User => "user",
                ContextMessageRole::Assistant => "assistant",
            };
            if role == "assistant"
                && entry.content.provider_kind.as_deref()
                    == Some(OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND)
                && entry.content.media_type.as_deref() == Some(MEDIA_TYPE_JSON)
            {
                let raw = read_json(blobs, &entry.content.content_ref).await?;
                let native: oai_c::CompletionMessage =
                    serde_json::from_value(raw).map_err(|error| {
                        LlmAdapterError::InvalidProviderRequest {
                            message: format!("invalid Chat Completions assistant content: {error}"),
                        }
                    })?;
                let text = native.text();
                let refusal = text
                    .is_empty()
                    .then(|| llm_clients::content::completion_refusal(&native))
                    .flatten();
                // Response annotations stay in CAS. Replay the established text/refusal
                // shape, with reasoning and calls folded from their semantic entries.
                return Ok(oai_c::CompletionMessage {
                    role: role.to_owned(),
                    content: Some(oai_c::CompletionMessageContent::Text(if text.is_empty() {
                        refusal.clone().unwrap_or_default()
                    } else {
                        text
                    })),
                    refusal,
                    ..Default::default()
                });
            }
            // Tool-produced media never fails a request: a text-only dialect
            // drops it with a note, while run-input media keeps its rejection
            // in `validate_dialect_capabilities`.
            let drop_media =
                dialect == CompletionDialect::DeepSeek && crate::blob_io::is_tool_sourced(entry);
            let content = if let Some(mime) =
                crate::blob_io::image_media_type(entry.content.media_type.as_deref())
            {
                if drop_media {
                    oai_c::CompletionMessageContent::Text(crate::blob_io::text_only_omission(entry))
                } else {
                    let data =
                        crate::blob_io::read_base64(blobs, &entry.content.content_ref).await?;
                    oai_c::CompletionMessageContent::Parts(vec![
                        text_part(crate::blob_io::media_announcement(entry)),
                        part_with_extra(
                            "image_url",
                            "image_url",
                            json!({ "url": format!("data:{mime};base64,{data}") }),
                        ),
                    ])
                }
            } else if let Some(document) = crate::blob_io::document_entry(
                entry.content.media_type.as_deref(),
                entry.preview.as_deref(),
            ) {
                if document.is_pdf && drop_media {
                    oai_c::CompletionMessageContent::Text(crate::blob_io::text_only_omission(entry))
                } else if document.is_pdf {
                    let data =
                        crate::blob_io::read_base64(blobs, &entry.content.content_ref).await?;
                    oai_c::CompletionMessageContent::Parts(vec![
                        text_part(crate::blob_io::media_announcement(entry)),
                        part_with_extra(
                            "file",
                            "file",
                            json!({
                                "filename": document.name.unwrap_or_else(|| "document.pdf".to_owned()),
                                "file_data": format!("data:{};base64,{data}", document.mime),
                            }),
                        ),
                    ])
                } else {
                    let text = read_text(blobs, &entry.content.content_ref).await?;
                    let header = document
                        .name
                        .map(|name| format!("[document: {name}]"))
                        .unwrap_or_else(|| "[document]".to_owned());
                    oai_c::CompletionMessageContent::Text(format!("{header}\n\n{text}"))
                }
            } else {
                oai_c::CompletionMessageContent::Text(
                    crate::blob_io::read_message_text(blobs, &entry.content).await?,
                )
            };
            Ok(oai_c::CompletionMessage {
                role: role.to_owned(),
                content: Some(content),
                ..Default::default()
            })
        }
        ContextEntryKind::Instructions => Ok(text_message(
            dialect.instruction_role(),
            read_text(blobs, &entry.content.content_ref).await?,
        )),
        ContextEntryKind::Catalog { .. } => Ok(text_message(
            dialect.instruction_role(),
            crate::catalog_prompts::stored_catalog_text(blobs, entry, &entry.content.content_ref)
                .await?,
        )),
        ContextEntryKind::ToolResult { call_id, .. } => Ok(oai_c::CompletionMessage {
            role: "tool".to_owned(),
            content: Some(oai_c::CompletionMessageContent::Text(
                read_text(blobs, &entry.content.content_ref).await?,
            )),
            tool_call_id: Some(call_id.as_str().to_owned()),
            ..Default::default()
        }),
        ContextEntryKind::ToolCall { .. }
        | ContextEntryKind::ReasoningState
        | ContextEntryKind::ProviderOpaque
        | ContextEntryKind::McpApprovalResponse { .. } => {
            unreachable!("handled by materialize_messages")
        }
    }
}

fn text_message(role: &str, text: String) -> oai_c::CompletionMessage {
    oai_c::CompletionMessage {
        role: role.to_owned(),
        content: Some(oai_c::CompletionMessageContent::Text(text)),
        ..Default::default()
    }
}

fn assistant_shell(dialect: CompletionDialect) -> oai_c::CompletionMessage {
    if dialect == CompletionDialect::DeepSeek {
        oai_c::CompletionMessage {
            role: "assistant".to_owned(),
            content: Some(oai_c::CompletionMessageContent::Text(String::new())),
            ..Default::default()
        }
    } else {
        oai_c::CompletionMessage {
            role: "assistant".to_owned(),
            extra: BTreeMap::from([("content".to_owned(), Value::Null)]),
            ..Default::default()
        }
    }
}

fn text_part(text: String) -> oai_c::CompletionContent {
    oai_c::CompletionContent {
        r#type: "text".to_owned(),
        text: Some(text),
        ..Default::default()
    }
}

fn part_with_extra(kind: &str, key: &str, value: Value) -> oai_c::CompletionContent {
    let mut extra = BTreeMap::new();
    extra.insert(key.to_owned(), value);
    oai_c::CompletionContent {
        r#type: kind.to_owned(),
        extra,
        ..Default::default()
    }
}

async fn materialize_tools(
    inventory: &dyn McpInventoryResolver,
    catalog: &mut crate::tool_catalog::ToolCatalog,
) -> LlmAdapterResult<Option<Vec<oai_c::CompletionTool>>> {
    let mut materialized = Vec::new();
    let mut native_mcp_tool_count = 0usize;
    for tool in &catalog.tools {
        match &tool.kind {
            crate::tool_catalog::ResolvedToolKind::Function(function) => {
                let mut definition = oai_c::CompletionFunction {
                    name: tool.name.as_str().to_owned(),
                    description: function.description.clone(),
                    parameters: Some(function.input_schema.clone()),
                    strict: function.strict,
                    extra: Default::default(),
                };
                if let Some(options) = &function.provider_options {
                    let Some(options) = options.as_object() else {
                        return Err(LlmAdapterError::InvalidProviderRequest {
                            message: format!(
                                "provider options for tool {} must be a JSON object",
                                tool.name
                            ),
                        });
                    };
                    definition.extra.extend(options.clone());
                }
                materialized.push(oai_c::CompletionTool {
                    r#type: oai_c::CompletionToolType::Function,
                    function: definition,
                });
            }
            crate::tool_catalog::ResolvedToolKind::RemoteMcp(spec)
                if spec.execution == RemoteMcpExecution::Native
                    && spec.exposure == RemoteMcpExposure::Search => {}
            crate::tool_catalog::ResolvedToolKind::RemoteMcp(spec)
                if spec.execution == RemoteMcpExecution::Native
                    && spec.exposure == RemoteMcpExposure::Inject =>
            {
                let mut native = inventory.list_tools(spec).await.map_err(|error| {
                    LlmAdapterError::McpInventory {
                        server: spec.server_id.clone(),
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
                        server_id = %spec.server_id,
                        omitted_tool_count = omitted_count,
                        "omitted native MCP tools with provider-incompatible names"
                    );
                }
                if native_mcp_tool_count.saturating_add(native.len())
                    > MAX_NATIVE_MCP_TOOLS_PER_REQUEST
                {
                    return Err(LlmAdapterError::McpInventory {
                        server: spec.server_id.clone(),
                        message: "native MCP inventory exceeds the per-request tool cap; author a Selected allowlist or switch the record to search exposure".to_owned(),
                    });
                }
                native_mcp_tool_count += native.len();
                for native_tool in native {
                    let name = format!("{}__{}", tool.name, native_tool.remote_name);
                    catalog
                        .names
                        .insert(ToolName::new(name.clone()), Some(tool.id.clone()))?;
                    materialized.push(oai_c::CompletionTool {
                        r#type: oai_c::CompletionToolType::Function,
                        function: oai_c::CompletionFunction {
                            name,
                            description: native_tool.description,
                            parameters: Some(native_tool.input_schema),
                            // MCP accepts general JSON Schema. OpenAI strict
                            // functions accept only a narrower subset.
                            strict: Some(false),
                            extra: Default::default(),
                        },
                    });
                }
            }
            crate::tool_catalog::ResolvedToolKind::ProviderNative(_)
            | crate::tool_catalog::ResolvedToolKind::RemoteMcp(_) => {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: format!(
                        "tool {} is not expressible by openai:completions",
                        tool.name
                    ),
                });
            }
        }
    }
    Ok((!materialized.is_empty()).then_some(materialized))
}

fn validate_dialect_capabilities(
    request: &LlmRequest,
    params: &crate::params::OpenAiCompletionsParams,
    dialect: CompletionDialect,
) -> LlmAdapterResult<()> {
    if dialect == CompletionDialect::OpenAi
        && params.stop.is_some()
        && is_openai_reasoning_model(&request.model.model)
    {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "OpenAI model {} does not support the stop parameter",
                request.model.model
            ),
        });
    }
    if dialect != CompletionDialect::DeepSeek {
        return Ok(());
    }

    for entry in &request.context.entries {
        // Tool-produced media is dropped with a note at materialization
        // instead; only deliberate run input is rejected here.
        if crate::blob_io::is_tool_sourced(entry) {
            continue;
        }
        let is_image =
            crate::blob_io::image_media_type(entry.content.media_type.as_deref()).is_some();
        let is_pdf = crate::blob_io::document_entry(
            entry.content.media_type.as_deref(),
            entry.preview.as_deref(),
        )
        .is_some_and(|document| document.is_pdf);
        if is_image || is_pdf {
            return Err(LlmAdapterError::InvalidProviderRequest {
                message: format!(
                    "DeepSeek Chat Completions accepts text input only; context entry {} is {}",
                    entry.entry_id,
                    entry.content.media_type.as_deref().unwrap_or("binary")
                ),
            });
        }
    }

    if params
        .response_format
        .as_ref()
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "json_object")
    {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "DeepSeek Chat Completions supports response_format type json_object only"
                .to_owned(),
        });
    }
    if params.store.is_some() || !params.metadata.is_empty() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message:
                "DeepSeek Chat Completions does not support OpenAI store or metadata parameters"
                    .to_owned(),
        });
    }
    if request.tools.iter().any(
        |tool| matches!(&tool.kind, ToolKind::Function(function) if function.strict == Some(true)),
    ) {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "DeepSeek strict function schemas require its beta API and are not portable through the standard completions dialect"
                .to_owned(),
        });
    }
    Ok(())
}

fn materialize_reasoning_effort(
    effort: Option<&str>,
    model: &str,
    dialect: CompletionDialect,
    extra: &mut BTreeMap<String, Value>,
) -> LlmAdapterResult<Option<String>> {
    let Some(effort) = effort else {
        return Ok(None);
    };
    validate_openai_reasoning_effort(effort)?;
    match dialect {
        CompletionDialect::DeepSeek => {
            if effort == "minimal" {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: "DeepSeek reasoning effort does not support minimal".to_owned(),
                });
            }
            let thinking = if effort == "none" {
                "disabled"
            } else {
                "enabled"
            };
            match extra.get("thinking") {
                Some(existing) if existing != &json!({"type":thinking}) => {
                    return Err(LlmAdapterError::InvalidProviderRequest {
                        message: "DeepSeek thinking provider param conflicts with reasoning_effort"
                            .to_owned(),
                    });
                }
                Some(_) => {}
                None => {
                    extra.insert("thinking".to_owned(), json!({"type":thinking}));
                }
            }
            Ok((effort != "none").then(|| effort.to_owned()))
        }
        CompletionDialect::OpenAi
            if model.to_ascii_lowercase().starts_with("gpt-5.5")
                && matches!(effort, "minimal" | "max") =>
        {
            Err(LlmAdapterError::InvalidProviderRequest {
                message: format!(
                    "OpenAI model {model} does not support reasoning effort {effort}; use none, low, medium, high, or xhigh"
                ),
            })
        }
        _ => Ok(Some(effort.to_owned())),
    }
}

fn is_openai_reasoning_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

fn openai_tool_choice(choice: &ToolChoice) -> oai_c::CompletionToolChoice {
    match choice {
        ToolChoice::Auto => {
            oai_c::CompletionToolChoice::Mode(oai_c::CompletionToolChoiceMode::Auto)
        }
        ToolChoice::None => {
            oai_c::CompletionToolChoice::Mode(oai_c::CompletionToolChoiceMode::None)
        }
        ToolChoice::RequiredAny => {
            oai_c::CompletionToolChoice::Mode(oai_c::CompletionToolChoiceMode::Required)
        }
        ToolChoice::Specific { tool_name } => oai_c::CompletionToolChoice::Function {
            r#type: oai_c::CompletionToolType::Function,
            function: oai_c::CompletionToolChoiceFunction {
                name: tool_name.as_str().to_owned(),
            },
        },
    }
}

fn materialize_tool_choice(
    choice: Option<&ToolChoice>,
    dialect: CompletionDialect,
    extra: &BTreeMap<String, Value>,
) -> LlmAdapterResult<Option<oai_c::CompletionToolChoice>> {
    if dialect == CompletionDialect::DeepSeek && deepseek_thinking_enabled(extra) {
        return match choice {
            None | Some(ToolChoice::Auto) => Ok(None),
            Some(_) => Err(LlmAdapterError::InvalidProviderRequest {
                message: "DeepSeek thinking mode does not accept the tool_choice parameter; use auto or disable thinking"
                    .to_owned(),
            }),
        };
    }
    Ok(choice.map(openai_tool_choice))
}

fn deepseek_thinking_enabled(extra: &BTreeMap<String, Value>) -> bool {
    extra
        .get("thinking")
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        != Some("disabled")
}

pub async fn result_from_response(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    response: &ApiResponse<oai_c::Completion>,
) -> LlmAdapterResult<LlmGenerationResult> {
    let choice =
        response
            .parsed
            .choices
            .first()
            .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
                message: format!("Chat completion {} has no choices", response.parsed.id),
            })?;
    let message =
        choice
            .message
            .as_ref()
            .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
                message: format!("Chat completion {} has no message", response.parsed.id),
            })?;

    // Reasoning and calls retain their semantic entries. Keep the remaining
    // native message fields together, including unknown response metadata.
    let mut payload = raw_assistant_message(&response.raw_json)
        .cloned()
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: "Chat Completions response has no raw assistant message".to_owned(),
        })?;
    let reasoning_state = ["reasoning_content", "reasoning", "reasoning_details"]
        .into_iter()
        .filter_map(|key| payload.remove(key).map(|value| (key.to_owned(), value)))
        .collect::<serde_json::Map<_, _>>();
    payload.remove("tool_calls");
    let mut context_entries = Vec::new();
    let mut tool_calls = Vec::new();
    let text = message.text();
    let refusal = llm_clients::content::completion_refusal(message);
    let visible = if text.is_empty() {
        refusal.as_deref().unwrap_or("")
    } else {
        &text
    };
    if !visible.is_empty()
        || payload
            .get("annotations")
            .is_some_and(|value| !value.is_null())
    {
        let content_ref = put_json(blobs, &payload).await?;
        context_entries.push(ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                provider_kind: Some(OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND.to_owned()),
            },
            preview: Some(visible.chars().take(256).collect()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        });
    }

    if !reasoning_state.is_empty() {
        let reasoning_state = Value::Object(reasoning_state);
        let preview = llm_clients::content::openai_completion_reasoning(&reasoning_state)
            .filter(|text| !text.is_empty())
            .map(|text| text.chars().take(256).collect());
        let content_ref = put_json(blobs, &reasoning_state).await?;
        context_entries.push(ContextEntryInput {
            kind: ContextEntryKind::ReasoningState,
            content: engine::ContentRef {
                content_ref,
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                provider_kind: Some(OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND.to_owned()),
            },
            preview,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        });
    }

    for (index, call) in message.tool_calls.iter().flatten().enumerate() {
        let raw_call = raw_tool_call(&response.raw_json, index, call)?;
        let (entry, observed) = tool_call_context(blobs, call, raw_call).await?;
        context_entries.push(entry);
        tool_calls.push(observed);
    }

    let usage = response.parsed.usage.as_ref().map(llm_usage);
    let context_token_estimate = response
        .parsed
        .usage
        .as_ref()
        .and_then(|usage| usage.prompt_tokens)
        .map(|tokens| TokenEstimate {
            tokens: u64_to_u32(tokens),
            quality: TokenEstimateQuality::ProviderCounted,
        });
    let finish = finish_reason(choice.finish_reason.as_deref(), !tool_calls.is_empty());
    let (status, failure_ref, context_entries, tool_calls) = if finish == LlmFinish::ContentFilter {
        // Same treatment as a provider refusal: the turn fails with the
        // reason (and the model's refusal text when it sent one), partial
        // content is dropped, and nothing falls back to another model.
        let detail = refusal
            .as_deref()
            .map(|refusal| format!(": {refusal}"))
            .unwrap_or_default();
        let failure_ref = put_text(
            blobs,
            format!(
                "core agent LLM generation failed\nrun_id={}\nturn_id={}\n\
                 error=Chat completion {} stopped for content_filter{detail}\n",
                request.run_id, request.turn_id, response.parsed.id
            ),
        )
        .await?;
        (
            LlmGenerationStatus::Failed,
            Some(failure_ref),
            Vec::new(),
            Vec::new(),
        )
    } else if finish == LlmFinish::Length {
        // Cut off at the output cap: fail the turn but keep the partial
        // text; tool calls from an unfinished turn have no outputs to replay
        // against and are dropped with the reasoning state.
        let failure_ref = put_text(
            blobs,
            truncation_failure_text(
                request.run_id,
                request.turn_id,
                "Chat completion",
                &response.parsed.id,
                request.request.output_limit.map(u64::from),
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

pub async fn result_from_compact_response(
    blobs: &dyn BlobStore,
    request: &ContextCompactionRequest,
    response: &ApiResponse<oai_c::Completion>,
) -> LlmAdapterResult<ContextCompactionResult> {
    let summary = response.parsed.output_text();
    let summary = summary.trim();
    if summary.is_empty() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "Chat Completions compaction response {} did not include summary text",
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
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: Some(MEDIA_TYPE_TEXT.to_owned()),
                provider_kind: Some(OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND.to_owned()),
            },
            preview: Some(summary.chars().take(256).collect()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }],
    })
}

fn raw_tool_call(
    raw_response: &Value,
    index: usize,
    call: &oai_c::CompletionToolCall,
) -> LlmAdapterResult<Value> {
    if let Some(raw) = raw_response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .and_then(|calls| calls.get(index))
    {
        return Ok(raw.clone());
    }
    serde_json::to_value(call).map_err(|error| LlmAdapterError::InvalidProviderRequest {
        message: format!("failed to encode Chat Completions tool call: {error}"),
    })
}

fn raw_assistant_message(raw_response: &Value) -> Option<&serde_json::Map<String, Value>> {
    raw_response
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .as_object()
}

/// Some compatible APIs encode provider failures as a successful HTTP
/// response with a failure finish reason. Turn those into typed provider
/// errors so the normal bounded retry path handles them instead of committing
/// a successful generation with `Unknown` finish.
fn reject_failure_finish(response: &ApiResponse<oai_c::Completion>) -> LlmAdapterResult<()> {
    let Some(choice) = response.parsed.choices.first() else {
        return Ok(());
    };
    let Some(reason @ ("insufficient_system_resource" | "error")) = choice.finish_reason.as_deref()
    else {
        return Ok(());
    };
    let details = choice
        .extra
        .get("error")
        .or_else(|| response.parsed.extra.get("error"));
    let message = details
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("provider returned completion finish_reason {reason}"));
    let error = llm_clients::ProviderHttpError {
        api_kind: oai_c::API_KIND.to_owned(),
        status: 503,
        kind: llm_clients::ProviderFailureKind::Server,
        message,
        error_code: Some(reason.to_owned()),
        error_type: Some("completion_finish_reason".to_owned()),
        retryable: true,
        retry_after: response.headers.retry_after(),
        raw_json: Some(response.raw_json.clone()),
        raw_text: None,
        headers: response.headers.clone(),
    };
    Err(llm_clients::LlmApiError::from(error).into())
}

async fn tool_call_context(
    blobs: &dyn BlobStore,
    call: &oai_c::CompletionToolCall,
    raw_call: Value,
) -> LlmAdapterResult<(ContextEntryInput, ObservedToolCall)> {
    let id = call
        .id
        .as_deref()
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: "Chat Completions tool call is missing id".to_owned(),
        })?;
    let call_id = ToolCallId::try_new(id.to_owned()).map_err(|error| {
        LlmAdapterError::InvalidProviderRequest {
            message: format!("invalid Chat Completions tool call id {id:?}: {error}"),
        }
    })?;
    let function =
        call.function
            .as_ref()
            .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
                message: format!("Chat Completions tool call {id} has no function"),
            })?;
    let name = function
        .name
        .as_deref()
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: format!("Chat Completions tool call {id} has no function name"),
        })?;
    let tool_name = ToolName::try_new(name.to_owned()).map_err(|error| {
        LlmAdapterError::InvalidProviderRequest {
            message: format!("invalid Chat Completions tool name {name:?}: {error}"),
        }
    })?;
    let raw_arguments = function.arguments.as_deref().unwrap_or("{}");
    let arguments =
        serde_json::from_str(raw_arguments).unwrap_or_else(|_| json!({ "__raw": raw_arguments }));
    let arguments_ref = put_json(blobs, &arguments).await?;
    let native_call_ref = put_json(blobs, &raw_call).await?;
    Ok((
        ContextEntryInput {
            kind: ContextEntryKind::ToolCall {
                call_id: call_id.clone(),
                name: tool_name.clone(),
            },
            content: engine::ContentRef {
                content_ref: native_call_ref.clone(),
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
                provider_kind: Some(OPENAI_COMPLETIONS_TOOL_CALL_PROVIDER_KIND.to_owned()),
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        },
        ObservedToolCall {
            call_id,
            tool_id: Some(tool_name.clone()),
            tool_name,
            provider_kind: Some(OPENAI_COMPLETIONS_TOOL_CALL_PROVIDER_KIND.to_owned()),
            arguments_ref,
            native_call_ref: Some(native_call_ref),
        },
    ))
}

fn finish_reason(reason: Option<&str>, has_tool_calls: bool) -> LlmFinish {
    match reason {
        Some("tool_calls" | "function_call") => LlmFinish::ToolCalls,
        Some("stop") => LlmFinish::Stop,
        Some("length") => LlmFinish::Length,
        Some("content_filter") => LlmFinish::ContentFilter,
        Some(_) => LlmFinish::Unknown,
        None if has_tool_calls => LlmFinish::ToolCalls,
        None => LlmFinish::Unknown,
    }
}

fn llm_usage(usage: &oai_c::CompletionUsage) -> LlmUsage {
    LlmUsage {
        input_tokens: usage.prompt_tokens.map(u64_to_u32),
        output_tokens: usage.completion_tokens.map(u64_to_u32),
        reasoning_tokens: usage.reasoning_tokens().map(u64_to_u32),
        total_tokens: usage.total_tokens.map(u64_to_u32),
        cached_input_tokens: usage.cached_tokens().map(u64_to_u32),
        cache_write_input_tokens: None,
        cache_miss_input_tokens: usage.cache_miss_tokens().map(u64_to_u32),
    }
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

fn non_empty_map<K, V>(map: BTreeMap<K, V>) -> Option<BTreeMap<K, V>> {
    (!map.is_empty()).then_some(map)
}

fn u64_to_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use engine::ToolSpec;
    use std::sync::{Arc, Mutex};

    use engine::{
        BlobRef, ContextEntryId, ContextSnapshot, FunctionToolSpec, ModelSelection, ProviderParams,
        RunId, SessionId, ToolParallelism, TurnId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use llm_clients::HeaderSnapshot;
    use serde_json::json;

    use super::*;

    struct StaticMcpInventory;

    #[async_trait]
    impl McpInventoryResolver for StaticMcpInventory {
        async fn list_tools(
            &self,
            _spec: &engine::RemoteMcpToolSpec,
        ) -> Result<Vec<crate::NativeMcpTool>, crate::McpInventoryError> {
            Ok(vec![crate::NativeMcpTool {
                remote_name: "lookup".to_owned(),
                description: Some("Lookup".to_owned()),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }])
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_mcp_is_available_on_openai_compatible_completions() {
        let blobs = InMemoryBlobStore::new();
        let tools = materialize_tools(
            &StaticMcpInventory,
            &mut crate::tool_catalog::ToolCatalog::resolve(
                &blobs,
                &tools::runtime::ToolTarget::api_kind(ProviderApiKind::OpenAiCompletions),
                &[ToolSpec {
                    name: ToolName::try_new("mcp_internal").expect("name"),
                    kind: ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
                        server_id: "internal".to_owned(),
                        record_revision: 2,
                        server_label: "internal".to_owned(),
                        server_url: "https://example.com/mcp".to_owned(),
                        description_ref: None,
                        allowed_tools: None,
                        execution: RemoteMcpExecution::Native,
                        exposure: RemoteMcpExposure::Inject,
                        approval: engine::RemoteMcpApprovalPolicy::Never,
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
        let value = serde_json::to_value(tools).expect("tools json");
        assert_eq!(value[0]["function"]["name"], "mcp_internal__lookup");
        assert_eq!(value[0]["function"]["strict"], false);
    }

    struct FakeOpenAiCompletionsApi {
        response: ApiResponse<oai_c::Completion>,
        seen_auth: Mutex<Vec<Option<String>>>,
        seen_endpoint: Mutex<Vec<bool>>,
    }

    #[async_trait]
    impl OpenAiCompletionsApi for FakeOpenAiCompletionsApi {
        async fn create(
            &self,
            _request: oai_c::CreateCompletionRequest,
            auth: Option<llm_clients::RequestAuth<'_>>,
            endpoint: Option<&llm_clients::EndpointOverride>,
        ) -> Result<ApiResponse<oai_c::Completion>, llm_clients::LlmApiError> {
            self.seen_auth
                .lock()
                .expect("lock")
                .push(auth.map(|auth| match auth {
                    llm_clients::RequestAuth::None => "none".to_owned(),
                    llm_clients::RequestAuth::ApiKey(value) => format!("api_key:{value}"),
                    llm_clients::RequestAuth::Bearer(value) => format!("bearer:{value}"),
                }));
            self.seen_endpoint
                .lock()
                .expect("lock")
                .push(endpoint.is_some());
            Ok(self.response.clone())
        }
    }

    fn fake_api() -> Arc<FakeOpenAiCompletionsApi> {
        let raw = json!({
            "id": "chatcmpl_fake",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "done" }
            }]
        });
        Arc::new(FakeOpenAiCompletionsApi {
            response: ApiResponse {
                parsed: serde_json::from_value(raw.clone()).expect("response"),
                raw_json: raw,
                status: 200,
                headers: HeaderSnapshot::default(),
            },
            seen_auth: Mutex::new(Vec::new()),
            seen_endpoint: Mutex::new(Vec::new()),
        })
    }

    fn model() -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::OpenAiCompletions,
            provider_id: "openai".to_owned(),
            model: "gpt-5.1".to_owned(),
        }
    }

    fn model_for(provider_id: &str, model: &str) -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::OpenAiCompletions,
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
        }
    }

    fn request(entries: Vec<ContextEntry>) -> LlmRequest {
        LlmRequest {
            model: model(),
            request_fingerprint: "sha256:test".to_owned(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiCompletions,
                context_revision: 7,
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

    fn entry(
        id: u64,
        kind: ContextEntryKind,
        source: ContextEntrySource,
        content_ref: BlobRef,
    ) -> ContextEntry {
        ContextEntry {
            entry_id: ContextEntryId::new(id),
            key: None,
            kind,
            source,
            content: engine::ContentRef {
                content_ref,
                media_type: Some(MEDIA_TYPE_TEXT.to_owned()),
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }
    }

    fn generation_request(request: LlmRequest) -> LlmGenerationRequest {
        LlmGenerationRequest {
            session_id: SessionId::try_new("session_test").expect("session id"),
            run_id: RunId::new(2),
            turn_id: TurnId::new(3),
            request,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_passes_stored_api_key_to_client() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api();
        let adapter = OpenAiCompletionsLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(
                crate::provider_keys::StaticProviderKeys::new().with_key("openai", "stored-key"),
            ));

        adapter
            .generate(generation_request(request(Vec::new())))
            .await
            .expect("generate");

        assert_eq!(
            api.seen_auth.lock().expect("lock").as_slice(),
            [Some("api_key:stored-key".to_owned())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_passes_stored_oauth_bearer_to_client() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api();
        let adapter = OpenAiCompletionsLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(
                crate::provider_keys::StaticProviderKeys::new()
                    .with_bearer("openai", "oauth-token"),
            ));

        adapter
            .generate(generation_request(request(Vec::new())))
            .await
            .expect("generate");

        assert_eq!(
            api.seen_auth.lock().expect("lock").as_slice(),
            [Some("bearer:oauth-token".to_owned())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn custom_provider_passes_endpoint_and_explicit_anonymous_auth_to_client() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api();
        let endpoint = crate::provider_keys::ResolvedEndpoint::new(
            "http://127.0.0.1:8080/v1",
            &BTreeMap::new(),
            ["openai:completions".to_owned()],
        )
        .expect("endpoint");
        let resolver = crate::provider_keys::StaticModelProviders::new().with_provider(
            "ollama",
            crate::provider_keys::ResolvedModelProvider {
                auth: None,
                endpoint: Some(endpoint),
            },
        );
        let adapter = OpenAiCompletionsLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(resolver));
        let mut request = request(Vec::new());
        request.model.provider_id = "ollama".to_owned();

        adapter
            .generate(generation_request(request))
            .await
            .expect("generate");

        assert_eq!(
            api.seen_auth.lock().expect("lock").as_slice(),
            [Some("none".to_owned())]
        );
        assert_eq!(api.seen_endpoint.lock().expect("lock").as_slice(), [true]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materializes_developer_user_params_and_neutral_fields() {
        let blobs = InMemoryBlobStore::new();
        let instructions = blobs.insert_text("Be precise.").await;
        let user = blobs.insert_text("Hello").await;
        let mut request = request(vec![
            entry(
                1,
                ContextEntryKind::Instructions,
                ContextEntrySource::ContextEdit,
                instructions,
            ),
            entry(
                2,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                ContextEntrySource::RunInput {
                    run_id: RunId::new(2),
                    input_index: 0,
                },
                user,
            ),
        ]);
        request.output_limit = Some(321);
        request.reasoning_effort = Some("max".to_owned());
        request.parallel_tool_use = Some(false);
        request.processing_tier = Some(engine::ModelProcessingTier::Fast);
        request.tool_choice = Some(ToolChoice::RequiredAny);
        request.params = Some(ProviderParams::new(
            ProviderApiKind::OpenAiCompletions,
            json!({
                "temperature": 0.25,
                "top_p": 0.9,
                "store": true,
                "metadata": { "suite": "unit" },
                "extra": { "extra_wire_field": "kept" }
            }),
        ));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["messages"],
            json!([
                { "role": "developer", "content": "Be precise." },
                { "role": "user", "content": "Hello" }
            ])
        );
        assert_eq!(value["max_completion_tokens"], 321);
        assert_eq!(value["reasoning_effort"], "max");
        assert_eq!(value["parallel_tool_calls"], false);
        assert_eq!(value["tool_choice"], "required");
        assert_eq!(value["temperature"], 0.25);
        assert_eq!(value["service_tier"], "fast");
        assert_eq!(value["extra_wire_field"], "kept");
        assert_eq!(value["stream"], false);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deepseek_uses_system_role_and_legacy_token_budget_for_generation_and_compaction() {
        let blobs = InMemoryBlobStore::new();
        let instructions = blobs.insert_text("Be precise.").await;
        let mut deepseek_request = request(vec![entry(
            1,
            ContextEntryKind::Instructions,
            ContextEntrySource::ContextEdit,
            instructions,
        )]);
        deepseek_request.model = model_for("deepseek", "deepseek-v4-pro");
        deepseek_request.output_limit = Some(321);
        deepseek_request.reasoning_effort = Some("max".to_owned());

        let materialized = materialize_create_request(&blobs, &deepseek_request)
            .await
            .expect("DeepSeek request");
        assert_eq!(materialized.messages[0].role, "system");
        assert_eq!(materialized.max_tokens, Some(321));
        assert_eq!(materialized.max_completion_tokens, None);
        assert_eq!(materialized.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(materialized.extra["thinking"], json!({"type":"enabled"}));

        let task = ContextCompactionTask {
            model: deepseek_request.model,
            request_fingerprint: "sha256:deepseek-compact".to_owned(),
            context: deepseek_request.context,
            target_tokens: Some(128),
            params: None,
        };
        let compact = materialize_compact_request(&blobs, &task)
            .await
            .expect("DeepSeek compact request");
        assert_eq!(compact.messages[0].role, "system");
        assert_eq!(compact.max_tokens, Some(128));
        assert_eq!(compact.max_completion_tokens, None);
        assert_eq!(compact.extra["thinking"], json!({"type":"disabled"}));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deepseek_rejects_nonportable_multimodal_schema_and_strict_features_before_io() {
        let blobs = InMemoryBlobStore::new();
        let image_ref = blobs.put_bytes(vec![1, 2, 3]).await.expect("image");
        let mut image = entry(
            1,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            ContextEntrySource::ContextEdit,
            image_ref,
        );
        image.content.media_type = Some("image/png".to_owned());
        let mut image_request = request(vec![image]);
        image_request.model = model_for("deepseek", "deepseek-v4-flash");
        assert!(matches!(
            materialize_create_request(&blobs, &image_request).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));

        let mut schema_request = request(Vec::new());
        schema_request.model = model_for("deepseek", "deepseek-v4-flash");
        schema_request.params = Some(ProviderParams::new(
            ProviderApiKind::OpenAiCompletions,
            json!({"response_format":{"type":"json_schema","json_schema":{"name":"x"}}}),
        ));
        assert!(matches!(
            materialize_create_request(&blobs, &schema_request).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));

        let schema_ref = put_json(&blobs, &json!({"type":"object"}))
            .await
            .expect("schema");
        let mut strict_request = request(Vec::new());
        strict_request.model = model_for("deepseek", "deepseek-v4-flash");
        strict_request.tools.push(ToolSpec {
            name: ToolName::new("strict_tool"),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: None,
                input_schema_ref: schema_ref,
                output_schema_ref: None,
                strict: Some(true),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::Exclusive,
            execution: Default::default(),
        });
        assert!(matches!(
            materialize_create_request(&blobs, &strict_request).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_stop_for_openai_reasoning_models_before_io() {
        let blobs = InMemoryBlobStore::new();
        let mut stop_request = request(Vec::new());
        stop_request.params = Some(ProviderParams::new(
            ProviderApiKind::OpenAiCompletions,
            json!({"stop":"STOP"}),
        ));

        let error = materialize_create_request(&blobs, &stop_request)
            .await
            .expect_err("gpt-5 stop must be rejected");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deepseek_none_reasoning_disables_thinking_and_gpt_5_5_rejects_unsupported_tiers() {
        let blobs = InMemoryBlobStore::new();
        let mut deepseek = request(Vec::new());
        deepseek.model = model_for("deepseek", "deepseek-v4-pro");
        deepseek.reasoning_effort = Some("none".to_owned());
        let materialized = materialize_create_request(&blobs, &deepseek)
            .await
            .expect("non-thinking DeepSeek request");
        assert_eq!(materialized.reasoning_effort, None);
        assert_eq!(materialized.extra["thinking"], json!({"type":"disabled"}));

        for effort in ["minimal", "max"] {
            let mut openai = request(Vec::new());
            openai.model = model_for("openai", "gpt-5.5");
            openai.reasoning_effort = Some(effort.to_owned());
            assert!(matches!(
                materialize_create_request(&blobs, &openai).await,
                Err(LlmAdapterError::InvalidProviderRequest { .. })
            ));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deepseek_thinking_omits_auto_tool_choice_and_rejects_forced_choices() {
        let blobs = InMemoryBlobStore::new();
        let mut automatic = request(Vec::new());
        automatic.model = model_for("deepseek", "deepseek-v4-pro");
        automatic.tool_choice = Some(ToolChoice::Auto);
        let automatic = materialize_create_request(&blobs, &automatic)
            .await
            .expect("automatic DeepSeek thinking request");
        assert_eq!(automatic.tool_choice, None);

        let mut forced = request(Vec::new());
        forced.model = model_for("deepseek", "deepseek-v4-pro");
        forced.tool_choice = Some(ToolChoice::RequiredAny);
        assert!(matches!(
            materialize_create_request(&blobs, &forced).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));

        let mut non_thinking = forced;
        non_thinking.reasoning_effort = Some("none".to_owned());
        let non_thinking = materialize_create_request(&blobs, &non_thinking)
            .await
            .expect("forced non-thinking DeepSeek request");
        assert!(matches!(
            non_thinking.tool_choice,
            Some(oai_c::CompletionToolChoice::Mode(
                oai_c::CompletionToolChoiceMode::Required
            ))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn folds_images_and_pdfs_into_one_user_message() {
        let blobs = InMemoryBlobStore::new();
        let image_ref = blobs.put_bytes(vec![1, 2, 3]).await.expect("store image");
        let pdf_ref = blobs
            .put_bytes(b"%PDF-test".to_vec())
            .await
            .expect("store PDF");
        let source = ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        };
        let mut image = entry(
            1,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source.clone(),
            image_ref,
        );
        image.content.media_type = Some("image/png".to_owned());
        let mut pdf = entry(
            2,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source,
            pdf_ref,
        );
        pdf.content.media_type = Some("application/pdf".to_owned());
        pdf.preview = Some("[document: brief.pdf]".to_owned());

        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request(vec![image, pdf]))
                .await
                .expect("materialize"),
        )
        .expect("json");

        let messages = value["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let parts = messages[0]["content"].as_array().expect("parts");
        assert_eq!(parts.len(), 4, "announcement + image, announcement + file");
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(
            parts[0]["text"],
            format!(
                "[image · {} · image/png]",
                engine::media::media_handle(&BlobRef::from_bytes(&[1, 2, 3]))
            )
        );
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AQID");
        assert_eq!(parts[2]["type"], "text");
        assert!(
            parts[2]["text"]
                .as_str()
                .expect("announcement")
                .starts_with("[document: brief.pdf · media:")
        );
        assert_eq!(parts[3]["type"], "file");
        assert_eq!(parts[3]["file"]["filename"], "brief.pdf");
        assert!(
            parts[3]["file"]["file_data"]
                .as_str()
                .expect("file data")
                .starts_with("data:application/pdf;base64,")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn folds_assistant_tool_calls_and_materializes_tool_results() {
        let blobs = InMemoryBlobStore::new();
        let assistant_ref = blobs.insert_text("Checking").await;
        let raw_call = json!({
            "id": "call_1",
            "type": "function",
            "function": { "name": "read_file", "arguments": "{\"path\":\"README.md\"}" }
        });
        let call_ref = put_json(&blobs, &raw_call).await.expect("call");
        let result_ref = blobs.insert_text("contents").await;
        let assistant_source = ContextEntrySource::AssistantOutput {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
        };
        let mut call = entry(
            2,
            ContextEntryKind::ToolCall {
                call_id: ToolCallId::try_new("call_1").expect("call id"),
                name: ToolName::try_new("read_file").expect("tool name"),
            },
            assistant_source.clone(),
            call_ref,
        );
        call.content.media_type = Some(MEDIA_TYPE_JSON.to_owned());
        call.content.provider_kind = Some(OPENAI_COMPLETIONS_TOOL_CALL_PROVIDER_KIND.to_owned());
        let entries = vec![
            entry(
                1,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                assistant_source,
                assistant_ref,
            ),
            call,
            entry(
                3,
                ContextEntryKind::ToolResult {
                    call_id: ToolCallId::try_new("call_1").expect("call id"),
                    is_error: false,
                },
                ContextEntrySource::Tool {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                    batch_id: None,
                },
                result_ref,
            ),
        ];

        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request(entries))
                .await
                .expect("materialize"),
        )
        .expect("json");

        assert_eq!(value["messages"].as_array().expect("messages").len(), 2);
        assert_eq!(value["messages"][0]["role"], "assistant");
        assert_eq!(value["messages"][0]["content"], "Checking");
        assert_eq!(value["messages"][0]["tool_calls"][0], raw_call);
        assert_eq!(
            value["messages"][1],
            json!({ "role": "tool", "content": "contents", "tool_call_id": "call_1" })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_foreign_provider_native_context() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = put_json(&blobs, &json!({ "role": "assistant" }))
            .await
            .expect("content");
        let mut native = entry(
            1,
            ContextEntryKind::ProviderOpaque,
            ContextEntrySource::AssistantOutput {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
            },
            content_ref,
        );
        native.content.media_type = Some(MEDIA_TYPE_JSON.to_owned());
        native.content.provider_kind = Some("openai.responses.message".to_owned());

        let error = materialize_create_request(&blobs, &request(vec![native]))
            .await
            .expect_err("foreign native context must fail");

        assert!(matches!(error, LlmAdapterError::RequestKindMismatch { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn assistant_content_projects_and_replays_exact_text_with_bounded_previews() {
        let blobs = InMemoryBlobStore::new();
        let long_text = "héllo 🦀\n".repeat(1000);
        let authored_json =
            r#"{"type":"message","content":[{"type":"output_text","text":"literal JSON"}]}"#;
        for (message, expected, provider_kind) in [
            (
                json!({"content": long_text}),
                long_text,
                OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND,
            ),
            (
                json!({"content": authored_json}),
                authored_json.to_owned(),
                OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND,
            ),
            (
                json!({"content": "\"quoted answer\""}),
                "\"quoted answer\"".to_owned(),
                OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND,
            ),
            (
                json!({"content": [{"type":"text","text":"hello "},{"type":"text","text":"world"}]}),
                "hello world".to_owned(),
                OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND,
            ),
            (
                json!({"content": null, "refusal": "I cannot do that."}),
                "I cannot do that.".to_owned(),
                OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND,
            ),
        ] {
            let raw = json!({
                "id": "chatcmpl_content",
                "choices": [{"index": 0, "finish_reason": "stop", "message": {
                    "role": "assistant", "content": message["content"], "refusal": message["refusal"]
                }}]
            });
            let response = ApiResponse {
                parsed: serde_json::from_value(raw.clone()).expect("response"),
                raw_json: raw,
                status: 200,
                headers: HeaderSnapshot::default(),
            };
            let result =
                result_from_response(&blobs, &generation_request(request(Vec::new())), &response)
                    .await
                    .expect("result");
            assert_eq!(result.status, LlmGenerationStatus::Succeeded);
            assert_eq!(result.context_entries.len(), 1);
            let input = result.context_entries.into_iter().next().unwrap();
            let content = input.content.clone();
            assert_eq!(
                read_json(&blobs, &content.content_ref).await.unwrap(),
                response.raw_json["choices"][0]["message"]
            );
            assert_eq!(content.media_type.as_deref(), Some("application/json"));
            assert_eq!(content.provider_kind.as_deref(), Some(provider_kind));
            assert!(input.preview.as_ref().unwrap().chars().count() <= 256);
            assert_eq!(
                api_projection::project_content_text(&blobs, &content)
                    .await
                    .unwrap(),
                Some(expected.clone())
            );
            let entry = ContextEntry {
                entry_id: ContextEntryId::new(1),
                key: None,
                kind: input.kind,
                source: ContextEntrySource::AssistantOutput {
                    run_id: result.run_id,
                    turn_id: result.turn_id,
                },
                content: input.content,
                preview: input.preview,
                origin: input.origin,
                provenance_ref: input.provenance_ref,
                token_estimate: input.token_estimate,
                supersedes: None,
            };
            let view = api_projection::CoreAgentProjector::new(&blobs)
                .project_context_entry(&entry, None)
                .await
                .unwrap();
            // Message views return full text; the entry preview stays bounded.
            assert!(!view.text_truncated);
            assert_eq!(view.text.as_deref(), Some(expected.as_str()));
            let replay = materialize_create_request(&blobs, &request(vec![entry]))
                .await
                .unwrap();
            assert_eq!(replay.messages.len(), 1);
            assert_eq!(replay.messages[0].text(), expected);
            if message["refusal"].is_string() {
                assert_eq!(
                    replay.messages[0].refusal.as_deref(),
                    Some(expected.as_str())
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maps_refusal_tool_call_usage_and_finish_facts() {
        let blobs = InMemoryBlobStore::new();
        let raw = json!({
            "id": "chatcmpl_1",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": "I cannot do that.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "safe_tool", "arguments": "not-json" }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4,
                "total_tokens": 14,
                "completion_tokens_details": { "reasoning_tokens": 2 }
            }
        });
        let parsed: oai_c::Completion = serde_json::from_value(raw.clone()).expect("completion");
        let response = ApiResponse {
            parsed,
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };

        let result =
            result_from_response(&blobs, &generation_request(request(Vec::new())), &response)
                .await
                .expect("result");

        assert_eq!(result.facts.finish, LlmFinish::ToolCalls);
        assert_eq!(
            result.facts.provider_response_id.as_deref(),
            Some("chatcmpl_1")
        );
        assert_eq!(
            result
                .facts
                .usage
                .as_ref()
                .and_then(|usage| usage.input_tokens),
            Some(10)
        );
        assert_eq!(
            result
                .facts
                .usage
                .as_ref()
                .and_then(|usage| usage.reasoning_tokens),
            Some(2)
        );
        assert_eq!(result.context_entries.len(), 2);
        assert_eq!(
            result.context_entries[0].content.provider_kind.as_deref(),
            Some(OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND)
        );
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
        assert_eq!(result.facts.tool_calls.len(), 1);
        let arguments = read_json(&blobs, &result.facts.tool_calls[0].arguments_ref)
            .await
            .expect("arguments");
        assert_eq!(arguments, json!({ "__raw": "not-json" }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_and_exactly_replays_reasoning_extensions_with_tool_calls() {
        let blobs = InMemoryBlobStore::new();
        let raw = json!({
            "id": "chatcmpl_reasoning",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "DeepSeek exact reasoning",
                    "reasoning": "OpenRouter reasoning",
                    "reasoning_details": [
                        {"type":"reasoning.text","text":"exact","signature":"sig_1"}
                    ],
                    "tool_calls": [{
                        "id":"call_reasoning",
                        "type":"function",
                        "function":{"name":"lookup","arguments":"{\"id\":1}"}
                    }]
                }
            }]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw.clone()).expect("response"),
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let result =
            result_from_response(&blobs, &generation_request(request(Vec::new())), &response)
                .await
                .expect("result");
        assert_eq!(result.context_entries.len(), 2);
        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::ReasoningState
        ));

        let source = ContextEntrySource::AssistantOutput {
            run_id: RunId::new(2),
            turn_id: TurnId::new(3),
        };
        let entries: Vec<ContextEntry> = result
            .context_entries
            .into_iter()
            .enumerate()
            .map(|(index, input)| ContextEntry {
                entry_id: ContextEntryId::new(index as u64 + 1),
                key: None,
                kind: input.kind,
                source: source.clone(),
                content: input.content,
                preview: input.preview,
                origin: input.origin,
                provenance_ref: input.provenance_ref,
                token_estimate: input.token_estimate,
                supersedes: None,
            })
            .collect();
        let replay = serde_json::to_value(
            materialize_create_request(&blobs, &request(entries.clone()))
                .await
                .expect("replay"),
        )
        .expect("json");
        assert_eq!(replay["messages"].as_array().expect("messages").len(), 1);
        let replayed = &replay["messages"][0];
        assert!(replayed["content"].is_null());
        assert_eq!(replayed["reasoning_content"], "DeepSeek exact reasoning");
        assert_eq!(replayed["reasoning"], "OpenRouter reasoning");
        assert_eq!(
            replayed["reasoning_details"],
            json!([{"type":"reasoning.text","text":"exact","signature":"sig_1"}])
        );
        assert_eq!(replayed["tool_calls"][0]["id"], "call_reasoning");

        let deepseek_replay = serde_json::to_value(
            materialize_messages(&blobs, &entries, CompletionDialect::DeepSeek)
                .await
                .expect("DeepSeek replay"),
        )
        .expect("DeepSeek JSON");
        assert_eq!(deepseek_replay[0]["content"], "");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_annotations_and_compatible_cache_accounting_without_replaying_annotations() {
        let blobs = InMemoryBlobStore::new();
        let raw = json!({
            "id": "chatcmpl_web",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "A cited answer",
                    "refusal": "A separate refusal is retained",
                    "provider_metadata": {"trace": "retained"},
                    "annotations": [{"type":"url_citation","url_citation":{"url":"https://example.com"}}]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 3,
                "total_tokens": 13,
                "prompt_cache_hit_tokens": 6,
                "prompt_cache_miss_tokens": 4
            }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw.clone()).expect("response"),
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let result =
            result_from_response(&blobs, &generation_request(request(Vec::new())), &response)
                .await
                .expect("result");
        let usage = result.facts.usage.expect("usage");
        assert_eq!(usage.cached_input_tokens, Some(6));
        assert_eq!(usage.cache_miss_input_tokens, Some(4));
        assert_eq!(result.context_entries.len(), 1);
        let input = &result.context_entries[0];
        assert_eq!(
            read_json(&blobs, &input.content.content_ref).await.unwrap(),
            response.raw_json["choices"][0]["message"]
        );
        let mut entry = entry(
            1,
            input.kind.clone(),
            ContextEntrySource::AssistantOutput {
                run_id: result.run_id,
                turn_id: result.turn_id,
            },
            input.content.content_ref.clone(),
        );
        entry.content = input.content.clone();
        let replay = materialize_messages(
            &blobs,
            std::slice::from_ref(&entry),
            CompletionDialect::OpenAi,
        )
        .await
        .expect("replay");
        assert_eq!(replay[0].text(), "A cited answer");
        assert!(replay[0].annotations().is_none());
        assert!(replay[0].extra.is_empty());
        assert!(replay[0].refusal.is_none());
        let view = api_projection::CoreAgentProjector::new(&blobs)
            .project_event_kind(&engine::CoreAgentEvent::Context(
                engine::ContextEvent::EntriesApplied {
                    base_revision: 0,
                    entries: vec![entry],
                },
            ))
            .await
            .unwrap();
        let view = serde_json::to_value(view).unwrap();
        assert_eq!(view["entries"][0]["citations"].as_array().unwrap().len(), 1);
        assert_eq!(
            view["entries"][0]["citations"][0]["url"],
            "https://example.com"
        );
    }

    /// A `content_filter` finish fails the turn like a provider refusal; the
    /// model's refusal text, when present, rides along in the failure.
    /// A `length` finish fails the turn but keeps the partial text; the
    /// dangling tool call is dropped and the failure names the cap.
    #[tokio::test(flavor = "current_thread")]
    async fn length_finish_fails_the_turn_but_keeps_partial_text() {
        let blobs = InMemoryBlobStore::new();
        let raw = json!({
            "id": "chatcmpl_cut",
            "choices": [{
                "index": 0,
                "finish_reason": "length",
                "message": {
                    "role": "assistant",
                    "content": "The bicycle was",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"Cargo.toml\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 16, "total_tokens": 26 }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw.clone()).expect("response"),
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let mut request = request(Vec::new());
        request.output_limit = Some(16);

        let result = result_from_response(&blobs, &generation_request(request), &response)
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
        assert_eq!(
            result.context_entries[0].preview.as_deref(),
            Some("The bicycle was")
        );
        assert_eq!(
            api_projection::project_content_text(&blobs, &result.context_entries[0].content)
                .await
                .expect("project partial text")
                .as_deref(),
            Some("The bicycle was")
        );
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(
            failure.contains("cut off at max output tokens 16 after 16 output tokens"),
            "{failure}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn content_filter_finish_fails_the_turn_with_the_refusal_text() {
        let blobs = InMemoryBlobStore::new();
        let raw = json!({
            "id": "chatcmpl_filtered",
            "choices": [{
                "index": 0,
                "finish_reason": "content_filter",
                "message": {
                    "role": "assistant",
                    "content": "partial",
                    "refusal": "I cannot do that."
                }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw.clone()).expect("response"),
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };

        let result =
            result_from_response(&blobs, &generation_request(request(Vec::new())), &response)
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
            Some("chatcmpl_filtered")
        );
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(
            failure.contains("stopped for content_filter: I cannot do that."),
            "{failure}"
        );
    }

    #[test]
    fn compatible_failure_finish_reenters_the_retry_path() {
        for reason in ["insufficient_system_resource", "error"] {
            let raw = json!({
                "id": "chatcmpl_failed",
                "choices": [{
                    "index": 0,
                    "finish_reason": reason,
                    "message": {"role":"assistant","content":null},
                    "error": {"message":"upstream unavailable"}
                }]
            });
            let response = ApiResponse {
                parsed: serde_json::from_value(raw.clone()).expect("response"),
                raw_json: raw,
                status: 200,
                headers: HeaderSnapshot::default(),
            };
            let error = reject_failure_finish(&response).expect_err("failure finish");
            let LlmAdapterError::Provider { source } = error else {
                panic!("failure finish must become a provider error");
            };
            assert!(source.retryable());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compaction_uses_target_budget_and_returns_recognized_summary() {
        let blobs = InMemoryBlobStore::new();
        let user_ref = blobs.insert_text("Long conversation").await;
        let task = ContextCompactionTask {
            model: model(),
            request_fingerprint: "sha256:compact".to_owned(),
            context: request(vec![entry(
                1,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                ContextEntrySource::ContextEdit,
                user_ref,
            )])
            .context,
            target_tokens: Some(256),
            params: None,
        };
        let materialized = materialize_compact_request(&blobs, &task)
            .await
            .expect("materialize compaction");
        assert_eq!(materialized.max_completion_tokens, Some(256));
        assert_eq!(
            materialized.messages.last().expect("instruction").role,
            "user"
        );

        let summary = "Retain the project facts. ".repeat(20);
        let raw = json!({
            "id": "chatcmpl_compact",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": summary }
            }]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw.clone()).expect("response"),
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let compact_request = ContextCompactionRequest {
            session_id: SessionId::try_new("session_test").expect("session id"),
            request: task,
        };
        let result = result_from_compact_response(&blobs, &compact_request, &response)
            .await
            .expect("result");

        assert_eq!(result.status, ContextCompactionStatus::Succeeded);
        assert_eq!(
            result.context_entries[0].content.provider_kind.as_deref(),
            Some(OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND)
        );
        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ));
        let entry = &result.context_entries[0];
        assert!(entry.preview.as_ref().unwrap().len() <= 256);
        assert_eq!(
            api_projection::project_content_text(&blobs, &entry.content)
                .await
                .unwrap()
                .as_deref(),
            Some(summary.trim())
        );
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

    #[tokio::test(flavor = "current_thread")]
    async fn tool_media_entries_follow_the_tool_message_as_user_parts() {
        let blobs = InMemoryBlobStore::new();
        let entries = tool_result_with_media(&blobs).await;
        let value = serde_json::to_value(
            materialize_create_request(&blobs, &request(entries))
                .await
                .expect("materialize"),
        )
        .expect("json");
        let messages = value["messages"].as_array().expect("messages");
        assert_eq!(
            messages.len(),
            2,
            "tool message then one user message: {messages:?}"
        );
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_1");
        assert_eq!(messages[1]["role"], "user");
        let parts = messages[1]["content"].as_array().expect("parts");
        let kinds = parts
            .iter()
            .map(|part| part["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["text", "image_url", "text", "file"]);
        assert!(
            parts[0]["text"]
                .as_str()
                .expect("text")
                .starts_with("[image · media:")
        );
        assert_eq!(parts[3]["file"]["filename"], "report.pdf");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deepseek_drops_tool_media_with_a_note_but_still_rejects_run_input_media() {
        let blobs = InMemoryBlobStore::new();
        let mut tool_request = request(tool_result_with_media(&blobs).await);
        tool_request.model = model_for("deepseek", "deepseek-v4-flash");
        let value = serde_json::to_value(
            materialize_create_request(&blobs, &tool_request)
                .await
                .expect("tool media never fails a text-only request"),
        )
        .expect("json");
        let messages = value["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2, "{messages:?}");
        let user = serde_json::to_string(&messages[1]).expect("user");
        assert!(
            user.contains("omitted: this model accepts text only"),
            "{user}"
        );
        assert!(!user.contains("image_url"), "{user}");
        assert!(!user.contains("file_data"), "{user}");
        assert!(user.contains("[image · media:"), "{user}");
        assert!(user.contains("[document: report.pdf · media:"), "{user}");

        let image_ref = blobs.put_bytes(vec![1, 2, 3]).await.expect("image");
        let mut image = entry(
            1,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            image_ref,
        );
        image.content.media_type = Some("image/png".to_owned());
        let mut input_request = request(vec![image]);
        input_request.model = model_for("deepseek", "deepseek-v4-flash");
        assert!(matches!(
            materialize_create_request(&blobs, &input_request).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));
    }
}
