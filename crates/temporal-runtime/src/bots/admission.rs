//! Store, then wake. Every event path — hook route, manual admit, replay,
//! schedule and poll fires, chat emit, `bot_emit`, receipts — runs through
//! here. The row in Postgres is authoritative; the controller signal is a
//! notification over it, never the system of record.
//!
//! Order of the trigger pipeline: filter (a miss stores nothing, so a strict
//! filter on a firehose costs nothing) → route → idle-close policy → coalesce →
//! delivery policy → store-then-wake. Callers have already checked that the
//! trigger is enabled and the breaker has not tripped.

use api::{
    BotEventDocument, BotEventMedia, BotEventOutcome, BotEventReplyRef, BotId, BotTriggerSpec,
    BotWhenBusy,
};
use bots::{
    BotCoalesceParams, BotControllerConfig, BotError, BotEvent, BotEventRecord, BotEventStore,
    BotRecord, BotRefusalCode, BotStore, BotTriggerRecord, BotTriggerStore, EventReceiver,
    InsertBotEventOutcome, RoutedSession, RoutedSessionClosePolicy,
    filter::{FilterContext, RoutePreset, compute_route_session, evaluate_filter},
    ids::{bot_controller_workflow_id, coalesce_key},
    records::{BotEventOutcomeWrite, BotEventRateScope},
    render::{DEFAULT_PROMPT_BUDGET, render_event_prompt},
    validate::{effective_coalesce, effective_route},
};
use harness::storage::BlobStore;
use serde_json::Value;
use temporal_workflow::bots::{BotControllerArgs, BotControllerCarry};
use temporalio_client::{UntypedSignal, UntypedWorkflow, WorkflowStartOptions};
use temporalio_common::data_converters::{PayloadConverter, RawValue};
use temporalio_common::protos::coresdk::AsJsonPayloadExt as _;

use super::now_ms;
use crate::gateway::GatewayAgentApi;

/// Workflow type name of the bot controller (`#[run(name = …)]`).
pub const BOT_CONTROLLER_WORKFLOW_TYPE: &str = "BotControllerWorkflow";

/// What an admission stores for one event.
#[derive(Clone, Debug)]
pub struct StoreBotEventInput {
    pub event_id: String,
    pub trigger_id: Option<api::BotTriggerId>,
    pub document: BotEventDocument,
    /// Projection rendered for the model instead of `document.data`.
    pub prompt_data: Option<Value>,
    pub session: Option<RoutedSession>,
    pub coalesce: Option<BotCoalesceParams>,
    pub when_busy: Option<BotWhenBusy>,
    pub sender_bot_id: Option<BotId>,
    pub hops: u32,
    pub in_reply_to: Option<BotEventReplyRef>,
    pub media: Vec<BotEventMedia>,
    /// Who hears back when the delivery finishes: the admitting chat
    /// conversation, or the asking bot of a `bot_emit { reply: true }`.
    pub receiver: Option<EventReceiver>,
    /// `false` archives the row at birth (a chat send, a replay's original)
    /// and never wakes the controller.
    pub deliver: bool,
    /// Reuse an already-stored envelope document (replays).
    pub document_ref: Option<String>,
}

impl StoreBotEventInput {
    pub fn new(event_id: String, document: BotEventDocument) -> Self {
        Self {
            event_id,
            trigger_id: None,
            document,
            prompt_data: None,
            session: None,
            coalesce: None,
            when_busy: None,
            sender_bot_id: None,
            hops: 0,
            in_reply_to: None,
            media: Vec::new(),
            receiver: None,
            deliver: true,
            document_ref: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StoredBotEvent {
    pub event: BotEvent,
    pub record: BotEventRecord,
    pub duplicate: bool,
}

/// Outcome of the trigger pipeline.
#[derive(Clone, Debug)]
pub enum AdmitTriggerOutcome {
    Admitted(Box<StoredBotEvent>),
    /// The filter did not match (or threw, fail-closed): nothing was stored.
    Filtered {
        error: Option<String>,
    },
}

impl GatewayAgentApi {
    /// Store the event and wake the controller. A duplicate admission
    /// returns the stored row (so `#N` stays stable) and wakes the
    /// controller again on purpose — the row may exist because an earlier
    /// wake failed after the insert — and the controller dedupes.
    pub(crate) async fn store_bot_event(
        &self,
        bot: &BotRecord,
        input: StoreBotEventInput,
    ) -> Result<StoredBotEvent, BotError> {
        // The resurrection guard: waking is a signal-with-start, so a closed
        // bot must be refused on its row before anything is stored.
        if bot.is_closed() {
            return Err(BotError::refused(
                BotRefusalCode::BotClosed,
                format!("{} was closed and no longer accepts events", bot.bot_id),
            ));
        }
        let store = self.store();
        let seq = store.allocate_bot_event_seq(&bot.bot_id).await?;
        let prompt = render_event_prompt(
            seq,
            &input.document,
            input.prompt_data.as_ref(),
            DEFAULT_PROMPT_BUDGET,
        );
        let blobs: &dyn BlobStore = store.as_ref();
        let document_ref = match &input.document_ref {
            Some(existing) => existing.clone(),
            None => {
                let bytes = serde_json::to_vec(&input.document)
                    .map_err(|error| BotError::store(format!("encode event document: {error}")))?;
                blobs
                    .put_bytes(bytes)
                    .await
                    .map_err(|error| BotError::store(format!("store event document: {error}")))?
                    .to_string()
            }
        };
        let prompt_ref = blobs
            .put_bytes(prompt.into_bytes())
            .await
            .map_err(|error| BotError::store(format!("store event prompt: {error}")))?
            .to_string();
        let now = now_ms();
        let record = BotEventRecord {
            bot_id: bot.bot_id.clone(),
            event_id: input.event_id.clone(),
            seq,
            trigger_id: input.trigger_id.clone(),
            kind: input.document.kind.clone(),
            summary: input.document.summary.clone(),
            occurred_at_ms: input.document.occurred_at_ms,
            received_at_ms: now,
            document_ref,
            prompt_ref: Some(prompt_ref),
            session: input.session.clone(),
            media: input.media.clone(),
            sender_bot_id: input.sender_bot_id.clone(),
            hops: input.hops,
            in_reply_to: input.in_reply_to.clone(),
            receiver: input.receiver.clone(),
            outcome: (!input.deliver).then_some(BotEventOutcome::Archived),
            outcome_detail: None,
            run_id: None,
            resolved_at_ms: (!input.deliver).then_some(now),
        };
        let inserted = store.insert_bot_event(record).await?;
        let duplicate = inserted.is_duplicate();
        let record = match inserted {
            InsertBotEventOutcome::Inserted(record) | InsertBotEventOutcome::Duplicate(record) => {
                record
            }
        };
        let event = BotEvent {
            id: record.event_id.clone(),
            seq: record.seq,
            document_ref: record.document_ref.clone(),
            prompt_ref: record.prompt_ref.clone(),
            session: input.session,
            coalesce: input.coalesce,
            when_busy: input.when_busy,
            hops: input.hops,
            reply: matches!(input.receiver, Some(EventReceiver::Bot { .. })),
            media: input.media,
            tools_ref: record.tools_ref().map(str::to_owned),
            notify: matches!(input.receiver, Some(EventReceiver::Workflow { .. })),
        };
        if input.deliver
            && let Err(error) = self.wake_bot_controller(bot, &event).await
        {
            // Compensation: a row this call inserted is discarded when the
            // wake fails, so the caller's retry admits from scratch with
            // nothing stranded. A duplicate's row is left alone.
            if !duplicate {
                let _ = store.delete_bot_event(&bot.bot_id, &record.event_id).await;
            }
            return Err(BotError::store(format!("wake bot controller: {error}")));
        }
        Ok(StoredBotEvent {
            event,
            record,
            duplicate,
        })
    }

    /// Signal-with-start the bot's controller with one event. The start
    /// argument is the bot's current configuration, so a controller that is
    /// not running comes up with the row's truth.
    pub(crate) async fn wake_bot_controller(
        &self,
        bot: &BotRecord,
        event: &BotEvent,
    ) -> anyhow::Result<()> {
        let config = BotControllerConfig::from_record(self.universe_id(), bot);
        let workflow_id = bot_controller_workflow_id(self.universe_id(), &bot.bot_id);
        let payload = event.as_json_payload()?;
        let options =
            WorkflowStartOptions::new(self.bot_task_queue().to_owned(), workflow_id).build();
        self.start_bot_controller(
            config,
            bot.created_at_ms,
            options,
            bots::BOT_EVENT_SIGNAL,
            RawValue::new(vec![payload]),
        )
        .await
    }

    /// Start (or signal-with-start) the controller workflow by type name so
    /// this module depends on the workflow's wire, not its Rust type.
    async fn start_bot_controller(
        &self,
        config: BotControllerConfig,
        created_at_ms: i64,
        options: WorkflowStartOptions,
        signal_name: &str,
        signal_input: RawValue,
    ) -> anyhow::Result<()> {
        let previous = self
            .store()
            .previous_bot_session_ids(&config.bot_id, created_at_ms)
            .await?;
        let carry = carry_after_previous_sessions(&config.bot_id, previous)?;
        let input = RawValue::from_value(
            &BotControllerArgs {
                config,
                carry: Some(carry),
            },
            &PayloadConverter::default(),
        );
        self.temporal_client()
            .signal_with_start_workflow(
                UntypedWorkflow::new(BOT_CONTROLLER_WORKFLOW_TYPE),
                input,
                UntypedSignal::new(signal_name),
                signal_input,
                options,
            )
            .await
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    /// Signal-with-start the controller with a configuration change (a put,
    /// a close). Starting a controller for a bot that has never received an
    /// event is harmless: it reconciles its main session and parks.
    pub(crate) async fn signal_bot_config(&self, bot: &BotRecord) -> anyhow::Result<()> {
        let config = BotControllerConfig::from_record(self.universe_id(), bot);
        let workflow_id = bot_controller_workflow_id(self.universe_id(), &bot.bot_id);
        let payload = config.as_json_payload()?;
        let options =
            WorkflowStartOptions::new(self.bot_task_queue().to_owned(), workflow_id).build();
        self.start_bot_controller(
            config,
            bot.created_at_ms,
            options,
            bots::BOT_CONFIG_SIGNAL,
            RawValue::new(vec![payload]),
        )
        .await
    }

    /// The trigger pipeline: filter, route, idle-close policy, coalesce, delivery
    /// policy, then store-then-wake.
    pub(crate) async fn admit_trigger_event(
        &self,
        bot: &BotRecord,
        trigger: &BotTriggerRecord,
        mut input: StoreBotEventInput,
    ) -> Result<AdmitTriggerOutcome, BotError> {
        let store = self.store();
        let context = FilterContext::from_document(&input.event_id, &input.document);
        if let Some(filter) = trigger.document.filter.as_deref() {
            let result = evaluate_filter(filter, &context);
            if !result.matched {
                // A filter that throws is a configuration problem, not an
                // event: surfaced on the trigger, cleared by the next match.
                if let Some(error) = &result.error {
                    store
                        .set_bot_trigger_filter_error(
                            &bot.bot_id,
                            &trigger.trigger_id,
                            Some(error.clone()),
                            now_ms(),
                        )
                        .await?;
                }
                return Ok(AdmitTriggerOutcome::Filtered {
                    error: result.error,
                });
            }
            if trigger.last_filter_error.is_some() {
                store
                    .set_bot_trigger_filter_error(&bot.bot_id, &trigger.trigger_id, None, now_ms())
                    .await?;
            }
        }
        let preset = match &trigger.document.spec {
            BotTriggerSpec::Webhook { preset, .. } => preset.map(RoutePreset::from),
            BotTriggerSpec::Chat { .. } => Some(RoutePreset::Chat),
            _ => None,
        };
        let route = effective_route(&trigger.document);
        let routed = compute_route_session(&bot.bot_id, &route, preset, &input.event_id, &context);
        // Per-trigger idle-close policy rides on the routed target: absent inherits
        // the bot's setting, 0 keeps the session open indefinitely.
        let session = routed.map(|mut session| {
            session.close_policy = match trigger.document.session_close_after_ms {
                None => RoutedSessionClosePolicy::Inherit,
                Some(0) => RoutedSessionClosePolicy::Never,
                Some(ms) => RoutedSessionClosePolicy::After { ms },
            };
            session
        });
        input.trigger_id = Some(trigger.trigger_id.clone());
        input.coalesce = effective_coalesce(&trigger.document).map(|policy| {
            BotCoalesceParams::from_policy(
                coalesce_key(
                    &trigger.trigger_id,
                    session.as_ref().map(|session| session.session_id.as_str()),
                ),
                policy,
            )
        });
        input.session = session;
        input.when_busy = trigger.document.deliver.map(|deliver| deliver.when_busy);
        let stored = self.store_bot_event(bot, input).await?;
        Ok(AdmitTriggerOutcome::Admitted(Box::new(stored)))
    }

    /// The per-trigger flood breaker: at or over `fires` admissions inside
    /// `window_ms` the trigger is disabled (a human re-enables it) and the
    /// caller refuses the event.
    pub(crate) async fn check_trigger_breaker(
        &self,
        bot: &BotRecord,
        trigger: &BotTriggerRecord,
    ) -> Result<(), BotError> {
        let Some(breaker) = bot.document.breaker else {
            return Ok(());
        };
        let now = now_ms();
        let count = self
            .store()
            .count_bot_events_since(
                BotEventRateScope::Trigger {
                    bot_id: &bot.bot_id,
                    trigger_id: &trigger.trigger_id,
                },
                now - breaker.window_ms as i64,
            )
            .await?;
        if count >= u64::from(breaker.fires) {
            self.store()
                .disable_bot_trigger(
                    &bot.bot_id,
                    &trigger.trigger_id,
                    api::BotTriggerDisabledReason::Breaker,
                    now,
                )
                .await?;
            if trigger.has_schedule() {
                let _ = self
                    .delete_bot_trigger_schedule(&bot.bot_id, &trigger.trigger_id)
                    .await;
            }
            return Err(BotError::refused(
                BotRefusalCode::BreakerTripped,
                format!(
                    "trigger {} admitted {count} events in {} ms and was disabled",
                    trigger.trigger_id, breaker.window_ms
                ),
            ));
        }
        Ok(())
    }

    /// Sender rate cap for emitting bots: the bot's own breaker rate, else
    /// the default. Without publish fan-out this is the whole amplification
    /// bound of federation.
    pub(crate) async fn check_sender_rate(&self, sender: &BotRecord) -> Result<(), BotError> {
        let (fires, window_ms) = match sender.document.breaker {
            Some(breaker) => (breaker.fires, breaker.window_ms),
            None => (
                bots::DEFAULT_SENDER_RATE_FIRES,
                bots::DEFAULT_SENDER_RATE_WINDOW_MS,
            ),
        };
        let count = self
            .store()
            .count_bot_events_since(
                BotEventRateScope::Sender {
                    sender_bot_id: &sender.bot_id,
                },
                now_ms() - window_ms as i64,
            )
            .await?;
        if count >= u64::from(fires) {
            return Err(BotError::refused(
                BotRefusalCode::RateLimited,
                format!(
                    "{} emitted {count} events in the last {} ms; wait before emitting again",
                    sender.bot_id, window_ms
                ),
            ));
        }
        Ok(())
    }

    /// Write-once outcome for a delivery's events.
    pub(crate) async fn record_bot_event_outcomes(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
        outcome: BotEventOutcome,
        detail: Option<String>,
        run_id: Option<String>,
    ) -> Result<u64, BotError> {
        self.store()
            .record_bot_event_outcomes(
                bot_id,
                event_ids,
                BotEventOutcomeWrite {
                    outcome,
                    detail,
                    run_id,
                    resolved_at_ms: now_ms(),
                },
            )
            .await
    }

    /// Load a bot or refuse with the typed `unknown_bot`.
    pub(crate) async fn load_bot_for_admission(
        &self,
        bot_id: &BotId,
    ) -> Result<BotRecord, BotError> {
        match self.store().read_bot(bot_id).await {
            Ok(bot) => Ok(bot),
            Err(BotError::BotNotFound { .. }) => Err(BotError::refused(
                BotRefusalCode::UnknownBot,
                format!("no bot named {bot_id}"),
            )),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_admissions_never_deliver() {
        let mut input = StoreBotEventInput::new(
            "e1".to_owned(),
            BotEventDocument {
                version: 1,
                kind: "chat.sent".to_owned(),
                source: "chat:x".to_owned(),
                occurred_at_ms: 0,
                summary: "sent".to_owned(),
                data: None,
                headers: Default::default(),
                correlation_id: None,
                links: Vec::new(),
                sender: None,
                hops: 0,
                in_reply_to: None,
            },
        );
        input.deliver = false;
        assert!(!input.deliver);
        assert!(input.trigger_id.is_none());
    }
}

/// Reusing a bot id starts after its retained session generations.
fn carry_after_previous_sessions(
    bot_id: &BotId,
    ids: Vec<String>,
) -> anyhow::Result<BotControllerCarry> {
    let main = bots::ids::bot_main_session_id(bot_id, 1);
    let mut carry = BotControllerCarry::default();
    for id in ids {
        let (base, generation) = if id == main {
            (id.as_str(), 1)
        } else if let Some(generation) = id.strip_prefix(&format!("{main}-g"))
            && let Ok(generation) = generation.parse::<u32>()
        {
            (main.as_str(), generation)
        } else if id.starts_with(&format!("{main}:")) {
            let base = bots::ids::routed_session_base(&id);
            let generation = if base == id {
                1
            } else {
                id[base.len() + 2..].parse::<u32>()?
            };
            (base, generation)
        } else {
            continue;
        };
        let next = generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("bot session generation exhausted"))?;
        if base == main {
            carry.main_generation = carry.main_generation.max(next);
        } else {
            let current = carry
                .session_generations
                .entry(base.to_owned())
                .or_insert(1);
            *current = (*current).max(next);
        }
    }
    Ok(carry)
}

#[cfg(test)]
mod retained_session_tests {
    use super::*;
    #[test]
    fn new_bot_incarnation_skips_old_main_and_routed_generations() {
        let carry = carry_after_previous_sessions(
            &BotId::new("triage"),
            vec![
                "bot:v1:triage".into(),
                "bot:v1:triage-g4".into(),
                "bot:v1:triage:k-mail-g3".into(),
                "bot:v1:triage-other".into(),
            ],
        )
        .unwrap();
        assert_eq!(carry.main_generation, 5);
        assert_eq!(
            carry.session_generations.get("bot:v1:triage:k-mail"),
            Some(&4)
        );
        assert_eq!(carry.session_generations.len(), 1);
        assert!(!carry.session_ready);
    }
}
