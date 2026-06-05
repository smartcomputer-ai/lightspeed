use engine::ContextCompactionResult;
use temporalio_sdk::activities::ActivityError;

use crate::ContextCompactActivityRequest;

use super::{common::activity_error, state::LlmActivityDeps};

pub(super) async fn compact_context(
    deps: &LlmActivityDeps,
    request: ContextCompactActivityRequest,
) -> Result<ContextCompactionResult, ActivityError> {
    let request = request.request;
    deps.llm
        .compact_context(request)
        .await
        .map_err(activity_error)
}
