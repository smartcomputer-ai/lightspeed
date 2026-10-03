//! `channels/inbound/admit`: which chat trigger serves a conversation,
//! whether it is paired, and the signal-with-start of the conversation
//! workflow. The connector host calls this once per provider message and
//! sends pairing replies itself from the decision.

use api::{
    AgentApiError, BotTriggerKind, BotTriggerSpec, ChannelConversationReadParams,
    ChannelConversationReadResponse, ChannelInboundAdmitParams, ChannelInboundAdmitResponse,
    ChannelInboundDecision, ChatActivation, ChatPairing, ChatScope,
};
use bots::{BotRecord, BotStore, BotTriggerRecord, BotTriggerStore};
use channels::{
    ChannelAccountStore, ChannelPairingRecord, ChannelPairingStore, ConversationRef,
    connector_task_queue, conversation_workflow_id,
    inbound::{
        AdmittedInbound, ConversationStart, NormalizedInbound, conversation_label,
        normalize_inbound,
    },
    policy::{authorize_sender, trigger_prefixes},
};
use temporal_workflow::channels::{CHAT_INBOUND_SIGNAL, CHAT_STATE_QUERY, ChannelConversationArgs};
use temporalio_client::{
    UntypedQuery, UntypedWorkflow, WorkflowQueryOptions, WorkflowStartOptions, WorkflowStartSignal,
    errors::WorkflowQueryError,
};
use temporalio_common::{
    data_converters::{PayloadConverter, RawValue},
    protos::{coresdk::AsJsonPayloadExt as _, temporal::api::common::v1::Payloads},
};

use super::map_channel_error;
use crate::{bots::map_bot_error, gateway::GatewayAgentApi};

pub const CHANNEL_CONVERSATION_WORKFLOW_TYPE: &str = "ChannelConversationWorkflow";

/// A chat trigger that could serve the conversation, with its bot.
#[derive(Clone, Debug)]
pub struct ChatTriggerCandidate {
    pub bot: BotRecord,
    pub trigger: BotTriggerRecord,
}

impl ChatTriggerCandidate {
    fn pairing(&self) -> ChatPairing {
        match &self.trigger.document.spec {
            BotTriggerSpec::Chat { pairing, .. } => *pairing,
            _ => ChatPairing::Open,
        }
    }

    fn priority(&self) -> u32 {
        match &self.trigger.document.spec {
            BotTriggerSpec::Chat { priority, .. } => *priority,
            _ => u32::MAX,
        }
    }

    fn activation(&self) -> ChatActivation {
        match &self.trigger.document.spec {
            BotTriggerSpec::Chat { activation, .. } => activation.clone(),
            _ => ChatActivation::default(),
        }
    }

    fn serves_scope(&self, scope: ChatScope) -> bool {
        match &self.trigger.document.spec {
            BotTriggerSpec::Chat { match_scope, .. } => {
                match_scope.is_none_or(|wanted| wanted == scope)
            }
            _ => false,
        }
    }
}

/// The pure admission decision over the candidates of one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionPlan {
    /// Route the message to the chat's paired trigger.
    Bound { index: usize },
    /// An open trigger claims the unpaired chat: record the pairing and
    /// route the message.
    Claim { index: usize },
    /// The message was this trigger's pairing code: record the pairing,
    /// consume the message.
    Pair { index: usize },
    /// A candidate would serve the chat but it is not paired; the message
    /// looked addressed to the bot, so prompt for the code.
    PairingRequired { index: usize },
    /// Same, but ambient traffic: stay silent.
    PairingPending { index: usize },
    /// No candidate serves this account and scope — or the chat is paired
    /// to a trigger that is currently inactive (disabled, its bot paused):
    /// the pairing survives and the chat stays silent until the owner is
    /// back or the pairing is deleted.
    Unbound,
}

/// Pairing is the routing authority: a paired chat routes only to its
/// paired trigger — whoever paired it first has the route, and no other
/// trigger sees the chat while the pairing exists. An unpaired chat is
/// claimed by the best open trigger, or by a trigger whose pairing code
/// the message is; otherwise prompt when the message looks addressed to
/// the bot. Candidates are ordered by priority.
pub fn plan_admission(
    candidates: &[ChatTriggerCandidate],
    paired_trigger: Option<&api::BotTriggerId>,
    inbound: &NormalizedInbound,
) -> AdmissionPlan {
    if let Some(paired) = paired_trigger {
        return match candidates
            .iter()
            .position(|candidate| &candidate.trigger.trigger_id == paired)
        {
            Some(index) => AdmissionPlan::Bound { index },
            // The owner is inactive: park the chat, never reroute it.
            None => AdmissionPlan::Unbound,
        };
    }
    if candidates.is_empty() {
        return AdmissionPlan::Unbound;
    }
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.pairing() == ChatPairing::Open)
    {
        return AdmissionPlan::Claim { index };
    }
    let text = inbound.inbound.text.trim();
    if !text.is_empty()
        && let Some(index) = candidates.iter().position(|candidate| {
            candidate
                .trigger
                .secrets
                .pairing_code
                .as_deref()
                .is_some_and(|code| code == text)
        })
    {
        return AdmissionPlan::Pair { index };
    }
    let first = &candidates[0];
    if should_prompt_for_pairing(inbound, &first.activation()) {
        AdmissionPlan::PairingRequired { index: 0 }
    } else {
        AdmissionPlan::PairingPending { index: 0 }
    }
}

/// Prompt on a direct message, a mention, a reply to the bot, or a trigger
/// prefix; never on ambient group chatter.
pub fn should_prompt_for_pairing(inbound: &NormalizedInbound, activation: &ChatActivation) -> bool {
    let message = &inbound.inbound;
    if message.is_direct || message.mentioned_bot || message.is_reply_to_bot {
        return true;
    }
    let text = message.text.trim_start();
    trigger_prefixes(activation)
        .iter()
        .any(|prefix| text.to_lowercase().starts_with(&prefix.to_lowercase()))
}

impl GatewayAgentApi {
    /// Every enabled chat trigger of the universe that serves this account
    /// and scope, on an enabled, open bot, ordered by priority then id.
    async fn chat_trigger_candidates(
        &self,
        account_id: &api::ChannelAccountId,
        scope: ChatScope,
    ) -> Result<Vec<ChatTriggerCandidate>, AgentApiError> {
        let store = self.store();
        let triggers = store
            .list_bot_triggers_by_kind(BotTriggerKind::Chat)
            .await
            .map_err(map_bot_error)?;
        let mut candidates = Vec::new();
        for trigger in triggers {
            if !trigger.enabled() {
                continue;
            }
            let serves_account = matches!(
                &trigger.document.spec,
                BotTriggerSpec::Chat { account_id: spec_account, .. } if spec_account == account_id.as_str()
            );
            if !serves_account {
                continue;
            }
            let bot = match store.read_bot(&trigger.bot_id).await {
                Ok(bot) => bot,
                Err(bots::BotError::BotNotFound { .. }) => continue,
                Err(error) => return Err(map_bot_error(error)),
            };
            if !bot.document.enabled || bot.is_closed() {
                continue;
            }
            let candidate = ChatTriggerCandidate { bot, trigger };
            if candidate.serves_scope(scope) {
                candidates.push(candidate);
            }
        }
        candidates.sort_by(|a, b| {
            a.priority()
                .cmp(&b.priority())
                .then_with(|| a.trigger.created_at_ms.cmp(&b.trigger.created_at_ms))
                .then_with(|| a.trigger.trigger_id.cmp(&b.trigger.trigger_id))
        });
        Ok(candidates)
    }

    pub(crate) async fn admit_channel_inbound_message(
        &self,
        params: ChannelInboundAdmitParams,
    ) -> Result<ChannelInboundAdmitResponse, AgentApiError> {
        let accounts: &dyn ChannelAccountStore = self.store().as_ref();
        let account = accounts
            .read_channel_account(&params.account_id)
            .await
            .map_err(map_channel_error)?;
        let unbound = ChannelInboundAdmitResponse {
            decision: ChannelInboundDecision::Unbound,
            bot_id: None,
            trigger_id: None,
        };
        if !account.enabled() {
            return Ok(unbound);
        }
        let inbound = normalize_inbound(&params.inbound).map_err(map_channel_error)?;
        let inbound = NormalizedInbound {
            provider: account.provider().clone(),
            account_id: account.account_id.clone(),
            inbound,
        };
        let scope = inbound.scope();
        let candidates = self
            .chat_trigger_candidates(&account.account_id, scope)
            .await?;
        let pairings: &dyn ChannelPairingStore = self.store().as_ref();
        let paired = pairings
            .read_channel_pairing(&account.account_id, &inbound.inbound.chat_id)
            .await
            .map_err(map_channel_error)?;
        let plan = plan_admission(
            &candidates,
            paired.as_ref().map(|pairing| &pairing.trigger_id),
            &inbound,
        );
        let (decision, candidate) = match plan {
            AdmissionPlan::Unbound => return Ok(unbound),
            AdmissionPlan::Bound { index } => (ChannelInboundDecision::Bound, &candidates[index]),
            AdmissionPlan::Claim { index } => {
                let candidate = &candidates[index];
                pairings
                    .upsert_channel_pairing(ChannelPairingRecord {
                        account_id: account.account_id.clone(),
                        chat_id: inbound.inbound.chat_id.clone(),
                        bot_id: candidate.bot.bot_id.clone(),
                        trigger_id: candidate.trigger.trigger_id.clone(),
                        paired_via: api::ChannelPairedVia::Open,
                        paired_at_ms: crate::bots::now_ms(),
                    })
                    .await
                    .map_err(map_channel_error)?;
                (ChannelInboundDecision::Bound, candidate)
            }
            AdmissionPlan::Pair { index } => {
                let candidate = &candidates[index];
                pairings
                    .upsert_channel_pairing(ChannelPairingRecord {
                        account_id: account.account_id.clone(),
                        chat_id: inbound.inbound.chat_id.clone(),
                        bot_id: candidate.bot.bot_id.clone(),
                        trigger_id: candidate.trigger.trigger_id.clone(),
                        paired_via: api::ChannelPairedVia::Code,
                        paired_at_ms: crate::bots::now_ms(),
                    })
                    .await
                    .map_err(map_channel_error)?;
                (ChannelInboundDecision::Paired, candidate)
            }
            AdmissionPlan::PairingRequired { index } => {
                (ChannelInboundDecision::PairingRequired, &candidates[index])
            }
            AdmissionPlan::PairingPending { index } => {
                (ChannelInboundDecision::PairingPending, &candidates[index])
            }
        };
        if decision == ChannelInboundDecision::Bound {
            self.signal_conversation_inbound(&account.account_id, candidate, inbound)
                .await?;
        }
        Ok(ChannelInboundAdmitResponse {
            decision,
            bot_id: Some(candidate.bot.bot_id.clone()),
            trigger_id: Some(candidate.trigger.trigger_id.clone()),
        })
    }

    /// Signal-with-start the conversation workflow with one admitted
    /// message. The start input is secret-free and re-derived on every
    /// call; an already-running workflow just receives the signal.
    async fn signal_conversation_inbound(
        &self,
        account_id: &api::ChannelAccountId,
        candidate: &ChatTriggerCandidate,
        inbound: NormalizedInbound,
    ) -> Result<(), AgentApiError> {
        let (activation, access) = match &candidate.trigger.document.spec {
            BotTriggerSpec::Chat {
                activation, access, ..
            } => (activation.clone(), access.clone()),
            _ => {
                return Err(AgentApiError::internal(
                    "chat candidate without a chat spec",
                ));
            }
        };
        let universe_id = self.universe_id();
        let conversation: ConversationRef = inbound.conversation();
        let start = ConversationStart {
            universe_id,
            bot_id: candidate.bot.bot_id.clone(),
            trigger_id: candidate.trigger.trigger_id.clone(),
            account_id: account_id.clone(),
            provider: inbound.provider.clone(),
            conversation: conversation.clone(),
            scope: inbound.scope(),
            activation,
            access: access.clone(),
            label: conversation_label(&inbound),
            connector_task_queue: connector_task_queue(universe_id, &inbound.provider, account_id),
        };
        let authorization = authorize_sender(&access, &inbound.inbound.sender_id);
        let admitted = AdmittedInbound {
            inbound,
            authorization,
        };
        let workflow_id = start.workflow_id();
        let payload = admitted
            .as_json_payload()
            .map_err(|error| AgentApiError::internal(format!("encode inbound: {error}")))?;
        let options = WorkflowStartOptions::new(self.channel_task_queue.clone(), workflow_id)
            .start_signal(
                WorkflowStartSignal::new(CHAT_INBOUND_SIGNAL)
                    .input(Payloads {
                        payloads: vec![payload],
                    })
                    .build(),
            )
            .build();
        let input = RawValue::from_value(
            &ChannelConversationArgs { start, carry: None },
            &PayloadConverter::default(),
        );
        self.temporal_client()
            .start_workflow(
                UntypedWorkflow::new(CHANNEL_CONVERSATION_WORKFLOW_TYPE),
                input,
                options,
            )
            .await
            .map(|_| ())
            .map_err(|error| {
                AgentApiError::internal(format!("signal conversation workflow: {error}"))
            })
    }

    pub(crate) async fn read_channel_conversation_snapshot(
        &self,
        params: ChannelConversationReadParams,
    ) -> Result<ChannelConversationReadResponse, AgentApiError> {
        let accounts: &dyn ChannelAccountStore = self.store().as_ref();
        let account = accounts
            .read_channel_account(&params.account_id)
            .await
            .map_err(map_channel_error)?;
        let conversation = ConversationRef {
            account_id: account.account_id.clone(),
            chat_id: params.chat_id,
            thread_id: params.thread_id,
        };
        let workflow_id =
            conversation_workflow_id(self.universe_id(), account.provider(), &conversation);
        let handle = self
            .temporal_client()
            .get_workflow_handle::<UntypedWorkflow>(workflow_id);
        match handle
            .query(
                UntypedQuery::new(CHAT_STATE_QUERY),
                RawValue::from_value(&(), &PayloadConverter::default()),
                WorkflowQueryOptions::default(),
            )
            .await
        {
            Ok(raw) => {
                let payload = raw.payloads.first().ok_or_else(|| {
                    AgentApiError::internal("conversation query returned no payload")
                })?;
                let snapshot = serde_json::from_slice(&payload.data).map_err(|error| {
                    AgentApiError::internal(format!("conversation query payload: {error}"))
                })?;
                Ok(ChannelConversationReadResponse {
                    conversation: Some(snapshot),
                })
            }
            Err(WorkflowQueryError::NotFound(_)) => {
                Ok(ChannelConversationReadResponse { conversation: None })
            }
            Err(error) => Err(AgentApiError::internal(format!(
                "query conversation workflow: {error}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{BotDocument, BotId, BotTriggerDocument, BotTriggerId, ChannelInbound, ProfileId};

    fn candidate(
        id: &str,
        pairing: ChatPairing,
        code: Option<&str>,
        priority: u32,
    ) -> ChatTriggerCandidate {
        let bot_id = BotId::new("triage");
        ChatTriggerCandidate {
            bot: BotRecord {
                bot_id: bot_id.clone(),
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
            },
            trigger: BotTriggerRecord {
                bot_id,
                trigger_id: BotTriggerId::new(id),
                revision: 1,
                document: BotTriggerDocument {
                    spec: BotTriggerSpec::Chat {
                        account_id: "tg".to_owned(),
                        match_scope: None,
                        activation: ChatActivation::default(),
                        access: Default::default(),
                        pairing,
                        priority,
                    },
                    filter: None,
                    route: None,
                    coalesce: None,
                    deliver: None,
                    session_close_after_ms: None,
                    enabled: true,
                },
                secrets: bots::BotTriggerSecrets {
                    webhook_token: None,
                    pairing_code: code.map(str::to_owned),
                },
                disabled_reason: None,
                disabled_at_ms: None,
                last_filter_error: None,
                last_filter_error_at_ms: None,
                cursor: None,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
        }
    }

    fn inbound(text: &str, direct: bool) -> NormalizedInbound {
        NormalizedInbound {
            provider: api::ChannelProvider::new("telegram"),
            account_id: api::ChannelAccountId::new("tg"),
            inbound: ChannelInbound {
                message_id: "1".to_owned(),
                chat_id: "c".to_owned(),
                thread_id: None,
                sender_id: "u".to_owned(),
                sender_name: "U".to_owned(),
                timestamp_ms: 0,
                text: text.to_owned(),
                media: Vec::new(),
                is_direct: direct,
                mentioned_bot: false,
                is_reply_to_bot: false,
            },
        }
    }

    #[test]
    fn open_trigger_claims_unpaired_chat() {
        let candidates = vec![candidate("open", ChatPairing::Open, None, 100)];
        assert_eq!(
            plan_admission(&candidates, None, &inbound("hi", false)),
            AdmissionPlan::Claim { index: 0 }
        );
    }

    #[test]
    fn paired_trigger_beats_open_candidates() {
        let candidates = vec![
            candidate("thief", ChatPairing::Open, None, 1),
            candidate("owner", ChatPairing::Code, Some("ABCDEFGHJKLM"), 100),
        ];
        let owner = BotTriggerId::new("owner");
        assert_eq!(
            plan_admission(&candidates, Some(&owner), &inbound("hi", false)),
            AdmissionPlan::Bound { index: 1 }
        );
    }

    #[test]
    fn paired_to_inactive_trigger_parks_the_chat() {
        // The owner is disabled (not a candidate); an eager open trigger
        // must not take the chat over.
        let candidates = vec![candidate("thief", ChatPairing::Open, None, 1)];
        let owner = BotTriggerId::new("owner");
        assert_eq!(
            plan_admission(&candidates, Some(&owner), &inbound("hi", false)),
            AdmissionPlan::Unbound
        );
    }

    #[test]
    fn paired_chat_binds_and_code_pairs() {
        let candidates = vec![candidate(
            "code",
            ChatPairing::Code,
            Some("ABCDEFGHJKLM"),
            100,
        )];
        let trigger = BotTriggerId::new("code");
        assert_eq!(
            plan_admission(&candidates, Some(&trigger), &inbound("hi", false)),
            AdmissionPlan::Bound { index: 0 }
        );
        assert_eq!(
            plan_admission(&candidates, None, &inbound(" ABCDEFGHJKLM ", false)),
            AdmissionPlan::Pair { index: 0 }
        );
        assert_eq!(
            plan_admission(&candidates, None, &inbound("hello?", true)),
            AdmissionPlan::PairingRequired { index: 0 }
        );
        assert_eq!(
            plan_admission(&candidates, None, &inbound("ambient chatter", false)),
            AdmissionPlan::PairingPending { index: 0 }
        );
        assert_eq!(
            plan_admission(&[], None, &inbound("x", true)),
            AdmissionPlan::Unbound
        );
    }
}
