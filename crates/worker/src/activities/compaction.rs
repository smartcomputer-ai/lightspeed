use engine::ContextCompactionResult;
use temporalio_sdk::activities::ActivityError;

use crate::ContextCompactActivityRequest;

use super::{
    common::{activity_error, failed_context_compaction_result_from_error},
    state::LlmActivityDeps,
};

pub(super) async fn compact_context(
    deps: &LlmActivityDeps,
    request: ContextCompactActivityRequest,
) -> Result<ContextCompactionResult, ActivityError> {
    let request = request.request;
    match deps.llm.compact_context(request.clone()).await {
        Ok(result) => Ok(result),
        Err(error) => {
            failed_context_compaction_result_from_error(deps.blobs.as_ref(), request, error)
                .await
                .map_err(activity_error)
        }
    }
}
