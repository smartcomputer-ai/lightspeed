//! Ensure declarations describe serialized results across presentation adapters.

use std::sync::Arc;

use async_trait::async_trait;
use harness::{ProviderApiKind, storage::InMemoryBlobStore};
use serde::Serialize;
use serde_json::{Value, json};

use super::*;
use crate::{
    environment::process::{
        ContinueProcessRequest, LeftoverProcess, ProcessExecResult, ProcessExecutor, ProcessHandle,
        ProcessOutput, ProcessRequest, ProcessStatus, StreamOutput,
    },
    fs::memory::InMemoryFileSystem,
};

fn validator(tool: BuiltinTool) -> jsonschema::Validator {
    let definition = tool
        .definition(
            &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
            false,
        )
        .expect("tool definition");
    jsonschema::validator_for(&definition.output_schema.expect("owned output schema"))
        .expect("valid output schema")
}

fn serialized<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serialize result")
}

#[tokio::test]
async fn filesystem_outputs_match_each_surface_and_claude_edit_creation() {
    for surface in [
        BuiltinToolSurface::Canonical,
        BuiltinToolSurface::CodexLike,
        BuiltinToolSurface::ClaudeCodeLike,
    ] {
        let fs = FsToolContext::new(
            Arc::new(InMemoryFileSystem::default()),
            Arc::new(InMemoryBlobStore::new()),
        );
        let ctx = BuiltinToolContext::Vfs {
            filesystem: &fs,
            attachments: &[],
        };
        let claude = surface == BuiltinToolSurface::ClaudeCodeLike;
        for (operation, args) in [
            (
                BuiltinToolOperation::WriteFile,
                if claude {
                    json!({"file_path":"/file","content":"first\nsecond\n"})
                } else {
                    json!({"path":"/file","content":"first\nsecond\n"})
                },
            ),
            (
                BuiltinToolOperation::ReadFile,
                if claude {
                    json!({"file_path":"/file"})
                } else {
                    json!({"path":"/file"})
                },
            ),
            (
                BuiltinToolOperation::EditFile,
                if claude {
                    json!({"file_path":"/file","old_string":"first","new_string":"updated"})
                } else {
                    json!({"path":"/file","old_string":"first","new_string":"updated"})
                },
            ),
            (
                BuiltinToolOperation::Grep,
                json!({"path":"/","pattern":"updated"}),
            ),
            (
                BuiltinToolOperation::Glob,
                json!({"path":"/","pattern":"*"}),
            ),
            (BuiltinToolOperation::ListDir, json!({"path":"/"})),
        ] {
            let tool = BuiltinTool::vfs(operation, surface);
            let output = tool.invoke_json(ctx, args).await.expect("invoke");
            let schema = validator(tool);
            schema
                .validate(&output.output_json)
                .expect("actual result matches schema");
            assert!(
                !schema.is_valid(&json!(output.model_visible_text)),
                "text rendering is not the structured result"
            );
            assert!(
                !schema.is_valid(&json!({})),
                "required result fields are described"
            );
        }
        if claude {
            let tool = BuiltinTool::vfs(BuiltinToolOperation::EditFile, surface);
            let output = tool
                .invoke_json(
                    ctx,
                    json!({"file_path":"/created","old_string":"","new_string":"new file"}),
                )
                .await
                .expect("Edit creation branch");
            validator(tool)
                .validate(&output.output_json)
                .expect("create result matches union");
            assert!(output.output_json.get("replacements").is_none());
            assert!(
                !validator(BuiltinTool::vfs(
                    BuiltinToolOperation::EditFile,
                    BuiltinToolSurface::Canonical
                ))
                .is_valid(&output.output_json),
                "canonical edit requires replacement count"
            );
        }
    }
}

struct FixedProcessOutput;

fn process_output() -> ProcessOutput {
    ProcessOutput {
        status: ProcessStatus::Running,
        handle: Some(ProcessHandle::new("process-1")),
        pid: Some(123),
        exit_code: None,
        failure: None,
        stdout: StreamOutput {
            bytes: b"hello".to_vec(),
            omitted_at: Some(2),
        },
        stderr: StreamOutput::default(),
        omitted_bytes: 32,
        leftover_processes: vec![LeftoverProcess {
            pid: 124,
            command: "child".into(),
        }],
    }
}

#[async_trait]
impl ProcessExecutor for FixedProcessOutput {
    async fn run_process(&self, _: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
        Ok(process_output())
    }

    async fn continue_process(
        &self,
        _: ContinueProcessRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        Ok(process_output())
    }
}

#[tokio::test]
async fn process_presentations_return_bytes_and_handles_not_the_text_header() {
    let ctx = EnvironmentToolContext::new(
        Some(Arc::new(FixedProcessOutput)),
        Arc::new(InMemoryBlobStore::new()),
    );
    for (surface, run_args, continue_args, kill_args) in [
        (
            BuiltinToolSurface::Canonical,
            json!({"argv":["true"]}),
            json!({"handle":"process-1"}),
            None,
        ),
        (
            BuiltinToolSurface::CodexLike,
            json!({"cmd":"true"}),
            json!({"session_id":"process-1"}),
            None,
        ),
        (
            BuiltinToolSurface::ClaudeCodeLike,
            json!({"command":"true"}),
            json!({"bash_id":"process-1"}),
            Some(json!({"shell_id":"process-1"})),
        ),
    ] {
        for (operation, args) in [
            (BuiltinToolOperation::RunProcess, run_args),
            (BuiltinToolOperation::ContinueProcess, continue_args),
        ] {
            let tool = BuiltinTool::environment(operation, surface);
            let result = tool
                .invoke_json(BuiltinToolContext::Environment(&ctx), args)
                .await
                .expect("process call");
            let schema = validator(tool);
            schema
                .validate(&result.output_json)
                .expect("serialized process result");
            assert_eq!(result.output_json, serialized(&process_output()));
            let mut invalid = result.output_json;
            invalid["stdout"]["bytes"] = json!("hello");
            assert!(
                !schema.is_valid(&invalid),
                "stream bytes are an array, not visible text"
            );
            invalid["stdout"]["bytes"] = json!([256]);
            assert!(!schema.is_valid(&invalid), "stream byte range is bounded");
        }
        if let Some(args) = kill_args {
            let tool = BuiltinTool::environment(BuiltinToolOperation::ContinueProcess, surface)
                .kill_variant();
            let result = tool
                .invoke_json(BuiltinToolContext::Environment(&ctx), args)
                .await
                .expect("kill call");
            validator(tool)
                .validate(&result.output_json)
                .expect("kill returns process result");
        }
    }
}

#[test]
fn durable_job_and_agent_results_preserve_their_exact_serialized_names() {
    use crate::environment::jobs::{
        JobHandle, JobSubmitResult, JobSubmitted, ModelJobOutputSegment, ModelJobResult,
        ModelJobResultSet,
    };
    use crate::subagents::{SubagentResultEnvelope, SubagentResultStatus, SubagentToolKind};
    use environment_protocol::{
        data::jobs::{JobArtifact, JobOutputStream, JobStatus, JobSummary},
        shared::{EnvironmentPath, JobId},
    };

    let handle = JobHandle {
        environment_id: "env-1".into(),
        job_id: JobId::new("build"),
    };
    let submitted = JobSubmitResult {
        jobs: vec![JobSubmitted {
            name: Some("Build".into()),
            job_id: handle.job_id.clone(),
            handle: Some(handle.clone()),
            status: JobStatus::Queued,
            dependencies: vec![],
            queue_key: None,
            promise: Some("promise_1".into()),
        }],
    };
    validator(BuiltinTool::environment_canonical(
        BuiltinToolOperation::JobSubmit,
    ))
    .validate(&serialized(&submitted))
    .expect("submitted jobs");
    let result = ModelJobResult {
        handle: Some(handle),
        summary: Some(JobSummary {
            namespace: "session".into(),
            job_id: JobId::new("build"),
            name: None,
            status: JobStatus::TimedOut,
            dependencies: vec![],
            created_at_ms: 1,
            queued_at_ms: None,
            started_at_ms: Some(2),
            finished_at_ms: Some(3),
            exit_code: None,
            orphaned_descendants: true,
            failure: None,
            queue_key: None,
        }),
        output: vec![ModelJobOutputSegment {
            stream: JobOutputStream::Stdout,
            text: Some("done".into()),
            blob_ref: None,
            media_type: None,
            byte_len: Some(4),
        }],
        output_next_seq: 9,
        truncated: false,
        artifacts: vec![JobArtifact {
            path: EnvironmentPath::new("/out").unwrap(),
            kind: None,
            metadata: Default::default(),
        }],
        error: None,
    };
    let schema = validator(BuiltinTool::environment_canonical(
        BuiltinToolOperation::JobRun,
    ));
    schema.validate(&serialized(&result)).expect("joined job");
    let mut invalid = serialized(&result);
    invalid["summary"]["status"] = json!("timed_out");
    assert!(!schema.is_valid(&invalid), "job statuses use camelCase");
    validator(BuiltinTool::environment_canonical(
        BuiltinToolOperation::JobRead,
    ))
    .validate(&serialized(&ModelJobResultSet { jobs: vec![result] }))
    .expect("job read set");

    let definition = crate::subagents::subagent_tool_definition(SubagentToolKind::Run).unwrap();
    let schema = jsonschema::validator_for(&definition.output_schema.unwrap()).unwrap();
    let result = SubagentResultEnvelope {
        agent: "reviewer".into(),
        session_id: "child".into(),
        run_id: None,
        status: SubagentResultStatus::Completed,
        output: Some("done".into()),
        error: None,
        attachments: vec![harness::Attachment::File(harness::FileAttachment::new(
            harness::BlobRef::from_bytes(b"result"),
            "report.txt".into(),
            Some("text/plain".into()),
        ))],
    };
    schema
        .validate(&serialized(&result))
        .expect("joined subagent and attachment");
    let mut invalid = serialized(&result);
    invalid["status"] = json!("succeeded");
    assert!(
        !schema.is_valid(&invalid),
        "agent status differs from process status"
    );
}
