//! Signal, query, and activity shapes of the conversation workflow. The
//! connector-facing payloads (`ChannelDeliveryCommand`,
//! `PrepareChannelMediaInput`, …) are defined in the `channels` crate and
//! exported through the workflow contract for the TypeScript connector
//! host.

use api::{BotEventOutcome, BotId, BotTriggerId, ChannelAccountId, ChannelProvider, ChatScope};
use channels::{ConversationRef, media::PreparedMediaItem, state::ChatHandle};
use engine::WorkflowEndpointRef;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use channels::inbound::{AdmittedInbound, ConversationStart};

pub const CHAT_INBOUND_SIGNAL: &str = "chat_inbound";
pub const CHAT_STATE_QUERY: &str = "chat_state";
/// Workflow kind recorded on the `message_*` receiver endpoint and on the
/// event's notify route.
pub const CHANNEL_CONVERSATION_WORKFLOW_KIND: &str = "ChannelConversationWorkflow";

// ── Core-side activities ────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatToolDeclarationsRequest {
    pub universe_id: Uuid,
    /// This conversation workflow: the receiver every `message_*` call is
    /// pushed to.
    pub receiver: WorkflowEndpointRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatToolDeclarationsResult {
    /// CAS ref of the declaration array; content-addressed, so stable per
    /// receiver.
    pub tools_ref: String,
    #[serde(default)]
    pub tool_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatReadJsonBlobRequest {
    pub universe_id: Uuid,
    pub blob_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPutJsonBlobRequest {
    pub universe_id: Uuid,
    pub value: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPutJsonBlobResult {
    pub blob_ref: String,
}

/// After the bot's delivery finished: nothing to do when the run answered
/// through a `message_*` tool, otherwise the assistant's final text (or a
/// failure line) to send as the reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatReconcileDeliveryRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotEventOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChatReconcileDeliveryResult {
    Suppress { reason: ChatSuppressReason },
    Deliver { text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatSuppressReason {
    /// The run used a `message_*` tool; the model already answered.
    MessagingTool,
    /// No run happened (steered, appended).
    NoRun,
}

/// The message of a chat event as admission stores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub message_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub timestamp_ms: i64,
    /// The activated text (prefix or mention stripped).
    pub text: String,
    pub is_direct: bool,
    pub mentioned_bot: bool,
    pub is_reply_to_bot: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatEmitEventRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub account_id: ChannelAccountId,
    pub provider: ChannelProvider,
    pub conversation: ConversationRef,
    pub label: String,
    pub scope: ChatScope,
    pub message: ChatMessage,
    #[serde(default)]
    pub media: Vec<PreparedMediaItem>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub transcript_refs: std::collections::BTreeMap<String, String>,
    /// CAS ref of the conversation's `message_*` declarations.
    pub tools_ref: String,
    /// This workflow, for `started` / `finished` receipts.
    pub notify: WorkflowEndpointRef,
    /// Opaque token echoed on receipts (the inbound dedupe key).
    pub notify_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChatEmitEventResult {
    Admitted {
        event_id: String,
        seq: u64,
        /// The routed session (logical base id).
        session_id: Option<String>,
    },
    Duplicate {
        event_id: String,
        seq: u64,
    },
    /// The trigger's filter refused it; nothing stored.
    Filtered {
        event_id: String,
    },
    Refused {
        reason: ChatRefusalReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRefusalReason {
    BreakerTripped,
    TriggerDisabled,
    BotDisabled,
    BotClosed,
}

/// Archive the bot's own send beside the conversation's events so it gets
/// a `#N`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStoreSentRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub account_id: ChannelAccountId,
    pub provider: ChannelProvider,
    pub conversation: ConversationRef,
    pub label: String,
    /// Stable across retries: the invocation id, or `fallback:{delivery}`.
    pub invocation_key: String,
    pub text: String,
    pub provider_message_ids: Vec<String>,
    /// The message number this send replied to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatStoreSentResult {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResolveHandleRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    /// The conversation the handle must belong to; a number from another
    /// chat is unknown here.
    pub conversation_key: String,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResolveHandleResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<ChatHandle>,
    /// The bot's event numbers run `1..=max_seq`, for the "unknown #N"
    /// error.
    pub max_seq: u64,
}

/// Liveness gate before delivering: the trigger, bot, account, and pairing
/// still serve this conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAssertTriggerActiveRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub account_id: ChannelAccountId,
    pub chat_id: String,
    pub scope: ChatScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChatTriggerActiveResult {
    Active,
    Inactive { reason: String },
}

/// Starts or reads independent transcription on the sessions worker queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatTranscribeMediaRequest {
    pub active: ChatAssertTriggerActiveRequest,
    pub idempotency_key: String,
    pub audio: api::TranscriptionAudio,
}
