//! Outcomes, bot-to-bot receipts, and the bot directory.
//!
//! Outcome and closed-session writes retry until they land. If the asking
//! bot's inbox refuses a best-effort `bot.reply`, the exchange ends.

use std::collections::BTreeMap;

use api::{
    AgentApiService as _, BotEventReplyRef, BotId, BotTriggerKind, BotWhenBusy, ContextAppendEntry,
    ContextAppendParams,
};
use bots::{
    BotError, BotEventRecord, BotEventStore as _, BotRecord, BotStore as _, BotTriggerRecord,
    BotTriggerStore as _,
    ids::{MAX_BOT_HOPS, receipt_event_id},
    views::{BOT_DIRECTORY_KEY, bot_directory_item, directory_entries_for, receipt_document},
};
use temporal_workflow::bots::*;
use temporalio_sdk::activities::ActivityError;

use super::{
    admission::StoreBotEventInput,
    now_ms,
    sessions::{activity_error, check_context_append, non_retryable, retryable},
};
use crate::gateway::GatewayAgentApi;

/// Bots-domain errors: only the store fails transiently; every other
/// variant describes the request or the record and never heals on retry.
fn bot_error(context: &str, error: BotError) -> ActivityError {
    let message = format!("{context}: {error}");
    match error {
        BotError::Store { .. } => retryable(message),
        _ => non_retryable(message),
    }
}

// ── Pure helpers ────────────────────────────────────────────────────────────

/// The hop count a receipt carries: one more than the delivery's highest,
/// or `None` when that would exceed the federation bound — the exchange
/// ends here, silently by design.
fn receipt_hops(delivery_hops: u32) -> Option<u32> {
    delivery_hops
        .checked_add(1)
        .filter(|hops| *hops <= MAX_BOT_HOPS)
}

/// Each bot with its inbox (`bot`-kind trigger). A bot declares at most
/// one; should the catalog ever hold several, an enabled one wins, then the
/// first by trigger id (the listing order).
fn pair_inboxes(
    bots: Vec<BotRecord>,
    inboxes: Vec<BotTriggerRecord>,
) -> Vec<(BotRecord, Option<BotTriggerRecord>)> {
    let mut by_bot: BTreeMap<BotId, BotTriggerRecord> = BTreeMap::new();
    for inbox in inboxes {
        match by_bot.get(&inbox.bot_id) {
            Some(existing) if existing.enabled() || !inbox.enabled() => {}
            _ => {
                by_bot.insert(inbox.bot_id.clone(), inbox);
            }
        }
    }
    bots.into_iter()
        .map(|bot| {
            let inbox = by_bot.remove(&bot.bot_id);
            (bot, inbox)
        })
        .collect()
}

// ── Activities ──────────────────────────────────────────────────────────────

pub async fn record_outcomes(
    api: &GatewayAgentApi,
    request: BotRecordOutcomesRequest,
) -> Result<BotRecordOutcomesResult, ActivityError> {
    if request.event_ids.is_empty() {
        return Ok(BotRecordOutcomesResult::default());
    }
    let updated = api
        .record_bot_event_outcomes(
            &request.bot_id,
            &request.event_ids,
            request.outcome,
            request.detail,
            request.run_id,
        )
        .await
        .map_err(|error| bot_error("record event outcomes", error))?;
    Ok(BotRecordOutcomesResult { updated })
}

/// The controller's last write before it completes: the sessions it closed,
/// unioned with what an earlier teardown attempt recorded so a retried
/// close never loses ids.
pub async fn record_closed(
    api: &GatewayAgentApi,
    request: BotRecordClosedRequest,
) -> Result<BotRecordClosedResult, ActivityError> {
    let sessions = api
        .store()
        .record_bot_closed_sessions(&request.bot_id, request.sessions)
        .await
        .map_err(|error| bot_error("record closed sessions", error))?;
    Ok(BotRecordClosedResult { sessions })
}

pub async fn send_bot_receipts(
    api: &GatewayAgentApi,
    request: BotSendBotReceiptsRequest,
) -> Result<BotReceiptsSent, ActivityError> {
    if request.event_ids.is_empty() {
        return Ok(BotReceiptsSent::default());
    }
    let answering = api
        .load_bot_for_admission(&request.bot_id)
        .await
        .map_err(|error| bot_error("load answering bot", error))?;
    let records = api
        .store()
        .read_bot_events(&request.bot_id, &request.event_ids)
        .await
        .map_err(|error| bot_error("read delivery events", error))?;
    let asked: Vec<&BotEventRecord> = records
        .iter()
        .filter(|record| record.reply_route().is_some())
        .collect();
    let Some(hops) = receipt_hops(request.hops) else {
        // Hop bound reached: the loop is cut here, silently by design.
        return Ok(BotReceiptsSent {
            sent: 0,
            skipped: u32::try_from(asked.len()).unwrap_or(u32::MAX),
        });
    };
    let mut sent = 0;
    let mut skipped = 0;
    for record in asked {
        let Some((asker_id, asker_session)) = record.reply_route() else {
            continue;
        };
        let asker = match api.load_bot_for_admission(asker_id).await {
            Ok(bot) => bot,
            Err(BotError::Refused { .. }) => {
                skipped += 1;
                continue;
            }
            Err(error) => return Err(bot_error("load asking bot", error)),
        };
        if !asker.document.enabled || asker.is_closed() {
            skipped += 1;
            continue;
        }
        let document = receipt_document(
            &answering.bot_id,
            request.outcome,
            request.summary.as_deref(),
            record.seq,
            hops,
            now_ms(),
        );
        let mut input = StoreBotEventInput::new(
            receipt_event_id(&answering.bot_id, &request.delivery_id, &record.event_id),
            document,
        );
        input.session = asker_session.cloned();
        input.when_busy = Some(BotWhenBusy::Queue);
        input.sender_bot_id = Some(answering.bot_id.clone());
        input.hops = hops;
        input.in_reply_to = Some(BotEventReplyRef {
            bot: answering.bot_id.clone(),
            seq: record.seq,
        });
        match api.store_bot_event(&asker, input).await {
            Ok(_) => sent += 1,
            // The asker closed between the ask and the answer, or refuses
            // the answering bot now: the receipt has nowhere to go.
            Err(BotError::Refused { .. }) => skipped += 1,
            Err(error) => return Err(bot_error("store bot receipt", error)),
        }
    }
    Ok(BotReceiptsSent { sent, skipped })
}

/// Put the `bot:directory` catalog into a session before a delivery: the
/// bots whose inbox accepts this one. A same-content append is an harness
/// no-op, so calling it before every delivery is cheap and keeps the prefix
/// cache intact.
pub async fn publish_directory(
    api: &GatewayAgentApi,
    request: BotPublishDirectoryRequest,
) -> Result<BotPublishDirectoryResult, ActivityError> {
    let store = api.store();
    let me = store
        .read_bot(&request.bot_id)
        .await
        .map_err(|error| bot_error("read bot", error))?;
    let bots = store
        .list_bots()
        .await
        .map_err(|error| bot_error("list bots", error))?;
    let inboxes = store
        .list_bot_triggers_by_kind(BotTriggerKind::Bot)
        .await
        .map_err(|error| bot_error("list bot inboxes", error))?;
    let entries = directory_entries_for(&me.bot_id, &pair_inboxes(bots, inboxes));
    let response = api
        .append_context(ContextAppendParams {
            session_id: request.session_id,
            entries: vec![ContextAppendEntry {
                key: BOT_DIRECTORY_KEY.to_owned(),
                item: bot_directory_item(&entries),
            }],
        })
        .await
        .map_err(|error| activity_error("publish bot directory", error))?;
    check_context_append(&response)?;
    Ok(BotPublishDirectoryResult {
        entries: u32::try_from(entries.len()).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{BotDocument, BotTriggerDocument, BotTriggerId, BotTriggerSpec, ProfileId};
    use bots::BotTriggerSecrets;

    fn bot(id: &str) -> BotRecord {
        BotRecord {
            bot_id: BotId::new(id),
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
                enabled: true,
            },
            event_seq: 0,
            closed_at_ms: None,
            closed_sessions: Vec::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn inbox(bot_id: &str, trigger_id: &str, enabled: bool) -> BotTriggerRecord {
        BotTriggerRecord {
            bot_id: BotId::new(bot_id),
            trigger_id: BotTriggerId::new(trigger_id),
            revision: 1,
            document: BotTriggerDocument {
                spec: BotTriggerSpec::Bot { from: None },
                filter: None,
                route: None,
                coalesce: None,
                deliver: None,
                session_close_after_ms: None,
                enabled,
            },
            secrets: BotTriggerSecrets::default(),
            disabled_reason: None,
            disabled_at_ms: None,
            last_filter_error: None,
            last_filter_error_at_ms: None,
            cursor: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn receipt_hops_cut_at_the_federation_bound() {
        assert_eq!(receipt_hops(0), Some(1));
        assert_eq!(receipt_hops(MAX_BOT_HOPS - 1), Some(MAX_BOT_HOPS));
        assert_eq!(receipt_hops(MAX_BOT_HOPS), None);
        assert_eq!(receipt_hops(u32::MAX), None);
    }

    #[test]
    fn inbox_pairing_prefers_an_enabled_inbox() {
        let pairs = pair_inboxes(
            vec![bot("alpha"), bot("beta"), bot("gamma")],
            vec![
                inbox("alpha", "inbox-off", false),
                inbox("alpha", "inbox-on", true),
                inbox("beta", "first", true),
                inbox("beta", "second", true),
            ],
        );
        assert_eq!(pairs.len(), 3);
        assert_eq!(
            pairs[0].1.as_ref().map(|inbox| inbox.trigger_id.as_str()),
            Some("inbox-on")
        );
        assert_eq!(
            pairs[1].1.as_ref().map(|inbox| inbox.trigger_id.as_str()),
            Some("first")
        );
        assert!(pairs[2].1.is_none());
    }
}
