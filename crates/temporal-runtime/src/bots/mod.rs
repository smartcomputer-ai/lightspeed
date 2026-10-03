//! Bots runtime: the admission pipeline every event path shares,
//! Temporal Schedule reconciliation for schedule/poll triggers, the
//! controller-facing session operations, tool execution, receipts, and the
//! trigger fires. Everything here runs in-process against the universe's
//! `GatewayAgentApi` and `PgStore` — no JSON-RPC hop.
//!
//! - [`admission`] — store-then-wake: allocate `#N`, render, CAS, insert,
//!   signal-with-start the controller, compensate on wake failure.
//! - [`schedules`] — upsert / pause / delete the Temporal Schedule of a
//!   `schedule` or `poll` trigger; boot-time reconciliation.
//! - [`sessions`] — ensure / rename / status / start / steer / append /
//!   close / descendants, the controller's view of its managed sessions.
//! - [`tools`] — execution of the pushed `bot_*` tools.
//! - [`receipts`] — delivery receipts, `bot.reply` receipts, the directory.
//! - [`fires`] — schedule and poll trigger fires.
//! - [`hooks`] — the public webhook ingress behind the gateway route.

pub mod admission;
pub mod fires;
pub mod hooks;
pub mod receipts;
pub mod schedules;
pub mod sessions;
pub mod tools;

use api::AgentApiError;
use bots::{BotError, BotRefusalCode};

/// Map a bots-domain error onto the API error vocabulary.
pub fn map_bot_error(error: BotError) -> AgentApiError {
    match error {
        BotError::BotAlreadyExists { .. } => AgentApiError::conflict(error.to_string()),
        BotError::BotNotFound { .. }
        | BotError::TriggerNotFound { .. }
        | BotError::EventNotFound { .. }
        | BotError::EventIdNotFound { .. } => AgentApiError::not_found(error.to_string()),
        BotError::BotRevisionConflict { .. } | BotError::TriggerRevisionConflict { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        BotError::BotClosed { .. } => AgentApiError::rejected(error.to_string()),
        BotError::InvalidInput { message } => AgentApiError::invalid_request(message),
        BotError::Refused { code, message } => match code {
            BotRefusalCode::UnknownBot => AgentApiError::not_found(message),
            BotRefusalCode::Filtered => AgentApiError::rejected(format!("filtered: {message}")),
            _ => AgentApiError::rejected(format!("{code}: {message}")),
        },
        BotError::Store { message } => AgentApiError::internal(message),
    }
}

/// Current wall-clock milliseconds.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
