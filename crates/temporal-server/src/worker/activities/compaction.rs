use engine::{ContextCompactionResult, CoreAgentIoError};
use temporalio_sdk::activities::ActivityError;

use crate::worker::ContextCompactActivityRequest;

use super::{
    common::{
        activity_error, failed_context_compaction_result_from_error, transient_provider_failure,
    },
    state::LlmActivityDeps,
};

pub(super) async fn compact_context(
    deps: &LlmActivityDeps,
    attempt: u32,
    request: ContextCompactActivityRequest,
) -> Result<ContextCompactionResult, ActivityError> {
    let ContextCompactActivityRequest {
        request,
        attached_resources,
    } = request;
    // Compaction calls the model too, so it is held to the same authority
    // as a turn; a refused compaction fails and the next turn is refused.
    if let Some((access, universe_id)) = &deps.access
        && let Some(revoked) = super::llm::revoked_authority(
            access,
            *universe_id,
            &request.session_id,
            &attached_resources,
        )
        .await?
    {
        return failed_context_compaction_result_from_error(
            deps.blobs.as_ref(),
            request,
            CoreAgentIoError::Failed {
                message: format!("run authority revoked before the model call: {revoked}"),
            },
        )
        .await
        .map_err(activity_error);
    }
    match deps.llm.compact_context(request.clone()).await {
        Ok(result) => Ok(result),
        // Transient provider errors become the typed retryable activity
        // failure; Temporal owns the durable backoff.
        Err(CoreAgentIoError::Retryable {
            message,
            retry_after,
        }) => Err(transient_provider_failure(
            "context compaction",
            attempt,
            message,
            retry_after,
        )),
        Err(error) => {
            failed_context_compaction_result_from_error(deps.blobs.as_ref(), request, error)
                .await
                .map_err(activity_error)
        }
    }
}
