//! Canonical run-process operation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    environment::EnvironmentToolContext,
    environment::process::{ProcessOutput, ProcessRequest},
    error::ToolResult,
    fs::{FsError, FsPath},
};

use super::{invalid_request, unsupported_process_capability};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunProcessArgs {
    pub argv: Vec<String>,
    pub cwd: Option<FsPath>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub stdin: Option<String>,
    pub timeout_ms: Option<u64>,
    pub yield_time_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

pub async fn invoke_run_process(
    ctx: &EnvironmentToolContext,
    args: RunProcessArgs,
) -> ToolResult<ProcessOutput> {
    if args.argv.is_empty() {
        return Err(invalid_request("run_process argv must not be empty"));
    }

    let process = ctx
        .process
        .as_ref()
        .ok_or_else(unsupported_process_capability)?;
    let cwd = match args.cwd {
        Some(cwd) => Some(resolve_process_cwd(ctx, &cwd)?),
        None => ctx.process_cwd.clone(),
    };

    process
        .run_process(ProcessRequest {
            argv: args.argv,
            cwd,
            env: args.env,
            stdin: args.stdin.map(String::into_bytes),
            timeout_ms: Some(
                args.timeout_ms
                    .unwrap_or(ctx.limits.default_process_timeout_ms),
            ),
            yield_time_ms: args.yield_time_ms,
            max_output_bytes: Some(
                args.max_output_bytes
                    .unwrap_or(ctx.limits.max_process_output_bytes),
            ),
        })
        .await
        .map_err(Into::into)
}

fn resolve_process_cwd(ctx: &EnvironmentToolContext, cwd: &FsPath) -> ToolResult<FsPath> {
    if cwd.is_absolute() {
        return Ok(cwd.clone());
    }

    let Some(base) = &ctx.process_cwd else {
        return Ok(cwd.clone());
    };

    base.join_path(cwd)
        .map_err(FsError::from)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        environment::process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessStatus, StreamOutput,
            WriteProcessStdinRequest,
        },
        error::ToolError,
    };

    #[derive(Default)]
    struct RecordingProcessExecutor {
        requests: Mutex<Vec<ProcessRequest>>,
    }

    #[async_trait]
    impl ProcessExecutor for RecordingProcessExecutor {
        async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            self.requests.lock().expect("lock").push(request);
            Ok(ProcessOutput {
                status: ProcessStatus::Succeeded,
                handle: None,
                exit_code: Some(0),
                stdout: StreamOutput {
                    bytes: b"ok".to_vec(),
                    truncated: false,
                },
                stderr: StreamOutput::default(),
            })
        }

        async fn write_stdin(
            &self,
            _request: WriteProcessStdinRequest,
        ) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not needed".to_string(),
            })
        }
    }

    fn context(process: Option<Arc<dyn ProcessExecutor>>) -> EnvironmentToolContext {
        EnvironmentToolContext::new(process, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_run_process_applies_defaults_and_resolves_cwd() {
        let process = Arc::new(RecordingProcessExecutor::default());
        let ctx = context(Some(process.clone()))
            .with_process_cwd(FsPath::new("/workspace").expect("cwd"));

        let output = invoke_run_process(
            &ctx,
            RunProcessArgs {
                argv: vec!["echo".to_string(), "hello".to_string()],
                cwd: Some(FsPath::new("subdir").expect("relative cwd")),
                env: BTreeMap::new(),
                stdin: Some("input".to_string()),
                timeout_ms: None,
                yield_time_ms: Some(10),
                max_output_bytes: None,
            },
        )
        .await
        .expect("run process");

        assert_eq!(output.stdout.text_lossy(), "ok");
        let requests = process.requests.lock().expect("lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].cwd,
            Some(FsPath::new("/workspace/subdir").unwrap())
        );
        assert_eq!(requests[0].timeout_ms, Some(60_000));
        assert_eq!(requests[0].max_output_bytes, Some(512 * 1024));
        assert_eq!(requests[0].stdin, Some(b"input".to_vec()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_run_process_requires_process_capability() {
        let ctx = context(None);

        let error = invoke_run_process(
            &ctx,
            RunProcessArgs {
                argv: vec!["echo".to_string()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: None,
                yield_time_ms: None,
                max_output_bytes: None,
            },
        )
        .await
        .expect_err("run should fail");

        assert!(matches!(error, ToolError::UnsupportedCapability { .. }));
        assert!(
            error
                .to_string()
                .contains("process tools require an active env target")
        );
    }
}
