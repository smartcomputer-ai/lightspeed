//! Generic promise/concurrency tool contracts.

use engine::{
    FunctionToolSpec, ToolKind, ToolName, ToolParallelism, ToolSpec, ToolTargetRequirement,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::{ToolBinding, ToolDocument, ToolExecutionMode, ToolSpecBundle},
};

pub const AWAIT_TOOL_NAME: &str = "await";
pub const CANCEL_TOOL_NAME: &str = "cancel";
pub const DETACH_TOOL_NAME: &str = "detach";
pub const SLEEP_TOOL_NAME: &str = "sleep";

pub const CONCURRENCY_LOGICAL_ID_PREFIX: &str = "concurrency.";
pub const CONCURRENCY_ACTIVITY_TYPE: &str = "lightspeed.concurrency";

pub const MAX_AWAIT_PROMISES: usize = 32;
pub const MAX_CANCEL_PROMISES: usize = 32;
pub const MAX_DETACH_PROMISES: usize = 32;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConcurrencyToolsetConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub timer: bool,
}

impl ConcurrencyToolsetConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            timer: false,
        }
    }

    pub fn enabled() -> Self {
        Self {
            enabled: true,
            timer: false,
        }
    }

    pub fn timer() -> Self {
        Self {
            enabled: true,
            timer: true,
        }
    }

    pub fn enabled_or_timer(&self) -> bool {
        self.enabled || self.timer
    }
}

pub fn is_concurrency_tool(tool_name: &ToolName) -> bool {
    matches!(
        tool_name.as_str(),
        AWAIT_TOOL_NAME | CANCEL_TOOL_NAME | DETACH_TOOL_NAME | SLEEP_TOOL_NAME
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AwaitArgs {
    #[serde(default)]
    pub promises: Vec<String>,
    #[serde(default)]
    pub mode: AwaitModeArg,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mailbox: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl AwaitArgs {
    /// Validate and dedupe the promise id list: 1..=32 non-empty ids,
    /// duplicates collapsed in first-occurrence order.
    pub fn validated_promise_ids(&self) -> ToolResult<Vec<String>> {
        if self.promises.is_empty() && !self.mailbox {
            return Err(ToolError::InvalidRequest {
                message: "await requires at least one promise id or mailbox=true".to_owned(),
            });
        }
        if self.promises.len() > MAX_AWAIT_PROMISES {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "await promises must contain at most {MAX_AWAIT_PROMISES} promise ids"
                ),
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut promise_ids = Vec::with_capacity(self.promises.len());
        for promise_id in &self.promises {
            if promise_id.trim().is_empty() {
                return Err(ToolError::InvalidRequest {
                    message: "await promise ids must be non-empty strings".to_owned(),
                });
            }
            if seen.insert(promise_id.clone()) {
                promise_ids.push(promise_id.clone());
            }
        }
        Ok(promise_ids)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitModeArg {
    #[default]
    All,
    Any,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CancelArgs {
    pub promises: Vec<String>,
}

impl CancelArgs {
    pub fn validated_promise_ids(&self) -> ToolResult<Vec<String>> {
        validated_non_empty_promise_ids(&self.promises, MAX_CANCEL_PROMISES, "cancel")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DetachArgs {
    pub promises: Vec<String>,
}

impl DetachArgs {
    pub fn validated_promise_ids(&self) -> ToolResult<Vec<String>> {
        validated_non_empty_promise_ids(&self.promises, MAX_DETACH_PROMISES, "detach")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SleepArgs {
    pub ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CancelOutput {
    #[serde(default)]
    pub promises: Vec<CancelPromiseOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CancelPromiseOutput {
    pub promise_id: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DetachOutput {
    #[serde(default)]
    pub promises: Vec<DetachPromiseOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DetachPromiseOutput {
    pub promise_id: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SleepOutput {
    pub promise: String,
    pub fire_at_ms: u64,
}

pub fn cancel_promises_model_visible_text(output: &CancelOutput) -> String {
    if output.promises.is_empty() {
        return "No promises cancelled.".to_owned();
    }
    output
        .promises
        .iter()
        .map(|promise| format!("{}: {}", promise.promise_id, promise.status))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn detach_promises_model_visible_text(output: &DetachOutput) -> String {
    if output.promises.is_empty() {
        return "No promises detached.".to_owned();
    }
    output
        .promises
        .iter()
        .map(|promise| format!("{}: {}", promise.promise_id, promise.status))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn sleep_model_visible_text(output: &SleepOutput, ms: u64) -> String {
    format!(
        "Timer scheduled for {ms} ms (promise {}). Await it with the await tool.",
        output.promise
    )
}

pub fn concurrency_tool_bundles(
    config: &ConcurrencyToolsetConfig,
) -> ToolResult<Vec<ToolSpecBundle>> {
    if !config.enabled_or_timer() {
        return Ok(Vec::new());
    }
    let mut bundles = vec![
        function_bundle(
            AWAIT_TOOL_NAME,
            "Park this run until the listed promises settle or, with mailbox=true, until the next inbound message. Timeout returns a partial snapshot; remaining promises stay pending and re-awaitable.",
            await_input_schema(),
        )?,
        function_bundle(
            CANCEL_TOOL_NAME,
            "Revoke pending promises held by this run. Cancellation is best-effort at the source and late source completions become no-ops.",
            promise_cancel_input_schema(),
        )?,
        function_bundle(
            DETACH_TOOL_NAME,
            "Promote pending promises held by this run to session scope so they survive this run's terminal state.",
            promise_detach_input_schema(),
        )?,
    ];
    if config.timer {
        bundles.push(function_bundle(
            SLEEP_TOOL_NAME,
            "Create a timer promise that resolves after the requested delay. Use await to park on the returned promise.",
            sleep_input_schema(),
        )?);
    }
    Ok(bundles)
}

pub fn concurrency_tool_bindings(
    execution: ToolExecutionMode,
    config: &ConcurrencyToolsetConfig,
) -> Vec<ToolBinding> {
    if !config.enabled_or_timer() {
        return Vec::new();
    }
    let mut tool_names = vec![AWAIT_TOOL_NAME, CANCEL_TOOL_NAME, DETACH_TOOL_NAME];
    if config.timer {
        tool_names.push(SLEEP_TOOL_NAME);
    }
    tool_names
        .into_iter()
        .map(|tool_name| concurrency_tool_binding(tool_name, execution.clone()))
        .collect()
}

fn concurrency_tool_binding(tool_name: &str, execution: ToolExecutionMode) -> ToolBinding {
    ToolBinding::new(
        ToolName::new(tool_name),
        format!("{CONCURRENCY_LOGICAL_ID_PREFIX}{tool_name}"),
        CONCURRENCY_ACTIVITY_TYPE,
        execution,
        ToolParallelism::Exclusive,
    )
}

fn validated_non_empty_promise_ids(
    promises: &[String],
    max_promises: usize,
    tool_name: &str,
) -> ToolResult<Vec<String>> {
    if promises.is_empty() {
        return Err(ToolError::InvalidRequest {
            message: format!("{tool_name} promises must contain at least one promise id"),
        });
    }
    if promises.len() > max_promises {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "{tool_name} promises must contain at most {max_promises} promise ids"
            ),
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut promise_ids = Vec::with_capacity(promises.len());
    for promise_id in promises {
        if promise_id.trim().is_empty() {
            return Err(ToolError::InvalidRequest {
                message: format!("{tool_name} promise ids must be non-empty strings"),
            });
        }
        if seen.insert(promise_id.clone()) {
            promise_ids.push(promise_id.clone());
        }
    }
    Ok(promise_ids)
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

fn await_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "promises": {
                "type": "array",
                "maxItems": MAX_AWAIT_PROMISES,
                "items": {
                    "type": "string",
                    "description": "Promise id returned by a promise-creating tool such as agent_spawn, job_start, or sleep."
                },
                "description": "Promise ids to park on. May be empty when mailbox is true."
            },
            "mode": {
                "type": "string",
                "enum": ["all", "any"],
                "default": "all",
                "description": "all waits for every promise; any wakes on the first terminal one."
            },
            "timeout_ms": {
                "type": ["integer", "null"],
                "minimum": 0,
                "description": "Optional timeout in milliseconds. On timeout the call returns a partial snapshot and the remaining promises stay pending and re-awaitable. Omit for an indefinite wait."
            },
            "mailbox": {
                "type": "boolean",
                "default": false,
                "description": "When true, also wake on the next inbound message instead of queueing that message as a separate run."
            }
        },
        "required": ["promises"],
        "additionalProperties": false
    })
}

fn promise_cancel_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "promises": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_CANCEL_PROMISES,
                "items": {
                    "type": "string",
                    "description": "Promise id to revoke."
                },
                "description": "Promise ids to cancel."
            }
        },
        "required": ["promises"],
        "additionalProperties": false
    })
}

fn promise_detach_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "promises": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_DETACH_PROMISES,
                "items": {
                    "type": "string",
                    "description": "Promise id to detach."
                },
                "description": "Promise ids to promote to session scope."
            }
        },
        "required": ["promises"],
        "additionalProperties": false
    })
}

fn sleep_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ms": {
                "type": "integer",
                "minimum": 0,
                "description": "Delay in milliseconds before the timer promise resolves."
            }
        },
        "required": ["ms"],
        "additionalProperties": false
    })
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn await_accepts_promises_mode_and_timeout() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": ["promise_a", "promise_b"],
            "mode": "any",
            "timeout_ms": 1000
        }))
        .expect("decode await args");

        assert_eq!(args.promises, vec!["promise_a", "promise_b"]);
        assert_eq!(args.mode, AwaitModeArg::Any);
        assert!(!args.mailbox);
        assert_eq!(args.timeout_ms, Some(1000));
    }

    #[test]
    fn await_accepts_mailbox_only() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": [],
            "mailbox": true
        }))
        .expect("decode await args");

        assert!(args.promises.is_empty());
        assert!(args.mailbox);
        assert!(args.validated_promise_ids().expect("valid").is_empty());
    }

    #[test]
    fn await_defaults_to_all_mode_without_timeout() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": ["promise_a"]
        }))
        .expect("decode await args");

        assert_eq!(args.mode, AwaitModeArg::All);
        assert_eq!(args.timeout_ms, None);
    }

    #[test]
    fn await_rejects_unknown_fields() {
        serde_json::from_value::<AwaitArgs>(json!({
            "promises": ["promise_a"],
            "until": "activity"
        }))
        .expect_err("unknown fields are denied");
    }

    #[test]
    fn await_validation_dedupes_and_preserves_order() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": ["promise_b", "promise_a", "promise_b"]
        }))
        .expect("decode await args");

        assert_eq!(
            args.validated_promise_ids().expect("validated ids"),
            vec!["promise_b", "promise_a"]
        );
    }

    #[test]
    fn await_validation_rejects_empty_list_and_blank_ids() {
        let empty: AwaitArgs =
            serde_json::from_value(json!({ "promises": [] })).expect("decode await args");
        assert!(matches!(
            empty.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));

        let blank: AwaitArgs =
            serde_json::from_value(json!({ "promises": [" "] })).expect("decode await args");
        assert!(matches!(
            blank.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn await_validation_rejects_too_many_promises() {
        let promises: Vec<String> = (0..=MAX_AWAIT_PROMISES)
            .map(|index| format!("promise_{index}"))
            .collect();
        let args: AwaitArgs =
            serde_json::from_value(json!({ "promises": promises })).expect("decode await args");
        assert!(matches!(
            args.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn detach_validation_dedupes_and_preserves_order() {
        let args: DetachArgs = serde_json::from_value(json!({
            "promises": ["promise_b", "promise_a", "promise_b"]
        }))
        .expect("decode detach args");

        assert_eq!(
            args.validated_promise_ids().expect("validated ids"),
            vec!["promise_b", "promise_a"]
        );
    }

    #[test]
    fn detach_validation_rejects_empty_list_and_blank_ids() {
        let empty: DetachArgs =
            serde_json::from_value(json!({ "promises": [] })).expect("decode detach args");
        assert!(matches!(
            empty.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));

        let blank: DetachArgs =
            serde_json::from_value(json!({ "promises": [" "] })).expect("decode detach args");
        assert!(matches!(
            blank.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn sleep_accepts_zero_delay() {
        let args: SleepArgs =
            serde_json::from_value(json!({ "ms": 0 })).expect("decode sleep args");

        assert_eq!(args.ms, 0);
    }

    #[test]
    fn timer_config_adds_sleep_and_base_tools() {
        let bundles =
            concurrency_tool_bundles(&ConcurrencyToolsetConfig::timer()).expect("bundles");
        let names = bundles
            .into_iter()
            .map(|bundle| bundle.spec.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&ToolName::new(AWAIT_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(CANCEL_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(DETACH_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(SLEEP_TOOL_NAME)));
    }
}
