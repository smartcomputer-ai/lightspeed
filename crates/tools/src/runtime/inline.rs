//! Inline tool invocation runtime for CoreAgent tool calls.

use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    CoreAgentIoError, CoreAgentTools, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationRequest, ToolInvocationResult, ToolName,
    storage::BlobStore,
};
use serde_json::Value;

use crate::{
    builtin::BuiltinTool,
    error::{ToolError, ToolResult},
    fs::FsToolContext,
    limits::ToolLimits,
    runtime::{ToolBinding, ToolCatalog, ToolExecutionMode, ToolInvocationOutput, ToolRuntime},
    targets::{ResolvedToolContext, SESSION_FS_TARGET_ID, ToolTargets},
    web::fetch::{WEB_FETCH_LOGICAL_ID, invoke_web_fetch},
};

#[derive(Clone)]
pub struct InlineToolRuntime {
    targets: ToolTargets,
    catalog: ToolCatalog,
    blobs: Arc<dyn BlobStore>,
    limits: ToolLimits,
}

impl InlineToolRuntime {
    pub fn with_session_filesystem(ctx: FsToolContext, catalog: ToolCatalog) -> Self {
        let blobs = ctx.blobs.clone();
        let limits = ctx.limits;
        let mut targets = ToolTargets::new();
        targets.insert_fs_context(SESSION_FS_TARGET_ID, ctx);
        Self::with_targets_and_blob_store(targets, blobs, limits, catalog)
    }

    pub fn with_targets(targets: ToolTargets, catalog: ToolCatalog) -> Self {
        let blobs = targets
            .blob_store()
            .expect("InlineToolRuntime::with_targets requires at least one target");
        let limits = targets
            .limits()
            .expect("InlineToolRuntime::with_targets requires at least one target");
        Self::with_targets_and_blob_store(targets.clone(), blobs, limits, catalog)
    }

    pub fn with_targets_and_blob_store(
        targets: ToolTargets,
        blobs: Arc<dyn BlobStore>,
        limits: ToolLimits,
        catalog: ToolCatalog,
    ) -> Self {
        Self {
            targets,
            catalog,
            blobs,
            limits,
        }
    }

    pub fn session_fs_context(&self) -> Option<&crate::fs::FsToolContext> {
        self.targets.get(SESSION_FS_TARGET_ID)
    }

    pub fn targets(&self) -> &ToolTargets {
        &self.targets
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    pub async fn invoke_call(
        &self,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let binding = match self.resolve_binding(call) {
            Ok(binding) => binding,
            Err(error) => return self.failed_result_without_context(call, error).await,
        };
        if binding.logical_id == WEB_FETCH_LOGICAL_ID {
            let arguments = match self.read_arguments_from_blobs(call).await {
                Ok(arguments) => arguments,
                Err(error) => return self.failed_result_without_context(call, error).await,
            };
            return match self
                .invoke_json_with_binding(None, &binding, &call.tool_name, arguments)
                .await
            {
                Ok(output) => self.succeeded_result_without_context(call, output).await,
                Err(error) => self.failed_result_without_context(call, error).await,
            };
        }

        let ctx = match self.resolve_call_context(call, &binding) {
            Ok(ctx) => ctx,
            Err(error) => return self.target_error_result(call, error).await,
        };
        ctx.drain_tool_effects();
        let arguments = match self.read_arguments(ctx, call).await {
            Ok(arguments) => arguments,
            Err(error) => return self.failed_result(ctx, call, error).await,
        };

        match self
            .invoke_json_with_binding(Some(ctx), &binding, &call.tool_name, arguments)
            .await
        {
            Ok(output) => self.succeeded_result(ctx, call, output).await,
            Err(error) => self.failed_result(ctx, call, error).await,
        }
    }

    fn resolve_binding(&self, call: &ToolInvocationRequest) -> ToolResult<ToolBinding> {
        self.catalog
            .get(&call.tool_name)
            .cloned()
            .ok_or_else(|| ToolError::UnsupportedCapability {
                message: format!("unknown tool: {}", call.tool_name),
            })
    }

    fn resolve_call_context(
        &self,
        call: &ToolInvocationRequest,
        binding: &ToolBinding,
    ) -> ToolResult<ResolvedToolContext<'_>> {
        let Some(target) = call.execution_target.as_ref() else {
            return Err(ToolError::InvalidRequest {
                message: "tool invocation requires an execution target".to_owned(),
            });
        };
        if let Some(builtin_tool) = BuiltinTool::from_logical_id(&binding.logical_id) {
            let expected_namespace = builtin_tool.target_namespace();
            if target.namespace != expected_namespace {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "tool {} requires execution target namespace {}, got {}",
                        call.tool_name, expected_namespace, target.namespace
                    ),
                });
            }
        }
        self.targets.resolve(target)
    }

    async fn read_arguments(
        &self,
        ctx: ResolvedToolContext<'_>,
        call: &ToolInvocationRequest,
    ) -> ToolResult<Value> {
        let bytes = ctx.blobs().read_bytes(&call.arguments_ref).await?;
        serde_json::from_slice(&bytes).map_err(|error| ToolError::InvalidRequest {
            message: format!("invalid JSON tool arguments: {error}"),
        })
    }

    async fn read_arguments_from_blobs(&self, call: &ToolInvocationRequest) -> ToolResult<Value> {
        let bytes = self.blobs.read_bytes(&call.arguments_ref).await?;
        serde_json::from_slice(&bytes).map_err(|error| ToolError::InvalidRequest {
            message: format!("invalid JSON tool arguments: {error}"),
        })
    }

    async fn succeeded_result(
        &self,
        ctx: ResolvedToolContext<'_>,
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
                    ctx.limits().max_model_visible_output_bytes,
                ),
            )
            .await?;

        let mut effects = output.effects;
        effects.extend(ctx.drain_tool_effects());

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
        ctx: ResolvedToolContext<'_>,
        call: &ToolInvocationRequest,
        error: ToolError,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let error_text = format!("{error}");
        let error_ref = self
            .put_blob(
                ctx,
                truncate_bytes(
                    error_text.into_bytes(),
                    ctx.limits().max_model_visible_output_bytes,
                ),
            )
            .await?;

        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Failed,
            output_ref: None,
            model_visible_output_ref: Some(error_ref.clone()),
            error_ref: Some(error_ref),
            effects: ctx.drain_tool_effects(),
        })
    }

    async fn succeeded_result_without_context(
        &self,
        call: &ToolInvocationRequest,
        output: ToolInvocationOutput,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let output_bytes = serde_json::to_vec(&output.output_json)
            .map_err(|error| io_error(format!("failed to encode tool output: {error}")))?;
        let output_ref = self.put_blob_bytes(output_bytes).await?;
        let model_visible_output_ref = self
            .put_blob_bytes(truncate_bytes(
                output.model_visible_text.into_bytes(),
                self.limits.max_model_visible_output_bytes,
            ))
            .await?;

        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_output_ref: Some(model_visible_output_ref),
            error_ref: None,
            effects: output.effects,
        })
    }

    async fn failed_result_without_context(
        &self,
        call: &ToolInvocationRequest,
        error: ToolError,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let error_ref = self
            .put_blob_bytes(truncate_bytes(
                error.to_string().into_bytes(),
                self.limits.max_model_visible_output_bytes,
            ))
            .await?;

        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Failed,
            output_ref: None,
            model_visible_output_ref: Some(error_ref.clone()),
            error_ref: Some(error_ref),
            effects: Vec::new(),
        })
    }

    async fn target_error_result(
        &self,
        call: &ToolInvocationRequest,
        error: ToolError,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        self.failed_result_without_context(call, error).await
    }

    async fn put_blob(
        &self,
        ctx: ResolvedToolContext<'_>,
        bytes: Vec<u8>,
    ) -> Result<engine::BlobRef, CoreAgentIoError> {
        ctx.blobs()
            .put_bytes(bytes)
            .await
            .map_err(|error| io_error(format!("failed to write tool blob: {error}")))
    }

    async fn put_blob_bytes(&self, bytes: Vec<u8>) -> Result<engine::BlobRef, CoreAgentIoError> {
        self.blobs
            .put_bytes(bytes)
            .await
            .map_err(|error| io_error(format!("failed to write tool blob: {error}")))
    }

    async fn invoke_json_with_binding(
        &self,
        ctx: Option<ResolvedToolContext<'_>>,
        binding: &ToolBinding,
        tool_name: &ToolName,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        if binding.execution != ToolExecutionMode::Inline {
            return Err(ToolError::UnsupportedCapability {
                message: format!(
                    "tool {} is configured for {} and cannot be invoked inline",
                    tool_name, binding.activity_type
                ),
            });
        }
        if binding.logical_id == WEB_FETCH_LOGICAL_ID {
            return invoke_web_fetch(arguments).await;
        }
        let builtin_tool = BuiltinTool::from_logical_id(&binding.logical_id).ok_or_else(|| {
            ToolError::UnsupportedCapability {
                message: format!("unsupported tool binding: {}", binding.logical_id),
            }
        })?;
        let ctx = ctx.ok_or_else(|| ToolError::InvalidRequest {
            message: format!("tool {tool_name} requires an execution target"),
        })?;
        builtin_tool.invoke_json(ctx, arguments).await
    }
}

#[async_trait]
impl ToolRuntime for InlineToolRuntime {
    async fn invoke_json(
        &self,
        tool_name: &ToolName,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        let binding =
            self.catalog
                .get(tool_name)
                .ok_or_else(|| ToolError::UnsupportedCapability {
                    message: format!("unknown tool: {tool_name}"),
                })?;
        if binding.logical_id == WEB_FETCH_LOGICAL_ID {
            return self
                .invoke_json_with_binding(None, binding, tool_name, arguments)
                .await;
        }
        let builtin_tool = BuiltinTool::from_logical_id(&binding.logical_id).ok_or_else(|| {
            ToolError::UnsupportedCapability {
                message: format!("unsupported tool binding: {}", binding.logical_id),
            }
        })?;
        let ctx = self
            .targets
            .default_for_namespace(builtin_tool.target_namespace())?;
        ctx.drain_tool_effects();
        let mut output = self
            .invoke_json_with_binding(Some(ctx), binding, tool_name, arguments)
            .await?;
        output.effects.extend(ctx.drain_tool_effects());
        Ok(output)
    }
}

#[async_trait]
impl CoreAgentTools for InlineToolRuntime {
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
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use engine::{
        BlobRef, RunId, SessionId, ToolBatchId, ToolCallId, ToolInvocationRequest, ToolName,
        TurnId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use serde_json::json;

    use super::*;
    use crate::builtin::BuiltinToolOperation;
    use crate::environment::EnvironmentToolContext;
    use crate::environment::process::{
        ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput, ProcessRequest,
        ProcessStatus, StreamOutput, WriteProcessStdinRequest,
    };
    use crate::fs::{FileSystem, FsPath, InMemoryFileSystem};
    use crate::runtime::{ToolCatalog, ToolTarget};
    use crate::toolset::{
        BuiltinToolPresentation, ToolsetConfig, ToolsetEnvironment, resolve_toolset,
    };
    use crate::web::fetch::WebFetchToolConfig;

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
                message: "not needed".to_owned(),
            })
        }
    }

    fn call(arguments_ref: BlobRef, tool_name: &str) -> ToolInvocationRequest {
        call_with_target(
            arguments_ref,
            tool_name,
            Some(ToolTargets::session_fs_execution_target()),
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

    fn workspace_catalog(api_kind: engine::ProviderApiKind) -> ToolCatalog {
        let target = ToolTarget::api_kind(api_kind);
        resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset")
        .catalog
    }

    fn catalog_for_operations_with_presentation(
        api_kind: engine::ProviderApiKind,
        presentation: BuiltinToolPresentation,
        operations: impl IntoIterator<Item = BuiltinToolOperation>,
    ) -> ToolCatalog {
        let target = ToolTarget::api_kind(api_kind);
        let mut config = ToolsetConfig::empty();
        config.builtin = crate::toolset::BuiltinToolsetConfig::from_operations(operations);
        config.builtin.presentation = presentation;
        resolve_toolset(ToolsetEnvironment { target: &target }, &config)
            .expect("toolset")
            .catalog
    }

    fn fs_context(fs: impl FileSystem + 'static, blobs: Arc<dyn BlobStore>) -> FsToolContext {
        FsToolContext::new(Arc::new(fs), blobs)
    }

    fn runtime_with_session_fs(
        fs: impl FileSystem + 'static,
        blobs: Arc<dyn BlobStore>,
        catalog: ToolCatalog,
    ) -> InlineToolRuntime {
        InlineToolRuntime::with_session_filesystem(fs_context(fs, blobs), catalog)
    }

    fn web_fetch_catalog() -> ToolCatalog {
        let target = ToolTarget::api_kind(engine::ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.web_fetch = WebFetchToolConfig::enabled();
        resolve_toolset(ToolsetEnvironment { target: &target }, &config)
            .expect("toolset")
            .catalog
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_runtime_maps_tool_name_to_builtin_operation() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").expect("path"), b"hello".to_vec())
            .await
            .expect("write file");
        let catalog = workspace_catalog(engine::ProviderApiKind::OpenAiResponses);
        let runtime = runtime_with_session_fs(fs, blobs.clone(), catalog);

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
        let catalog = catalog_for_operations_with_presentation(
            engine::ProviderApiKind::AnthropicMessages,
            BuiltinToolPresentation::ClaudeCodeLike,
            [BuiltinToolOperation::ReadFile],
        );
        let runtime = runtime_with_session_fs(fs, blobs, catalog);

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
        let catalog = workspace_catalog(engine::ProviderApiKind::OpenAiResponses);
        let runtime = runtime_with_session_fs(fs, blobs.clone(), catalog);
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
        let catalog = workspace_catalog(engine::ProviderApiKind::OpenAiResponses);
        let runtime = runtime_with_session_fs(fs, blobs.clone(), catalog);
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
                    execution_target: Some(ToolTargets::session_fs_execution_target()),
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
    async fn targetless_web_fetch_does_not_require_host_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let runtime = InlineToolRuntime::with_targets_and_blob_store(
            ToolTargets::new(),
            blobs.clone(),
            ToolLimits::default(),
            web_fetch_catalog(),
        );
        let args_ref = blobs
            .put_bytes(br#"{"url":"http://127.0.0.1:1/","max_chars":1000}"#.to_vec())
            .await
            .expect("write args");

        let result = runtime
            .invoke_batch(batch_request(call_with_target(args_ref, "web_fetch", None)))
            .await
            .expect("invoke batch")
            .single_result()
            .expect("single result");

        assert_eq!(result.status, ToolCallStatus::Failed);
        let error_ref = result.error_ref.expect("error ref");
        let error = blobs.read_text(&error_ref).await.expect("error text");
        assert!(error.contains("non-public"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_resolves_fs_target_id_to_context() {
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
        let ctx_one = fs_context(fs_one, blobs.clone());
        let ctx_two = fs_context(fs_two, blobs.clone());
        let catalog = workspace_catalog(engine::ProviderApiKind::OpenAiResponses);
        let runtime = InlineToolRuntime::with_targets(
            ToolTargets::new()
                .with_fs_context("one", ctx_one)
                .with_fs_context("two", ctx_two),
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
                Some(ToolTargets::fs_execution_target("two")),
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
    async fn core_tools_routes_process_tools_to_local_environment_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let process = Arc::new(RecordingProcessExecutor::default());
        let process_ctx: Arc<dyn ProcessExecutor> = process.clone();
        let fs_ctx = fs_context(InMemoryFileSystem::full_access(), blobs.clone());
        let env_ctx = EnvironmentToolContext::new(Some(process_ctx), blobs.clone());
        let catalog = catalog_for_operations_with_presentation(
            engine::ProviderApiKind::OpenAiResponses,
            BuiltinToolPresentation::Canonical,
            [BuiltinToolOperation::RunProcess],
        );
        let runtime = InlineToolRuntime::with_targets(
            ToolTargets::new()
                .with_session_fs_context(fs_ctx)
                .with_local_environment_context(env_ctx),
            catalog,
        );
        let args_ref = blobs
            .put_bytes(br#"{"argv":["echo","hello"]}"#.to_vec())
            .await
            .expect("write args");

        let result = runtime
            .invoke_batch(batch_request(call_with_target(
                args_ref,
                "exec_command",
                Some(ToolTargets::local_environment_execution_target()),
            )))
            .await
            .expect("invoke batch")
            .single_result()
            .expect("single result");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let visible_ref = result.model_visible_output_ref.expect("visible ref");
        let visible = blobs.read_text(&visible_ref).await.expect("visible text");
        assert!(visible.contains("ok"));
        let requests = process.requests.lock().expect("lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].argv, ["echo", "hello"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_fail_process_tools_without_environment_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs_ctx = fs_context(InMemoryFileSystem::full_access(), blobs.clone());
        let catalog = catalog_for_operations_with_presentation(
            engine::ProviderApiKind::OpenAiResponses,
            BuiltinToolPresentation::Canonical,
            [BuiltinToolOperation::RunProcess],
        );
        let runtime = InlineToolRuntime::with_targets(
            ToolTargets::new().with_session_fs_context(fs_ctx),
            catalog,
        );
        let args_ref = blobs
            .put_bytes(br#"{"argv":["echo","hello"]}"#.to_vec())
            .await
            .expect("write args");

        let result = runtime
            .invoke_batch(batch_request(call_with_target(
                args_ref,
                "exec_command",
                Some(ToolTargets::local_environment_execution_target()),
            )))
            .await
            .expect("invoke batch")
            .single_result()
            .expect("single result");

        assert_eq!(result.status, ToolCallStatus::Failed);
        let error_ref = result.error_ref.expect("error ref");
        let error = blobs.read_text(&error_ref).await.expect("error text");
        assert!(error.contains("no execution environment target is configured"));
        assert!(error.contains("process tools require an active environment"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_requires_execution_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        let catalog = workspace_catalog(engine::ProviderApiKind::OpenAiResponses);
        let runtime = runtime_with_session_fs(fs, blobs.clone(), catalog);

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
        assert!(error.contains("requires an execution target"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn core_tools_rejects_wrong_namespace_and_unknown_targets() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        let catalog = workspace_catalog(engine::ProviderApiKind::OpenAiResponses);
        let runtime = runtime_with_session_fs(fs, blobs.clone(), catalog);

        let cases = [
            (
                ToolTargets::local_environment_execution_target(),
                "requires execution target namespace fs",
            ),
            (
                ToolTargets::fs_execution_target("missing"),
                "unknown filesystem tool execution target id missing",
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
