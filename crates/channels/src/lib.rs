//! Channels domain crate: provider accounts, pairings, the inbound and
//! delivery shapes shared with the connector host, and the pure policy of
//! a conversation hosted in the core runtime.
//!
//! A chat connection is a bot trigger of kind `chat`; this crate holds
//! what that trigger points at and the conversation-side logic. The
//! conversation workflow lives in `temporal-workflow`, its activities and
//! the control plane in `temporal-runtime`, the tables in `store-pg`, and
//! the provider bridges (Telegram, WhatsApp) in `platform/connectors`.
//!
//! Module map:
//! - [`records`] — account and pairing records and their store traits.
//! - [`memory`] — in-memory stores for tests.
//! - [`ids`] — conversation keys, workflow ids, delivery task queues,
//!   pairing keys.
//! - [`inbound`] — the normalized inbound envelope and admission input.
//! - [`policy`] — activation, access, and control commands.
//! - [`delivery`] — delivery commands, tool operations, chunked plans.
//! - [`media`] — media validation and the media/typing activity payloads.
//! - [`state`] — conversation workflow state and compaction.
//! - [`tools`] — the `message_*` tool declarations.

pub mod delivery;
pub mod ids;
pub mod inbound;
pub mod media;
pub mod memory;
pub mod policy;
pub mod records;
pub mod state;
pub mod tools;

use thiserror::Error;

pub use api::{
    ChannelAccountDocument, ChannelAccountId, ChannelAccountInput, ChannelAccountSettings,
    ChannelAccountView, ChannelInbound, ChannelInboundDecision, ChannelInboundMedia,
    ChannelMediaKind, ChannelPairedVia, ChannelPairingView, ChannelProvider,
};
pub use ids::*;
pub use memory::InMemoryChannelStore;
pub use records::*;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChannelError {
    #[error("channel account already exists: {account_id}")]
    AccountAlreadyExists { account_id: ChannelAccountId },

    #[error("channel account not found: {account_id}")]
    AccountNotFound { account_id: ChannelAccountId },

    #[error(
        "channel account revision conflict for {account_id}: expected {expected}, actual {actual}"
    )]
    AccountRevisionConflict {
        account_id: ChannelAccountId,
        expected: u64,
        actual: u64,
    },

    #[error("channel pairing not found: {account_id}/{chat_id}")]
    PairingNotFound { account_id: String, chat_id: String },

    #[error("invalid channel input: {message}")]
    InvalidInput { message: String },

    #[error("channel store failure: {message}")]
    Store { message: String },
}

impl ChannelError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    pub fn store(message: impl Into<String>) -> Self {
        Self::Store {
            message: message.into(),
        }
    }
}
