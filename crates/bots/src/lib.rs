//! Bots domain crate: records, validation, store contracts, and the pure
//! pipeline logic of the bots subsystem.
//!
//! Wire DTOs live in `api`; this crate adds what the runtime needs around
//! them and nothing that does I/O. The Temporal workflows live in
//! `temporal-workflow`, the activities and service in `temporal-runtime`, the
//! tables in `store-pg`.
//!
//! Module map:
//! - [`ids`] — every identity derivation (session ids, event ids, workflow
//!   and schedule ids, delivery ids, submission ids, terminal tokens).
//! - [`records`] — bot, trigger, and event records plus the store traits.
//! - [`memory`] — in-memory stores for tests.
//! - [`signal`] — the controller's signal payloads and query names.
//! - [`validate`] — bot and trigger document validation.
//! - [`filter`] — CEL filter evaluation and route computation.
//! - [`webhook`] — webhook verification, header sanitization, presets.
//! - [`render`] — event prompt rendering and value pruning.
//! - [`poll`] — poll item extraction and cursor discipline.
//! - [`tools`] — the `bot_*` tool declarations and instruction composition.
//! - [`views`] — model-facing views (no ids the model must copy back).
#![recursion_limit = "256"]

pub mod filter;
pub mod ids;
pub mod memory;
pub mod poll;
pub mod records;
pub mod render;
pub mod signal;
pub mod tools;
pub mod validate;
pub mod views;
pub mod webhook;

use thiserror::Error;

pub use api::{
    BotActiveDeliverySnapshot, BotBreaker, BotBufferSnapshot, BotCoalescePolicy,
    BotControllerSnapshot, BotControllerStatus, BotDeliverPolicy, BotDocument, BotEventDocument,
    BotEventInput, BotEventMedia, BotEventMediaKind, BotEventOutcome, BotEventReplyRef,
    BotEventSender, BotEventView, BotId, BotIdError, BotInput, BotRecentDeliverySnapshot,
    BotRoutedSessionView, BotSessionKind, BotSessionSnapshot, BotSetupStatus,
    BotTriggerDisabledReason, BotTriggerDocument, BotTriggerId, BotTriggerInput, BotTriggerKind,
    BotTriggerRoute, BotTriggerSpec, BotTriggerView, BotView, BotWhenBusy, ChatAccess,
    ChatActivation, ChatGroupActivation, ChatPairing, ChatScope, ChatTurnAccess, PollCursorSpec,
    PollCursorState, PollHttpAuth, PollHttpMethod, PollSource, WebhookPreset, WebhookVerification,
    validate_bot_name,
};
pub use ids::*;
pub use memory::InMemoryBotStore;
pub use records::*;
pub use signal::*;

/// Errors of the bots registry and pipeline. `Refused` carries a stable
/// [`BotRefusalCode`] the model can read; everything else is operator-facing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BotError {
    #[error("bot already exists: {bot_id}")]
    BotAlreadyExists { bot_id: BotId },

    #[error("bot not found: {bot_id}")]
    BotNotFound { bot_id: BotId },

    #[error("bot revision conflict for {bot_id}: expected {expected}, actual {actual}")]
    BotRevisionConflict {
        bot_id: BotId,
        expected: u64,
        actual: u64,
    },

    #[error("bot trigger not found: {bot_id}/{trigger_id}")]
    TriggerNotFound {
        bot_id: BotId,
        trigger_id: BotTriggerId,
    },

    #[error(
        "bot trigger revision conflict for {bot_id}/{trigger_id}: expected {expected}, actual {actual}"
    )]
    TriggerRevisionConflict {
        bot_id: BotId,
        trigger_id: BotTriggerId,
        expected: u64,
        actual: u64,
    },

    #[error("bot event not found: {bot_id} #{seq}")]
    EventNotFound { bot_id: BotId, seq: u64 },

    #[error("bot event not found: {bot_id}/{event_id}")]
    EventIdNotFound { bot_id: BotId, event_id: String },

    #[error("bot is closed: {bot_id}")]
    BotClosed { bot_id: BotId },

    #[error("invalid bot input: {message}")]
    InvalidInput { message: String },

    /// A typed refusal the pipeline hands back to the caller (and, through
    /// `bot_emit`, to the model).
    #[error("{code}: {message}")]
    Refused {
        code: BotRefusalCode,
        message: String,
    },

    #[error("bot store failure: {message}")]
    Store { message: String },
}

impl BotError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    pub fn refused(code: BotRefusalCode, message: impl Into<String>) -> Self {
        Self::Refused {
            code,
            message: message.into(),
        }
    }

    pub fn store(message: impl Into<String>) -> Self {
        Self::Store {
            message: message.into(),
        }
    }
}

/// Stable refusal codes of the admission pipeline. The code is the
/// contract; the message is written for a model to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRefusalCode {
    UnknownBot,
    BotDisabled,
    BotClosed,
    TriggerDisabled,
    /// The addressed bot has no inbox trigger.
    NoInbox,
    /// The addressed bot's inbox does not accept this sender.
    NotAccepted,
    /// The trigger's CEL filter did not match.
    Filtered,
    /// The trigger's flood breaker tripped (and disabled it).
    BreakerTripped,
    /// The sending bot exceeded its emit rate.
    RateLimited,
    /// The federation hop bound would be exceeded.
    LoopCut,
}

impl BotRefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownBot => "unknown_bot",
            Self::BotDisabled => "bot_disabled",
            Self::BotClosed => "bot_closed",
            Self::TriggerDisabled => "trigger_disabled",
            Self::NoInbox => "no_inbox",
            Self::NotAccepted => "not_accepted",
            Self::Filtered => "filtered",
            Self::BreakerTripped => "breaker_tripped",
            Self::RateLimited => "rate_limited",
            Self::LoopCut => "loop_cut",
        }
    }
}

impl std::fmt::Display for BotRefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Default sender rate cap for emitting bots without a breaker of their own:
/// 60 emitted events per hour.
pub const DEFAULT_SENDER_RATE_FIRES: u32 = 60;
pub const DEFAULT_SENDER_RATE_WINDOW_MS: u64 = 60 * 60 * 1000;

/// Chat conversations coalesce like Channels always did: a short quiet
/// period, a bounded wait.
pub const CHAT_COALESCE_DEFAULT: BotCoalescePolicy = BotCoalescePolicy {
    debounce_ms: 400,
    max_wait_ms: 1_500,
    max_count: 8,
};

/// Alphabet of minted pairing codes: unambiguous alphanumerics.
pub const PAIRING_CODE_ALPHABET: &[u8] =
    b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
pub const PAIRING_CODE_LEN: usize = 12;

/// Mint a pairing code from caller-supplied random bytes (one byte per
/// character), so this crate stays free of a random source.
pub fn pairing_code_from_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(PAIRING_CODE_LEN)
        .map(|byte| PAIRING_CODE_ALPHABET[usize::from(*byte) % PAIRING_CODE_ALPHABET.len()] as char)
        .collect()
}
