//! Bot workflows: the controller that owns a bot's controlled sessions
//! and the trigger fire started by Temporal Schedules. Both run in the
//! `bots` worker role on its own task queue; their activities are defined in
//! [`activities`] and implemented in `temporal-runtime`.

pub mod activities;
mod controller;
mod trigger_fire;
pub mod types;

use std::time::Duration;

use temporalio_common::protos::temporal::api::common::v1::RetryPolicy;
use temporalio_sdk::{ActivityCloseTimeouts, ActivityOptions};

pub use activities::*;
pub use controller::{
    BOT_CONTROLLER_WORKFLOW_KIND, BotControllerArgs, BotControllerCarry, BotControllerWorkflow,
    BotDelivery, CoalesceBuffer, ControlledSession, WaitingLane,
};
pub use trigger_fire::BotTriggerFireWorkflow;
pub use types::*;

/// Default task queue of the `bots` worker role.
pub const DEFAULT_BOTS_TASK_QUEUE: &str = "lightspeed-bots";

/// Ordinary bot activities: core calls, store writes, receipts.
pub const BOT_ACTIVITY_START_TO_CLOSE: Duration = Duration::from_secs(60);
pub const BOT_ACTIVITY_MAX_ATTEMPTS: i32 = 5;
/// The poll activity absorbs environment wake latency (`environmentNotReady`
/// retries) without badly overlapping the next fire; overlap policy `Skip`
/// drops collisions.
pub const BOT_POLL_START_TO_CLOSE: Duration = Duration::from_secs(240);
pub const BOT_POLL_MAX_ATTEMPTS: i32 = 6;
/// While a lane waits on a started run, how often the controller checks that the
/// lanes' sessions are still alive. A long run is normal, so a lane waits for as long
/// as its session lives, across continue-as-new, and only a dead session ends the wait.
pub const BOT_DEAD_SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Retry pause for a busy session, and the first step of the backoffs below.
pub const BOT_BUSY_RETRY_DELAY: Duration = Duration::from_secs(5);
/// A run start waits out the gateway's command wait, so a start held behind a
/// compaction answers `operationPending` instead of timing out the activity.
pub const BOT_START_RUN_START_TO_CLOSE: Duration = Duration::from_secs(120);
/// Start attempts a delivery gets before it ends `runFailed`. With the backoff
/// below that is about 20 minutes between attempts, held starts aside.
pub const BOT_START_MAX_ATTEMPTS: u32 = 10;
pub const BOT_START_RETRY_MAX_DELAY: Duration = Duration::from_secs(5 * 60);
/// Longest pause between attempts to bring a degraded main session back.
pub const BOT_RECONCILE_RETRY_MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// Descendant (sub-agent) budget refresh cadence while a run is in flight.
pub const BOT_DESCENDANT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Controller continue-as-new threshold, in processed events.
pub const BOT_CONTINUE_AS_NEW_AFTER_EVENTS: u64 = 100;
/// Cap on routed sessions a controller keeps open before evicting the idlest.
pub const BOT_EXTRA_SESSION_CAP: usize = 200;
/// Recent deliveries retained in the controller snapshot.
pub const BOT_RECENT_DELIVERY_CAP: usize = 50;
/// Dedupe sets carried across continue-as-new (tail-capped).
pub const BOT_SEEN_ID_CAP: usize = 2_000;

fn retry(max_attempts: i32) -> RetryPolicy {
    RetryPolicy {
        initial_interval: Some(
            Duration::from_secs(1)
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        backoff_coefficient: 2.0,
        maximum_interval: Some(
            Duration::from_secs(30)
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        maximum_attempts: max_attempts,
        non_retryable_error_types: Vec::new(),
    }
}

pub fn bot_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::StartToClose(
        BOT_ACTIVITY_START_TO_CLOSE,
    ))
    .cancellation_token(temporalio_sdk::WorkflowCancellationToken::new())
    .retry_policy(retry(BOT_ACTIVITY_MAX_ATTEMPTS))
    .build()
}

pub fn bot_start_run_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::StartToClose(
        BOT_START_RUN_START_TO_CLOSE,
    ))
    .cancellation_token(temporalio_sdk::WorkflowCancellationToken::new())
    .retry_policy(retry(BOT_ACTIVITY_MAX_ATTEMPTS))
    .build()
}

pub fn bot_poll_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::StartToClose(
        BOT_POLL_START_TO_CLOSE,
    ))
    .cancellation_token(temporalio_sdk::WorkflowCancellationToken::new())
    .retry_policy(retry(BOT_POLL_MAX_ATTEMPTS))
    .build()
}
