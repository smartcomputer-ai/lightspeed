//! CoreAgent helpers for session graph operations.

use thiserror::Error;

use crate::{
    CodecError, CoreAgentCodec, CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent,
    CoreAgentState, ToolConfigEvent, UncommittedCoreAgentEvent, session::UncommittedStoredEvent,
};

#[derive(Debug, Error)]
pub enum CoreAgentCloneError {
    #[error("source session has no live config to clone")]
    MissingConfig,

    #[error(transparent)]
    Codec(#[from] CodecError),
}

/// Materializes the source state needed to open a config-only clone.
///
/// `SessionStore::create_cloned_session` stays domain-neutral and persists the
/// stored events passed by the caller. CoreAgent hosts should replay the source
/// state, call this helper, then pass the returned events as the clone's
/// `opening_events`.
pub fn core_agent_clone_opening_events(
    state: &CoreAgentState,
    observed_at_ms: u64,
) -> Result<Vec<UncommittedStoredEvent>, CoreAgentCloneError> {
    let config = state
        .lifecycle
        .config
        .clone()
        .ok_or(CoreAgentCloneError::MissingConfig)?;
    let codec = CoreAgentCodec;
    let mut events = vec![codec.encode_uncommitted(&UncommittedCoreAgentEvent {
        observed_at_ms,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
    })?];

    if !state.tooling.tools.is_empty() {
        events.push(codec.encode_uncommitted(&UncommittedCoreAgentEvent {
            observed_at_ms,
            joins: CoreAgentJoins::default(),
            event: CoreAgentEvent::ToolConfig(ToolConfigEvent::ToolsReplaced {
                base_revision: 0,
                tools: state.tooling.tools.clone(),
            }),
        })?);
    }

    for target in state.tooling.routing.default_targets.values() {
        events.push(codec.encode_uncommitted(&UncommittedCoreAgentEvent {
            observed_at_ms,
            joins: CoreAgentJoins::default(),
            event: CoreAgentEvent::ToolConfig(ToolConfigEvent::DefaultTargetSet {
                target: target.clone(),
            }),
        })?);
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContextConfig, ModelSelection, ProviderApiKind, RunConfig, SessionConfig, ToolConfig,
        TurnConfig,
    };

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            run: RunConfig::default(),
            turn: TurnConfig {
                max_output_tokens: None,
                tool_choice: None,
                provider_params: None,
            },
            context: ContextConfig { compaction: None },
            tools: ToolConfig::default(),
            fleet: Default::default(),
        }
    }

    #[test]
    fn clone_opening_events_require_live_config() {
        let error = core_agent_clone_opening_events(&CoreAgentState::new(), 10)
            .expect_err("missing config fails");
        assert!(matches!(error, CoreAgentCloneError::MissingConfig));
    }

    #[test]
    fn clone_opening_events_materialize_opened_config() {
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(config());
        let events = core_agent_clone_opening_events(&state, 10).expect("clone opening events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.kind, "lightspeed.core.lifecycle.opened");
        assert_eq!(events[0].observed_at_ms, 10);
    }
}
