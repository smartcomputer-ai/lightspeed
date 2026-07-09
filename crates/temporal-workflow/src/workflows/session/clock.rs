use super::*;

pub(super) fn workflow_time_ms(ctx: &WorkflowContext<AgentSessionWorkflow>) -> u64 {
    ctx.workflow_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}
