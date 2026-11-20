//! Teardown finds executions by their common universe prefix, including plugin workflows.
use futures_util::TryStreamExt;
use temporalio_client::errors::WorkflowInteractionError;
use temporalio_client::{Client, UntypedWorkflow, WorkflowListOptions, WorkflowTerminateOptions};
use uuid::Uuid;

/// Return the observed and successfully terminated counts.
/// The caller requires two empty passes to allow for eventually consistent visibility.
pub(crate) async fn terminate_universe_workflows(
    client: &Client,
    universe_id: Uuid,
) -> anyhow::Result<(u64, u64)> {
    let query = format!("WorkflowId STARTS_WITH '{universe_id}/' AND ExecutionStatus = 'Running'");
    let mut executions = client.list_workflows(query, WorkflowListOptions::default());
    let mut observed = 0;
    let mut terminated = 0;
    while let Some(execution) = executions.try_next().await? {
        observed += 1;
        match client
            .get_workflow_handle::<UntypedWorkflow>(execution.id())
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("universe deleted")
                    .build(),
            )
            .await
        {
            Ok(()) => terminated += 1,
            Err(WorkflowInteractionError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok((observed, terminated))
}
