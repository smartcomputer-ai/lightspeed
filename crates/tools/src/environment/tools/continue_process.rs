//! Substrate operation: continue with a running handle.
//!
//! Optionally act on the process (deliver input, close stdin, or send a
//! signal), then wait for the window and return the output produced since
//! the last call. Terminate is a field of this operation, not an operation.

use serde::{Deserialize, Serialize};

use crate::{
    environment::EnvironmentToolContext,
    environment::process::{ContinueProcessRequest, ProcessHandle, ProcessOutput, ProcessSignal},
    error::ToolResult,
};

use super::unsupported_process_capability;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContinueProcessArgs {
    pub handle: ProcessHandle,
    pub input: Option<String>,
    #[serde(default)]
    pub close_stdin: bool,
    pub signal: Option<ProcessSignal>,
    /// Collect output for this long, returning early only when the process
    /// exits. Absent waits up to the deployment ceiling.
    pub wait_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

impl ContinueProcessArgs {
    pub fn wait(handle: ProcessHandle, wait_ms: Option<u64>) -> Self {
        Self {
            handle,
            input: None,
            close_stdin: false,
            signal: None,
            wait_ms,
            max_output_bytes: None,
        }
    }
}

pub async fn invoke_continue_process(
    ctx: &EnvironmentToolContext,
    args: ContinueProcessArgs,
) -> ToolResult<ProcessOutput> {
    let process = ctx
        .process
        .as_ref()
        .ok_or_else(unsupported_process_capability)?;
    let ceiling = ctx.limits.max_process_timeout_ms;

    process
        .continue_process(ContinueProcessRequest {
            handle: args.handle,
            input: args.input.map(String::into_bytes),
            close_stdin: args.close_stdin,
            signal: args.signal,
            wait_ms: Some(args.wait_ms.unwrap_or(ceiling).min(ceiling)),
            max_output_bytes: Some(
                args.max_output_bytes
                    .unwrap_or(ctx.limits.max_process_output_bytes),
            ),
        })
        .await
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use harness::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        environment::process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessRequest, ProcessStatus,
            StreamOutput,
        },
        error::ToolError,
    };

    #[derive(Default)]
    struct RecordingProcessExecutor {
        requests: Mutex<Vec<ContinueProcessRequest>>,
    }

    #[async_trait]
    impl ProcessExecutor for RecordingProcessExecutor {
        async fn run_process(&self, _request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not needed".to_string(),
            })
        }

        async fn continue_process(
            &self,
            request: ContinueProcessRequest,
        ) -> ProcessExecResult<ProcessOutput> {
            self.requests.lock().expect("lock").push(request);
            Ok(ProcessOutput {
                status: ProcessStatus::Running,
                handle: Some(ProcessHandle::new("proc-1")),
                pid: Some(7),
                exit_code: None,
                failure: None,
                stdout: StreamOutput {
                    bytes: b"next".to_vec(),
                    omitted_at: None,
                },
                stderr: StreamOutput::default(),
                omitted_bytes: 0,
                leftover_processes: Vec::new(),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_continue_process_forwards_input_signal_and_bounded_wait() {
        let process = Arc::new(RecordingProcessExecutor::default());
        let ctx =
            EnvironmentToolContext::new(Some(process.clone()), Arc::new(InMemoryBlobStore::new()));
        let ceiling = ctx.limits.max_process_timeout_ms;

        let output = invoke_continue_process(
            &ctx,
            ContinueProcessArgs {
                handle: ProcessHandle::new("proc-1"),
                input: Some("hello".to_string()),
                close_stdin: true,
                signal: None,
                wait_ms: Some(10),
                max_output_bytes: None,
            },
        )
        .await
        .expect("continue");
        assert_eq!(output.status, ProcessStatus::Running);

        invoke_continue_process(
            &ctx,
            ContinueProcessArgs {
                handle: ProcessHandle::new("proc-1"),
                input: None,
                close_stdin: false,
                signal: Some(ProcessSignal::Interrupt),
                wait_ms: None,
                max_output_bytes: Some(16),
            },
        )
        .await
        .expect("interrupt");

        let requests = process.requests.lock().expect("lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].handle, ProcessHandle::new("proc-1"));
        assert_eq!(requests[0].input, Some(b"hello".to_vec()));
        assert!(requests[0].close_stdin);
        assert_eq!(requests[0].wait_ms, Some(10));
        assert_eq!(requests[0].max_output_bytes, Some(512 * 1024));
        assert_eq!(requests[1].signal, Some(ProcessSignal::Interrupt));
        assert_eq!(requests[1].wait_ms, Some(ceiling));
        assert_eq!(requests[1].max_output_bytes, Some(16));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_continue_process_requires_process_capability() {
        let ctx = EnvironmentToolContext::new(None, Arc::new(InMemoryBlobStore::new()));

        let error = invoke_continue_process(
            &ctx,
            ContinueProcessArgs::wait(ProcessHandle::new("proc-1"), None),
        )
        .await
        .expect_err("continue should fail");

        assert!(matches!(error, ToolError::UnsupportedCapability { .. }));
        assert!(
            error
                .to_string()
                .contains("environment_process_unavailable")
        );
    }
}
