//! Channels runtime: the control plane behind `channels/inbound/admit`
//! (which chat trigger serves a conversation, pairing), the conversation
//! workflow's core-side activities, and the mapping onto bots admission.
//!
//! - [`control_plane`] — chat trigger candidates, pairing decisions, and
//!   the signal-with-start of the conversation workflow.
//! - [`activities`] — the `ChannelActivities` implementations.

pub mod activities;
pub mod control_plane;

use api::AgentApiError;
use channels::ChannelError;

pub fn map_channel_error(error: ChannelError) -> AgentApiError {
    crate::gateway::service::channels_api::map_channel_error(error)
}
