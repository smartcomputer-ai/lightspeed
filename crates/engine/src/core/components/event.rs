use serde::{Deserialize, Serialize};

use crate::{
    ContextEvent, CoreAgentLifecycleEvent, RunEvent, SkillEvent, ToolConfigEvent, ToolEvent,
    TurnEvent,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreAgentEvent {
    pub kind: CoreAgentEventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentEventKind {
    Lifecycle(CoreAgentLifecycleEvent),
    Run(RunEvent),
    Turn(TurnEvent),
    Context(ContextEvent),
    Skill(SkillEvent),
    ToolConfig(ToolConfigEvent),
    Tool(ToolEvent),
}
