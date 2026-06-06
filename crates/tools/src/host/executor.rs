//! Host tool invocation runtime for CoreAgent tool calls.

use async_trait::async_trait;
use engine::{
    CoreAgentIoError, CoreAgentTools, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationRequest, ToolInvocationResult, ToolName,
};
use serde_json::Value;

use crate::{
    error::{ToolError, ToolResult},
    host::{
        context::HostToolContext,
        targets::{HostToolTargets, LOCAL_HOST_TARGET_ID},
        tools::HostTool,
    },
    runtime::{ToolCatalog, ToolExecutionMode, ToolInvocationOutput, ToolRuntime},
};

#[derive(Clone)]
pub struct InlineHostToolRuntime {
    targets: HostToolTargets,
    catalog: ToolCatalog,
}

impl InlineHostToolRuntime {
    pub fn new(ctx: HostToolContext, catalog: ToolCatalog) -> Self {
        Self::with_targets(HostToolTargets::local(ctx), catalog)
    }

    pub fn with_targets(targets: HostToolTargets, catalog: ToolCatalog) -> Self {
        Self { targets, catalog }
    }

    pub fn local_context(&self) -> Option<&HostToolContext> {
        self.targets.get(LOCAL_HOST_TARGET_ID)
    }

    pub fn targets(&self) -> &HostToolTargets {
        &self.targets
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    pub async fn invoke_call(
        &self,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let ctx = match self.resolve_call_context(call) {
            Ok(ctx) => ctx,
            Err(error) => return self.target_error_result(call, error).await,
        };
        ctx.fs.drain_tool_effects();
        let arguments = match self.read_arguments(ctx, call).await {
            Ok(arguments) => arguments,
            Err(error) => return self.failed_result(ctx, call, error).await,
        };

        match self
            .invoke_json_with_context(ctx, &call.tool_name, arguments)
            .await
        {
            Ok(output) => self.succeeded_result(ctx, call, output).await,
            Err(error) => self.failed_result(ctx, call, error).await,
        }
    }

    fn resolve_call_context(&self, call: &ToolInvocationRequest) -> ToolResult<&HostToolContext> {
        let Some(target) = call.execution_target.as_ref() else {
            return Err(ToolError::InvalidRequest {
                message: "host tool invocation requires execution target host:<id>".to_owned(),
            });
        };
        self.targets.resolve(target)
    }

    async fn read_arguments(
        &self,
        ctx: &HostToolContext,
        call: &ToolInvocationRequest,
    ) -> ToolResult<Value> {
        let bytes = ctx.blobs.read_bytes(&call.arguments_ref).await?;
        serde_json::from_slice(&bytes).map_err(|error| ToolError::InvalidRequest {
            message: format!("invalid JSON tool arguments: {error}"),
        })
    }

    async fn succeeded_result(
        &self,
        ctx: &HostToolContext,
        call: &ToolInvocationRequest,
        output: ToolInvocationOutput,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let output_bytes = serde_json::to_vec(&output.output_json)
            .map_err(|error| io_error(format!("failed to encode tool output: {error}")))?;
        let output_ref = self.put_blob(ctx, output_bytes).await?;
        let model_visible_output_ref = self
            .put_blob(
                ctx,
                truncate_bytes(
                    output.model_visible_text.into_bytes(),
                    ctx.limits.max_model_visible_output_bytes,
                ),
            )
            .await?;

        let mut effects = output.effects;
        effects.extend(ctx.fs.drain_tool_effects());

        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_output_ref: Some(model_visible_output_ref),
            error_ref: None,
            effects,
        })
    }

    async fn failed_result(
        &self,
        ctx: &HostToolContext,
        call: &ToolInvocationRequest,
        error: ToolError,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let error_text = format!("{error}");
        let error_ref = self
            .put_blob(
                ctx,
                truncate_bytes(
                    error_text.into_bytes(),
                    ctx.limits.max_model_visible_output_bytes,
                ),
            )
            .await?;

        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Failed,
            output_ref: None,
            model_visible_output_ref: Some(error_ref.clone()),
            error_ref: Some(error_ref),
            effects: ctx.fs.drain_tool_effects(),
        })
    }

    async fn target_error_result(
        &self,
        call: &ToolInvocationRequest,
        error: ToolError,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let Some(ctx) = self.targets.error_context() else {
            return Err(io_error(format!("{error}")));
        };
        self.failed_result(ctx, call, error).await
    }

    async fn put_blob(
        &self,
        ctx: &HostToolContext,
        bytes: Vec<u8>,
    ) -> Result<engine::BlobRef, CoreAgentIoError> {
        ctx.blobs
            .put_bytes(bytes)
            .await
            .map_err(|error| io_error(format!("failed to write tool blob: {error}")))
    }

    async fn invoke_json_with_context(
        &self,
        ctx: &HostToolContext,
        tool_name: &ToolName,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        let binding =
            self.catalog
                .get(tool_name)
                .ok_or_else(|| ToolError::UnsupportedCapability {
                    message: format!("unknown host tool: {tool_name}"),
                })?;
        if binding.execution != ToolExecutionMode::Inline {
            return Err(ToolError::UnsupportedCapability {
                message: format!(
                    "host tool {} is configured for {} and cannot be invoked inline",
                    tool_name, binding.activity_type
                ),
            });
        }
        let host_tool = HostTool::from_logical_id(&binding.logical_id).ok_or_else(|| {
            ToolError::UnsupportedCapability {
                message: format!("unsupported host tool binding: {}", binding.logical_id),
            }
        })?;
        host_tool.invoke_json(ctx, arguments).await
    }
}

#[async_trait]
impl ToolRuntime for InlineHostToolRuntime {
    async fn invoke_json(
        &self,
        tool_name: &ToolName,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        let ctx = self
            .targets
            .get(LOCAL_HOST_TARGET_ID)
            .ok_or_else(|| ToolError::InvalidRequest {
                message: format!(
                    "targetless host runtime invocation requires configured host target {LOCAL_HOST_TARGET_ID}"
                ),
            })?;
        ctx.fs.drain_tool_effects();
        let mut output = self
            .invoke_json_with_context(ctx, tool_name, arguments)
            .await?;
        output.effects.extend(ctx.fs.drain_tool_effects());
        Ok(output)
    }
}

#[async_trait]
impl CoreAgentTools for InlineHostToolRuntime {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
        let mut results = Vec::with_capacity(request.calls.len());
        for call in request.calls {
            results.push(self.invoke_call(&call).await?);
        }
        Ok(ToolInvocationBatchResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            results,
        })
    }
}

fn truncate_bytes(mut bytes: Vec<u8>, max_bytes: u64) -> Vec<u8> {
    let max_bytes = max_bytes as usize;
    if bytes.len() <= max_bytes {
        return bytes;
    }
    bytes.truncate(max_bytes);
    while std::str::from_utf8(&bytes).is_err() {
        bytes.pop();
    }
    bytes.extend_from_slice(b"\n[truncated]");
    bytes
}

fn io_error(message: impl Into<String>) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        BlobRef, RunId, SessionId, ToolBatchId, ToolCallId, ToolInvocationRequest, ToolName,
        TurnId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use serde_json::json;

    use super::*;
    use crate::host::fs::{FileSystem, FsPath, InMemoryFileSystem};
    use crate::runtime::{ToolCatalog, ToolTarget};
    use crate::toolset::{
        HostToolPresentation, ToolsetConfig, ToolsetEnvironment, resolve_toolset,
    };

    fn call(arguments_ref: BlobRef, tool_name: &str) -> ToolInvocationRequest {
        call_with_target(
            arguments_ref,
            tool_name,
            Some(HostToolTargets::local_execution_target()),
        )
    }

    fn call_with_target(
        arguments_ref: BlobRef,
        tool_name: &str,
        execution_target: Option<engine::ToolExecutionTarget>,
    ) -> ToolInvocationRequest {
        ToolInvocationRequest {
            call_id: ToolCallId::new("call-1"),
            tool_name: ToolName::new(tool_name),
            arguments_ref,
            execution_target,
        }
    }

    fn batch_request(call: ToolInvocationRequest) -> ToolInvocationBatchRequest {
        ToolInvocationBatchRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            calls: vec![call],
        }
    }

    fn workspace_catalog(ctx: &HostToolContext, api_kind: engine::ProviderApiKind) -> ToolCatalog {
        let target = ToolTarget::api_kind(api_kind);
        resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: Some(ctx),
            },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset")
        .catalog
    }

    fn catalog_for_operations_with_presentation(
        ctx: &HostToolContext,
        api_kind: engine::ProviderApiKind,
        presentation: HostToolPresentation,
        operations: impl IntoIterator<Item = crate::host::tools::HostToolOperation>,
    ) -> ToolCatalog {
        let target = ToolTarget::api_kind(api_kind);
        let mut config = ToolsetConfig::empty();
        config.host = crate::toolset::HostToolsetConfig::from_operations(operations);
        config.host.presentation = presentation;
        resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: Some(ctx),
            },
            &config,
        )
        .expect("toolset")
        .catalog
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_runtime_maps_tool_name_to_host_operation() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").expect("path"), b"hello".to_vec())
            .await
            .expect("write file");
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
        let catalog = workspace_catalog(&ctx, engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineHostToolRuntime::new(ctx, catalog);

        let output = runtime
            .invoke_json(
                &ToolName::new("read_file"),
                json!({ "path": "/file.txt", "offset": null, "limit": null }),
            )
            .await
            .expect("invoke tool");

        assert!(output.model_visible_text.contains("hello"));
        assert_eq!(output.output_json["text"], "hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_runtime_maps_claude_code_like_tool_arguments() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").expect("path"), b"hello".to_vec())
            .await
            .expect("write file");
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs);
        let catalog = catalog_for_operations_with_presentation(
            &ctx,
            engine::ProviderApiKind::AnthropicMessages,
            HostToolPresentation::ClaudeCodeLike,
            [crate::host::tools::HostToolOperation::ReadFile],
        );
        let runtime = InlineHostToolRuntime::new(ctx, catalog);

        let output = runtime
            .invoke_json(
                &ToolName::new("Read"),
                json!({ "file_path": "/file.txt", "offset": null, "limit": null }),
            )
            .await
            .expect("invoke tool");

        assert!(output.model_visible_text.contains("hello"));
        assert_eq!(output.output_json["text"], "hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_reads_arguments_and_writes_result_blobs() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").expect("path"), b"hello".to_vec())
            .await
            .expect("write file");
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
        let catalog = workspace_catalog(&ctx, engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineHostToolRuntime::new(ctx, catalog);
        let args_ref = blobs
            .put_bytes(br#"{"path":"/file.txt","offset":null,"limit":null}"#.to_vec())
            .await
            .expect("write args");

        let result = runtime
            .invoke_batch(batch_request(call(args_ref, "read_file")))
            .await
            .expect("invoke batch")
            .single_result()
            .expect("single result");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let visible_ref = result.model_visible_output_ref.expect("visible ref");
        let visible = blobs.read_text(&visible_ref).await.expect("visible text");
        assert!(visible.contains("hello"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_invokes_batch_and_writes_result_blobs() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").expect("path"), b"hello".to_vec())
            .await
            .expect("write file");
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
        let catalog = workspace_catalog(&ctx, engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineHostToolRuntime::new(ctx, catalog);
        let args_ref = blobs
            .put_bytes(br#"{"path":"/file.txt","offset":null,"limit":null}"#.to_vec())
            .await
            .expect("write args");

        let result = CoreAgentTools::invoke_batch(
            &runtime,
            ToolInvocationBatchRequest {
                session_id: SessionId::new("session-a"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call-1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref: args_ref,
                    execution_target: Some(HostToolTargets::local_execution_target()),
                }],
            },
        )
        .await
        .expect("invoke batch");

        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let visible_ref = result.results[0]
            .model_visible_output_ref
            .clone()
            .expect("visible ref");
        let visible = blobs.read_text(&visible_ref).await.expect("visible text");
        assert!(visible.contains("hello"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_resolves_host_target_id_to_context() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs_one = InMemoryFileSystem::full_access();
        fs_one
            .write_file(&FsPath::new("/file.txt").expect("path"), b"one".to_vec())
            .await
            .expect("write first file");
        let fs_two = InMemoryFileSystem::full_access();
        fs_two
            .write_file(&FsPath::new("/file.txt").expect("path"), b"two".to_vec())
            .await
            .expect("write second file");
        let ctx_one = HostToolContext::new(Arc::new(fs_one), None, blobs.clone());
        let ctx_two = HostToolContext::new(Arc::new(fs_two), None, blobs.clone());
        let catalog = workspace_catalog(&ctx_one, engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineHostToolRuntime::with_targets(
            HostToolTargets::new()
                .with_target("one", ctx_one)
                .with_target("two", ctx_two),
            catalog,
        );
        let args_ref = blobs
            .put_bytes(br#"{"path":"/file.txt","offset":null,"limit":null}"#.to_vec())
            .await
            .expect("write args");

        let result = runtime
            .invoke_batch(batch_request(call_with_target(
                args_ref,
                "read_file",
                Some(HostToolTargets::execution_target("two")),
            )))
            .await
            .expect("invoke batch")
            .single_result()
            .expect("single result");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let visible_ref = result.model_visible_output_ref.expect("visible ref");
        let visible = blobs.read_text(&visible_ref).await.expect("visible text");
        assert!(visible.contains("two"));
        assert!(!visible.contains("one"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_requires_host_execution_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
        let catalog = workspace_catalog(&ctx, engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineHostToolRuntime::new(ctx, catalog);

        let result = runtime
            .invoke_batch(batch_request(call_with_target(
                BlobRef::from_bytes(b"unused"),
                "read_file",
                None,
            )))
            .await
            .expect("invoke batch")
            .single_result()
            .expect("single result");

        assert_eq!(result.status, ToolCallStatus::Failed);
        let error_ref = result.error_ref.expect("error ref");
        let error = blobs.read_text(&error_ref).await.expect("error text");
        assert!(error.contains("requires execution target host:<id>"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_rejects_non_host_and_unknown_targets() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
        let catalog = workspace_catalog(&ctx, engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineHostToolRuntime::new(ctx, catalog);

        let cases = [
            (
                engine::ToolExecutionTarget::new("connector", "local"),
                "must use namespace host",
            ),
            (
                HostToolTargets::execution_target("missing"),
                "unknown host execution target id missing",
            ),
        ];

        for (target, expected) in cases {
            let result = runtime
                .invoke_batch(batch_request(call_with_target(
                    BlobRef::from_bytes(b"unused"),
                    "read_file",
                    Some(target),
                )))
                .await
                .expect("invoke batch")
                .single_result()
                .expect("single result");

            assert_eq!(result.status, ToolCallStatus::Failed);
            let error_ref = result.error_ref.expect("error ref");
            let error = blobs.read_text(&error_ref).await.expect("error text");
            assert!(
                error.contains(expected),
                "{error:?} did not contain {expected:?}"
            );
        }
    }
}
