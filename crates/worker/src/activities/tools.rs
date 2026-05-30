use engine::ToolInvocationBatchResult;
use temporalio_sdk::activities::ActivityError;

use crate::ToolInvokeBatchActivityRequest;

use super::{
    common::{activity_error, failed_tool_batch_result},
    state::ToolActivityDeps,
};

pub(super) async fn invoke_batch(
    deps: &ToolActivityDeps,
    request: ToolInvokeBatchActivityRequest,
) -> Result<ToolInvocationBatchResult, ActivityError> {
    let request = request.request;
    match deps.tools.invoke_batch(request.clone()).await {
        Ok(result) => Ok(result),
        Err(error) => failed_tool_batch_result(deps.blobs.as_ref(), &request, error.to_string())
            .await
            .map_err(activity_error),
    }
}
