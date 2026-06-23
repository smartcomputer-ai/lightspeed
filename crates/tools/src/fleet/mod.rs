//! Fleet subagent control-plane tool contracts.

use engine::{
    FunctionToolSpec, ToolKind, ToolName, ToolParallelism, ToolSpec, ToolTargetRequirement,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::{ToolBinding, ToolDocument, ToolExecutionMode, ToolSpecBundle},
};

pub const AGENT_SPAWN_TOOL_NAME: &str = "agent_spawn";
pub const AGENT_LIST_TOOL_NAME: &str = "agent_list";
pub const AGENT_READ_TOOL_NAME: &str = "agent_read";
pub const AGENT_CANCEL_TOOL_NAME: &str = "agent_cancel";

pub const FLEET_LOGICAL_ID_PREFIX: &str = "fleet.";
pub const FLEET_ACTIVITY_TYPE: &str = "lightspeed.fleet";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FleetToolsetConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl FleetToolsetConfig {
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    pub fn enabled() -> Self {
        Self { enabled: true }
    }
}

pub fn is_fleet_tool(tool_name: &ToolName) -> bool {
    matches!(
        tool_name.as_str(),
        AGENT_SPAWN_TOOL_NAME
            | AGENT_LIST_TOOL_NAME
            | AGENT_READ_TOOL_NAME
            | AGENT_CANCEL_TOOL_NAME
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSpawnArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    pub input: String,
    #[serde(default)]
    pub source: AgentSpawnSource,
    #[serde(default)]
    pub fork: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_at_seq: Option<u64>,
    #[serde(default)]
    pub vfs: VfsPolicy,
    #[serde(default)]
    pub environment: EnvironmentPolicy,
    #[serde(default)]
    pub lifecycle: AgentSpawnLifecycle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentSpawnSource {
    #[serde(rename = "self")]
    #[default]
    Self_,
    Session {
        session_id: String,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VfsPolicy {
    #[default]
    Share,
    Isolate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentPolicy {
    #[default]
    Share,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSpawnLifecycle {
    #[serde(default = "default_run_immediately")]
    pub run_immediately: bool,
}

impl Default for AgentSpawnLifecycle {
    fn default() -> Self {
        Self {
            run_immediately: true,
        }
    }
}

fn default_run_immediately() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentReadArgs {
    pub target_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_transcript: Option<RecentTranscriptSelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_events: Option<RecentEventsSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    #[serde(default)]
    pub direction: AgentListDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentListDirection {
    #[default]
    Children,
    Parents,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentCancelArgs {
    pub target_agent_id: String,
    pub scope: AgentCancelScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCancelScope {
    ActiveRun,
    Session,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecentTranscriptSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecentEventsSelector {
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentSpawnOutput {
    pub child_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentLineageView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentLinkView {
    pub from_agent_id: String,
    pub to_agent_id: String,
    pub relationship: String,
    pub created_at_ms: u64,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentListItem {
    pub agent_id: String,
    pub relationship: String,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    pub lineage: AgentLineageView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentListOutput {
    pub target_agent_id: String,
    pub direction: AgentListDirection,
    #[serde(default)]
    pub agents: Vec<AgentListItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentReadOutput {
    pub agent_id: String,
    pub session: Value,
    pub lineage: AgentLineageView,
    #[serde(default)]
    pub links: Vec<AgentLinkView>,
    #[serde(default)]
    pub environments: Value,
    #[serde(default)]
    pub recent_events: Vec<Value>,
    #[serde(default)]
    pub recent_transcript: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCancelOutput {
    pub target_agent_id: String,
    pub scope: AgentCancelScope,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Value>,
}

pub fn fleet_tool_bundles(config: &FleetToolsetConfig) -> ToolResult<Vec<ToolSpecBundle>> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    Ok(vec![
        function_bundle(
            AGENT_SPAWN_TOOL_NAME,
            "Create a linked child agent session by cloning or forking a source session and optionally start its first run.",
            spawn_input_schema(),
        )?,
        function_bundle(
            AGENT_LIST_TOOL_NAME,
            "List related Fleet agents with compact status. Use agent_read for details on one agent.",
            list_input_schema(),
        )?,
        function_bundle(
            AGENT_READ_TOOL_NAME,
            "Read one Fleet agent's status, full effective config, resource summary, lineage, and bounded recent activity.",
            read_input_schema(),
        )?,
        function_bundle(
            AGENT_CANCEL_TOOL_NAME,
            "Cancel a related agent's active run or close the child agent, subject to policy.",
            cancel_input_schema(),
        )?,
    ])
}

pub fn fleet_tool_bindings(execution: ToolExecutionMode) -> Vec<ToolBinding> {
    [
        AGENT_SPAWN_TOOL_NAME,
        AGENT_LIST_TOOL_NAME,
        AGENT_READ_TOOL_NAME,
        AGENT_CANCEL_TOOL_NAME,
    ]
    .into_iter()
    .map(|tool_name| {
        ToolBinding::new(
            ToolName::new(tool_name),
            format!("{FLEET_LOGICAL_ID_PREFIX}{tool_name}"),
            FLEET_ACTIVITY_TYPE,
            execution.clone(),
            ToolParallelism::Exclusive,
        )
    })
    .collect()
}

fn function_bundle(
    tool_name: &'static str,
    description: &'static str,
    input_schema: Value,
) -> ToolResult<ToolSpecBundle> {
    let description = ToolDocument::text("text/plain; charset=utf-8", description);
    let input_schema = ToolDocument::text(
        "application/schema+json",
        serde_json::to_string(&input_schema).map_err(|error| ToolError::InvalidRequest {
            message: format!("failed to encode {tool_name} schema: {error}"),
        })?,
    );
    Ok(ToolSpecBundle {
        spec: ToolSpec {
            name: ToolName::new(tool_name),
            kind: ToolKind::Function(FunctionToolSpec {
                model_name: None,
                description_ref: Some(description.blob_ref.clone()),
                input_schema_ref: input_schema.blob_ref.clone(),
                output_schema_ref: None,
                strict: Some(true),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::Exclusive,
            target_requirement: ToolTargetRequirement::None,
        },
        documents: vec![description, input_schema],
    })
}

fn source_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "self" }
                },
                "required": ["kind"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "session" },
                    "session_id": {
                        "type": "string",
                        "description": "Source session id to clone or fork."
                    }
                },
                "required": ["kind", "session_id"],
                "additionalProperties": false
            }
        ],
        "default": { "kind": "self" }
    })
}

fn spawn_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "child_session_id": {
                "type": ["string", "null"],
                "description": "Optional explicit durable child session id. If omitted, the runtime derives one from this parent tool call."
            },
            "input": {
                "type": "string",
                "description": "Initial task text for the child run."
            },
            "source": source_schema(),
            "fork": {
                "type": "boolean",
                "default": false,
                "description": "When true, create a history fork. When false, create a fresh-log clone."
            },
            "fork_at_seq": {
                "type": ["integer", "null"],
                "minimum": 0,
                "description": "Optional explicit source sequence for fork; rejected if it lands inside an open run."
            },
            "vfs": {
                "type": "string",
                "enum": ["share", "isolate"],
                "default": "share"
            },
            "environment": {
                "type": "string",
                "enum": ["share"],
                "default": "share"
            },
            "lifecycle": {
                "type": "object",
                "properties": {
                    "run_immediately": { "type": "boolean", "default": true }
                },
                "additionalProperties": false,
                "default": { "run_immediately": true }
            }
        },
        "required": ["input"],
        "additionalProperties": false
    })
}

fn read_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_agent_id": { "type": "string" },
            "recent_transcript": recent_transcript_schema(),
            "recent_events": recent_events_schema()
        },
        "required": ["target_agent_id"],
        "additionalProperties": false
    })
}

fn list_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_agent_id": {
                "type": ["string", "null"],
                "description": "Agent whose relationships should be listed. Defaults to the caller."
            },
            "direction": {
                "type": "string",
                "enum": ["children", "parents"],
                "default": "children"
            },
            "limit": {
                "type": ["integer", "null"],
                "minimum": 1,
                "maximum": 100
            }
        },
        "required": [],
        "additionalProperties": false
    })
}

fn cancel_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_agent_id": { "type": "string" },
            "scope": {
                "type": "string",
                "enum": ["active_run", "session"]
            },
            "reason": { "type": ["string", "null"] }
        },
        "required": ["target_agent_id", "scope"],
        "additionalProperties": false
    })
}

fn recent_transcript_schema() -> Value {
    json!({
        "type": ["object", "null"],
        "properties": {
            "turns": { "type": ["integer", "null"], "minimum": 1, "maximum": 20 },
            "events": { "type": ["integer", "null"], "minimum": 1, "maximum": 100 }
        },
        "additionalProperties": false
    })
}

fn recent_events_schema() -> Value {
    json!({
        "type": ["object", "null"],
        "properties": {
            "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
        },
        "required": ["limit"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_source_is_tagged_so_self_can_be_a_session_id() {
        let args: AgentSpawnArgs = serde_json::from_value(json!({
            "input": "do work",
            "source": { "kind": "session", "session_id": "self" }
        }))
        .expect("decode args");

        assert_eq!(
            args.source,
            AgentSpawnSource::Session {
                session_id: "self".to_owned()
            }
        );
    }

    #[test]
    fn spawn_rejects_environment_isolate() {
        let error = serde_json::from_value::<AgentSpawnArgs>(json!({
            "input": "do work",
            "environment": "isolate"
        }))
        .expect_err("environment isolate is not a v1 value");

        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn spawn_rejects_unknown_fields() {
        serde_json::from_value::<AgentSpawnArgs>(json!({
            "input": "do work",
            "task_name": "old contract"
        }))
        .expect_err("unknown fields are denied");
    }

    #[test]
    fn spawn_rejects_config_overrides() {
        serde_json::from_value::<AgentSpawnArgs>(json!({
            "input": "do work",
            "config_overrides": {
                "tools": {
                    "fleet": { "op": "set", "value": true }
                }
            }
        }))
        .expect_err("raw API config patches are not part of agent_spawn");
    }

    #[test]
    fn cancel_rejects_queued_runs_scope() {
        let error = serde_json::from_value::<AgentCancelArgs>(json!({
            "target_agent_id": "child",
            "scope": "queued_runs"
        }))
        .expect_err("queued run cancellation is not part of v1");

        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn disabled_config_produces_no_tools() {
        let bundles = fleet_tool_bundles(&FleetToolsetConfig::disabled()).expect("bundles");
        assert!(bundles.is_empty());
    }
}
