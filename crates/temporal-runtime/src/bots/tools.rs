//! Execution of the pushed `bot_*` tools. The controller reads a pushed
//! invocation off the session log and hands it here with its arguments
//! blob; the result — or a failure written for the model — goes back to the
//! CAS for the controller to reply with. Configuration mutations run
//! through the same service functions the operator API uses, so a bot
//! editing itself is validated exactly like an operator editing it.
//!
//! Two failure classes: a [`ToolFailure`] is a final answer for the model
//! (a typed refusal, bad arguments, a rejected put), stored as
//! `{ error: { code, message } }`; an infrastructure problem (store, CAS,
//! Temporal) raises a retryable activity error instead and the controller
//! sees the invocation again.

use api::{
    AgentApiError, AgentApiErrorKind, BotEventDocument, BotEventSender, BotFilterTestParams, BotId,
    BotInput, BotTriggerId, BotTriggerInput, BotTriggerKind,
};
use bots::{
    BotError, BotEventStore, BotRecord, BotRefusalCode, BotTriggerRecord, BotTriggerStore,
    EventReceiver, RoutedSession, RoutedSessionClosePolicy,
    ids::{MAX_BOT_HOPS, bot_emit_event_id, bot_keyed_session_id},
    tools::{
        BOT_BRIEF_PUT_TOOL_ID, BOT_EMIT_TOOL_ID, BOT_EVENT_LIST_TOOL_ID, BOT_EVENT_READ_TOOL_ID,
        BOT_FILTER_TEST_TOOL_ID, BOT_STATUS_TOOL_ID, BOT_TRIGGER_DELETE_TOOL_ID,
        BOT_TRIGGER_LIST_TOOL_ID, BOT_TRIGGER_PUT_TOOL_ID, is_self_config_tool,
        parse_trigger_put_args,
    },
    views::{
        bot_status_view, event_envelope_view, event_list_row_view, filter_result_view, read_budget,
        resolve_inbox, trigger_tool_view,
    },
};
use harness::{
    BlobRef,
    storage::{BlobStore, BlobStoreError},
};
use serde_json::{Map, Value, json};
use temporal_workflow::bots::*;
use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;

use super::{
    admission::{AdmitTriggerOutcome, StoreBotEventInput},
    now_ms,
};
use crate::gateway::GatewayAgentApi;

/// Longest brief a bot may write for itself.
const MAX_BRIEF_CHARS: usize = 20_000;
/// Bounds of an emitted event's `kind` and `summary`, the same the manual
/// admission applies.
const MAX_EVENT_KIND_BYTES: usize = 200;
const MAX_EVENT_SUMMARY_BYTES: usize = 2_000;
/// Sample size of `bot_event_list` / `bot_filter_test` when the model gives
/// none.
const DEFAULT_LIST_LIMIT: u32 = 20;

// ── Failures ────────────────────────────────────────────────────────────────

/// A final answer for the model: a stable code plus a message written to be
/// read. Stored in the CAS as `{ error: { code, message } }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolFailure {
    pub code: String,
    pub message: String,
}

impl ToolFailure {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// The arguments do not describe a valid call.
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }

    /// The bot lacks the capability grant the tool needs.
    pub(crate) fn not_permitted(message: impl Into<String>) -> Self {
        Self::new("not_permitted", message)
    }

    /// A typed refusal of the admission pipeline.
    pub(crate) fn refused(code: BotRefusalCode, message: impl Into<String>) -> Self {
        Self::new(code.as_str(), message)
    }

    /// The one-line message the controller hands back with the error ref.
    pub(crate) fn summary(&self) -> String {
        format!("{}: {}", self.code, self.message)
    }

    /// The document stored for the model to read.
    pub(crate) fn document(&self) -> Value {
        json!({ "error": { "code": self.code, "message": self.message } })
    }
}

/// Outcome of one tool body short of a payload.
#[derive(Debug)]
pub(crate) enum ToolError {
    /// A final answer for the model.
    Failed(ToolFailure),
    /// A runtime problem; the activity retries.
    Infrastructure(anyhow::Error),
}

impl From<ToolFailure> for ToolError {
    fn from(failure: ToolFailure) -> Self {
        Self::Failed(failure)
    }
}

impl From<BotError> for ToolError {
    fn from(error: BotError) -> Self {
        match error {
            BotError::Store { message } => {
                Self::Infrastructure(anyhow::anyhow!("bot store failure: {message}"))
            }
            BotError::Refused { code, message } => {
                Self::Failed(ToolFailure::refused(code, message))
            }
            BotError::BotClosed { .. } => Self::Failed(ToolFailure::refused(
                BotRefusalCode::BotClosed,
                error.to_string(),
            )),
            BotError::BotNotFound { .. }
            | BotError::TriggerNotFound { .. }
            | BotError::EventNotFound { .. }
            | BotError::EventIdNotFound { .. } => {
                Self::Failed(ToolFailure::new("not_found", error.to_string()))
            }
            BotError::BotAlreadyExists { .. }
            | BotError::BotRevisionConflict { .. }
            | BotError::TriggerRevisionConflict { .. } => {
                Self::Failed(ToolFailure::new("conflict", error.to_string()))
            }
            BotError::InvalidInput { message } => {
                Self::Failed(ToolFailure::new("invalid_request", message))
            }
        }
    }
}

/// The stable code of a service error the model may read.
fn api_error_code(kind: AgentApiErrorKind) -> &'static str {
    match kind {
        AgentApiErrorKind::InvalidRequest => "invalid_request",
        AgentApiErrorKind::NotFound => "not_found",
        AgentApiErrorKind::Conflict => "conflict",
        AgentApiErrorKind::EnvironmentNotReady => "environment_not_ready",
        AgentApiErrorKind::Internal => "internal",
        _ => "rejected",
    }
}

impl From<AgentApiError> for ToolError {
    fn from(error: AgentApiError) -> Self {
        match error.kind {
            AgentApiErrorKind::Internal => {
                Self::Infrastructure(anyhow::anyhow!("{}", error.message))
            }
            kind => Self::Failed(ToolFailure::new(api_error_code(kind), error.message)),
        }
    }
}

impl From<BlobStoreError> for ToolError {
    fn from(error: BlobStoreError) -> Self {
        Self::Infrastructure(anyhow::anyhow!("{error}"))
    }
}

fn retryable(error: anyhow::Error) -> ActivityError {
    ActivityError::from(error)
}

// ── Entry point ─────────────────────────────────────────────────────────────

pub async fn execute_tool(
    api: &GatewayAgentApi,
    request: BotExecuteToolRequest,
) -> Result<BotExecuteToolResult, ActivityError> {
    match run(api, &request).await {
        Ok(payload) => Ok(BotExecuteToolResult::Resolved {
            payload_ref: put_json(api, &payload).await?,
        }),
        Err(ToolError::Failed(failure)) => {
            tracing::info!(
                target: "temporal_runtime",
                bot_id = %request.bot_id,
                tool_id = %request.tool_id,
                invocation_id = %request.invocation_id,
                code = %failure.code,
                message = %failure.message,
                "bot tool refused"
            );
            Ok(BotExecuteToolResult::Failed {
                message: failure.summary(),
                error_ref: put_json(api, &failure.document()).await?,
            })
        }
        Err(ToolError::Infrastructure(error)) => Err(retryable(error.context(format!(
            "bot tool {} for {}",
            request.tool_id, request.bot_id
        )))),
    }
}

async fn run(api: &GatewayAgentApi, request: &BotExecuteToolRequest) -> Result<Value, ToolError> {
    let args = read_arguments(api, &request.arguments_ref).await?;
    let bot = api.load_bot_for_admission(&request.bot_id).await?;
    check_capability(&bot, &request.tool_id)?;
    match request.tool_id.as_str() {
        BOT_STATUS_TOOL_ID => Ok(bot_status_view(&bot, Some(&request.controller.snapshot))),
        BOT_TRIGGER_LIST_TOOL_ID => list_triggers(api, &bot).await,
        BOT_TRIGGER_PUT_TOOL_ID => put_trigger(api, &bot, &args).await,
        BOT_TRIGGER_DELETE_TOOL_ID => delete_trigger(api, &bot, &args).await,
        BOT_FILTER_TEST_TOOL_ID => test_filter(api, &bot, &args).await,
        BOT_EVENT_LIST_TOOL_ID => list_events(api, &bot, &args).await,
        BOT_EVENT_READ_TOOL_ID => read_event(api, &bot, &args).await,
        BOT_BRIEF_PUT_TOOL_ID => put_brief(api, &bot, &args).await,
        BOT_EMIT_TOOL_ID => emit(api, &bot, request, &args).await,
        other => Err(ToolFailure::invalid(format!("unknown bot tool {other}")).into()),
    }
}

/// Defense in depth: the gated tools are not declared to sessions of a bot
/// without the grant, but the fresh row is authoritative — a stale
/// pre-toggle session must not mutate configuration or emit either.
pub(crate) fn check_capability(bot: &BotRecord, tool_id: &str) -> Result<(), ToolFailure> {
    if is_self_config_tool(tool_id) && !bot.document.self_config {
        return Err(ToolFailure::not_permitted(
            "self-configuration is disabled for this bot; an operator can enable it in the bot's settings",
        ));
    }
    if tool_id == BOT_EMIT_TOOL_ID && !bot.document.emit {
        return Err(ToolFailure::not_permitted(
            "bot_emit is disabled for this bot (the emit grant); an operator can enable it in the bot's settings",
        ));
    }
    Ok(())
}

// ── CAS ─────────────────────────────────────────────────────────────────────

async fn read_arguments(api: &GatewayAgentApi, arguments_ref: &str) -> Result<Value, ToolError> {
    let blob_ref = BlobRef::parse(arguments_ref).map_err(|error| {
        ToolError::Infrastructure(anyhow::anyhow!("invalid tool arguments ref: {error}"))
    })?;
    let blobs: &dyn BlobStore = api.store().as_ref();
    let bytes = blobs.read_bytes(&blob_ref).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ToolFailure::invalid(format!("tool arguments are not valid JSON: {error}"))
    })?;
    arguments_object(value).map_err(Into::into)
}

/// The arguments as an object: absent / null is an empty call, anything
/// but an object is a bad one.
pub(crate) fn arguments_object(value: Value) -> Result<Value, ToolFailure> {
    match value {
        Value::Null => Ok(Value::Object(Map::new())),
        object @ Value::Object(_) => Ok(object),
        other => Err(ToolFailure::invalid(format!(
            "tool arguments must be an object, got {}",
            json_type_name(&other)
        ))),
    }
}

async fn put_json(api: &GatewayAgentApi, value: &Value) -> Result<String, ActivityError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        ActivityError::application(ApplicationFailure::non_retryable(anyhow::anyhow!(
            "encode bot tool result: {error}"
        )))
    })?;
    let blobs: &dyn BlobStore = api.store().as_ref();
    blobs
        .put_bytes(bytes)
        .await
        .map(|blob_ref| blob_ref.to_string())
        .map_err(|error| retryable(anyhow::anyhow!("store bot tool result: {error}")))
}

// ── Arguments ───────────────────────────────────────────────────────────────

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A non-empty string argument; null, absent, empty, or non-string reads
/// as absent.
fn nullable_string<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn require_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolFailure> {
    nullable_string(args, key).ok_or_else(|| ToolFailure::invalid(format!("{key} is required")))
}

/// A non-negative integer argument; null or absent reads as absent.
fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, ToolFailure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| ToolFailure::invalid(format!("{key} must be a non-negative integer"))),
        Some(other) => Err(ToolFailure::invalid(format!(
            "{key} must be an integer, got {}",
            json_type_name(other)
        ))),
    }
}

fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolFailure> {
    optional_u64(args, key)?
        .map(|value| {
            u32::try_from(value).map_err(|_| ToolFailure::invalid(format!("{key} is too large")))
        })
        .transpose()
}

fn insert_some<T: Into<Value>>(object: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value.into());
    }
}

// ── Triggers ────────────────────────────────────────────────────────────────

/// Public ingest URL of a webhook trigger: the gateway's public origin plus
/// the tokenized route.
pub(crate) fn ingest_url(public_base_url: &str, ingest_path: &str) -> String {
    format!("{}{ingest_path}", public_base_url.trim_end_matches('/'))
}

/// A trigger as the model sees it. The bot is its own manager under the
/// self-configuration grant, so it keeps its chat pairing codes (it has to
/// tell the human what to send); everything else is redacted as for a
/// non-managing member.
fn trigger_view(api: &GatewayAgentApi, bot: &BotRecord, record: &BotTriggerRecord) -> Value {
    let ingest_url = api
        .trigger_view(record)
        .ingest_path
        .map(|path| ingest_url(api.public_base_url(), &path));
    trigger_tool_view(record, ingest_url, bot.document.self_config)
}

async fn list_triggers(api: &GatewayAgentApi, bot: &BotRecord) -> Result<Value, ToolError> {
    let triggers = api.store().list_bot_triggers(&bot.bot_id).await?;
    Ok(json!({
        "triggers": triggers
            .iter()
            .map(|record| trigger_view(api, bot, record))
            .collect::<Vec<_>>(),
    }))
}

async fn put_trigger(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    args: &Value,
) -> Result<Value, ToolError> {
    let put = parse_trigger_put_args(args).map_err(ToolFailure::invalid)?;
    let existing = match api
        .store()
        .read_bot_trigger(&bot.bot_id, &put.trigger_id)
        .await
    {
        Ok(existing) => Some(existing),
        Err(BotError::TriggerNotFound { .. }) => None,
        Err(error) => return Err(error.into()),
    };
    let document = put
        .apply_to(existing.as_ref().map(|record| &record.document))
        .map_err(ToolFailure::invalid)?;
    // The service validates and reconciles the Schedule exactly as for an
    // operator put; the expected revision is left open because the model
    // never sees revisions.
    let record = api
        .put_bot_trigger_record(
            &bot.bot_id,
            BotTriggerInput {
                trigger_id: put.trigger_id.clone(),
                document,
                pairing_code: None,
            },
            None,
        )
        .await?;
    Ok(json!({
        "trigger": trigger_view(api, bot, &record),
        "created": existing.is_none(),
    }))
}

async fn delete_trigger(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    args: &Value,
) -> Result<Value, ToolError> {
    let name = require_string(args, "name")?;
    let trigger_id = BotTriggerId::try_new(name)
        .map_err(|error| ToolFailure::invalid(format!("name: {error}")))?;
    match api
        .delete_bot_trigger_record(&bot.bot_id, &trigger_id)
        .await
    {
        Ok(_) => Ok(json!({ "deleted": true, "name": name })),
        Err(error) if error.kind == AgentApiErrorKind::NotFound => {
            Err(ToolFailure::new("not_found", format!("no trigger named {name}")).into())
        }
        Err(error) => Err(error.into()),
    }
}

// ── Events ──────────────────────────────────────────────────────────────────

async fn test_filter(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    args: &Value,
) -> Result<Value, ToolError> {
    let filter = require_string(args, "filter")?.to_owned();
    // One document, no traffic needed: the way to write a filter before any
    // event exists, since refused events are never stored.
    let payload = args
        .get("payload")
        .filter(|value| value.is_object())
        .cloned();
    let limit = optional_u32(args, "limit")?.unwrap_or(DEFAULT_LIST_LIMIT);
    let response = api
        .test_bot_filter_records(BotFilterTestParams {
            bot_id: bot.bot_id.clone(),
            filter: filter.clone(),
            payload: payload.clone(),
            limit: Some(limit),
        })
        .await?;
    if payload.is_some() {
        let result = response.results.first();
        let mut view = Map::new();
        view.insert("filter".to_owned(), json!(filter));
        view.insert("payload".to_owned(), json!(true));
        view.insert(
            "matched".to_owned(),
            json!(result.is_some_and(|result| result.matched)),
        );
        insert_some(
            &mut view,
            "error",
            result.and_then(|result| result.error.clone()),
        );
        return Ok(Value::Object(view));
    }
    let mut results = Vec::with_capacity(response.results.len());
    for result in &response.results {
        let Some(seq) = result.seq else {
            continue;
        };
        match api.store().read_bot_event_by_seq(&bot.bot_id, seq).await {
            Ok(record) => {
                results.push(filter_result_view(
                    &record,
                    result.matched,
                    result.error.as_deref(),
                ));
            }
            Err(BotError::EventNotFound { .. }) => {
                let mut row = Map::new();
                row.insert("seq".to_owned(), json!(seq));
                row.insert("matched".to_owned(), json!(result.matched));
                insert_some(&mut row, "error", result.error.clone());
                results.push(Value::Object(row));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(json!({
        "filter": filter,
        "sampled": response.sampled,
        "matched": response.matched,
        "errors": response.errors,
        "results": results,
    }))
}

async fn list_events(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    args: &Value,
) -> Result<Value, ToolError> {
    let limit = optional_u32(args, "limit")?.unwrap_or(DEFAULT_LIST_LIMIT);
    let (records, _) = api
        .list_bot_events_page(&bot.bot_id, Some(limit), None)
        .await?;
    Ok(json!({
        "events": records.iter().map(event_list_row_view).collect::<Vec<_>>(),
    }))
}

/// Why `#seq` is not one of the bot's events, naming the valid range.
pub(crate) fn unknown_seq_message(seq: u64, event_seq: u64) -> String {
    if event_seq > 0 {
        format!("no event #{seq}; this bot's events run #1..#{event_seq}")
    } else {
        format!("no event #{seq}; this bot has no events yet")
    }
}

async fn read_event(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    args: &Value,
) -> Result<Value, ToolError> {
    let seq = optional_u64(args, "seq")?
        .filter(|seq| *seq >= 1)
        .ok_or_else(|| ToolFailure::invalid("seq must be a positive integer (the event's #N)"))?;
    let record = match api.store().read_bot_event_by_seq(&bot.bot_id, seq).await {
        Ok(record) => record,
        Err(BotError::EventNotFound { .. }) => {
            return Err(
                ToolFailure::new("not_found", unknown_seq_message(seq, bot.event_seq)).into(),
            );
        }
        Err(error) => return Err(error.into()),
    };
    let document = api.read_bot_event_document(&record).await?;
    let path = nullable_string(args, "path");
    let max_bytes = read_budget(optional_u64(args, "maxBytes")?);
    event_envelope_view(&record, &document, path, max_bytes)
        .map_err(|message| ToolFailure::invalid(message).into())
}

// ── Brief ───────────────────────────────────────────────────────────────────

async fn put_brief(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    args: &Value,
) -> Result<Value, ToolError> {
    let brief = require_string(args, "brief")?.trim().to_owned();
    if brief.is_empty() {
        return Err(ToolFailure::invalid("brief is required").into());
    }
    if brief.chars().count() > MAX_BRIEF_CHARS {
        return Err(ToolFailure::invalid(format!(
            "brief is too long (max {MAX_BRIEF_CHARS} characters)"
        ))
        .into());
    }
    let mut document = bot.document.clone();
    document.brief = Some(brief.clone());
    // The put signals the controller, which re-applies the brief at its
    // next idle boundary.
    api.put_bot_record(
        BotInput {
            bot_id: bot.bot_id.clone(),
            document,
        },
        Some(bot.revision),
    )
    .await?;
    Ok(json!({ "brief": brief, "appliesAt": "next idle boundary" }))
}

// ── bot_emit ────────────────────────────────────────────────────────────────

/// The validated `bot_emit` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmitArgs {
    pub kind: String,
    pub summary: String,
    pub data: Option<Value>,
    /// Addressed emit: the receiving bot.
    pub to: Option<BotId>,
    /// Ask the receiver for a `bot.reply` receipt.
    pub reply: bool,
    /// Self emit: route into one of the bot's own keyed sessions.
    pub session_key: Option<String>,
}

pub(crate) fn parse_emit_args(args: &Value) -> Result<EmitArgs, ToolFailure> {
    let kind = require_string(args, "kind")?;
    if kind.trim().is_empty() || kind.len() > MAX_EVENT_KIND_BYTES {
        return Err(ToolFailure::invalid(format!(
            "kind must be 1..={MAX_EVENT_KIND_BYTES} bytes"
        )));
    }
    let summary = require_string(args, "summary")?;
    if summary.trim().is_empty() || summary.len() > MAX_EVENT_SUMMARY_BYTES {
        return Err(ToolFailure::invalid(format!(
            "summary must be 1..={MAX_EVENT_SUMMARY_BYTES} bytes"
        )));
    }
    let data = args.get("data").filter(|value| !value.is_null()).cloned();
    let to = nullable_string(args, "to")
        .map(|to| BotId::try_new(to).map_err(|error| ToolFailure::invalid(format!("to: {error}"))))
        .transpose()?;
    let reply = match args.get("reply") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(reply)) => *reply,
        Some(other) => {
            return Err(ToolFailure::invalid(format!(
                "reply must be a boolean, got {}",
                json_type_name(other)
            )));
        }
    };
    let session_key = nullable_string(args, "sessionKey").map(str::to_owned);
    if to.is_some() && session_key.is_some() {
        return Err(ToolFailure::invalid(
            "sessionKey routes your own keyed sessions; it cannot be combined with to",
        ));
    }
    if reply && to.is_none() {
        return Err(ToolFailure::invalid(
            "reply needs to: receipts come from the bot you address",
        ));
    }
    Ok(EmitArgs {
        kind: kind.to_owned(),
        summary: summary.to_owned(),
        data,
        to,
        reply,
        session_key,
    })
}

/// Loop bound: an addressed emit is one hop further from the world than
/// the delivery being handled; a self emit stays at the same distance (the
/// sender rate cap bounds self loops). Past [`MAX_BOT_HOPS`] the chain is
/// cut.
pub(crate) fn emit_hops(current: u32, addressed: bool) -> Result<u32, ToolFailure> {
    let hops = if addressed {
        current.saturating_add(1)
    } else {
        current
    };
    if hops > MAX_BOT_HOPS {
        return Err(ToolFailure::refused(
            BotRefusalCode::LoopCut,
            format!(
                "this event would be {hops} bot-to-bot hops from the world (limit {MAX_BOT_HOPS}); the chain was cut"
            ),
        ));
    }
    Ok(hops)
}

async fn emit(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    request: &BotExecuteToolRequest,
    args: &Value,
) -> Result<Value, ToolError> {
    let emit = parse_emit_args(args)?;
    let hops = emit_hops(request.controller.hops, emit.to.is_some())?;
    api.check_sender_rate(bot).await?;
    // The invocation id is stable across retries, so a retried emit
    // converges on one event per receiver.
    let event_id = bot_emit_event_id(&bot.bot_id, &request.invocation_id);
    let document = BotEventDocument {
        version: BotEventDocument::VERSION,
        kind: emit.kind,
        source: format!("bot:{}", bot.bot_id),
        occurred_at_ms: now_ms(),
        summary: emit.summary,
        data: emit.data,
        headers: Default::default(),
        correlation_id: None,
        links: Vec::new(),
        sender: Some(BotEventSender {
            bot: bot.bot_id.clone(),
        }),
        hops,
        in_reply_to: None,
    };
    let mut input = StoreBotEventInput::new(event_id, document);
    input.sender_bot_id = Some(bot.bot_id.clone());
    input.hops = hops;

    let Some(to) = emit.to else {
        input.session = emit.session_key.map(|key| RoutedSession {
            session_id: bot_keyed_session_id(&bot.bot_id, &key),
            label: key.clone(),
            session_key: Some(key),
            close_policy: RoutedSessionClosePolicy::Inherit,
        });
        let stored = api.store_bot_event(bot, input).await?;
        return Ok(json!({ "seq": stored.record.seq }));
    };

    let target = api.load_bot_for_admission(&to).await?;
    let triggers = api.store().list_bot_triggers(&to).await?;
    let inbox = triggers
        .iter()
        .find(|trigger| trigger.kind() == BotTriggerKind::Bot);
    let inbox = resolve_inbox(&target, inbox, &bot.bot_id)?;
    if let Err(error) = api.check_trigger_breaker(&target, inbox).await {
        return Err(match error {
            BotError::Refused {
                code: BotRefusalCode::BreakerTripped,
                ..
            } => ToolFailure::refused(
                BotRefusalCode::BreakerTripped,
                format!("{to}'s inbox exceeded its flood breaker and was disabled; a human re-enables it"),
            )
            .into(),
            other => other.into(),
        });
    }
    if emit.reply {
        input.receiver = Some(EventReceiver::Bot {
            bot_id: bot.bot_id.clone(),
            session: request.controller.routed_session.clone(),
        });
    }
    match api.admit_trigger_event(&target, inbox, input).await? {
        AdmitTriggerOutcome::Admitted(stored) => Ok(json!({ "to": to, "seq": stored.record.seq })),
        AdmitTriggerOutcome::Filtered { .. } => Err(ToolFailure::refused(
            BotRefusalCode::Filtered,
            format!("{to}'s inbox filter refused your event; it was not stored or delivered"),
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{BotDocument, ProfileId};

    fn bot(self_config: bool, emit: bool) -> BotRecord {
        BotRecord {
            bot_id: BotId::new("triage"),
            revision: 4,
            document: BotDocument {
                display_name: None,
                description: None,
                profile_id: ProfileId::new("p"),
                brief: None,
                runs_per_day: None,
                breaker: None,
                routed_session_close_after_ms: None,
                self_config,
                emit,
                enabled: true,
            },
            event_seq: 0,
            closed_at_ms: None,
            closed_sessions: Vec::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn gates_follow_the_fresh_row_not_the_declaration() {
        let ungranted = bot(false, false);
        for tool in [
            BOT_TRIGGER_PUT_TOOL_ID,
            BOT_TRIGGER_DELETE_TOOL_ID,
            BOT_BRIEF_PUT_TOOL_ID,
            BOT_EMIT_TOOL_ID,
        ] {
            let failure = check_capability(&ungranted, tool).unwrap_err();
            assert_eq!(failure.code, "not_permitted", "{tool}");
        }
        for tool in [
            BOT_STATUS_TOOL_ID,
            BOT_TRIGGER_LIST_TOOL_ID,
            BOT_FILTER_TEST_TOOL_ID,
            BOT_EVENT_LIST_TOOL_ID,
            BOT_EVENT_READ_TOOL_ID,
        ] {
            assert!(check_capability(&ungranted, tool).is_ok(), "{tool}");
        }
        let configurer = bot(true, false);
        assert!(check_capability(&configurer, BOT_TRIGGER_PUT_TOOL_ID).is_ok());
        assert_eq!(
            check_capability(&configurer, BOT_EMIT_TOOL_ID)
                .unwrap_err()
                .code,
            "not_permitted"
        );
        let emitter = bot(false, true);
        assert!(check_capability(&emitter, BOT_EMIT_TOOL_ID).is_ok());
        assert_eq!(
            check_capability(&emitter, BOT_BRIEF_PUT_TOOL_ID)
                .unwrap_err()
                .code,
            "not_permitted"
        );
    }

    #[test]
    fn emit_args_validate_the_exclusive_fields() {
        let self_emit = parse_emit_args(&json!({
            "kind": "digest.ready",
            "summary": "Digest built",
            "data": { "items": 3 },
            "sessionKey": "digest",
        }))
        .unwrap();
        assert_eq!(
            self_emit,
            EmitArgs {
                kind: "digest.ready".to_owned(),
                summary: "Digest built".to_owned(),
                data: Some(json!({ "items": 3 })),
                to: None,
                reply: false,
                session_key: Some("digest".to_owned()),
            }
        );
        let addressed = parse_emit_args(&json!({
            "kind": "incident.opened",
            "summary": "Incident 7",
            "data": null,
            "to": "infra",
            "reply": true,
        }))
        .unwrap();
        assert_eq!(addressed.to, Some(BotId::new("infra")));
        assert!(addressed.reply);
        assert_eq!(addressed.data, None);
        assert_eq!(addressed.session_key, None);

        let both = parse_emit_args(&json!({
            "kind": "k", "summary": "s", "to": "infra", "sessionKey": "x",
        }))
        .unwrap_err();
        assert_eq!(both.code, "invalid_arguments");
        assert!(both.message.contains("sessionKey"), "{}", both.message);

        let reply_alone =
            parse_emit_args(&json!({ "kind": "k", "summary": "s", "reply": true })).unwrap_err();
        assert!(
            reply_alone.message.contains("reply needs to"),
            "{}",
            reply_alone.message
        );

        let no_kind = parse_emit_args(&json!({ "summary": "s" })).unwrap_err();
        assert_eq!(no_kind.message, "kind is required");
        let no_summary = parse_emit_args(&json!({ "kind": "k" })).unwrap_err();
        assert_eq!(no_summary.message, "summary is required");
        let blank_summary = parse_emit_args(&json!({ "kind": "k", "summary": "   " })).unwrap_err();
        assert!(blank_summary.message.starts_with("summary must be"));
        let long_kind =
            parse_emit_args(&json!({ "kind": "k".repeat(201), "summary": "s" })).unwrap_err();
        assert!(long_kind.message.starts_with("kind must be"));

        let bad_to = parse_emit_args(&json!({ "kind": "k", "summary": "s", "to": "Not A Bot!" }))
            .unwrap_err();
        assert!(bad_to.message.starts_with("to: "), "{}", bad_to.message);
        let bad_reply =
            parse_emit_args(&json!({ "kind": "k", "summary": "s", "to": "infra", "reply": "yes" }))
                .unwrap_err();
        assert!(bad_reply.message.contains("reply must be a boolean"));
    }

    #[test]
    fn emit_hops_keep_self_emits_and_advance_addressed_ones_until_the_cut() {
        assert_eq!(emit_hops(3, false), Ok(3));
        assert_eq!(emit_hops(3, true), Ok(4));
        assert_eq!(emit_hops(MAX_BOT_HOPS, false), Ok(MAX_BOT_HOPS));
        assert_eq!(emit_hops(MAX_BOT_HOPS - 1, true), Ok(MAX_BOT_HOPS));
        let cut = emit_hops(MAX_BOT_HOPS, true).unwrap_err();
        assert_eq!(cut.code, "loop_cut");
        assert!(cut.message.contains(&MAX_BOT_HOPS.to_string()));
        assert_eq!(emit_hops(u32::MAX, true).unwrap_err().code, "loop_cut");
    }

    #[test]
    fn failures_are_stored_as_error_documents_with_a_one_line_summary() {
        let failure = ToolFailure::refused(BotRefusalCode::RateLimited, "wait a while");
        assert_eq!(failure.summary(), "rate_limited: wait a while");
        assert_eq!(
            failure.document(),
            json!({ "error": { "code": "rate_limited", "message": "wait a while" } })
        );
        assert_eq!(ToolFailure::invalid("x").code, "invalid_arguments");
        assert_eq!(ToolFailure::not_permitted("x").code, "not_permitted");
    }

    #[test]
    fn store_errors_retry_while_everything_else_answers_the_model() {
        let retry = ToolError::from(BotError::store("connection reset"));
        assert!(matches!(retry, ToolError::Infrastructure(_)), "{retry:?}");
        let refused = ToolError::from(BotError::refused(BotRefusalCode::UnknownBot, "no bot"));
        assert!(
            matches!(&refused, ToolError::Failed(failure) if failure.code == "unknown_bot"),
            "{refused:?}"
        );
        let missing = ToolError::from(BotError::TriggerNotFound {
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new("gh"),
        });
        assert!(
            matches!(&missing, ToolError::Failed(failure) if failure.code == "not_found"),
            "{missing:?}"
        );
        let conflict = ToolError::from(BotError::BotRevisionConflict {
            bot_id: BotId::new("triage"),
            expected: 1,
            actual: 2,
        });
        assert!(
            matches!(&conflict, ToolError::Failed(failure) if failure.code == "conflict"),
            "{conflict:?}"
        );
        let closed = ToolError::from(BotError::BotClosed {
            bot_id: BotId::new("triage"),
        });
        assert!(
            matches!(&closed, ToolError::Failed(failure) if failure.code == "bot_closed"),
            "{closed:?}"
        );

        let internal = ToolError::from(AgentApiError::internal("db down"));
        assert!(matches!(internal, ToolError::Infrastructure(_)));
        let invalid = ToolError::from(AgentApiError::invalid_request("bad cron"));
        assert!(
            matches!(&invalid, ToolError::Failed(failure) if failure.code == "invalid_request" && failure.message == "bad cron"),
            "{invalid:?}"
        );
        let rejected = ToolError::from(AgentApiError::rejected("closed"));
        assert!(matches!(&rejected, ToolError::Failed(failure) if failure.code == "rejected"));
        let not_found = ToolError::from(AgentApiError::not_found("gone"));
        assert!(matches!(&not_found, ToolError::Failed(failure) if failure.code == "not_found"));
    }

    #[test]
    fn arguments_default_to_an_empty_object_and_reject_other_shapes() {
        assert_eq!(arguments_object(Value::Null).unwrap(), json!({}));
        assert_eq!(
            arguments_object(json!({ "seq": 1 })).unwrap(),
            json!({ "seq": 1 })
        );
        let list = arguments_object(json!([1])).unwrap_err();
        assert!(list.message.contains("an array"), "{}", list.message);
        let text = arguments_object(json!("x")).unwrap_err();
        assert!(text.message.contains("a string"), "{}", text.message);
    }

    #[test]
    fn typed_argument_helpers_read_leniently_and_fail_readably() {
        let args = json!({ "name": "gh", "empty": "", "limit": 5, "neg": -1, "text": "x" });
        assert_eq!(nullable_string(&args, "name"), Some("gh"));
        assert_eq!(nullable_string(&args, "empty"), None);
        assert_eq!(nullable_string(&args, "missing"), None);
        assert_eq!(
            require_string(&args, "empty").unwrap_err().message,
            "empty is required"
        );
        assert_eq!(optional_u64(&args, "limit"), Ok(Some(5)));
        assert_eq!(optional_u64(&args, "missing"), Ok(None));
        assert!(
            optional_u64(&args, "neg")
                .unwrap_err()
                .message
                .contains("non-negative")
        );
        assert!(
            optional_u64(&args, "text")
                .unwrap_err()
                .message
                .contains("a string")
        );
        assert_eq!(optional_u32(&args, "limit"), Ok(Some(5)));
        assert!(
            optional_u32(&json!({ "limit": u64::MAX }), "limit")
                .unwrap_err()
                .message
                .contains("too large")
        );
    }

    #[test]
    fn ingest_urls_join_the_origin_and_the_route_once() {
        assert_eq!(
            ingest_url("https://ls.example", "/hooks/bots/u/b/t/tok"),
            "https://ls.example/hooks/bots/u/b/t/tok"
        );
        assert_eq!(
            ingest_url("https://ls.example/", "/hooks/bots/u/b/t/tok"),
            "https://ls.example/hooks/bots/u/b/t/tok"
        );
    }

    #[test]
    fn unknown_seqs_name_the_valid_range() {
        assert_eq!(
            unknown_seq_message(9, 17),
            "no event #9; this bot's events run #1..#17"
        );
        assert_eq!(
            unknown_seq_message(1, 0),
            "no event #1; this bot has no events yet"
        );
    }

    #[test]
    fn api_error_codes_are_stable_snake_case() {
        assert_eq!(
            api_error_code(AgentApiErrorKind::InvalidRequest),
            "invalid_request"
        );
        assert_eq!(api_error_code(AgentApiErrorKind::NotFound), "not_found");
        assert_eq!(api_error_code(AgentApiErrorKind::Conflict), "conflict");
        assert_eq!(api_error_code(AgentApiErrorKind::Rejected), "rejected");
        assert_eq!(
            api_error_code(AgentApiErrorKind::EnvironmentNotReady),
            "environment_not_ready"
        );
    }
}
