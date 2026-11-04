use crate::{
    ApprovalEvent, CodecError, ContextEvent, CoreAgentEntry, CoreAgentEvent, CoreAgentJoins,
    CoreAgentLifecycleEvent, PromiseEvent, RunEvent, RunId, StoredEvent, SubmissionId, ToolBatchId,
    ToolCallId, ToolConfigEvent, ToolEvent, TurnEvent, TurnId, UncommittedCoreAgentEvent,
    WorkflowToolConfigEvent,
    session::{StoredJoins, StoredSessionEntry, UncommittedStoredEvent},
};

const CORE_AGENT_EVENT_KIND_PREFIX: &str = "lightspeed.core.";
const CORE_AGENT_SCHEMA_VERSION: u32 = 1;

/// The stored `kind` of every core agent event. Durable log rows carry these
/// strings and store projections branch on them, so every reader names a
/// constant here instead of spelling the string.
pub mod event_kind {
    pub const LIFECYCLE_OPENED: &str = "lightspeed.core.lifecycle.opened";
    pub const LIFECYCLE_CONFIG_CHANGED: &str = "lightspeed.core.lifecycle.config_changed";
    pub const LIFECYCLE_CLOSED: &str = "lightspeed.core.lifecycle.closed";

    pub const RUN_ACCEPTED: &str = "lightspeed.core.run.accepted";
    pub const RUN_STARTED: &str = "lightspeed.core.run.started";
    pub const RUN_STEERING_ACCEPTED: &str = "lightspeed.core.run.steering_accepted";
    pub const RUN_CANCELLATION_REQUESTED: &str = "lightspeed.core.run.cancellation_requested";
    pub const RUN_COMPLETED: &str = "lightspeed.core.run.completed";
    pub const RUN_FAILED: &str = "lightspeed.core.run.failed";
    pub const RUN_CANCELLED: &str = "lightspeed.core.run.cancelled";
    pub const RUN_FORCE_CANCELLED: &str = "lightspeed.core.run.force_cancelled";
    pub const RUN_QUEUED_CANCELLED: &str = "lightspeed.core.run.queued_cancelled";
    /// The events a pruned run's record can be rebuilt from.
    pub const RUN_RECORD_KINDS: [&str; 7] = [
        RUN_ACCEPTED,
        RUN_STARTED,
        RUN_COMPLETED,
        RUN_FAILED,
        RUN_CANCELLED,
        RUN_FORCE_CANCELLED,
        RUN_QUEUED_CANCELLED,
    ];

    pub const APPROVAL_REQUESTED: &str = "lightspeed.core.approval.requested";
    pub const APPROVAL_RUN_PARKED: &str = "lightspeed.core.approval.run_parked";
    pub const APPROVAL_RUN_UNPARKED: &str = "lightspeed.core.approval.run_unparked";
    pub const APPROVAL_DECIDED: &str = "lightspeed.core.approval.decided";
    pub const APPROVAL_CANCELLED: &str = "lightspeed.core.approval.cancelled";

    pub const TURN_STARTED: &str = "lightspeed.core.turn.started";
    pub const TURN_PLANNED: &str = "lightspeed.core.turn.planned";
    pub const TURN_GENERATION_REQUESTED: &str = "lightspeed.core.turn.generation_requested";
    pub const TURN_GENERATION_COMPLETED: &str = "lightspeed.core.turn.generation_completed";
    pub const TURN_COMPLETED: &str = "lightspeed.core.turn.completed";
    pub const TURN_CANCELLED: &str = "lightspeed.core.turn.cancelled";

    pub const CONTEXT_ENTRIES_APPLIED: &str = "lightspeed.core.context.entries_applied";
    pub const CONTEXT_ENTRIES_REMOVED: &str = "lightspeed.core.context.entries_removed";
    pub const CONTEXT_ENTRIES_REPLACED: &str = "lightspeed.core.context.entries_replaced";
    pub const CONTEXT_KEYS_REMOVED: &str = "lightspeed.core.context.keys_removed";
    pub const CONTEXT_KEY_PREFIX_REPLACED: &str = "lightspeed.core.context.key_prefix_replaced";
    pub const CONTEXT_COMPACTION_QUEUED: &str = "lightspeed.core.context.compaction_queued";
    pub const CONTEXT_COMPACTION_REQUESTED: &str = "lightspeed.core.context.compaction_requested";
    pub const CONTEXT_COMPACTION_FINISHED: &str = "lightspeed.core.context.compaction_finished";

    pub const ENVIRONMENT_ACTIVE_SET: &str = "lightspeed.core.environment.active_set";
    pub const ENVIRONMENT_ACTIVE_CLEARED: &str = "lightspeed.core.environment.active_cleared";

    pub const TOOL_CONFIG_TOOLS_REPLACED: &str = "lightspeed.core.tool_config.tools_replaced";
    pub const TOOL_CONFIG_TOOLS_PATCHED: &str = "lightspeed.core.tool_config.tools_patched";

    pub const TOOL_BATCH_STARTED: &str = "lightspeed.core.tool.batch_started";
    pub const TOOL_CALL_STARTED: &str = "lightspeed.core.tool.call_started";
    pub const TOOL_CALL_COMPLETED: &str = "lightspeed.core.tool.call_completed";
    pub const TOOL_BATCH_PARKED: &str = "lightspeed.core.tool.batch_parked";
    pub const TOOL_BATCH_UNPARKED: &str = "lightspeed.core.tool.batch_unparked";
    pub const TOOL_BATCH_COMPLETED: &str = "lightspeed.core.tool.batch_completed";

    pub const PROMISE_CREATED: &str = "lightspeed.core.promise.created";
    pub const PROMISE_RESOLVED: &str = "lightspeed.core.promise.resolved";
    pub const PROMISE_FAILED: &str = "lightspeed.core.promise.failed";
    pub const PROMISE_CANCELLED: &str = "lightspeed.core.promise.cancelled";
    pub const PROMISE_DETACHED: &str = "lightspeed.core.promise.detached";

    pub const WORKFLOW_TOOL_CONFIG_DECLARED_BINDINGS_ADMITTED: &str =
        "lightspeed.core.workflow_tool_config.declared_bindings_admitted";
    pub const WORKFLOW_TOOL_CONFIG_SYSTEM_BINDING_ADMITTED: &str =
        "lightspeed.core.workflow_tool_config.system_binding_admitted";

    pub const CODE_TOOL_SCOPE_OPENED: &str = "lightspeed.core.code_tool.scope_opened";
    pub const CODE_TOOL_CALL_ADMITTED: &str = "lightspeed.core.code_tool.call_admitted";
    pub const CODE_TOOL_CALL_COMPLETED: &str = "lightspeed.core.code_tool.call_completed";
    pub const CODE_TOOL_CALL_DEFERRED: &str = "lightspeed.core.code_tool.call_deferred";
    pub const CODE_TOOL_SCOPE_CLOSED: &str = "lightspeed.core.code_tool.scope_closed";

    pub const WORKFLOW_TOOL_INVOKED: &str = "lightspeed.core.workflow_tool.invoked";
    pub const WORKFLOW_TOOL_DELIVERY_FAILED: &str = "lightspeed.core.workflow_tool.delivery_failed";
    pub const WORKFLOW_TOOL_START_REQUESTED: &str = "lightspeed.core.workflow_tool.start_requested";
    pub const WORKFLOW_TOOL_START_FAILED: &str = "lightspeed.core.workflow_tool.start_failed";
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreAgentCodec;

impl CoreAgentCodec {
    pub fn encode_event(&self, event: &CoreAgentEvent) -> Result<StoredEvent, CodecError> {
        let mut payload = serde_json::to_value(event).map_err(codec_failure)?;
        replace_nul_in_json(&mut payload);
        Ok(StoredEvent::new(
            core_agent_event_envelope_kind(event),
            CORE_AGENT_SCHEMA_VERSION,
            payload,
        ))
    }

    pub fn decode_event(&self, event: &StoredEvent) -> Result<CoreAgentEvent, CodecError> {
        ensure_core_agent_event_envelope(&event.kind, event.version)?;
        serde_json::from_value(event.payload.clone()).map_err(codec_failure)
    }

    pub fn encode_joins(&self, joins: &CoreAgentJoins) -> StoredJoins {
        let mut encoded = StoredJoins::new();
        insert_numeric(&mut encoded, "run_id", joins.run_id.map(RunId::as_u64));
        insert_numeric(&mut encoded, "turn_id", joins.turn_id.map(TurnId::as_u64));
        insert_numeric(
            &mut encoded,
            "tool_batch_id",
            joins.tool_batch_id.map(ToolBatchId::as_u64),
        );
        insert_string(&mut encoded, "tool_call_id", joins.tool_call_id.as_ref());
        insert_string(&mut encoded, "submission_id", joins.submission_id.as_ref());
        encoded
    }

    pub fn decode_joins(&self, joins: &StoredJoins) -> Result<CoreAgentJoins, CodecError> {
        Ok(CoreAgentJoins {
            run_id: parse_numeric(joins, "run_id")?.map(RunId::new),
            turn_id: parse_numeric(joins, "turn_id")?.map(TurnId::new),
            tool_batch_id: parse_numeric(joins, "tool_batch_id")?.map(ToolBatchId::new),
            tool_call_id: parse_string_id(joins, "tool_call_id", ToolCallId::try_new)?,
            submission_id: parse_string_id(joins, "submission_id", SubmissionId::try_new)?,
        })
    }

    pub fn encode_entry(&self, entry: &CoreAgentEntry) -> Result<StoredSessionEntry, CodecError> {
        Ok(StoredSessionEntry {
            position: entry.position.clone(),
            observed_at_ms: entry.observed_at_ms,
            joins: self.encode_joins(&entry.joins),
            event: self.encode_event(&entry.event)?,
        })
    }

    pub fn decode_entry(&self, entry: &StoredSessionEntry) -> Result<CoreAgentEntry, CodecError> {
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
    ) -> Result<UncommittedStoredEvent, CodecError> {
        Ok(UncommittedStoredEvent {
            observed_at_ms: event.observed_at_ms,
            joins: self.encode_joins(&event.joins),
            event: self.encode_event(&event.event)?,
        })
    }
}

/// A session log cannot hold U+0000: Postgres refuses it in jsonb and in text. A preview
/// of what a model wrote or a caller's label is not worth a run, so the durable format
/// replaces it with U+FFFD. State only ever applies committed entries, so the log, live
/// state and replay all see the same replacement.
fn replace_nul(text: &str) -> String {
    text.replace('\0', "\u{FFFD}")
}

fn replace_nul_in_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) if text.contains('\0') => *text = replace_nul(text),
        serde_json::Value::Array(values) => values.iter_mut().for_each(replace_nul_in_json),
        serde_json::Value::Object(object) => {
            if object.keys().any(|key| key.contains('\0')) {
                *object = std::mem::take(object)
                    .into_iter()
                    .map(|(key, value)| (replace_nul(&key), value))
                    .collect();
            }
            object.values_mut().for_each(replace_nul_in_json);
        }
        _ => {}
    }
}

/// The envelope kind is derived metadata for indexing and debugging; the
/// payload embeds its own discriminant, so decode only guards the namespace
/// and schema version before parsing the payload wholesale.
fn ensure_core_agent_event_envelope(kind: &str, version: u32) -> Result<(), CodecError> {
    if version == CORE_AGENT_SCHEMA_VERSION && kind.starts_with(CORE_AGENT_EVENT_KIND_PREFIX) {
        Ok(())
    } else {
        Err(CodecError::Unsupported {
            kind: kind.to_owned(),
            version,
        })
    }
}

fn core_agent_event_envelope_kind(event: &CoreAgentEvent) -> &'static str {
    match event {
        CoreAgentEvent::Lifecycle(event) => match event {
            CoreAgentLifecycleEvent::Opened { .. } => event_kind::LIFECYCLE_OPENED,
            CoreAgentLifecycleEvent::ConfigChanged { .. } => event_kind::LIFECYCLE_CONFIG_CHANGED,
            CoreAgentLifecycleEvent::Closed => event_kind::LIFECYCLE_CLOSED,
        },
        CoreAgentEvent::Run(event) => match event {
            RunEvent::Accepted(_) => event_kind::RUN_ACCEPTED,
            RunEvent::Started { .. } => event_kind::RUN_STARTED,
            RunEvent::SteeringAccepted { .. } => event_kind::RUN_STEERING_ACCEPTED,
            RunEvent::CancellationRequested { .. } => event_kind::RUN_CANCELLATION_REQUESTED,
            RunEvent::Completed { .. } => event_kind::RUN_COMPLETED,
            RunEvent::Failed { .. } => event_kind::RUN_FAILED,
            RunEvent::Cancelled { .. } => event_kind::RUN_CANCELLED,
            RunEvent::ForceCancelled { .. } => event_kind::RUN_FORCE_CANCELLED,
            RunEvent::QueuedCancelled { .. } => event_kind::RUN_QUEUED_CANCELLED,
        },
        CoreAgentEvent::Approval(event) => match event {
            ApprovalEvent::Requested { .. } => event_kind::APPROVAL_REQUESTED,
            ApprovalEvent::RunParked { .. } => event_kind::APPROVAL_RUN_PARKED,
            ApprovalEvent::RunUnparked { .. } => event_kind::APPROVAL_RUN_UNPARKED,
            ApprovalEvent::Decided { .. } => event_kind::APPROVAL_DECIDED,
            ApprovalEvent::Cancelled { .. } => event_kind::APPROVAL_CANCELLED,
        },
        CoreAgentEvent::Turn(event) => match event {
            TurnEvent::Started { .. } => event_kind::TURN_STARTED,
            TurnEvent::Planned { .. } => event_kind::TURN_PLANNED,
            TurnEvent::GenerationRequested { .. } => event_kind::TURN_GENERATION_REQUESTED,
            TurnEvent::GenerationCompleted { .. } => event_kind::TURN_GENERATION_COMPLETED,
            TurnEvent::Completed { .. } => event_kind::TURN_COMPLETED,
            TurnEvent::Cancelled { .. } => event_kind::TURN_CANCELLED,
        },
        CoreAgentEvent::Context(event) => match event {
            ContextEvent::EntriesApplied { .. } => event_kind::CONTEXT_ENTRIES_APPLIED,
            ContextEvent::EntriesRemoved { .. } => event_kind::CONTEXT_ENTRIES_REMOVED,
            ContextEvent::EntriesReplaced { .. } => event_kind::CONTEXT_ENTRIES_REPLACED,
            ContextEvent::KeysRemoved { .. } => event_kind::CONTEXT_KEYS_REMOVED,
            ContextEvent::KeyPrefixReplaced { .. } => event_kind::CONTEXT_KEY_PREFIX_REPLACED,
            ContextEvent::CompactionQueued { .. } => event_kind::CONTEXT_COMPACTION_QUEUED,
            ContextEvent::CompactionRequested { .. } => event_kind::CONTEXT_COMPACTION_REQUESTED,
            ContextEvent::CompactionFinished { .. } => event_kind::CONTEXT_COMPACTION_FINISHED,
        },
        CoreAgentEvent::Environment(event) => match event {
            crate::EnvironmentEvent::ActiveEnvironmentSet { .. } => {
                event_kind::ENVIRONMENT_ACTIVE_SET
            }
            crate::EnvironmentEvent::ActiveEnvironmentCleared => {
                event_kind::ENVIRONMENT_ACTIVE_CLEARED
            }
        },
        CoreAgentEvent::ToolConfig(event) => match event {
            ToolConfigEvent::ToolsReplaced { .. } => event_kind::TOOL_CONFIG_TOOLS_REPLACED,
            ToolConfigEvent::ToolsPatched { .. } => event_kind::TOOL_CONFIG_TOOLS_PATCHED,
        },
        CoreAgentEvent::Tool(event) => match event {
            ToolEvent::BatchStarted { .. } => event_kind::TOOL_BATCH_STARTED,
            ToolEvent::CallStarted { .. } => event_kind::TOOL_CALL_STARTED,
            ToolEvent::CallCompleted { .. } => event_kind::TOOL_CALL_COMPLETED,
            ToolEvent::BatchParked { .. } => event_kind::TOOL_BATCH_PARKED,
            ToolEvent::BatchUnparked { .. } => event_kind::TOOL_BATCH_UNPARKED,
            ToolEvent::BatchCompleted { .. } => event_kind::TOOL_BATCH_COMPLETED,
        },
        CoreAgentEvent::Promise(event) => match event {
            PromiseEvent::Created { .. } => event_kind::PROMISE_CREATED,
            PromiseEvent::Resolved { .. } => event_kind::PROMISE_RESOLVED,
            PromiseEvent::Failed { .. } => event_kind::PROMISE_FAILED,
            PromiseEvent::Cancelled { .. } => event_kind::PROMISE_CANCELLED,
            PromiseEvent::Detached { .. } => event_kind::PROMISE_DETACHED,
        },
        CoreAgentEvent::WorkflowToolConfig(event) => match event {
            WorkflowToolConfigEvent::DeclaredBindingsAdmitted { .. } => {
                event_kind::WORKFLOW_TOOL_CONFIG_DECLARED_BINDINGS_ADMITTED
            }
            WorkflowToolConfigEvent::SystemBindingAdmitted { .. } => {
                event_kind::WORKFLOW_TOOL_CONFIG_SYSTEM_BINDING_ADMITTED
            }
        },
        CoreAgentEvent::CodeTool(event) => match event {
            crate::CodeToolEvent::ScopeOpened { .. } => event_kind::CODE_TOOL_SCOPE_OPENED,
            crate::CodeToolEvent::CallAdmitted { .. } => event_kind::CODE_TOOL_CALL_ADMITTED,
            crate::CodeToolEvent::CallCompleted { .. } => event_kind::CODE_TOOL_CALL_COMPLETED,
            crate::CodeToolEvent::CallDeferred { .. } => event_kind::CODE_TOOL_CALL_DEFERRED,
            crate::CodeToolEvent::ScopeClosed { .. } => event_kind::CODE_TOOL_SCOPE_CLOSED,
        },
        CoreAgentEvent::WorkflowTool(event) => match event {
            crate::WorkflowToolEvent::Invoked { .. } => event_kind::WORKFLOW_TOOL_INVOKED,
            crate::WorkflowToolEvent::DeliveryFailed { .. } => {
                event_kind::WORKFLOW_TOOL_DELIVERY_FAILED
            }
            crate::WorkflowToolEvent::StartRequested { .. } => {
                event_kind::WORKFLOW_TOOL_START_REQUESTED
            }
            crate::WorkflowToolEvent::StartFailed { .. } => event_kind::WORKFLOW_TOOL_START_FAILED,
        },
    }
}

fn insert_numeric(joins: &mut StoredJoins, key: &'static str, value: Option<u64>) {
    if let Some(value) = value {
        joins.insert(key.to_owned(), value.to_string());
    }
}

fn insert_string<T: ToString>(joins: &mut StoredJoins, key: &'static str, value: Option<&T>) {
    if let Some(value) = value {
        joins.insert(key.to_owned(), value.to_string());
    }
}

fn parse_numeric(joins: &StoredJoins, key: &'static str) -> Result<Option<u64>, CodecError> {
    joins
        .get(key)
        .map(|value: &String| {
            value.parse::<u64>().map_err(|error| CodecError::Failed {
                message: format!("invalid stored join {key}: {error}"),
            })
        })
        .transpose()
}

fn parse_string_id<T, F>(
    joins: &StoredJoins,
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
                message: format!("invalid stored join {key}: {error}"),
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
        CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent, EventSeq, RunId, SessionPosition,
        ToolBatchId, ToolEvent, TurnId,
    };

    use super::*;

    #[test]
    fn core_agent_event_round_trips_through_stored_envelope() {
        let codec = CoreAgentCodec;
        let event = CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed);

        let encoded = codec.encode_event(&event).expect("encode event");
        assert_eq!(encoded.kind, "lightspeed.core.lifecycle.closed");
        assert_eq!(encoded.version, CORE_AGENT_SCHEMA_VERSION);

        assert_eq!(codec.decode_event(&encoded).expect("decode event"), event);
    }

    #[test]
    fn nul_is_replaced_in_every_string_value_and_key_at_any_depth() {
        let mut value = serde_json::json!({
            "plain": "untouched",
            "text": "a\0b\0",
            "key\0": 1,
            "list": ["\0", { "nested\0key": "nested\0value" }, 2, null],
        });
        replace_nul_in_json(&mut value);
        assert_eq!(
            value,
            serde_json::json!({
                "plain": "untouched",
                "text": "a\u{FFFD}b\u{FFFD}",
                "key\u{FFFD}": 1,
                "list": ["\u{FFFD}", { "nested\u{FFFD}key": "nested\u{FFFD}value" }, 2, null],
            })
        );
    }

    #[test]
    fn every_event_kind_is_a_distinct_core_kind() {
        let kinds = [
            event_kind::LIFECYCLE_OPENED,
            event_kind::LIFECYCLE_CONFIG_CHANGED,
            event_kind::LIFECYCLE_CLOSED,
            event_kind::RUN_ACCEPTED,
            event_kind::RUN_STARTED,
            event_kind::RUN_STEERING_ACCEPTED,
            event_kind::RUN_CANCELLATION_REQUESTED,
            event_kind::RUN_COMPLETED,
            event_kind::RUN_FAILED,
            event_kind::RUN_CANCELLED,
            event_kind::RUN_FORCE_CANCELLED,
            event_kind::RUN_QUEUED_CANCELLED,
            event_kind::APPROVAL_REQUESTED,
            event_kind::APPROVAL_RUN_PARKED,
            event_kind::APPROVAL_RUN_UNPARKED,
            event_kind::APPROVAL_DECIDED,
            event_kind::APPROVAL_CANCELLED,
            event_kind::TURN_STARTED,
            event_kind::TURN_PLANNED,
            event_kind::TURN_GENERATION_REQUESTED,
            event_kind::TURN_GENERATION_COMPLETED,
            event_kind::TURN_COMPLETED,
            event_kind::TURN_CANCELLED,
            event_kind::CONTEXT_ENTRIES_APPLIED,
            event_kind::CONTEXT_ENTRIES_REMOVED,
            event_kind::CONTEXT_ENTRIES_REPLACED,
            event_kind::CONTEXT_KEYS_REMOVED,
            event_kind::CONTEXT_KEY_PREFIX_REPLACED,
            event_kind::CONTEXT_COMPACTION_QUEUED,
            event_kind::CONTEXT_COMPACTION_REQUESTED,
            event_kind::CONTEXT_COMPACTION_FINISHED,
            event_kind::ENVIRONMENT_ACTIVE_SET,
            event_kind::ENVIRONMENT_ACTIVE_CLEARED,
            event_kind::TOOL_CONFIG_TOOLS_REPLACED,
            event_kind::TOOL_CONFIG_TOOLS_PATCHED,
            event_kind::TOOL_BATCH_STARTED,
            event_kind::TOOL_CALL_STARTED,
            event_kind::TOOL_CALL_COMPLETED,
            event_kind::TOOL_BATCH_PARKED,
            event_kind::TOOL_BATCH_UNPARKED,
            event_kind::TOOL_BATCH_COMPLETED,
            event_kind::PROMISE_CREATED,
            event_kind::PROMISE_RESOLVED,
            event_kind::PROMISE_FAILED,
            event_kind::PROMISE_CANCELLED,
            event_kind::PROMISE_DETACHED,
            event_kind::WORKFLOW_TOOL_CONFIG_DECLARED_BINDINGS_ADMITTED,
            event_kind::WORKFLOW_TOOL_CONFIG_SYSTEM_BINDING_ADMITTED,
            event_kind::CODE_TOOL_SCOPE_OPENED,
            event_kind::CODE_TOOL_CALL_ADMITTED,
            event_kind::CODE_TOOL_CALL_COMPLETED,
            event_kind::CODE_TOOL_CALL_DEFERRED,
            event_kind::CODE_TOOL_SCOPE_CLOSED,
            event_kind::WORKFLOW_TOOL_INVOKED,
            event_kind::WORKFLOW_TOOL_DELIVERY_FAILED,
            event_kind::WORKFLOW_TOOL_START_REQUESTED,
            event_kind::WORKFLOW_TOOL_START_FAILED,
        ];
        assert!(
            kinds
                .iter()
                .all(|kind| kind.starts_with(CORE_AGENT_EVENT_KIND_PREFIX))
        );
        let distinct = kinds.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), kinds.len());
    }

    #[test]
    fn foreign_envelope_kinds_and_versions_are_unsupported() {
        let codec = CoreAgentCodec;
        let foreign_kind = StoredEvent::new(
            "lightspeed.custom.lifecycle.closed",
            CORE_AGENT_SCHEMA_VERSION,
            serde_json::json!({
                "lifecycle": "closed"
            }),
        );
        let foreign_version = StoredEvent::new(
            "lightspeed.core.lifecycle.closed",
            CORE_AGENT_SCHEMA_VERSION + 1,
            serde_json::json!({
                "lifecycle": "closed"
            }),
        );

        assert!(matches!(
            codec.decode_event(&foreign_kind),
            Err(CodecError::Unsupported { .. })
        ));
        assert!(matches!(
            codec.decode_event(&foreign_version),
            Err(CodecError::Unsupported { .. })
        ));
    }

    #[test]
    fn core_agent_lifecycle_fixture_matches_codec() {
        assert_fixture_round_trip(
            include_str!("../../fixtures/core_lifecycle_closed_stored_event.json"),
            CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed),
        );
    }

    #[test]
    fn core_agent_tool_fixture_matches_codec() {
        assert_fixture_round_trip(
            include_str!("../../fixtures/core_tool_batch_completed_stored_event.json"),
            CoreAgentEvent::Tool(ToolEvent::BatchCompleted {
                run_id: RunId::new(1),
                turn_id: TurnId::new(2),
                batch_id: ToolBatchId::new(3),
            }),
        );
    }

    #[test]
    fn core_agent_stored_entry_fixture_matches_codec() {
        let codec = CoreAgentCodec;
        let fixture_entry = serde_json::from_str::<StoredSessionEntry>(include_str!(
            "../../fixtures/core_tool_batch_completed_stored_entry.json"
        ))
        .expect("fixture is a stored session entry");
        let typed_entry = CoreAgentEntry {
            position: SessionPosition {
                seq: EventSeq::new(42),
            },
            observed_at_ms: 1_700_000_000_000,
            joins: CoreAgentJoins {
                run_id: Some(RunId::new(1)),
                turn_id: Some(TurnId::new(2)),
                tool_batch_id: Some(ToolBatchId::new(3)),
                ..CoreAgentJoins::default()
            },
            event: CoreAgentEvent::Tool(ToolEvent::BatchCompleted {
                run_id: RunId::new(1),
                turn_id: TurnId::new(2),
                batch_id: ToolBatchId::new(3),
            }),
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
            serde_json::from_str::<StoredEvent>(fixture).expect("fixture is a stored event");

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
