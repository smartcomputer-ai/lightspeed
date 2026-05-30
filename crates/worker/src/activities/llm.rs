use engine::LlmGenerationResult;
use temporalio_sdk::activities::ActivityError;

use crate::LlmGenerateActivityRequest;

use super::{
    common::{activity_error, failed_generation_result_from_error},
    state::LlmActivityDeps,
};

pub(super) async fn generate(
    deps: &LlmActivityDeps,
    request: LlmGenerateActivityRequest,
) -> Result<LlmGenerationResult, ActivityError> {
    let request = request.request;
    match deps.llm.generate(request.clone()).await {
        Ok(result) => Ok(result),
        Err(error) => failed_generation_result_from_error(deps.blobs.as_ref(), request, error)
            .await
            .map_err(activity_error),
    }
}
