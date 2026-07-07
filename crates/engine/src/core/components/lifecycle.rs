use serde::{Deserialize, Serialize};

use crate::{CoreAgentState, DomainError, SessionConfig};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Opened {
        config: SessionConfig,
    },
    ConfigChanged {
        config: SessionConfig,
        revision: u64,
    },
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    New,
    Open,
    Closed,
}

pub type CoreAgentLifecycleEvent = Event;
pub type CoreAgentStatus = Status;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleState {
    pub status: Status,
    pub config: Option<SessionConfig>,
    pub config_revision: u64,
}

impl Default for LifecycleState {
    fn default() -> Self {
        Self {
            status: Status::New,
            config: None,
            config_revision: 0,
        }
    }
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::Opened { config } => {
            if state.lifecycle.status != Status::New {
                return Err(DomainError::InvariantViolation(
                    "session can only be opened from new state".into(),
                ));
            }
            config.validate_provider_compatibility()?;
            state.lifecycle.status = Status::Open;
            state.lifecycle.config = Some(config.clone());
            state.lifecycle.config_revision = 0;
            Ok(())
        }
        Event::ConfigChanged { config, revision } => {
            if state.lifecycle.status != Status::Open {
                return Err(DomainError::InvariantViolation(
                    "session config can only change while open".into(),
                ));
            }
            let expected = state
                .lifecycle
                .config_revision
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::InvariantViolation("config revision exhausted".into())
                })?;
            if *revision != expected {
                return Err(DomainError::InvariantViolation(format!(
                    "expected config revision {}, got {}",
                    expected, revision
                )));
            }
            crate::core::components::config::validate_config_update_for_state(state, config)?;
            state.lifecycle.config = Some(config.clone());
            state.lifecycle.config_revision = *revision;
            Ok(())
        }
        Event::Closed => {
            if state.lifecycle.status != Status::Open {
                return Err(DomainError::InvariantViolation(
                    "only open sessions can be closed".into(),
                ));
            }
            if state.runs.active.is_some()
                || !state.runs.queued.is_empty()
                || state
                    .promises
                    .pending()
                    .any(|promise| matches!(promise.scope, crate::PromiseScope::Session))
            {
                return Err(DomainError::InvariantViolation(
                    "session cannot close with active work".into(),
                ));
            }
            state.lifecycle.status = Status::Closed;
            Ok(())
        }
    }
}
