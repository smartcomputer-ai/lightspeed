//! Core-side activities of the conversation workflow: the receiver-bound
//! `message_*` declarations, CAS helpers, the text-reply reconciliation
//! after a bot delivery, chat messages through the bot's trigger admission,
//! the archived sends, `#N` resolution, and the liveness gate before a
//! delivery.
//!
//! Everything runs in-process against the universe's [`GatewayAgentApi`]
//! and its store; the document, prompt, handle, and reconciliation shapes
//! are pure functions so they are tested without one.

use std::collections::{BTreeMap, HashSet};

use api::{
    AgentApiError, AgentApiErrorKind, BotEventDocument, BotEventMedia, BotEventOutcome,
    BotTriggerKind, BotTriggerSpec, ChannelAccountId, ChannelProvider, ChatScope,
    ContextEntryKindView, ContextMessageRoleView, RunView, ToolItemStatus, WorkflowEndpointInput,
};
use bots::{
    BotError, BotEventStore, BotRecord, BotRefusalCode, BotStore, BotTriggerRecord,
    BotTriggerStore, EventReceiver,
    ids::{chat_message_event_id, chat_sent_event_id},
};
use channels::{
    ChannelAccountRecord, ChannelAccountStore, ChannelError, ChannelPairingRecord,
    ChannelPairingStore, ConversationRef,
    media::{PreparedMediaItem, media_label},
    policy::format_message_line,
    state::ChatHandle,
    tools::{CHANNEL_TOOL_DESCRIPTIONS, CHANNEL_TOOL_SCHEMAS, channel_workflow_tool_declarations},
};
use engine::{
    BlobRef,
    storage::{BlobStore, BlobStoreError},
};
use serde_json::{Map, Value, json};
use temporal_workflow::channels::*;
use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;

use crate::{
    bots::{
        admission::{AdmitTriggerOutcome, StoreBotEventInput},
        now_ms,
    },
    gateway::GatewayAgentApi,
};

/// Event kind of an inbound chat message.
pub const CHAT_MESSAGE_KIND: &str = "chat.message";
/// Event kind of the bot's own archived send.
pub const CHAT_SENT_KIND: &str = "chat.sent";
/// The summary is the model-facing line; a longer message continues via
/// `bot_event_read`.
const SUMMARY_CAP_CHARS: usize = 2_000;
const SUMMARY_CONTINUATION: &str = "… (full text via bot_event_read)";
/// Name prefix shared by the conversation's `message_*` tools as the model
/// sees them; the session views carry tool names, not tool ids.
const MESSAGING_TOOL_PREFIX: &str = "message_";
const RUN_FAILED_REPLY: &str = "I couldn't complete that request.";
const NO_REPLY_TEXT: &str = "I didn't produce a reply.";

// ── Errors ──────────────────────────────────────────────────────────────────

/// A store or gateway hiccup: Temporal retries the activity.
fn retryable(context: &str, error: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(ApplicationFailure::new(anyhow::anyhow!(
        "{context}: {error}"
    )))
}

/// A structural problem no retry can fix.
fn non_retryable(message: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(ApplicationFailure::non_retryable(anyhow::anyhow!(
        "{message}"
    )))
}

fn bot_error(context: &str, error: BotError) -> ActivityError {
    match error {
        BotError::Store { .. } => retryable(context, error),
        other => non_retryable(format!("{context}: {other}")),
    }
}

fn channel_error(context: &str, error: ChannelError) -> ActivityError {
    match error {
        ChannelError::Store { .. } => retryable(context, error),
        other => non_retryable(format!("{context}: {other}")),
    }
}

fn blob_error(context: &str, error: BlobStoreError) -> ActivityError {
    match error {
        BlobStoreError::NotFound { .. } => non_retryable(format!("{context}: {error}")),
        BlobStoreError::Store { .. } => retryable(context, error),
    }
}

fn api_error(context: &str, error: AgentApiError) -> ActivityError {
    match error.kind {
        AgentApiErrorKind::NotFound | AgentApiErrorKind::InvalidRequest => {
            non_retryable(format!("{context}: {error}"))
        }
        _ => retryable(context, error),
    }
}

fn parse_blob_ref(value: &str) -> Result<BlobRef, ActivityError> {
    BlobRef::parse(value).map_err(|error| non_retryable(format!("invalid blob ref: {error}")))
}

fn blob_store(api: &GatewayAgentApi) -> &dyn BlobStore {
    api.store().as_ref()
}

// ── Tool declarations and CAS ───────────────────────────────────────────────

/// CAS-put every schema and description of the `message_*` tools, by name.
/// Content-addressed, so a repeat returns the same refs.
async fn put_tool_assets(
    blobs: &dyn BlobStore,
) -> Result<
    (
        BTreeMap<&'static str, String>,
        BTreeMap<&'static str, String>,
    ),
    ActivityError,
> {
    let mut names = Vec::new();
    let mut payloads = Vec::new();
    for (name, schema) in CHANNEL_TOOL_SCHEMAS.iter() {
        names.push(*name);
        payloads.push(serde_json::to_vec(schema).map_err(|error| {
            non_retryable(format!("encode channel tool schema {name}: {error}"))
        })?);
    }
    let schema_count = names.len();
    for (name, description) in CHANNEL_TOOL_DESCRIPTIONS {
        names.push(name);
        payloads.push(description.as_bytes().to_vec());
    }
    let refs = blobs
        .put_many(payloads)
        .await
        .map_err(|error| blob_error("store channel tool assets", error))?;
    if refs.len() != names.len() {
        return Err(non_retryable(format!(
            "blob store returned {} refs for {} channel tool assets",
            refs.len(),
            names.len()
        )));
    }
    let mut schemas = BTreeMap::new();
    let mut descriptions = BTreeMap::new();
    for (index, (name, blob_ref)) in names.into_iter().zip(refs).enumerate() {
        let target = if index < schema_count {
            &mut schemas
        } else {
            &mut descriptions
        };
        target.insert(name, blob_ref.to_string());
    }
    Ok((schemas, descriptions))
}

pub async fn chat_tool_declarations(
    api: &GatewayAgentApi,
    request: ChatToolDeclarationsRequest,
) -> Result<ChatToolDeclarationsResult, ActivityError> {
    let blobs = blob_store(api);
    let (schema_refs, description_refs) = put_tool_assets(blobs).await?;
    let receiver = WorkflowEndpointInput {
        workflow_id: request.receiver.workflow_id,
        workflow_kind: request.receiver.workflow_kind,
    };
    let declarations =
        channel_workflow_tool_declarations(receiver, &schema_refs, &description_refs);
    let bytes = serde_json::to_vec(&declarations)
        .map_err(|error| non_retryable(format!("encode channel tool declarations: {error}")))?;
    let tools_ref = blobs
        .put_bytes(bytes)
        .await
        .map_err(|error| blob_error("store channel tool declarations", error))?;
    let children = schema_refs
        .values()
        .chain(description_refs.values())
        .map(|value| parse_blob_ref(value))
        .collect::<Result<Vec<_>, _>>()?;
    engine::storage::record_contains_edges(Some(api.store().as_ref()), &tools_ref, children)
        .await
        .map_err(|error| blob_error("record channel declaration edges", error))?;
    Ok(ChatToolDeclarationsResult {
        tools_ref: tools_ref.to_string(),
        tool_ids: declarations
            .iter()
            .map(|declaration| declaration.definition.tool_id.clone())
            .collect(),
    })
}

pub async fn read_json_blob(
    api: &GatewayAgentApi,
    request: ChatReadJsonBlobRequest,
) -> Result<serde_json::Value, ActivityError> {
    let blob_ref = parse_blob_ref(&request.blob_ref)?;
    let bytes = blob_store(api)
        .read_bytes(&blob_ref)
        .await
        .map_err(|error| blob_error("read blob", error))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| non_retryable(format!("blob {blob_ref} is not JSON: {error}")))
}

pub async fn put_json_blob(
    api: &GatewayAgentApi,
    request: ChatPutJsonBlobRequest,
) -> Result<ChatPutJsonBlobResult, ActivityError> {
    let bytes = serde_json::to_vec(&request.value)
        .map_err(|error| non_retryable(format!("encode JSON blob: {error}")))?;
    let blob_ref = blob_store(api)
        .put_bytes(bytes)
        .await
        .map_err(|error| blob_error("store JSON blob", error))?;
    Ok(ChatPutJsonBlobResult {
        blob_ref: blob_ref.to_string(),
    })
}

// ── Delivery reconciliation ─────────────────────────────────────────────────

fn is_messaging_tool_name(name: &str) -> bool {
    name.starts_with(MESSAGING_TOOL_PREFIX)
}

/// Whether the run answered through a `message_*` tool: a successful
/// result for one of its calls (in the context entries, or in the tool
/// batches when the result entry has not been projected).
pub fn run_used_messaging_tool(run: &RunView) -> bool {
    let calls: HashSet<&str> = run
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            ContextEntryKindView::ToolCall { call_id, name } if is_messaging_tool_name(name) => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect();
    let answered_in_entries = run.entries.iter().any(|entry| {
        matches!(
            &entry.kind,
            ContextEntryKindView::ToolResult { call_id, is_error: false }
                if calls.contains(call_id.as_str())
        )
    });
    let answered_in_batches = run
        .tool_batches
        .iter()
        .flat_map(|batch| batch.calls.iter())
        .any(|call| {
            is_messaging_tool_name(&call.tool_name)
                && call.status == ToolItemStatus::Succeeded
                && !call.is_error
        });
    answered_in_entries || answered_in_batches
}

/// The run's assistant messages, trimmed and joined; `None` when it wrote
/// nothing.
fn assistant_text(run: &RunView) -> Option<String> {
    let texts: Vec<&str> = run
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                ContextEntryKindView::Message {
                    role: ContextMessageRoleView::Assistant
                }
            )
        })
        .filter_map(|entry| entry.text.as_deref().map(str::trim))
        .filter(|text| !text.is_empty())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

/// The reconciliation over the session's runs: suppress when the run
/// answered through a `message_*` tool, otherwise deliver its text (or the
/// canned line when it wrote nothing, including when the run is unknown).
#[cfg(test)]
fn reconcile_runs(runs: &[RunView], run_id: &str) -> ChatReconcileDeliveryResult {
    let run = runs.iter().find(|run| run.id == run_id);
    if run.is_some_and(run_used_messaging_tool) {
        return ChatReconcileDeliveryResult::Suppress {
            reason: ChatSuppressReason::MessagingTool,
        };
    }
    ChatReconcileDeliveryResult::Deliver {
        text: run
            .and_then(assistant_text)
            .unwrap_or_else(|| NO_REPLY_TEXT.to_owned()),
    }
}

pub async fn reconcile_delivery(
    api: &GatewayAgentApi,
    workflow_id: &str,
    request: ChatReconcileDeliveryRequest,
) -> Result<ChatReconcileDeliveryResult, ActivityError> {
    if request.outcome == Some(BotEventOutcome::RunFailed) {
        return Ok(ChatReconcileDeliveryResult::Deliver {
            text: RUN_FAILED_REPLY.to_owned(),
        });
    }
    let Some(run_id) = request.run_id else {
        return Ok(ChatReconcileDeliveryResult::Suppress {
            reason: ChatSuppressReason::NoRun,
        });
    };
    let run = match api
        .read_run_for_channel(
            workflow_id,
            api::RunReadParams {
                session_id: request.session_id,
                run_id: run_id.clone(),
            },
        )
        .await
    {
        Ok(response) => Some(response.result.run),
        // The run is no longer resolvable (session pruned or force-closed
        // between trigger and reconcile): deliver the canned line instead of
        // wedging the delivery workflow on a permanent error.
        Err(error) if error.kind == AgentApiErrorKind::NotFound => None,
        Err(error) => return Err(api_error("read run", error)),
    };
    let Some(run) = run else {
        return Ok(ChatReconcileDeliveryResult::Deliver {
            text: NO_REPLY_TEXT.to_owned(),
        });
    };
    if run_used_messaging_tool(&run) {
        return Ok(ChatReconcileDeliveryResult::Suppress {
            reason: ChatSuppressReason::MessagingTool,
        });
    }
    Ok(ChatReconcileDeliveryResult::Deliver {
        text: assistant_text(&run).unwrap_or_else(|| NO_REPLY_TEXT.to_owned()),
    })
}

// ── Event documents ─────────────────────────────────────────────────────────

fn chat_source(trigger_id: &api::BotTriggerId) -> String {
    format!("chat:{trigger_id}")
}

/// Cap the model-facing summary line; the full text stays in `data`.
fn cap_summary(line: String) -> String {
    if line.chars().count() <= SUMMARY_CAP_CHARS {
        return line;
    }
    let mut capped: String = line.chars().take(SUMMARY_CAP_CHARS).collect();
    capped.push_str(SUMMARY_CONTINUATION);
    capped
}

fn insert_some(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value);
    }
}

/// `data.conversation`: the routing key (the CEL route key of the chat
/// preset) and the provider coordinates the model never sees.
fn conversation_data(
    conversation: &ConversationRef,
    label: &str,
    scope: Option<ChatScope>,
    provider: ChannelProvider,
) -> Value {
    let mut object = Map::new();
    object.insert("key".to_owned(), json!(conversation.key()));
    object.insert("label".to_owned(), json!(label));
    insert_some(&mut object, "scope", scope.map(|scope| json!(scope)));
    object.insert("provider".to_owned(), json!(provider));
    object.insert("accountId".to_owned(), json!(conversation.account_id));
    object.insert("chatId".to_owned(), json!(conversation.chat_id));
    insert_some(
        &mut object,
        "threadId",
        conversation
            .thread_id
            .as_ref()
            .map(|thread_id| json!(thread_id)),
    );
    Value::Object(object)
}

fn media_data(item: &PreparedMediaItem) -> Value {
    let mut object = Map::new();
    object.insert("kind".to_owned(), json!(item.kind));
    object.insert("mime".to_owned(), json!(item.mime));
    insert_some(
        &mut object,
        "name",
        item.name.as_ref().map(|name| json!(name)),
    );
    Value::Object(object)
}

fn event_document(
    kind: &str,
    source: String,
    occurred_at_ms: i64,
    summary: String,
    data: Value,
) -> BotEventDocument {
    BotEventDocument {
        version: BotEventDocument::VERSION,
        kind: kind.to_owned(),
        source,
        occurred_at_ms,
        summary: cap_summary(summary),
        data: Some(data),
        headers: BTreeMap::new(),
        correlation_id: None,
        links: Vec::new(),
        sender: None,
        hops: 0,
        in_reply_to: None,
    }
}

/// The stored envelope of one inbound message. The summary is the message
/// line the model reads; `data` keeps the provider ids for filters, route
/// keys, and `bot_event_read`.
pub fn chat_message_document(request: &ChatEmitEventRequest) -> BotEventDocument {
    let message = &request.message;
    let line = format_message_line(&message.sender_name, message.timestamp_ms, &message.text);
    let data = json!({
        "conversation": conversation_data(
            &request.conversation,
            &request.label,
            Some(request.scope),
            request.provider.clone(),
        ),
        "message": {
            "messageId": message.message_id,
            "senderId": message.sender_id,
            "senderName": message.sender_name,
            "timestampMs": message.timestamp_ms,
            "text": message.text,
            "isDirect": message.is_direct,
            "mentionedBot": message.mentioned_bot,
            "isReplyToBot": message.is_reply_to_bot,
        },
        "media": request.media.iter().map(media_data).collect::<Vec<_>>(),
    });
    event_document(
        CHAT_MESSAGE_KIND,
        chat_source(&request.trigger_id),
        message.timestamp_ms,
        line,
        data,
    )
}

/// What the session prompt shows below the summary: the attachment labels
/// only. An empty array deliberately suppresses the raw provider ids and
/// routing metadata of `data`.
pub fn chat_prompt_data(media: &[PreparedMediaItem]) -> Value {
    Value::Array(
        media
            .iter()
            .map(|item| Value::String(media_label(item.kind, item.name.as_deref())))
            .collect(),
    )
}

/// The archived envelope of one of the bot's own sends, in the same voice
/// as an inbound line so the log reads as the chat.
pub fn chat_sent_document(request: &ChatStoreSentRequest, sent_at_ms: i64) -> BotEventDocument {
    let line = format_message_line("you", sent_at_ms, &request.text);
    let mut message = Map::new();
    message.insert(
        "providerMessageIds".to_owned(),
        json!(request.provider_message_ids),
    );
    message.insert("text".to_owned(), json!(request.text));
    message.insert("fromMe".to_owned(), json!(true));
    insert_some(
        &mut message,
        "replyTo",
        request.reply_to.map(|reply_to| json!(reply_to)),
    );
    let data = json!({
        "conversation": conversation_data(
            &request.conversation,
            &request.label,
            None,
            request.provider.clone(),
        ),
        "message": Value::Object(message),
    });
    event_document(
        CHAT_SENT_KIND,
        chat_source(&request.trigger_id),
        sent_at_ms,
        line,
        data,
    )
}

/// The handle behind a stored chat row, or `None` when the row is not a
/// chat event of this conversation.
pub fn handle_from_document(
    document: &BotEventDocument,
    conversation_key: &str,
) -> Option<ChatHandle> {
    let data = document.data.as_ref()?;
    if data.pointer("/conversation/key").and_then(Value::as_str) != Some(conversation_key) {
        return None;
    }
    let message = data.get("message")?;
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match document.kind.as_str() {
        CHAT_SENT_KIND => {
            let provider_message_ids: Vec<String> = message
                .get("providerMessageIds")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            (!provider_message_ids.is_empty()).then_some(ChatHandle {
                provider_message_ids,
                from_me: true,
                sender_id: None,
                text,
            })
        }
        CHAT_MESSAGE_KIND => {
            let message_id = message
                .get("messageId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())?;
            Some(ChatHandle {
                provider_message_ids: vec![message_id.to_owned()],
                from_me: false,
                sender_id: message
                    .get("senderId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                text,
            })
        }
        _ => None,
    }
}

// ── Chat events ─────────────────────────────────────────────────────────────

/// Why a chat message is refused before admission: a closed bot first (its
/// triggers are disabled with that reason), then the trigger, then the
/// bot's pause.
pub fn chat_refusal(bot: &BotRecord, trigger: &BotTriggerRecord) -> Option<ChatRefusalReason> {
    if bot.is_closed() {
        return Some(ChatRefusalReason::BotClosed);
    }
    if !trigger.enabled() {
        return Some(ChatRefusalReason::TriggerDisabled);
    }
    if !bot.document.enabled {
        return Some(ChatRefusalReason::BotDisabled);
    }
    None
}

/// The admission pipeline's typed refusals that a chat can report.
pub fn refusal_reason(code: BotRefusalCode) -> Option<ChatRefusalReason> {
    match code {
        BotRefusalCode::BotClosed => Some(ChatRefusalReason::BotClosed),
        BotRefusalCode::BotDisabled => Some(ChatRefusalReason::BotDisabled),
        BotRefusalCode::TriggerDisabled => Some(ChatRefusalReason::TriggerDisabled),
        BotRefusalCode::BreakerTripped => Some(ChatRefusalReason::BreakerTripped),
        _ => None,
    }
}

fn refused(reason: ChatRefusalReason) -> ChatEmitEventResult {
    ChatEmitEventResult::Refused { reason }
}

pub async fn emit_chat_event(
    api: &GatewayAgentApi,
    mut request: ChatEmitEventRequest,
) -> Result<ChatEmitEventResult, ActivityError> {
    let store = api.store();
    // A deleted bot was closed first: the conversation is over.
    let bot = match store.read_bot(&request.bot_id).await {
        Ok(bot) => bot,
        Err(BotError::BotNotFound { .. }) => return Ok(refused(ChatRefusalReason::BotClosed)),
        Err(error) => return Err(bot_error("read bot", error)),
    };
    let trigger = match store
        .read_bot_trigger(&request.bot_id, &request.trigger_id)
        .await
    {
        Ok(trigger) => trigger,
        Err(BotError::TriggerNotFound { .. }) => {
            return Ok(refused(ChatRefusalReason::TriggerDisabled));
        }
        Err(error) => return Err(bot_error("read trigger", error)),
    };
    if trigger.kind() != BotTriggerKind::Chat {
        return Err(non_retryable(format!(
            "trigger {} is a {} trigger, not a chat trigger",
            trigger.trigger_id,
            trigger.kind()
        )));
    }
    if let Some(reason) = chat_refusal(&bot, &trigger) {
        return Ok(refused(reason));
    }
    match api.check_trigger_breaker(&bot, &trigger).await {
        Ok(()) => {}
        Err(BotError::Refused {
            code: BotRefusalCode::BreakerTripped,
            ..
        }) => return Ok(refused(ChatRefusalReason::BreakerTripped)),
        Err(error) => return Err(bot_error("check trigger breaker", error)),
    }

    // A conversation may outlive all its bot events. Its cached declaration
    // ref is borrowed until the next event commits; recover if collected.
    let tools_ref = parse_blob_ref(&request.tools_ref)?;
    match store.touch_blob_refs(&[tools_ref]).await {
        Ok(()) => {}
        Err(engine::storage::BlobStoreError::NotFound { .. }) => {
            request.tools_ref = chat_tool_declarations(
                api,
                ChatToolDeclarationsRequest {
                    universe_id: request.universe_id,
                    receiver: request.notify.clone(),
                },
            )
            .await?
            .tools_ref;
        }
        Err(error) => return Err(blob_error("refresh channel declaration grace", error)),
    }

    let event_id = chat_message_event_id(
        &request.trigger_id,
        &request.conversation.key(),
        &request.message.message_id,
    );
    let mut input = StoreBotEventInput::new(event_id.clone(), chat_message_document(&request));
    input.prompt_data = Some(chat_prompt_data(&request.media));
    input.media = request.media.into_iter().map(BotEventMedia::from).collect();
    input.receiver = Some(EventReceiver::Workflow {
        workflow_id: request.notify.workflow_id,
        workflow_kind: request.notify.workflow_kind,
        token: request.notify_token,
        tools_ref: Some(request.tools_ref),
    });
    match api.admit_trigger_event(&bot, &trigger, input).await {
        Ok(AdmitTriggerOutcome::Admitted(stored)) => Ok(if stored.duplicate {
            ChatEmitEventResult::Duplicate {
                event_id,
                seq: stored.record.seq,
            }
        } else {
            ChatEmitEventResult::Admitted {
                event_id,
                seq: stored.record.seq,
                session_id: stored.event.session.map(|session| session.session_id),
            }
        }),
        Ok(AdmitTriggerOutcome::Filtered { .. }) => Ok(ChatEmitEventResult::Filtered { event_id }),
        Err(BotError::Refused { code, message }) => match refusal_reason(code) {
            Some(reason) => Ok(refused(reason)),
            None => Err(non_retryable(format!(
                "admit chat event refused ({code}): {message}"
            ))),
        },
        Err(error) => Err(bot_error("admit chat event", error)),
    }
}

pub async fn store_chat_sent(
    api: &GatewayAgentApi,
    request: ChatStoreSentRequest,
) -> Result<ChatStoreSentResult, ActivityError> {
    let store = api.store();
    let bot = store
        .read_bot(&request.bot_id)
        .await
        .map_err(|error| bot_error("read bot", error))?;
    let trigger = store
        .read_bot_trigger(&request.bot_id, &request.trigger_id)
        .await
        .map_err(|error| bot_error("read trigger", error))?;
    let event_id = chat_sent_event_id(&trigger.trigger_id, &request.invocation_key);
    let mut input = StoreBotEventInput::new(event_id, chat_sent_document(&request, now_ms()));
    input.trigger_id = Some(trigger.trigger_id.clone());
    // Never rendered to a session (archived at birth), but a replay would
    // be: keep the rendering to the summary line, like an inbound message.
    input.prompt_data = Some(Value::Array(Vec::new()));
    // Archived: the send is already in the session as the tool call; the
    // row exists so the number resolves and the log reads as the chat.
    input.deliver = false;
    let stored = api
        .store_bot_event(&bot, input)
        .await
        .map_err(|error| bot_error("store chat send", error))?;
    Ok(ChatStoreSentResult {
        seq: stored.record.seq,
    })
}

pub async fn resolve_chat_handle(
    api: &GatewayAgentApi,
    request: ChatResolveHandleRequest,
) -> Result<ChatResolveHandleResult, ActivityError> {
    let store = api.store();
    let bot = store
        .read_bot(&request.bot_id)
        .await
        .map_err(|error| bot_error("read bot", error))?;
    let max_seq = bot.event_seq;
    let unknown = ChatResolveHandleResult {
        handle: None,
        max_seq,
    };
    let record = match store
        .read_bot_event_by_seq(&request.bot_id, request.seq)
        .await
    {
        Ok(record) => record,
        Err(BotError::EventNotFound { .. }) => return Ok(unknown),
        Err(error) => return Err(bot_error("read bot event", error)),
    };
    if record.kind != CHAT_MESSAGE_KIND && record.kind != CHAT_SENT_KIND {
        return Ok(unknown);
    }
    let document = api
        .read_bot_event_document(&record)
        .await
        .map_err(|error| api_error("read event document", error))?;
    Ok(ChatResolveHandleResult {
        handle: handle_from_document(&document, &request.conversation_key),
        max_seq,
    })
}

// ── Liveness gate ───────────────────────────────────────────────────────────

fn scope_name(scope: ChatScope) -> &'static str {
    match scope {
        ChatScope::Direct => "direct",
        ChatScope::Group => "group",
    }
}

/// Why the trigger no longer serves the conversation, or `None` while it
/// does: enabled chat trigger on this account and scope, open and enabled
/// bot, enabled account, and the chat's pairing pointing at this trigger —
/// pairing is the routing authority, so an unpaired or re-paired chat ends
/// the conversation.
pub fn trigger_inactive_reason(
    bot: &BotRecord,
    trigger: &BotTriggerRecord,
    account: Option<&ChannelAccountRecord>,
    pairing: Option<&ChannelPairingRecord>,
    account_id: &ChannelAccountId,
    scope: ChatScope,
) -> Option<String> {
    let trigger_id = &trigger.trigger_id;
    if !trigger.enabled() {
        return Some(match trigger.disabled_reason {
            Some(reason) => format!("trigger {trigger_id} is disabled ({reason:?})"),
            None => format!("trigger {trigger_id} is disabled"),
        });
    }
    let BotTriggerSpec::Chat {
        account_id: served_account,
        match_scope,
        ..
    } = &trigger.document.spec
    else {
        return Some(format!("trigger {trigger_id} is not a chat trigger"));
    };
    if served_account != account_id.as_str() {
        return Some(format!(
            "trigger {trigger_id} serves account {served_account}, not {account_id}"
        ));
    }
    if let Some(wanted) = match_scope
        && *wanted != scope
    {
        return Some(format!(
            "trigger {trigger_id} serves {} chats only",
            scope_name(*wanted)
        ));
    }
    if bot.is_closed() {
        return Some(format!("bot {} is closed", bot.bot_id));
    }
    if !bot.document.enabled {
        return Some(format!("bot {} is disabled", bot.bot_id));
    }
    match account {
        None => return Some(format!("channel account {account_id} no longer exists")),
        Some(account) if !account.enabled() => {
            return Some(format!("channel account {account_id} is disabled"));
        }
        Some(_) => {}
    }
    if !pairing.is_some_and(|pairing| &pairing.trigger_id == trigger_id) {
        return Some(format!(
            "the conversation is not paired to trigger {trigger_id}"
        ));
    }
    None
}

pub async fn assert_trigger_active(
    api: &GatewayAgentApi,
    request: ChatAssertTriggerActiveRequest,
) -> Result<ChatTriggerActiveResult, ActivityError> {
    let store = api.store();
    let inactive = |reason: String| Ok(ChatTriggerActiveResult::Inactive { reason });
    let trigger = match store
        .read_bot_trigger(&request.bot_id, &request.trigger_id)
        .await
    {
        Ok(trigger) => trigger,
        Err(BotError::TriggerNotFound { .. }) => {
            return inactive(format!("trigger {} no longer exists", request.trigger_id));
        }
        Err(error) => return Err(bot_error("read trigger", error)),
    };
    let bot = match store.read_bot(&request.bot_id).await {
        Ok(bot) => bot,
        Err(BotError::BotNotFound { .. }) => {
            return inactive(format!("bot {} no longer exists", request.bot_id));
        }
        Err(error) => return Err(bot_error("read bot", error)),
    };
    let accounts: &dyn ChannelAccountStore = store.as_ref();
    let account = match accounts.read_channel_account(&request.account_id).await {
        Ok(account) => Some(account),
        Err(ChannelError::AccountNotFound { .. }) => None,
        Err(error) => return Err(channel_error("read channel account", error)),
    };
    let pairings: &dyn ChannelPairingStore = store.as_ref();
    let pairing = pairings
        .read_channel_pairing(&request.account_id, &request.chat_id)
        .await
        .map_err(|error| channel_error("read channel pairing", error))?;
    Ok(
        match trigger_inactive_reason(
            &bot,
            &trigger,
            account.as_ref(),
            pairing.as_ref(),
            &request.account_id,
            request.scope,
        ) {
            Some(reason) => ChatTriggerActiveResult::Inactive { reason },
            None => ChatTriggerActiveResult::Active,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::ChatPairing;
    use api::{
        BotDocument, BotId, BotTriggerDocument, BotTriggerId, ChannelMediaKind, ChatAccess,
        ChatActivation, ProfileId,
    };
    use engine::WorkflowEndpointRef;
    use uuid::Uuid;

    fn conversation(thread_id: Option<&str>) -> ConversationRef {
        ConversationRef {
            account_id: ChannelAccountId::new("tg-main"),
            chat_id: "chat-42".to_owned(),
            thread_id: thread_id.map(str::to_owned),
        }
    }

    fn media(kind: ChannelMediaKind, name: Option<&str>) -> PreparedMediaItem {
        PreparedMediaItem {
            blob_ref: "sha256:abc".to_owned(),
            kind,
            mime: "image/png".to_owned(),
            name: name.map(str::to_owned),
        }
    }

    fn emit_request(text: &str, media: Vec<PreparedMediaItem>) -> ChatEmitEventRequest {
        ChatEmitEventRequest {
            universe_id: Uuid::nil(),
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new("tg"),
            account_id: ChannelAccountId::new("tg-main"),
            provider: ChannelProvider::new("telegram"),
            conversation: conversation(Some("7")),
            label: "Ada (direct)".to_owned(),
            scope: ChatScope::Direct,
            message: ChatMessage {
                message_id: "m-1".to_owned(),
                sender_id: "u-9".to_owned(),
                sender_name: "Ada".to_owned(),
                timestamp_ms: 1_700_000_000_000,
                text: text.to_owned(),
                is_direct: true,
                mentioned_bot: false,
                is_reply_to_bot: true,
            },
            media,
            tools_ref: "sha256:tools".to_owned(),
            notify: WorkflowEndpointRef {
                workflow_id: "u/chat-telegram-x".to_owned(),
                workflow_kind: CHANNEL_CONVERSATION_WORKFLOW_KIND.to_owned(),
            },
            notify_token: "inbound-key".to_owned(),
        }
    }

    fn sent_request(reply_to: Option<u64>) -> ChatStoreSentRequest {
        ChatStoreSentRequest {
            universe_id: Uuid::nil(),
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new("tg"),
            account_id: ChannelAccountId::new("tg-main"),
            provider: ChannelProvider::new("telegram"),
            conversation: conversation(None),
            label: "Ada (direct)".to_owned(),
            invocation_key: "inv-1".to_owned(),
            text: "On it.".to_owned(),
            provider_message_ids: vec!["p-1".to_owned(), "p-2".to_owned()],
            reply_to,
        }
    }

    fn bot(enabled: bool, closed: bool) -> BotRecord {
        BotRecord {
            bot_id: BotId::new("triage"),
            revision: 1,
            document: BotDocument {
                display_name: None,
                description: None,
                profile_id: ProfileId::new("p"),
                brief: None,
                runs_per_day: None,
                breaker: None,
                routed_session_close_after_ms: None,
                self_config: false,
                emit: false,
                enabled,
            },
            event_seq: 3,
            closed_at_ms: closed.then_some(1),
            closed_sessions: Vec::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn trigger(
        enabled: bool,
        pairing: ChatPairing,
        match_scope: Option<ChatScope>,
    ) -> BotTriggerRecord {
        BotTriggerRecord {
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new("tg"),
            revision: 1,
            document: BotTriggerDocument {
                spec: BotTriggerSpec::Chat {
                    account_id: "tg-main".to_owned(),
                    match_scope,
                    activation: ChatActivation::default(),
                    access: ChatAccess::default(),
                    pairing,
                    priority: 100,
                },
                filter: None,
                route: None,
                coalesce: None,
                deliver: None,
                session_close_after_ms: None,
                enabled,
            },
            secrets: bots::BotTriggerSecrets::default(),
            disabled_reason: (!enabled).then_some(api::BotTriggerDisabledReason::Operator),
            disabled_at_ms: None,
            last_filter_error: None,
            last_filter_error_at_ms: None,
            cursor: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn account(enabled: bool) -> ChannelAccountRecord {
        ChannelAccountRecord {
            account_id: ChannelAccountId::new("tg-main"),
            revision: 1,
            document: serde_json::from_value(json!({
                "provider": "telegram",
                "providerAccountId": "12345",
                "displayName": "Main",
                "enabled": enabled,
            }))
            .unwrap(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn pairing(trigger_id: &str) -> ChannelPairingRecord {
        ChannelPairingRecord {
            account_id: ChannelAccountId::new("tg-main"),
            chat_id: "chat-42".to_owned(),
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new(trigger_id),
            paired_via: api::ChannelPairedVia::Open,
            paired_at_ms: 0,
        }
    }

    fn run(entries: Value, tool_batches: Value) -> RunView {
        serde_json::from_value(json!({
            "id": "run-1",
            "status": "completed",
            "source": { "type": "input", "items": [] },
            "entries": entries,
            "toolBatches": tool_batches,
        }))
        .unwrap()
    }

    fn message_entry(role: &str, text: &str) -> Value {
        json!({
            "id": format!("{role}-{}", text.len()),
            "kind": { "type": "message", "role": role },
            "content": { "contentRef": "sha256:x", "mediaType": "text/plain", "providerKind": null },
            "text": text,
        })
    }

    fn tool_call(call_id: &str, name: &str) -> Value {
        json!({
            "id": format!("call-{call_id}"),
            "kind": { "type": "toolCall", "callId": call_id, "name": name },
            "content": { "contentRef": "sha256:x", "mediaType": "text/plain", "providerKind": null },
        })
    }

    fn tool_result(call_id: &str, is_error: bool) -> Value {
        json!({
            "id": format!("result-{call_id}"),
            "kind": { "type": "toolResult", "callId": call_id, "isError": is_error },
            "content": { "contentRef": "sha256:x", "mediaType": "text/plain", "providerKind": null },
        })
    }

    #[test]
    fn message_document_carries_the_line_and_the_provider_ids() {
        let request = emit_request(
            "hello there",
            vec![
                media(ChannelMediaKind::Image, Some("photo.jpg")),
                media(ChannelMediaKind::Document, None),
            ],
        );
        let document = chat_message_document(&request);
        assert_eq!(document.version, BotEventDocument::VERSION);
        assert_eq!(document.kind, CHAT_MESSAGE_KIND);
        assert_eq!(document.source, "chat:tg");
        assert_eq!(document.occurred_at_ms, 1_700_000_000_000);
        assert_eq!(
            document.summary,
            format_message_line("Ada", 1_700_000_000_000, "hello there")
        );
        assert!(document.summary.starts_with("Ada ("));
        assert!(document.summary.ends_with("): hello there"));
        assert_eq!(document.hops, 0);
        assert!(document.sender.is_none());

        let data = document.data.unwrap();
        assert_eq!(
            data["conversation"],
            json!({
                "key": "tg-main/chat-42/7",
                "label": "Ada (direct)",
                "scope": "direct",
                "provider": "telegram",
                "accountId": "tg-main",
                "chatId": "chat-42",
                "threadId": "7",
            })
        );
        assert_eq!(
            data["message"],
            json!({
                "messageId": "m-1",
                "senderId": "u-9",
                "senderName": "Ada",
                "timestampMs": 1_700_000_000_000_i64,
                "text": "hello there",
                "isDirect": true,
                "mentionedBot": false,
                "isReplyToBot": true,
            })
        );
        assert_eq!(
            data["media"],
            json!([
                { "kind": "image", "mime": "image/png", "name": "photo.jpg" },
                { "kind": "document", "mime": "image/png" },
            ])
        );
    }

    #[test]
    fn message_document_omits_absent_thread_and_keeps_media_an_array() {
        let mut request = emit_request("hi", Vec::new());
        request.conversation.thread_id = None;
        let data = chat_message_document(&request).data.unwrap();
        assert!(data["conversation"].get("threadId").is_none());
        assert_eq!(data["conversation"]["key"], "tg-main/chat-42");
        assert_eq!(data["media"], json!([]));
    }

    #[test]
    fn long_summaries_are_capped_with_a_continuation_hint() {
        let text = "ü".repeat(5_000);
        let request = emit_request(&text, Vec::new());
        let document = chat_message_document(&request);
        assert!(document.summary.ends_with(SUMMARY_CONTINUATION));
        assert_eq!(
            document.summary.chars().count(),
            SUMMARY_CAP_CHARS + SUMMARY_CONTINUATION.chars().count()
        );
        assert_eq!(
            document.data.unwrap()["message"]["text"].as_str().unwrap(),
            text
        );
        let short = cap_summary("short".to_owned());
        assert_eq!(short, "short");
    }

    #[test]
    fn prompt_data_is_the_attachment_labels_only() {
        assert_eq!(chat_prompt_data(&[]), json!([]));
        assert_eq!(
            chat_prompt_data(&[
                media(ChannelMediaKind::Image, Some("photo.jpg")),
                media(ChannelMediaKind::Audio, None),
            ]),
            json!(["[image: photo.jpg]", "[audio]"])
        );
    }

    #[test]
    fn sent_document_archives_the_send_with_its_provider_ids() {
        let document = chat_sent_document(&sent_request(Some(17)), 1_700_000_000_000);
        assert_eq!(document.kind, CHAT_SENT_KIND);
        assert_eq!(document.source, "chat:tg");
        assert_eq!(document.occurred_at_ms, 1_700_000_000_000);
        assert!(
            document.summary.starts_with("you ("),
            "{}",
            document.summary
        );
        assert!(document.summary.ends_with("): On it."));
        let data = document.data.unwrap();
        assert_eq!(data["conversation"]["key"], "tg-main/chat-42");
        assert_eq!(data["conversation"]["provider"], "telegram");
        assert!(data["conversation"].get("scope").is_none());
        assert_eq!(
            data["message"],
            json!({
                "providerMessageIds": ["p-1", "p-2"],
                "text": "On it.",
                "fromMe": true,
                "replyTo": 17,
            })
        );

        let without_reply = chat_sent_document(&sent_request(None), 0);
        assert!(
            without_reply.data.unwrap()["message"]
                .get("replyTo")
                .is_none()
        );
    }

    #[test]
    fn handles_come_from_chat_documents_of_the_same_conversation() {
        let inbound = chat_message_document(&emit_request("hello", Vec::new()));
        assert_eq!(
            handle_from_document(&inbound, "tg-main/chat-42/7"),
            Some(ChatHandle {
                provider_message_ids: vec!["m-1".to_owned()],
                from_me: false,
                sender_id: Some("u-9".to_owned()),
                text: Some("hello".to_owned()),
            })
        );
        assert_eq!(handle_from_document(&inbound, "tg-main/chat-42"), None);

        let sent = chat_sent_document(&sent_request(None), 0);
        assert_eq!(
            handle_from_document(&sent, "tg-main/chat-42"),
            Some(ChatHandle {
                provider_message_ids: vec!["p-1".to_owned(), "p-2".to_owned()],
                from_me: true,
                sender_id: None,
                text: Some("On it.".to_owned()),
            })
        );

        let mut empty_send = sent_request(None);
        empty_send.provider_message_ids = vec![String::new()];
        assert_eq!(
            handle_from_document(&chat_sent_document(&empty_send, 0), "tg-main/chat-42"),
            None
        );

        let mut other_kind = inbound.clone();
        other_kind.kind = "bot.reply".to_owned();
        assert_eq!(handle_from_document(&other_kind, "tg-main/chat-42/7"), None);

        let mut no_data = inbound;
        no_data.data = None;
        assert_eq!(handle_from_document(&no_data, "tg-main/chat-42/7"), None);
    }

    #[test]
    fn a_successful_messaging_tool_suppresses_the_text_reply() {
        let runs = vec![run(
            json!([
                message_entry("user", "hi"),
                tool_call("c1", "message_send"),
                tool_result("c1", false),
                message_entry("assistant", "Sent."),
            ]),
            json!([]),
        )];
        assert_eq!(
            reconcile_runs(&runs, "run-1"),
            ChatReconcileDeliveryResult::Suppress {
                reason: ChatSuppressReason::MessagingTool
            }
        );
    }

    #[test]
    fn a_failed_messaging_tool_still_delivers_the_assistant_text() {
        let runs = vec![run(
            json!([
                tool_call("c1", "message_send"),
                tool_result("c1", true),
                tool_call("c2", "bot_status"),
                tool_result("c2", false),
                message_entry("assistant", "  First.  "),
                message_entry("user", "ignored"),
                message_entry("assistant", "Second."),
                message_entry("assistant", "   "),
            ]),
            json!([]),
        )];
        assert_eq!(
            reconcile_runs(&runs, "run-1"),
            ChatReconcileDeliveryResult::Deliver {
                text: "First.\n\nSecond.".to_owned()
            }
        );
    }

    #[test]
    fn a_succeeded_batch_call_counts_when_no_result_entry_is_projected() {
        let batches = json!([{
            "id": "b1",
            "turnId": "t1",
            "status": "succeeded",
            "calls": [{
                "callId": "c1",
                "toolName": "message_react",
                "argumentsRef": "sha256:x",
                "status": "succeeded",
            }],
        }]);
        let runs = vec![run(
            json!([message_entry("assistant", "Reacted.")]),
            batches,
        )];
        assert_eq!(
            reconcile_runs(&runs, "run-1"),
            ChatReconcileDeliveryResult::Suppress {
                reason: ChatSuppressReason::MessagingTool
            }
        );
        let failed = json!([{
            "id": "b1",
            "turnId": "t1",
            "status": "failed",
            "calls": [{
                "callId": "c1",
                "toolName": "message_react",
                "argumentsRef": "sha256:x",
                "status": "failed",
                "isError": true,
            }],
        }]);
        let runs = vec![run(json!([message_entry("assistant", "Reacted.")]), failed)];
        assert_eq!(
            reconcile_runs(&runs, "run-1"),
            ChatReconcileDeliveryResult::Deliver {
                text: "Reacted.".to_owned()
            }
        );
    }

    #[test]
    fn no_assistant_text_or_unknown_run_delivers_the_canned_line() {
        let runs = vec![run(json!([message_entry("user", "hi")]), json!([]))];
        assert_eq!(
            reconcile_runs(&runs, "run-1"),
            ChatReconcileDeliveryResult::Deliver {
                text: NO_REPLY_TEXT.to_owned()
            }
        );
        assert_eq!(
            reconcile_runs(&runs, "run-other"),
            ChatReconcileDeliveryResult::Deliver {
                text: NO_REPLY_TEXT.to_owned()
            }
        );
    }

    #[test]
    fn a_full_assistant_message_is_delivered_without_reading_its_native_blob() {
        let full_text = "long reply ".repeat(1_000);
        let mut entry = message_entry("assistant", &full_text);
        entry["content"] = json!({
            "contentRef": "sha256:unavailable-native-payload",
            "mediaType": "application/json",
            "providerKind": "anthropic.messages.text_blocks"
        });
        let run = run(json!([entry]), json!([]));
        assert_eq!(assistant_text(&run).as_deref(), Some(full_text.trim()));
    }

    #[test]
    fn refusals_name_the_closed_bot_before_its_disabled_trigger() {
        assert_eq!(
            chat_refusal(&bot(true, true), &trigger(false, ChatPairing::Open, None)),
            Some(ChatRefusalReason::BotClosed)
        );
        assert_eq!(
            chat_refusal(&bot(false, false), &trigger(false, ChatPairing::Open, None)),
            Some(ChatRefusalReason::TriggerDisabled)
        );
        assert_eq!(
            chat_refusal(&bot(false, false), &trigger(true, ChatPairing::Open, None)),
            Some(ChatRefusalReason::BotDisabled)
        );
        assert_eq!(
            chat_refusal(&bot(true, false), &trigger(true, ChatPairing::Open, None)),
            None
        );
        assert_eq!(
            refusal_reason(BotRefusalCode::BreakerTripped),
            Some(ChatRefusalReason::BreakerTripped)
        );
        assert_eq!(
            refusal_reason(BotRefusalCode::BotClosed),
            Some(ChatRefusalReason::BotClosed)
        );
        assert_eq!(refusal_reason(BotRefusalCode::Filtered), None);
    }

    #[test]
    fn the_liveness_gate_checks_every_link_of_the_conversation() {
        let account_id = ChannelAccountId::new("tg-main");
        let reason = |bot: &BotRecord,
                      trigger: &BotTriggerRecord,
                      account: Option<&ChannelAccountRecord>,
                      pairing: Option<&ChannelPairingRecord>,
                      scope: ChatScope| {
            trigger_inactive_reason(bot, trigger, account, pairing, &account_id, scope)
        };
        let open = trigger(true, ChatPairing::Open, None);
        let live_account = account(true);
        let paired_here = pairing("tg");
        assert_eq!(
            reason(
                &bot(true, false),
                &open,
                Some(&live_account),
                Some(&paired_here),
                ChatScope::Direct
            ),
            None
        );
        // Pairing is the routing authority for every mode: an open
        // conversation whose chat was unpaired or re-paired ends too.
        assert!(
            reason(
                &bot(true, false),
                &open,
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("not paired")
        );
        assert!(
            reason(
                &bot(true, false),
                &open,
                Some(&live_account),
                Some(&pairing("other")),
                ChatScope::Direct
            )
            .unwrap()
            .contains("not paired")
        );
        assert!(
            reason(
                &bot(true, false),
                &trigger(false, ChatPairing::Open, None),
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("disabled")
        );
        assert!(
            reason(
                &bot(true, true),
                &open,
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("closed")
        );
        assert!(
            reason(
                &bot(false, false),
                &open,
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("disabled")
        );
        assert!(
            reason(&bot(true, false), &open, None, None, ChatScope::Direct)
                .unwrap()
                .contains("no longer exists")
        );
        assert!(
            reason(
                &bot(true, false),
                &open,
                Some(&account(false)),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("disabled")
        );
        let group_only = trigger(true, ChatPairing::Open, Some(ChatScope::Group));
        assert!(
            reason(
                &bot(true, false),
                &group_only,
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("group chats only")
        );
        assert_eq!(
            reason(
                &bot(true, false),
                &group_only,
                Some(&live_account),
                Some(&paired_here),
                ChatScope::Group
            ),
            None
        );

        let coded = trigger(true, ChatPairing::Code, None);
        assert!(
            reason(
                &bot(true, false),
                &coded,
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("not paired")
        );
        assert!(
            reason(
                &bot(true, false),
                &coded,
                Some(&live_account),
                Some(&pairing("other")),
                ChatScope::Direct
            )
            .unwrap()
            .contains("not paired")
        );
        assert_eq!(
            reason(
                &bot(true, false),
                &coded,
                Some(&live_account),
                Some(&pairing("tg")),
                ChatScope::Direct
            ),
            None
        );

        let mut other_account = trigger(true, ChatPairing::Open, None);
        if let BotTriggerSpec::Chat { account_id, .. } = &mut other_account.document.spec {
            *account_id = "wa-main".to_owned();
        }
        assert!(
            reason(
                &bot(true, false),
                &other_account,
                Some(&live_account),
                None,
                ChatScope::Direct
            )
            .unwrap()
            .contains("serves account wa-main")
        );
    }
}
