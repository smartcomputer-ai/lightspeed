//! Live universe-environment discovery and session selection tool contracts.

use harness::ToolName;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolResult;

pub const ENVIRONMENT_LIST_TOOL_NAME: &str = "environment_list";
pub const ENVIRONMENT_READ_TOOL_NAME: &str = "environment_read";
pub const ENVIRONMENT_ACTIVATE_TOOL_NAME: &str = "environment_activate";
pub const ENVIRONMENT_DEACTIVATE_TOOL_NAME: &str = "environment_deactivate";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentListArgs {}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EnvironmentReadArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EnvironmentActivateArgs {
    pub environment_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDeactivateArgs {}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvironmentModelView {
    pub environment_id: String,
    pub provider_id: Option<String>,
    pub display_name: Option<String>,
    pub status: Option<String>,
    pub access: String,
    pub default: bool,
    pub working_directory: Option<String>,
    pub active: bool,
    pub observed_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvironmentListOutput {
    pub environments: Vec<EnvironmentModelView>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvironmentActivateOutput {
    pub environment_id: String,
    pub active: bool,
    pub ready: bool,
    pub status: String,
    pub access: String,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvironmentDeactivateOutput {
    pub active: bool,
}

pub fn is_environment_control_tool(tool_id: &ToolName) -> bool {
    matches!(
        tool_id.as_str(),
        "environment.list" | "environment.read" | "environment.activate" | "environment.deactivate"
    )
}

pub fn is_environment_selection_tool(tool_id: &ToolName) -> bool {
    matches!(
        tool_id.as_str(),
        "environment.activate" | "environment.deactivate"
    )
}

pub fn environment_control_tool_definitions(
    selection: bool,
) -> ToolResult<Vec<crate::runtime::FunctionDefinition>> {
    let mut tools = vec![(
        ENVIRONMENT_READ_TOOL_NAME,
        "Read live details and this session's access for an environment. Omit environment_id to inspect the active environment; provide an attached environment reference (short handle or full ID) to inspect another environment.",
        optional_environment_id_schema(),
    )];
    if selection {
        tools.extend([
            (
                ENVIRONMENT_LIST_TOOL_NAME,
                "List the environments attached to this session with their status, this session's access on each, and which one is active.",
                empty_schema(),
            ),
            (
                ENVIRONMENT_ACTIVATE_TOOL_NAME,
                "Select one attached environment as this session's active environment using its short handle or full ID. The tool surface does not change; calls outside the active environment's access are rejected. Environment-dependent tools must be called in a later turn.",
                required_environment_id_schema(),
            ),
            (
                ENVIRONMENT_DEACTIVATE_TOOL_NAME,
                "Clear this session's active environment without closing or changing the universe environment.",
                empty_schema(),
            ),
        ]);
    }
    tools
        .into_iter()
        .map(|(name, description, schema)| function_definition(name, description, schema))
        .collect()
}

fn function_definition(
    name: &'static str,
    description: &'static str,
    schema: Value,
) -> ToolResult<crate::runtime::FunctionDefinition> {
    let output_schema = match name {
        ENVIRONMENT_READ_TOOL_NAME => {
            crate::definitions::output_schema_for::<EnvironmentModelView>()
        }
        ENVIRONMENT_LIST_TOOL_NAME => {
            crate::definitions::output_schema_for::<EnvironmentListOutput>()
        }
        ENVIRONMENT_ACTIVATE_TOOL_NAME => {
            crate::definitions::output_schema_for::<EnvironmentActivateOutput>()
        }
        ENVIRONMENT_DEACTIVATE_TOOL_NAME => {
            crate::definitions::output_schema_for::<EnvironmentDeactivateOutput>()
        }
        _ => unreachable!("owned environment control tool"),
    };
    Ok(
        crate::runtime::FunctionDefinition::new(name, description, schema)
            .with_output_schema(output_schema),
    )
}

fn optional_environment_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "environment_id": { "type": ["string", "null"], "minLength": 1 }
        },
        "additionalProperties": false
    })
}

fn required_environment_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "environment_id": { "type": "string", "minLength": 1 }
        },
        "required": ["environment_id"],
        "additionalProperties": false
    })
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_surface_separates_always_on_read_from_selection_tools() {
        let read_only = environment_control_tool_definitions(false).expect("read tool bundle");
        assert_eq!(read_only.len(), 1);
        assert_eq!(read_only[0].name.as_str(), ENVIRONMENT_READ_TOOL_NAME);

        let bundles = environment_control_tool_definitions(true).expect("control tool bundles");
        assert_eq!(
            bundles
                .iter()
                .map(|bundle| bundle.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                ENVIRONMENT_READ_TOOL_NAME,
                ENVIRONMENT_LIST_TOOL_NAME,
                ENVIRONMENT_ACTIVATE_TOOL_NAME,
                ENVIRONMENT_DEACTIVATE_TOOL_NAME,
            ]
        );
    }

    #[test]
    fn read_arguments_default_to_the_active_environment() {
        let args: EnvironmentReadArgs = serde_json::from_value(json!({})).expect("read args");
        assert_eq!(args.environment_id, None);
        let explicit: EnvironmentReadArgs =
            serde_json::from_value(json!({ "environment_id": "environment_1" }))
                .expect("explicit read args");
        assert_eq!(explicit.environment_id.as_deref(), Some("environment_1"));
    }

    #[test]
    fn selection_classification_excludes_discovery_tools() {
        assert!(!is_environment_selection_tool(&ToolName::new(
            "environment.list",
        )));
        assert!(!is_environment_selection_tool(&ToolName::new(
            "environment.read",
        )));
        assert!(is_environment_selection_tool(&ToolName::new(
            "environment.activate",
        )));
        assert!(is_environment_selection_tool(&ToolName::new(
            "environment.deactivate",
        )));
    }
}
