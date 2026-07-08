use engine::{CoreAgentIoError, ToolBatchOutcome};
use temporalio_sdk::activities::ActivityError;

use crate::worker::ToolInvokeBatchActivityRequest;

use super::{
    common::{activity_error, failed_tool_batch_result},
    state::ToolActivityDeps,
};

pub(super) fn activity_error_for_core(error: CoreAgentIoError) -> ActivityError {
    activity_error(anyhow::anyhow!("{error}"))
}

pub(super) async fn invoke_batch(
    deps: &ToolActivityDeps,
    request: ToolInvokeBatchActivityRequest,
) -> Result<ToolBatchOutcome, ActivityError> {
    let request = request.request;
    match deps.tools.invoke_batch(request.clone()).await {
        Ok(result) => Ok(result),
        Err(error) => failed_tool_batch_result(deps.blobs.as_ref(), &request, error.to_string())
            .await
            .map(ToolBatchOutcome::completed)
            .map_err(activity_error),
    }
}
