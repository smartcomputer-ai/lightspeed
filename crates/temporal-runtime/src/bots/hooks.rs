//! Public webhook ingress: `POST /hooks/bots/{universe}/{bot}/{trigger}/{token}`
//! on the gateway, outside RPC auth. The URL token is checked in constant
//! time before anything else — a probe of the path never produces a lease
//! or a stored row — then the optional HMAC over the raw body with a secret
//! leased in-process, then the shared trigger pipeline.

use std::collections::BTreeMap;

use api::{
    AuthGrantLeaseParams, BotEventDocument, BotId, BotTriggerId, BotTriggerSpec,
    WebhookVerification,
};
use bots::{
    BotError, BotRefusalCode, BotStore, BotTriggerStore,
    webhook::{
        WebhookRefusal, constant_time_eq, extract_webhook_event, sanitize_headers, verify_webhook,
    },
};

use super::admission::{AdmitTriggerOutcome, StoreBotEventInput};
use crate::gateway::GatewayAgentApi;

pub const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;

/// HTTP outcome of one webhook delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookIngestOutcome {
    /// 202: stored (or already stored) and the controller woken.
    Admitted { event_id: String, duplicate: bool },
    /// 202: the trigger's filter refused it; nothing stored.
    Filtered { error: Option<String> },
    /// 404: unknown universe/bot/trigger, wrong kind, or wrong token —
    /// indistinguishable on purpose.
    UnknownEndpoint,
    /// 401: the signature did not verify.
    Unauthorized { message: String },
    /// 410: the bot was closed.
    Gone,
    /// 409: the bot or trigger is disabled.
    Disabled { message: String },
    /// 429: the flood breaker tripped (and disabled the trigger).
    Throttled { message: String },
    /// 413: body over the cap.
    TooLarge,
    /// 503: the signing secret could not be leased.
    SecretUnavailable { message: String },
    /// 400: the body is not a usable payload.
    BadPayload { message: String },
    /// 502: admission failed inside the runtime.
    Failed { message: String },
}

impl GatewayAgentApi {
    pub async fn ingest_bot_webhook(
        &self,
        bot_id: &str,
        trigger_id: &str,
        token: &str,
        headers: BTreeMap<String, String>,
        body: &[u8],
    ) -> WebhookIngestOutcome {
        let (Ok(bot_id), Ok(trigger_id)) =
            (BotId::try_new(bot_id), BotTriggerId::try_new(trigger_id))
        else {
            return WebhookIngestOutcome::UnknownEndpoint;
        };
        let store = self.store();
        let Ok(trigger) = store.read_bot_trigger(&bot_id, &trigger_id).await else {
            return WebhookIngestOutcome::UnknownEndpoint;
        };
        let (verification, preset) = match &trigger.document.spec {
            BotTriggerSpec::Webhook {
                verification,
                preset,
            } => (verification.clone(), *preset),
            _ => return WebhookIngestOutcome::UnknownEndpoint,
        };
        let Some(expected_token) = trigger.secrets.webhook_token.as_deref() else {
            return WebhookIngestOutcome::UnknownEndpoint;
        };
        // Constant time, and before any lease: a path probe never produces a
        // credential or audit event.
        if !constant_time_eq(expected_token, token) {
            return WebhookIngestOutcome::UnknownEndpoint;
        }
        if body.len() > MAX_WEBHOOK_BODY_BYTES {
            return WebhookIngestOutcome::TooLarge;
        }
        let headers = sanitize_headers(
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        let signing_secret = match &verification {
            WebhookVerification::HmacSha256 {
                grant_id, audience, ..
            // Ingest has no caller: the stored trigger names its signing secret.
            } => match self
                .lease_grant_token(AuthGrantLeaseParams {
                    grant_id: grant_id.clone(),
                    audience: audience.clone(),
                })
                .await
            {
                Ok(leased) => Some(leased.result.token),
                Err(error) => {
                    return WebhookIngestOutcome::SecretUnavailable {
                        message: error.message,
                    };
                }
            },
            WebhookVerification::Token => None,
        };
        if let Err(refusal) = verify_webhook(
            &verification,
            expected_token,
            token,
            body,
            &headers,
            signing_secret.as_deref(),
        ) {
            return match refusal {
                WebhookRefusal::UnknownEndpoint => WebhookIngestOutcome::UnknownEndpoint,
                other => WebhookIngestOutcome::Unauthorized {
                    message: other.to_string(),
                },
            };
        }
        let Ok(bot) = store.read_bot(&bot_id).await else {
            return WebhookIngestOutcome::UnknownEndpoint;
        };
        if bot.is_closed() {
            return WebhookIngestOutcome::Gone;
        }
        if !bot.document.enabled {
            return WebhookIngestOutcome::Disabled {
                message: format!("bot {bot_id} is disabled"),
            };
        }
        if !trigger.enabled() {
            return WebhookIngestOutcome::Disabled {
                message: format!("trigger {trigger_id} is disabled"),
            };
        }
        if let Err(error) = self.check_trigger_breaker(&bot, &trigger).await {
            return match error {
                BotError::Refused {
                    code: BotRefusalCode::BreakerTripped,
                    message,
                } => WebhookIngestOutcome::Throttled { message },
                other => WebhookIngestOutcome::Failed {
                    message: other.to_string(),
                },
            };
        }
        let extracted = match extract_webhook_event(preset, body, &headers) {
            Ok(extracted) => extracted,
            Err(message) => return WebhookIngestOutcome::BadPayload { message },
        };
        let document = BotEventDocument {
            version: BotEventDocument::VERSION,
            kind: extracted.kind,
            source: format!("webhook:{trigger_id}"),
            occurred_at_ms: super::now_ms(),
            summary: extracted.summary,
            data: Some(extracted.data),
            headers,
            correlation_id: None,
            links: Vec::new(),
            sender: None,
            hops: 0,
            in_reply_to: None,
        };
        let mut input = StoreBotEventInput::new(extracted.event_id, document);
        input.prompt_data = extracted.prompt_data;
        match self.admit_trigger_event(&bot, &trigger, input).await {
            Ok(AdmitTriggerOutcome::Admitted(stored)) => WebhookIngestOutcome::Admitted {
                event_id: stored.record.event_id,
                duplicate: stored.duplicate,
            },
            Ok(AdmitTriggerOutcome::Filtered { error }) => WebhookIngestOutcome::Filtered { error },
            Err(BotError::Refused {
                code: BotRefusalCode::BotClosed,
                ..
            }) => WebhookIngestOutcome::Gone,
            Err(error) => WebhookIngestOutcome::Failed {
                message: error.to_string(),
            },
        }
    }
}
