use std::collections::BTreeMap;

use harness::{
    PromiseControlArgumentCallFacts, PromiseControlArgumentFacts, PromiseControlArgumentRequest,
    PromiseControlKind, ToolBatchOutcome, ToolCallStatus, ToolInvocationResult, storage::BlobStore,
};
use temporalio_sdk::activities::ActivityError;
use tools::concurrency::{CancelArgs, DetachArgs};

use crate::worker::{
    AwaitEnvironmentReadyActivityRequest, AwaitEnvironmentReadyActivityResult, ToolCallExecution,
    ToolInvokeBatchActivityRequest, ToolInvokeCallActivityResult,
};

use super::{
    common::{activity_error, failed_tool_batch_result},
    state::ToolActivityDeps,
};

pub(super) async fn prepare_promise_controls(
    blobs: &dyn BlobStore,
    request: PromiseControlArgumentRequest,
) -> Result<PromiseControlArgumentFacts, ActivityError> {
    if request.version != PromiseControlArgumentRequest::VERSION {
        return Err(activity_error(anyhow::anyhow!(
            "unsupported promise-control argument request version {}",
            request.version
        )));
    }
    let mut calls = Vec::with_capacity(request.calls.len());
    for call in request.calls {
        let parsed = match blobs.read_bytes(&call.arguments_ref).await {
            Ok(bytes) => match call.kind {
                PromiseControlKind::Cancel => serde_json::from_slice::<CancelArgs>(&bytes)
                    .ok()
                    .and_then(|args| args.validated_promise_ids().ok()),
                PromiseControlKind::Detach => serde_json::from_slice::<DetachArgs>(&bytes)
                    .ok()
                    .and_then(|args| args.validated_promise_ids().ok()),
            },
            Err(_) => None,
        };
        calls.push(match parsed {
            Some(promise_ids) => PromiseControlArgumentCallFacts::Parsed {
                call_id: call.call_id,
                promise_ids,
            },
            None => PromiseControlArgumentCallFacts::Invalid {
                call_id: call.call_id,
            },
        });
    }
    Ok(PromiseControlArgumentFacts {
        version: PromiseControlArgumentFacts::VERSION,
        calls,
    })
}

/// Resolve only host-callable presentations from the exact target seen by the
/// model. The session intersects these handles with its current authority.
pub(super) async fn prepare_code_tool_scope(
    deps: &ToolActivityDeps,
    request: temporal_workflow::CodeToolPrepareScopeActivityRequest,
) -> Result<temporal_workflow::CodeToolPrepareScopeActivityResult, ActivityError> {
    let target = tools::runtime::ToolTarget::from(&request.model);
    let resolved = tools::callable::resolve(deps.blobs.as_ref(), &target, &request.tools)
        .await
        .map_err(activity_error)?;
    let mut bindings = BTreeMap::new();
    let mut names = std::collections::BTreeSet::new();
    let mut runtime_tool_count = 0;
    let mut insert = |tool_id: harness::ToolId, tool_name: harness::ToolName| {
        if !names.insert(tool_name.clone()) {
            return Err(activity_error(anyhow::anyhow!(
                "duplicate exposed code tool name {tool_name}"
            )));
        }
        let input = serde_json::to_vec(&(&tool_id, &tool_name)).map_err(activity_error)?;
        let handle = format!("binding:{}", harness::BlobRef::from_bytes(&input));
        bindings.insert(handle, harness::CodeToolBinding { tool_id, tool_name });
        Ok(())
    };
    for tool in resolved {
        if let tools::callable::ResolvedToolKind::RemoteMcp(spec) = &tool.kind {
            if spec.execution == harness::RemoteMcpExecution::Runtime
                && spec.exposure == harness::RemoteMcpExposure::Inject
            {
                let executor = deps.runtime_mcp.as_ref().ok_or_else(|| {
                    activity_error(anyhow::anyhow!("runtime MCP inventory is unavailable"))
                })?;
                for (name, _) in executor
                    .injected_tools(spec, &tool.name, &mut runtime_tool_count)
                    .await
                    .map_err(activity_error)?
                {
                    insert(tool.id.clone(), harness::ToolName::new(name))?;
                }
            }
        } else if let Some(callable) = tool.into_callable() {
            insert(callable.tool_id, callable.definition.name)?;
        }
    }
    Ok(temporal_workflow::CodeToolPrepareScopeActivityResult { bindings })
}

/// Execute the calls of a batch that must commit together: workflow-tool calls
/// and `await`. A runtime failure becomes an ordinary failed result for each of
/// them.
pub(super) async fn invoke_batch(
    deps: &ToolActivityDeps,
    request: ToolInvokeBatchActivityRequest,
) -> Result<ToolBatchOutcome, ActivityError> {
    let request = request.request;
    match deps.tools.invoke_batch(request.clone()).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => failed_tool_batch_result(deps.blobs.as_ref(), &request, error.to_string())
            .await
            .map(ToolBatchOutcome::completed)
            .map_err(activity_error),
    }
}

/// Outcome of one runtime MCP call executed by this activity wrapper.
enum RuntimeMcpCallOutcome {
    Completed(ToolInvocationResult),
    /// The call must receive a single-use run-owned decision before the
    /// worker performs any MCP wire I/O.
    NeedsApproval {
        subject: harness::ApprovalSubject,
    },
}

/// Execute one runtime MCP call under its class operation deadline. Tool-level
/// failures and an elapsed deadline become an ordinary terminal failed result;
/// only blob-store failures fail the activity.
async fn invoke_runtime_mcp_call(
    deps: &ToolActivityDeps,
    request: &harness::ToolInvocationCallRequest,
) -> Result<RuntimeMcpCallOutcome, ActivityError> {
    let deadline = temporal_workflow::tool_call_operation_timeout(request.execution.class);
    let started = std::time::Instant::now();
    let stamp = |mut result: ToolInvocationResult| {
        result.duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        result
    };
    let failed = |message: String| async move {
        super::common::failed_tool_call_result(deps.blobs.as_ref(), request, message)
            .await
            .map(|result| RuntimeMcpCallOutcome::Completed(stamp(result)))
            .map_err(activity_error)
    };
    match tokio::time::timeout(deadline, execute_runtime_mcp_call(deps, request)).await {
        Ok(Ok(RuntimeMcpCallOutcome::Completed(result))) => {
            Ok(RuntimeMcpCallOutcome::Completed(stamp(result)))
        }
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => failed(error.to_string()).await,
        Err(_elapsed) => {
            failed(format!(
                "tool call exceeded its {}s operation deadline",
                deadline.as_secs()
            ))
            .await
        }
    }
}

async fn execute_runtime_mcp_call(
    deps: &ToolActivityDeps,
    request: &harness::ToolInvocationCallRequest,
) -> Result<RuntimeMcpCallOutcome, harness::CoreAgentIoError> {
    let executor = deps
        .runtime_mcp
        .as_ref()
        .ok_or_else(|| harness::CoreAgentIoError::Failed {
            message: "runtime MCP execution is unavailable on this worker".to_owned(),
        })?;
    let argument_bytes = deps
        .blobs
        .read_bytes(&request.call.arguments_ref)
        .await
        .map_err(|error| harness::CoreAgentIoError::Failed {
            message: format!("read runtime MCP arguments: {error}"),
        })?;
    let arguments = serde_json::from_slice(&argument_bytes).map_err(|error| {
        harness::CoreAgentIoError::Failed {
            message: format!("runtime MCP arguments are invalid JSON: {error}"),
        }
    })?;
    match executor.execute(request, arguments).await {
        Ok(crate::worker::mcp::RuntimeMcpExecutionOutcome::NeedsApproval { subject }) => {
            Ok(RuntimeMcpCallOutcome::NeedsApproval { subject })
        }
        Ok(crate::worker::mcp::RuntimeMcpExecutionOutcome::Completed {
            output,
            visible,
            is_error,
            assets,
            media,
        }) => store_runtime_mcp_result(deps, request, output, visible, is_error, assets, media)
            .await
            .map(RuntimeMcpCallOutcome::Completed),
        Err(message) => Err(harness::CoreAgentIoError::Failed { message }),
    }
}

/// Store a completed runtime MCP result: its assets and full output by reference,
/// and its model-visible text under the projection budget every inline tool uses.
async fn store_runtime_mcp_result(
    deps: &ToolActivityDeps,
    request: &harness::ToolInvocationCallRequest,
    mut output: serde_json::Value,
    visible: String,
    is_error: bool,
    assets: Vec<crate::worker::mcp::RuntimeMcpAsset>,
    media: Vec<crate::worker::mcp::RuntimeMcpMedia>,
) -> Result<ToolInvocationResult, harness::CoreAgentIoError> {
    let mut asset_refs = Vec::with_capacity(assets.len());
    for asset in assets {
        let blob_ref = deps.blobs.put_bytes(asset.bytes).await.map_err(|error| {
            harness::CoreAgentIoError::Failed {
                message: format!(
                    "store runtime MCP {} asset ({}): {error}",
                    asset.kind,
                    asset.media_type.as_deref().unwrap_or("unknown media type")
                ),
            }
        })?;
        asset_refs.push(blob_ref);
    }
    crate::worker::mcp::attach_mcp_asset_refs(&mut output, &asset_refs);
    let output_ref = deps
        .blobs
        .put_bytes(serde_json::to_vec(&output).map_err(|error| {
            harness::CoreAgentIoError::Failed {
                message: format!("encode runtime MCP result: {error}"),
            }
        })?)
        .await
        .map_err(|error| harness::CoreAgentIoError::Failed {
            message: format!("store runtime MCP result: {error}"),
        })?;
    // The output embeds the asset refs it was just given; the edges
    // keep the assets alive as long as the output.
    harness::storage::record_contains_edges(
        deps.blob_graph.as_deref(),
        &output_ref,
        asset_refs.iter().cloned(),
    )
    .await
    .map_err(|error| harness::CoreAgentIoError::Failed {
        message: format!("record runtime MCP asset edges: {error}"),
    })?;
    // The server decides the size of its text, and an oversized result would sit
    // in context beyond the reach of compaction. Only the visible text is cut:
    // the full output stays behind `output_ref`.
    let projection = tools::limits::projected(
        visible.into_bytes(),
        tools::limits::ToolLimits::default().max_model_visible_output_bytes,
    );
    let visible_ref = deps
        .blobs
        .put_bytes(projection.bytes)
        .await
        .map_err(|error| harness::CoreAgentIoError::Failed {
            message: format!("store runtime MCP visible result: {error}"),
        })?;
    let status = if is_error {
        ToolCallStatus::Failed
    } else {
        ToolCallStatus::Succeeded
    };
    // Admitted media follows the tool result as ordinary user-role
    // media entries, in the order the visible text announces them.
    let mut model_visible_context_entries = Vec::with_capacity(1 + media.len());
    model_visible_context_entries.push(ToolInvocationResult::tool_result_context_entry(
        &request.call.call_id,
        status,
        visible_ref.clone(),
    ));
    let mut attachments = Vec::with_capacity(media.len());
    for item in media {
        let Some(blob_ref) = asset_refs.get(item.asset_index) else {
            continue;
        };
        let descriptor = harness::media::MediaDescriptor {
            handle: harness::media::media_handle(blob_ref),
            content_ref: blob_ref.clone(),
            media_type: item.media_type,
            kind: item.kind,
            name: item.name,
        };
        model_visible_context_entries.push(descriptor.context_entry());
        attachments.push(harness::Attachment::Media(descriptor));
    }
    Ok(ToolInvocationResult {
        attachments,
        duration_ms: None,
        output_bytes: Some(projection.output_bytes),
        truncated: projection.truncated,
        call_id: request.call.call_id.clone(),
        status,
        output_ref: Some(output_ref),
        model_visible_context_entries,
        error_ref: is_error.then_some(visible_ref),
        effects: Vec::new(),
    })
}

/// Execute one call of an admitted tool batch under its class operation
/// deadline. Tool-level failures and an elapsed operation deadline return an
/// ordinary terminal failed result; only blob-store failures fail the
/// activity. A call whose active environment is still provisioning does not
/// run and reports `EnvironmentNotReady` so the workflow can wait outside
/// this deadline.
pub(super) async fn invoke_call(
    deps: &ToolActivityDeps,
    request: crate::worker::ToolInvokeCallActivityRequest,
) -> Result<ToolInvokeCallActivityResult, ActivityError> {
    let request = request.request;
    if request.call.remote_mcp.is_some() {
        return Ok(match invoke_runtime_mcp_call(deps, &request).await? {
            RuntimeMcpCallOutcome::Completed(result) => {
                ToolInvokeCallActivityResult::Completed { result }
            }
            RuntimeMcpCallOutcome::NeedsApproval { subject } => {
                ToolInvokeCallActivityResult::NeedsApproval { subject }
            }
        });
    }
    let deadline = temporal_workflow::tool_call_operation_timeout(request.execution.class);
    let execution = async {
        match deps.hosted.as_ref() {
            Some(hosted) => hosted.invoke_call_execution(request.clone()).await,
            None => deps
                .tools
                .invoke_call(request.clone())
                .await
                .map(ToolCallExecution::Completed),
        }
    };
    let started = std::time::Instant::now();
    let stamp = |mut result: ToolInvocationResult| {
        result.duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        result
    };
    match tokio::time::timeout(deadline, execution).await {
        Ok(Ok(ToolCallExecution::Completed(result))) => {
            Ok(ToolInvokeCallActivityResult::Completed {
                result: stamp(result),
            })
        }
        Ok(Ok(ToolCallExecution::EnvironmentNotReady { environment_id, .. })) => {
            Ok(ToolInvokeCallActivityResult::EnvironmentNotReady { environment_id })
        }
        Ok(Err(error)) => {
            super::common::failed_tool_call_result(deps.blobs.as_ref(), &request, error.to_string())
                .await
                .map(|result| ToolInvokeCallActivityResult::Completed {
                    result: stamp(result),
                })
                .map_err(activity_error)
        }
        Err(_elapsed) => super::common::failed_tool_call_result(
            deps.blobs.as_ref(),
            &request,
            format!(
                "tool call exceeded its {}s operation deadline",
                deadline.as_secs()
            ),
        )
        .await
        .map(|result| ToolInvokeCallActivityResult::Completed {
            result: stamp(result),
        })
        .map_err(activity_error),
    }
}

/// Execute a call admitted in a code tool execution scope. Ordinary calls retain
/// the same operation deadlines and backend path as model calls. Workflow
/// preparation and promise waits return facts to the session owner instead of
/// consuming or parking its outer batch.
pub(super) async fn invoke_code_tool(
    deps: &ToolActivityDeps,
    request: temporal_workflow::CodeToolInvokeActivityRequest,
) -> Result<temporal_workflow::CodeToolInvokeActivityResult, ActivityError> {
    use temporal_workflow::CodeToolInvokeActivityResult as Outcome;

    let mut request = request.request;
    if let Err(message) = validate_code_tool_presentation(&request.call) {
        return Ok(Outcome::Completed {
            result: super::common::failed_tool_call_result(deps.blobs.as_ref(), &request, message)
                .await
                .map_err(activity_error)?,
        });
    }
    // A prior model-call approval must never authorize a code tool invocation.
    // The runtime MCP executor reports NeedsApproval before performing the call;
    // below that becomes a terminal failed result, with no approval lifecycle.
    if let Some(remote) = &mut request.call.remote_mcp {
        match remote {
            harness::RemoteMcpCallRuntime::Injected {
                approval_decision, ..
            }
            | harness::RemoteMcpCallRuntime::Search {
                approval_decision, ..
            } => {
                *approval_decision = None;
            }
        }
    }
    let special = request.call.workflow_tool.is_some()
        || request
            .call
            .tool_id
            .as_ref()
            .is_some_and(|id| id.as_str() == harness::AWAIT_TOOL_ID);
    if !special || request.call.remote_mcp.is_some() {
        return Ok(
            match invoke_call(
                deps,
                crate::worker::ToolInvokeCallActivityRequest {
                    request: request.clone(),
                },
            )
            .await?
            {
                ToolInvokeCallActivityResult::Completed { result } => Outcome::Completed { result },
                ToolInvokeCallActivityResult::EnvironmentNotReady { environment_id } => {
                    Outcome::EnvironmentNotReady { environment_id }
                }
                ToolInvokeCallActivityResult::NeedsApproval { .. } => Outcome::Completed {
                    result: super::common::failed_tool_call_result(
                        deps.blobs.as_ref(),
                        &request,
                        "approval-required tools are unavailable in code tool executions"
                            .to_owned(),
                    )
                    .await
                    .map_err(activity_error)?,
                },
            },
        );
    }

    let deadline = temporal_workflow::tool_call_operation_timeout(request.execution.class);
    let started = std::time::Instant::now();
    let execution = async {
        let hosted = deps
            .hosted
            .as_ref()
            .ok_or_else(|| harness::CoreAgentIoError::Failed {
                message:
                    "workflow tools and waits invoked from code require the hosted tool runtime"
                        .to_owned(),
            })?;
        hosted
            .invoke_code_tool_call_execution(request.clone())
            .await
    };
    let result = match tokio::time::timeout(deadline, execution).await {
        Ok(Ok(crate::worker::session_tools::CodeToolCallExecution::Completed(result))) => result,
        Ok(Ok(crate::worker::session_tools::CodeToolCallExecution::Deferred(spec))) => {
            return Ok(Outcome::Deferred { spec });
        }
        Ok(Ok(crate::worker::session_tools::CodeToolCallExecution::EnvironmentNotReady {
            environment_id,
        })) => return Ok(Outcome::EnvironmentNotReady { environment_id }),
        outcome => {
            let message = match outcome {
                Ok(Err(error)) => error.to_string(),
                Err(_) => format!(
                    "tool call exceeded its {}s operation deadline",
                    deadline.as_secs()
                ),
                Ok(Ok(_)) => unreachable!("successful code tool outcomes handled above"),
            };
            super::common::failed_tool_call_result(deps.blobs.as_ref(), &request, message)
                .await
                .map_err(activity_error)?
        }
    };
    let mut result = result;
    result.duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    Ok(Outcome::Completed { result })
}

fn validate_code_tool_presentation(call: &harness::ToolInvocationRequest) -> Result<(), String> {
    let id = call
        .tool_id
        .as_ref()
        .ok_or_else(|| "code tool call has no admitted tool ID".to_owned())?;
    if let Some(harness::RemoteMcpCallRuntime::Injected {
        remote_tool_name, ..
    }) = &call.remote_mcp
    {
        return (call.tool_name.as_str() == format!("{id}__{remote_tool_name}"))
            .then_some(())
            .ok_or_else(|| "code tool MCP name does not match its admitted binding".to_owned());
    }
    if let Some(builtin) = &call.builtin {
        let resolved = tools::definitions::resolve(id, &builtin.spec, &(&builtin.model).into())
            .map_err(|error| error.to_string())?;
        return resolved
            .into_iter()
            .any(|tool| tool.name == call.tool_name && tool.binding.is_some())
            .then_some(())
            .ok_or_else(|| {
                "code tool name is absent from its pinned callable presentation".to_owned()
            });
    }
    if call.tool_name.as_str() != id.as_str() {
        return Err("code tool function name does not match its admitted binding".to_owned());
    }
    Ok(())
}

/// Wait, heartbeating on every poll, until the session's active environment
/// is reachable, terminally unusable, or the bounded readiness window
/// elapses. Runs as its own activity so tool classes keep their deadlines.
pub(super) async fn await_environment_ready(
    deps: &ToolActivityDeps,
    ctx: &temporalio_sdk::activities::ActivityContext,
    request: AwaitEnvironmentReadyActivityRequest,
) -> Result<AwaitEnvironmentReadyActivityResult, ActivityError> {
    let Some(hosted) = deps.hosted.as_ref() else {
        return Ok(AwaitEnvironmentReadyActivityResult::Failed {
            message: "environment readiness is unavailable on this runtime".to_owned(),
        });
    };
    let deadline = tokio::time::Instant::now() + temporal_workflow::ENVIRONMENT_READY_WAIT;
    Ok(hosted
        .await_environment_ready(&request, deadline, || async {
            ctx.record_heartbeat(())
                .await
                .expect("unit heartbeat serializes")
        })
        .await)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use harness::{
        BlobRef, CoreAgentIoError, CoreAgentTools, PromiseControlArgumentCall,
        RemoteMcpApprovalPolicy, RemoteMcpCallRuntime, RemoteMcpCallTarget, RunId, SessionId,
        ToolBatchId, ToolCallId, ToolExecutionClass, ToolExecutionSpec, ToolId,
        ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationRequest, ToolName,
        TurnId, storage::InMemoryBlobStore,
    };
    use llm_runtime::secrets::{AbsentSecretResolver, SecretResolver};

    use super::*;
    use crate::{
        gateway::service::mcp_discovery::ConfiguratorTrustedHeaderPolicy,
        worker::mcp::{McpPrivateNetworkPolicy, RuntimeMcpExecutor, RuntimeMcpInventoryResolver},
    };

    #[tokio::test(flavor = "current_thread")]
    async fn preparation_reads_only_cas_and_returns_bounded_validated_ids() {
        let blobs = InMemoryBlobStore::new();
        let valid_ref = blobs
            .put_bytes(br#"{"promises":["promise_1","promise_1","promise_2"]}"#.to_vec())
            .await
            .expect("valid args");
        let invalid_ref = blobs
            .put_bytes(br#"{"promises":[]}"#.to_vec())
            .await
            .expect("invalid args");
        let facts = prepare_promise_controls(
            &blobs,
            PromiseControlArgumentRequest {
                version: PromiseControlArgumentRequest::VERSION,
                calls: vec![
                    PromiseControlArgumentCall {
                        call_id: ToolCallId::new("cancel"),
                        kind: PromiseControlKind::Cancel,
                        arguments_ref: valid_ref,
                    },
                    PromiseControlArgumentCall {
                        call_id: ToolCallId::new("detach-invalid"),
                        kind: PromiseControlKind::Detach,
                        arguments_ref: invalid_ref,
                    },
                    PromiseControlArgumentCall {
                        call_id: ToolCallId::new("missing"),
                        kind: PromiseControlKind::Cancel,
                        arguments_ref: BlobRef::from_bytes(b"missing"),
                    },
                ],
            },
        )
        .await
        .expect("prepare facts");

        assert_eq!(
            facts.calls,
            vec![
                PromiseControlArgumentCallFacts::Parsed {
                    call_id: ToolCallId::new("cancel"),
                    promise_ids: vec![
                        harness::PromiseId::new("promise_1"),
                        harness::PromiseId::new("promise_2")
                    ],
                },
                PromiseControlArgumentCallFacts::Invalid {
                    call_id: ToolCallId::new("detach-invalid"),
                },
                PromiseControlArgumentCallFacts::Invalid {
                    call_id: ToolCallId::new("missing"),
                },
            ]
        );
    }

    #[derive(Clone, Copy)]
    enum RuntimeScript {
        Complete,
        Fail,
    }

    /// Tool runtime double: records every batch it receives and completes or
    /// fails it by script. It never understands runtime MCP calls, exactly like
    /// the hosted runtime.
    struct RecordingTools {
        script: RuntimeScript,
        received: Mutex<Vec<ToolInvocationBatchRequest>>,
    }

    #[async_trait]
    impl CoreAgentTools for RecordingTools {
        async fn invoke_batch(
            &self,
            request: ToolInvocationBatchRequest,
        ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
            self.received
                .lock()
                .expect("recording lock")
                .push(request.clone());
            match self.script {
                RuntimeScript::Fail => Err(CoreAgentIoError::Failed {
                    message: "scripted runtime failure".to_owned(),
                }),
                RuntimeScript::Complete => {
                    Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                        run_id: request.run_id,
                        turn_id: request.turn_id,
                        batch_id: request.batch_id,
                        results: request.calls.iter().map(succeeded).collect(),
                    }))
                }
            }
        }
    }

    fn succeeded(call: &ToolInvocationRequest) -> ToolInvocationResult {
        let content_ref = BlobRef::from_bytes(call.call_id.as_str().as_bytes());
        ToolInvocationResult {
            attachments: Vec::new(),
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(content_ref.clone()),
            model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
                &call.call_id,
                ToolCallStatus::Succeeded,
                content_ref,
            )],
            error_ref: None,
            effects: Vec::new(),
        }
    }

    fn runtime_call(call_id: &str, tool_name: &str) -> ToolInvocationRequest {
        ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new(call_id),
            tool_id: Some(ToolId::new(tool_name)),
            tool_name: ToolName::new(tool_name),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        }
    }

    async fn mcp_call(
        blobs: &dyn BlobStore,
        call_id: &str,
        approval_decision: Option<bool>,
    ) -> ToolInvocationRequest {
        let arguments_ref = blobs
            .put_bytes(b"{}".to_vec())
            .await
            .expect("runtime MCP arguments");
        ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new(call_id),
            tool_id: Some(ToolId::new("mcp_echo")),
            tool_name: ToolName::new("mcp_echo__hello"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: Some(RemoteMcpCallRuntime::Injected {
                target: RemoteMcpCallTarget {
                    server_id: "echo".to_owned(),
                    record_revision: 1,
                    server_label: "echo".to_owned(),
                    allowed_tools: None,
                    approval: RemoteMcpApprovalPolicy::Always,
                },
                remote_tool_name: "hello".to_owned(),
                approval_decision,
            }),
        }
    }

    fn batch(calls: Vec<ToolInvocationRequest>) -> ToolInvokeBatchActivityRequest {
        ToolInvokeBatchActivityRequest {
            request: ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: SessionId::new("session-mixed"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                workspace_attachments: Vec::new(),
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                code_mode_policy: None,
                promise_id_base: 7,
                calls,
            },
        }
    }

    /// A runtime MCP executor over an empty registry: approval gating and rejection
    /// happen before any registry or network access, and an approved call
    /// fails at record validation instead of reaching the wire.
    fn runtime_mcp_executor() -> Arc<RuntimeMcpExecutor> {
        let secrets: Arc<dyn SecretResolver> = Arc::new(AbsentSecretResolver);
        let private_networks =
            McpPrivateNetworkPolicy::parse(None).expect("private network policy");
        let trusted_header = ConfiguratorTrustedHeaderPolicy;
        let universe_id = uuid::Uuid::from_u128(7);
        let servers: Arc<dyn mcp::McpRegistryStore> =
            Arc::new(mcp::InMemoryMcpRegistryStore::new());
        let inventory = Arc::new(RuntimeMcpInventoryResolver::new(
            servers.clone(),
            secrets.clone(),
            private_networks.clone(),
            trusted_header.clone(),
            universe_id,
        ));
        Arc::new(RuntimeMcpExecutor::new(
            servers,
            secrets,
            inventory,
            private_networks,
            trusted_header,
            universe_id,
        ))
    }

    fn deps(
        script: RuntimeScript,
        blobs: Arc<dyn BlobStore>,
        runtime_mcp: Option<Arc<RuntimeMcpExecutor>>,
    ) -> (ToolActivityDeps, Arc<RecordingTools>) {
        let tools = Arc::new(RecordingTools {
            script,
            received: Mutex::new(Vec::new()),
        });
        (
            ToolActivityDeps {
                tools: tools.clone(),
                blobs,
                blob_graph: None,
                hosted: None,
                runtime_mcp,
            },
            tools,
        )
    }

    fn received_call_ids(tools: &RecordingTools) -> Vec<Vec<String>> {
        tools
            .received
            .lock()
            .expect("recording lock")
            .iter()
            .map(|request| {
                request
                    .calls
                    .iter()
                    .map(|call| call.call_id.as_str().to_owned())
                    .collect()
            })
            .collect()
    }

    fn call_ids(results: &[ToolInvocationResult]) -> Vec<&str> {
        results
            .iter()
            .map(|result| result.call_id.as_str())
            .collect()
    }

    async fn error_text(blobs: &dyn BlobStore, result: &ToolInvocationResult) -> String {
        blobs
            .read_text(
                result
                    .error_ref
                    .as_ref()
                    .expect("failed result carries an error ref"),
            )
            .await
            .expect("error text")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_tool_calls_reject_approval_before_mcp_execution_even_with_an_old_decision() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(
            RuntimeScript::Complete,
            blobs.clone(),
            Some(runtime_mcp_executor()),
        );
        for decision in [None, Some(true), Some(false)] {
            let call = mcp_call(blobs.as_ref(), "code-tool:mcp", decision).await;
            let request = batch(vec![call])
                .request
                .call_request(
                    0,
                    ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, false),
                )
                .expect("single call");
            let outcome = invoke_code_tool(
                &deps,
                temporal_workflow::CodeToolInvokeActivityRequest { request },
            )
            .await
            .expect("code tool activity");
            let temporal_workflow::CodeToolInvokeActivityResult::Completed { result } = outcome
            else {
                panic!("approval must produce a terminal code tool failure");
            };
            assert_eq!(result.status, ToolCallStatus::Failed);
            assert!(result.effects.is_empty());
            assert!(
                error_text(blobs.as_ref(), &result)
                    .await
                    .contains("approval-required")
            );
        }
        assert!(received_call_ids(&tools).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_tool_ordinary_call_preserves_pinned_runtime_facts() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(RuntimeScript::Complete, blobs, None);
        let mut call = runtime_call("code-tool:read", "Read");
        call.tool_id = Some(ToolId::new("env.read_file"));
        call.builtin = Some(harness::BuiltinToolCallRuntime {
            spec: harness::BuiltinToolSpec::default(),
            model: harness::ModelSelection {
                api_kind: harness::ProviderApiKind::AnthropicMessages,
                provider_id: "anthropic".to_owned(),
                model: "claude-pinned".to_owned(),
            },
        });
        let request = batch(vec![call.clone()])
            .request
            .call_request(0, ToolExecutionSpec::default())
            .expect("single call");
        let outcome = invoke_code_tool(
            &deps,
            temporal_workflow::CodeToolInvokeActivityRequest { request },
        )
        .await
        .expect("code tool activity");
        let temporal_workflow::CodeToolInvokeActivityResult::Completed { result } = outcome else {
            panic!("ordinary call completes");
        };
        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let received = tools.received.lock().expect("recording lock");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].calls, vec![call]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_tool_scope_uses_exact_model_names_and_omits_provider_hosted_tools() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _) = deps(RuntimeScript::Complete, blobs, None);
        let request = temporal_workflow::CodeToolPrepareScopeActivityRequest {
            tools: ["env.read_file", "web.search", "mcp.find_tools", "mcp.call"]
                .into_iter()
                .map(|id| {
                    tools::definitions::register(
                        id,
                        Default::default(),
                        harness::ToolParallelism::ParallelSafe,
                        Default::default(),
                    )
                })
                .collect(),
            model: harness::ModelSelection {
                api_kind: harness::ProviderApiKind::AnthropicMessages,
                provider_id: "anthropic".to_owned(),
                model: "claude".to_owned(),
            },
        };
        let prepared = prepare_code_tool_scope(&deps, request.clone())
            .await
            .expect("prepare");
        let replayed = prepare_code_tool_scope(&deps, request)
            .await
            .expect("repeat preparation");
        assert_eq!(prepared, replayed);
        let names: std::collections::BTreeSet<_> = prepared
            .bindings
            .values()
            .map(|binding| binding.tool_name.as_str())
            .collect();
        assert_eq!(
            names,
            std::collections::BTreeSet::from(["Read", "mcp_find_tools", "mcp_call"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_tool_call_rejects_legacy_alias_before_the_backend() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(RuntimeScript::Complete, blobs.clone(), None);
        let mut call = runtime_call("code-tool:wrong-name", "read_file");
        call.tool_id = Some(ToolId::new("env.read_file"));
        call.builtin = Some(harness::BuiltinToolCallRuntime {
            spec: Default::default(),
            model: harness::ModelSelection {
                api_kind: harness::ProviderApiKind::AnthropicMessages,
                provider_id: "anthropic".to_owned(),
                model: "claude".to_owned(),
            },
        });
        let request = batch(vec![call])
            .request
            .call_request(0, Default::default())
            .unwrap();
        let outcome = invoke_code_tool(
            &deps,
            temporal_workflow::CodeToolInvokeActivityRequest { request },
        )
        .await
        .expect("code tool activity");
        let temporal_workflow::CodeToolInvokeActivityResult::Completed { result } = outcome else {
            panic!("invalid name must fail");
        };
        assert_eq!(result.status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result)
                .await
                .contains("pinned callable presentation")
        );
        assert!(received_call_ids(&tools).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn batches_reach_the_runtime_unchanged() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(RuntimeScript::Complete, blobs, Some(runtime_mcp_executor()));
        let request = batch(vec![
            runtime_call("call_a", "tool_a"),
            runtime_call("call_b", "tool_b"),
        ]);

        let outcome = invoke_batch(&deps, request.clone()).await.expect("batch");

        let received = tools.received.lock().expect("recording lock");
        assert_eq!(received.as_slice(), std::slice::from_ref(&request.request));
        let ToolBatchOutcome::Completed { result } = outcome else {
            panic!("expected a completed batch, got {outcome:?}");
        };
        assert_eq!(call_ids(&result.results), vec!["call_a", "call_b"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_runtime_failure_becomes_a_failed_result_for_each_call() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(RuntimeScript::Fail, blobs.clone(), None);
        let request = batch(vec![
            runtime_call("call_a", "tool_a"),
            runtime_call("call_b", "tool_b"),
        ]);

        let outcome = invoke_batch(&deps, request).await.expect("batch");

        let ToolBatchOutcome::Completed { result } = outcome else {
            panic!("expected a completed batch, got {outcome:?}");
        };
        assert_eq!(call_ids(&result.results), vec!["call_a", "call_b"]);
        for failed in &result.results {
            assert_eq!(failed.status, ToolCallStatus::Failed);
            assert!(
                error_text(blobs.as_ref(), failed)
                    .await
                    .contains("scripted runtime failure")
            );
        }
    }

    async fn invoke_runtime_call(
        deps: &ToolActivityDeps,
        blobs: &dyn BlobStore,
        approval_decision: Option<bool>,
    ) -> ToolInvokeCallActivityResult {
        let call = mcp_call(blobs, "call_m", approval_decision).await;
        let request = batch(vec![call])
            .request
            .call_request(
                0,
                ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, false),
            )
            .expect("single call");
        invoke_call(
            deps,
            crate::worker::ToolInvokeCallActivityRequest { request },
        )
        .await
        .expect("call activity")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_undecided_gated_call_reports_its_subject_before_any_mcp_io() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(
            RuntimeScript::Complete,
            blobs.clone(),
            Some(runtime_mcp_executor()),
        );

        let outcome = invoke_runtime_call(&deps, blobs.as_ref(), None).await;

        let ToolInvokeCallActivityResult::NeedsApproval { subject } = outcome else {
            panic!("expected an approval outcome, got {outcome:?}");
        };
        let harness::ApprovalSubject::McpToolCall {
            server_id,
            tool_name,
            ..
        } = subject;
        assert_eq!(server_id, "echo");
        assert_eq!(tool_name, "hello");
        assert!(received_call_ids(&tools).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_decided_gated_call_completes_without_reaching_the_runtime() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(
            RuntimeScript::Complete,
            blobs.clone(),
            Some(runtime_mcp_executor()),
        );

        let rejected = invoke_runtime_call(&deps, blobs.as_ref(), Some(false)).await;
        let ToolInvokeCallActivityResult::Completed { result } = rejected else {
            panic!("expected a completed call, got {rejected:?}");
        };
        assert_eq!(result.status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result)
                .await
                .contains("rejected")
        );

        // The approved call reached the executor and failed there (no such
        // server record); it never fell through to the inline runtime.
        let approved = invoke_runtime_call(&deps, blobs.as_ref(), Some(true)).await;
        let ToolInvokeCallActivityResult::Completed { result } = approved else {
            panic!("expected a completed call, got {approved:?}");
        };
        assert_eq!(result.status, ToolCallStatus::Failed);
        assert!(result.duration_ms.is_some());
        let approved_error = error_text(blobs.as_ref(), &result).await;
        assert!(!approved_error.contains("unknown tool"), "{approved_error}");
        assert!(received_call_ids(&tools).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_large_runtime_result_is_cut_for_the_model_and_kept_whole_by_reference() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(RuntimeScript::Complete, blobs.clone(), None);
        let call = mcp_call(blobs.as_ref(), "call_m", None).await;
        let request = batch(vec![call])
            .request
            .call_request(
                0,
                ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, false),
            )
            .expect("single call");
        let budget = tools::limits::ToolLimits::default().max_model_visible_output_bytes as usize;
        let text = "x".repeat(3 * 512 * 1024);
        // The announcement of the image falls past the cut; the entry stays.
        let visible = format!("{text}\n[image 1]");
        let image = harness::BlobRef::from_bytes(b"png");

        for is_error in [false, true] {
            let result = store_runtime_mcp_result(
                &deps,
                &request,
                serde_json::json!({"content": [{"type": "text", "text": text}]}),
                visible.clone(),
                is_error,
                vec![crate::worker::mcp::RuntimeMcpAsset {
                    bytes: b"png".to_vec(),
                    blob_ref: image.clone(),
                    media_type: Some("image/png".to_owned()),
                    kind: "image".to_owned(),
                    uri: None,
                }],
                vec![crate::worker::mcp::RuntimeMcpMedia {
                    asset_index: 0,
                    kind: harness::media::MediaKind::Image,
                    media_type: "image/png".to_owned(),
                    name: None,
                }],
            )
            .await
            .expect("stored result");

            assert!(result.truncated);
            assert_eq!(result.output_bytes, Some(visible.len() as u64));
            assert_eq!(result.model_visible_context_entries.len(), 2);
            let shown = blobs
                .read_text(&result.model_visible_context_entries[0].content.content_ref)
                .await
                .expect("visible text");
            assert!(shown.len() <= budget + "\n[truncated]".len());
            assert!(shown.ends_with("\n[truncated]"));
            assert_eq!(
                result.error_ref.as_ref(),
                is_error.then_some(&result.model_visible_context_entries[0].content.content_ref)
            );
            let kept: serde_json::Value = serde_json::from_slice(
                &blobs
                    .read_bytes(result.output_ref.as_ref().expect("full output"))
                    .await
                    .expect("full output blob"),
            )
            .expect("output JSON");
            assert_eq!(kept["content"][0]["text"], text.as_str());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_small_runtime_result_passes_through_uncut() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(RuntimeScript::Complete, blobs.clone(), None);
        let call = mcp_call(blobs.as_ref(), "call_m", None).await;
        let request = batch(vec![call])
            .request
            .call_request(
                0,
                ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, false),
            )
            .expect("single call");

        let result = store_runtime_mcp_result(
            &deps,
            &request,
            serde_json::json!({"content": []}),
            "short".to_owned(),
            false,
            Vec::new(),
            Vec::new(),
        )
        .await
        .expect("stored result");

        assert!(!result.truncated);
        assert_eq!(result.output_bytes, Some(5));
        assert_eq!(
            blobs
                .read_text(&result.model_visible_context_entries[0].content.content_ref)
                .await
                .expect("visible text"),
            "short"
        );
    }

    /// An argument error quotes the tool's input schema, which a third-party server
    /// can make up to 512 KiB, so a failure is cut like a result.
    #[tokio::test(flavor = "current_thread")]
    async fn a_large_failure_message_is_cut_for_the_model() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let call = mcp_call(blobs.as_ref(), "call_m", None).await;
        let request = batch(vec![call])
            .request
            .call_request(
                0,
                ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, false),
            )
            .expect("single call");
        let budget = tools::limits::ToolLimits::default().max_model_visible_output_bytes as usize;
        let message = "é".repeat(256 * 1024);

        let result = crate::worker::activities::common::failed_tool_call_result(
            blobs.as_ref(),
            &request,
            &message,
        )
        .await
        .expect("failed result");

        assert_eq!(result.status, ToolCallStatus::Failed);
        assert!(result.truncated);
        assert_eq!(result.output_bytes, Some(message.len() as u64));
        let shown = blobs
            .read_text(&result.model_visible_context_entries[0].content.content_ref)
            .await
            .expect("visible text");
        assert!(shown.len() < budget + 256, "{}", shown.len());
        assert!(shown.contains("\n[truncated]\nrun_id="));

        let short = crate::worker::activities::common::failed_tool_call_result(
            blobs.as_ref(),
            &request,
            "short",
        )
        .await
        .expect("failed result");
        assert!(!short.truncated);
        assert_eq!(short.output_bytes, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_runtime_call_fails_as_a_tool_failure_when_the_worker_has_no_runtime_mcp_executor() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(RuntimeScript::Complete, blobs.clone(), None);

        let outcome = invoke_runtime_call(&deps, blobs.as_ref(), None).await;

        let ToolInvokeCallActivityResult::Completed { result } = outcome else {
            panic!("expected a completed call, got {outcome:?}");
        };
        assert_eq!(result.status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result)
                .await
                .contains("runtime MCP execution is unavailable on this worker")
        );
    }
}
