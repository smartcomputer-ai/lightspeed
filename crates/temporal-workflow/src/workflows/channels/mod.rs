//! Channels workflows: one conversation workflow per chat, the
//! source of that conversation's bot events and the receiver of its
//! `message_*` tools. Runs in the `channels` worker role on its own task
//! queue; its core-side activities are implemented in `temporal-runtime`,
//! its connector-side activities by the TypeScript connector host on the
//! account's own task queue.

pub mod activities;
mod conversation;
pub mod types;

use std::time::Duration;

use temporalio_common::protos::temporal::api::common::v1::RetryPolicy;
use temporalio_sdk::{ActivityCloseTimeouts, ActivityOptions};

pub use activities::*;
pub use conversation::{ChannelConversationArgs, ChannelConversationWorkflow};
pub use types::*;

/// Default task queue of the `channels` worker role.
pub const DEFAULT_CHANNELS_TASK_QUEUE: &str = "lightspeed-channels";

/// Core-side activities: CAS, admission, control-plane reads.
pub const CHANNEL_ACTIVITY_START_TO_CLOSE: Duration = Duration::from_secs(30);
pub const CHANNEL_ACTIVITY_MAX_ATTEMPTS: i32 = 5;
/// Liveness gate: fail fast when the trigger stopped serving the chat.
pub const CHANNEL_ASSERT_ACTIVE_START_TO_CLOSE: Duration = Duration::from_secs(15);
/// Connector delivery: one provider send, retried within the deadline.
pub const CONNECTOR_DELIVERY_START_TO_CLOSE: Duration = Duration::from_secs(30);
pub const CONNECTOR_DELIVERY_SCHEDULE_TO_CLOSE: Duration = Duration::from_secs(110);
/// Connector media preparation: download and CAS upload.
pub const CONNECTOR_MEDIA_START_TO_CLOSE: Duration = Duration::from_secs(90);
pub const CONNECTOR_MEDIA_SCHEDULE_TO_CLOSE: Duration = Duration::from_secs(5 * 60);
/// Connector typing pulse: a long-running heartbeating activity cancelled
/// by scope.
pub const CONNECTOR_TYPING_START_TO_CLOSE: Duration = Duration::from_secs(24 * 60 * 60);
pub const CONNECTOR_TYPING_HEARTBEAT: Duration = Duration::from_secs(15);
/// Bound on queued inbound messages per conversation workflow.
pub const CHANNEL_INBOUND_INBOX_CAP: usize = 256;

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

pub fn channel_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::StartToClose(
        CHANNEL_ACTIVITY_START_TO_CLOSE,
    ))
    .retry_policy(retry(CHANNEL_ACTIVITY_MAX_ATTEMPTS))
    .build()
}

pub fn channel_assert_active_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::StartToClose(
        CHANNEL_ASSERT_ACTIVE_START_TO_CLOSE,
    ))
    .retry_policy(retry(3))
    .build()
}

/// Delivery on the connector's task queue.
pub fn connector_delivery_options(task_queue: impl Into<String>) -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
        start_to_close: CONNECTOR_DELIVERY_START_TO_CLOSE,
        schedule_to_close: CONNECTOR_DELIVERY_SCHEDULE_TO_CLOSE,
    })
    .task_queue(task_queue.into())
    .retry_policy(retry(5))
    .build()
}

pub fn connector_media_options(task_queue: impl Into<String>) -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
        start_to_close: CONNECTOR_MEDIA_START_TO_CLOSE,
        schedule_to_close: CONNECTOR_MEDIA_SCHEDULE_TO_CLOSE,
    })
    .task_queue(task_queue.into())
    .retry_policy(retry(3))
    .build()
}

pub fn connector_typing_options(task_queue: impl Into<String>) -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::StartToClose(
        CONNECTOR_TYPING_START_TO_CLOSE,
    ))
    .task_queue(task_queue.into())
    .heartbeat_timeout(CONNECTOR_TYPING_HEARTBEAT)
    .retry_policy(retry(1))
    .build()
}
