//! Activities of the `channels` worker role: the core-side set of the
//! conversation workflow. Every request carries its universe.

use std::sync::Arc;

use super::universes::WorkerUniverses;

use temporal_workflow::channels::*;
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::{gateway::GatewayAgentApi, universe::UniverseRuntime};

pub struct ChannelWorkerActivities {
    universes: WorkerUniverses,
}

impl ChannelWorkerActivities {
    pub fn for_universe(universe_id: uuid::Uuid, api: Arc<GatewayAgentApi>) -> Self {
        Self {
            universes: WorkerUniverses::Fixed { universe_id, api },
        }
    }

    pub fn with_runtime(runtime: Arc<UniverseRuntime>) -> Self {
        Self {
            universes: WorkerUniverses::Runtime(runtime),
        }
    }
}

#[activities]
impl ChannelWorkerActivities {
    #[activity(name = ACTIVITY_CHAT_TOOL_DECLARATIONS)]
    pub async fn chat_tool_declarations(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatToolDeclarationsRequest,
    ) -> Result<ChatToolDeclarationsResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::chat_tool_declarations(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_READ_JSON_BLOB)]
    pub async fn read_json_blob(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatReadJsonBlobRequest,
    ) -> Result<serde_json::Value, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::read_json_blob(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_PUT_JSON_BLOB)]
    pub async fn put_json_blob(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatPutJsonBlobRequest,
    ) -> Result<ChatPutJsonBlobResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::put_json_blob(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_RECONCILE_DELIVERY)]
    pub async fn reconcile_delivery(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatReconcileDeliveryRequest,
    ) -> Result<ChatReconcileDeliveryResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::reconcile_delivery(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_EMIT_EVENT)]
    pub async fn emit_chat_event(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatEmitEventRequest,
    ) -> Result<ChatEmitEventResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::emit_chat_event(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_STORE_SENT)]
    pub async fn store_chat_sent(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatStoreSentRequest,
    ) -> Result<ChatStoreSentResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::store_chat_sent(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_RESOLVE_HANDLE)]
    pub async fn resolve_chat_handle(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatResolveHandleRequest,
    ) -> Result<ChatResolveHandleResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::resolve_chat_handle(&api, request).await
    }

    #[activity(name = ACTIVITY_CHAT_ASSERT_TRIGGER_ACTIVE)]
    pub async fn assert_trigger_active(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ChatAssertTriggerActiveRequest,
    ) -> Result<ChatTriggerActiveResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        crate::channels::activities::assert_trigger_active(&api, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_names_match_workflow_definitions() {
        assert_eq!(
            ChannelWorkerActivities::emit_chat_event.name(),
            ChannelActivities::emit_chat_event.name()
        );
        assert_eq!(
            ChannelWorkerActivities::resolve_chat_handle.name(),
            ChannelActivities::resolve_chat_handle.name()
        );
    }
}
