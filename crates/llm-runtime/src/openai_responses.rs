use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    BlobRef, CompactionPolicy, ContextCompactionRequest, ContextCompactionResult,
    ContextCompactionStatus, ContextCompactionTask, ContextEntry, ContextEntryInput,
    ContextEntryKind, ContextMessageRole, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, LlmRequest, LlmUsage,
    OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND, OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND,
    OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND, OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND,
    OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND, ObservedToolCall, ProviderApiKind,
    ProviderNativeToolExecution, RemoteMcpApprovalPolicy, RemoteMcpToolSpec, SkillId, TokenEstimate,
    TokenEstimateQuality, ToolCallId, ToolChoice, ToolChoiceMode, ToolKind, ToolName, ToolSpec,
    storage::BlobStore,
};
use llm_clients::{ApiResponse, openai::responses as oai};
use serde_json::{Value, json};
use tools::skills::{SkillCatalogSnapshot, SkillLocation, SkillMetadata};

use crate::{
    blob_io::{put_json, put_text, read_json, read_text},
    error::{LlmAdapterError, LlmAdapterResult},
    executor::{LlmCompactionAdapter, LlmGenerationAdapter},
    params::openai_responses_params,
    result::LlmGenerationExecution,
    secrets::{REDACTED_SECRET_PLACEHOLDER, SecretResolver, UnconfiguredSecretResolver},
};

const PROVIDER_KIND_MESSAGE: &str = "openai.responses.message";
const PROVIDER_KIND_FUNCTION_CALL: &str = "openai.responses.function_call";
const MEDIA_TYPE_JSON: &str = "application/json";
const MEDIA_TYPE_TEXT: &str = "text/plain";

#[async_trait]
pub trait OpenAiResponsesApi: Send + Sync {
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError>;

    async fn compact(
        &self,
        request: oai::CompactResponseRequest,
    ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError>;
}

#[async_trait]
impl OpenAiResponsesApi for oai::Client {
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
        oai::Client::create(self, request).await
    }

    async fn compact(
        &self,
        request: oai::CompactResponseRequest,
    ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError> {
        oai::Client::compact(self, request).await
    }
}

#[derive(Clone)]
pub struct OpenAiResponsesLlmAdapter {
    client: Arc<dyn OpenAiResponsesApi>,
    blobs: Arc<dyn BlobStore>,
    secrets: Arc<dyn SecretResolver>,
}

impl OpenAiResponsesLlmAdapter {
    pub fn new(client: Arc<dyn OpenAiResponsesApi>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            client,
            blobs,
            secrets: Arc::new(UnconfiguredSecretResolver),
        }
    }

    pub fn with_secret_resolver(mut self, secrets: Arc<dyn SecretResolver>) -> Self {
        self.secrets = secrets;
        self
    }

    pub async fn materialize_create_request(
        &self,
        request: &LlmRequest,
    ) -> LlmAdapterResult<oai::CreateResponseRequest> {
        materialize_create_request(self.blobs.as_ref(), request).await
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

        let provider_request = self.materialize_create_request(&request.request).await?;
        let (send_request, redacted_request) =
            inject_remote_mcp_auth(self.secrets.as_ref(), &request.request, provider_request)
                .await?;
        let provider_request_ref = put_json(self.blobs.as_ref(), &redacted_request).await?;
        let response = self.client.create(send_request).await?;
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
        let _provider_request_ref = put_json(self.blobs.as_ref(), &provider_request).await?;
        let response = self.client.compact(provider_request).await?;
        let _raw_response_ref = put_json(self.blobs.as_ref(), &response.raw_json).await?;
        result_from_compact_response(self.blobs.as_ref(), &request, &response).await
    }
}

pub async fn materialize_create_request(
    blobs: &dyn BlobStore,
    request: &LlmRequest,
) -> LlmAdapterResult<oai::CreateResponseRequest> {
    let params = openai_responses_params(request.params.as_ref())?;
    let instructions = materialize_instructions(blobs, &request.context.entries).await?;
    let input_entries = request
        .context
        .entries
        .iter()
        .filter(|entry| !matches!(entry.kind, ContextEntryKind::Instructions))
        .cloned()
        .collect::<Vec<_>>();
    let input_items = materialize_input_items(blobs, &input_entries).await?;
    let tools = materialize_tools(blobs, &request.tools).await?;

    let mut extra = params.extra.clone();
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
        tool_choice: request.tool_choice.as_ref().map(openai_tool_choice),
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
        context_management: context_management_from_compaction(request.compaction.as_ref()),
        extra,
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
        Ok(Some(Value::String(parts.join("\n\n"))))
    }
}

async fn materialize_input_items(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
) -> LlmAdapterResult<Vec<oai::ResponseInputItem>> {
    let mut input = Vec::with_capacity(entries.len());
    for item in entries {
        input.push(materialize_input_item(blobs, item).await?);
    }
    Ok(input)
}

async fn materialize_input_item(
    blobs: &dyn BlobStore,
    item: &ContextEntry,
) -> LlmAdapterResult<oai::ResponseInputItem> {
    if is_openai_raw_item(item) {
        return Ok(oai::ResponseInputItem::Raw(
            read_json(blobs, &item.content_ref).await?,
        ));
    }

    match &item.kind {
        ContextEntryKind::Message { role } => {
            let text = read_text(blobs, &item.content_ref).await?;
            Ok(oai::ResponseInputItem::Message(oai::InputMessage {
                role: match role {
                    ContextMessageRole::User => oai::MessageRole::User,
                    ContextMessageRole::Assistant => oai::MessageRole::Assistant,
                },
                content: oai::InputMessageContent::Text(text),
                extra: Default::default(),
            }))
        }
        ContextEntryKind::ToolResult { call_id, .. } => {
            let output = read_text(blobs, &item.content_ref).await?;
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
        ContextEntryKind::SkillCatalog => {
            let catalog = read_skill_catalog(blobs, &item.content_ref).await?;
            Ok(oai::ResponseInputItem::Message(oai::InputMessage {
                role: oai::MessageRole::Developer,
                content: oai::InputMessageContent::Text(openai_skill_catalog_text(&catalog)),
                extra: Default::default(),
            }))
        }
        ContextEntryKind::SkillActivation { skill_id } => {
            let text = read_text(blobs, &item.content_ref).await?;
            Ok(oai::ResponseInputItem::Message(oai::InputMessage {
                role: oai::MessageRole::Developer,
                content: oai::InputMessageContent::Text(openai_skill_activation_text(
                    skill_id, text,
                )),
                extra: Default::default(),
            }))
        }
        ContextEntryKind::ToolCall { .. }
        | ContextEntryKind::ReasoningState
        | ContextEntryKind::ProviderOpaque => Ok(oai::ResponseInputItem::Raw(
            read_json(blobs, &item.content_ref).await?,
        )),
    }
}

fn is_openai_raw_item(item: &ContextEntry) -> bool {
    matches!(
        item.kind,
        ContextEntryKind::ToolCall { .. }
            | ContextEntryKind::ReasoningState
            | ContextEntryKind::ProviderOpaque
    ) && item.media_type.as_deref() == Some(MEDIA_TYPE_JSON)
}

async fn read_skill_catalog(
    blobs: &dyn BlobStore,
    blob_ref: &BlobRef,
) -> LlmAdapterResult<SkillCatalogSnapshot> {
    let bytes = blobs.read_bytes(blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| LlmAdapterError::InvalidJson {
        blob_ref: blob_ref.clone(),
        message: error.to_string(),
    })
}

fn openai_skill_catalog_text(catalog: &SkillCatalogSnapshot) -> String {
    let mut text = String::from("Forge skill catalog:\n\n");
    if catalog.skills.is_empty() {
        text.push_str("No Forge skills are currently available.");
        return text;
    }

    text.push_str(
        "When a skill is relevant, read its SKILL.md through the available file tool before following it.\n\n",
    );
    for skill in &catalog.skills {
        text.push_str(&openai_skill_catalog_entry(skill));
    }
    text
}

fn openai_skill_catalog_entry(skill: &SkillMetadata) -> String {
    let mut entry = format!(
        "- {} ({})\n  description: {}\n  skill_doc_path: {}",
        skill.name,
        skill.skill_id,
        skill.description,
        skill_doc_path(&skill.location)
    );
    if let Some(target) = &skill.target {
        entry.push_str(&format!("\n  target: {}:{}", target.namespace, target.id));
    }
    if let Some(short_description) = &skill.short_description {
        entry.push_str(&format!("\n  short_description: {short_description}"));
    }
    entry.push('\n');
    entry
}

fn skill_doc_path(location: &SkillLocation) -> &str {
    match location {
        SkillLocation::MountedSnapshot { skill_doc_path, .. }
        | SkillLocation::MountedWorkspace { skill_doc_path, .. } => skill_doc_path.as_str(),
        SkillLocation::HostFilesystem { skill_doc_path, .. } => skill_doc_path,
    }
}

fn openai_skill_activation_text(skill_id: &SkillId, text: String) -> String {
    format!("Forge loaded skill ({skill_id}):\n\n{text}")
}

async fn materialize_tools(
    blobs: &dyn BlobStore,
    tools: &[ToolSpec],
) -> LlmAdapterResult<Vec<oai::Tool>> {
    let mut materialized = Vec::with_capacity(tools.len());
    for tool in tools {
        materialized.push(materialize_tool(blobs, tool).await?);
    }
    Ok(materialized)
}

async fn materialize_tool(blobs: &dyn BlobStore, tool: &ToolSpec) -> LlmAdapterResult<oai::Tool> {
    match &tool.kind {
        ToolKind::Function(function) => {
            let mut materialized = oai::FunctionTool::new(
                function.model_name.as_ref().unwrap_or(&tool.name).as_str(),
                read_json(blobs, &function.input_schema_ref).await?,
            );
            materialized.description = match &function.description_ref {
                Some(blob_ref) => Some(read_text(blobs, blob_ref).await?),
                None => None,
            };
            materialized.strict = function.strict;
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
                    materialized.extra.insert(key.clone(), value.clone());
                }
            }
            Ok(oai::Tool::Function(materialized))
        }
        ToolKind::ProviderNative(native) => {
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
                | ProviderNativeToolExecution::ClientEffect => Ok(oai::Tool::Raw(
                    read_json(blobs, &native.native_tool_ref).await?,
                )),
            }
        }
        ToolKind::RemoteMcp(remote_mcp) => materialize_remote_mcp_tool(blobs, remote_mcp).await,
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
        RemoteMcpApprovalPolicy::ProviderDefault => {}
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
            ToolKind::RemoteMcp(remote_mcp) if remote_mcp.auth_ref.is_some() => {
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
        let token = secrets
            .resolve(auth_ref, Some(remote_mcp.server_url.as_str()))
            .await
            .map_err(|error| LlmAdapterError::SecretResolution {
                tool: tool.name.to_string(),
                message: error.to_string(),
            })?;
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
    match &choice.mode {
        ToolChoiceMode::Auto => oai::ToolChoice::Mode(oai::ToolChoiceMode::Auto),
        ToolChoiceMode::None => oai::ToolChoice::Mode(oai::ToolChoiceMode::None),
        ToolChoiceMode::RequiredAny => oai::ToolChoice::Mode(oai::ToolChoiceMode::Required),
        ToolChoiceMode::Specific { tool_name } => oai::ToolChoice::Function {
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

    for (index, item) in response.parsed.output.iter().enumerate() {
        let raw_item = raw_output_item(&response.raw_json, index, item)?;
        match item.r#type.as_str() {
            "message" => {
                if let Some(context_entry) =
                    assistant_context_entry(blobs, request, item, &response.parsed).await?
                {
                    context_entries.push(context_entry);
                }
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
                context_entries.push(compaction_context_entry(blobs, item, raw_item).await?);
            }
            "web_search_call" => {
                context_entries.push(web_search_call_context_entry(blobs, item, raw_item).await?);
            }
            "mcp_list_tools" | "mcp_call" | "mcp_approval_request" => {
                context_entries.push(mcp_context_entry(blobs, item, raw_item).await?);
            }
            _ => {}
        }
    }

    let status = generation_status(response.parsed.status);
    let failure_ref = if status == LlmGenerationStatus::Failed {
        Some(provider_failure_ref(blobs, &response.parsed).await?)
    } else {
        None
    };
    let usage = response.parsed.usage.as_ref().map(llm_usage);
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status,
        failure_ref,
        context_entries,
        facts: LlmGenerationFacts {
            provider_response_id: Some(response.parsed.id.clone()),
            finish: finish_reason(&response.parsed, !tool_calls.is_empty()),
            usage,
            tool_calls,
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
            context_entries.push(compaction_context_entry(blobs, item, raw_item).await?);
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

async fn assistant_context_entry(
    blobs: &dyn BlobStore,
    _request: &LlmGenerationRequest,
    item: &oai::ResponseOutputItem,
    response: &oai::Response,
) -> LlmAdapterResult<Option<ContextEntryInput>> {
    let text = item
        .content
        .iter()
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>()
        .join("");
    let text = if text.is_empty() {
        response.output_text()
    } else {
        text
    };
    if text.is_empty() {
        return Ok(None);
    }

    let content_ref = put_text(blobs, &text).await?;
    Ok(Some(ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        },
        content_ref,
        media_type: Some(MEDIA_TYPE_TEXT.to_string()),
        preview: Some(text),
        provider_kind: Some(PROVIDER_KIND_MESSAGE.to_string()),
        provider_item_id: item.id.clone(),
        token_estimate: None,
    }))
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
        content_ref: native_call_ref.clone(),
        media_type: Some(MEDIA_TYPE_JSON.to_string()),
        preview: Some(format!("{}({})", call.name, call.arguments)),
        provider_kind: Some(PROVIDER_KIND_FUNCTION_CALL.to_string()),
        provider_item_id: call.item_id.map(ToOwned::to_owned),
        token_estimate: None,
    };
    let tool_call = ObservedToolCall {
        call_id,
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
    let summaries = item
        .summary
        .iter()
        .chain(item.content.iter())
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>();
    let text = summaries.join("\n");
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(Some(ContextEntryInput {
        kind: ContextEntryKind::ReasoningState,
        content_ref,
        media_type: Some(MEDIA_TYPE_JSON.to_string()),
        preview: Some(if text.is_empty() {
            item.id
                .as_deref()
                .map(|id| format!("reasoning state {id}"))
                .unwrap_or_else(|| "reasoning state".to_string())
        } else {
            text
        }),
        provider_kind: Some("openai.responses.reasoning".to_string()),
        provider_item_id: item.id.clone(),
        token_estimate: None,
    }))
}

async fn compaction_context_entry(
    blobs: &dyn BlobStore,
    item: &oai::ResponseOutputItem,
    raw_item: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content_ref,
        media_type: Some(MEDIA_TYPE_JSON.to_string()),
        preview: Some("OpenAI Responses compaction item".to_string()),
        provider_kind: Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND.to_string()),
        provider_item_id: item.id.clone(),
        token_estimate: None,
    })
}

async fn web_search_call_context_entry(
    blobs: &dyn BlobStore,
    item: &oai::ResponseOutputItem,
    raw_item: Value,
) -> LlmAdapterResult<ContextEntryInput> {
    let content_ref = put_json(blobs, &raw_item).await?;
    Ok(ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content_ref,
        media_type: Some(MEDIA_TYPE_JSON.to_string()),
        preview: Some("OpenAI Responses web search call".to_string()),
        provider_kind: Some(OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND.to_string()),
        provider_item_id: item.id.clone(),
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
        content_ref,
        media_type: Some(MEDIA_TYPE_JSON.to_string()),
        preview: Some(mcp_preview(item, &raw_item)),
        provider_kind: Some(provider_kind.to_string()),
        provider_item_id: item.id.clone(),
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
    }
}

fn u64_to_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use engine::{
        ContextCompactionTask, ContextEntryId, ContextEntrySource, ContextSnapshot, CoreAgentLlm,
        FunctionToolSpec, LlmGenerationRequest, LlmRequest, ModelSelection,
        ProviderParams, RunId, SessionId, ToolExecutionTarget, ToolParallelism, TurnId,
        storage::InMemoryBlobStore,
    };
    use llm_clients::HeaderSnapshot;
    use serde_json::json;
    use tools::skills::{
        SKILL_CATALOG_SCHEMA_VERSION, SkillDependencies, SkillScope, SkillSource, SkillTrustLevel,
    };
    use tools::web::search::{
        OpenAiResponsesWebSearchConfig, WebSearchContextSize, WebSearchMode,
        openai_responses_web_search_tool_bundle,
    };

    use super::*;
    use crate::executor::{LlmAdapterRegistry, LlmRuntime};
    use crate::params::{OpenAiReasoningConfig, OpenAiResponsesParams};

    struct FakeOpenAiResponsesApi {
        response: ApiResponse<oai::Response>,
        compact_response: ApiResponse<oai::CompactResponse>,
        seen: Mutex<Vec<oai::CreateResponseRequest>>,
        seen_compact: Mutex<Vec<oai::CompactResponseRequest>>,
    }

    #[async_trait]
    impl OpenAiResponsesApi for FakeOpenAiResponsesApi {
        async fn create(
            &self,
            request: oai::CreateResponseRequest,
        ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
            self.seen.lock().expect("lock").push(request);
            Ok(self.response.clone())
        }

        async fn compact(
            &self,
            request: oai::CompactResponseRequest,
        ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError> {
            self.seen_compact.lock().expect("lock").push(request);
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
            content_ref: item.content_ref.clone(),
            media_type: item.media_type.clone(),
            preview: item.preview.clone(),
            provider_kind: item.provider_kind.clone(),
            provider_item_id: item.provider_item_id.clone(),
            token_estimate: item.token_estimate.clone(),
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
            content_ref: instructions_ref,
            media_type: Some("text/plain".to_owned()),
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
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
            content_ref: input_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        };
        let mut request = intent_request(vec![instructions_item, item]);
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
            disable_parallel_tool_use: None,
        });
        request.output_limit = Some(2048);
        request.provider_response_id = Some("resp_prev".to_string());
        request.compaction = Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens: Some(120_000),
        });
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
            extra: BTreeMap::from([("service_tier".to_string(), json!("flex"))]),
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
        let bundle = openai_responses_web_search_tool_bundle(&OpenAiResponsesWebSearchConfig {
            mode: WebSearchMode::Cached,
            search_context_size: Some(WebSearchContextSize::Low),
            allowed_domains: vec!["docs.rs".to_string()],
            blocked_domains: Vec::new(),
            user_location: None,
            include_sources: true,
        })
        .expect("web search bundle")
        .expect("enabled web search");
        for document in &bundle.documents {
            let stored_ref = crate::blob_io::put_bytes(&blobs, document.blob_bytes())
                .await
                .expect("store native tool");
            assert_eq!(stored_ref, document.blob_ref);
        }
        let mut request = intent_request(Vec::new());
        request.tools = vec![bundle.spec];
        request.tool_choice = Some(ToolChoice {
            mode: ToolChoiceMode::Auto,
            disable_parallel_tool_use: None,
        });
        request.output_limit = Some(1024);
        request.params = Some(openai_params(&OpenAiResponsesParams {
            include: vec![
                crate::params::OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE.to_string(),
            ],
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
            kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
                server_label: "echo".to_string(),
                server_url: "https://echo.example.com/mcp".to_string(),
                description_ref: Some(description_ref),
                allowed_tools: Some(vec!["echo".to_string()]),
                approval: RemoteMcpApprovalPolicy::Never,
                defer_loading: Some(true),
                auth_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: Default::default(),
        }];
        request.tool_choice = Some(ToolChoice {
            mode: ToolChoiceMode::Auto,
            disable_parallel_tool_use: None,
        });
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

    fn auth_remote_mcp_tool() -> ToolSpec {
        ToolSpec {
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
            .with_secret_resolver(Arc::new(
                crate::secrets::StaticSecretResolver::new().with_secret(
                    "auth_grant",
                    "grant_123",
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

        let stored = crate::blob_io::read_json(blobs.as_ref(), &execution.provider_request_ref)
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

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_create_request_maps_skill_context_as_developer_messages() {
        let blobs = InMemoryBlobStore::new();
        let target = ToolExecutionTarget::new("host", "vm-1");
        let skill_id = SkillId::new("skill:deploy-review");
        let catalog_ref = crate::blob_io::put_json(
            &blobs,
            &SkillCatalogSnapshot {
                schema_version: SKILL_CATALOG_SCHEMA_VERSION.to_string(),
                target: Some(target.clone()),
                skills: vec![SkillMetadata {
                    skill_id: skill_id.clone(),
                    name: "deploy-review".to_string(),
                    description: "Review deployment risk.".to_string(),
                    short_description: None,
                    source: SkillSource::HostPath {
                        root_id: "host".to_string(),
                        target: target.clone(),
                    },
                    scope: SkillScope::Target,
                    target: Some(target.clone()),
                    enabled: true,
                    trust: SkillTrustLevel::Host,
                    interface: None,
                    dependencies: SkillDependencies::default(),
                    location: SkillLocation::HostFilesystem {
                        target,
                        root_path: "/skills".to_string(),
                        skill_dir_path: "/skills/deploy-review".to_string(),
                        skill_doc_path: "/skills/deploy-review/SKILL.md".to_string(),
                    },
                    skill_doc_ref: None,
                }],
                warnings: Vec::new(),
            },
        )
        .await
        .expect("catalog");
        let input_ref = text_blob(&blobs, "Review this rollout.").await;
        let activation_ref = text_blob(
            &blobs,
            "# Deploy Review\n\nCheck rollout scope, blast radius, and rollback plan.",
        )
        .await;

        let catalog_item = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::SkillCatalog,
            source: ContextEntrySource::Runtime {
                label: "skills.catalog".to_string(),
            },
            content_ref: catalog_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
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
            content_ref: input_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        };
        let activation_item = ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(3),
            kind: ContextEntryKind::SkillActivation {
                skill_id: skill_id.clone(),
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
        let request = intent_request(vec![catalog_item, user_item, activation_item]);

        let materialized = materialize_create_request(&blobs, &request)
            .await
            .expect("materialize");
        let value = serde_json::to_value(materialized).expect("json");

        assert_eq!(
            value["input"],
            json!([
                {
                    "role": "developer",
                    "content": "Forge skill catalog:\n\nWhen a skill is relevant, read its SKILL.md through the available file tool before following it.\n\n- deploy-review (skill:deploy-review)\n  description: Review deployment risk.\n  skill_doc_path: /skills/deploy-review/SKILL.md\n  target: host:vm-1\n"
                },
                { "role": "user", "content": "Review this rollout." },
                {
                    "role": "developer",
                    "content": "Forge loaded skill (skill:deploy-review):\n\n# Deploy Review\n\nCheck rollout scope, blast radius, and rollback plan."
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
            content_ref: input_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
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
        assert_eq!(
            blobs
                .read_text(&result.context_entries[0].content_ref)
                .await
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
                { "role": "assistant", "content": "I'll inspect it." },
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
            content_ref: input_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
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
            entry.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
        assert_eq!(entry.provider_item_id.as_deref(), Some("cmp_1"));
        assert_eq!(
            crate::blob_io::read_json(blobs.as_ref(), &entry.content_ref)
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
        assert_eq!(entry.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        assert_eq!(
            entry.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
        assert_eq!(entry.provider_item_id.as_deref(), Some("cmp_1"));
        let retained: Value = read_json(&blobs, &entry.content_ref)
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
                "query": "Forge P66 web search",
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
        assert_eq!(entry.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        assert_eq!(
            entry.provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND)
        );
        assert_eq!(entry.provider_item_id.as_deref(), Some("ws_1"));
        let retained: Value = read_json(&blobs, &entry.content_ref)
            .await
            .expect("raw item");
        assert_eq!(retained, raw_item);
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
            "arguments": "{\"data\":\"FORGE-MCP-ECHO\"}",
            "error": null,
            "name": "echo",
            "output": "{\"data\":\"FORGE-MCP-ECHO\"}",
            "server_label": "echo"
        });
        let approval_item = json!({
            "id": "mcpr_1",
            "type": "mcp_approval_request",
            "arguments": "{\"data\":\"FORGE-MCP-ECHO\"}",
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
        for entry in &result.context_entries {
            assert!(matches!(entry.kind, ContextEntryKind::ProviderOpaque));
            assert_eq!(entry.media_type.as_deref(), Some(MEDIA_TYPE_JSON));
        }
        assert_eq!(
            result.context_entries[0].provider_kind.as_deref(),
            Some(engine::OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[1].provider_kind.as_deref(),
            Some(engine::OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[2].provider_kind.as_deref(),
            Some(engine::OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND)
        );
        assert_eq!(
            result.context_entries[1].preview.as_deref(),
            Some("OpenAI Responses MCP tool call: echo.echo")
        );
        let retained: Value = read_json(&blobs, &result.context_entries[1].content_ref)
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
}
