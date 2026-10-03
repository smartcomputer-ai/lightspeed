use temporal_workflow::{
    SubagentCloseActivityRequest, SubagentPrepareActivityRequest, SubagentPrepareActivityResult,
    SubagentResolveActivityRequest,
};
use temporalio_sdk::activities::ActivityError;

use super::{common::activity_error, state::SubagentActivityDeps};

pub(super) async fn prepare(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentPrepareActivityRequest,
) -> Result<SubagentPrepareActivityResult, ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    deps.service
        .prepare(request.start, now_ms)
        .await
        .map_err(activity_error)
}

pub(super) async fn resolve(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentResolveActivityRequest,
) -> Result<harness::PromiseResolution, ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    deps.service
        .resolve(request.child, request.terminal)
        .await
        .map_err(activity_error)
}

pub(super) async fn close(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentCloseActivityRequest,
) -> Result<(), ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    deps.service
        .close(&request.session_id)
        .await
        .map_err(activity_error)
}
