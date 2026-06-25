use engine::{
    SessionId, ToolBatchOutcome, ToolInvocationBatchRequest, ToolInvocationRequest, ToolName,
};
use host_protocol::shared::JobId;
use temporal_workflow::{
    CheckEnvironmentJobWaitActivityRequest, CheckEnvironmentJobWaitActivityResult,
    EnvironmentJobWaitMode, EnvironmentJobWaitTerminalPolicy,
};
use temporalio_sdk::activities::ActivityError;
use tools::environment::jobs::{
    JOB_WAIT_TOOL_NAME, JobHandleArg, JobWaitArgs, JobWaitMode, JobWaitTerminalPolicy,
};

use crate::worker::ToolInvokeBatchActivityRequest;

use super::{
    common::{activity_error, failed_tool_batch_result},
    state::ToolActivityDeps,
};

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

pub(super) async fn check_environment_job_wait(
    deps: &ToolActivityDeps,
    request: CheckEnvironmentJobWaitActivityRequest,
) -> Result<CheckEnvironmentJobWaitActivityResult, ActivityError> {
    let batch_request = environment_job_wait_batch_request(deps, &request).await?;
    let timed_out = request
        .wait
        .deadline_ms
        .is_some_and(|deadline_ms| deadline_ms <= request.observed_at_ms);
    match deps.tools.invoke_batch(batch_request.clone()).await {
        Ok(ToolBatchOutcome::Completed { result }) => {
            Ok(CheckEnvironmentJobWaitActivityResult::Ready { result })
        }
        Ok(ToolBatchOutcome::Deferred { .. }) if !timed_out => {
            Ok(CheckEnvironmentJobWaitActivityResult::Pending)
        }
        Ok(ToolBatchOutcome::Deferred { .. }) => failed_tool_batch_result(
            deps.blobs.as_ref(),
            &batch_request,
            "environment job wait remained pending after timeout",
        )
        .await
        .map(|result| CheckEnvironmentJobWaitActivityResult::Ready { result })
        .map_err(activity_error),
        Err(error) => {
            failed_tool_batch_result(deps.blobs.as_ref(), &batch_request, error.to_string())
                .await
                .map(|result| CheckEnvironmentJobWaitActivityResult::Ready { result })
                .map_err(activity_error)
        }
    }
}

async fn environment_job_wait_batch_request(
    deps: &ToolActivityDeps,
    request: &CheckEnvironmentJobWaitActivityRequest,
) -> Result<ToolInvocationBatchRequest, ActivityError> {
    let wait = &request.wait;
    let session_id = wait
        .handles
        .first()
        .ok_or_else(|| activity_error(anyhow::anyhow!("environment job wait has no handles")))
        .and_then(|handle| {
            SessionId::try_new(handle.session_id.clone())
                .map_err(|error| activity_error(anyhow::anyhow!("invalid session id: {error}")))
        })?;
    let timed_out = wait
        .deadline_ms
        .is_some_and(|deadline_ms| deadline_ms <= request.observed_at_ms);
    let args = JobWaitArgs {
        jobs: wait
            .handles
            .iter()
            .map(|handle| JobHandleArg {
                session_id: Some(handle.session_id.clone()),
                env_id: Some(handle.env_id.clone()),
                job_id: JobId::new(handle.job_id.clone()),
            })
            .collect(),
        mode: tool_job_wait_mode(wait.mode),
        terminal_policy: tool_job_wait_terminal_policy(wait.terminal_policy),
        timeout_ms: timed_out.then_some(0),
        output_bytes: wait.output_bytes,
        include_artifacts: wait.include_artifacts,
    };
    let arguments_ref = deps
        .blobs
        .put_bytes(serde_json::to_vec(&args).map_err(activity_error)?)
        .await
        .map_err(activity_error)?;
    Ok(ToolInvocationBatchRequest {
        session_id,
        run_id: wait.run_id,
        turn_id: wait.turn_id,
        batch_id: wait.batch_id,
        default_targets: Default::default(),
        calls: vec![ToolInvocationRequest {
            call_id: wait.call_id.clone(),
            tool_name: ToolName::new(JOB_WAIT_TOOL_NAME),
            arguments_ref,
            execution_target: None,
        }],
    })
}

fn tool_job_wait_mode(mode: EnvironmentJobWaitMode) -> JobWaitMode {
    match mode {
        EnvironmentJobWaitMode::All => JobWaitMode::All,
        EnvironmentJobWaitMode::Any => JobWaitMode::Any,
    }
}

fn tool_job_wait_terminal_policy(
    policy: EnvironmentJobWaitTerminalPolicy,
) -> JobWaitTerminalPolicy {
    match policy {
        EnvironmentJobWaitTerminalPolicy::AnyTerminal => JobWaitTerminalPolicy::AnyTerminal,
        EnvironmentJobWaitTerminalPolicy::AllSucceeded => JobWaitTerminalPolicy::AllSucceeded,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use engine::{
        CoreAgentIoError, CoreAgentTools, RunId, ToolBatchId, ToolBatchResumeDirective, ToolCallId,
        ToolCallStatus, ToolInvocationBatchResult, ToolInvocationResult, TurnId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use temporal_workflow::{ActiveEnvironmentJobWait, EnvironmentJobHandle};

    use super::*;

    struct RecordingTools {
        blobs: Arc<InMemoryBlobStore>,
        requests: Mutex<Vec<ToolInvocationBatchRequest>>,
        complete_when_timeout: bool,
    }

    impl RecordingTools {
        fn new(blobs: Arc<InMemoryBlobStore>, complete_when_timeout: bool) -> Self {
            Self {
                blobs,
                requests: Mutex::new(Vec::new()),
                complete_when_timeout,
            }
        }
    }

    #[async_trait]
    impl CoreAgentTools for RecordingTools {
        async fn invoke_batch(
            &self,
            request: ToolInvocationBatchRequest,
        ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
            let args_bytes = self
                .blobs
                .read_bytes(&request.calls[0].arguments_ref)
                .await
                .map_err(test_io_error)?;
            let args: JobWaitArgs = serde_json::from_slice(&args_bytes).map_err(test_io_error)?;
            if self.complete_when_timeout && args.timeout_ms == Some(0) {
                return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results: vec![ToolInvocationResult {
                        call_id: request.calls[0].call_id.clone(),
                        status: ToolCallStatus::Succeeded,
                        output_ref: None,
                        model_visible_context_entries: Vec::new(),
                        error_ref: None,
                        effects: Vec::new(),
                    }],
                }));
            }
            Ok(ToolBatchOutcome::Deferred {
                batch_id: request.batch_id,
                resume_directive: ToolBatchResumeDirective::new(
                    temporal_workflow::ENVIRONMENT_JOB_WAIT_DIRECTIVE_KIND,
                    serde_json::json!({}),
                ),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn environment_job_wait_check_returns_pending_for_deferred_wait_before_deadline() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let recorder = Arc::new(RecordingTools::new(blobs.clone(), false));
        let deps = ToolActivityDeps {
            tools: recorder.clone(),
            blobs: blobs.clone(),
        };

        let result = check_environment_job_wait(
            &deps,
            CheckEnvironmentJobWaitActivityRequest {
                wait: active_environment_job_wait(Some(5_000)),
                observed_at_ms: 1_000,
            },
        )
        .await
        .expect("check wait");

        assert!(matches!(
            result,
            CheckEnvironmentJobWaitActivityResult::Pending
        ));
        let request = recorder.requests.lock().expect("requests lock")[0].clone();
        let args = read_wait_args(blobs.as_ref(), &request).await;
        assert_eq!(args.timeout_ms, None);
        assert_eq!(request.session_id, SessionId::new("session_1"));
        assert_eq!(request.batch_id, ToolBatchId::new(30));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn environment_job_wait_check_sets_zero_timeout_after_deadline() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let recorder = Arc::new(RecordingTools::new(blobs.clone(), true));
        let deps = ToolActivityDeps {
            tools: recorder.clone(),
            blobs: blobs.clone(),
        };

        let result = check_environment_job_wait(
            &deps,
            CheckEnvironmentJobWaitActivityRequest {
                wait: active_environment_job_wait(Some(1_000)),
                observed_at_ms: 1_000,
            },
        )
        .await
        .expect("check wait");

        let CheckEnvironmentJobWaitActivityResult::Ready { result } = result else {
            panic!("expected ready result after timeout");
        };
        assert_eq!(result.batch_id, ToolBatchId::new(30));
        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let request = recorder.requests.lock().expect("requests lock")[0].clone();
        let args = read_wait_args(blobs.as_ref(), &request).await;
        assert_eq!(args.timeout_ms, Some(0));
    }

    async fn read_wait_args(
        blobs: &InMemoryBlobStore,
        request: &ToolInvocationBatchRequest,
    ) -> JobWaitArgs {
        let bytes = blobs
            .read_bytes(&request.calls[0].arguments_ref)
            .await
            .expect("read args");
        serde_json::from_slice(&bytes).expect("decode args")
    }

    fn active_environment_job_wait(deadline_ms: Option<u64>) -> ActiveEnvironmentJobWait {
        ActiveEnvironmentJobWait {
            batch_id: ToolBatchId::new(30),
            run_id: RunId::new(10),
            turn_id: TurnId::new(20),
            call_id: ToolCallId::new("call_job_wait"),
            handles: vec![EnvironmentJobHandle {
                session_id: "session_1".to_owned(),
                env_id: "env_1".to_owned(),
                job_id: "job_1".to_owned(),
            }],
            mode: EnvironmentJobWaitMode::All,
            terminal_policy: EnvironmentJobWaitTerminalPolicy::AnyTerminal,
            output_bytes: Some(2048),
            include_artifacts: false,
            deadline_ms,
            next_check_at_ms: 2_000,
            poll_attempt: 0,
        }
    }

    fn test_io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
        CoreAgentIoError::Failed {
            message: error.to_string(),
        }
    }
}
