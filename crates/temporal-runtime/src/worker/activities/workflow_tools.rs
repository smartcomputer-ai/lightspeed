//! Generic start-on-call adapter activities: resolve a trusted versioned
//! recipe, issue the deterministic execution start, recover terminal
//! results through the fixed recovery query, and cancel the exact
//! execution. No plugin workflow type or activity is compiled into this
//! worker; the recipe is data.

use harness::PromiseSourceCheckResult;
use temporal_workflow::{
    WORKFLOW_TOOL_RECIPE_FORMAT_V1, WORKFLOW_TOOL_RECOVERY_QUERY,
    WorkflowToolExecutionCancelRequest, WorkflowToolExecutionCheckRequest, WorkflowToolRecipeV1,
    WorkflowToolRecoveryResult, WorkflowToolStartActivityRequest, WorkflowToolStartActivityResult,
    split_workflow_id, workflow_tool_recipe_fingerprint,
};
use temporalio_client::WorkflowExecutionStatus;
use temporalio_client::{
    UntypedWorkflow, WorkflowCancelOptions, WorkflowDescribeOptions, WorkflowIdReusePolicy,
    WorkflowQueryOptions, WorkflowStartOptions,
};
use temporalio_common::data_converters::PayloadConverter;
use temporalio_common::data_converters::RawValue;
use temporalio_sdk::activities::ActivityError;

use super::{
    common::activity_error,
    state::{StorageActivityDeps, WorkflowToolExecutionDeps},
};

pub(super) async fn start_execution(
    deps: Option<&WorkflowToolExecutionDeps>,
    storage: &StorageActivityDeps,
    request: WorkflowToolStartActivityRequest,
) -> Result<WorkflowToolStartActivityResult, ActivityError> {
    let Some(deps) = deps else {
        return Err(activity_error(anyhow::anyhow!(
            "workflow-tool execution activities are not configured"
        )));
    };
    let recipe_bytes = match storage.blobs.read_bytes(&request.recipe_ref).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(WorkflowToolStartActivityResult::FailedRetryable {
                message: format!("read start recipe: {error}"),
            });
        }
    };
    let observed_fingerprint = workflow_tool_recipe_fingerprint(&recipe_bytes);
    if observed_fingerprint != request.recipe_fingerprint {
        return Ok(WorkflowToolStartActivityResult::FailedTerminal {
            message: format!(
                "start recipe fingerprint mismatch: admitted {} observed {observed_fingerprint}",
                request.recipe_fingerprint
            ),
        });
    }
    if request.recipe_format != WORKFLOW_TOOL_RECIPE_FORMAT_V1 {
        return Ok(WorkflowToolStartActivityResult::FailedTerminal {
            message: format!("unsupported start recipe format {}", request.recipe_format),
        });
    }
    let recipe: WorkflowToolRecipeV1 = match serde_json::from_slice(&recipe_bytes) {
        Ok(recipe) => recipe,
        Err(error) => {
            return Ok(WorkflowToolStartActivityResult::FailedTerminal {
                message: format!("start recipe is not a valid v1 recipe: {error}"),
            });
        }
    };
    if recipe.workflow_type.is_empty() || recipe.task_queue.is_empty() {
        return Ok(WorkflowToolStartActivityResult::FailedTerminal {
            message: "start recipe must name a workflow type and task queue".to_owned(),
        });
    }

    let Some((universe_id, _)) = split_workflow_id(&request.holder_workflow_id) else {
        return Ok(WorkflowToolStartActivityResult::FailedTerminal {
            message: format!(
                "holder workflow id is not universe-composed: {}",
                request.holder_workflow_id
            ),
        });
    };
    let args = temporal_workflow::WorkflowToolStartArgs {
        universe_id,
        holder_workflow_id: request.holder_workflow_id.clone(),
        execution_id: request.execution_id.clone(),
        invocation: request.invocation.clone(),
    };
    let input = RawValue::from_value(&args, &PayloadConverter::default());
    match deps
        .client
        .start_workflow(
            UntypedWorkflow::new(recipe.workflow_type.clone()),
            input,
            execution_start_options(recipe.task_queue.clone(), request.execution_id.clone()),
        )
        .await
    {
        Ok(_) | Err(temporalio_client::errors::WorkflowStartError::AlreadyStarted { .. }) => {
            Ok(WorkflowToolStartActivityResult::Started)
        }
        Err(error) => Ok(WorkflowToolStartActivityResult::FailedRetryable {
            message: format!("start workflow tool execution: {error}"),
        }),
    }
}

fn execution_start_options(task_queue: String, execution_id: String) -> WorkflowStartOptions {
    // A lost start acknowledgement may be retried after the execution has
    // already closed. Reusing that id would replay domain work and replace the
    // recovery view, so the same invocation must always name its original run.
    WorkflowStartOptions::new(task_queue, execution_id)
        .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
        .build()
}

/// Recovery check for one keyed promise of a started execution. Running (or
/// not-yet-visible) executions stay pending; a closed execution either
/// yields its keyed terminal resolution through the fixed recovery query,
/// or fails the promise — completion without a valid keyed result is a
/// contract violation.
pub(super) async fn check_execution(
    deps: Option<&WorkflowToolExecutionDeps>,
    storage: &StorageActivityDeps,
    request: WorkflowToolExecutionCheckRequest,
) -> Result<PromiseSourceCheckResult, ActivityError> {
    let Some(deps) = deps else {
        return Err(activity_error(anyhow::anyhow!(
            "workflow-tool execution activities are not configured"
        )));
    };
    let handle = deps
        .client
        .get_workflow_handle::<UntypedWorkflow>(request.execution_id.clone());
    let description = match handle.describe(WorkflowDescribeOptions::default()).await {
        Ok(description) => description,
        // Not visible yet: the deterministic start may not have been issued
        // or become visible; the start loop owns terminal start failure.
        Err(_) => return Ok(PromiseSourceCheckResult::Pending),
    };
    let failed = |message: String| async {
        let error_ref = storage
            .blobs
            .put_bytes(message.into_bytes())
            .await
            .map_err(activity_error)?;
        Ok(PromiseSourceCheckResult::Failed {
            error_ref: Some(error_ref),
        })
    };
    match description.status() {
        WorkflowExecutionStatus::Running | WorkflowExecutionStatus::ContinuedAsNew => {
            Ok(PromiseSourceCheckResult::Pending)
        }
        WorkflowExecutionStatus::Completed => {
            let recovered: Result<WorkflowToolRecoveryResult, _> = handle
                .query(
                    temporalio_client::UntypedQuery::new(WORKFLOW_TOOL_RECOVERY_QUERY),
                    RawValue::from_value(&(), &PayloadConverter::default()),
                    WorkflowQueryOptions::default(),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|raw| decode_recovery_result(raw).map_err(|error| error.to_string()));
            match recovered {
                Ok(result) => match result.resolutions.get(&request.completion_key) {
                    Some(harness::PromiseResolution::Resolved { payload_ref }) => {
                        Ok(PromiseSourceCheckResult::Resolved {
                            payload_ref: payload_ref.clone(),
                        })
                    }
                    Some(harness::PromiseResolution::Failed { error_ref }) => {
                        Ok(PromiseSourceCheckResult::Failed {
                            error_ref: error_ref.clone(),
                        })
                    }
                    Some(harness::PromiseResolution::Cancelled) => {
                        failed(format!(
                            "started execution {} reported completion key `{}` as cancelled",
                            request.execution_id, request.completion_key
                        ))
                        .await
                    }
                    None => {
                        failed(format!(
                            "started execution {} completed without a terminal result for completion key `{}` (contract violation)",
                            request.execution_id, request.completion_key
                        ))
                        .await
                    }
                },
                Err(error) => {
                    failed(format!(
                        "started execution {} completed but its recovery query failed: {error}",
                        request.execution_id
                    ))
                    .await
                }
            }
        }
        status => {
            failed(format!(
                "started execution {} reached terminal status {status:?} without resolving completion key `{}`",
                request.execution_id, request.completion_key
            ))
            .await
        }
    }
}

fn decode_recovery_result(raw: RawValue) -> anyhow::Result<WorkflowToolRecoveryResult> {
    let payload = raw
        .payloads
        .first()
        .ok_or_else(|| anyhow::anyhow!("recovery query returned no payload"))?;
    serde_json::from_slice(&payload.data)
        .map_err(|error| anyhow::anyhow!("recovery query payload is not valid JSON: {error}"))
}

pub(super) async fn cancel_execution(
    deps: Option<&WorkflowToolExecutionDeps>,
    request: WorkflowToolExecutionCancelRequest,
) -> Result<(), ActivityError> {
    let Some(deps) = deps else {
        return Err(activity_error(anyhow::anyhow!(
            "workflow-tool execution activities are not configured"
        )));
    };
    let handle = deps
        .client
        .get_workflow_handle::<UntypedWorkflow>(request.execution_id);
    // A normally completed or already-cancelled execution makes this a
    // no-op; other failures surface so the workflow can record them.
    match handle.cancel(WorkflowCancelOptions::default()).await {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("not found") => Ok(()),
        Err(error) => Err(activity_error(anyhow::anyhow!(
            "cancel workflow execution: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrying_a_start_cannot_reexecute_a_closed_invocation() {
        let options = execution_start_options("workflow-tools".into(), "wte:execution".into());
        assert_eq!(
            options.id_reuse_policy,
            WorkflowIdReusePolicy::RejectDuplicate
        );
        assert_eq!(options.workflow_id, "wte:execution");
        assert_eq!(options.task_queue, "workflow-tools");
    }
}
