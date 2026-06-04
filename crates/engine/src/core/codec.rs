use crate::{
    CodecError, CommandCodec, ContextEvent, CoreAgentCommand, CoreAgentEntry, CoreAgentEvent,
    CoreAgentEventKind, CoreAgentJoins, CoreAgentLifecycleEvent, CorrelationId, DynamicEvent,
    EventCodec, JoinsCodec, RunEvent, RunId, SkillEvent, SubmissionId, ToolBatchId, ToolCallId,
    ToolConfigEvent, ToolEvent, TurnEvent, TurnId, UncommittedCoreAgentEvent,
    session::{DynamicJoins, DynamicSessionEntry, DynamicUncommittedSessionEvent},
};

const CORE_AGENT_COMMAND_KIND: &str = "forge.core.command";
const CORE_AGENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreAgentCodec;

impl CommandCodec for CoreAgentCodec {
    type Command = CoreAgentCommand;

    fn encode_command(&self, command: &Self::Command) -> Result<crate::DynamicCommand, CodecError> {
        Ok(crate::DynamicCommand::new(
            CORE_AGENT_COMMAND_KIND,
            CORE_AGENT_SCHEMA_VERSION,
            serde_json::to_value(command).map_err(codec_failure)?,
        ))
    }

    fn decode_command(&self, command: &crate::DynamicCommand) -> Result<Self::Command, CodecError> {
        ensure_envelope(
            &command.kind,
            command.version,
            CORE_AGENT_COMMAND_KIND,
            CORE_AGENT_SCHEMA_VERSION,
        )?;
        serde_json::from_value(command.payload.clone()).map_err(codec_failure)
    }
}

impl EventCodec for CoreAgentCodec {
    type Event = CoreAgentEvent;

    fn encode_event(&self, event: &Self::Event) -> Result<DynamicEvent, CodecError> {
        Ok(DynamicEvent::new(
            core_agent_event_envelope_kind(event),
            CORE_AGENT_SCHEMA_VERSION,
            serde_json::to_value(event).map_err(codec_failure)?,
        ))
    }

    fn decode_event(&self, event: &DynamicEvent) -> Result<Self::Event, CodecError> {
        ensure_core_agent_event_envelope(&event.kind, event.version)?;
        serde_json::from_value(event.payload.clone()).map_err(codec_failure)
    }
}

impl JoinsCodec for CoreAgentCodec {
    type Joins = CoreAgentJoins;

    fn encode_joins(&self, joins: &Self::Joins) -> DynamicJoins {
        CoreAgentCodec::encode_joins(self, joins)
    }

    fn decode_joins(&self, joins: &DynamicJoins) -> Result<Self::Joins, CodecError> {
        CoreAgentCodec::decode_joins(self, joins)
    }
}

impl CoreAgentCodec {
    pub fn encode_joins(&self, joins: &CoreAgentJoins) -> DynamicJoins {
        let mut encoded = DynamicJoins::new();
        insert_numeric(&mut encoded, "run_id", joins.run_id.map(RunId::as_u64));
        insert_numeric(&mut encoded, "turn_id", joins.turn_id.map(TurnId::as_u64));
        insert_numeric(
            &mut encoded,
            "tool_batch_id",
            joins.tool_batch_id.map(ToolBatchId::as_u64),
        );
        insert_string(&mut encoded, "tool_call_id", joins.tool_call_id.as_ref());
        insert_string(&mut encoded, "submission_id", joins.submission_id.as_ref());
        insert_string(
            &mut encoded,
            "correlation_id",
            joins.correlation_id.as_ref(),
        );
        encoded
    }

    pub fn decode_joins(&self, joins: &DynamicJoins) -> Result<CoreAgentJoins, CodecError> {
        Ok(CoreAgentJoins {
            run_id: parse_numeric(joins, "run_id")?.map(RunId::new),
            turn_id: parse_numeric(joins, "turn_id")?.map(TurnId::new),
            tool_batch_id: parse_numeric(joins, "tool_batch_id")?.map(ToolBatchId::new),
            tool_call_id: parse_string_id(joins, "tool_call_id", ToolCallId::try_new)?,
            submission_id: parse_string_id(joins, "submission_id", SubmissionId::try_new)?,
            correlation_id: parse_string_id(joins, "correlation_id", CorrelationId::try_new)?,
        })
    }

    pub fn encode_entry(&self, entry: &CoreAgentEntry) -> Result<DynamicSessionEntry, CodecError> {
        Ok(DynamicSessionEntry {
            position: entry.position.clone(),
            observed_at_ms: entry.observed_at_ms,
            joins: self.encode_joins(&entry.joins),
            event: self.encode_event(&entry.event)?,
        })
    }

    pub fn decode_entry(&self, entry: &DynamicSessionEntry) -> Result<CoreAgentEntry, CodecError> {
        Ok(CoreAgentEntry {
            position: entry.position.clone(),
            observed_at_ms: entry.observed_at_ms,
            joins: self.decode_joins(&entry.joins)?,
            event: self.decode_event(&entry.event)?,
        })
    }

    pub fn encode_uncommitted(
        &self,
        event: &UncommittedCoreAgentEvent,
    ) -> Result<DynamicUncommittedSessionEvent, CodecError> {
        Ok(DynamicUncommittedSessionEvent {
            observed_at_ms: event.observed_at_ms,
            joins: self.encode_joins(&event.joins),
            event: self.encode_event(&event.event)?,
        })
    }
}

fn ensure_envelope(
    actual_kind: &str,
    actual_version: u32,
    expected_kind: &str,
    expected_version: u32,
) -> Result<(), CodecError> {
    if actual_kind == expected_kind && actual_version == expected_version {
        Ok(())
    } else {
        Err(CodecError::Unsupported {
            kind: actual_kind.to_owned(),
            version: actual_version,
        })
    }
}

fn ensure_core_agent_event_envelope(kind: &str, version: u32) -> Result<(), CodecError> {
    if version == CORE_AGENT_SCHEMA_VERSION && is_core_agent_event_envelope_kind(kind) {
        Ok(())
    } else {
        Err(CodecError::Unsupported {
            kind: kind.to_owned(),
            version,
        })
    }
}

fn is_core_agent_event_envelope_kind(kind: &str) -> bool {
    matches!(
        kind,
        "forge.core.lifecycle.opened"
            | "forge.core.lifecycle.config_changed"
            | "forge.core.lifecycle.closed"
            | "forge.core.run.accepted"
            | "forge.core.run.started"
            | "forge.core.run.steering_accepted"
            | "forge.core.run.cancellation_requested"
            | "forge.core.run.completed"
            | "forge.core.run.failed"
            | "forge.core.run.cancelled"
            | "forge.core.turn.started"
            | "forge.core.turn.planned"
            | "forge.core.turn.generation_requested"
            | "forge.core.turn.generation_completed"
            | "forge.core.turn.completed"
            | "forge.core.context.entries_applied"
            | "forge.core.context.entries_removed"
            | "forge.core.context.keys_removed"
            | "forge.core.context.state_replaced"
            | "forge.core.skill.catalog_set"
            | "forge.core.skill.activations_set"
            | "forge.core.tool_config.registry_changed"
            | "forge.core.tool_config.profile_selected"
            | "forge.core.tool_config.default_target_set"
            | "forge.core.tool_config.default_target_cleared"
            | "forge.core.tool.batch_started"
            | "forge.core.tool.call_started"
            | "forge.core.tool.call_completed"
            | "forge.core.tool.batch_completed"
    )
}

fn core_agent_event_envelope_kind(event: &CoreAgentEvent) -> &'static str {
    match &event.kind {
        CoreAgentEventKind::Lifecycle(event) => match event {
            CoreAgentLifecycleEvent::Opened { .. } => "forge.core.lifecycle.opened",
            CoreAgentLifecycleEvent::ConfigChanged { .. } => "forge.core.lifecycle.config_changed",
            CoreAgentLifecycleEvent::Closed => "forge.core.lifecycle.closed",
        },
        CoreAgentEventKind::Run(event) => match event {
            RunEvent::Accepted { .. } => "forge.core.run.accepted",
            RunEvent::Started { .. } => "forge.core.run.started",
            RunEvent::SteeringAccepted { .. } => "forge.core.run.steering_accepted",
            RunEvent::CancellationRequested { .. } => "forge.core.run.cancellation_requested",
            RunEvent::Completed { .. } => "forge.core.run.completed",
            RunEvent::Failed { .. } => "forge.core.run.failed",
            RunEvent::Cancelled { .. } => "forge.core.run.cancelled",
        },
        CoreAgentEventKind::Turn(event) => match event {
            TurnEvent::Started { .. } => "forge.core.turn.started",
            TurnEvent::Planned { .. } => "forge.core.turn.planned",
            TurnEvent::GenerationRequested { .. } => "forge.core.turn.generation_requested",
            TurnEvent::GenerationCompleted { .. } => "forge.core.turn.generation_completed",
            TurnEvent::Completed { .. } => "forge.core.turn.completed",
        },
        CoreAgentEventKind::Context(event) => match event {
            ContextEvent::EntriesApplied { .. } => "forge.core.context.entries_applied",
            ContextEvent::EntriesRemoved { .. } => "forge.core.context.entries_removed",
            ContextEvent::KeysRemoved { .. } => "forge.core.context.keys_removed",
            ContextEvent::StateReplaced { .. } => "forge.core.context.state_replaced",
        },
        CoreAgentEventKind::Skill(event) => match event {
            SkillEvent::CatalogSet { .. } => "forge.core.skill.catalog_set",
            SkillEvent::ActivationsSet { .. } => "forge.core.skill.activations_set",
        },
        CoreAgentEventKind::ToolConfig(event) => match event {
            ToolConfigEvent::RegistryChanged { .. } => "forge.core.tool_config.registry_changed",
            ToolConfigEvent::ProfileSelected { .. } => "forge.core.tool_config.profile_selected",
            ToolConfigEvent::DefaultTargetSet { .. } => "forge.core.tool_config.default_target_set",
            ToolConfigEvent::DefaultTargetCleared { .. } => {
                "forge.core.tool_config.default_target_cleared"
            }
        },
        CoreAgentEventKind::Tool(event) => match event {
            ToolEvent::BatchStarted { .. } => "forge.core.tool.batch_started",
            ToolEvent::CallStarted { .. } => "forge.core.tool.call_started",
            ToolEvent::CallCompleted { .. } => "forge.core.tool.call_completed",
            ToolEvent::BatchCompleted { .. } => "forge.core.tool.batch_completed",
        },
    }
}

fn insert_numeric(joins: &mut DynamicJoins, key: &'static str, value: Option<u64>) {
    if let Some(value) = value {
        joins.insert(key.to_owned(), value.to_string());
    }
}

fn insert_string<T: ToString>(joins: &mut DynamicJoins, key: &'static str, value: Option<&T>) {
    if let Some(value) = value {
        joins.insert(key.to_owned(), value.to_string());
    }
}

fn parse_numeric(joins: &DynamicJoins, key: &'static str) -> Result<Option<u64>, CodecError> {
    joins
        .get(key)
        .map(|value: &String| {
            value.parse::<u64>().map_err(|error| CodecError::Failed {
                message: format!("invalid dynamic join {key}: {error}"),
            })
        })
        .transpose()
}

fn parse_string_id<T, F>(
    joins: &DynamicJoins,
    key: &'static str,
    parse: F,
) -> Result<Option<T>, CodecError>
where
    F: FnOnce(String) -> Result<T, crate::StringIdError>,
{
    joins
        .get(key)
        .cloned()
        .map(|value| {
            parse(value).map_err(|error| CodecError::Failed {
                message: format!("invalid dynamic join {key}: {error}"),
            })
        })
        .transpose()
}

fn codec_failure(error: impl std::fmt::Display) -> CodecError {
    CodecError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        CoreAgentCommand, CoreAgentEventKind, CoreAgentJoins, CoreAgentLifecycleEvent,
        CorrelationId, EventSeq, RunId, SessionPosition, ToolBatchId, ToolEvent, TurnId,
    };

    use super::*;

    #[test]
    fn core_agent_event_round_trips_through_dynamic_envelope() {
        let codec = CoreAgentCodec;
        let event = CoreAgentEvent {
            kind: CoreAgentEventKind::Lifecycle(CoreAgentLifecycleEvent::Closed),
        };

        let encoded = codec.encode_event(&event).expect("encode event");
        assert_eq!(encoded.kind, "forge.core.lifecycle.closed");
        assert_eq!(encoded.version, CORE_AGENT_SCHEMA_VERSION);

        assert_eq!(codec.decode_event(&encoded).expect("decode event"), event);
    }

    #[test]
    fn old_core_agent_dynamic_envelope_names_are_unsupported() {
        let codec = CoreAgentCodec;
        let old_command = crate::DynamicCommand::new(
            "forge.core_agent.command",
            CORE_AGENT_SCHEMA_VERSION,
            serde_json::json!("close_session"),
        );
        let old_event = DynamicEvent::new(
            "forge.core_agent.lifecycle.closed",
            CORE_AGENT_SCHEMA_VERSION,
            serde_json::json!({
                "kind": {
                    "lifecycle": "closed"
                }
            }),
        );

        assert!(matches!(
            codec.decode_command(&old_command),
            Err(CodecError::Unsupported { .. })
        ));
        assert!(matches!(
            codec.decode_event(&old_event),
            Err(CodecError::Unsupported { .. })
        ));
    }

    #[test]
    fn core_agent_lifecycle_fixture_matches_codec() {
        assert_fixture_round_trip(
            include_str!("../../fixtures/core_lifecycle_closed_dynamic_event.json"),
            CoreAgentEvent {
                kind: CoreAgentEventKind::Lifecycle(CoreAgentLifecycleEvent::Closed),
            },
        );
    }

    #[test]
    fn core_agent_command_fixture_matches_codec() {
        let codec = CoreAgentCodec;
        let fixture_command = serde_json::from_str::<crate::DynamicCommand>(include_str!(
            "../../fixtures/core_close_session_dynamic_command.json"
        ))
        .expect("fixture is a dynamic command");

        assert_eq!(
            codec
                .encode_command(&CoreAgentCommand::CloseSession)
                .expect("encode command"),
            fixture_command
        );
        assert_eq!(
            codec
                .decode_command(&fixture_command)
                .expect("decode command"),
            CoreAgentCommand::CloseSession
        );
    }

    #[test]
    fn core_agent_tool_fixture_matches_codec() {
        assert_fixture_round_trip(
            include_str!("../../fixtures/core_tool_batch_completed_dynamic_event.json"),
            CoreAgentEvent {
                kind: CoreAgentEventKind::Tool(ToolEvent::BatchCompleted {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(2),
                    batch_id: ToolBatchId::new(3),
                }),
            },
        );
    }

    #[test]
    fn core_agent_dynamic_entry_fixture_matches_codec() {
        let codec = CoreAgentCodec;
        let fixture_entry = serde_json::from_str::<DynamicSessionEntry>(include_str!(
            "../../fixtures/core_tool_batch_completed_dynamic_entry.json"
        ))
        .expect("fixture is a dynamic session entry");
        let typed_entry = CoreAgentEntry {
            position: SessionPosition {
                seq: EventSeq::new(42),
            },
            observed_at_ms: 1_700_000_000_000,
            joins: CoreAgentJoins {
                run_id: Some(RunId::new(1)),
                turn_id: Some(TurnId::new(2)),
                tool_batch_id: Some(ToolBatchId::new(3)),
                correlation_id: Some(CorrelationId::new("corr-1")),
                ..CoreAgentJoins::default()
            },
            event: CoreAgentEvent {
                kind: CoreAgentEventKind::Tool(ToolEvent::BatchCompleted {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(2),
                    batch_id: ToolBatchId::new(3),
                }),
            },
        };

        assert_eq!(
            codec.encode_entry(&typed_entry).expect("encode entry"),
            fixture_entry
        );
        assert_eq!(
            codec.decode_entry(&fixture_entry).expect("decode entry"),
            typed_entry
        );
    }

    fn assert_fixture_round_trip(fixture: &str, event: CoreAgentEvent) {
        let codec = CoreAgentCodec;
        let fixture_event =
            serde_json::from_str::<DynamicEvent>(fixture).expect("fixture is a dynamic event");

        assert_eq!(
            codec.encode_event(&event).expect("encode event"),
            fixture_event
        );
        assert_eq!(
            codec.decode_event(&fixture_event).expect("decode event"),
            event
        );
    }
}
