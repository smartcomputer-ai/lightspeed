use serde::{Deserialize, Serialize};

use crate::{
    ContextEvent, CoreAgentLifecycleEvent, PromiseEvent, RunEvent, ToolConfigEvent, ToolEvent,
    TurnEvent, WorkflowPortConfigEvent, WorkflowPortEvent,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentEvent {
    Lifecycle(CoreAgentLifecycleEvent),
    Run(RunEvent),
    Turn(TurnEvent),
    Context(ContextEvent),
    ToolConfig(ToolConfigEvent),
    Tool(ToolEvent),
    Promise(PromiseEvent),
    WorkflowPortConfig(WorkflowPortConfigEvent),
    WorkflowPort(WorkflowPortEvent),
}
