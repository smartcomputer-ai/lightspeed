//! `bots/*` service methods: bot and trigger documents, the event log,
//! manual admission and replay, filter tests, controller state, close and
//! delete. Tool execution (`bot_*`) goes through the same functions where
//! it mutates configuration, so a bot editing itself and an operator
//! editing it are validated identically.

use super::*;

use ::bots::{
    BotError, BotEventCursor, BotEventStore, BotRecord, BotStore, BotTriggerRecord,
    BotTriggerSecrets, BotTriggerStore, BotTriggerWrite, InsertBotEventOutcome,
    filter::{FilterContext, evaluate_filter},
    ids::{
        bot_controller_workflow_id, is_bot_session, manual_event_id, replay_event_id,
        routed_session_base,
    },
    validate::{validate_pairing_code, validate_trigger_document},
};
use ::channels::ChannelAccountStore;
use rand::RngCore as _;
use serde_json::Value;
use temporalio_client::{UntypedQuery, UntypedSignal, UntypedWorkflow, errors::WorkflowQueryError};
use temporalio_common::data_converters::{PayloadConverter, RawValue};

use crate::bots::{
    admission::{AdmitTriggerOutcome, StoreBotEventInput},
    map_bot_error, now_ms as bot_now_ms,
};

const DEFAULT_EVENT_PAGE: usize = 50;
const MAX_EVENT_PAGE: usize = 200;
const DEFAULT_FILTER_SAMPLE: usize = 20;
const MAX_FILTER_SAMPLE: usize = 50;
/// How long `bots/delete` waits for a closing controller to complete.
const BOT_CLOSE_WAIT: Duration = Duration::from_secs(30);

/// Public ingest path of a webhook trigger. The route is outside RPC auth,
/// so it names the universe, bot, and trigger; the token is the secret.
pub fn webhook_ingest_path(
    universe_id: uuid::Uuid,
    bot_id: &BotId,
    trigger_id: &BotTriggerId,
    token: &str,
) -> String {
    format!("/hooks/bots/{universe_id}/{bot_id}/{trigger_id}/{token}")
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

fn mint_webhook_token() -> String {
    hex_encode(&random_bytes(24))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn mint_pairing_code() -> String {
    ::bots::pairing_code_from_bytes(&random_bytes(::bots::PAIRING_CODE_LEN))
}

fn decode_raw_value<T: serde::de::DeserializeOwned>(raw: RawValue) -> Result<T, AgentApiError> {
    let payload = raw
        .payloads
        .first()
        .ok_or_else(|| AgentApiError::internal("bot controller query returned no payload"))?;
    serde_json::from_slice(&payload.data).map_err(|error| {
        AgentApiError::internal(format!(
            "bot controller query payload is not valid JSON: {error}"
        ))
    })
}

fn encode_event_cursor(record: &::bots::BotEventRecord) -> String {
    format!("{}:{}", record.received_at_ms, record.seq)
}

fn decode_event_cursor(cursor: &str) -> Result<BotEventCursor, AgentApiError> {
    let invalid = || AgentApiError::invalid_request(format!("invalid event cursor: {cursor}"));
    let (received_at_ms, seq) = cursor.split_once(':').ok_or_else(invalid)?;
    Ok(BotEventCursor {
        received_at_ms: received_at_ms.parse().map_err(|_| invalid())?,
        seq: seq.parse().map_err(|_| invalid())?,
    })
}

impl GatewayAgentApi {
    fn bot_controller_handle(
        &self,
        bot_id: &BotId,
    ) -> temporalio_client::WorkflowHandle<Client, UntypedWorkflow> {
        self.temporal_client()
            .get_workflow_handle::<UntypedWorkflow>(bot_controller_workflow_id(
                self.universe_id(),
                bot_id,
            ))
    }

    pub(crate) fn trigger_view(&self, record: &BotTriggerRecord) -> BotTriggerView {
        let ingest_path = record.secrets.webhook_token.as_deref().map(|token| {
            webhook_ingest_path(
                self.universe_id(),
                &record.bot_id,
                &record.trigger_id,
                token,
            )
        });
        record.view(false, ingest_path)
    }

    async fn read_bot_record(&self, bot_id: &BotId) -> Result<BotRecord, AgentApiError> {
        self.store().read_bot(bot_id).await.map_err(map_bot_error)
    }

    /// Read a bot's stored envelope document from the CAS.
    pub(crate) async fn read_bot_event_document(
        &self,
        record: &::bots::BotEventRecord,
    ) -> Result<BotEventDocument, AgentApiError> {
        let blob_ref = BlobRef::parse(&record.document_ref).map_err(|error| {
            AgentApiError::internal(format!("invalid event document ref: {error}"))
        })?;
        let bytes =
            self.store().read_bytes(&blob_ref).await.map_err(|error| {
                AgentApiError::internal(format!("read event document: {error}"))
            })?;
        serde_json::from_slice(&bytes)
            .map_err(|error| AgentApiError::internal(format!("decode event document: {error}")))
    }

    /// Query the controller's live snapshot; `None` when no controller
    /// workflow is running.
    pub(crate) async fn query_bot_controller(
        &self,
        bot_id: &BotId,
    ) -> Result<Option<BotControllerSnapshot>, AgentApiError> {
        match self
            .bot_controller_handle(bot_id)
            .query(
                UntypedQuery::new(::bots::BOT_STATE_QUERY),
                RawValue::from_value(&(), &PayloadConverter::default()),
                WorkflowQueryOptions::default(),
            )
            .await
        {
            Ok(raw) => decode_raw_value(raw).map(Some),
            Err(WorkflowQueryError::NotFound(_)) => Ok(None),
            Err(error) => Err(AgentApiError::internal(format!(
                "query bot controller {bot_id}: {error}"
            ))),
        }
    }

    async fn validate_retrievable_grant_id(
        &self,
        grant_id: &str,
    ) -> Result<auth::AuthGrantRecord, AgentApiError> {
        let grant_id = parse_auth_grant_id(grant_id.to_owned())?;
        let grants: &dyn AuthGrantStore = self.store().as_ref();
        let record = grants.read_grant(&grant_id).await.map_err(map_auth_error)?;
        if record.status != auth::AuthGrantStatus::Active {
            return Err(AgentApiError::rejected(format!(
                "auth grant {grant_id} is not active"
            )));
        }
        require_retrievable_grant(&record)?;
        Ok(record)
    }

    /// A poll sends its leased credential to the poll URL, so the URL must
    /// lie within the grant's audience. A grant bound to no audience would
    /// follow any URL; attaching one is universe configuration, not bot use.
    async fn validate_trigger_grants(
        &self,
        document: &BotTriggerDocument,
    ) -> Result<(), AgentApiError> {
        match &document.spec {
            BotTriggerSpec::Webhook {
                verification: WebhookVerification::HmacSha256 { grant_id, .. },
                ..
            } => self
                .validate_retrievable_grant_id(grant_id)
                .await
                .map(|_| ()),
            BotTriggerSpec::Poll {
                source:
                    PollSource::Http {
                        url,
                        auth: Some(auth),
                        ..
                    },
                ..
            } => {
                let grant = self.validate_retrievable_grant_id(&auth.grant_id).await?;
                match &grant.audience {
                    Some(audience) if auth::audience_covers(audience, url) => Ok(()),
                    Some(audience) => Err(AgentApiError::rejected(format!(
                        "auth grant {} is bound to {audience}, which does not cover the poll URL",
                        grant.grant_id
                    ))),
                    None => {
                        if self
                            .permitted(
                                MethodAccess::Universe(UniverseAction::ConfigureResource),
                                None,
                            )
                            .await?
                        {
                            Ok(())
                        } else {
                            Err(AgentApiError::forbidden())
                        }
                    }
                }
            }
            _ => Ok(()),
        }
    }

    /// Create or replace a trigger: validation, one inbox per bot, chat
    /// account existence, grant checks, secret minting, poll cursor reset,
    /// then the row and its Schedule.
    pub(crate) async fn put_bot_trigger_record(
        &self,
        bot_id: &BotId,
        input: BotTriggerInput,
        expected_revision: Option<u64>,
    ) -> Result<BotTriggerRecord, AgentApiError> {
        self.authorize_method(
            METHOD_BOTS_TRIGGERS_PUT,
            Some(ResourceRef::Bot(bot_id.as_str().to_owned())),
        )
        .await?;
        let bot = self.read_bot_record(bot_id).await?;
        if bot.is_closed() {
            return Err(AgentApiError::rejected(format!(
                "bot {bot_id} is closed; its triggers cannot change"
            )));
        }
        let now = bot_now_ms();
        validate_trigger_document(&input.document, now).map_err(map_bot_error)?;
        let store = self.store();
        let existing = match store.read_bot_trigger(bot_id, &input.trigger_id).await {
            Ok(existing) => Some(existing),
            Err(BotError::TriggerNotFound { .. }) => None,
            Err(error) => return Err(map_bot_error(error)),
        };
        let kind = input.document.spec.kind();
        if let Some(existing) = &existing
            && existing.kind() != kind
        {
            return Err(AgentApiError::conflict(format!(
                "trigger {} is a {} trigger; delete it before changing its kind",
                input.trigger_id,
                existing.kind()
            )));
        }
        if kind == BotTriggerKind::Bot {
            let inboxes = store
                .list_bot_triggers(bot_id)
                .await
                .map_err(map_bot_error)?;
            if inboxes.iter().any(|trigger| {
                trigger.kind() == BotTriggerKind::Bot && trigger.trigger_id != input.trigger_id
            }) {
                return Err(AgentApiError::conflict(format!(
                    "bot {bot_id} already has an inbox trigger; a bot has at most one"
                )));
            }
        }
        if let BotTriggerSpec::Chat { account_id, .. } = &input.document.spec {
            let account_id = ChannelAccountId::try_new(account_id.clone()).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid channel account id: {error}"))
            })?;
            let accounts: &dyn ChannelAccountStore = store.as_ref();
            accounts
                .read_channel_account(&account_id)
                .await
                .map_err(|error| match error {
                    ::channels::ChannelError::AccountNotFound { .. } => {
                        AgentApiError::invalid_request(format!(
                            "unknown channel account: {account_id}"
                        ))
                    }
                    other => AgentApiError::internal(other.to_string()),
                })?;
        }
        self.validate_trigger_grants(&input.document).await?;

        let mut secrets = existing
            .as_ref()
            .map(|existing| existing.secrets.clone())
            .unwrap_or_default();
        match &input.document.spec {
            BotTriggerSpec::Webhook { .. } => {
                // The URL token survives spec edits: rotation means a new
                // trigger.
                if secrets.webhook_token.is_none() {
                    secrets.webhook_token = Some(mint_webhook_token());
                }
            }
            BotTriggerSpec::Chat { pairing, .. } => {
                secrets.pairing_code = match pairing {
                    ChatPairing::Open => None,
                    ChatPairing::Code => match input.pairing_code {
                        Some(code) => {
                            validate_pairing_code(&code).map_err(map_bot_error)?;
                            Some(code)
                        }
                        None => Some(secrets.pairing_code.unwrap_or_else(mint_pairing_code)),
                    },
                };
            }
            _ => secrets = BotTriggerSecrets::default(),
        }
        // A poll spec edit resets the cursor so the next fire re-baselines.
        let cursor = match (&existing, &input.document.spec) {
            (Some(existing), BotTriggerSpec::Poll { .. })
                if existing.document.spec != input.document.spec =>
            {
                Some(None)
            }
            _ => None,
        };
        let created = existing.is_none();
        let record = store
            .put_bot_trigger(
                bot_id,
                BotTriggerWrite {
                    trigger_id: input.trigger_id.clone(),
                    document: input.document,
                    secrets,
                    cursor,
                },
                expected_revision,
                now,
            )
            .await
            .map_err(map_bot_error)?;
        if let Err(error) = self.reconcile_bot_trigger_schedule(&bot, &record).await {
            if created {
                let _ = store.delete_bot_trigger(bot_id, &record.trigger_id).await;
            }
            return Err(AgentApiError::internal(format!(
                "reconcile trigger schedule: {error}"
            )));
        }
        Ok(record)
    }

    pub(crate) async fn delete_bot_trigger_record(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, AgentApiError> {
        self.authorize_method(
            METHOD_BOTS_TRIGGERS_DELETE,
            Some(ResourceRef::Bot(bot_id.as_str().to_owned())),
        )
        .await?;
        self.delete_bot_trigger_schedule(bot_id, trigger_id)
            .await
            .map_err(|error| {
                AgentApiError::internal(format!("delete trigger schedule: {error}"))
            })?;
        self.store()
            .delete_bot_trigger(bot_id, trigger_id)
            .await
            .map_err(map_bot_error)
    }

    /// Replace the bot document and tell the controller. A failed signal
    /// rolls the row back so the record and the controller never disagree.
    pub(crate) async fn put_bot_record(
        &self,
        input: BotInput,
        expected_revision: Option<u64>,
    ) -> Result<BotRecord, AgentApiError> {
        self.authorize_method(
            METHOD_BOTS_PUT,
            Some(ResourceRef::Bot(input.bot_id.as_str().to_owned())),
        )
        .await?;
        let store = self.store();
        let previous = self.read_bot_record(&input.bot_id).await?;
        self.require_profile(&input.document.profile_id).await?;
        let record = store
            .put_bot(
                input.bot_id.clone(),
                input.document,
                expected_revision,
                bot_now_ms(),
            )
            .await
            .map_err(map_bot_error)?;
        if let Err(error) = self.signal_bot_config(&record).await {
            let _ = store
                .put_bot(
                    previous.bot_id.clone(),
                    previous.document.clone(),
                    Some(record.revision),
                    bot_now_ms(),
                )
                .await;
            return Err(AgentApiError::internal(format!(
                "signal bot controller: {error}"
            )));
        }
        if previous.document.enabled != record.document.enabled {
            self.reconcile_bot_trigger_schedules(&record).await?;
        }
        Ok(record)
    }

    async fn reconcile_bot_trigger_schedules(&self, bot: &BotRecord) -> Result<(), AgentApiError> {
        for trigger in self
            .store()
            .list_bot_triggers(&bot.bot_id)
            .await
            .map_err(map_bot_error)?
        {
            if trigger.has_schedule() {
                self.reconcile_bot_trigger_schedule(bot, &trigger)
                    .await
                    .map_err(|error| {
                        AgentApiError::internal(format!("reconcile trigger schedule: {error}"))
                    })?;
            }
        }
        Ok(())
    }

    async fn require_profile(&self, profile_id: &ProfileId) -> Result<(), AgentApiError> {
        let profiles: &dyn ::profiles::ProfileStore = self.store().as_ref();
        profiles
            .read_agent_profile(profile_id)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                ::profiles::ProfileError::NotFound { .. } => {
                    AgentApiError::invalid_request(format!("unknown profile: {profile_id}"))
                }
                other => AgentApiError::internal(other.to_string()),
            })
    }

    /// Terminal close: the row first (admission refuses on it), then every
    /// trigger off and its Schedule gone, then the controller told to tear
    /// down.
    pub(crate) async fn close_bot_record(
        &self,
        bot_id: &BotId,
    ) -> Result<BotRecord, AgentApiError> {
        self.authorize_method(
            METHOD_BOTS_CLOSE,
            Some(ResourceRef::Bot(bot_id.as_str().to_owned())),
        )
        .await?;
        let store = self.store();
        let bot = store
            .close_bot(bot_id, bot_now_ms())
            .await
            .map_err(map_bot_error)?;
        store
            .disable_bot_triggers(bot_id, BotTriggerDisabledReason::BotClosed, bot_now_ms())
            .await
            .map_err(map_bot_error)?;
        for trigger in store
            .list_bot_triggers(bot_id)
            .await
            .map_err(map_bot_error)?
        {
            if trigger.has_schedule() {
                self.delete_bot_trigger_schedule(bot_id, &trigger.trigger_id)
                    .await
                    .map_err(|error| {
                        AgentApiError::internal(format!("delete trigger schedule: {error}"))
                    })?;
            }
        }
        self.signal_bot_config(&bot)
            .await
            .map_err(|error| AgentApiError::internal(format!("signal bot controller: {error}")))?;
        Ok(bot)
    }

    /// Wait (bounded) for a closing controller to complete.
    async fn wait_for_bot_controller_closed(&self, bot_id: &BotId) -> Result<(), AgentApiError> {
        let handle = self.bot_controller_handle(bot_id);
        let started = Instant::now();
        loop {
            match handle.describe(WorkflowDescribeOptions::default()).await {
                Ok(description) => {
                    if description.status() != WorkflowExecutionStatus::Running {
                        return Ok(());
                    }
                }
                Err(WorkflowInteractionError::NotFound(_)) => return Ok(()),
                Err(error) => {
                    return Err(AgentApiError::internal(format!(
                        "describe bot controller {bot_id}: {error}"
                    )));
                }
            }
            if started.elapsed() > BOT_CLOSE_WAIT {
                return Err(AgentApiError::rejected(format!(
                    "bot {bot_id} is still closing; retry the delete once its controller has completed"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(crate) async fn delete_bot_record(
        &self,
        bot_id: &BotId,
    ) -> Result<(BotRecord, Vec<String>), AgentApiError> {
        self.authorize_method(
            METHOD_BOTS_DELETE,
            Some(ResourceRef::Bot(bot_id.as_str().to_owned())),
        )
        .await?;
        let bot = self.read_bot_record(bot_id).await?;
        if !bot.is_closed() {
            self.close_bot_record(bot_id).await?;
        }
        self.wait_for_bot_controller_closed(bot_id).await?;
        let bot = self.read_bot_record(bot_id).await?;
        let mut deleted_sessions = Vec::new();
        for session_id in &bot.closed_sessions {
            match self
                .delete_session(SessionDeleteParams {
                    session_id: session_id.clone(),
                    cascade: true,
                })
                .await
            {
                Ok(_) => deleted_sessions.push(session_id.clone()),
                Err(error) if error.kind == AgentApiErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let store = self.store();
        for trigger in store
            .list_bot_triggers(bot_id)
            .await
            .map_err(map_bot_error)?
        {
            if trigger.has_schedule() {
                let _ = self
                    .delete_bot_trigger_schedule(bot_id, &trigger.trigger_id)
                    .await;
            }
        }
        let bot = store.delete_bot(bot_id).await.map_err(map_bot_error)?;
        Ok((bot, deleted_sessions))
    }

    pub(crate) async fn bot_state_view(
        &self,
        bot_id: &BotId,
    ) -> Result<BotStateView, AgentApiError> {
        self.read_bot_record(bot_id).await?;
        let controller = self.query_bot_controller(bot_id).await?;
        let mut descendants = Vec::new();
        if let Some(snapshot) = &controller {
            for session in &snapshot.sessions {
                let listed = self
                    .list_sessions(SessionListParams {
                        metadata: Default::default(),
                        cursor: None,
                        limit: Some(MAX_SESSION_LIST_LIMIT as u32),
                        root_session_id: Some(session.session_id.clone()),
                        parent_session_id: None,
                        exclude_closed: false,
                    })
                    .await?;
                descendants.extend(listed.result.sessions);
            }
        }
        Ok(BotStateView {
            controller,
            descendants,
        })
    }

    /// Manual admission: an operator-authored event for the main session.
    pub(crate) async fn admit_bot_event_record(
        &self,
        bot_id: &BotId,
        event: BotEventInput,
    ) -> Result<(::bots::BotEventRecord, bool), AgentApiError> {
        let bot = self.read_bot_record(bot_id).await?;
        if event.kind.trim().is_empty() || event.kind.len() > 200 {
            return Err(AgentApiError::invalid_request(
                "event kind must be 1..=200 bytes",
            ));
        }
        if event.summary.trim().is_empty() || event.summary.len() > 2_000 {
            return Err(AgentApiError::invalid_request(
                "event summary must be 1..=2000 bytes",
            ));
        }
        let event_id = match event.event_id {
            Some(id) if !id.is_empty() && id.len() <= 200 => id,
            Some(_) => {
                return Err(AgentApiError::invalid_request(
                    "eventId must be 1..=200 bytes",
                ));
            }
            None => manual_event_id(uuid::Uuid::new_v4()),
        };
        let document = BotEventDocument {
            version: BotEventDocument::VERSION,
            kind: event.kind,
            source: "manual".to_owned(),
            occurred_at_ms: event.occurred_at_ms.unwrap_or_else(bot_now_ms),
            summary: event.summary,
            data: event.data,
            headers: event.headers,
            correlation_id: event.correlation_id,
            links: event.links,
            sender: None,
            hops: 0,
            in_reply_to: None,
        };
        let stored = self
            .store_bot_event(&bot, StoreBotEventInput::new(event_id, document))
            .await
            .map_err(map_bot_error)?;
        Ok((stored.record, stored.duplicate))
    }

    /// Re-admit a stored event as a fresh one with the original routing;
    /// the replay never coalesces.
    pub(crate) async fn replay_bot_event_record(
        &self,
        bot_id: &BotId,
        seq: u64,
    ) -> Result<::bots::BotEventRecord, AgentApiError> {
        let bot = self.read_bot_record(bot_id).await?;
        let original = self
            .store()
            .read_bot_event_by_seq(bot_id, seq)
            .await
            .map_err(map_bot_error)?;
        let document = self.read_bot_event_document(&original).await?;
        let mut input = StoreBotEventInput::new(replay_event_id(uuid::Uuid::new_v4()), document);
        input.trigger_id = original.trigger_id.clone();
        input.document_ref = Some(original.document_ref.clone());
        input.session = original.session.clone();
        input.media = original.media.clone();
        input.receiver = original.receiver.clone();
        input.hops = original.hops;
        input.sender_bot_id = original.sender_bot_id.clone();
        let stored = self
            .store_bot_event(&bot, input)
            .await
            .map_err(map_bot_error)?;
        Ok(stored.record)
    }

    pub(crate) async fn list_bot_events_page(
        &self,
        bot_id: &BotId,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<(Vec<::bots::BotEventRecord>, Option<String>), AgentApiError> {
        self.read_bot_record(bot_id).await?;
        let limit = limit
            .map(|limit| limit as usize)
            .unwrap_or(DEFAULT_EVENT_PAGE)
            .clamp(1, MAX_EVENT_PAGE);
        let before = cursor.as_deref().map(decode_event_cursor).transpose()?;
        let mut records = self
            .store()
            .list_bot_events(bot_id, limit + 1, before)
            .await
            .map_err(map_bot_error)?;
        let next_cursor = if records.len() > limit {
            records.truncate(limit);
            records.last().map(encode_event_cursor)
        } else {
            None
        };
        Ok((records, next_cursor))
    }

    pub(crate) async fn test_bot_filter_records(
        &self,
        params: BotFilterTestParams,
    ) -> Result<BotFilterTestResponse, AgentApiError> {
        self.read_bot_record(&params.bot_id).await?;
        ::bots::filter::validate_expression(&params.filter)
            .map_err(|error| AgentApiError::invalid_request(format!("invalid CEL: {error}")))?;
        let mut results = Vec::new();
        if let Some(payload) = params.payload {
            let document = BotEventDocument {
                version: BotEventDocument::VERSION,
                kind: payload
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("test")
                    .to_owned(),
                source: "filter-test".to_owned(),
                occurred_at_ms: bot_now_ms(),
                summary: "filter test".to_owned(),
                data: payload.get("data").cloned(),
                headers: payload
                    .get("headers")
                    .and_then(|headers| serde_json::from_value(headers.clone()).ok())
                    .unwrap_or_default(),
                correlation_id: None,
                links: Vec::new(),
                sender: None,
                hops: 0,
                in_reply_to: None,
            };
            let context = FilterContext::from_document("filter-test", &document);
            let result = evaluate_filter(&params.filter, &context);
            results.push(BotFilterTestResult {
                seq: None,
                matched: result.matched,
                error: result.error,
            });
        } else {
            let limit = params
                .limit
                .map(|limit| limit as usize)
                .unwrap_or(DEFAULT_FILTER_SAMPLE)
                .clamp(1, MAX_FILTER_SAMPLE);
            let records = self
                .store()
                .list_bot_events(&params.bot_id, limit, None)
                .await
                .map_err(map_bot_error)?;
            for record in records {
                let document = self.read_bot_event_document(&record).await?;
                let context = FilterContext::from_document(&record.event_id, &document);
                let result = evaluate_filter(&params.filter, &context);
                results.push(BotFilterTestResult {
                    seq: Some(record.seq),
                    matched: result.matched,
                    error: result.error,
                });
            }
        }
        Ok(BotFilterTestResponse {
            sampled: results.len() as u32,
            matched: results.iter().filter(|result| result.matched).count() as u32,
            errors: results
                .iter()
                .filter(|result| result.error.is_some())
                .count() as u32,
            results,
        })
    }

    pub(crate) async fn rotate_bot_session_record(
        &self,
        bot_id: &BotId,
        session_id: &str,
    ) -> Result<bool, AgentApiError> {
        self.read_bot_record(bot_id).await?;
        if !is_bot_session(bot_id, session_id) {
            return Err(AgentApiError::invalid_request(format!(
                "session {session_id} does not belong to bot {bot_id}"
            )));
        }
        let Some(snapshot) = self.query_bot_controller(bot_id).await? else {
            return Err(AgentApiError::rejected(format!(
                "bot {bot_id} has no running controller to rotate a session"
            )));
        };
        let known = snapshot.sessions.iter().any(|session| {
            session.session_id == session_id
                || routed_session_base(&session.session_id) == routed_session_base(session_id)
        });
        if !known {
            return Err(AgentApiError::not_found(format!(
                "session {session_id} is not one of bot {bot_id}'s sessions"
            )));
        }
        self.bot_controller_handle(bot_id)
            .signal(
                UntypedSignal::new(::bots::BOT_SESSION_ROTATE_SIGNAL),
                RawValue::from_value(
                    &::bots::BotSessionRotate {
                        session_id: session_id.to_owned(),
                    },
                    &PayloadConverter::default(),
                ),
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(|error| AgentApiError::internal(format!("signal bot controller: {error}")))?;
        Ok(true)
    }

    /// Tell every open bot on a profile that the profile moved; the
    /// controllers re-apply it at their next idle boundary.
    pub(crate) async fn signal_bots_for_profile(&self, profile_id: &ProfileId) {
        let bots = match self.store().list_bots_for_profile(profile_id).await {
            Ok(bots) => bots,
            Err(error) => {
                tracing::warn!(target: "temporal_server", %profile_id, %error, "list bots for profile failed");
                return;
            }
        };
        for bot in bots {
            if let Err(error) = self.signal_bot_config(&bot).await {
                tracing::warn!(target: "temporal_server", bot_id = %bot.bot_id, %error, "signal bot config after profile put failed");
            }
        }
    }

    // ── AgentApiService entry points ────────────────────────────────────

    pub(super) async fn create_bot_record(
        &self,
        params: BotCreateParams,
    ) -> Result<BotCreateResponse, AgentApiError> {
        let store = self.store();
        let BotInput { bot_id, document } = params.bot;
        self.require_profile(&document.profile_id).await?;
        // A bot created into a collection is a member of it, its sessions
        // included; otherwise the bot is its own root and its sessions
        // follow it. Either way its sessions carry its audience.
        let resource = ResourceRef::Bot(bot_id.as_str().to_owned());
        let root = params
            .access
            .as_ref()
            .and_then(|access| access.root.clone());
        self.reserve_resource(resource.clone(), root.as_ref())
            .await?;
        self.apply_creation_access(&resource, params.access).await?;
        let bot = store
            .create_bot(bot_id.clone(), document, bot_now_ms())
            .await
            .map_err(map_bot_error)?;
        let mut triggers = Vec::new();
        let rollback = |store: Arc<PgStore>, bot_id: BotId| async move {
            let _ = store.delete_bot(&bot_id).await;
        };
        for trigger in params.triggers {
            match self.put_bot_trigger_record(&bot_id, trigger, None).await {
                Ok(record) => triggers.push(record),
                Err(error) => {
                    for created in &triggers {
                        let _ = self
                            .delete_bot_trigger_schedule(&bot_id, &created.trigger_id)
                            .await;
                    }
                    rollback(store.clone(), bot_id).await;
                    return Err(error);
                }
            }
        }
        if let Err(error) = self.signal_bot_config(&bot).await {
            for created in &triggers {
                let _ = self
                    .delete_bot_trigger_schedule(&bot_id, &created.trigger_id)
                    .await;
            }
            rollback(store.clone(), bot_id).await;
            return Err(AgentApiError::internal(format!(
                "start bot controller: {error}"
            )));
        }
        Ok(BotCreateResponse {
            bot: bot.view(),
            triggers: triggers
                .iter()
                .map(|record| self.trigger_view(record))
                .collect(),
        })
    }

    pub(super) async fn list_bot_roster(&self) -> Result<BotListResponse, AgentApiError> {
        let reader = self.reader()?;
        let rows = self
            .store()
            .list_bot_roster_for(&reader)
            .await
            .map_err(map_bot_error)?;
        Ok(BotListResponse {
            bots: rows
                .into_iter()
                .map(|row| BotListItem {
                    bot: row.bot.view(),
                    trigger_count: row.trigger_count,
                    pending_count: row.pending_count,
                    last_event: row.last_event.map(|event| event.view()),
                })
                .collect(),
        })
    }

    pub(super) async fn read_bot_event_with_document(
        &self,
        bot_id: &BotId,
        seq: u64,
    ) -> Result<BotEventReadResponse, AgentApiError> {
        self.read_bot_record(bot_id).await?;
        let record = self
            .store()
            .read_bot_event_by_seq(bot_id, seq)
            .await
            .map_err(map_bot_error)?;
        let document = self.read_bot_event_document(&record).await?;
        Ok(BotEventReadResponse {
            event: record.view(),
            document,
        })
    }
}

impl From<AdmitTriggerOutcome> for Option<InsertBotEventOutcome> {
    fn from(outcome: AdmitTriggerOutcome) -> Self {
        match outcome {
            AdmitTriggerOutcome::Admitted(stored) if stored.duplicate => {
                Some(InsertBotEventOutcome::Duplicate(stored.record))
            }
            AdmitTriggerOutcome::Admitted(stored) => {
                Some(InsertBotEventOutcome::Inserted(stored.record))
            }
            AdmitTriggerOutcome::Filtered { .. } => None,
        }
    }
}
