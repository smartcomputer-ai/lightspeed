//! Model-facing views of the `bot_*` tool results, the bot directory
//! catalog, receipts, inbox resolution, and run input items.
//!
//! The rule: the model reads and echoes `#N` and names; digest ids, uuids,
//! and session ids stay on records and the platform API. Every shape is a
//! pure function here so the guarantee is tested without a store, and a new
//! field cannot quietly bring a digest back.

use api::{
    BotControllerSnapshot, BotEventDocument, BotEventOutcome, BotEventReplyRef, BotEventSender,
    BotId, BotTriggerKind, BotTriggerSpec, ChatPairing, InputItem, MediaKind,
};
use serde_json::{Map, Value, json};

use crate::render::{DEFAULT_READ_BUDGET, iso_time, largest_branches, render_value, resolve_path};
use crate::{
    BotError, BotEvent, BotEventMediaKind, BotRecord, BotRefusalCode, BotTriggerRecord,
    ids::routed_session_base, records::BotEventRecord,
};

/// Bounds of the `bot_event_read` size cap.
pub const MIN_READ_BUDGET: usize = 256;
pub const MAX_READ_BUDGET: usize = 65_536;

fn insert_some<T: Into<Value>>(object: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value.into());
    }
}

// ── bot_status ──────────────────────────────────────────────────────────────

/// The bot as it sees itself: the authored id, labels, budgets, and the
/// controller's live snapshot reduced to labels and `#N`s. Without a
/// snapshot (no controller running yet) only the record is shown.
pub fn bot_status_view(record: &BotRecord, snapshot: Option<&BotControllerSnapshot>) -> Value {
    let document = &record.document;
    let mut bot = Map::new();
    bot.insert("botId".to_owned(), json!(record.bot_id));
    insert_some(&mut bot, "displayName", document.display_name.clone());
    insert_some(&mut bot, "description", document.description.clone());
    bot.insert("enabled".to_owned(), json!(document.enabled));
    bot.insert("closed".to_owned(), json!(record.is_closed()));
    insert_some(&mut bot, "brief", document.brief.clone());
    insert_some(&mut bot, "runsPerDay", document.runs_per_day);
    insert_some(
        &mut bot,
        "runsToday",
        snapshot.map(|snapshot| snapshot.runs_today),
    );
    insert_some(
        &mut bot,
        "breaker",
        document.breaker.map(|breaker| json!(breaker)),
    );
    insert_some(
        &mut bot,
        "routedSessionCloseAfterMs",
        document.routed_session_close_after_ms,
    );
    bot.insert("selfConfig".to_owned(), json!(document.self_config));
    bot.insert("emit".to_owned(), json!(document.emit));
    bot.insert("eventSeq".to_owned(), json!(record.event_seq));
    insert_some(
        &mut bot,
        "eventsProcessed",
        snapshot.map(|snapshot| snapshot.events_processed),
    );

    let mut view = Map::new();
    view.insert("bot".to_owned(), Value::Object(bot));
    let Some(snapshot) = snapshot else {
        view.insert("controllerStatus".to_owned(), json!("not_started"));
        view.insert("sessions".to_owned(), json!([]));
        view.insert("activeDeliveries".to_owned(), json!([]));
        view.insert("buffers".to_owned(), json!([]));
        view.insert("recentDeliveries".to_owned(), json!([]));
        return Value::Object(view);
    };
    let label_of = |session_id: &str| session_label(snapshot, session_id);
    view.insert(
        "controllerStatus".to_owned(),
        json!(snapshot.controller_status),
    );
    view.insert("setupStatus".to_owned(), json!(snapshot.setup_status));
    view.insert(
        "sessions".to_owned(),
        snapshot
            .sessions
            .iter()
            .map(|session| {
                let mut entry = Map::new();
                entry.insert("label".to_owned(), json!(session.label));
                entry.insert("kind".to_owned(), json!(session.kind));
                entry.insert("generation".to_owned(), json!(session.generation));
                entry.insert("busy".to_owned(), json!(session.busy));
                insert_some(
                    &mut entry,
                    "lastActiveAt",
                    session.last_active_at_ms.map(iso_time),
                );
                Value::Object(entry)
            })
            .collect(),
    );
    view.insert(
        "activeDeliveries".to_owned(),
        snapshot
            .active_deliveries
            .iter()
            .map(|delivery| {
                let mut entry = Map::new();
                entry.insert("events".to_owned(), json!(delivery.seqs));
                insert_some(&mut entry, "session", label_of(&delivery.session_id));
                entry.insert(
                    "startedAt".to_owned(),
                    json!(iso_time(delivery.started_at_ms)),
                );
                Value::Object(entry)
            })
            .collect(),
    );
    view.insert(
        "buffers".to_owned(),
        snapshot
            .buffers
            .iter()
            .map(|buffer| {
                let mut entry = Map::new();
                let (trigger, session) =
                    buffer.key.split_once('|').unwrap_or((&buffer.key, "main"));
                entry.insert("trigger".to_owned(), json!(trigger));
                let label = if session == "main" {
                    Some("main".to_owned())
                } else {
                    label_of(session)
                };
                insert_some(&mut entry, "session", label);
                entry.insert("count".to_owned(), json!(buffer.seqs.len()));
                entry.insert("events".to_owned(), json!(buffer.seqs));
                entry.insert("flushAt".to_owned(), json!(iso_time(buffer.flush_at_ms)));
                Value::Object(entry)
            })
            .collect(),
    );
    view.insert(
        "recentDeliveries".to_owned(),
        snapshot
            .recent_deliveries
            .iter()
            .map(|delivery| {
                let mut entry = Map::new();
                entry.insert("events".to_owned(), json!(delivery.seqs));
                insert_some(&mut entry, "session", label_of(&delivery.session_id));
                entry.insert("outcome".to_owned(), json!(delivery.outcome));
                insert_some(&mut entry, "summary", delivery.summary.clone());
                entry.insert(
                    "finishedAt".to_owned(),
                    json!(iso_time(delivery.finished_at_ms)),
                );
                Value::Object(entry)
            })
            .collect(),
    );
    insert_some(&mut view, "lastError", snapshot.last_error.clone());
    Value::Object(view)
}

/// The label of one of the controller's sessions: by exact id, by logical
/// base (a rotated generation), or `main` for the main session.
fn session_label(snapshot: &BotControllerSnapshot, session_id: &str) -> Option<String> {
    if let Some(session) = snapshot
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
    {
        return Some(session.label.clone());
    }
    let base = routed_session_base(session_id);
    if let Some(session) = snapshot
        .sessions
        .iter()
        .find(|session| routed_session_base(&session.session_id) == base)
    {
        return Some(session.label.clone());
    }
    (routed_session_base(&snapshot.main_session_id) == base).then(|| "main".to_owned())
}

// ── bot_trigger_list / bot_trigger_put ──────────────────────────────────────

/// A trigger by its name. The spec is the document's minus the kind tag; a
/// chat trigger names its account as `account` (the same id
/// `bot_trigger_put` accepts). Secrets stay out except the pairing code
/// when the caller says the reader manages the trigger (a bot under the
/// self-configuration grant has to tell the human what to send).
pub fn trigger_tool_view(
    record: &BotTriggerRecord,
    ingest_url: Option<String>,
    show_pairing_code: bool,
) -> Value {
    let document = &record.document;
    let mut spec = match serde_json::to_value(&document.spec) {
        Ok(Value::Object(spec)) => spec,
        _ => Map::new(),
    };
    spec.remove("kind");
    if let Some(account) = spec.remove("accountId") {
        spec.insert("account".to_owned(), account);
    }
    let mut view = Map::new();
    view.insert("name".to_owned(), json!(record.trigger_id));
    view.insert("kind".to_owned(), json!(record.kind()));
    view.insert("spec".to_owned(), Value::Object(spec));
    view.insert("filter".to_owned(), json!(document.filter));
    view.insert("route".to_owned(), json!(document.route));
    view.insert("coalesce".to_owned(), json!(document.coalesce));
    view.insert("deliver".to_owned(), json!(document.deliver));
    insert_some(
        &mut view,
        "sessionCloseAfterMs",
        document.session_close_after_ms,
    );
    view.insert("enabled".to_owned(), json!(document.enabled));
    insert_some(
        &mut view,
        "disabledReason",
        record.disabled_reason.map(|reason| json!(reason)),
    );
    insert_some(
        &mut view,
        "lastFilterError",
        record.last_filter_error.clone(),
    );
    if record.kind() == BotTriggerKind::Webhook {
        insert_some(&mut view, "ingestUrl", ingest_url);
    }
    if show_pairing_code
        && matches!(
            document.spec,
            BotTriggerSpec::Chat {
                pairing: ChatPairing::Code,
                ..
            }
        )
    {
        insert_some(
            &mut view,
            "pairingCode",
            record.secrets.pairing_code.clone(),
        );
    }
    Value::Object(view)
}

// ── bot_event_list / bot_filter_test / bot_event_read ───────────────────────

/// What came of the event, or `pending` while the controller still holds it.
fn outcome_fields(record: &BotEventRecord, view: &mut Map<String, Value>) {
    view.insert(
        "outcome".to_owned(),
        match record.outcome {
            Some(outcome) => json!(outcome),
            None => json!("pending"),
        },
    );
    insert_some(view, "outcomeDetail", record.outcome_detail.clone());
}

fn session_field(record: &BotEventRecord, view: &mut Map<String, Value>) {
    insert_some(
        view,
        "session",
        record.session.as_ref().map(|session| session.label.clone()),
    );
}

/// One row of `bot_event_list`: `#N`, kind, source, summary, session label,
/// outcome.
pub fn event_list_row_view(record: &BotEventRecord) -> Value {
    let mut view = Map::new();
    view.insert("seq".to_owned(), json!(record.seq));
    view.insert("kind".to_owned(), json!(record.kind));
    insert_some(
        &mut view,
        "trigger",
        record.trigger_id.as_ref().map(|trigger| json!(trigger)),
    );
    view.insert(
        "occurredAt".to_owned(),
        json!(iso_time(record.occurred_at_ms)),
    );
    view.insert(
        "receivedAt".to_owned(),
        json!(iso_time(record.received_at_ms)),
    );
    session_field(record, &mut view);
    view.insert("summary".to_owned(), json!(record.summary));
    insert_some(
        &mut view,
        "sender",
        record.sender_bot_id.as_ref().map(|bot| json!(bot)),
    );
    outcome_fields(record, &mut view);
    Value::Object(view)
}

/// One row of `bot_filter_test` over stored events.
pub fn filter_result_view(record: &BotEventRecord, matched: bool, error: Option<&str>) -> Value {
    let mut view = Map::new();
    view.insert("seq".to_owned(), json!(record.seq));
    view.insert("kind".to_owned(), json!(record.kind));
    insert_some(
        &mut view,
        "trigger",
        record.trigger_id.as_ref().map(|trigger| json!(trigger)),
    );
    view.insert("summary".to_owned(), json!(record.summary));
    view.insert("matched".to_owned(), json!(matched));
    insert_some(&mut view, "error", error.map(str::to_owned));
    Value::Object(view)
}

/// The full archived envelope behind `bot_event_read #N`, without ids.
fn envelope(record: &BotEventRecord, document: &BotEventDocument) -> Map<String, Value> {
    let mut view = Map::new();
    view.insert("seq".to_owned(), json!(record.seq));
    view.insert("kind".to_owned(), json!(record.kind));
    view.insert("source".to_owned(), json!(document.source));
    view.insert(
        "occurredAt".to_owned(),
        json!(iso_time(record.occurred_at_ms)),
    );
    view.insert(
        "receivedAt".to_owned(),
        json!(iso_time(record.received_at_ms)),
    );
    session_field(record, &mut view);
    outcome_fields(record, &mut view);
    view.insert("summary".to_owned(), json!(document.summary));
    insert_some(&mut view, "correlationId", document.correlation_id.clone());
    if !document.links.is_empty() {
        view.insert("links".to_owned(), json!(document.links));
    }
    insert_some(
        &mut view,
        "sender",
        document.sender.as_ref().map(|sender| json!(sender.bot)),
    );
    insert_some(
        &mut view,
        "inReplyTo",
        document.in_reply_to.as_ref().map(|reply| json!(reply)),
    );
    insert_some(&mut view, "data", document.data.clone());
    if !document.headers.is_empty() {
        view.insert("headers".to_owned(), json!(document.headers));
    }
    view
}

/// The `bot_event_read` result: the envelope (or the value at `path`) when
/// it fits `max_bytes` (clamped to [`MIN_READ_BUDGET`]..=[`MAX_READ_BUDGET`]),
/// otherwise an honest over-budget report with the size, a pruned preview,
/// and the largest branches so the narrowing follow-up call is obvious. An
/// unknown path is an error naming the top-level keys.
pub fn event_envelope_view(
    record: &BotEventRecord,
    document: &BotEventDocument,
    path: Option<&str>,
    max_bytes: usize,
) -> Result<Value, String> {
    let envelope = Value::Object(envelope(record, document));
    let path = path.filter(|path| !path.is_empty());
    let target = match path {
        None => &envelope,
        Some(path) => resolve_path(&envelope, path).ok_or_else(|| {
            let keys: Vec<&str> = envelope
                .as_object()
                .map(|object| object.keys().map(String::as_str).collect())
                .unwrap_or_default();
            format!(
                "path {path:?} not found in event #{}; top-level keys: {}",
                record.seq,
                keys.join(", ")
            )
        })?,
    };
    let max_bytes = max_bytes.clamp(MIN_READ_BUDGET, MAX_READ_BUDGET);
    let json = serde_json::to_string(target).map_err(|error| error.to_string())?;
    let mut view = Map::new();
    view.insert("seq".to_owned(), json!(record.seq));
    insert_some(&mut view, "path", path.map(str::to_owned));
    if json.len() <= max_bytes {
        view.insert("value".to_owned(), target.clone());
        return Ok(Value::Object(view));
    }
    view.insert("truncated".to_owned(), json!(true));
    view.insert("bytes".to_owned(), json!(json.len()));
    view.insert(
        "preview".to_owned(),
        json!(render_value(target, max_bytes).text),
    );
    view.insert(
        "largest".to_owned(),
        largest_branches(target, 5)
            .into_iter()
            .map(|mut branch| {
                if let Some(path) = path {
                    branch.path = format!("{path}.{}", branch.path);
                }
                branch.to_value()
            })
            .collect(),
    );
    view.insert(
        "hint".to_owned(),
        json!(format!(
            "narrow with path or raise maxBytes (max {MAX_READ_BUDGET})"
        )),
    );
    Ok(Value::Object(view))
}

/// The default `bot_event_read` budget when the model gives none.
pub fn read_budget(requested: Option<u64>) -> usize {
    requested
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_READ_BUDGET)
        .clamp(MIN_READ_BUDGET, MAX_READ_BUDGET)
}

// ── Bot directory ───────────────────────────────────────────────────────────

pub const BOT_DIRECTORY_KEY: &str = "bot:directory";
pub const BOT_DIRECTORY_TITLE: &str = "Bot directory";

/// What the directory says about one neighbour that accepts the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub bot_id: BotId,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

/// Whether `inbox` (the target's `bot`-kind trigger, if any) accepts events
/// from `sender`, with the reason it does not.
fn inbox_refusal(
    target: &BotRecord,
    inbox: Option<&BotTriggerRecord>,
    sender: &BotId,
) -> Option<BotError> {
    let name = &target.bot_id;
    if target.is_closed() {
        return Some(BotError::refused(
            BotRefusalCode::BotClosed,
            format!("{name} was closed and no longer accepts events"),
        ));
    }
    if !target.document.enabled {
        return Some(BotError::refused(
            BotRefusalCode::BotDisabled,
            format!("{name} is disabled"),
        ));
    }
    let Some(inbox) = inbox.filter(|inbox| inbox.kind() == BotTriggerKind::Bot) else {
        return Some(BotError::refused(
            BotRefusalCode::NoInbox,
            format!(
                "{name} has no enabled inbox (a trigger of kind bot) for events from other bots"
            ),
        ));
    };
    if !inbox.enabled() {
        return Some(BotError::refused(
            BotRefusalCode::TriggerDisabled,
            format!("{name}'s inbox is disabled"),
        ));
    }
    if let BotTriggerSpec::Bot { from: Some(from) } = &inbox.document.spec
        && !from.contains(sender)
    {
        return Some(BotError::refused(
            BotRefusalCode::NotAccepted,
            format!("{name}'s inbox does not accept events from {sender}"),
        ));
    }
    None
}

/// Only the neighbours whose inbox accepts `me`: bots that are not listening
/// cost context and help nobody. Ordered by bot id; never the reader itself.
pub fn directory_entries_for(
    me: &BotId,
    bots: &[(BotRecord, Option<BotTriggerRecord>)],
) -> Vec<DirectoryEntry> {
    let mut entries: Vec<DirectoryEntry> = bots
        .iter()
        .filter(|(bot, _)| &bot.bot_id != me)
        .filter(|(bot, inbox)| inbox_refusal(bot, inbox.as_ref(), me).is_none())
        .map(|(bot, _)| DirectoryEntry {
            bot_id: bot.bot_id.clone(),
            display_name: bot.document.display_name.clone(),
            description: bot.document.description.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.bot_id.cmp(&b.bot_id));
    entries
}

/// The catalog body: one line per bot that accepts events from the reader.
pub fn render_bot_directory(entries: &[DirectoryEntry]) -> String {
    if entries.is_empty() {
        return "No other bot accepts events from you right now.".to_owned();
    }
    let mut lines = vec!["Bots that accept events addressed by you (bot_emit with to):".to_owned()];
    for entry in entries {
        let mut line = format!("- {}", entry.bot_id);
        if let Some(display_name) = &entry.display_name
            && display_name != entry.bot_id.as_str()
        {
            line.push_str(&format!(" ({display_name})"));
        }
        if let Some(description) = &entry.description {
            line.push_str(&format!(" — {description}"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// The directory as the catalog item a controller appends to its sessions.
pub fn bot_directory_item(entries: &[DirectoryEntry]) -> InputItem {
    InputItem::Catalog {
        title: BOT_DIRECTORY_TITLE.to_owned(),
        text: render_bot_directory(entries),
    }
}

// ── Addressed emits ─────────────────────────────────────────────────────────

/// Resolve the inbox an addressed emit goes through: the target must be
/// open and enabled, declare an enabled inbox, and list the sender (or
/// nobody). Refusals are typed so the sending model can read why; a closed
/// bot has its own code so the sender stops retrying. `UnknownBot` is the
/// caller's (it has the lookup).
pub fn resolve_inbox<'a>(
    target: &BotRecord,
    inbox: Option<&'a BotTriggerRecord>,
    sender: &BotId,
) -> Result<&'a BotTriggerRecord, BotError> {
    match inbox_refusal(target, inbox, sender) {
        Some(error) => Err(error),
        None => Ok(inbox.expect("an accepting inbox exists")),
    }
}

// ── Receipts ────────────────────────────────────────────────────────────────

/// The deterministic receipt for one asked event: the delivery's outcome,
/// never a model-authored reply. Kind `bot.reply`, addressed from the
/// answering bot, correlated by the asked event's `#N` there.
pub fn receipt_document(
    answering_bot: &BotId,
    outcome: BotEventOutcome,
    summary: Option<&str>,
    in_reply_to_seq: u64,
    hops: u32,
    now_ms: i64,
) -> BotEventDocument {
    BotEventDocument {
        version: BotEventDocument::VERSION,
        kind: "bot.reply".to_owned(),
        source: format!("bot:{answering_bot}"),
        occurred_at_ms: now_ms,
        summary: summary
            .map(str::to_owned)
            .unwrap_or_else(|| format!("#{in_reply_to_seq} at {answering_bot} finished {outcome}")),
        data: Some(json!({ "status": outcome })),
        headers: Default::default(),
        correlation_id: None,
        links: Vec::new(),
        sender: Some(BotEventSender {
            bot: answering_bot.clone(),
        }),
        hops,
        in_reply_to: Some(BotEventReplyRef {
            bot: answering_bot.clone(),
            seq: in_reply_to_seq,
        }),
    }
}

// ── Run input ───────────────────────────────────────────────────────────────

fn media_kind(kind: BotEventMediaKind) -> MediaKind {
    match kind {
        BotEventMediaKind::Image => MediaKind::Image,
        BotEventMediaKind::Audio => MediaKind::Audio,
        BotEventMediaKind::Document => MediaKind::Document,
    }
}

/// One event's items: its rendering (the envelope when none was stored)
/// and its media.
fn event_input_items(event: &BotEvent) -> impl Iterator<Item = InputItem> + '_ {
    std::iter::once(InputItem::TextRef {
        provenance_ref: None,
        origin: Some("event".to_owned()),
        blob_ref: event
            .prompt_ref
            .clone()
            .unwrap_or_else(|| event.document_ref.clone()),
    })
    .chain(event.media.iter().map(|item| {
        if let Some(text_ref) = &item.text_ref {
            InputItem::TextRef {
                provenance_ref: Some(item.blob_ref.clone()),
                origin: Some("event".into()),
                blob_ref: text_ref.clone(),
            }
        } else {
            InputItem::Media {
                origin: Some("event".to_owned()),
                blob_ref: item.blob_ref.clone(),
                mime: item.mime.clone(),
                kind: media_kind(item.kind),
                name: item.name.clone(),
            }
        }
    }))
}

/// A delivery is the event renderings themselves — the standing protocol
/// (untrusted content, resolve semantics) lives in the session
/// instructions, so a single event needs no framing item at all. Only a
/// batch gets a one-line header binding it to one decision.
pub fn delivery_input_items(events: &[BotEvent]) -> Vec<InputItem> {
    let mut items = Vec::new();
    if events.len() > 1 {
        items.push(InputItem::Text { provenance_ref: None, origin: Some("event".to_owned()),
            text: format!(
                "{} events delivered as one batch — handle them together and resolve the delivery once.",
                events.len()
            ),
        });
    }
    items.extend(events.iter().flat_map(event_input_items));
    items
}

/// Steering input for events folded into a running run.
pub fn steer_input_items(events: &[BotEvent]) -> Vec<InputItem> {
    let mut items = vec![InputItem::Text {
        provenance_ref: None,
        origin: Some("event".to_owned()),
        text: format!(
            "{} more event(s) arrived while you were working — fold them into your current work where relevant.",
            events.len()
        ),
    }];
    items.extend(events.iter().flat_map(event_input_items));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{BotTriggerSecrets, EventReceiver, RoutedSession};
    use api::{
        BotActiveDeliverySnapshot, BotBreaker, BotBufferSnapshot, BotDocument, BotEventMedia,
        BotRecentDeliverySnapshot, BotSessionKind, BotSessionSnapshot, BotTriggerDocument,
        BotTriggerId, BotTriggerRoute, ProfileId, WebhookPreset, WebhookVerification,
    };

    const T0: i64 = 1_787_738_400_000; // 2026-08-26T10:00:00Z

    /// The §8 guarantee: nothing the model reads carries a uuid or a digest.
    fn assert_no_ids(value: &Value) {
        let json = serde_json::to_string(value).unwrap();
        assert!(
            !contains_hex_run(&json, 32),
            "digest in model-facing view: {json}"
        );
        assert!(!contains_uuid(&json), "uuid in model-facing view: {json}");
    }

    fn contains_hex_run(text: &str, min: usize) -> bool {
        let mut run = 0;
        for ch in text.chars() {
            if ch.is_ascii_hexdigit() {
                run += 1;
                if run >= min {
                    return true;
                }
            } else {
                run = 0;
            }
        }
        false
    }

    fn contains_uuid(text: &str) -> bool {
        let bytes = text.as_bytes();
        let is_hex = |index: usize| bytes.get(index).is_some_and(u8::is_ascii_hexdigit);
        (0..bytes.len().saturating_sub(35)).any(|start| {
            [8usize, 4, 4, 4, 12]
                .iter()
                .scan(start, |offset, len| {
                    let ok = (*offset..*offset + len).all(is_hex);
                    let dash = *offset + len;
                    let sep_ok = *len == 12 || bytes.get(dash) == Some(&b'-');
                    *offset = dash + 1;
                    Some(ok && sep_ok)
                })
                .all(|ok| ok)
        })
    }

    fn bot(id: &str) -> BotRecord {
        BotRecord {
            bot_id: BotId::new(id),
            revision: 3,
            document: BotDocument {
                display_name: Some("Triage".to_owned()),
                description: Some("Routes incidents to the right bot.".to_owned()),
                profile_id: ProfileId::new("profile_0123456789abcdef0123456789abcdef"),
                brief: Some("Watch the queue.".to_owned()),
                runs_per_day: Some(20),
                breaker: Some(BotBreaker {
                    fires: 60,
                    window_ms: 3_600_000,
                }),
                routed_session_close_after_ms: None,
                self_config: true,
                emit: true,
                enabled: true,
            },
            event_seq: 17,
            closed_at_ms: None,
            closed_sessions: vec!["bot:v1:triage:k-old-0123abcd".to_owned()],
            created_at_ms: T0,
            updated_at_ms: T0,
        }
    }

    fn trigger(bot_id: &str, trigger_id: &str, spec: BotTriggerSpec) -> BotTriggerRecord {
        BotTriggerRecord {
            bot_id: BotId::new(bot_id),
            trigger_id: BotTriggerId::new(trigger_id),
            revision: 1,
            document: BotTriggerDocument {
                spec,
                filter: None,
                route: None,
                coalesce: None,
                deliver: None,
                session_close_after_ms: None,
                enabled: true,
            },
            secrets: BotTriggerSecrets {
                webhook_token: Some("a".repeat(48)),
                pairing_code: Some("Pair-Code-42".to_owned()),
            },
            disabled_reason: None,
            disabled_at_ms: None,
            last_filter_error: None,
            last_filter_error_at_ms: None,
            cursor: None,
            created_at_ms: T0,
            updated_at_ms: T0,
        }
    }

    fn inbox(from: Option<&[&str]>) -> BotTriggerRecord {
        trigger(
            "infra",
            "inbox",
            BotTriggerSpec::Bot {
                from: from.map(|ids| ids.iter().map(|id| BotId::new(*id)).collect()),
            },
        )
    }

    fn event(seq: u64, event_id: &str) -> BotEventRecord {
        BotEventRecord {
            bot_id: BotId::new("triage"),
            event_id: event_id.to_owned(),
            seq,
            trigger_id: Some(BotTriggerId::new("github")),
            kind: "pull_request.opened".to_owned(),
            summary: "PR #12 opened".to_owned(),
            occurred_at_ms: T0,
            received_at_ms: T0 + 1_000,
            document_ref: format!("sha256:{}", "b".repeat(64)),
            prompt_ref: Some(format!("sha256:{}", "c".repeat(64))),
            session: Some(RoutedSession {
                session_id: "bot:v1:triage:k-pr-12-0123abcd".to_owned(),
                label: "pr-12".to_owned(),
                session_key: None,
                close_policy: Default::default(),
            }),
            sender_bot_id: None,
            hops: 0,
            in_reply_to: None,
            media: Vec::new(),
            receiver: None,
            outcome: None,
            outcome_detail: None,
            run_id: None,
            resolved_at_ms: None,
        }
    }

    fn events() -> Vec<BotEventRecord> {
        let poll = event(13, &format!("poll:github:{}", "d".repeat(32)));
        let mut schedule = event(14, "schedule:nightly:1787738400000");
        schedule.session = None;
        schedule.outcome = Some(BotEventOutcome::Handled);
        schedule.outcome_detail = Some("done".to_owned());
        schedule.run_id = Some("run_0123456789abcdef0123456789abcdef".to_owned());
        let mut addressed = event(16, &format!("bot:triage:{}", "e".repeat(64)));
        addressed.sender_bot_id = Some(BotId::new("infra"));
        addressed.hops = 2;
        addressed.receiver = Some(EventReceiver::Bot {
            bot_id: BotId::new("infra"),
            session: Some(RoutedSession {
                session_id: "bot:v1:infra:k-inc-7-abcd1234".to_owned(),
                label: "inc-7".to_owned(),
                session_key: None,
                close_policy: Default::default(),
            }),
        });
        let mut reply = event(17, &format!("reply:infra:{}", "f".repeat(64)));
        reply.kind = "bot.reply".to_owned();
        reply.sender_bot_id = Some(BotId::new("infra"));
        reply.hops = 3;
        reply.in_reply_to = Some(BotEventReplyRef {
            bot: BotId::new("infra"),
            seq: 9,
        });
        vec![
            event(12, &format!("whk-{}", "a".repeat(64))),
            poll,
            schedule,
            addressed,
            reply,
        ]
    }

    fn document() -> BotEventDocument {
        BotEventDocument {
            version: 1,
            kind: "pull_request.opened".to_owned(),
            source: "webhook:github".to_owned(),
            occurred_at_ms: T0,
            summary: "PR #12 opened".to_owned(),
            data: Some(json!({ "number": 12, "title": "Fix" })),
            headers: [("x-github-event".to_owned(), "pull_request".to_owned())]
                .into_iter()
                .collect(),
            correlation_id: None,
            links: vec!["https://github.com/acme/repo/pull/12".to_owned()],
            sender: None,
            hops: 0,
            in_reply_to: None,
        }
    }

    fn snapshot() -> BotControllerSnapshot {
        BotControllerSnapshot {
            controller_status: api::BotControllerStatus::DeliveringEvent,
            setup_status: api::BotSetupStatus::Ready,
            enabled: true,
            closed: false,
            main_session_id: "bot:v1:triage-g2".to_owned(),
            sessions: vec![
                BotSessionSnapshot {
                    session_id: "bot:v1:triage-g2".to_owned(),
                    label: "main".to_owned(),
                    kind: BotSessionKind::Main,
                    generation: 2,
                    last_active_at_ms: Some(T0),
                    busy: false,
                },
                BotSessionSnapshot {
                    session_id: "bot:v1:triage:k-pr-12-0123abcd".to_owned(),
                    label: "pr-12".to_owned(),
                    kind: BotSessionKind::PerKey,
                    generation: 1,
                    last_active_at_ms: None,
                    busy: true,
                },
            ],
            pending_deliveries: 1,
            buffers: vec![BotBufferSnapshot {
                key: "github|main".to_owned(),
                seqs: vec![15, 16, 17],
                first_at_ms: T0,
                last_at_ms: T0,
                flush_at_ms: T0 + 400,
            }],
            active_deliveries: vec![BotActiveDeliverySnapshot {
                delivery_id: format!("batch-{}", "e".repeat(64)),
                seqs: vec![12, 13],
                session_id: "bot:v1:triage:k-pr-12-0123abcd-g3".to_owned(),
                run_id: Some("run_0123456789abcdef0123456789abcdef".to_owned()),
                started_at_ms: T0,
            }],
            recent_deliveries: vec![BotRecentDeliverySnapshot {
                delivery_id: format!("whk-{}", "a".repeat(64)),
                seqs: vec![11],
                session_id: "bot:v1:triage".to_owned(),
                run_id: Some("run_0123456789abcdef0123456789abcdef".to_owned()),
                outcome: BotEventOutcome::Handled,
                summary: Some("merged".to_owned()),
                finished_at_ms: T0,
                usage: None,
            }],
            run_day: Some("2026-08-26".to_owned()),
            runs_today: 4,
            descendants_today: 1,
            events_processed: 16,
            duplicate_events: 0,
            applied_profile_revision: Some(3),
            last_error: None,
        }
    }

    #[test]
    fn id_guards_detect_uuids_and_digests() {
        assert!(contains_uuid("x 0b54d227-08a2-45a8-9b3f-6a4c21d1a222 y"));
        assert!(!contains_uuid("0b54d227-08a2-45a8-9b3f-6a4c21d1a22"));
        assert!(!contains_uuid("bot:v1:triage:k-pr-12-0123abcd"));
        assert!(contains_hex_run(&format!("sha256:{}", "b".repeat(64)), 32));
        assert!(contains_hex_run(&"d".repeat(32), 32));
        assert!(!contains_hex_run("k-pr-12-0123abcd-g3 deadbeef", 32));
    }

    #[test]
    fn shows_status_as_the_authored_id_labels_and_seqs() {
        let view = bot_status_view(&bot("triage"), Some(&snapshot()));
        assert_no_ids(&view);
        assert_eq!(view["bot"]["botId"], json!("triage"));
        assert_eq!(view["bot"]["displayName"], json!("Triage"));
        assert_eq!(view["bot"]["runsToday"], json!(4));
        assert_eq!(view["bot"]["eventsProcessed"], json!(16));
        assert_eq!(
            view["bot"]["breaker"],
            json!({ "fires": 60, "windowMs": 3_600_000 })
        );
        assert!(view["bot"].get("profileId").is_none());
        assert!(view.get("invocation").is_none());
        assert_eq!(view["controllerStatus"], json!("delivering_event"));
        assert_eq!(view["sessions"][0]["label"], json!("main"));
        assert_eq!(view["sessions"][0]["kind"], json!("main"));
        assert_eq!(view["sessions"][1]["busy"], json!(true));
        assert!(view["sessions"][0].get("sessionId").is_none());
        // Deliveries are named by their #Ns and the session label, a rotated
        // generation included.
        assert_eq!(
            view["activeDeliveries"][0],
            json!({ "events": [12, 13], "session": "pr-12", "startedAt": "2026-08-26T10:00:00.000Z" })
        );
        assert_eq!(view["buffers"][0]["session"], json!("main"));
        assert_eq!(view["buffers"][0]["trigger"], json!("github"));
        assert_eq!(view["buffers"][0]["count"], json!(3));
        assert_eq!(view["recentDeliveries"][0]["session"], json!("main"));
        assert_eq!(view["recentDeliveries"][0]["outcome"], json!("handled"));
        assert!(view["recentDeliveries"][0].get("runId").is_none());

        let dormant = bot_status_view(&bot("triage"), None);
        assert_no_ids(&dormant);
        assert_eq!(dormant["controllerStatus"], json!("not_started"));
        assert_eq!(dormant["sessions"], json!([]));
        assert!(dormant["bot"].get("runsToday").is_none());
    }

    #[test]
    fn lists_events_by_seq_and_session_label_never_by_event_id() {
        for record in events() {
            let view = event_list_row_view(&record);
            assert_no_ids(&view);
            assert!(view.get("eventId").is_none());
            assert!(view.get("deliveryId").is_none());
            assert_eq!(view["seq"], json!(record.seq));
            assert_eq!(view["occurredAt"], json!("2026-08-26T10:00:00.000Z"));
            assert_eq!(view["receivedAt"], json!("2026-08-26T10:00:01.000Z"));
            match &record.session {
                None => assert!(view.get("session").is_none()),
                Some(session) => assert_eq!(view["session"], json!(session.label)),
            }
            match record.outcome {
                None => assert_eq!(view["outcome"], json!("pending")),
                Some(outcome) => assert_eq!(view["outcome"], json!(outcome)),
            }
            match &record.sender_bot_id {
                None => assert!(view.get("sender").is_none()),
                Some(sender) => assert_eq!(view["sender"], json!(sender)),
            }
        }
    }

    #[test]
    fn reads_an_envelope_without_ids() {
        let document = document();
        for record in events() {
            let view = event_envelope_view(&record, &document, None, DEFAULT_READ_BUDGET).unwrap();
            assert_no_ids(&view);
            assert_eq!(view["seq"], json!(record.seq));
            let value = &view["value"];
            assert!(value.get("eventId").is_none());
            assert_eq!(value["summary"], json!("PR #12 opened"));
            assert_eq!(value["data"], document.data.clone().unwrap());
            assert_eq!(value["links"], json!(document.links));
            assert_eq!(value["headers"]["x-github-event"], json!("pull_request"));
            assert!(view.get("truncated").is_none());
        }
        let mut reply = document.clone();
        reply.sender = Some(BotEventSender {
            bot: BotId::new("infra"),
        });
        reply.in_reply_to = Some(BotEventReplyRef {
            bot: BotId::new("infra"),
            seq: 9,
        });
        let view = event_envelope_view(&events()[4], &reply, None, DEFAULT_READ_BUDGET).unwrap();
        assert_eq!(view["value"]["sender"], json!("infra"));
        assert_eq!(
            view["value"]["inReplyTo"],
            json!({ "bot": "infra", "seq": 9 })
        );
    }

    #[test]
    fn narrows_the_envelope_by_path_and_names_the_keys_on_a_miss() {
        let record = event(12, "whk-x");
        let view = event_envelope_view(
            &record,
            &document(),
            Some("data.title"),
            DEFAULT_READ_BUDGET,
        )
        .unwrap();
        assert_eq!(
            view,
            json!({ "seq": 12, "path": "data.title", "value": "Fix" })
        );
        let headers =
            event_envelope_view(&record, &document(), Some("headers"), DEFAULT_READ_BUDGET)
                .unwrap();
        assert_eq!(
            headers["value"],
            json!({ "x-github-event": "pull_request" })
        );
        // An empty path is no path.
        assert!(event_envelope_view(&record, &document(), Some(""), DEFAULT_READ_BUDGET).unwrap()["value"].is_object());
        let error =
            event_envelope_view(&record, &document(), Some("data.body"), DEFAULT_READ_BUDGET)
                .unwrap_err();
        assert!(
            error.contains("\"data.body\" not found in event #12"),
            "{error}"
        );
        assert!(error.contains("top-level keys: "), "{error}");
        assert!(error.contains("data"), "{error}");
        assert!(error.contains("summary"), "{error}");
    }

    #[test]
    fn reports_an_over_budget_read_honestly() {
        let record = event(12, "whk-x");
        let mut document = document();
        let commits: Vec<Value> = (0..200)
            .map(|index| json!({ "message": format!("commit {index}"), "sha": "not-hex" }))
            .collect();
        document.data = Some(json!({ "commits": commits, "ref": "refs/heads/main" }));
        let view = event_envelope_view(&record, &document, Some("data"), 300).unwrap();
        assert_no_ids(&view);
        assert_eq!(view["truncated"], json!(true));
        assert_eq!(view["path"], json!("data"));
        assert!(view["bytes"].as_u64().unwrap() > 300);
        assert!(view["preview"].as_str().unwrap().ends_with("(truncated)"));
        assert!(view["preview"].as_str().unwrap().len() <= 300);
        assert_eq!(view["largest"][0]["path"], json!("data.commits"));
        assert_eq!(view["largest"][0]["items"], json!(200));
        assert!(view["hint"].as_str().unwrap().contains("65536"));
        assert!(view.get("value").is_none());
        // The whole envelope, unprefixed branches.
        let whole = event_envelope_view(&record, &document, None, 0).unwrap();
        assert_eq!(whole["largest"][0]["path"], json!("data"));
        assert!(whole.get("path").is_none());
        // A generous budget returns the value.
        let fits = event_envelope_view(&record, &document, Some("data"), MAX_READ_BUDGET).unwrap();
        assert!(fits.get("truncated").is_none());
        assert_eq!(fits["value"]["ref"], json!("refs/heads/main"));
        assert_eq!(read_budget(None), DEFAULT_READ_BUDGET);
        assert_eq!(read_budget(Some(1)), MIN_READ_BUDGET);
        assert_eq!(read_budget(Some(1 << 40)), MAX_READ_BUDGET);
    }

    #[test]
    fn reports_filter_results_by_seq() {
        for record in events() {
            let view = filter_result_view(&record, true, None);
            assert_no_ids(&view);
            assert!(view.get("eventId").is_none());
            assert_eq!(view["matched"], json!(true));
            assert!(view.get("error").is_none());
        }
        let view = filter_result_view(&events()[0], false, Some("no such field"));
        assert_eq!(view["seq"], json!(12));
        assert_eq!(view["matched"], json!(false));
        assert_eq!(view["error"], json!("no such field"));
        assert_eq!(view["summary"], json!("PR #12 opened"));
    }

    #[test]
    fn shows_triggers_by_name_with_the_ingest_url_never_the_secrets() {
        let mut webhook = trigger(
            "triage",
            "github",
            BotTriggerSpec::Webhook {
                verification: WebhookVerification::Token,
                preset: Some(WebhookPreset::Github),
            },
        );
        webhook.document.filter = Some("event.kind.startsWith(\"pull_request\")".to_owned());
        webhook.document.route = Some(BotTriggerRoute::PerKey {
            key: Some("data.number".to_owned()),
        });
        let view = trigger_tool_view(
            &webhook,
            Some("https://example.test/hooks/bots/u-1/triage/github/tok-1234".to_owned()),
            false,
        );
        assert_no_ids(&view);
        assert!(view.get("id").is_none());
        assert!(view.get("botId").is_none());
        assert!(view.get("cursor").is_none());
        assert_eq!(view["name"], json!("github"));
        assert_eq!(view["kind"], json!("webhook"));
        assert_eq!(
            view["spec"],
            json!({ "verification": { "scheme": "token" }, "preset": "github" })
        );
        assert_eq!(
            view["route"],
            json!({ "policy": "perKey", "key": "data.number" })
        );
        assert_eq!(
            view["filter"],
            json!("event.kind.startsWith(\"pull_request\")")
        );
        assert_eq!(view["coalesce"], Value::Null);
        assert!(view.get("sessionCloseAfterMs").is_none());
        assert!(view["ingestUrl"].as_str().unwrap().contains("/hooks/bots/"));
        assert!(
            !serde_json::to_string(&view)
                .unwrap()
                .contains(&"a".repeat(48))
        );

        let schedule = trigger(
            "triage",
            "nightly",
            BotTriggerSpec::Schedule {
                cron: Some("0 3 * * *".to_owned()),
                at_ms: None,
                timezone: "UTC".to_owned(),
                summary: "Triage overnight".to_owned(),
            },
        );
        let view = trigger_tool_view(&schedule, Some("https://ignored".to_owned()), true);
        assert!(view.get("ingestUrl").is_none());
        assert!(view.get("pairingCode").is_none());
        assert_eq!(view["spec"]["cron"], json!("0 3 * * *"));
        assert!(view["spec"].get("kind").is_none());
    }

    #[test]
    fn chat_triggers_show_the_account_id_and_the_code_only_to_managers() {
        let mut chat = trigger(
            "triage",
            "tg",
            BotTriggerSpec::Chat {
                account_id: "tg-main".to_owned(),
                match_scope: None,
                activation: Default::default(),
                access: Default::default(),
                pairing: ChatPairing::Code,
                priority: 100,
            },
        );
        chat.document.session_close_after_ms = Some(0);
        chat.disabled_reason = Some(api::BotTriggerDisabledReason::Breaker);
        chat.document.enabled = false;
        let managed = trigger_tool_view(&chat, None, true);
        assert_no_ids(&managed);
        assert_eq!(managed["spec"]["account"], json!("tg-main"));
        assert!(managed["spec"].get("accountId").is_none());
        assert_eq!(managed["pairingCode"], json!("Pair-Code-42"));
        assert_eq!(managed["sessionCloseAfterMs"], json!(0));
        assert_eq!(managed["enabled"], json!(false));
        assert_eq!(managed["disabledReason"], json!("breaker"));
        let redacted = trigger_tool_view(&chat, None, false);
        assert!(redacted.get("pairingCode").is_none());
        // An open connection has no code to show.
        if let BotTriggerSpec::Chat { pairing, .. } = &mut chat.document.spec {
            *pairing = ChatPairing::Open;
        }
        assert!(
            trigger_tool_view(&chat, None, true)
                .get("pairingCode")
                .is_none()
        );
    }

    #[test]
    fn resolves_the_target_inbox_when_it_accepts_the_sender() {
        let sender = BotId::new("triage");
        let infra = bot("infra");
        let open = inbox(None);
        assert_eq!(
            resolve_inbox(&infra, Some(&open), &sender)
                .unwrap()
                .trigger_id,
            open.trigger_id
        );
        let listed = inbox(Some(&["ops", "triage"]));
        assert!(resolve_inbox(&infra, Some(&listed), &sender).is_ok());
    }

    fn refusal(result: Result<&BotTriggerRecord, BotError>) -> BotRefusalCode {
        match result {
            Err(BotError::Refused { code, .. }) => code,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn refuses_with_a_typed_code_for_every_admission_failure() {
        let sender = BotId::new("triage");
        // Closed is terminal and wins over disabled: a sender should stop, not retry.
        let mut closed = bot("infra");
        closed.closed_at_ms = Some(T0);
        closed.document.enabled = false;
        assert_eq!(
            refusal(resolve_inbox(&closed, Some(&inbox(None)), &sender)),
            BotRefusalCode::BotClosed
        );
        let mut disabled = bot("infra");
        disabled.document.enabled = false;
        assert_eq!(
            refusal(resolve_inbox(&disabled, Some(&inbox(None)), &sender)),
            BotRefusalCode::BotDisabled
        );
        let infra = bot("infra");
        assert_eq!(
            refusal(resolve_inbox(&infra, None, &sender)),
            BotRefusalCode::NoInbox
        );
        let webhook = trigger(
            "infra",
            "hook",
            BotTriggerSpec::Webhook {
                verification: WebhookVerification::Token,
                preset: None,
            },
        );
        assert_eq!(
            refusal(resolve_inbox(&infra, Some(&webhook), &sender)),
            BotRefusalCode::NoInbox
        );
        let mut paused = inbox(None);
        paused.document.enabled = false;
        assert_eq!(
            refusal(resolve_inbox(&infra, Some(&paused), &sender)),
            BotRefusalCode::TriggerDisabled
        );
        let error = resolve_inbox(&infra, Some(&inbox(Some(&["ops"]))), &sender).unwrap_err();
        assert!(matches!(
            error,
            BotError::Refused {
                code: BotRefusalCode::NotAccepted,
                ..
            }
        ));
        assert!(error.to_string().contains("not_accepted"), "{error}");
        assert!(error.to_string().contains("triage"), "{error}");
    }

    fn neighbour(
        id: &str,
        enabled: bool,
        description: Option<&str>,
        inbox: Option<(bool, Option<&[&str]>)>,
    ) -> (BotRecord, Option<BotTriggerRecord>) {
        let mut record = bot(id);
        record.document.display_name = None;
        record.document.enabled = enabled;
        record.document.description = description.map(str::to_owned);
        let inbox = inbox.map(|(enabled, from)| {
            let mut trigger = trigger(
                id,
                "inbox",
                BotTriggerSpec::Bot {
                    from: from.map(|ids| ids.iter().map(|id| BotId::new(*id)).collect()),
                },
            );
            trigger.document.enabled = enabled;
            trigger
        });
        (record, inbox)
    }

    fn neighbours() -> Vec<(BotRecord, Option<BotTriggerRecord>)> {
        let mut closed = neighbour("gone", true, Some("Closed."), Some((true, None)));
        closed.0.closed_at_ms = Some(T0);
        vec![
            neighbour("triage", true, Some("me"), Some((true, None))),
            neighbour(
                "infra",
                true,
                Some("Investigates incidents."),
                Some((true, Some(&["triage"]))),
            ),
            neighbour("comms", true, None, Some((true, None))),
            neighbour(
                "ops",
                true,
                Some("Not for me."),
                Some((true, Some(&["comms"]))),
            ),
            neighbour("paused", false, Some("Disabled."), Some((true, None))),
            neighbour("deaf", true, Some("No inbox."), None),
            neighbour("muted", true, Some("Inbox paused."), Some((false, None))),
            closed,
        ]
    }

    #[test]
    fn lists_only_bots_whose_inbox_accepts_the_reader_never_the_reader_itself() {
        let entries = directory_entries_for(&BotId::new("triage"), &neighbours());
        assert_eq!(
            entries,
            vec![
                DirectoryEntry {
                    bot_id: BotId::new("comms"),
                    display_name: None,
                    description: None,
                },
                DirectoryEntry {
                    bot_id: BotId::new("infra"),
                    display_name: None,
                    description: Some("Investigates incidents.".to_owned()),
                },
            ]
        );
        let for_comms: Vec<String> = directory_entries_for(&BotId::new("comms"), &neighbours())
            .into_iter()
            .map(|entry| entry.bot_id.to_string())
            .collect();
        assert_eq!(for_comms, vec!["ops".to_owned(), "triage".to_owned()]);
    }

    #[test]
    fn renders_one_line_per_bot_and_says_so_when_nobody_listens() {
        let text =
            render_bot_directory(&directory_entries_for(&BotId::new("triage"), &neighbours()));
        assert!(text.starts_with("Bots that accept events addressed by you (bot_emit with to):\n"));
        assert!(text.contains("- comms\n"), "{text}");
        assert!(
            text.ends_with("- infra — Investigates incidents."),
            "{text}"
        );
        assert!(!text.contains("ops"));
        assert!(!text.contains("gone"));
        assert!(!contains_hex_run(&text, 32));
        assert_eq!(
            render_bot_directory(&[]),
            "No other bot accepts events from you right now."
        );
        let named = render_bot_directory(&[DirectoryEntry {
            bot_id: BotId::new("infra"),
            display_name: Some("Infra Desk".to_owned()),
            description: Some("Incidents.".to_owned()),
        }]);
        assert!(
            named.ends_with("- infra (Infra Desk) — Incidents."),
            "{named}"
        );
        assert_eq!(
            bot_directory_item(&[]),
            InputItem::Catalog {
                title: "Bot directory".to_owned(),
                text: "No other bot accepts events from you right now.".to_owned(),
            }
        );
        assert_eq!(BOT_DIRECTORY_KEY, "bot:directory");
    }

    #[test]
    fn builds_a_deterministic_receipt_from_the_delivery_outcome() {
        let document = receipt_document(
            &BotId::new("infra"),
            BotEventOutcome::Handled,
            Some("root cause: bad deploy"),
            17,
            2,
            T0,
        );
        assert_eq!(document.version, 1);
        assert_eq!(document.kind, "bot.reply");
        assert_eq!(document.source, "bot:infra");
        assert_eq!(document.summary, "root cause: bad deploy");
        assert_eq!(document.data, Some(json!({ "status": "handled" })));
        assert_eq!(
            document.sender.as_ref().map(|sender| sender.bot.as_str()),
            Some("infra")
        );
        assert_eq!(document.hops, 2);
        assert_eq!(document.occurred_at_ms, T0);
        assert_eq!(
            document.in_reply_to,
            Some(BotEventReplyRef {
                bot: BotId::new("infra"),
                seq: 17,
            })
        );
        assert_eq!(
            receipt_document(
                &BotId::new("infra"),
                BotEventOutcome::Appended,
                None,
                3,
                1,
                T0
            )
            .summary,
            "#3 at infra finished appended"
        );
        assert_eq!(
            receipt_document(
                &BotId::new("infra"),
                BotEventOutcome::RunFailed,
                None,
                3,
                1,
                T0
            )
            .data,
            Some(json!({ "status": "run_failed" }))
        );
        // Rendered, the correlation is #N at the answering bot, never an id.
        let prompt = crate::render::render_event_prompt(12, &document, None, 2_048);
        assert!(prompt.contains("event #12 · bot.reply · bot:infra"));
        assert!(prompt.contains("reply to your #17 at infra"));
        assert!(!contains_hex_run(&prompt, 32));
    }

    fn signal_event(document_ref: &str, prompt_ref: Option<&str>) -> BotEvent {
        BotEvent {
            id: "evt".to_owned(),
            seq: 1,
            document_ref: document_ref.to_owned(),
            prompt_ref: prompt_ref.map(str::to_owned),
            session: None,
            coalesce: None,
            when_busy: None,
            hops: 0,
            reply: false,
            media: Vec::new(),
            tools_ref: None,
            notify: false,
        }
    }

    #[test]
    fn delivers_a_single_event_as_exactly_one_rendered_item_no_framing() {
        let document_ref = format!("sha256:{}", "a".repeat(64));
        let prompt_ref = format!("sha256:{}", "b".repeat(64));
        assert_eq!(
            delivery_input_items(&[signal_event(&document_ref, Some(&prompt_ref))]),
            vec![InputItem::TextRef {
                provenance_ref: None,
                origin: Some("event".to_owned()),
                blob_ref: prompt_ref.clone(),
            }]
        );
        // Events without a rendering fall back to the envelope.
        assert_eq!(
            delivery_input_items(&[signal_event(&document_ref, None)]),
            vec![InputItem::TextRef {
                provenance_ref: None,
                origin: Some("event".to_owned()),
                blob_ref: document_ref.clone(),
            }]
        );
        // Media follows its event's rendering.
        let mut with_media = signal_event(&document_ref, Some(&prompt_ref));
        with_media.media = vec![BotEventMedia {
            text_ref: None,
            blob_ref: format!("sha256:{}", "c".repeat(64)),
            kind: BotEventMediaKind::Image,
            mime: "image/png".to_owned(),
            name: Some("shot.png".to_owned()),
        }];
        let items = delivery_input_items(&[with_media]);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[1],
            InputItem::Media {
                origin: Some("event".to_owned()),
                blob_ref: format!("sha256:{}", "c".repeat(64)),
                mime: "image/png".to_owned(),
                kind: MediaKind::Image,
                name: Some("shot.png".to_owned()),
            }
        );
    }

    #[test]
    fn prepared_attachment_delivery_uses_text_with_source_provenance() {
        let reference = format!("sha256:{}", "d".repeat(64));
        let mut event = signal_event(&format!("sha256:{}", "a".repeat(64)), None);
        event.media.push(BotEventMedia {
            text_ref: Some(reference.clone()),
            blob_ref: format!("sha256:{}", "c".repeat(64)),
            kind: BotEventMediaKind::Audio,
            mime: "audio/ogg".into(),
            name: Some("voice.ogg".into()),
        });
        assert_eq!(
            delivery_input_items(&[event])[1],
            InputItem::TextRef {
                provenance_ref: Some(format!("sha256:{}", "c".repeat(64))),
                origin: Some("event".into()),
                blob_ref: reference
            }
        );
    }

    #[test]
    fn frames_a_batch_with_one_header_line_binding_it_to_one_decision() {
        let a = format!("sha256:{}", "a".repeat(64));
        let b = format!("sha256:{}", "b".repeat(64));
        let items = delivery_input_items(&[signal_event(&a, Some(&b)), signal_event(&b, Some(&a))]);
        assert_eq!(items.len(), 3);
        let InputItem::Text { text, origin, .. } = &items[0] else {
            panic!("expected a text header, got {:?}", items[0]);
        };
        assert_eq!(origin.as_deref(), Some("event"));
        assert!(text.contains("2 events"), "{text}");
        assert!(text.contains("resolve the delivery once"), "{text}");
        assert_eq!(
            items[1],
            InputItem::TextRef {
                provenance_ref: None,
                origin: Some("event".to_owned()),
                blob_ref: b.clone()
            }
        );
        assert_eq!(
            items[2],
            InputItem::TextRef {
                provenance_ref: None,
                origin: Some("event".to_owned()),
                blob_ref: a.clone()
            }
        );
    }

    #[test]
    fn steers_with_a_short_note_and_the_renderings() {
        let a = format!("sha256:{}", "a".repeat(64));
        let b = format!("sha256:{}", "b".repeat(64));
        let items = steer_input_items(&[signal_event(&a, Some(&b))]);
        assert_eq!(items.len(), 2);
        let InputItem::Text { text, origin, .. } = &items[0] else {
            panic!("expected a text header, got {:?}", items[0]);
        };
        assert_eq!(origin.as_deref(), Some("event"));
        assert!(text.contains("1 more event(s)"), "{text}");
        assert!(text.contains("fold them into your current work"), "{text}");
        assert_eq!(
            items[1],
            InputItem::TextRef {
                provenance_ref: None,
                origin: Some("event".to_owned()),
                blob_ref: b
            }
        );
    }
}
