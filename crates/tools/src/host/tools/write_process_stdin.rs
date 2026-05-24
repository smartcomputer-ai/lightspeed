//! Canonical write-process-stdin operation.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::{
        context::HostToolContext,
        process::{ProcessHandle, ProcessOutput, WriteProcessStdinRequest},
    },
};

use super::unsupported_capability;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteProcessStdinArgs {
    pub handle: ProcessHandle,
    pub input: String,
    #[serde(default)]
    pub close_stdin: bool,
    pub yield_time_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

pub async fn invoke_write_process_stdin(
    ctx: &HostToolContext,
    args: WriteProcessStdinArgs,
) -> ToolResult<ProcessOutput> {
    let process = ctx
        .process
        .as_ref()
        .ok_or_else(|| unsupported_capability("process execution is not available"))?;

    process
        .write_stdin(WriteProcessStdinRequest {
            handle: args.handle,
            input: args.input.into_bytes(),
            close_stdin: args.close_stdin,
            yield_time_ms: args.yield_time_ms,
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
    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        host::{
            fs::InMemoryFileSystem,
            process::{
                ProcessError, ProcessExecResult, ProcessExecutor, ProcessRequest, ProcessStatus,
                StreamOutput,
            },
        },
    };

    #[derive(Default)]
    struct RecordingProcessExecutor {
        requests: Mutex<Vec<WriteProcessStdinRequest>>,
    }

    #[async_trait]
    impl ProcessExecutor for RecordingProcessExecutor {
        async fn run_process(&self, _request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not needed".to_string(),
            })
        }

        async fn write_stdin(
            &self,
            request: WriteProcessStdinRequest,
        ) -> ProcessExecResult<ProcessOutput> {
            self.requests.lock().expect("lock").push(request);
            Ok(ProcessOutput {
                status: ProcessStatus::Running,
                handle: Some(ProcessHandle::new("proc-1")),
                exit_code: None,
                stdout: StreamOutput {
                    bytes: b"next".to_vec(),
                    truncated: false,
                },
                stderr: StreamOutput::default(),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_write_process_stdin_writes_to_existing_handle() {
        let process = Arc::new(RecordingProcessExecutor::default());
        let ctx = HostToolContext::new(
            Arc::new(InMemoryFileSystem::full_access()),
            Some(process.clone()),
            Arc::new(InMemoryBlobStore::new()),
        );

        let output = invoke_write_process_stdin(
            &ctx,
            WriteProcessStdinArgs {
                handle: ProcessHandle::new("proc-1"),
                input: "hello".to_string(),
                close_stdin: true,
                yield_time_ms: Some(10),
                max_output_bytes: None,
            },
        )
        .await
        .expect("write stdin");

        assert_eq!(output.status, ProcessStatus::Running);
        let requests = process.requests.lock().expect("lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].handle, ProcessHandle::new("proc-1"));
        assert_eq!(requests[0].input, b"hello".to_vec());
        assert!(requests[0].close_stdin);
        assert_eq!(requests[0].max_output_bytes, Some(512 * 1024));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_write_process_stdin_requires_process_capability() {
        let ctx = HostToolContext::new(
            Arc::new(InMemoryFileSystem::full_access()),
            None,
            Arc::new(InMemoryBlobStore::new()),
        );

        let error = invoke_write_process_stdin(
            &ctx,
            WriteProcessStdinArgs {
                handle: ProcessHandle::new("proc-1"),
                input: "hello".to_string(),
                close_stdin: false,
                yield_time_ms: None,
                max_output_bytes: None,
            },
        )
        .await
        .expect_err("write stdin should fail");

        assert!(matches!(error, ToolError::UnsupportedCapability { .. }));
    }
}
