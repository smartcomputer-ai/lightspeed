//! Fleet subagent control-plane tool contracts.

use api::{AgentProfile, AgentProfileSummary, ProfileSource};
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
pub const AGENT_REQUEST_TOOL_NAME: &str = "agent_request";
pub const AGENT_SEND_TOOL_NAME: &str = "agent_send";
pub const AGENT_LIST_TOOL_NAME: &str = "agent_list";
pub const AGENT_READ_TOOL_NAME: &str = "agent_read";
pub const PROFILE_LIST_TOOL_NAME: &str = "profile_list";
pub const PROFILE_READ_TOOL_NAME: &str = "profile_read";

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
            | AGENT_REQUEST_TOOL_NAME
            | AGENT_SEND_TOOL_NAME
            | AGENT_LIST_TOOL_NAME
            | AGENT_READ_TOOL_NAME
            | PROFILE_LIST_TOOL_NAME
            | PROFILE_READ_TOOL_NAME
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSpawnArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    pub input: String,
    #[serde(default, skip_serializing_if = "is_default_spawn_base")]
    pub base: AgentSpawnBase,
    #[serde(default)]
    pub vfs: VfsPolicy,
    #[serde(default)]
    pub environment: EnvironmentPolicy,
    #[serde(default)]
    pub lifecycle: AgentSpawnLifecycle,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentSpawnBase {
    #[serde(rename = "self")]
    Self_ {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork: Option<AgentSpawnFork>,
    },
    Session {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork: Option<AgentSpawnFork>,
    },
    Profile {
        profile: ProfileSource,
    },
}

impl Default for AgentSpawnBase {
    fn default() -> Self {
        Self::Self_ { fork: None }
    }
}

impl AgentSpawnBase {
    pub fn profile(&self) -> Option<&ProfileSource> {
        match self {
            Self::Profile { profile } => Some(profile),
            Self::Self_ { .. } | Self::Session { .. } => None,
        }
    }

    pub fn fork(&self) -> Option<&AgentSpawnFork> {
        match self {
            Self::Self_ { fork } | Self::Session { fork, .. } => fork.as_ref(),
            Self::Profile { .. } => None,
        }
    }
}

fn is_default_spawn_base(base: &AgentSpawnBase) -> bool {
    matches!(base, AgentSpawnBase::Self_ { fork: None })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentSpawnFork {
    Safe,
    AtSeq { seq: u64 },
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub close_on_terminal: bool,
}

impl Default for AgentSpawnLifecycle {
    fn default() -> Self {
        Self {
            run_immediately: true,
            close_on_terminal: false,
        }
    }
}

fn default_run_immediately() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentSendArgs {
    pub to: AgentSendTarget,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<AgentSendInputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentRequestArgs {
    pub to: AgentSendTarget,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<AgentSendInputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentSendTarget {
    Parent,
    Session { target_session_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AgentSendInputItem {
    Text {
        text: String,
    },
    TextRef {
        blob_ref: String,
    },
    Media {
        blob_ref: String,
        mime: String,
        kind: AgentSendMediaKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSendMediaKind {
    Image,
    Audio,
    Document,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentReadArgs {
    pub target_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_transcript: Option<RecentTranscriptSelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_events: Option<RecentEventsSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AgentListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_id: Option<String>,
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProfileListArgs {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProfileReadArgs {
    pub profile_id: String,
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
    /// Promise settled by the started child run's terminal state; absent
    /// when the spawn did not start a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentSendOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    pub status: AgentSendStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSendStatus {
    Delivered,
    NotReachable,
    QueueFull,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentRequestOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise: Option<String>,
    pub status: AgentRequestStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRequestStatus {
    Delivered,
    NotReachable,
    QueueFull,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentLineageView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentLinkView {
    pub from_session_id: String,
    pub to_session_id: String,
    pub relationship: String,
    pub created_at_ms: u64,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentListItem {
    pub session_id: String,
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
    pub target_session_id: String,
    pub direction: AgentListDirection,
    #[serde(default)]
    pub agents: Vec<AgentListItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentReadOutput {
    pub session_id: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProfileListOutput {
    #[serde(default)]
    pub profiles: Vec<AgentProfileSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProfileReadOutput {
    pub profile: AgentProfile,
}

pub fn fleet_tool_bundles(config: &FleetToolsetConfig) -> ToolResult<Vec<ToolSpecBundle>> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    let bundles = vec![
        function_bundle(
            AGENT_SPAWN_TOOL_NAME,
            "Create a linked child agent session by cloning or forking a source session and optionally start its first run.",
            spawn_input_schema(),
        )?,
        function_bundle(
            AGENT_REQUEST_TOOL_NAME,
            "Ask a reachable session to do work and return a promise resolved by the requested run's terminal output.",
            request_input_schema(),
        )?,
        function_bundle(
            AGENT_SEND_TOOL_NAME,
            "Deliver a fire-and-forget message to a reachable session. This never returns a promise.",
            send_input_schema(),
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
            PROFILE_LIST_TOOL_NAME,
            "List named agent profiles available for profile-based agent_spawn. Use profile_read to inspect a full profile document.",
            profile_list_input_schema(),
        )?,
        function_bundle(
            PROFILE_READ_TOOL_NAME,
            "Read one full named agent profile document by id before spawning or explaining profile setup.",
            profile_read_input_schema(),
        )?,
    ];
    Ok(bundles)
}

pub fn fleet_tool_bindings(execution: ToolExecutionMode) -> Vec<ToolBinding> {
    [
        AGENT_SPAWN_TOOL_NAME,
        AGENT_REQUEST_TOOL_NAME,
        AGENT_SEND_TOOL_NAME,
        AGENT_LIST_TOOL_NAME,
        AGENT_READ_TOOL_NAME,
        PROFILE_LIST_TOOL_NAME,
        PROFILE_READ_TOOL_NAME,
    ]
    .into_iter()
    .map(|tool_name| fleet_tool_binding(tool_name, execution.clone()))
    .collect()
}

fn fleet_tool_binding(tool_name: &str, execution: ToolExecutionMode) -> ToolBinding {
    ToolBinding::new(
        ToolName::new(tool_name),
        format!("{FLEET_LOGICAL_ID_PREFIX}{tool_name}"),
        FLEET_ACTIVITY_TYPE,
        execution,
        ToolParallelism::Exclusive,
    )
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
                strict: Some(false),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::Exclusive,
            target_requirement: ToolTargetRequirement::None,
        },
        documents: vec![description, input_schema],
    })
}

fn spawn_base_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "self" },
                    "fork": fork_schema()
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
                    },
                    "fork": fork_schema()
                },
                "required": ["kind", "session_id"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "profile" },
                    "profile": profile_source_schema()
                },
                "required": ["kind", "profile"],
                "additionalProperties": false
            }
        ],
        "default": { "kind": "self" },
        "description": "Base used to create the child: clone/fork self, clone/fork another live session, or instantiate a profile."
    })
}

fn fork_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": {
                        "const": "safe",
                        "description": "Fork at the runtime-computed safe sequence."
                    }
                },
                "required": ["kind"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "at_seq" },
                    "seq": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Explicit source sequence for fork; rejected if it lands inside an open run."
                    }
                },
                "required": ["kind", "seq"],
                "additionalProperties": false
            }
        ],
        "description": "When present, create a history fork instead of a fresh-log clone. Omit for clone semantics."
    })
}

fn profile_source_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "named" },
                    "profile_id": {
                        "type": "string",
                        "description": "Named agent profile id to instantiate."
                    },
                    "profileId": {
                        "type": "string",
                        "description": "Camel-case alias for profile_id."
                    }
                },
                "required": ["kind"],
                "oneOf": [
                    { "required": ["profile_id"] },
                    { "required": ["profileId"] }
                ],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "inline" },
                    "profile": {
                        "type": "object",
                        "description": "Inline agent profile document."
                    }
                },
                "required": ["kind", "profile"],
                "additionalProperties": false
            }
        ],
        "description": "Profile to instantiate as a fresh child session."
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
            "base": spawn_base_schema(),
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
                    "run_immediately": { "type": "boolean", "default": true },
                    "close_on_terminal": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, close the spawned child session after its started run reaches a terminal state and no queued work remains."
                    }
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
            "target_session_id": { "type": "string" },
            "recent_transcript": recent_transcript_schema(),
            "recent_events": recent_events_schema()
        },
        "required": ["target_session_id"],
        "additionalProperties": false
    })
}

fn send_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "parent" }
                },
                "required": ["kind"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "session" },
                    "target_session_id": {
                        "type": "string",
                        "description": "Recipient session id."
                    }
                },
                "required": ["kind", "target_session_id"],
                "additionalProperties": false
            }
        ]
    })
}

fn send_input_item_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "type": { "const": "text" },
                    "text": { "type": "string" }
                },
                "required": ["type", "text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "textRef" },
                    "blobRef": { "type": "string" }
                },
                "required": ["type", "blobRef"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "media" },
                    "blobRef": { "type": "string" },
                    "mime": { "type": "string" },
                    "kind": {
                        "type": "string",
                        "enum": ["image", "audio", "document"]
                    },
                    "name": { "type": ["string", "null"] }
                },
                "required": ["type", "blobRef", "mime", "kind"],
                "additionalProperties": false
            }
        ]
    })
}

fn arbitrary_json_schema(description: &'static str) -> Value {
    json!({
        "description": description,
        "anyOf": [
            {
                "type": "object",
                "additionalProperties": true
            },
            {
                "type": "array",
                "items": true
            },
            { "type": "string" },
            { "type": "number" },
            { "type": "boolean" },
            { "type": "null" }
        ]
    })
}

fn send_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": send_target_schema(),
            "text": {
                "type": "string",
                "description": "Message text placed inside the Fleet send envelope."
            },
            "input": {
                "type": "array",
                "items": send_input_item_schema(),
                "description": "Optional additional run input items appended after the Fleet send envelope."
            },
            "payload": arbitrary_json_schema("Optional structured JSON payload included in the Fleet send envelope."),
        },
        "required": ["to", "text"],
        "additionalProperties": false
    })
}

fn request_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": send_target_schema(),
            "text": {
                "type": "string",
                "description": "Request text placed inside the Fleet request envelope."
            },
            "input": {
                "type": "array",
                "items": send_input_item_schema(),
                "description": "Optional additional run input items appended after the Fleet request envelope."
            },
            "payload": arbitrary_json_schema("Optional structured JSON payload included in the Fleet request envelope."),
        },
        "required": ["to", "text"],
        "additionalProperties": false
    })
}

fn list_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_session_id": {
                "type": ["string", "null"],
                "description": "Session whose relationships should be listed. Defaults to the caller."
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

fn profile_list_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": false
    })
}

fn profile_read_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "profile_id": {
                "type": "string",
                "description": "Named agent profile id to read."
            }
        },
        "required": ["profile_id"],
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
    fn spawn_base_is_tagged_so_self_can_be_a_session_id() {
        let args: AgentSpawnArgs = serde_json::from_value(json!({
            "input": "do work",
            "base": { "kind": "session", "session_id": "self" }
        }))
        .expect("decode args");

        assert_eq!(
            args.base,
            AgentSpawnBase::Session {
                session_id: "self".to_owned(),
                fork: None
            }
        );
    }

    #[test]
    fn spawn_omits_base_as_clone_self_default() {
        let args: AgentSpawnArgs = serde_json::from_value(json!({
            "input": "do work"
        }))
        .expect("decode args");

        assert_eq!(args.base, AgentSpawnBase::Self_ { fork: None });
    }

    #[test]
    fn spawn_accepts_fork_on_live_session_base() {
        let args: AgentSpawnArgs = serde_json::from_value(json!({
            "input": "do work",
            "base": {
                "kind": "session",
                "session_id": "parent",
                "fork": { "kind": "at_seq", "seq": 10 }
            }
        }))
        .expect("decode args");

        assert_eq!(
            args.base,
            AgentSpawnBase::Session {
                session_id: "parent".to_owned(),
                fork: Some(AgentSpawnFork::AtSeq { seq: 10 })
            }
        );
    }

    #[test]
    fn spawn_accepts_safe_fork_on_self_base() {
        let args: AgentSpawnArgs = serde_json::from_value(json!({
            "input": "do work",
            "base": {
                "kind": "self",
                "fork": { "kind": "safe" }
            }
        }))
        .expect("decode args");

        assert_eq!(
            args.base,
            AgentSpawnBase::Self_ {
                fork: Some(AgentSpawnFork::Safe)
            }
        );
    }

    #[test]
    fn spawn_rejects_legacy_top_level_source() {
        serde_json::from_value::<AgentSpawnArgs>(json!({
            "input": "do work",
            "source": { "kind": "session", "session_id": "parent" }
        }))
        .expect_err("source moved under base");
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
    fn spawn_rejects_report_back() {
        serde_json::from_value::<AgentSpawnArgs>(json!({
            "input": "do work",
            "report_back": {}
        }))
        .expect_err("report_back is not part of agent_spawn");
    }

    #[test]
    fn spawn_accepts_close_on_terminal_lifecycle() {
        let args: AgentSpawnArgs = serde_json::from_value(json!({
            "input": "do work",
            "lifecycle": {
                "close_on_terminal": true
            }
        }))
        .expect("decode args");

        assert!(args.lifecycle.run_immediately);
        assert!(args.lifecycle.close_on_terminal);
    }

    #[test]
    fn send_rejects_unknown_fields() {
        serde_json::from_value::<AgentSendArgs>(json!({
            "to": { "kind": "session", "target_session_id": "child" },
            "text": "do more work",
            "priority": "high"
        }))
        .expect_err("unknown fields are denied");
    }

    #[test]
    fn send_accepts_tagged_parent_and_payload() {
        let args: AgentSendArgs = serde_json::from_value(json!({
            "to": { "kind": "parent" },
            "text": "done",
            "payload": { "ok": true },
            "input": [
                { "type": "text", "text": "trailing context" }
            ]
        }))
        .expect("decode send args");

        assert_eq!(args.to, AgentSendTarget::Parent);
        assert_eq!(args.payload, Some(json!({ "ok": true })));
        assert_eq!(args.input.len(), 1);
    }

    #[test]
    fn send_rejects_expect_tracking() {
        serde_json::from_value::<AgentSendArgs>(json!({
            "to": { "kind": "parent" },
            "text": "done",
            "expect": "completion"
        }))
        .expect_err("agent_send is fire-and-forget");
    }

    #[test]
    fn request_accepts_tagged_parent_and_payload() {
        let args: AgentRequestArgs = serde_json::from_value(json!({
            "to": { "kind": "parent" },
            "text": "please do work",
            "payload": { "ok": true },
            "input": [
                { "type": "text", "text": "details" }
            ]
        }))
        .expect("decode request args");

        assert_eq!(args.to, AgentSendTarget::Parent);
        assert_eq!(args.payload, Some(json!({ "ok": true })));
        assert_eq!(args.input.len(), 1);
    }

    #[test]
    fn send_rejects_kind_framing_field() {
        serde_json::from_value::<AgentSendArgs>(json!({
            "to": { "kind": "parent" },
            "text": "done",
            "kind": "result"
        }))
        .expect_err("kind is not part of the minimal first-cut send surface");
    }

    #[test]
    fn send_rejects_report_back() {
        serde_json::from_value::<AgentSendArgs>(json!({
            "to": { "kind": "parent" },
            "text": "done",
            "report_back": {}
        }))
        .expect_err("report_back is not part of agent_send");
    }

    #[test]
    fn request_rejects_report_back() {
        serde_json::from_value::<AgentRequestArgs>(json!({
            "to": { "kind": "session", "target_session_id": "child" },
            "text": "do work",
            "report_back": {}
        }))
        .expect_err("report_back is not part of agent_request");
    }

    #[test]
    fn profile_read_accepts_profile_id() {
        let args: ProfileReadArgs = serde_json::from_value(json!({
            "profile_id": "support"
        }))
        .expect("decode profile read args");

        assert_eq!(args.profile_id, "support");
    }

    #[test]
    fn profile_list_rejects_unknown_fields() {
        serde_json::from_value::<ProfileListArgs>(json!({
            "limit": 10
        }))
        .expect_err("unknown fields are denied");
    }

    #[test]
    fn enabled_config_includes_profile_tools() {
        let bundles = fleet_tool_bundles(&FleetToolsetConfig::enabled()).expect("bundles");
        let names: Vec<_> = bundles
            .iter()
            .map(|bundle| bundle.spec.name.clone())
            .collect();
        let binding_names: Vec<_> = fleet_tool_bindings(ToolExecutionMode::Inline)
            .into_iter()
            .map(|binding| binding.tool_name)
            .collect();

        assert!(names.contains(&ToolName::new(AGENT_REQUEST_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(PROFILE_LIST_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(PROFILE_READ_TOOL_NAME)));
        assert!(binding_names.contains(&ToolName::new(AGENT_REQUEST_TOOL_NAME)));
        for bundle in bundles {
            let ToolKind::Function(function) = bundle.spec.kind else {
                panic!("fleet tools should be functions");
            };
            assert_eq!(function.strict, Some(false));
        }
    }

    #[test]
    fn disabled_config_produces_no_tools() {
        let bundles = fleet_tool_bundles(&FleetToolsetConfig::disabled()).expect("bundles");
        assert!(bundles.is_empty());
    }
}
