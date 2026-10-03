use serde::{Deserialize, Serialize};

use crate::{
    ApprovalEvent, ContextEvent, CoreAgentLifecycleEvent, EnvironmentEvent, PromiseEvent, RunEvent,
    ToolConfigEvent, ToolEvent, TurnEvent, WorkflowToolConfigEvent, WorkflowToolEvent,
};

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
}
