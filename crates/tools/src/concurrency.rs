//! Generic promise/concurrency tool contracts.

use harness::{
    PromiseControlCallRuntime, PromiseControlStateRuntime, PromiseId, PromiseOwnership,
    PromiseScope, PromiseStatus, RunId, ToolEffect, ToolName, promise_cancel_effect,
    promise_detach_effect,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::error::{ToolError, ToolResult};

pub const AWAIT_TOOL_NAME: &str = "await";
pub const CANCEL_TOOL_NAME: &str = "cancel";
pub const DETACH_TOOL_NAME: &str = "detach";
pub const SLEEP_TOOL_NAME: &str = "sleep";

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
        "concurrency.await" | "concurrency.cancel" | "concurrency.detach" | "concurrency.sleep"
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AwaitArgs {
    #[serde(default)]
    pub promises: Vec<String>,
    #[serde(default)]
    pub mode: AwaitModeArg,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Canonical model-visible value written by the await materializer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AwaitOutput {
    pub outcome: harness::AwaitOutcome,
    #[serde(default)]
    pub results: Vec<AwaitPromiseOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AwaitPromiseOutput {
    pub promise_id: String,
    /// pending, resolved, failed, cancelled, or unknown. A timeout may leave
    /// pending promises that can be awaited again.
    pub status: String,
    /// Resolved producer payload as JSON, text, or an opaque artifact reference.
    /// Its tool-specific shape is determined by the promise's producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    /// Failed producer payload, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

impl AwaitArgs {
    /// Validate and dedupe the promise id list: 1..=32 well-formed
    /// `promise_<n>` ids, duplicates collapsed in first-occurrence order.
    pub fn validated_promise_ids(&self) -> ToolResult<Vec<PromiseId>> {
        if self.promises.is_empty() {
            return Err(ToolError::InvalidRequest {
                message: "await requires at least one promise id".to_owned(),
            });
        }
        validated_promise_ids(&self.promises, MAX_AWAIT_PROMISES, "await")
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
    pub fn validated_promise_ids(&self) -> ToolResult<Vec<PromiseId>> {
        validated_non_empty_promise_ids(&self.promises, MAX_CANCEL_PROMISES, "cancel")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DetachArgs {
    pub promises: Vec<String>,
}

impl DetachArgs {
    pub fn validated_promise_ids(&self) -> ToolResult<Vec<PromiseId>> {
        validated_non_empty_promise_ids(&self.promises, MAX_DETACH_PROMISES, "detach")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SleepArgs {
    pub ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CancelOutput {
    #[serde(default)]
    pub promises: Vec<CancelPromiseOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CancelPromiseOutput {
    pub promise_id: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DetachOutput {
    #[serde(default)]
    pub promises: Vec<DetachPromiseOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DetachPromiseOutput {
    pub promise_id: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct PromiseControlError {
    message: String,
}

impl PromiseControlError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn supplied_promise_controls<'a>(
    requested_ids: &[PromiseId],
    runtime: Option<&'a PromiseControlCallRuntime>,
) -> Result<&'a [harness::PromiseControlRuntime], PromiseControlError> {
    let runtime = runtime
        .ok_or_else(|| PromiseControlError::new("promise control runtime facts are missing"))?;
    if runtime.version != PromiseControlCallRuntime::VERSION {
        return Err(PromiseControlError::new(format!(
            "unsupported promise control runtime facts version {}",
            runtime.version
        )));
    }
    if runtime.controls.len() != requested_ids.len()
        || runtime
            .controls
            .iter()
            .zip(requested_ids)
            .any(|(control, requested)| control.promise_id != *requested)
    {
        return Err(PromiseControlError::new(
            "promise control runtime facts do not match the requested promise ids",
        ));
    }
    Ok(&runtime.controls)
}

pub fn cancel_promises_from_runtime(
    args: &CancelArgs,
    runtime: Option<&PromiseControlCallRuntime>,
) -> Result<(CancelOutput, Vec<ToolEffect>), PromiseControlError> {
    let requested_ids = args
        .validated_promise_ids()
        .map_err(|error| PromiseControlError::new(error.to_string()))?;
    let controls = supplied_promise_controls(&requested_ids, runtime)?;
    let mut promises = Vec::with_capacity(controls.len());
    let mut effects = Vec::new();
    for control in controls {
        let promise_id = control.promise_id.as_str().to_owned();
        let PromiseControlStateRuntime::Known {
            ownership,
            promise_status,
            ..
        } = &control.state
        else {
            return Err(PromiseControlError::new(format!(
                "unknown promise {promise_id}"
            )));
        };
        if *ownership != PromiseOwnership::Model {
            return Err(PromiseControlError::new(format!(
                "promise {promise_id} is runtime-owned and cannot be cancelled"
            )));
        }
        if promise_status.is_terminal() {
            promises.push(CancelPromiseOutput {
                promise_id,
                status: promise_status_name(*promise_status).to_owned(),
            });
            continue;
        }
        effects.push(promise_cancel_effect(&control.promise_id));
        promises.push(CancelPromiseOutput {
            promise_id,
            status: "cancelled".to_owned(),
        });
    }
    Ok((CancelOutput { promises }, effects))
}

pub fn detach_promises_from_runtime(
    args: &DetachArgs,
    run_id: RunId,
    runtime: Option<&PromiseControlCallRuntime>,
) -> Result<(DetachOutput, Vec<ToolEffect>), PromiseControlError> {
    let requested_ids = args
        .validated_promise_ids()
        .map_err(|error| PromiseControlError::new(error.to_string()))?;
    let controls = supplied_promise_controls(&requested_ids, runtime)?;
    let mut promises = Vec::with_capacity(controls.len());
    let mut effects = Vec::new();
    for control in controls {
        let promise_id = control.promise_id.as_str().to_owned();
        let PromiseControlStateRuntime::Known {
            ownership,
            scope,
            promise_status,
        } = &control.state
        else {
            return Err(PromiseControlError::new(format!(
                "unknown promise {promise_id}"
            )));
        };
        if *ownership != PromiseOwnership::Model {
            return Err(PromiseControlError::new(format!(
                "promise {promise_id} is runtime-owned and cannot be detached"
            )));
        }
        if promise_status.is_terminal() {
            return Err(PromiseControlError::new(format!(
                "promise {promise_id} is already {}",
                promise_status_name(*promise_status)
            )));
        }
        match scope {
            PromiseScope::Session => promises.push(DetachPromiseOutput {
                promise_id,
                status: "already_detached".to_owned(),
            }),
            PromiseScope::Run {
                run_id: promise_run_id,
            } if *promise_run_id == run_id => {
                effects.push(promise_detach_effect(&control.promise_id));
                promises.push(DetachPromiseOutput {
                    promise_id,
                    status: "detached".to_owned(),
                });
            }
            PromiseScope::Run {
                run_id: promise_run_id,
            } => {
                return Err(PromiseControlError::new(format!(
                    "promise {promise_id} is scoped to run {promise_run_id}, not current run {run_id}",
                )));
            }
        }
    }
    Ok((DetachOutput { promises }, effects))
}

pub fn promise_status_name(status: PromiseStatus) -> &'static str {
    match status {
        PromiseStatus::Pending => "pending",
        PromiseStatus::Resolved => "resolved",
        PromiseStatus::Failed => "failed",
        PromiseStatus::Cancelled => "cancelled",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

pub fn concurrency_tool_definitions(
    config: &ConcurrencyToolsetConfig,
) -> ToolResult<Vec<crate::runtime::FunctionDefinition>> {
    if !config.enabled_or_timer() {
        return Ok(Vec::new());
    }
    let mut definitions = vec![
        function_definition(
            AWAIT_TOOL_NAME,
            "Park this run until the listed promises settle. Timeout returns a partial snapshot; remaining promises stay pending and re-awaitable.",
            await_input_schema(),
        )?,
        function_definition(
            CANCEL_TOOL_NAME,
            "Revoke pending promises held by this run. Cancellation is best-effort at the source and late source completions become no-ops.",
            promise_cancel_input_schema(),
        )?,
        function_definition(
            DETACH_TOOL_NAME,
            "Promote pending promises held by this run to session scope so they survive this run's terminal state.",
            promise_detach_input_schema(),
        )?,
    ];
    if config.timer {
        definitions.push(function_definition(
            SLEEP_TOOL_NAME,
            "Create a timer promise that resolves after the requested delay. Use await to park on the returned promise.",
            sleep_input_schema(),
        )?);
    }
    Ok(definitions)
}

fn validated_non_empty_promise_ids(
    promises: &[String],
    max_promises: usize,
    tool_name: &str,
) -> ToolResult<Vec<PromiseId>> {
    if promises.is_empty() {
        return Err(ToolError::InvalidRequest {
            message: format!("{tool_name} promises must contain at least one promise id"),
        });
    }
    validated_promise_ids(promises, max_promises, tool_name)
}

/// Parse the model's promise id list: every entry must be a `promise_<n>`
/// handle as returned by a promise-creating tool. A malformed entry is an
/// ordinary tool error, so a mistyped id costs the model one turn and
/// never reaches the harness.
fn validated_promise_ids(
    promises: &[String],
    max_promises: usize,
    tool_name: &str,
) -> ToolResult<Vec<PromiseId>> {
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
        let promise_id =
            PromiseId::try_new(promise_id.clone()).map_err(|error| ToolError::InvalidRequest {
                message: format!(
                    "{tool_name} promise ids must be the promise_<n> handles returned by promise-creating tools: {error}"
                ),
            })?;
        if seen.insert(promise_id.clone()) {
            promise_ids.push(promise_id);
        }
    }
    Ok(promise_ids)
}

fn function_definition(
    tool_name: &'static str,
    description: &'static str,
    input_schema: Value,
) -> ToolResult<crate::runtime::FunctionDefinition> {
    let definition = crate::runtime::FunctionDefinition::new(tool_name, description, input_schema);
    let schema = match tool_name {
        CANCEL_TOOL_NAME => crate::definitions::output_schema_for::<CancelOutput>(),
        DETACH_TOOL_NAME => crate::definitions::output_schema_for::<DetachOutput>(),
        SLEEP_TOOL_NAME => crate::definitions::output_schema_for::<SleepOutput>(),
        AWAIT_TOOL_NAME => crate::definitions::output_schema_for::<AwaitOutput>(),
        _ => return Ok(definition),
    };
    Ok(definition.with_output_schema(schema))
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
                    "description": "Promise handle (promise_<n>) returned by a promise-creating tool such as agent_spawn, job_submit, or sleep."
                },
                "description": "Promise ids to park on."
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
                    "description": "Promise handle (promise_<n>) to revoke."
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
                    "description": "Promise handle (promise_<n>) to detach."
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn known_control(
        id: &str,
        ownership: PromiseOwnership,
        scope: PromiseScope,
        promise_status: PromiseStatus,
    ) -> harness::PromiseControlRuntime {
        harness::PromiseControlRuntime {
            promise_id: harness::PromiseId::new(id),
            state: PromiseControlStateRuntime::Known {
                ownership,
                scope,
                promise_status,
            },
        }
    }

    fn runtime(controls: Vec<harness::PromiseControlRuntime>) -> PromiseControlCallRuntime {
        PromiseControlCallRuntime::v1(controls)
    }

    #[test]
    fn cancel_uses_supplied_status_and_ownership_facts() {
        let args = CancelArgs {
            promises: vec!["promise_1".to_owned(), "promise_2".to_owned()],
        };
        let facts = runtime(vec![
            known_control(
                "promise_1",
                PromiseOwnership::Model,
                PromiseScope::Run {
                    run_id: RunId::new(1),
                },
                PromiseStatus::Pending,
            ),
            known_control(
                "promise_2",
                PromiseOwnership::Model,
                PromiseScope::Session,
                PromiseStatus::Resolved,
            ),
        ]);
        let (output, effects) =
            cancel_promises_from_runtime(&args, Some(&facts)).expect("cancel evaluation");
        assert_eq!(
            output.promises,
            vec![
                CancelPromiseOutput {
                    promise_id: "promise_1".to_owned(),
                    status: "cancelled".to_owned(),
                },
                CancelPromiseOutput {
                    promise_id: "promise_2".to_owned(),
                    status: "resolved".to_owned(),
                },
            ]
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, harness::PROMISE_CANCEL_EFFECT_KIND);

        let runtime_owned = runtime(vec![known_control(
            "promise_1",
            PromiseOwnership::Runtime,
            PromiseScope::Session,
            PromiseStatus::Pending,
        )]);
        assert_eq!(
            cancel_promises_from_runtime(
                &CancelArgs {
                    promises: vec!["promise_1".to_owned()],
                },
                Some(&runtime_owned),
            )
            .expect_err("runtime-owned rejection")
            .to_string(),
            "promise promise_1 is runtime-owned and cannot be cancelled"
        );
    }

    #[test]
    fn promise_control_rejects_unknown_missing_and_mismatched_facts() {
        let args = CancelArgs {
            promises: vec!["promise_99".to_owned()],
        };
        let unknown = runtime(vec![harness::PromiseControlRuntime {
            promise_id: harness::PromiseId::new("promise_99"),
            state: PromiseControlStateRuntime::Unknown,
        }]);
        assert_eq!(
            cancel_promises_from_runtime(&args, Some(&unknown))
                .expect_err("unknown rejection")
                .to_string(),
            "unknown promise promise_99"
        );
        assert_eq!(
            cancel_promises_from_runtime(&args, None)
                .expect_err("missing runtime rejection")
                .to_string(),
            "promise control runtime facts are missing"
        );
        let mismatched = runtime(vec![harness::PromiseControlRuntime {
            promise_id: harness::PromiseId::new("promise_98"),
            state: PromiseControlStateRuntime::Unknown,
        }]);
        assert_eq!(
            cancel_promises_from_runtime(&args, Some(&mismatched))
                .expect_err("mismatch rejection")
                .to_string(),
            "promise control runtime facts do not match the requested promise ids"
        );
    }

    #[test]
    fn detach_uses_supplied_scope_status_and_ownership_facts() {
        let args = DetachArgs {
            promises: vec!["promise_1".to_owned(), "promise_2".to_owned()],
        };
        let facts = runtime(vec![
            known_control(
                "promise_1",
                PromiseOwnership::Model,
                PromiseScope::Session,
                PromiseStatus::Pending,
            ),
            known_control(
                "promise_2",
                PromiseOwnership::Model,
                PromiseScope::Run {
                    run_id: RunId::new(7),
                },
                PromiseStatus::Pending,
            ),
        ]);
        let (output, effects) = detach_promises_from_runtime(&args, RunId::new(7), Some(&facts))
            .expect("detach evaluation");
        assert_eq!(
            output.promises,
            vec![
                DetachPromiseOutput {
                    promise_id: "promise_1".to_owned(),
                    status: "already_detached".to_owned(),
                },
                DetachPromiseOutput {
                    promise_id: "promise_2".to_owned(),
                    status: "detached".to_owned(),
                },
            ]
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, harness::PROMISE_DETACH_EFFECT_KIND);

        for (facts, expected) in [
            (
                known_control(
                    "promise_1",
                    PromiseOwnership::Runtime,
                    PromiseScope::Session,
                    PromiseStatus::Pending,
                ),
                "promise promise_1 is runtime-owned and cannot be detached".to_owned(),
            ),
            (
                known_control(
                    "promise_1",
                    PromiseOwnership::Model,
                    PromiseScope::Session,
                    PromiseStatus::Failed,
                ),
                "promise promise_1 is already failed".to_owned(),
            ),
            (
                known_control(
                    "promise_1",
                    PromiseOwnership::Model,
                    PromiseScope::Run {
                        run_id: RunId::new(8),
                    },
                    PromiseStatus::Pending,
                ),
                "promise promise_1 is scoped to run 8, not current run 7".to_owned(),
            ),
        ] {
            let error = detach_promises_from_runtime(
                &DetachArgs {
                    promises: vec!["promise_1".to_owned()],
                },
                RunId::new(7),
                Some(&runtime(vec![facts])),
            )
            .expect_err("detach rejection");
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn await_accepts_promises_mode_and_timeout() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": ["promise_1", "promise_2"],
            "mode": "any",
            "timeout_ms": 1000
        }))
        .expect("decode await args");

        assert_eq!(args.promises, vec!["promise_1", "promise_2"]);
        assert_eq!(args.mode, AwaitModeArg::Any);
        assert_eq!(args.timeout_ms, Some(1000));
    }

    #[test]
    fn await_defaults_to_all_mode_without_timeout() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": ["promise_1"]
        }))
        .expect("decode await args");

        assert_eq!(args.mode, AwaitModeArg::All);
        assert_eq!(args.timeout_ms, None);
    }

    #[test]
    fn await_rejects_unknown_fields() {
        serde_json::from_value::<AwaitArgs>(json!({
            "promises": ["promise_1"],
            "until": "activity"
        }))
        .expect_err("unknown fields are denied");
    }

    #[test]
    fn await_validation_dedupes_and_preserves_order() {
        let args: AwaitArgs = serde_json::from_value(json!({
            "promises": ["promise_2", "promise_1", "promise_2"]
        }))
        .expect("decode await args");

        assert_eq!(
            args.validated_promise_ids().expect("validated ids"),
            vec![PromiseId::new("promise_2"), PromiseId::new("promise_1")]
        );
    }

    #[test]
    fn await_validation_rejects_malformed_ids() {
        let malformed: AwaitArgs = serde_json::from_value(json!({
            "promises": ["wtp:sha256:0000", "promise_7"]
        }))
        .expect("decode await args");
        assert!(matches!(
            malformed.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));
        let leading_zero: CancelArgs =
            serde_json::from_value(json!({ "promises": ["promise_07"] })).expect("decode");
        assert!(matches!(
            leading_zero.validated_promise_ids(),
            Err(ToolError::InvalidRequest { .. })
        ));
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
            "promises": ["promise_2", "promise_1", "promise_2"]
        }))
        .expect("decode detach args");

        assert_eq!(
            args.validated_promise_ids().expect("validated ids"),
            vec![PromiseId::new("promise_2"), PromiseId::new("promise_1")]
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
            concurrency_tool_definitions(&ConcurrencyToolsetConfig::timer()).expect("bundles");
        let names = bundles
            .into_iter()
            .map(|bundle| bundle.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&ToolName::new(AWAIT_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(CANCEL_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(DETACH_TOOL_NAME)));
        assert!(names.contains(&ToolName::new(SLEEP_TOOL_NAME)));
    }
}
