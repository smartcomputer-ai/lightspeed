//! Generic start-on-call adapter activities: resolve a trusted versioned
//! recipe, issue the deterministic execution start, recover terminal
//! results through the fixed recovery query, and cancel the exact
//! execution. No plugin workflow type or activity is compiled into this
//! worker; the recipe is data.

use std::time::Duration;

use harness::PromiseSourceCheckResult;
use temporal_workflow::{
    WORKFLOW_TOOL_RECIPE_FORMAT_V1, WORKFLOW_TOOL_RECOVERY_QUERY,
    WorkflowToolExecutionCancelRequest, WorkflowToolExecutionCheckRequest, WorkflowToolRecipeV1,
    WorkflowToolRecoveryResult, WorkflowToolStartActivityRequest, WorkflowToolStartActivityResult,
    split_workflow_id, workflow_tool_recipe_fingerprint,
};
use temporalio_client::WorkflowExecutionStatus;
use temporalio_client::errors::{
    WorkflowGetResultError, WorkflowInteractionError, WorkflowQueryError,
};
use temporalio_client::{
    RpcOptions, UntypedWorkflow, WorkflowCancelOptions, WorkflowDescribeOptions,
    WorkflowGetResultOptions, WorkflowIdReusePolicy, WorkflowQueryOptions, WorkflowStartOptions,
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

/// A query waits for a poller on the execution's queue, and the poll blocks the
/// session's outer loop. A hung query should read as a failed one and ask again on the
/// next poll, well inside the activity's start-to-close.
const RECOVERY_QUERY_RPC_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether asking again cannot change this answer. A handler that failed or does not
/// exist comes back as `InvalidArgument`. Everything else, such as no recent poller
/// on the queue during a deploy, a timeout or a transport failure, can pass, and the
/// promise's deadline or its run still bound the wait.
fn recovery_query_failure_is_final(error: &WorkflowQueryError) -> bool {
    match error {
        WorkflowQueryError::Rejected { .. } | WorkflowQueryError::PayloadConversion(_) => true,
        WorkflowQueryError::Rpc(status) => status.code() == tonic::Code::InvalidArgument,
        _ => false,
    }
}

/// Recovery check for one keyed promise of a started execution. Running (or
/// not-yet-visible) executions stay pending; a closed execution either
/// yields its keyed terminal resolution through the fixed recovery query,
/// or fails the promise — completion without a valid keyed result is a
/// contract violation. A query that may answer differently later stays pending.
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
            let raw = match handle
                .query(
                    temporalio_client::UntypedQuery::new(WORKFLOW_TOOL_RECOVERY_QUERY),
                    RawValue::from_value(&(), &PayloadConverter::default()),
                    WorkflowQueryOptions::builder()
                        .rpc_options(
                            RpcOptions::builder()
                                .timeout(RECOVERY_QUERY_RPC_TIMEOUT)
                                .build(),
                        )
                        .build(),
                )
                .await
            {
                Ok(raw) => raw,
                Err(error) if !recovery_query_failure_is_final(&error) => {
                    return Ok(PromiseSourceCheckResult::Pending);
                }
                Err(error) => {
                    return failed(format!(
                        "started execution {} completed but its recovery query failed: {error}",
                        request.execution_id
                    ))
                    .await;
                }
            };
            let result = match decode_recovery_result(raw) {
                Ok(result) => result,
                Err(error) => {
                    return failed(format!(
                        "started execution {} completed but its recovery query failed: {error}",
                        request.execution_id
                    ))
                    .await;
                }
            };
            match result.resolutions.get(&request.completion_key) {
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
            }
        }
        status => {
            let cause = match status {
                WorkflowExecutionStatus::Failed => execution_failure_message(&handle).await,
                _ => None,
            };
            failed(unresolved_terminal_message(
                &request,
                status,
                cause.as_deref(),
            ))
            .await
        }
    }
}

/// Why an execution failed, in its own words. A closed execution answers at once,
/// and the promise fails with or without this.
async fn execution_failure_message(
    handle: &temporalio_client::WorkflowHandle<temporalio_client::Client, UntypedWorkflow>,
) -> Option<String> {
    let options = WorkflowGetResultOptions::builder()
        .rpc_options(
            RpcOptions::builder()
                .timeout(RECOVERY_QUERY_RPC_TIMEOUT)
                .build(),
        )
        .build();
    match handle.get_result(options).await {
        Err(WorkflowGetResultError::Failed(failure)) => Some(failure.to_string()),
        _ => None,
    }
}

fn unresolved_terminal_message(
    request: &WorkflowToolExecutionCheckRequest,
    status: WorkflowExecutionStatus,
    cause: Option<&str>,
) -> String {
    let message = format!(
        "started execution {} reached terminal status {status:?} without resolving completion key `{}`",
        request.execution_id, request.completion_key
    );
    match cause {
        Some(cause) => format!("{message}: {cause}"),
        None => message,
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
        Err(WorkflowInteractionError::NotFound(_)) => Ok(()),
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

    #[test]
    fn an_unresolved_terminal_reports_why_the_execution_failed() {
        let request = WorkflowToolExecutionCheckRequest {
            execution_id: "wte:execution".into(),
            completion_key: "reply".into(),
        };
        let message = unresolved_terminal_message(
            &request,
            WorkflowExecutionStatus::Failed,
            Some("sub-agent resolve failed: blob missing"),
        );
        assert!(message.contains("wte:execution"), "{message}");
        assert!(
            message.ends_with("sub-agent resolve failed: blob missing"),
            "{message}"
        );
        assert!(
            !unresolved_terminal_message(&request, WorkflowExecutionStatus::Terminated, None)
                .ends_with(':')
        );
    }

    #[test]
    fn only_a_recovery_query_failure_a_retry_cannot_change_is_final() {
        let rpc = |code| WorkflowQueryError::Rpc(tonic::Status::new(code, "query"));
        for error in [
            WorkflowQueryError::Rejected { status: None },
            rpc(tonic::Code::InvalidArgument),
        ] {
            assert!(recovery_query_failure_is_final(&error), "{error}");
        }
        for error in [
            rpc(tonic::Code::FailedPrecondition),
            rpc(tonic::Code::DeadlineExceeded),
            rpc(tonic::Code::Unavailable),
            rpc(tonic::Code::ResourceExhausted),
            WorkflowQueryError::NotFound(tonic::Status::not_found("gone")),
        ] {
            assert!(!recovery_query_failure_is_final(&error), "{error}");
        }
    }
}
