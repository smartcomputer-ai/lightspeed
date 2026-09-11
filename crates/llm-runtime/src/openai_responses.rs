use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    ApprovalContinuation, ApprovalSubject, BlobRef, CompactionPolicy, ContextCompactionRequest,
    ContextCompactionResult, ContextCompactionStatus, ContextCompactionTask, ContextEntry,
    ContextEntryInput, ContextEntryKind, ContextMessageRole, LlmFinish, LlmGenerationFacts,
    LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, LlmRequest, LlmUsage,
    OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND, OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND,
    OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND, OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND,
    OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND, OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND,
    ObservedApprovalRequest, ObservedToolCall, ProviderApiKind, ProviderNativeToolExecution,
    RemoteMcpApprovalPolicy, RemoteMcpExecution, RemoteMcpExposure, RemoteMcpToolSpec,
    TokenEstimate, TokenEstimateQuality, ToolCallId, ToolChoice, ToolKind, ToolName, ToolSpec,
    storage::BlobStore,
};
use llm_clients::{ApiResponse, openai::responses as oai};
use serde_json::{Value, json};

use crate::{
    blob_io::{put_json, put_text, read_json, read_text},
    error::{LlmAdapterError, LlmAdapterResult},
    executor::{LlmCompactionAdapter, LlmGenerationAdapter},
    mcp::{
        MAX_NATIVE_MCP_TOOLS_PER_REQUEST, McpInventoryResolver, UnconfiguredMcpInventoryResolver,
    },
    params::{openai_reasoning_from_effort, openai_responses_params},
    provider_keys::{ModelProviderResolver, NoStoredModelProviders, resolve_model_provider},
    result::{
        LlmGenerationExecution, debug_dump_request, partial_output_entries, store_debug_dumps,
        truncation_failure_text,
    },
    secrets::{
        REDACTED_SECRET_PLACEHOLDER, SecretResolveError, SecretResolver, UnconfiguredSecretResolver,
    },
};

const PROVIDER_KIND_MESSAGE: &str = "openai.responses.message";
/// A model-authored refusal rendered as the assistant message.
const PROVIDER_KIND_REFUSAL: &str = "openai.responses.refusal";
const PROVIDER_KIND_FUNCTION_CALL: &str = "openai.responses.function_call";
const MEDIA_TYPE_JSON: &str = "application/json";
const MEDIA_TYPE_TEXT: &str = "text/plain";

#[async_trait]
pub trait OpenAiResponsesApi: Send + Sync {
    /// `auth` overrides the client's transport-configured key for this
    /// request when stored provider credentials are used.
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError>;

    async fn compact(
        &self,
        request: oai::CompactResponseRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError>;
}

#[async_trait]
impl OpenAiResponsesApi for oai::Client {
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
        oai::Client::create_with_transport(self, request, auth, endpoint).await
    }

    async fn compact(
        &self,
        request: oai::CompactResponseRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError> {
        oai::Client::compact_with_transport(self, request, auth, endpoint).await
    }
}

#[derive(Clone)]
pub struct OpenAiResponsesLlmAdapter {
    client: Arc<dyn OpenAiResponsesApi>,
    blobs: Arc<dyn BlobStore>,
    /// Store the raw provider request and response of every generation as
    /// unrooted debug blobs. Off by default: each request carries the whole
    /// context, so the dumps grow quadratically with turn count.
    debug_dumps: bool,
    secrets: Arc<dyn SecretResolver>,
    provider_keys: Arc<dyn ModelProviderResolver>,
    inventory: Arc<dyn McpInventoryResolver>,
}

impl OpenAiResponsesLlmAdapter {
    pub fn new(client: Arc<dyn OpenAiResponsesApi>, blobs: Arc<dyn BlobStore>) -> Self {
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
    ) -> LlmAdapterResult<oai::CreateResponseRequest> {
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
    ) -> LlmAdapterResult<oai::CompactResponseRequest> {
        materialize_compact_request(self.blobs.as_ref(), task).await
    }
}

#[async_trait]
impl LlmGenerationAdapter for OpenAiResponsesLlmAdapter {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> LlmAdapterResult<LlmGenerationExecution> {
        if request.request.model.api_kind != ProviderApiKind::OpenAiResponses {
            return Err(LlmAdapterError::RequestKindMismatch {
                message: format!(
                    "expected OpenAiResponses request, got {:?}",
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
        let (send_request, redacted_request) =
            inject_remote_mcp_auth(self.secrets.as_ref(), &request.request, provider_request)
                .await?;
        let provider =
            resolve_model_provider(self.provider_keys.as_ref(), &request.request.model).await?;
        let request_dump = debug_dump_request(self.debug_dumps, &redacted_request)?;
        let response = self
            .client
            .create(
                send_request,
                provider.as_ref().map(|provider| provider.as_request_auth()),
                provider
                    .as_ref()
                    .and_then(|provider| provider.endpoint.as_ref())
                    .map(|endpoint| &endpoint.transport),
            )
            .await?;
        let mut result = result_from_response(self.blobs.as_ref(), &request, &response).await?;
        catalog.normalize(&mut result);
        let debug_dumps = store_debug_dumps(
            self.blobs.as_ref(),
            request_dump,
            &response.raw_json,
            &request,
            "openai_responses",
        )
        .await?;

        Ok(LlmGenerationExecution {
            result,
            debug_dumps,
        })
    }
}

#[async_trait]
impl LlmCompactionAdapter for OpenAiResponsesLlmAdapter {
    async fn compact_context(
        &self,
        request: ContextCompactionRequest,
    ) -> LlmAdapterResult<ContextCompactionResult> {
        if request.request.model.api_kind != ProviderApiKind::OpenAiResponses {
            return Err(LlmAdapterError::RequestKindMismatch {
                message: format!(
                    "expected OpenAiResponses compaction task, got {:?}",
                    request.request.model.api_kind
                ),
            });
        }
        let provider_request = self.materialize_compact_request(&request.request).await?;
        let provider =
            resolve_model_provider(self.provider_keys.as_ref(), &request.request.model).await?;
        let response = self
            .client
            .compact(
                provider_request,
                provider.as_ref().map(|provider| provider.as_request_auth()),
                provider
                    .as_ref()
                    .and_then(|provider| provider.endpoint.as_ref())
                    .map(|endpoint| &endpoint.transport),
            )
            .await?;
        result_from_compact_response(self.blobs.as_ref(), &request, &response).await
    }
}

pub async fn materialize_create_request(
    blobs: &dyn BlobStore,
    request: &LlmRequest,
) -> LlmAdapterResult<oai::CreateResponseRequest> {
    materialize_create_request_with_inventory(blobs, &UnconfiguredMcpInventoryResolver, request)
        .await
}

async fn materialize_create_request_with_inventory(
    blobs: &dyn BlobStore,
    inventory: &dyn McpInventoryResolver,
    request: &LlmRequest,
) -> LlmAdapterResult<oai::CreateResponseRequest> {
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
) -> LlmAdapterResult<oai::CreateResponseRequest> {
    let mut params = openai_responses_params(request.params.as_ref())?;
    // Materialize intent fields into provider params. Explicit per-run
    // provider params win: derived values never overwrite fields the params
    // body already sets.
    if let Some(effort) = request.reasoning_effort.as_deref() {
        let derived = openai_reasoning_from_effort(effort)?;
        if params.reasoning.is_none() {
            params.reasoning = derived;
        }
    }
    if params.parallel_tool_calls.is_none() {
        params.parallel_tool_calls = request.parallel_tool_use;
    }
    let instructions = materialize_instructions(blobs, &request.context.entries).await?;
    let input_entries = request
        .context
        .entries
        .iter()
        .filter(|entry| !matches!(entry.kind, ContextEntryKind::Instructions))
        .cloned()
        .collect::<Vec<_>>();
    let input_items = materialize_input_items(blobs, &input_entries).await?;
    let tools = materialize_tools(blobs, inventory, catalog).await?;

    let mut extra = params.extra.clone();
    let service_tier = crate::params::take_openai_service_tier(&mut extra, params.service_tier)?
        .or(crate::params::openai_processing_service_tier(
            &request.model.provider_id,
            request.processing_tier,
        )?);
    insert_optional(&mut extra, "truncation", params.truncation.clone());
    if let Some(max_tool_calls) = params.max_tool_calls {
        extra.insert("max_tool_calls".to_string(), Value::from(max_tool_calls));
    }

    Ok(oai::CreateResponseRequest {
        model: Some(request.model.model.clone()),
        input: Some(oai::ResponseInput::Items(input_items)),
        instructions,
        previous_response_id: request.provider_response_id.clone(),
        tools: non_empty(tools),
        tool_choice: catalog
            .tool_choice(request.tool_choice.as_ref())?
            .as_ref()
            .map(openai_tool_choice),
        reasoning: params.reasoning.as_ref().map(|reasoning| oai::Reasoning {
            effort: reasoning.effort.clone(),
            summary: reasoning.summary.clone(),
            extra: reasoning.extra.clone(),
        }),
        text: params.text.clone(),
        include: non_empty(params.include.clone()),
        max_output_tokens: request.output_limit.map(u64::from),
        temperature: optional_f64(params.temperature.as_ref(), "temperature")?,
        top_p: optional_f64(params.top_p.as_ref(), "top_p")?,
        metadata: non_empty_map(params.metadata.clone()),
        parallel_tool_calls: params.parallel_tool_calls,
        store: params.store,
        stream: params.stream,
        service_tier,
        context_management: context_management_from_compaction(request.compaction.as_ref()),
        extra,
        prompt_cache_key: None,
    })
}

fn context_management_from_compaction(compaction: Option<&CompactionPolicy>) -> Option<Value> {
    match compaction {
        Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        }) => {
            let mut compaction = json!({ "type": "compaction" });
            if let Some(compact_threshold_tokens) = compact_threshold_tokens {
                compaction["compact_threshold"] = json!(compact_threshold_tokens);
            }
            Some(json!([compaction]))
        }
        None | Some(CompactionPolicy::Disabled | CompactionPolicy::ProviderStandalone { .. }) => {
            None
        }
    }
}

pub async fn materialize_compact_request(
    blobs: &dyn BlobStore,
    task: &ContextCompactionTask,
) -> LlmAdapterResult<oai::CompactResponseRequest> {
    let input_items = materialize_input_items(blobs, &task.context.entries).await?;
    Ok(oai::CompactResponseRequest {
        model: task.model.model.clone(),
        input: Some(oai::ResponseInput::Items(input_items)),
        extra: Default::default(),
    })
}

async fn materialize_instructions(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
) -> LlmAdapterResult<Option<Value>> {
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
        Ok(None)
    } else {
        Ok(Some(Value::String(parts.join("\n\n"))))
    }
}

async fn materialize_input_items(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
) -> LlmAdapterResult<Vec<oai::ResponseInputItem>> {
    let mut input: Vec<oai::ResponseInputItem> = Vec::with_capacity(entries.len());
    for item in entries {
        let next = materialize_input_item(blobs, item).await?;
        // Consecutive same-role USER messages (for example an image entry
        // plus its caption) fold into one message with multiple content
        // parts — the canonical Responses input shape. Assistant history is
        // never folded: assistant-role input items require `output_text`
        // parts, not `input_text`, so they stay as plain text messages.
        match (input.last_mut(), next) {
            (
                Some(oai::ResponseInputItem::Message(previous)),
                oai::ResponseInputItem::Message(message),
            ) if previous.role == oai::MessageRole::User
                && message.role == oai::MessageRole::User
                && previous.extra.is_empty()
                && message.extra.is_empty() =>
            {
                let mut parts = input_message_parts(std::mem::replace(
                    &mut previous.content,
                    oai::InputMessageContent::Parts(Vec::new()),
                ));
                parts.extend(input_message_parts(message.content));
                previous.content = oai::InputMessageContent::Parts(parts);
            }
            (_, next) => input.push(next),
        }
    }
    Ok(input)
}

fn input_message_parts(content: oai::InputMessageContent) -> Vec<oai::InputContent> {
    match content {
        oai::InputMessageContent::Text(text) => vec![oai::InputContent::InputText {
            r#type: oai::InputContentType::InputText,
            text,
        }],
        oai::InputMessageContent::Parts(parts) => parts,
    }
}

async fn materialize_input_item(
    blobs: &dyn BlobStore,
    item: &ContextEntry,
) -> LlmAdapterResult<oai::ResponseInputItem> {
    if is_openai_raw_item(item)
        || (item.content.media_type.as_deref() == Some(MEDIA_TYPE_JSON)
            && item.content.provider_kind.as_deref()
                == Some(OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND))
    {
        return Ok(oai::ResponseInputItem::Raw(
            read_json(blobs, &item.content.content_ref).await?,
        ));
    }

    match &item.kind {
        ContextEntryKind::Message { role } => {
            let role = match role {
                ContextMessageRole::User => oai::MessageRole::User,
                ContextMessageRole::Assistant => oai::MessageRole::Assistant,
            };
            if let Some(mime) = crate::blob_io::image_media_type(item.content.media_type.as_deref())
            {
                let data = crate::blob_io::read_base64(blobs, &item.content.content_ref).await?;
                return Ok(oai::ResponseInputItem::Message(oai::InputMessage {
                    role,
                    content: oai::InputMessageContent::Parts(vec![
                        oai::InputContent::InputText {
                            r#type: oai::InputContentType::InputText,
                            text: crate::blob_io::media_announcement(item),
                        },
                        oai::InputContent::InputImage {
                            r#type: oai::InputImageContentType::InputImage,
                            image_url: format!("data:{mime};base64,{data}"),
                            detail: None,
                        },
                    ]),
                    extra: Default::default(),
                }));
            }
            if let Some(document) = crate::blob_io::document_entry(
                item.content.media_type.as_deref(),
                item.preview.as_deref(),
            ) {
                let parts = if document.is_pdf {
                    let data =
                        crate::blob_io::read_base64(blobs, &item.content.content_ref).await?;
                    vec![
                        oai::InputContent::InputText {
                            r#type: oai::InputContentType::InputText,
                            text: crate::blob_io::media_announcement(item),
                        },
                        oai::InputContent::InputFile {
                            r#type: oai::InputFileContentType::InputFile,
                            filename: Some(
                                document.name.unwrap_or_else(|| "document.pdf".to_owned()),
                            ),
                            file_data: Some(format!("data:{};base64,{data}", document.mime)),
                            file_id: None,
                        },
                    ]
                } else {
                    // The Responses API takes files as PDF only; text-based
                    // documents are inlined with their name as a header.
                    let text = read_text(blobs, &item.content.content_ref).await?;
                    let header = match document.name {
                        Some(name) => format!("[document: {name}]"),
                        None => "[document]".to_owned(),
                    };
                    vec![oai::InputContent::InputText {
                        r#type: oai::InputContentType::InputText,
                        text: format!("{header}\n{text}"),
                    }]
                };
                return Ok(oai::ResponseInputItem::Message(oai::InputMessage {
                    role,
                    content: oai::InputMessageContent::Parts(parts),
                    extra: Default::default(),
                }));
            }
            let text = crate::blob_io::read_message_text(blobs, &item.content).await?;
            Ok(oai::ResponseInputItem::Message(oai::InputMessage {
                role,
                content: oai::InputMessageContent::Text(text),
                extra: Default::default(),
            }))
        }
        ContextEntryKind::ToolResult { call_id, .. } => {
            let output = read_text(blobs, &item.content.content_ref).await?;
            Ok(oai::ResponseInputItem::FunctionCallOutput(
                oai::FunctionCallOutput {
                    r#type: oai::FunctionCallOutputType::FunctionCallOutput,
                    call_id: call_id.as_str().to_string(),
                    output,
                    extra: Default::default(),
                },
            ))
        }
        ContextEntryKind::Instructions => Err(LlmAdapterError::InvalidProviderRequest {
            message: "instruction context entries must materialize as top-level instructions"
                .to_owned(),
        }),
        ContextEntryKind::Catalog { .. } => Ok(developer_message(
            crate::catalog_prompts::stored_catalog_text(blobs, item, &item.content.content_ref)
                .await?,
        )),
        ContextEntryKind::ToolCall { .. }
        | ContextEntryKind::ReasoningState
        | ContextEntryKind::ProviderOpaque => Ok(oai::ResponseInputItem::Raw(
            read_json(blobs, &item.content.content_ref).await?,
        )),
        ContextEntryKind::McpApprovalResponse {
            approval_request_id,
            approve,
        } => Ok(oai::ResponseInputItem::Raw(json!({
            "type": "mcp_approval_response",
            "approval_request_id": approval_request_id,
            "approve": approve,
        }))),
    }
}

fn is_openai_raw_item(item: &ContextEntry) -> bool {
    matches!(
        item.kind,
        ContextEntryKind::ToolCall { .. }
            | ContextEntryKind::ReasoningState
            | ContextEntryKind::ProviderOpaque
            | ContextEntryKind::McpApprovalResponse { .. }
    ) && item.content.media_type.as_deref() == Some(MEDIA_TYPE_JSON)
}

async fn materialize_tools(
    blobs: &dyn BlobStore,
    inventory: &dyn McpInventoryResolver,
    catalog: &mut crate::tool_catalog::ToolCatalog,
) -> LlmAdapterResult<Vec<oai::Tool>> {
    let mut materialized = Vec::with_capacity(catalog.tools.len());
    let mut native_mcp_tool_count = 0usize;
    for tool in &catalog.tools {
        let crate::tool_catalog::ResolvedToolKind::RemoteMcp(spec) = &tool.kind else {
            materialized.push(materialize_tool(blobs, tool).await?);
            continue;
        };
        match (spec.execution, spec.exposure) {
            (RemoteMcpExecution::Provider, _) => {
                materialized.push(materialize_tool(blobs, tool).await?);
            }
            (RemoteMcpExecution::Native, RemoteMcpExposure::Search) => {}
            (RemoteMcpExecution::Native, RemoteMcpExposure::Inject) => {
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
                    let mut function = oai::FunctionTool::new(name, native_tool.input_schema);
                    function.description = native_tool.description;
                    // MCP accepts general JSON Schema. OpenAI strict functions
                    // accept only a narrower subset, so a live remote schema
                    // cannot safely be promoted to strict mode.
                    function.strict = Some(false);
                    materialized.push(oai::Tool::Function(function));
                }
            }
        }
    }
    Ok(materialized)
}

async fn materialize_tool(
    blobs: &dyn BlobStore,
    tool: &crate::tool_catalog::ResolvedTool,
) -> LlmAdapterResult<oai::Tool> {
    match &tool.kind {
        crate::tool_catalog::ResolvedToolKind::Function(function) => {
            let mut materialized =
                oai::FunctionTool::new(tool.name.as_str(), function.input_schema.clone());
            materialized.description = function.description.clone();
            materialized.strict = function.strict;
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
                    materialized.extra.insert(key.clone(), value.clone());
                }
            }
            Ok(oai::Tool::Function(materialized))
        }
        crate::tool_catalog::ResolvedToolKind::ProviderNative(native) => {
            if native.api_kind != ProviderApiKind::OpenAiResponses {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: format!(
                        "provider-native tool {} targets {:?}, not OpenAiResponses",
                        tool.name, native.api_kind
                    ),
                });
            }
            match native.execution {
                ProviderNativeToolExecution::ProviderHosted
                | ProviderNativeToolExecution::ClientEffect => {
                    Ok(oai::Tool::Raw(native.definition.clone()))
                }
            }
        }
        crate::tool_catalog::ResolvedToolKind::RemoteMcp(remote_mcp) => {
            materialize_remote_mcp_tool(blobs, remote_mcp).await
        }
    }
}

async fn materialize_remote_mcp_tool(
    blobs: &dyn BlobStore,
    remote_mcp: &RemoteMcpToolSpec,
) -> LlmAdapterResult<oai::Tool> {
    // Materialized requests never contain auth values; `inject_remote_mcp_auth`
    // adds `authorization` to the send request immediately before provider I/O.
    let mut value = json!({
        "type": "mcp",
        "server_label": remote_mcp.server_label,
        "server_url": remote_mcp.server_url,
    });
    let object = value.as_object_mut().expect("mcp tool object");

    if let Some(description_ref) = &remote_mcp.description_ref {
        object.insert(
            "server_description".to_string(),
            Value::String(read_text(blobs, description_ref).await?),
        );
    }
    if let Some(allowed_tools) = &remote_mcp.allowed_tools {
        object.insert("allowed_tools".to_string(), json!(allowed_tools));
    }
    match remote_mcp.approval {
        RemoteMcpApprovalPolicy::Always => {
            object.insert(
                "require_approval".to_string(),
                Value::String("always".to_string()),
            );
        }
        RemoteMcpApprovalPolicy::Never => {
            object.insert(
                "require_approval".to_string(),
                Value::String("never".to_string()),
            );
        }
    }
    if let Some(defer_loading) = remote_mcp.defer_loading {
        object.insert("defer_loading".to_string(), Value::Bool(defer_loading));
    }

    Ok(oai::Tool::Raw(value))
}

/// Produce the request pair `generate` actually uses: the send request with
/// `authorization` resolved at the last moment, and the redacted request that
/// is persisted to blobs, preserving only the fact that auth was configured.
async fn inject_remote_mcp_auth(
    secrets: &dyn SecretResolver,
    request: &LlmRequest,
    materialized: oai::CreateResponseRequest,
) -> LlmAdapterResult<(oai::CreateResponseRequest, oai::CreateResponseRequest)> {
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
        set_remote_mcp_authorization(
            &mut send_request,
            &remote_mcp.server_label,
            token.expose(),
            tool,
        )?;
        set_remote_mcp_authorization(
            &mut redacted_request,
            &remote_mcp.server_label,
            REDACTED_SECRET_PLACEHOLDER,
            tool,
        )?;
    }
    Ok((send_request, redacted_request))
}

fn set_remote_mcp_authorization(
    request: &mut oai::CreateResponseRequest,
    server_label: &str,
    value: &str,
    tool: &ToolSpec,
) -> LlmAdapterResult<()> {
    let entry = request
        .tools
        .as_mut()
        .into_iter()
        .flatten()
        .find_map(|materialized| match materialized {
            oai::Tool::Raw(raw)
                if raw.get("type").and_then(Value::as_str) == Some("mcp")
                    && raw.get("server_label").and_then(Value::as_str) == Some(server_label) =>
            {
                raw.as_object_mut()
            }
            _ => None,
        });
    let Some(entry) = entry else {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "materialized request is missing MCP tool entry for {} (server label {server_label})",
                tool.name
            ),
        });
    };
    entry.insert("authorization".to_string(), Value::String(value.to_owned()));
    Ok(())
}

fn openai_tool_choice(choice: &ToolChoice) -> oai::ToolChoice {
    match choice {
        ToolChoice::Auto => oai::ToolChoice::Mode(oai::ToolChoiceMode::Auto),
        ToolChoice::None => oai::ToolChoice::Mode(oai::ToolChoiceMode::None),
        ToolChoice::RequiredAny => oai::ToolChoice::Mode(oai::ToolChoiceMode::Required),
        ToolChoice::Specific { tool_name } => oai::ToolChoice::Function {
            r#type: oai::FunctionToolType::Function,
            name: tool_name.as_str().to_string(),
        },
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

fn non_empty<T>(entries: Vec<T>) -> Option<Vec<T>> {
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn non_empty_map<T>(
    entries: std::collections::BTreeMap<String, T>,
) -> Option<std::collections::BTreeMap<String, T>> {
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn insert_optional<T>(
    extra: &mut std::collections::BTreeMap<String, Value>,
    key: &str,
    value: Option<T>,
) where
    T: Into<Value>,
{
    if let Some(value) = value {
        extra.insert(key.to_string(), value.into());
    }
}

pub async fn result_from_response(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    response: &ApiResponse<oai::Response>,
) -> LlmAdapterResult<LlmGenerationResult> {
    let mut context_entries = Vec::new();
    let mut tool_calls = Vec::new();
    let mut approval_requests = Vec::new();

    for (index, item) in response.parsed.output.iter().enumerate() {
        let raw_item = raw_output_item(&response.raw_json, index, item)?;
        match item.r#type.as_str() {
            "message" => {
                context_entries.extend(
                    assistant_context_entries(blobs, request, item, &response.parsed, raw_item)
                        .await?,
                );
            }
            "function_call" => {
                let (context_entry, tool_call) =
                    function_call_context(blobs, request, item, raw_item, index).await?;
                context_entries.push(context_entry);
                tool_calls.push(tool_call);
            }
            "reasoning" => {
                if let Some(item) = reasoning_context_entry(blobs, request, item, raw_item).await? {
                    context_entries.push(item);
                }
            }
            "compaction" | "compaction_summary" | "context_compaction" => {
                context_entries.push(compaction_context_entry(blobs, raw_item).await?);
            }
            "web_search_call" => {
                context_entries.push(web_search_call_context_entry(blobs, raw_item).await?);
            }
            "mcp_list_tools" | "mcp_call" => {
                context_entries.push(mcp_context_entry(blobs, item, raw_item).await?);
            }
            "mcp_approval_request" => {
                let (entry, approval) =
                    mcp_approval_request_context(blobs, request, item, raw_item).await?;
                context_entries.push(entry);
                approval_requests.push(approval);
            }
            _ => {}
        }
    }

    let finish = finish_reason(&response.parsed, !tool_calls.is_empty());
    let status = generation_status(response.parsed.status);
    let usage = response.parsed.usage.as_ref().map(llm_usage);
    let (status, failure_ref, context_entries, tool_calls) =
        if status == LlmGenerationStatus::Failed {
            (
                status,
                Some(provider_failure_ref(blobs, &response.parsed).await?),
                context_entries,
                tool_calls,
            )
        } else if finish == LlmFinish::ContentFilter {
            // A content filter is terminal for the turn, the same as a provider
            // refusal: fail it with the reason instead of completing as an empty
            // or partial answer, drop the partial content, and never fall back
            // to another model.
            (
                LlmGenerationStatus::Failed,
                Some(content_filter_failure_ref(blobs, request, &response.parsed.id).await?),
                Vec::new(),
                Vec::new(),
            )
        } else if finish == LlmFinish::Length {
            // Cut off at the output cap: fail the turn but keep the partial
            // text; function calls from an unfinished turn have no outputs to
            // replay against and are dropped with the reasoning items.
            let failure_ref = put_text(
                blobs,
                truncation_failure_text(
                    request.run_id,
                    request.turn_id,
                    "OpenAI Responses",
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
            (status, None, context_entries, tool_calls)
        };
    if status != LlmGenerationStatus::Succeeded {
        approval_requests.clear();
    }
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
            approval_requests,
            context_token_estimate: response
                .parsed
                .usage
                .as_ref()
                .and_then(|usage| usage.input_tokens)
                .map(|tokens| TokenEstimate {
                    tokens: u64_to_u32(tokens),
                    quality: TokenEstimateQuality::ProviderCounted,
                }),
        },
    })
}

async fn mcp_approval_request_context(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    item: &oai::ResponseOutputItem,
    raw_item: Value,
) -> LlmAdapterResult<(ContextEntryInput, ObservedApprovalRequest)> {
    let provider_request_id =
        item.id
            .clone()
            .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
                message: "OpenAI MCP approval request is missing id".to_owned(),
            })?;
    let server_label = raw_item
        .get("server_label")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: "OpenAI MCP approval request is missing server_label".to_owned(),
        })?
        .to_owned();
    let tool_name = item
        .name
        .as_deref()
        .or_else(|| raw_item.get("name").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: "OpenAI MCP approval request is missing tool name".to_owned(),
        })?
        .to_owned();
    let arguments = item
        .arguments
        .as_deref()
        .or_else(|| raw_item.get("arguments").and_then(Value::as_str))
        .unwrap_or("{}");
    let remote = request
        .request
        .tools
        .iter()
        .find_map(|tool| match &tool.kind {
            ToolKind::RemoteMcp(remote) if remote.server_label == server_label => Some(remote),
            _ => None,
        })
        .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "OpenAI MCP approval request names unknown server label {server_label}"
            ),
        })?;
    let arguments_ref = put_text(blobs, arguments).await?;
    let entry = mcp_context_entry(blobs, item, raw_item).await?;
    Ok((
        entry,
        ObservedApprovalRequest {
            subject: ApprovalSubject::McpToolCall {
                server_id: remote.server_id.clone(),
                server_label,
                tool_name,
                arguments_ref,
            },
            continuation: ApprovalContinuation::OpenAiMcp {
                provider_request_id,
            },
        },
    ))
}

pub async fn result_from_compact_response(
    blobs: &dyn BlobStore,
    request: &ContextCompactionRequest,
    response: &ApiResponse<oai::CompactResponse>,
) -> LlmAdapterResult<ContextCompactionResult> {
    let mut context_entries = Vec::new();
    for (index, item) in response.parsed.output.iter().enumerate() {
        let raw_item = raw_output_item(&response.raw_json, index, item)?;
        if matches!(
            item.r#type.as_str(),
            "compaction" | "compaction_summary" | "context_compaction"
        ) {
            context_entries.push(compaction_context_entry(blobs, raw_item).await?);
        }
    }
    if context_entries.is_empty() {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "OpenAI Responses compact response {} did not include a compaction output item",
                response.parsed.id
            ),
        });
    }
    Ok(ContextCompactionResult {
        session_id: request.session_id.clone(),
        context_revision: request.request.context.context_revision,
        status: ContextCompactionStatus::Succeeded,
        failure_ref: None,
        context_entries,
    })
}

/// Failure text for a `content_filter` stop, in the worker's provider-error
/// blob layout so clients render it like any other model failure.
async fn content_filter_failure_ref(
    blobs: &dyn BlobStore,
    request: &LlmGenerationRequest,
    response_id: &str,
) -> LlmAdapterResult<BlobRef> {
    put_text(
        blobs,
        format!(
            "core agent LLM generation failed\nrun_id={}\nturn_id={}\n\
             error=OpenAI Responses stopped response {response_id} for content_filter\n",
            request.run_id, request.turn_id
        ),
    )
    .await
}

async fn provider_failure_ref(
    blobs: &dyn BlobStore,
    response: &oai::Response,
) -> LlmAdapterResult<BlobRef> {
    let message = match &response.error {
        Some(error) => {
            let detail = error
                .message
                .as_deref()
                .unwrap_or("OpenAI response failed without an error message");
            let code = error.code.as_deref().unwrap_or("unknown_code");
            let kind = error.r#type.as_deref().unwrap_or("unknown_type");
            format!(
                "OpenAI Responses generation failed\nresponse_id={}\nerror_type={kind}\nerror_code={code}\nmessage={detail}\n",
                response.id
            )
        }
        None => format!(
            "OpenAI Responses generation failed\nresponse_id={}\nmessage=response status was failed\n",
            response.id
        ),
    };
    put_text(blobs, &message).await
}

fn raw_output_item(
    raw_response: &Value,
    index: usize,
    item: &oai::ResponseOutputItem,
) -> LlmAdapterResult<Value> {
    if let Some(raw_item) = raw_response
        .get("output")
        .and_then(Value::as_array)
        .and_then(|output| output.get(index))
    {
        return Ok(raw_item.clone());
    }
    serde_json::to_value(item).map_err(|error| LlmAdapterError::InvalidProviderRequest {
        message: format!("failed to encode OpenAI output item: {error}"),
    })
}

async fn assistant_context_entries(
    blobs: &dyn BlobStore,
    _request: &LlmGenerationRequest,
    item: &oai::ResponseOutputItem,
    response: &oai::Response,
    raw_item: Value,
) -> LlmAdapterResult<Vec<ContextEntryInput>> {
    let text = item
        .content
        .iter()
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>()
        .join("");
    // Fall back to the response-wide output text only when this is the
    // single message item; with multiple message items (gpt-5.5 sometimes
    // emits a trailing empty one) the whole-response fallback would
    // duplicate the other item's text.
    let message_items = response
        .output
        .iter()
        .filter(|candidate| candidate.r#type == "message")
        .count();
    let text = if text.is_empty() && message_items == 1 {
        response.output_text()
    } else {
        text
    };
    // A model-authored refusal (`{"type": "refusal"}` parts) is the visible
    // answer, as with Chat Completions' `refusal` field: render it so the
    // turn never looks empty. Server-side filtering is a `content_filter`
    // stop and fails the turn instead.
    let (text, provider_kind) = if text.is_empty() {
        let refusal = item
            .content
            .iter()
            .filter_map(|content| content.refusal.as_deref())
            .collect::<Vec<_>>()
            .join("");
        (refusal, PROVIDER_KIND_REFUSAL)
    } else {
        (text, PROVIDER_KIND_MESSAGE)
    };
    if text.is_empty() {
        return Ok(Vec::new());
    }

    // Incomplete/test responses can expose response-wide text without a native
    // message body. Preserve that text explicitly; normal messages stay native.
    let native = llm_clients::content::openai_response_message(&raw_item)
        .is_some_and(|native_text| !native_text.is_empty());
    Ok(vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        },
        content: engine::ContentRef {
            content_ref: if native {
                put_json(blobs, &raw_item).await?
            } else {
                put_text(blobs, &text).await?
            },
            media_type: Some(
                if native {
                    MEDIA_TYPE_JSON
                } else {
                    MEDIA_TYPE_TEXT
                }
                .to_owned(),
            ),
            provider_kind: Some(
                if native {
                    OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND
                } else {
                    provider_kind
                }
                .to_owned(),
            ),
        },
        preview: Some(text.chars().take(256).collect()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }])
}

async fn function_call_context(
    blobs: &dyn BlobStore,
    _request: &LlmGenerationRequest,
    item: &oai::ResponseOutputItem,
    raw_item: Value,
    index: usize,
) -> LlmAdapterResult<(ContextEntryInput, ObservedToolCall)> {
    let call = oai::FunctionCallRef {
        item_id: item.id.as_deref(),
        call_id: item.call_id.as_deref(),
        name: item
            .name
            .as_deref()
            .ok_or_else(|| LlmAdapterError::InvalidProviderRequest {
                message: "OpenAI function_call item is missing name".to_string(),
            })?,
        arguments: item.arguments.as_deref().unwrap_or("{}"),
    };
    let call_id = call
        .call_id
        .or(call.item_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("call_{index}"));
    let call_id = ToolCallId::try_new(call_id.clone()).map_err(|error| {
        LlmAdapterError::InvalidProviderRequest {
            message: format!("invalid OpenAI tool call id {call_id:?}: {error}"),
        }
    })?;
    let tool_name = ToolName::try_new(call.name.to_string()).map_err(|error| {
        LlmAdapterError::InvalidProviderRequest {
            message: format!("invalid OpenAI tool name {:?}: {error}", call.name),
        }
    })?;
    let arguments_ref =
        crate::blob_io::put_bytes(blobs, call.arguments.as_bytes().to_vec()).await?;
    let native_call_ref = put_json(blobs, &raw_item).await?;

    let context_entry = ContextEntryInput {
        kind: ContextEntryKind::ToolCall {
            call_id: call_id.clone(),
            name: tool_name.clone(),
        },
        content: engine::ContentRef {
            content_ref: native_call_ref.clone(),
            media_type: Some(MEDIA_TYPE_JSON.to_string()),
            provider_kind: Some(PROVIDER_KIND_FUNCTION_CALL.to_string()),
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
        provider_kind: Some(PROVIDER_KIND_FUNCTION_CALL.to_string()),
        arguments_ref,
        native_call_ref: Some(native_call_ref),
    };
    Ok((context_entry, tool_call))
}

async fn reasoning_context_entry(
    blobs: &dyn BlobStore,
    _request: &LlmGenerationRequest,
    item: &oai::ResponseOutputItem,
    raw_item: Value,
) -> LlmAdapterResult<Option<ContextEntryInput>> {
    let text = llm_clients::content::openai_response_reasoning(&raw_item).unwrap_or_default();
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(Some(ContextEntryInput {
        kind: ContextEntryKind::ReasoningState,
        content: engine::ContentRef {
            content_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_string()),
            provider_kind: Some(
                llm_clients::content::OPENAI_RESPONSES_REASONING_PROVIDER_KIND.to_owned(),
            ),
        },
        preview: Some(if text.is_empty() {
            item.id
                .as_deref()
                .map(|id| format!("reasoning state {id}"))
                .unwrap_or_else(|| "reasoning state".to_string())
        } else {
            text.chars().take(256).collect()
        }),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }))
}

async fn compaction_context_entry(
    blobs: &dyn BlobStore,
    raw_item: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content: engine::ContentRef {
            content_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_string()),
            provider_kind: Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND.to_string()),
        },
        preview: Some("OpenAI Responses compaction item".to_string()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    })
}

async fn web_search_call_context_entry(
    blobs: &dyn BlobStore,
    raw_item: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content: engine::ContentRef {
            content_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_string()),
            provider_kind: Some(OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND.to_string()),
        },
        preview: Some("OpenAI Responses web search call".to_string()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    })
}

async fn mcp_context_entry(
    blobs: &dyn BlobStore,
    item: &oai::ResponseOutputItem,
    raw_item: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let provider_kind = match item.r#type.as_str() {
        "mcp_list_tools" => OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND,
        "mcp_call" => OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND,
        "mcp_approval_request" => OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND,
        _ => {
            return Err(LlmAdapterError::InvalidProviderRequest {
                message: format!("unsupported OpenAI MCP output item type {}", item.r#type),
            });
        }
    };
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content: engine::ContentRef {
            content_ref,
            media_type: Some(MEDIA_TYPE_JSON.to_string()),
            provider_kind: Some(provider_kind.to_string()),
        },
        preview: Some(mcp_preview(item, &raw_item)),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    })
}

fn mcp_preview(item: &oai::ResponseOutputItem, raw_item: &Value) -> String {
    let server_label = raw_item.get("server_label").and_then(Value::as_str);
    match item.r#type.as_str() {
        "mcp_list_tools" => match server_label {
            Some(server_label) => format!("OpenAI Responses MCP tool list: {server_label}"),
            None => "OpenAI Responses MCP tool list".to_string(),
        },
        "mcp_call" => {
            let name = item
                .name
                .as_deref()
                .or_else(|| raw_item.get("name").and_then(Value::as_str));
            match (server_label, name) {
                (Some(server_label), Some(name)) => {
                    format!("OpenAI Responses MCP tool call: {server_label}.{name}")
                }
                (None, Some(name)) => format!("OpenAI Responses MCP tool call: {name}"),
                _ => "OpenAI Responses MCP tool call".to_string(),
            }
        }
        "mcp_approval_request" => match server_label {
            Some(server_label) => {
                format!("OpenAI Responses MCP approval request: {server_label}")
            }
            None => "OpenAI Responses MCP approval request".to_string(),
        },
        _ => "OpenAI Responses MCP output item".to_string(),
    }
}

fn generation_status(status: Option<oai::ResponseStatus>) -> LlmGenerationStatus {
    match status {
        Some(oai::ResponseStatus::Failed) => LlmGenerationStatus::Failed,
        Some(oai::ResponseStatus::Cancelled) => LlmGenerationStatus::Cancelled,
        _ => LlmGenerationStatus::Succeeded,
    }
}

fn finish_reason(response: &oai::Response, has_tool_calls: bool) -> LlmFinish {
    match response.status {
        Some(oai::ResponseStatus::Failed) => LlmFinish::Failed,
        Some(oai::ResponseStatus::Cancelled) => LlmFinish::Cancelled,
        Some(oai::ResponseStatus::Incomplete) => match response
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
        {
            Some("max_output_tokens") => LlmFinish::Length,
            Some("content_filter") => LlmFinish::ContentFilter,
            Some("context_length_exceeded" | "max_input_tokens" | "max_prompt_tokens") => {
                LlmFinish::ContextLimit
            }
            _ => LlmFinish::Unknown,
        },
        _ if has_tool_calls => LlmFinish::ToolCalls,
        Some(oai::ResponseStatus::Completed) => LlmFinish::Stop,
        _ => LlmFinish::Unknown,
    }
}

fn llm_usage(usage: &oai::Usage) -> LlmUsage {
    LlmUsage {
        input_tokens: usage.input_tokens.map(u64_to_u32),
        output_tokens: usage.output_tokens.map(u64_to_u32),
        reasoning_tokens: usage.reasoning_tokens().map(u64_to_u32),
        total_tokens: usage.total_tokens.map(u64_to_u32),
        cached_input_tokens: usage.cached_tokens().map(u64_to_u32),
        cache_write_input_tokens: None,
        cache_miss_input_tokens: None,
    }
}

fn u64_to_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

/// A developer-role text item; catalogs and instructions render this way.
fn developer_message(text: String) -> oai::ResponseInputItem {
    oai::ResponseInputItem::Message(oai::InputMessage {
        role: oai::MessageRole::Developer,
        content: oai::InputMessageContent::Text(text),
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use engine::{
        ContextCompactionTask, ContextEntryId, ContextEntrySource, ContextSnapshot, CoreAgentLlm,
        FunctionToolSpec, LlmGenerationRequest, LlmRequest, ModelSelection, ProviderParams, RunId,
        SessionId, ToolParallelism, TurnId, storage::InMemoryBlobStore,
    };
    use llm_clients::HeaderSnapshot;
    use serde_json::json;
    use tools::web::search::{OpenAiResponsesWebSearchConfig, WebSearchContextSize, WebSearchMode};

    use super::*;
    use crate::executor::{LlmAdapterRegistry, LlmRuntime};
    use crate::params::{OpenAiReasoningConfig, OpenAiResponsesParams};

    struct StaticMcpInventory;

    #[async_trait]
    impl McpInventoryResolver for StaticMcpInventory {
        async fn list_tools(
            &self,
            _spec: &RemoteMcpToolSpec,
        ) -> Result<Vec<crate::NativeMcpTool>, crate::McpInventoryError> {
            Ok(vec![crate::NativeMcpTool {
                remote_name: "search_issues".to_owned(),
                description: Some("Search issues".to_owned()),
                input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
                annotations: None,
            }])
        }
    }

    fn native_mcp_tool(exposure: RemoteMcpExposure) -> ToolSpec {
        ToolSpec {
            name: ToolName::try_new("mcp_github").expect("name"),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "github".to_owned(),
                record_revision: 1,
                server_label: "github".to_owned(),
                server_url: "https://example.com/mcp".to_owned(),
                description_ref: None,
                allowed_tools: None,
                execution: RemoteMcpExecution::Native,
                exposure,
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: None,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            execution: Default::default(),
            parallelism: ToolParallelism::ParallelSafe,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_mcp_injects_functions_and_search_omits_server_entry() {
        let blobs = InMemoryBlobStore::new();
        let tools = materialize_tools(
            &blobs,
            &StaticMcpInventory,
            &mut crate::tool_catalog::ToolCatalog::resolve(
                &blobs,
                &tools::runtime::ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                &[native_mcp_tool(RemoteMcpExposure::Inject), {
                    let mut tool = native_mcp_tool(RemoteMcpExposure::Search);
                    tool.name = ToolName::new("mcp_other");
                    tool
                }],
            )
            .await
            .expect("catalog"),
        )
        .await
        .expect("materialize native MCP");
        let value = serde_json::to_value(tools).expect("tools json");
        assert_eq!(value.as_array().expect("array").len(), 1);
        assert_eq!(value[0]["name"], "mcp_github__search_issues");
        assert_eq!(value[0]["strict"], false);
    }

    struct FakeOpenAiResponsesApi {
        response: ApiResponse<oai::Response>,
        compact_response: ApiResponse<oai::CompactResponse>,
        seen: Mutex<Vec<oai::CreateResponseRequest>>,
        seen_compact: Mutex<Vec<oai::CompactResponseRequest>>,
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
    impl OpenAiResponsesApi for FakeOpenAiResponsesApi {
        async fn create(
            &self,
            request: oai::CreateResponseRequest,
            auth: Option<llm_clients::RequestAuth<'_>>,
            _endpoint: Option<&llm_clients::EndpointOverride>,
        ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
            self.seen.lock().expect("lock").push(request);
            self.seen_api_keys
                .lock()
                .expect("lock")
                .push(observed_auth(auth));
            Ok(self.response.clone())
        }

        async fn compact(
            &self,
            request: oai::CompactResponseRequest,
            auth: Option<llm_clients::RequestAuth<'_>>,
            _endpoint: Option<&llm_clients::EndpointOverride>,
        ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError> {
            self.seen_compact.lock().expect("lock").push(request);
            self.seen_api_keys
                .lock()
                .expect("lock")
                .push(observed_auth(auth));
            Ok(self.compact_response.clone())
        }
    }

    async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
        blobs.insert_text(text).await
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

    fn model() -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_string(),
            model: "gpt-5.1".to_string(),
        }
    }

    fn intent_request(entries: Vec<ContextEntry>) -> LlmRequest {
        LlmRequest {
            model: model(),
            request_fingerprint: "sha256:test".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
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

    fn openai_params(params: &OpenAiResponsesParams) -> ProviderParams {
        ProviderParams::new(
            ProviderApiKind::OpenAiResponses,
            serde_json::to_value(params).expect("serialize params"),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_derives_reasoning_and_parallel_from_intent() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("xhigh".to_string());
        request.parallel_tool_use = Some(false);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["reasoning"],
            json!({ "effort": "xhigh", "summary": "auto" })
        );
        assert_eq!(value["parallel_tool_calls"], json!(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_none_reasoning_effort_omits_reasoning() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("none".to_string());

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert!(value.get("reasoning").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_params_win_over_intent_fields() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.reasoning_effort = Some("high".to_string());
        request.parallel_tool_use = Some(false);
        request.params = Some(openai_params(&OpenAiResponsesParams {
            reasoning: Some(OpenAiReasoningConfig {
                effort: Some("low".to_string()),
                summary: None,
                extra: BTreeMap::new(),
            }),
            parallel_tool_calls: Some(true),
            ..OpenAiResponsesParams::default()
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(value["reasoning"], json!({ "effort": "low" }));
        assert_eq!(value["parallel_tool_calls"], json!(true));
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
    async fn materialize_create_request_maps_context_tools_and_defaults() {
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
            crate::blob_io::put_json(&blobs, &json!({ "x-openai-extra": true }))
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
        let item = ContextEntry {
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
        let mut request = intent_request(vec![instructions_item, item]);
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
        request.output_limit = Some(2048);
        request.provider_response_id = Some("resp_prev".to_string());
        request.compaction = Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens: Some(120_000),
        });
        request.processing_tier = Some(engine::ModelProcessingTier::Flex);
        request.params = Some(openai_params(&OpenAiResponsesParams {
            reasoning: Some(OpenAiReasoningConfig {
                effort: Some("medium".to_string()),
                summary: Some("auto".to_string()),
                extra: BTreeMap::new(),
            }),
            text: Some(json!({ "format": { "type": "text" } })),
            include: vec!["reasoning.encrypted_content".to_string()],
            temperature: Some(json!(0.2)),
            top_p: Some(json!(0.9)),
            metadata: BTreeMap::from([("run".to_string(), "1".to_string())]),
            parallel_tool_calls: Some(true),
            store: Some(false),
            stream: Some(true),
            truncation: Some("auto".to_string()),
            max_tool_calls: Some(4),
            service_tier: None,
            extra: BTreeMap::new(),
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value,
            json!({
                "model": "gpt-5.1",
                "input": [{ "role": "user", "content": "Read Cargo.toml" }],
                "instructions": "Be precise.",
                "previous_response_id": "resp_prev",
                "tools": [{
                    "type": "function",
                    "name": "read_file",
                    "description": "Read a file",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    },
                    "strict": true,
                    "x-openai-extra": true
                }],
                "tool_choice": { "type": "function", "name": "read_file" },
                "reasoning": { "effort": "medium", "summary": "auto" },
                "text": { "format": { "type": "text" } },
                "include": ["reasoning.encrypted_content"],
                "max_output_tokens": 2048,
                "temperature": 0.2,
                "top_p": 0.9,
                "metadata": { "run": "1" },
                "parallel_tool_calls": true,
                "store": false,
                "stream": true,
                "service_tier": "flex",
                "truncation": "auto",
                "context_management": [{ "type": "compaction", "compact_threshold": 120000 }],
                "max_tool_calls": 4
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_passes_provider_native_web_search_tool() {
        let blobs = InMemoryBlobStore::new();
        let native = OpenAiResponsesWebSearchConfig {
            mode: WebSearchMode::Cached,
            search_context_size: Some(WebSearchContextSize::Low),
            allowed_domains: vec!["docs.rs".to_string()],
            blocked_domains: Vec::new(),
            user_location: None,
            include_sources: true,
        }
        .native_tool_json()
        .expect("web search definition")
        .expect("enabled web search");

        let mut request = intent_request(Vec::new());
        request.tools = vec![engine::ToolSpec {
            name: engine::ToolName::new("web_search"),
            kind: engine::ToolKind::ProviderNative(engine::ProviderNativeToolSpec {
                api_kind: ProviderApiKind::OpenAiResponses,
                native_tool_ref: blobs
                    .put_bytes(serde_json::to_vec(&native).expect("native json"))
                    .await
                    .expect("native definition"),
                execution: engine::ProviderNativeToolExecution::ProviderHosted,
            }),
            parallelism: engine::ToolParallelism::ParallelSafe,
            execution: Default::default(),
        }];
        request.tool_choice = Some(ToolChoice::Auto);
        request.output_limit = Some(1024);
        request.params = Some(openai_params(&OpenAiResponsesParams {
            include: vec![crate::params::OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE.to_string()],
            store: Some(false),
            ..OpenAiResponsesParams::default()
        }));

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["tools"],
            json!([{
                "type": "web_search",
                "external_web_access": false,
                "search_context_size": "low",
                "filters": {
                    "allowed_domains": ["docs.rs"]
                }
            }])
        );
        assert_eq!(value["tool_choice"], json!("auto"));
        assert_eq!(
            value["include"],
            json!([crate::params::OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_lowers_no_auth_remote_mcp_tool() {
        let blobs = InMemoryBlobStore::new();
        let description_ref = text_blob(&blobs, "Echo test MCP server").await;
        let mut request = intent_request(Vec::new());
        request.tools = vec![ToolSpec {
            name: ToolName::new("mcp_echo"),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_id: "echo".to_string(),
                record_revision: 1,
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: Some(description_ref),
                allowed_tools: Some(vec!["echo".to_string()]),
                execution: RemoteMcpExecution::Provider,
                exposure: RemoteMcpExposure::Inject,
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: Some(true),
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }];
        request.tool_choice = Some(ToolChoice::Auto);
        request.output_limit = Some(1024);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["tools"],
            json!([{
                "type": "mcp",
                "server_label": "echo",
                "server_url": "https://echo.example.com/mcp",
                "server_description": "Echo test MCP server",
                "allowed_tools": ["echo"],
                "require_approval": "never",
                "defer_loading": true
            }])
        );
        assert_eq!(value["tool_choice"], json!("auto"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_writes_always_approval_policy_explicitly() {
        let blobs = InMemoryBlobStore::new();
        let mut tool = auth_remote_mcp_tool();
        let ToolKind::RemoteMcp(remote) = &mut tool.kind else {
            unreachable!("fixture is remote MCP")
        };
        remote.approval = RemoteMcpApprovalPolicy::Always;
        let mut request = intent_request(Vec::new());
        request.tools = vec![tool];

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(value["tools"][0]["require_approval"], json!("always"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_replays_typed_mcp_approval_response() {
        let blobs = InMemoryBlobStore::new();
        let response = json!({
            "type": "mcp_approval_response",
            "approval_request_id": "mcpr_1",
            "approve": true
        });
        let response_ref = text_blob(&blobs, &response.to_string()).await;
        let entry = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::McpApprovalResponse {
                approval_request_id: "mcpr_1".to_owned(),
                approve: true,
            },
            source: ContextEntrySource::ApprovalDecision {
                run_id: RunId::new(1),
                approval_id: engine::ApprovalId::try_new("approval_1").expect("approval id"),
            },
            content: engine::ContentRef {
                content_ref: response_ref,
                media_type: Some(MEDIA_TYPE_JSON.to_owned()),
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

        assert_eq!(value["input"], json!([response]));
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

    fn completed_message_response() -> ApiResponse<oai::Response> {
        let raw_json = json!({
            "id": "resp_mcp_auth",
            "status": "completed",
            "output": [
                {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Done." }]
                }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
        });
        ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        }
    }

    fn fake_api_with(response: ApiResponse<oai::Response>) -> Arc<FakeOpenAiResponsesApi> {
        Arc::new(FakeOpenAiResponsesApi {
            response,
            compact_response: ApiResponse {
                parsed: oai::CompactResponse::default(),
                raw_json: json!({ "id": "compact_empty", "output": [] }),
                status: 200,
                headers: HeaderSnapshot::default(),
            },
            seen: Mutex::new(Vec::new()),
            seen_compact: Mutex::new(Vec::new()),
            seen_api_keys: Mutex::new(Vec::new()),
        })
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

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_omits_authorization_for_remote_mcp_auth_ref() {
        let blobs = InMemoryBlobStore::new();
        let mut request = intent_request(Vec::new());
        request.tools = vec![auth_remote_mcp_tool()];
        request.output_limit = Some(1024);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(value["tools"][0]["type"], json!("mcp"));
        assert!(
            value["tools"][0].get("authorization").is_none(),
            "materialized requests must not carry auth values: {value}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_injects_remote_mcp_authorization_and_redacts_persisted_request() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs.clone())
            .with_debug_dumps(true)
            .with_secret_resolver(Arc::new(
                crate::secrets::StaticSecretResolver::new().with_secret(
                    "mcp_server",
                    "echo",
                    "token-xyz",
                ),
            ));

        let execution = LlmGenerationAdapter::generate(&adapter, mcp_auth_generation_request())
            .await
            .expect("generate");

        let sent = api.seen.lock().expect("lock").clone();
        assert_eq!(sent.len(), 1);
        let sent_json = serde_json::to_value(&sent[0]).expect("sent json");
        assert_eq!(sent_json["tools"][0]["authorization"], json!("token-xyz"));

        let dumps = execution.debug_dumps.expect("debug dumps are enabled");
        let stored = crate::blob_io::read_json(blobs.as_ref(), &dumps.provider_request_ref)
            .await
            .expect("stored provider request");
        assert_eq!(stored["tools"][0]["authorization"], json!("<redacted>"));
        assert!(
            !serde_json::to_string(&stored)
                .expect("stored string")
                .contains("token-xyz"),
            "persisted provider request must not contain the resolved token"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_omits_optional_unbound_remote_mcp_authorization() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs)
            .with_secret_resolver(Arc::new(crate::secrets::AbsentSecretResolver));

        LlmGenerationAdapter::generate(&adapter, optional_unbound_mcp_generation_request())
            .await
            .expect("optional unbound MCP auth should be omitted");

        let sent = api.seen.lock().expect("lock").clone();
        let sent_json = serde_json::to_value(&sent[0]).expect("sent json");
        assert!(sent_json["tools"][0].get("authorization").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_does_not_treat_a_missing_optional_mcp_server_as_unbound() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs)
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
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs.clone());

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

    struct FailingProviderKeys;

    #[async_trait]
    impl crate::provider_keys::ModelProviderResolver for FailingProviderKeys {
        async fn resolve_model_provider(
            &self,
            provider_id: &str,
        ) -> Result<
            Option<crate::provider_keys::ResolvedModelProvider>,
            crate::provider_keys::ProviderKeyError,
        > {
            Err(crate::provider_keys::ProviderKeyError::NotUsable {
                provider_id: provider_id.to_owned(),
                message: "provider key is disabled".to_owned(),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_passes_stored_provider_key_to_the_client() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(
                crate::provider_keys::StaticProviderKeys::new().with_key("openai", "stored-key"),
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
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(
                crate::provider_keys::StaticProviderKeys::new()
                    .with_bearer("openai", "oauth-token"),
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
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs);

        LlmGenerationAdapter::generate(&adapter, generation_request())
            .await
            .expect("generate");

        assert_eq!(api.seen_api_keys.lock().expect("lock").clone(), vec![None]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_fails_before_provider_io_when_stored_key_is_not_usable() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let api = fake_api_with(completed_message_response());
        let adapter = OpenAiResponsesLlmAdapter::new(api.clone(), blobs)
            .with_provider_key_resolver(Arc::new(FailingProviderKeys));

        let error = LlmGenerationAdapter::generate(&adapter, generation_request())
            .await
            .expect_err("unusable stored key must fail generation");

        assert!(matches!(
            error,
            LlmAdapterError::ProviderKeyResolution { .. }
        ));
        assert!(
            api.seen.lock().expect("lock").is_empty(),
            "no provider call may happen when stored key resolution fails"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn catalog_and_inserted_skill_text_use_their_ordinary_message_roles() {
        let blobs = InMemoryBlobStore::new();
        let catalog_ref = text_blob(
            &blobs,
            "Read /skills/deploy-review/SKILL.md to review deployment risk.\n",
        )
        .await;
        let input_ref = text_blob(&blobs, "Review this rollout.").await;
        let skill_text_ref = text_blob(
            &blobs,
            "# Deploy Review\n\nCheck rollout scope, blast radius, and rollback plan.",
        )
        .await;

        let catalog_item = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::Catalog {
                title: "VFS skill catalog".to_owned(),
            },
            source: ContextEntrySource::Runtime {
                label: "runtime.catalog.skills.vfs".to_string(),
            },
            content: engine::ContentRef {
                content_ref: catalog_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };
        let user_item = ContextEntry {
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
        let skill_text_item = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(3),
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
        let request = intent_request(vec![catalog_item, user_item, skill_text_item]);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["input"],
            json!([
                {
                    "role": "developer",
                    "content": "VFS skill catalog:\n\nRead /skills/deploy-review/SKILL.md to review deployment risk.\n"
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "Review this rollout."},
                        {"type": "input_text", "text": "# Deploy Review\n\nCheck rollout scope, blast radius, and rollback plan."}
                    ]
                }
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn llm_runtime_returns_generation_result_for_openai_response() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let input_ref = text_blob(&blobs, "Use the tool").await;
        let context = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
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
        let raw_json = json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "I'll inspect it." }]
                },
                {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"Cargo.toml\"}"
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15,
                "output_tokens_details": { "reasoning_tokens": 2 }
            }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let api = Arc::new(FakeOpenAiResponsesApi {
            response,
            compact_response: ApiResponse {
                parsed: oai::CompactResponse::default(),
                raw_json: json!({ "id": "compact_empty", "output": [] }),
                status: 200,
                headers: HeaderSnapshot::default(),
            },
            seen: Mutex::new(Vec::new()),
            seen_compact: Mutex::new(Vec::new()),
            seen_api_keys: Mutex::new(Vec::new()),
        });
        let adapter = Arc::new(OpenAiResponsesLlmAdapter::new(api.clone(), blobs.clone()));
        let registry = LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, adapter);
        let executor = LlmRuntime::new(registry);
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: {
                let mut request = intent_request(vec![context]);
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
        assert_eq!(result.facts.provider_response_id.as_deref(), Some("resp_1"));
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
        assert_eq!(
            api_projection::project_content_text(&blobs, &result.context_entries[0].content)
                .await
                .expect("project message")
                .expect("assistant text"),
            "I'll inspect it."
        );
        let retained_entries = result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(index, item))
            .collect::<Vec<_>>();
        let mut followup_request = intent_request(retained_entries);
        followup_request.output_limit = Some(256);
        let followup = materialize_create_request(blobs.as_ref(), &followup_request)
            .await
            .expect("followup request");
        let followup_json = serde_json::to_value(followup).expect("followup json");
        assert_eq!(
            followup_json["input"],
            json!([
                { "id": "msg_1", "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "I'll inspect it." }] },
                {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"Cargo.toml\"}"
                }
            ])
        );
        assert_eq!(api.seen.lock().expect("lock").len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn llm_runtime_runs_openai_response_compaction() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let input_ref = text_blob(&blobs, "Summarize the prior work.").await;
        let context = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
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
        let raw_json = json!({
            "id": "cmp_resp_1",
            "output": [{
                "id": "cmp_1",
                "type": "compaction",
                "encrypted_content": "opaque"
            }]
        });
        let api = Arc::new(FakeOpenAiResponsesApi {
            response: ApiResponse {
                parsed: oai::Response::default(),
                raw_json: json!({ "id": "unused", "output": [] }),
                status: 200,
                headers: HeaderSnapshot::default(),
            },
            compact_response: ApiResponse {
                parsed: serde_json::from_value(raw_json.clone()).expect("compact response"),
                raw_json,
                status: 200,
                headers: HeaderSnapshot::default(),
            },
            seen: Mutex::new(Vec::new()),
            seen_compact: Mutex::new(Vec::new()),
            seen_api_keys: Mutex::new(Vec::new()),
        });
        let adapter = Arc::new(OpenAiResponsesLlmAdapter::new(api.clone(), blobs.clone()));
        let registry = LlmAdapterRegistry::new()
            .with_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter);
        let executor = LlmRuntime::new(registry);
        let request = ContextCompactionRequest {
            session_id: SessionId::new("session-a"),
            request: ContextCompactionTask {
                model: model(),
                request_fingerprint: "sha256:compact".to_string(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 7,
                    entries: vec![context],
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
        assert!(matches!(entry.kind, ContextEntryKind::ProviderOpaque));
        assert_eq!(
            entry.content.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
        assert_eq!(
            read_json(blobs.as_ref(), &entry.content.content_ref)
                .await
                .expect("native item")["id"],
            "cmp_1"
        );
        assert_eq!(
            crate::blob_io::read_json(blobs.as_ref(), &entry.content.content_ref)
                .await
                .expect("blob")["encrypted_content"],
            json!("opaque")
        );
        let seen = api.seen_compact.lock().expect("seen compact");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            serde_json::to_value(&seen[0]).expect("request json"),
            json!({
                "model": "gpt-5.1",
                "input": [{ "role": "user", "content": "Summarize the prior work." }]
            })
        );
    }

    /// An incomplete response stopped for `content_filter` fails the turn
    /// like a provider refusal instead of completing with partial output.
    /// An incomplete response stopped for `max_output_tokens` fails the turn
    /// but keeps the partial text; the dangling function call is dropped.
    #[tokio::test(flavor = "current_thread")]
    async fn max_output_tokens_stop_fails_the_turn_but_keeps_partial_text() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "resp_cut",
            "object": "response",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "incomplete",
                    "content": [{ "type": "output_text", "text": "The bicycle was", "annotations": [] }]
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"Cargo.toml\"}"
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 32,
                "total_tokens": 42,
                "output_tokens_details": { "reasoning_tokens": 8 }
            }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: {
                let mut request = intent_request(Vec::new());
                request.output_limit = Some(32);
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
        assert_eq!(
            result.context_entries[0].preview.as_deref(),
            Some("The bicycle was")
        );
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(
            failure.contains("cut off at max output tokens 32 after 32 output tokens (8 thinking)"),
            "{failure}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn content_filter_stop_fails_the_turn() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "resp_filtered",
            "object": "response",
            "status": "incomplete",
            "incomplete_details": { "reason": "content_filter" },
            "output": [{
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "incomplete",
                "content": [{ "type": "output_text", "text": "partial", "annotations": [] }]
            }],
            "usage": { "input_tokens": 10, "output_tokens": 1, "total_tokens": 11 }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(4),
            turn_id: TurnId::new(9),
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
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(failure.contains("run_id=4"), "{failure}");
        assert!(failure.contains("turn_id=9"), "{failure}");
        assert!(
            failure.contains("stopped response resp_filtered for content_filter"),
            "{failure}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_preserves_reasoning_items_without_visible_summary() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {
                    "id": "rs_1",
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "opaque"
                },
                {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"Cargo.toml\"}"
                }
            ]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
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

        assert_eq!(result.context_entries.len(), 2);
        assert!(matches!(
            result.context_entries[0].kind,
            ContextEntryKind::ReasoningState
        ));
        let retained_entries = result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(index, item))
            .collect::<Vec<_>>();
        let followup_request = intent_request(retained_entries);
        let followup = materialize_create_request(&blobs, &followup_request)
            .await
            .expect("followup request");
        let followup_json = serde_json::to_value(followup).expect("followup json");
        assert_eq!(followup_json["input"][0]["type"], "reasoning");
        assert_eq!(followup_json["input"][0]["id"], "rs_1");
        assert_eq!(followup_json["input"][1]["type"], "function_call");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_captures_compaction_output_as_provider_opaque_context() {
        let blobs = InMemoryBlobStore::new();
        let raw_item = json!({
            "id": "cmp_1",
            "type": "compaction",
            "encrypted_content": "opaque"
        });
        let raw_json = json!({
            "id": "resp_1",
            "status": "completed",
            "output": [raw_item.clone()]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
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

        assert_eq!(result.context_entries.len(), 1);
        let entry = &result.context_entries[0];
        assert!(matches!(entry.kind, ContextEntryKind::ProviderOpaque));
        assert_eq!(entry.content.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        assert_eq!(
            entry.content.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
        let retained: Value = read_json(&blobs, &entry.content.content_ref)
            .await
            .expect("raw item");
        assert_eq!(retained, raw_item);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_captures_web_search_call_as_provider_opaque_context() {
        let blobs = InMemoryBlobStore::new();
        let raw_item = json!({
            "id": "ws_1",
            "type": "web_search_call",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "Lightspeed web search",
                "sources": [{
                    "url": "https://example.com/source",
                    "title": "Example"
                }]
            }
        });
        let raw_json = json!({
            "id": "resp_1",
            "status": "completed",
            "output": [raw_item.clone()]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
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

        assert_eq!(result.context_entries.len(), 1);
        let entry = &result.context_entries[0];
        assert!(matches!(entry.kind, ContextEntryKind::ProviderOpaque));
        assert_eq!(entry.content.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        assert_eq!(
            entry.content.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND)
        );
        let retained: Value = read_json(&blobs, &entry.content.content_ref)
            .await
            .expect("raw item");
        assert_eq!(retained, raw_item);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_is_one_native_item_which_replays() {
        let blobs = InMemoryBlobStore::new();
        let raw_item = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "OpenAI has citations.",
                "annotations": [{
                    "type": "url_citation",
                    "start_index": 11,
                    "end_index": 20,
                    "url": "https://example.com/source",
                    "title": "Example source"
                }]
            }]
        });
        let raw_json = json!({
            "id": "resp_1",
            "status": "completed",
            "output": [raw_item.clone()]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
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

        assert_eq!(result.context_entries.len(), 1);
        let message = &result.context_entries[0];
        assert!(matches!(
            message.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ));
        assert_eq!(message.preview.as_deref(), Some("OpenAI has citations."));
        assert_eq!(
            read_json(&blobs, &message.content.content_ref)
                .await
                .expect("native message"),
            raw_item
        );

        let retained = result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| retained_context_entry(index, entry))
            .collect::<Vec<_>>();
        let input = materialize_input_items(&blobs, &retained)
            .await
            .expect("materialize cited output");
        assert_eq!(
            serde_json::to_value(input).expect("input JSON"),
            json!([raw_item])
        );

        // A truncated turn strips native metadata and retains only safe text.
        let partial = partial_output_entries(&blobs, result.context_entries)
            .await
            .expect("partial output");
        let entry = retained_context_entry(0, &partial[0]);
        let input = materialize_input_items(&blobs, &[entry])
            .await
            .expect("partial replay");
        let input = serde_json::to_value(input).expect("input JSON");
        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"], "OpenAI has citations.");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn result_captures_mcp_outputs_as_provider_opaque_context() {
        let blobs = InMemoryBlobStore::new();
        let list_item = json!({
            "id": "mcpl_1",
            "type": "mcp_list_tools",
            "server_label": "echo",
            "tools": [{
                "name": "echo",
                "description": "Echo input"
            }]
        });
        let call_item = json!({
            "id": "mcp_1",
            "type": "mcp_call",
            "approval_request_id": null,
            "arguments": "{\"data\":\"LIGHTSPEED-MCP-ECHO\"}",
            "error": null,
            "name": "echo",
            "output": "{\"data\":\"LIGHTSPEED-MCP-ECHO\"}",
            "server_label": "echo"
        });
        let approval_item = json!({
            "id": "mcpr_1",
            "type": "mcp_approval_request",
            "arguments": "{\"data\":\"LIGHTSPEED-MCP-ECHO\"}",
            "name": "echo",
            "server_label": "echo"
        });
        let raw_json = json!({
            "id": "resp_1",
            "status": "completed",
            "output": [list_item.clone(), call_item.clone(), approval_item.clone()]
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let mut intent = intent_request(Vec::new());
        intent.tools = vec![auth_remote_mcp_tool()];
        let request = LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: intent,
        };

        let result = result_from_response(&blobs, &request, &response)
            .await
            .expect("result");

        assert_eq!(result.context_entries.len(), 3);
        assert!(result.facts.tool_calls.is_empty());
        assert_eq!(result.facts.approval_requests.len(), 1);
        assert_eq!(
            result.facts.approval_requests[0].continuation,
            ApprovalContinuation::OpenAiMcp {
                provider_request_id: "mcpr_1".to_owned()
            }
        );
        assert!(matches!(
            &result.facts.approval_requests[0].subject,
            ApprovalSubject::McpToolCall {
                server_id,
                server_label,
                tool_name,
                ..
            } if server_id == "echo" && server_label == "echo" && tool_name == "echo"
        ));
        for entry in &result.context_entries {
            assert!(matches!(entry.kind, ContextEntryKind::ProviderOpaque));
            assert_eq!(entry.content.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        }
        assert_eq!(
            result.context_entries[0].content.provider_kind.as_deref(),
            Some(engine::OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[1].content.provider_kind.as_deref(),
            Some(engine::OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[2].content.provider_kind.as_deref(),
            Some(engine::OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[1].preview.as_deref(),
            Some("OpenAI Responses MCP tool call: echo.echo")
        );
        let retained: Value = read_json(&blobs, &result.context_entries[1].content.content_ref)
            .await
            .expect("raw MCP call");
        assert_eq!(retained, call_item);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_provider_response_records_failure_message() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "resp_failed",
            "status": "failed",
            "error": {
                "code": "invalid_model",
                "message": "The requested model is unavailable.",
                "type": "invalid_request_error"
            },
            "output": []
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
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

        assert_eq!(result.status, LlmGenerationStatus::Failed);
        assert_eq!(result.facts.finish, LlmFinish::Failed);
        let failure = blobs
            .read_text(&result.failure_ref.expect("failure ref"))
            .await
            .expect("failure text");
        assert!(failure.contains("invalid_request_error"));
        assert!(failure.contains("The requested model is unavailable."));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consecutive_user_entries_fold_into_one_message_with_parts() {
        let blobs = InMemoryBlobStore::new();
        let image_ref = blobs
            .put_bytes(vec![0x89, 0x50, 0x4e, 0x47])
            .await
            .expect("store image");
        let caption_ref = blobs.insert_text("what is in this picture?").await;

        let entry = |id: u64, content_ref: BlobRef, media_type: Option<&str>| ContextEntry {
            entry_id: ContextEntryId::new(id),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: id as u32 - 1,
            },
            content: engine::ContentRef {
                content_ref,
                media_type: media_type.map(str::to_owned),
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };
        let entries = vec![
            entry(1, image_ref, Some("image/png")),
            entry(2, caption_ref, None),
        ];

        let input = materialize_input_items(&blobs, &entries)
            .await
            .expect("materialize entries");

        assert_eq!(input.len(), 1, "expected one folded message, got {input:?}");
        let value = serde_json::to_value(&input[0]).expect("serialize message");
        assert_eq!(value["role"], json!("user"));
        assert_eq!(value["content"][0]["type"], json!("input_text"));
        assert!(
            value["content"][0]["text"]
                .as_str()
                .expect("announcement")
                .starts_with("[image · media:")
        );
        assert_eq!(value["content"][1]["type"], json!("input_image"));
        assert_eq!(value["content"][2]["type"], json!("input_text"));
        assert_eq!(
            value["content"][2]["text"],
            json!("what is in this picture?")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pdf_document_entry_materializes_as_input_file_part() {
        let blobs = InMemoryBlobStore::new();
        let pdf_ref = blobs
            .put_bytes(b"%PDF-1.4 fake".to_vec())
            .await
            .expect("store pdf");

        let entries = vec![ContextEntry {
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
                content_ref: pdf_ref,
                media_type: Some("application/pdf".to_owned()),
                provider_kind: None,
            },
            preview: Some("[document: offer.pdf]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }];

        let input = materialize_input_items(&blobs, &entries)
            .await
            .expect("materialize entries");

        let value = serde_json::to_value(&input[0]).expect("serialize message");
        assert_eq!(value["content"][0]["type"], json!("input_text"));
        assert_eq!(
            value["content"][0]["text"],
            json!(format!(
                "[document: offer.pdf · {} · application/pdf]",
                engine::media::media_handle(&BlobRef::from_bytes(b"%PDF-1.4 fake"))
            ))
        );
        assert_eq!(value["content"][1]["type"], json!("input_file"));
        assert_eq!(value["content"][1]["filename"], json!("offer.pdf"));
        let file_data = value["content"][1]["file_data"]
            .as_str()
            .expect("file data");
        assert!(file_data.starts_with("data:application/pdf;base64,"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn markdown_document_entry_inlines_text_with_header() {
        let blobs = InMemoryBlobStore::new();
        let doc_ref = blobs.insert_text("# Notes\nhello").await;

        let entries = vec![ContextEntry {
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
                content_ref: doc_ref,
                media_type: Some("text/markdown".to_owned()),
                provider_kind: None,
            },
            preview: Some("[document: notes.md]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }];

        let input = materialize_input_items(&blobs, &entries)
            .await
            .expect("materialize entries");

        let value = serde_json::to_value(&input[0]).expect("serialize message");
        assert_eq!(value["content"][0]["type"], json!("input_text"));
        assert_eq!(
            value["content"][0]["text"],
            json!("[document: notes.md]\n# Notes\nhello")
        );
    }

    /// A model-authored refusal part is the visible answer (as with Chat
    /// Completions' `refusal` field); it must not render as an empty turn.
    #[tokio::test(flavor = "current_thread")]
    async fn refusal_content_part_renders_as_the_assistant_message() {
        let blobs = InMemoryBlobStore::new();
        let raw_json = json!({
            "id": "resp_refusal",
            "object": "response",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "refusal", "refusal": "I can't help with that." }]
            }],
            "usage": { "input_tokens": 10, "output_tokens": 6, "total_tokens": 16 }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
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

        assert_eq!(result.status, LlmGenerationStatus::Succeeded);
        assert_eq!(result.facts.finish, LlmFinish::Stop);
        assert_eq!(result.context_entries.len(), 1);
        let entry = &result.context_entries[0];
        assert!(matches!(
            entry.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ));
        assert_eq!(entry.preview.as_deref(), Some("I can't help with that."));
        assert_eq!(
            entry.content.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND)
        );
        assert_eq!(
            api_projection::project_content_text(&blobs, &entry.content)
                .await
                .expect("project refusal")
                .as_deref(),
            Some("I can't help with that.")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_trailing_message_item_does_not_duplicate_assistant_text() {
        // gpt-5.5 sometimes emits a second message item with an empty text
        // part; the whole-response output_text fallback must not turn it
        // into a duplicate assistant entry.
        let raw_json = json!({
            "id": "resp_double",
            "status": "completed",
            "output": [
                {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Hi — what can I help with?" }]
                },
                {
                    "id": "msg_2",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "" }]
                }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
        });
        let response = ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("response"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let blobs = InMemoryBlobStore::new();
        let request = generation_request_fixture();

        let result = result_from_response(&blobs, &request, &response)
            .await
            .expect("result");

        let assistant_entries: Vec<_> = result
            .context_entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::Assistant,
                    }
                )
            })
            .collect();
        assert_eq!(
            assistant_entries.len(),
            1,
            "empty message item must not duplicate the assistant text"
        );
        assert_eq!(
            assistant_entries[0].preview.as_deref(),
            Some("Hi — what can I help with?")
        );
    }

    fn generation_request_fixture() -> LlmGenerationRequest {
        LlmGenerationRequest {
            session_id: SessionId::new("session-test"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: LlmRequest {
                model: model(),
                request_fingerprint: "test".to_string(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 0,
                    entries: Vec::new(),
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
            },
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consecutive_assistant_entries_do_not_fold_into_parts() {
        let blobs = InMemoryBlobStore::new();
        let first_ref = blobs.insert_text("first assistant message").await;
        let second_ref = blobs.insert_text("second assistant message").await;

        let entry = |id: u64, content_ref: BlobRef| ContextEntry {
            entry_id: ContextEntryId::new(id),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            },
            source: ContextEntrySource::AssistantOutput {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
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
        };
        let entries = vec![entry(1, first_ref), entry(2, second_ref)];

        let input = materialize_input_items(&blobs, &entries)
            .await
            .expect("materialize entries");

        // Assistant input items must stay as plain text messages: folding
        // them into parts would produce `input_text` under an assistant
        // role, which the Responses API rejects.
        assert_eq!(input.len(), 2, "assistant messages must not fold");
        for item in &input {
            let value = serde_json::to_value(item).expect("serialize");
            assert_eq!(value["role"], json!("assistant"));
            assert!(value["content"].is_string(), "expected plain text content");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn image_message_entry_materializes_as_input_image_part() {
        let blobs = InMemoryBlobStore::new();
        let image_bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
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
                media_type: Some("image/png".to_owned()),
                provider_kind: None,
            },
            preview: Some("[image: photo.png]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let item = materialize_input_item(&blobs, &entry)
            .await
            .expect("materialize image entry");

        let value = serde_json::to_value(&item).expect("serialize input item");
        assert_eq!(value["role"], json!("user"));
        assert_eq!(value["content"][0]["type"], json!("input_text"));
        assert_eq!(
            value["content"][0]["text"],
            json!(format!(
                "[image: photo.png · {} · image/png]",
                engine::media::media_handle(&entry.content.content_ref)
            ))
        );
        assert_eq!(value["content"][1]["type"], json!("input_image"));
        let url = value["content"][1]["image_url"]
            .as_str()
            .expect("image url");
        assert!(url.starts_with("data:image/png;base64,"));
        use base64::Engine as _;
        let encoded = url.strip_prefix("data:image/png;base64,").expect("prefix");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(decoded, image_bytes);
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

    /// A catalog update must append, never rewrite: every input item before
    /// it is byte-identical, and only the successor carries the update
    /// header. This is what keeps OpenAI's automatic prefix cache warm on
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

        let before_items = serde_json::to_value(&before.input)
            .expect("json")
            .as_array()
            .cloned()
            .expect("items");
        let after_items = serde_json::to_value(&after.input)
            .expect("json")
            .as_array()
            .cloned()
            .expect("items");
        assert_eq!(after_items.len(), before_items.len() + 1);
        assert_eq!(&after_items[..before_items.len()], &before_items[..]);
        let successor =
            serde_json::to_string(after_items.last().expect("successor")).expect("json");
        assert!(successor.contains(crate::catalog_prompts::CATALOG_UPDATE_HEADER));
        assert!(successor.contains("Bot directory:"));
        assert!(
            !serde_json::to_string(&before_items[0])
                .expect("json")
                .contains("Updated catalog")
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
    async fn tool_media_entries_follow_the_function_output_as_user_parts() {
        let blobs = InMemoryBlobStore::new();
        let entries = tool_result_with_media(&blobs).await;
        let input = materialize_input_items(&blobs, &entries)
            .await
            .expect("materialize");
        assert_eq!(
            input.len(),
            2,
            "function output then one user message: {input:?}"
        );
        let output = serde_json::to_value(&input[0]).expect("json");
        assert_eq!(output["type"], json!("function_call_output"));
        assert_eq!(output["call_id"], json!("call_1"));
        let user = serde_json::to_value(&input[1]).expect("json");
        assert_eq!(user["role"], json!("user"));
        let parts = user["content"].as_array().expect("parts");
        let kinds = parts
            .iter()
            .map(|part| part["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec!["input_text", "input_image", "input_text", "input_file"]
        );
        assert!(
            parts[0]["text"]
                .as_str()
                .expect("text")
                .starts_with("[image · media:")
        );
        assert_eq!(parts[3]["filename"], json!("report.pdf"));
    }
}
