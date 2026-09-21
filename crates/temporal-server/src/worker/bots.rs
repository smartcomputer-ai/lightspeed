//! Activities of the `bots` worker role. Every activity resolves its
//! universe explicitly from the request (bot workflow ids are
//! `{universe}/bot-…`, but the request carries the universe too) and runs
//! against that universe's in-process service.

use std::sync::Arc;

use super::universes::WorkerUniverses;

use temporal_workflow::bots::*;
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::{gateway::GatewayAgentApi, universe::UniverseRuntime};

fn authority_error(error: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(temporalio_common::error::ApplicationFailure::non_retryable(
        anyhow::anyhow!("{error}"),
    ))
}

pub struct BotWorkerActivities {
    universes: WorkerUniverses,
}

impl BotWorkerActivities {
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
impl BotWorkerActivities {
    #[activity(name = ACTIVITY_BOT_ENSURE_SESSION)]
    pub async fn ensure_session(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotEnsureSessionRequest,
    ) -> Result<BotEnsureSessionResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::ensure_session(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_RENAME_SESSION)]
    pub async fn rename_session(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotRenameSessionRequest,
    ) -> Result<(), ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::rename_session(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_READ_SESSION_STATUS)]
    pub async fn read_session_status(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotSessionRequest,
    ) -> Result<BotSessionStatus, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::read_session_status(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_READ_RUN_USAGE)]
    pub async fn read_run_usage(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotReadRunUsageRequest,
    ) -> Result<BotReadRunUsageResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::read_run_usage(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_START_RUN)]
    pub async fn start_run(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotStartRunRequest,
    ) -> Result<BotStartRunResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::start_run(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_STEER_RUN)]
    pub async fn steer_run(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotSteerRunRequest,
    ) -> Result<BotSteerRunResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::steer_run(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_APPEND_CONTEXT)]
    pub async fn append_context(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotAppendContextRequest,
    ) -> Result<(), ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::append_context(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_CLOSE_SESSION)]
    pub async fn close_session(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotCloseSessionRequest,
    ) -> Result<BotCloseSessionResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::close_session(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_COUNT_DESCENDANTS)]
    pub async fn count_descendants(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotCountDescendantsRequest,
    ) -> Result<BotCountDescendantsResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::count_descendants(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_READ_TOOL_INVOCATIONS)]
    pub async fn read_tool_invocations(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotReadToolInvocationsRequest,
    ) -> Result<BotReadToolInvocationsResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::read_tool_invocations(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_READ_JSON_BLOB)]
    pub async fn read_json_blob(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotReadJsonBlobRequest,
    ) -> Result<serde_json::Value, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, None)
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::sessions::read_json_blob(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_EXECUTE_TOOL)]
    pub async fn execute_tool(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotExecuteToolRequest,
    ) -> Result<BotExecuteToolResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::tools::execute_tool(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_RECORD_OUTCOMES)]
    pub async fn record_outcomes(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotRecordOutcomesRequest,
    ) -> Result<BotRecordOutcomesResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::receipts::record_outcomes(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_RECORD_CLOSED)]
    pub async fn record_closed(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotRecordClosedRequest,
    ) -> Result<BotRecordClosedResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::receipts::record_closed(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_SEND_DELIVERY_RECEIPTS)]
    pub async fn send_delivery_receipts(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotSendDeliveryReceiptsRequest,
    ) -> Result<BotReceiptsSent, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::receipts::send_delivery_receipts(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_SEND_BOT_RECEIPTS)]
    pub async fn send_bot_receipts(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotSendBotReceiptsRequest,
    ) -> Result<BotReceiptsSent, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::receipts::send_bot_receipts(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_PUBLISH_DIRECTORY)]
    pub async fn publish_directory(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotPublishDirectoryRequest,
    ) -> Result<BotPublishDirectoryResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::receipts::publish_directory(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_ADMIT_SCHEDULE_EVENT)]
    pub async fn admit_schedule_event(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotTriggerFireRequest,
    ) -> Result<BotScheduleFireResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::fires::admit_schedule_event(&api, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_BOT_POLL_TRIGGER)]
    pub async fn poll_trigger(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: BotTriggerFireRequest,
    ) -> Result<BotPollFireResult, ActivityError> {
        let api = self.universes.api_for(request.universe_id).await?;
        let workflow_id = &_ctx
            .info()
            .workflow_execution
            .as_ref()
            .ok_or_else(|| authority_error("missing controller workflow identity"))?
            .workflow_id;
        let authority = api
            .bot_activity_authority(workflow_id, Some(&request.bot_id))
            .await
            .map_err(authority_error)?;
        crate::gateway::service::authorization::with_controller_authority(
            authority,
            crate::bots::fires::poll_trigger(&api, request),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_names_match_workflow_definitions() {
        assert_eq!(
            BotWorkerActivities::ensure_session.name(),
            BotActivities::ensure_session.name()
        );
        assert_eq!(
            BotWorkerActivities::start_run.name(),
            BotActivities::start_run.name()
        );
        assert_eq!(
            BotWorkerActivities::execute_tool.name(),
            BotActivities::execute_tool.name()
        );
        assert_eq!(
            BotWorkerActivities::poll_trigger.name(),
            BotActivities::poll_trigger.name()
        );
        assert_eq!(
            BotWorkerActivities::send_delivery_receipts.name(),
            BotActivities::send_delivery_receipts.name()
        );
    }
}
