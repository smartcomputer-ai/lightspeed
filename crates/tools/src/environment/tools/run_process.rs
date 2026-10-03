//! Substrate operation: start a process and wait up to a yield or to exit.

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
    #[serde(default)]
    pub tty: bool,
    /// Return with a handle after this long if the process is still
    /// running. Absent waits up to the deployment ceiling.
    pub yield_ms: Option<u64>,
    /// Kill deadline. Absent means the call never kills a running process.
    pub timeout_ms: Option<u64>,
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
    // Every wait is bounded by the same ceiling the process activity
    // deadline derives from, so a call always returns before the deadline.
    let ceiling = ctx.limits.max_process_timeout_ms;

    process
        .run_process(ProcessRequest {
            argv: args.argv,
            cwd,
            env: args.env,
            secret_env: BTreeMap::new(),
            stdin: args.stdin.map(String::into_bytes),
            tty: args.tty,
            timeout_ms: args.timeout_ms.map(|timeout| timeout.min(ceiling)),
            yield_ms: Some(args.yield_ms.unwrap_or(ceiling).min(ceiling)),
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
    use harness::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        environment::process::{
            ContinueProcessRequest, ProcessError, ProcessExecResult, ProcessExecutor,
            ProcessStatus, StreamOutput,
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
                pid: Some(7),
                exit_code: Some(0),
                failure: None,
                stdout: StreamOutput {
                    bytes: b"ok".to_vec(),
                    omitted_at: None,
                },
                stderr: StreamOutput::default(),
                omitted_bytes: 0,
                leftover_processes: Vec::new(),
            })
        }

        async fn continue_process(
            &self,
            _request: ContinueProcessRequest,
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
    async fn invoke_run_process_applies_bounds_and_resolves_cwd() {
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
                tty: false,
                yield_ms: Some(10),
                timeout_ms: None,
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
        assert_eq!(
            requests[0].timeout_ms, None,
            "no timeout means the call never kills the process"
        );
        assert_eq!(requests[0].yield_ms, Some(10));
        assert_eq!(requests[0].max_output_bytes, Some(512 * 1024));
        assert_eq!(requests[0].stdin, Some(b"input".to_vec()));
        assert!(!requests[0].tty);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_run_process_caps_waits_and_timeouts_at_the_ceiling() {
        let process = Arc::new(RecordingProcessExecutor::default());
        let ctx = context(Some(process.clone()));
        let ceiling = ctx.limits.max_process_timeout_ms;

        invoke_run_process(
            &ctx,
            RunProcessArgs {
                argv: vec!["sleep".to_string(), "1".to_string()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                tty: true,
                yield_ms: None,
                timeout_ms: Some(ceiling * 4),
                max_output_bytes: Some(64),
            },
        )
        .await
        .expect("run process");

        let requests = process.requests.lock().expect("lock");
        assert_eq!(
            requests[0].yield_ms,
            Some(ceiling),
            "absent yield waits to the ceiling"
        );
        assert_eq!(requests[0].timeout_ms, Some(ceiling));
        assert_eq!(requests[0].max_output_bytes, Some(64));
        assert!(requests[0].tty);
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
                tty: false,
                yield_ms: None,
                timeout_ms: None,
                max_output_bytes: None,
            },
        )
        .await
        .expect_err("run should fail");

        assert!(matches!(error, ToolError::UnsupportedCapability { .. }));
        assert!(
            error
                .to_string()
                .contains("environment_process_unavailable")
        );
    }
}
