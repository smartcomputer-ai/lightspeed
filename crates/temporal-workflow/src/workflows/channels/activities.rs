//! Activity definitions of the conversation workflow: the core-side set
//! (implemented in `temporal-server::worker::channels`) and the
//! connector-side set (implemented by the TypeScript connector host under
//! these exact names on the account's task queue).

use channels::{
    delivery::{ChannelDeliveryCommand, ChannelDeliveryResult},
    media::{MaintainChannelTypingInput, PrepareChannelMediaInput, PrepareChannelMediaResult},
};
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use super::types::*;

pub const ACTIVITY_CHAT_TOOL_DECLARATIONS: &str = "ChannelActivities::chat_tool_declarations";
pub const ACTIVITY_CHAT_READ_JSON_BLOB: &str = "ChannelActivities::read_json_blob";
pub const ACTIVITY_CHAT_PUT_JSON_BLOB: &str = "ChannelActivities::put_json_blob";
pub const ACTIVITY_CHAT_RECONCILE_DELIVERY: &str = "ChannelActivities::reconcile_delivery";
pub const ACTIVITY_CHAT_TRANSCRIBE_MEDIA: &str = "ChannelActivities::transcribe_media";
pub const ACTIVITY_CHAT_EMIT_EVENT: &str = "ChannelActivities::emit_chat_event";
pub const ACTIVITY_CHAT_STORE_SENT: &str = "ChannelActivities::store_chat_sent";
pub const ACTIVITY_CHAT_RESOLVE_HANDLE: &str = "ChannelActivities::resolve_chat_handle";
pub const ACTIVITY_CHAT_ASSERT_TRIGGER_ACTIVE: &str = "ChannelActivities::assert_trigger_active";

/// Connector activity names: what the TypeScript connector host registers.
pub const ACTIVITY_CONNECTOR_DELIVER_MESSAGE: &str = "deliverChannelMessage";
pub const ACTIVITY_CONNECTOR_PREPARE_MEDIA: &str = "prepareChannelMedia";
pub const ACTIVITY_CONNECTOR_MAINTAIN_TYPING: &str = "maintainChannelTyping";

pub struct ChannelActivities;

#[activities]
impl ChannelActivities {
    #[activity(name = ACTIVITY_CHAT_TRANSCRIBE_MEDIA)]
    pub async fn transcribe_media(
        _ctx: ActivityContext,
        _request: ChatTranscribeMediaRequest,
    ) -> Result<api::TranscriptionView, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Store the `message_*` declarations bound to this conversation as
    /// receiver; content-addressed, so stable per receiver.
    #[activity(name = ACTIVITY_CHAT_TOOL_DECLARATIONS)]
    pub async fn chat_tool_declarations(
        _ctx: ActivityContext,
        _request: ChatToolDeclarationsRequest,
    ) -> Result<ChatToolDeclarationsResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CHAT_READ_JSON_BLOB)]
    pub async fn read_json_blob(
        _ctx: ActivityContext,
        _request: ChatReadJsonBlobRequest,
    ) -> Result<serde_json::Value, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CHAT_PUT_JSON_BLOB)]
    pub async fn put_json_blob(
        _ctx: ActivityContext,
        _request: ChatPutJsonBlobRequest,
    ) -> Result<ChatPutJsonBlobResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CHAT_RECONCILE_DELIVERY)]
    pub async fn reconcile_delivery(
        _ctx: ActivityContext,
        _request: ChatReconcileDeliveryRequest,
    ) -> Result<ChatReconcileDeliveryResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Admit one message as a bot event through the chat trigger.
    #[activity(name = ACTIVITY_CHAT_EMIT_EVENT)]
    pub async fn emit_chat_event(
        _ctx: ActivityContext,
        _request: ChatEmitEventRequest,
    ) -> Result<ChatEmitEventResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CHAT_STORE_SENT)]
    pub async fn store_chat_sent(
        _ctx: ActivityContext,
        _request: ChatStoreSentRequest,
    ) -> Result<ChatStoreSentResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CHAT_RESOLVE_HANDLE)]
    pub async fn resolve_chat_handle(
        _ctx: ActivityContext,
        _request: ChatResolveHandleRequest,
    ) -> Result<ChatResolveHandleResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CHAT_ASSERT_TRIGGER_ACTIVE)]
    pub async fn assert_trigger_active(
        _ctx: ActivityContext,
        _request: ChatAssertTriggerActiveRequest,
    ) -> Result<ChatTriggerActiveResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }
}

/// Activities the connector host serves on its per-account task queue.
/// Never implemented in Rust; the definitions let the workflow schedule
/// them with `ActivityOptions::task_queue`.
pub struct ConnectorActivities;

#[activities]
impl ConnectorActivities {
    /// Send, edit, or react on the provider; completes after the provider
    /// acknowledged.
    #[activity(name = ACTIVITY_CONNECTOR_DELIVER_MESSAGE)]
    pub async fn deliver_channel_message(
        _ctx: ActivityContext,
        _command: ChannelDeliveryCommand,
    ) -> Result<ChannelDeliveryResult, ActivityError> {
        unimplemented!("implemented by the connector host")
    }

    /// Download a provider attachment and put it in the core CAS.
    #[activity(name = ACTIVITY_CONNECTOR_PREPARE_MEDIA)]
    pub async fn prepare_channel_media(
        _ctx: ActivityContext,
        _input: PrepareChannelMediaInput,
    ) -> Result<PrepareChannelMediaResult, ActivityError> {
        unimplemented!("implemented by the connector host")
    }

    /// Keep the provider's typing indicator on until cancelled.
    #[activity(name = ACTIVITY_CONNECTOR_MAINTAIN_TYPING)]
    pub async fn maintain_channel_typing(
        _ctx: ActivityContext,
        _input: MaintainChannelTypingInput,
    ) -> Result<(), ActivityError> {
        unimplemented!("implemented by the connector host")
    }
}
