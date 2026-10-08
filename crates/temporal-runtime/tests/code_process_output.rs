//! Native QuickJS consumes the production process-tool projection. Only the
//! process executor is a fixture; no external services or shell are required.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use codemode::{Cancellation, ExecutionEvent, ExecutionInput, ExecutionLimits, HostCompletion};
use harness::storage::InMemoryBlobStore;
use serde_json::json;
use tools::{
    builtin::{BuiltinTool, BuiltinToolContext, BuiltinToolOperation, BuiltinToolSurface},
    callable::ScriptToolResult,
    environment::{
        EnvironmentToolContext,
        process::{
            ContinueProcessRequest, ProcessExecResult, ProcessExecutor, ProcessHandle,
            ProcessOutput, ProcessRequest, ProcessStatus, StreamOutput,
        },
    },
};

struct ProcessFixture;

#[async_trait]
impl ProcessExecutor for ProcessFixture {
    async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
        assert_eq!(request.argv.last().map(String::as_str), Some("fixture"));
        Ok(ProcessOutput {
            status: ProcessStatus::Running,
            handle: Some(ProcessHandle::new("process-fixture")),
            pid: Some(123),
            exit_code: None,
            failure: None,
            stdout: StreamOutput {
                bytes: "{\"answer\":42,\"label\":\"héllo 🌍\"}\n"
                    .as_bytes()
                    .to_vec(),
                omitted_at: None,
            },
            stderr: StreamOutput {
                bytes: vec![255, 0],
                omitted_at: None,
            },
            omitted_bytes: 0,
            leftover_processes: Vec::new(),
        })
    }

    async fn continue_process(
        &self,
        request: ContinueProcessRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        assert_eq!(request.handle.as_str(), "process-fixture");
        Ok(ProcessOutput {
            status: ProcessStatus::Succeeded,
            handle: None,
            pid: Some(123),
            exit_code: Some(0),
            failure: None,
            stdout: StreamOutput {
                bytes: vec![254, 128],
                omitted_at: Some(1),
            },
            stderr: StreamOutput {
                bytes: b"finished\n".to_vec(),
                omitted_at: None,
            },
            omitted_bytes: 16,
            leftover_processes: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn javascript_parses_text_and_handles_binary_process_output_across_polls() {
    let context = EnvironmentToolContext::new(
        Some(Arc::new(ProcessFixture)),
        Arc::new(InMemoryBlobStore::new()),
    );
    let input = ExecutionInput {
        source: r#"
            const started = await tools.exec_command({cmd: "fixture"});
            const parsed = JSON.parse(started.stdout);
            if ("stdout_bytes" in started || "stderr" in started)
                throw new Error("duplicate stream encoding");
            text({parsed, stderr_bytes: started.stderr_bytes});
            const finished = await tools.write_stdin({session_id: started.handle});
            if ("stdout" in finished || "stderr_bytes" in finished)
                throw new Error("duplicate poll stream encoding");
            text(finished.stderr.trim());
            return {
                answer: parsed.answer,
                success: finished.exit_code === 0 && finished.stderr.includes("finished"),
                stdout_bytes: finished.stdout_bytes,
                omitted_bytes: finished.omitted_bytes,
                stdout_omitted_at: finished.stdout_omitted_at,
            };
        "#
        .into(),
        bindings: [("exec_command", "run"), ("write_stdin", "poll")]
            .into_iter()
            .map(|(name, binding_id)| codemode::ToolBinding {
                name: name.into(),
                binding_id: binding_id.into(),
            })
            .collect(),
        limits: ExecutionLimits {
            timeout_ms: 5000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_stack_bytes: 512 * 1024,
            max_source_bytes: 8192,
            max_catalog_bytes: 8192,
            max_request_bytes: 8192,
            max_result_bytes: 8192,
            max_output_bytes: 8192,
            max_tool_calls: 2,
            max_outstanding_tool_calls: 1,
        },
    };
    let mut execution = codemode::start(input, Cancellation::default()).expect("start QuickJS");
    let sender = execution.completion_sender();
    let report = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match execution.next_event().await.expect("execution event") {
                ExecutionEvent::Request(request) => {
                    let operation = match request.binding_id.as_str() {
                        "run" => BuiltinToolOperation::RunProcess,
                        "poll" => BuiltinToolOperation::ContinueProcess,
                        other => panic!("unexpected tool binding: {other}"),
                    };
                    let result = BuiltinTool::environment(operation, BuiltinToolSurface::CodexLike)
                        .invoke_json(BuiltinToolContext::Environment(&context), request.arguments)
                        .await
                        .expect("invoke production process tool");
                    let ScriptToolResult::Succeeded { value } =
                        ScriptToolResult::succeeded(&result)
                    else {
                        panic!("successful tool result");
                    };
                    sender
                        .complete(HostCompletion {
                            request_id: request.request_id,
                            outcome: Ok(value),
                        })
                        .expect("deliver structured output to JavaScript");
                }
                ExecutionEvent::Finished(report) => break report,
            }
        }
    })
    .await
    .expect("JavaScript must finish");
    assert_eq!(report.error, None);
    assert!(report.pending_request_ids.is_empty());
    assert_eq!(report.metrics.tool_calls, 2);
    assert_eq!(
        report.output,
        vec![
            json!({"parsed": {"answer": 42, "label": "héllo 🌍"}, "stderr_bytes": [255, 0]}),
            json!("finished"),
        ]
    );
    assert_eq!(
        report.return_value,
        Some(json!({
            "answer": 42, "success": true, "stdout_bytes": [254, 128],
            "omitted_bytes": 16, "stdout_omitted_at": 1,
        }))
    );
}
