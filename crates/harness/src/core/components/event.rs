use serde::{Deserialize, Serialize};

use crate::{
    ApprovalEvent, ContextEvent, CoreAgentLifecycleEvent, EnvironmentEvent, PromiseEvent, RunEvent,
    ToolConfigEvent, ToolEvent, TurnEvent, WorkflowToolConfigEvent, WorkflowToolEvent,
};

/// A stored session's log has no fallback: it decodes and reduces, or the session cannot
/// load. So a new field on any event gets a serde default, removing a field is harmless,
/// and a variant is never renamed or removed while logs hold it. An unknown event stays a
/// decode error and is never skipped, because a session missing history must not load as
/// if it were whole. `tests/recorded_log.rs` holds these rules against a recorded log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// These durable, serde-backed domain events deliberately retain direct variant
// payloads. Boxing the lifecycle variant would change every construction and
// pattern match for an allocation-only size optimization.
#[allow(clippy::large_enum_variant)]
pub enum CoreAgentEvent {
    Lifecycle(CoreAgentLifecycleEvent),
    Run(RunEvent),
    Approval(ApprovalEvent),
    Turn(TurnEvent),
    Context(ContextEvent),
    Environment(EnvironmentEvent),
    ToolConfig(ToolConfigEvent),
    Tool(ToolEvent),
    Promise(PromiseEvent),
    WorkflowToolConfig(WorkflowToolConfigEvent),
    WorkflowTool(WorkflowToolEvent),
    CodeTool(crate::CodeToolEvent),
}
