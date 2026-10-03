//! CoreAgent helpers for session graph operations.

use thiserror::Error;

use crate::{
    CodecError, CoreAgentCodec, CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent,
    CoreAgentState, ToolConfigEvent, UncommittedCoreAgentEvent, WorkflowToolConfigEvent,
    session::UncommittedStoredEvent,
};

#[derive(Debug, Error)]
pub enum CoreAgentCloneError {
    #[error("source session has no live config to clone")]
    MissingConfig,

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error("source session has incomplete managed workflow-tool creation state")]
    IncompleteManagedWorkflowTools,

    #[error("lifecycle-managed sessions cannot be cloned")]
    LifecycleManagedSession,
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
    if state.workflow_tools.lifecycle_controller.is_some() {
        return Err(CoreAgentCloneError::LifecycleManagedSession);
    }
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

    let has_non_system_bindings = state
        .workflow_tools
        .bindings
        .keys()
        .any(|tool_id| !state.workflow_tools.system_binding_ids.contains(tool_id));
    if let Some(session_universe_id) = state.workflow_tools.session_universe_id {
        let declaration_version = state
            .workflow_tools
            .managed_declaration_version
            .ok_or(CoreAgentCloneError::IncompleteManagedWorkflowTools)?;
        let creation_fingerprint = state
            .workflow_tools
            .managed_creation_fingerprint
            .clone()
            .ok_or(CoreAgentCloneError::IncompleteManagedWorkflowTools)?;
        events.push(
            codec.encode_uncommitted(&UncommittedCoreAgentEvent {
                observed_at_ms,
                joins: CoreAgentJoins::default(),
                event: CoreAgentEvent::WorkflowToolConfig(
                    WorkflowToolConfigEvent::ManagedBindingsAdmitted {
                        session_universe_id,
                        declaration_version,
                        lifecycle_controller: state.workflow_tools.lifecycle_controller.clone(),
                        creation_fingerprint,
                        bindings: state
                            .workflow_tools
                            .bindings
                            .iter()
                            .filter(|(tool_id, _)| {
                                !state.workflow_tools.system_binding_ids.contains(*tool_id)
                            })
                            .map(|(_, binding)| binding.clone())
                            .collect(),
                    },
                ),
            })?,
        );
    } else if state.workflow_tools.managed_declaration_version.is_some()
        || state.workflow_tools.managed_creation_fingerprint.is_some()
        || state.workflow_tools.lifecycle_controller.is_some()
        || has_non_system_bindings
    {
        return Err(CoreAgentCloneError::IncompleteManagedWorkflowTools);
    }

    let system_tool_names = state
        .workflow_tools
        .system_binding_ids
        .iter()
        .filter_map(|tool_id| state.workflow_tools.bindings.get(tool_id))
        .map(|binding| &binding.definition.tool.name)
        .collect::<std::collections::BTreeSet<_>>();
    let cloned_tools: std::collections::BTreeMap<_, _> = state
        .tooling
        .tools
        .iter()
        .filter(|(name, _)| !system_tool_names.contains(name))
        .map(|(name, tool)| (name.clone(), tool.clone()))
        .collect();
    if !cloned_tools.is_empty() {
        events.push(codec.encode_uncommitted(&UncommittedCoreAgentEvent {
            observed_at_ms,
            joins: CoreAgentJoins::default(),
            event: CoreAgentEvent::ToolConfig(ToolConfigEvent::ToolsReplaced {
                base_revision: 0,
                tools: cloned_tools,
            }),
        })?);
    }

    if let Some(environment_id) = &state.environment.active_environment_id {
        events.push(codec.encode_uncommitted(&UncommittedCoreAgentEvent {
            observed_at_ms,
            joins: CoreAgentJoins::default(),
            event: CoreAgentEvent::Environment(crate::EnvironmentEvent::ActiveEnvironmentSet {
                environment_id: environment_id.clone(),
            }),
        })?);
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContextConfig, ManagedSessionWorkflowTools, ModelSelection, ProviderApiKind, SessionConfig,
    };
    use uuid::Uuid;

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            generation: Default::default(),
            limits: Default::default(),
            context: ContextConfig {
                reported_input_limit_tokens: None,
                input_limit_tokens: None,
                compaction: None,
            },
            features: Default::default(),
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

    #[test]
    fn clone_opening_events_preserve_tool_only_workflow_creation_state() {
        let universe_id = Uuid::from_u128(7);
        let admitted = ManagedSessionWorkflowTools::v1(None, Vec::new())
            .admit(universe_id)
            .expect("managed declaration");
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(config());
        state.workflow_tools.session_universe_id = Some(admitted.session_universe_id);
        state.workflow_tools.managed_declaration_version = Some(admitted.version);
        state.workflow_tools.lifecycle_controller = admitted.lifecycle_controller;
        state.workflow_tools.managed_creation_fingerprint = Some(admitted.creation_fingerprint);

        let events = core_agent_clone_opening_events(&state, 10).expect("clone opening events");

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1].event.kind,
            "lightspeed.core.workflow_tool_config.managed_bindings_admitted"
        );
    }

    #[test]
    fn clone_opening_events_reject_lifecycle_managed_sessions() {
        let universe_id = Uuid::from_u128(7);
        let admitted = ManagedSessionWorkflowTools::v1(
            Some(crate::WorkflowEndpointRef {
                workflow_id: "controller/session-1".to_owned(),
                workflow_kind: "controller.workflow.v1".to_owned(),
            }),
            Vec::new(),
        )
        .admit(universe_id)
        .expect("managed declaration");
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(config());
        state.workflow_tools.session_universe_id = Some(admitted.session_universe_id);
        state.workflow_tools.managed_declaration_version = Some(admitted.version);
        state.workflow_tools.lifecycle_controller = admitted.lifecycle_controller;
        state.workflow_tools.managed_creation_fingerprint = Some(admitted.creation_fingerprint);

        let error = core_agent_clone_opening_events(&state, 10)
            .expect_err("lifecycle-managed session cannot be cloned");

        assert!(matches!(
            error,
            CoreAgentCloneError::LifecycleManagedSession
        ));
    }

    #[test]
    fn clone_opening_events_omit_system_workflow_tools() {
        let universe_id = Uuid::from_u128(7);
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(config());
        let tool = crate::WorkflowToolDefinition {
            tool_id: crate::WorkflowToolId::new("core-job-submit"),
            revision: 1,
            semantic_type: "lightspeed.environment.job.submit.v1".to_owned(),
            tool: crate::ToolSpec {
                name: crate::ToolName::new("job_submit"),
                execution: Default::default(),
                kind: crate::ToolKind::Function(crate::FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: crate::BlobRef::from_bytes(b"schema"),
                    output_schema_ref: None,
                    strict: Some(true),
                    provider_options_ref: None,
                }),
                parallelism: crate::ToolParallelism::ParallelSafe,
            },
        };
        let binding = crate::WorkflowToolBinding::admit(
            universe_id,
            tool.clone(),
            crate::WorkflowToolTarget::Start {
                start: crate::WorkflowStartRef {
                    recipe_format: 1,
                    revision: 1,
                    recipe_ref: crate::BlobRef::from_bytes(b"recipe"),
                    recipe_fingerprint: "recipe".to_owned(),
                },
            },
            crate::WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: None,
                max_promises: 1,
                key_source: crate::WorkflowToolCompletionKeySource::Reply,
            },
        )
        .expect("system binding");
        state
            .workflow_tools
            .system_binding_ids
            .insert(tool.tool_id.clone());
        state
            .workflow_tools
            .bindings
            .insert(tool.tool_id.clone(), binding);
        state
            .tooling
            .tools
            .insert(tool.tool.name.clone(), tool.tool);

        let events = core_agent_clone_opening_events(&state, 10).expect("clone opening events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.kind, "lightspeed.core.lifecycle.opened");
    }
}
