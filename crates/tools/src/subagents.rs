//! Sub-agent tool contracts: `agent_run` (joined completion, result inline)
//! and `agent_spawn` (promise completion, joined with `await`). Both are
//! system workflow-tool bindings whose start-on-call recipe runs the
//! sub-agent execution workflow; the session runtime only admits the call,
//! pins the grant, and hands the invocation to the generic start machinery.

use crate::catalog::{
    SUBAGENT_CATALOG_CONTEXT_KEY, catalog_context_input, catalog_publication_command,
};
use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand, SubagentLimits,
    storage::{BlobStore, BlobStoreError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{ToolError, ToolResult};

pub const AGENT_RUN_TOOL_NAME: &str = "agent_run";
pub const AGENT_SPAWN_TOOL_NAME: &str = "agent_spawn";
pub const AGENT_RUN_WORKFLOW_TOOL_ID: &str = "subagent-run";
pub const AGENT_SPAWN_WORKFLOW_TOOL_ID: &str = "subagent-spawn";
pub const AGENT_RUN_WORKFLOW_SEMANTIC_TYPE: &str = "lightspeed.subagent.run.v1";
pub const AGENT_SPAWN_WORKFLOW_SEMANTIC_TYPE: &str = "lightspeed.subagent.spawn.v1";
pub const SUBAGENT_WORKFLOW_TYPE: &str = "SubagentExecutionWorkflow";
pub const MAX_SUBAGENT_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_SUBAGENT_LABEL_BYTES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentToolKind {
    Run,
    Spawn,
}

impl SubagentToolKind {
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Run => AGENT_RUN_TOOL_NAME,
            Self::Spawn => AGENT_SPAWN_TOOL_NAME,
        }
    }

    pub fn workflow_tool_id(self) -> &'static str {
        match self {
            Self::Run => AGENT_RUN_WORKFLOW_TOOL_ID,
            Self::Spawn => AGENT_SPAWN_WORKFLOW_TOOL_ID,
        }
    }

    pub fn semantic_type(self) -> &'static str {
        match self {
            Self::Run => AGENT_RUN_WORKFLOW_SEMANTIC_TYPE,
            Self::Spawn => AGENT_SPAWN_WORKFLOW_SEMANTIC_TYPE,
        }
    }

    pub fn from_binding(tool_id: &str, semantic_type: &str) -> Option<Self> {
        match (tool_id, semantic_type) {
            (AGENT_RUN_WORKFLOW_TOOL_ID, AGENT_RUN_WORKFLOW_SEMANTIC_TYPE) => Some(Self::Run),
            (AGENT_SPAWN_WORKFLOW_TOOL_ID, AGENT_SPAWN_WORKFLOW_SEMANTIC_TYPE) => Some(Self::Spawn),
            _ => None,
        }
    }
}

pub fn is_subagent_workflow_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        AGENT_RUN_WORKFLOW_TOOL_ID | AGENT_SPAWN_WORKFLOW_TOOL_ID
    )
}

/// Model arguments of `agent_run` and `agent_spawn`; identical on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCallArgs {
    /// Profile id from the session's sub-agent catalog.
    pub agent: String,
    /// The complete brief; becomes the child's first user message.
    pub input: String,
    /// Human-readable name shown in the sessions tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl AgentCallArgs {
    pub fn validate(&self) -> ToolResult<()> {
        if self.agent.trim().is_empty() {
            return Err(ToolError::InvalidRequest {
                message: "agent must name a profile from the sub-agent catalog".to_owned(),
            });
        }
        if self.input.trim().is_empty() {
            return Err(ToolError::InvalidRequest {
                message: "input must be a non-empty brief".to_owned(),
            });
        }
        if self.input.len() > MAX_SUBAGENT_INPUT_BYTES {
            return Err(ToolError::InvalidRequest {
                message: format!("input must be at most {MAX_SUBAGENT_INPUT_BYTES} bytes"),
            });
        }
        if let Some(label) = &self.label
            && (label.trim().is_empty() || label.len() > MAX_SUBAGENT_LABEL_BYTES)
        {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "label must be non-empty and at most {MAX_SUBAGENT_LABEL_BYTES} bytes"
                ),
            });
        }
        Ok(())
    }
}

/// Runtime-owned facts pinned when a sub-agent call is admitted: the grant
/// as admitted for the batch and the parent identity. The execution's
/// prepare activity reads this through the invocation's opaque execution
/// context; it is not part of the model-facing schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentExecutionContextV1 {
    pub version: u32,
    pub parent_session_id: String,
    pub parent_run_id: u64,
    pub agent_profile_id: String,
    /// The parent's grant limits at admission; the prepare activity
    /// attenuates them by the parent's own origin.
    pub grant_limits: SubagentLimits,
}

impl SubagentExecutionContextV1 {
    pub const VERSION: u32 = 1;

    pub fn new(
        parent_session_id: String,
        parent_run_id: u64,
        agent_profile_id: String,
        grant_limits: SubagentLimits,
    ) -> Self {
        Self {
            version: Self::VERSION,
            parent_session_id,
            parent_run_id,
            agent_profile_id,
            grant_limits,
        }
    }
}

/// The child's result as the parent sees it: inline for `agent_run`, as the
/// `await` payload for `agent_spawn`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubagentResultEnvelope {
    pub agent: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: SubagentResultStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Media the child referenced in `output` by `media:` handle, resolved
    /// against what the child saw. The parent's runtime appends each as a
    /// media entry after the result, so the parent sees it and can use the
    /// same handle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<engine::media::MediaDescriptor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentResultStatus {
    Completed,
    Failed,
    Cancelled,
    Deadline,
}

pub const SUBAGENT_CATALOG_SCHEMA_VERSION: &str = "lightspeed.subagents.catalog.v1";

/// The agent menu as the model sees it: the grant's allowlist joined with
/// the current profile records. Published as a generic `Catalog` context
/// entry and refreshed like the skill catalog (before each run on an idle
/// session, and on idle API reads), so description edits land at the next
/// run while the pinned profile revision still governs each child.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentCatalogSnapshot {
    pub schema_version: String,
    pub agents: Vec<SubagentCatalogAgent>,
    pub limits: SubagentLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentCatalogAgent {
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Absent when the allowlisted profile no longer exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl SubagentCatalogSnapshot {
    pub fn new(agents: Vec<SubagentCatalogAgent>, limits: SubagentLimits) -> Self {
        Self {
            schema_version: SUBAGENT_CATALOG_SCHEMA_VERSION.to_owned(),
            agents,
            limits,
        }
    }
}

pub async fn subagent_catalog_context_input(
    blobs: &dyn BlobStore,
    snapshot: &SubagentCatalogSnapshot,
    snapshot_ref: BlobRef,
) -> Result<ContextEntryInput, BlobStoreError> {
    catalog_context_input(
        blobs,
        "Sub-agent catalog",
        crate::subagent_catalog_text::subagent_catalog_text(snapshot),
        snapshot_ref,
    )
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum SubagentCatalogError {
    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("failed to encode subagent catalog: {message}")]
    Encode { message: String },
}

/// Write the snapshot to CAS and return the upsert when it differs from the
/// active entry (content-addressed, so an unchanged catalog is a no-op).
pub async fn prepare_subagent_catalog_publication(
    blobs: &dyn BlobStore,
    current: Option<&ContextEntryInput>,
    snapshot: &SubagentCatalogSnapshot,
) -> Result<Option<CoreAgentCommand>, SubagentCatalogError> {
    let bytes = serde_json::to_vec(snapshot).map_err(|error| SubagentCatalogError::Encode {
        message: error.to_string(),
    })?;
    let catalog_ref = blobs.put_bytes(bytes).await?;
    let entry = subagent_catalog_context_input(blobs, snapshot, catalog_ref).await?;
    Ok(catalog_publication_command(
        current,
        SUBAGENT_CATALOG_CONTEXT_KEY,
        entry,
    ))
}

pub fn subagent_tool_definition(
    kind: SubagentToolKind,
) -> ToolResult<crate::runtime::FunctionDefinition> {
    let description = match kind {
        SubagentToolKind::Run => {
            "Run a sub-agent from the sub-agent catalog with a complete brief and return its result when it finishes. Several agent_run calls in one turn run concurrently and return together."
        }
        SubagentToolKind::Spawn => {
            "Start a sub-agent from the sub-agent catalog with a complete brief and return a promise immediately. Join it later with await (any/all/timeout); cancel closes the child."
        }
    };
    Ok(crate::runtime::FunctionDefinition::new(
        kind.tool_name(),
        description,
        agent_call_input_schema(),
    ))
}

fn agent_call_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agent": {
                "type": "string",
                "description": "Profile id of the sub-agent, from the sub-agent catalog in your context."
            },
            "input": {
                "type": "string",
                "description": "The complete brief. The sub-agent sees only this text plus its own profile instructions; include everything it needs."
            },
            "label": {
                "type": "string",
                "description": "Short human-readable name for this delegation, shown in the sessions tree."
            }
        },
        "required": ["agent", "input"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_reject_blank_agent_and_input_and_unknown_fields() {
        let ok: AgentCallArgs = serde_json::from_value(json!({
            "agent": "reviewer",
            "input": "review PR 1234",
            "label": "reviewer: PR 1234"
        }))
        .expect("decode");
        ok.validate().expect("valid");
        let blank: AgentCallArgs =
            serde_json::from_value(json!({ "agent": " ", "input": "x" })).expect("decode");
        assert!(matches!(
            blank.validate(),
            Err(ToolError::InvalidRequest { .. })
        ));
        assert!(
            serde_json::from_value::<AgentCallArgs>(json!({
                "agent": "reviewer",
                "input": "x",
                "wait": true
            }))
            .is_err()
        );
    }

    #[test]
    fn binding_identity_round_trips() {
        for kind in [SubagentToolKind::Run, SubagentToolKind::Spawn] {
            assert_eq!(
                SubagentToolKind::from_binding(kind.workflow_tool_id(), kind.semantic_type()),
                Some(kind)
            );
            let bundle = subagent_tool_definition(kind).expect("bundle");
            assert_eq!(bundle.name.as_str(), kind.tool_name());
        }
        assert_eq!(
            SubagentToolKind::from_binding(AGENT_RUN_WORKFLOW_TOOL_ID, "other"),
            None
        );
    }
}
