//! Activity definitions of the `bots` worker role. Definition only: the
//! implementations live in `temporal-runtime::worker::bots` and must keep
//! these names (a test there asserts they match).

use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use super::types::*;

pub const ACTIVITY_BOT_ENSURE_SESSION: &str = "BotActivities::ensure_session";
pub const ACTIVITY_BOT_RENAME_SESSION: &str = "BotActivities::rename_session";
pub const ACTIVITY_BOT_READ_SESSION_STATUS: &str = "BotActivities::read_session_status";
pub const ACTIVITY_BOT_READ_RUN_USAGE: &str = "BotActivities::read_run_usage";
pub const ACTIVITY_BOT_START_RUN: &str = "BotActivities::start_run";
pub const ACTIVITY_BOT_STEER_RUN: &str = "BotActivities::steer_run";
pub const ACTIVITY_BOT_APPEND_CONTEXT: &str = "BotActivities::append_context";
pub const ACTIVITY_BOT_CLOSE_SESSION: &str = "BotActivities::close_session";
pub const ACTIVITY_BOT_COUNT_DESCENDANTS: &str = "BotActivities::count_descendants";
pub const ACTIVITY_BOT_READ_TOOL_INVOCATIONS: &str = "BotActivities::read_tool_invocations";
pub const ACTIVITY_BOT_READ_JSON_BLOB: &str = "BotActivities::read_json_blob";
pub const ACTIVITY_BOT_EXECUTE_TOOL: &str = "BotActivities::execute_tool";
pub const ACTIVITY_BOT_RECORD_OUTCOMES: &str = "BotActivities::record_outcomes";
pub const ACTIVITY_BOT_RECORD_CLOSED: &str = "BotActivities::record_closed";
pub const ACTIVITY_BOT_SEND_DELIVERY_RECEIPTS: &str = "BotActivities::send_delivery_receipts";
pub const ACTIVITY_BOT_SEND_BOT_RECEIPTS: &str = "BotActivities::send_bot_receipts";
pub const ACTIVITY_BOT_PUBLISH_DIRECTORY: &str = "BotActivities::publish_directory";
pub const ACTIVITY_BOT_ADMIT_SCHEDULE_EVENT: &str = "BotActivities::admit_schedule_event";
pub const ACTIVITY_BOT_POLL_TRIGGER: &str = "BotActivities::poll_trigger";

pub struct BotActivities;

#[activities]
impl BotActivities {
    /// Create or reconcile a managed session of the bot (profile, brief,
    /// `bot_*` tools, carried tools, lifecycle controller).
    #[activity(name = ACTIVITY_BOT_ENSURE_SESSION)]
    pub async fn ensure_session(
        _ctx: ActivityContext,
        _request: BotEnsureSessionRequest,
    ) -> Result<BotEnsureSessionResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_RENAME_SESSION)]
    pub async fn rename_session(
        _ctx: ActivityContext,
        _request: BotRenameSessionRequest,
    ) -> Result<(), ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_READ_SESSION_STATUS)]
    pub async fn read_session_status(
        _ctx: ActivityContext,
        _request: BotSessionRequest,
    ) -> Result<BotSessionStatus, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_READ_RUN_USAGE)]
    pub async fn read_run_usage(
        _ctx: ActivityContext,
        _request: BotReadRunUsageRequest,
    ) -> Result<BotReadRunUsageResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Start the delivery's run with a deterministic submission id and a
    /// terminal notify token addressed to the controller.
    #[activity(name = ACTIVITY_BOT_START_RUN)]
    pub async fn start_run(
        _ctx: ActivityContext,
        _request: BotStartRunRequest,
    ) -> Result<BotStartRunResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_STEER_RUN)]
    pub async fn steer_run(
        _ctx: ActivityContext,
        _request: BotSteerRunRequest,
    ) -> Result<BotSteerRunResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_APPEND_CONTEXT)]
    pub async fn append_context(
        _ctx: ActivityContext,
        _request: BotAppendContextRequest,
    ) -> Result<(), ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Close a session and its sub-agent descendants.
    #[activity(name = ACTIVITY_BOT_CLOSE_SESSION)]
    pub async fn close_session(
        _ctx: ActivityContext,
        _request: BotCloseSessionRequest,
    ) -> Result<BotCloseSessionResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_COUNT_DESCENDANTS)]
    pub async fn count_descendants(
        _ctx: ActivityContext,
        _request: BotCountDescendantsRequest,
    ) -> Result<BotCountDescendantsResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Read workflow-tool invocations from the session log after a cursor
    /// (the controller pulls `bot_event_resolve` from here).
    #[activity(name = ACTIVITY_BOT_READ_TOOL_INVOCATIONS)]
    pub async fn read_tool_invocations(
        _ctx: ActivityContext,
        _request: BotReadToolInvocationsRequest,
    ) -> Result<BotReadToolInvocationsResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_READ_JSON_BLOB)]
    pub async fn read_json_blob(
        _ctx: ActivityContext,
        _request: BotReadJsonBlobRequest,
    ) -> Result<serde_json::Value, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Execute one pushed `bot_*` tool and store its result document.
    #[activity(name = ACTIVITY_BOT_EXECUTE_TOOL)]
    pub async fn execute_tool(
        _ctx: ActivityContext,
        _request: BotExecuteToolRequest,
    ) -> Result<BotExecuteToolResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Write-once outcome onto every event row of a delivery.
    #[activity(name = ACTIVITY_BOT_RECORD_OUTCOMES)]
    pub async fn record_outcomes(
        _ctx: ActivityContext,
        _request: BotRecordOutcomesRequest,
    ) -> Result<BotRecordOutcomesResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_RECORD_CLOSED)]
    pub async fn record_closed(
        _ctx: ActivityContext,
        _request: BotRecordClosedRequest,
    ) -> Result<BotRecordClosedResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_SEND_DELIVERY_RECEIPTS)]
    pub async fn send_delivery_receipts(
        _ctx: ActivityContext,
        _request: BotSendDeliveryReceiptsRequest,
    ) -> Result<BotReceiptsSent, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_SEND_BOT_RECEIPTS)]
    pub async fn send_bot_receipts(
        _ctx: ActivityContext,
        _request: BotSendBotReceiptsRequest,
    ) -> Result<BotReceiptsSent, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Put the `bot:directory` catalog into the session before a delivery
    /// (same content is an harness no-op, so the prefix cache holds).
    #[activity(name = ACTIVITY_BOT_PUBLISH_DIRECTORY)]
    pub async fn publish_directory(
        _ctx: ActivityContext,
        _request: BotPublishDirectoryRequest,
    ) -> Result<BotPublishDirectoryResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_ADMIT_SCHEDULE_EVENT)]
    pub async fn admit_schedule_event(
        _ctx: ActivityContext,
        _request: BotTriggerFireRequest,
    ) -> Result<BotScheduleFireResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_BOT_POLL_TRIGGER)]
    pub async fn poll_trigger(
        _ctx: ActivityContext,
        _request: BotTriggerFireRequest,
    ) -> Result<BotPollFireResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }
}
