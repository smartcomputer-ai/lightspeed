use std::collections::BTreeMap;

use engine::{
    NativeMcpApprovalRequest, PromiseControlArgumentCallFacts, PromiseControlArgumentFacts,
    PromiseControlArgumentRequest, PromiseControlKind, ToolBatchOutcome, ToolCallStatus,
    ToolExecutionClass, ToolExecutionSpec, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationResult, storage::BlobStore,
};
use futures::{StreamExt, stream};
use temporal_workflow::MAX_CONCURRENT_TOOL_CALLS_PER_BATCH;
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

/// Execute an admitted tool batch as one unit.
///
/// Native MCP calls never reach the tool runtime behind `deps.tools`: this
/// wrapper owns their dispatch on both the per-call and the batch-unit path.
/// A batch that mixes workflow tools or `await` with injected MCP calls
/// therefore still executes as one logical batch in one activity: the
/// runtime receives the batch minus its MCP calls, the MCP calls run here
/// concurrently under the remote operation deadline, and the results merge
/// back in batch order. Approval gating follows the per-call contract: every
/// ungated call completes, the gated calls stay pending, and the outcome
/// carries the full set of approval subjects so the run parks exactly once.
pub(super) async fn invoke_batch(
    deps: &ToolActivityDeps,
    request: ToolInvokeBatchActivityRequest,
) -> Result<ToolBatchOutcome, ActivityError> {
    let request = request.request;
    let native_indices = request
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.remote_mcp.is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if native_indices.is_empty() {
        return invoke_runtime_batch(deps, request).await;
    }

    let runtime_request = ToolInvocationBatchRequest {
        calls: request
            .calls
            .iter()
            .filter(|call| call.remote_mcp.is_none())
            .cloned()
            .collect(),
        ..request.clone()
    };
    let runtime = async {
        if runtime_request.calls.is_empty() {
            return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results: Vec::new(),
            }));
        }
        invoke_runtime_batch(deps, runtime_request).await
    };
    let native = stream::iter(native_indices)
        .map(|index| {
            let request = &request;
            async move {
                (
                    index,
                    invoke_native_mcp_batch_call(deps, request, index).await,
                )
            }
        })
        .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS_PER_BATCH)
        .collect::<Vec<_>>();
    let (runtime_outcome, native_outcomes) = futures::future::join(runtime, native).await;
    let runtime_outcome = runtime_outcome?;

    let mut approvals = Vec::new();
    let mut native_results = BTreeMap::new();
    let mut native_outcomes = native_outcomes;
    native_outcomes.sort_by_key(|(index, _)| *index);
    for (index, outcome) in native_outcomes {
        match outcome? {
            NativeMcpCallOutcome::Completed(result) => {
                native_results.insert(index, result);
            }
            NativeMcpCallOutcome::NeedsApproval { subject } => {
                approvals.push(NativeMcpApprovalRequest {
                    call_id: request.calls[index].call_id.clone(),
                    subject,
                });
            }
        }
    }
    let merge = |runtime_results: Vec<ToolInvocationResult>| {
        merge_in_batch_order(&request, runtime_results, native_results)
    };

    if !approvals.is_empty() {
        let runtime_results = match runtime_outcome {
            ToolBatchOutcome::Completed { result } => result.results,
            // An `await` never defers while a sibling waits on a decision:
            // the deferral is dropped, the await stays pending, and it runs
            // again with the gated calls once the run unparks.
            ToolBatchOutcome::Deferred {
                completed_results, ..
            } => completed_results,
            ToolBatchOutcome::AwaitingApproval { .. } => {
                return Err(runtime_approval_invariant_error());
            }
        };
        return Ok(ToolBatchOutcome::AwaitingApproval {
            batch_id: request.batch_id,
            completed_results: merge(runtime_results),
            approvals,
        });
    }
    Ok(match runtime_outcome {
        ToolBatchOutcome::Completed { result } => ToolBatchOutcome::Completed {
            result: ToolInvocationBatchResult {
                results: merge(result.results),
                ..result
            },
        },
        ToolBatchOutcome::Deferred {
            batch_id,
            call_id,
            completed_results,
            spec,
        } => ToolBatchOutcome::Deferred {
            batch_id,
            call_id,
            completed_results: merge(completed_results),
            spec,
        },
        ToolBatchOutcome::AwaitingApproval { .. } => {
            return Err(runtime_approval_invariant_error());
        }
    })
}

fn runtime_approval_invariant_error() -> ActivityError {
    activity_error(anyhow::anyhow!(
        "tool runtime reported an approval outcome for a batch without native MCP calls"
    ))
}

/// Hand a batch without native MCP calls to the tool runtime; a runtime
/// failure becomes an ordinary failed result for every call it covered.
async fn invoke_runtime_batch(
    deps: &ToolActivityDeps,
    request: ToolInvocationBatchRequest,
) -> Result<ToolBatchOutcome, ActivityError> {
    match deps.tools.invoke_batch(request.clone()).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => failed_tool_batch_result(deps.blobs.as_ref(), &request, error.to_string())
            .await
            .map(ToolBatchOutcome::completed)
            .map_err(activity_error),
    }
}

/// Order merged results by their call's position in the original batch.
/// Results the runtime returned for calls this batch never scheduled keep
/// their relative order at the end; the engine rejects them on resume.
fn merge_in_batch_order(
    request: &ToolInvocationBatchRequest,
    runtime_results: Vec<ToolInvocationResult>,
    native_results: BTreeMap<usize, ToolInvocationResult>,
) -> Vec<ToolInvocationResult> {
    let position = |call_id: &engine::ToolCallId| {
        request
            .calls
            .iter()
            .position(|call| call.call_id == *call_id)
            .unwrap_or(usize::MAX)
    };
    let mut results = native_results.into_iter().collect::<Vec<_>>();
    results.extend(
        runtime_results
            .into_iter()
            .map(|result| (position(&result.call_id), result)),
    );
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// One native MCP call of a batch-unit dispatch, executed exactly as the
/// per-call activity would execute it.
async fn invoke_native_mcp_batch_call(
    deps: &ToolActivityDeps,
    request: &ToolInvocationBatchRequest,
    index: usize,
) -> Result<NativeMcpCallOutcome, ActivityError> {
    let call_request = request
        .call_request(
            index,
            ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, false),
        )
        .expect("native MCP indices come from this batch request");
    invoke_native_mcp_call(deps, &call_request).await
}

/// Outcome of one native MCP call executed by this activity wrapper.
enum NativeMcpCallOutcome {
    Completed(ToolInvocationResult),
    /// The call must receive a single-use run-owned decision before the
    /// worker performs any MCP wire I/O.
    NeedsApproval {
        subject: engine::ApprovalSubject,
    },
}

/// Execute one native MCP call under the remote operation deadline. Shared by
/// per-call and batch-unit execution so both paths apply the same target
/// validation, approval gate, asset storage, result projection, deadline, and
/// failure conversion. Tool-level failures and an elapsed deadline become an
/// ordinary terminal failed result; only blob-store failures fail the
/// activity.
async fn invoke_native_mcp_call(
    deps: &ToolActivityDeps,
    request: &engine::ToolInvocationCallRequest,
) -> Result<NativeMcpCallOutcome, ActivityError> {
    let deadline =
        temporal_workflow::tool_call_operation_timeout(ToolExecutionClass::RemoteInteractive);
    let started = std::time::Instant::now();
    let stamp = |mut result: ToolInvocationResult| {
        result.duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        result
    };
    let failed = |message: String| async move {
        super::common::failed_tool_call_result(deps.blobs.as_ref(), request, message)
            .await
            .map(|result| NativeMcpCallOutcome::Completed(stamp(result)))
            .map_err(activity_error)
    };
    match tokio::time::timeout(deadline, execute_native_mcp_call(deps, request)).await {
        Ok(Ok(NativeMcpCallOutcome::Completed(result))) => {
            Ok(NativeMcpCallOutcome::Completed(stamp(result)))
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

async fn execute_native_mcp_call(
    deps: &ToolActivityDeps,
    request: &engine::ToolInvocationCallRequest,
) -> Result<NativeMcpCallOutcome, engine::CoreAgentIoError> {
    let native = deps
        .native_mcp
        .as_ref()
        .ok_or_else(|| engine::CoreAgentIoError::Failed {
            message: "native MCP execution is unavailable on this worker".to_owned(),
        })?;
    let argument_bytes = deps
        .blobs
        .read_bytes(&request.call.arguments_ref)
        .await
        .map_err(|error| engine::CoreAgentIoError::Failed {
            message: format!("read native MCP arguments: {error}"),
        })?;
    let arguments = serde_json::from_slice(&argument_bytes).map_err(|error| {
        engine::CoreAgentIoError::Failed {
            message: format!("native MCP arguments are invalid JSON: {error}"),
        }
    })?;
    match native.execute(request, arguments).await {
        Ok(crate::worker::mcp::NativeMcpExecutionOutcome::NeedsApproval { subject }) => {
            Ok(NativeMcpCallOutcome::NeedsApproval { subject })
        }
        Ok(crate::worker::mcp::NativeMcpExecutionOutcome::Completed {
            mut output,
            visible,
            is_error,
            assets,
        }) => {
            let mut asset_refs = Vec::with_capacity(assets.len());
            for asset in assets {
                let blob_ref = deps.blobs.put_bytes(asset.bytes).await.map_err(|error| {
                    engine::CoreAgentIoError::Failed {
                        message: format!(
                            "store native MCP {} asset ({}): {error}",
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
                    engine::CoreAgentIoError::Failed {
                        message: format!("encode native MCP result: {error}"),
                    }
                })?)
                .await
                .map_err(|error| engine::CoreAgentIoError::Failed {
                    message: format!("store native MCP result: {error}"),
                })?;
            // The output embeds the asset refs it was just given; the edges
            // keep the assets alive as long as the output.
            engine::storage::record_contains_edges(
                deps.blob_graph.as_deref(),
                &output_ref,
                asset_refs.iter().cloned(),
            )
            .await
            .map_err(|error| engine::CoreAgentIoError::Failed {
                message: format!("record native MCP asset edges: {error}"),
            })?;
            let visible = visible.into_bytes();
            let output_bytes = visible.len() as u64;
            let visible_ref = deps.blobs.put_bytes(visible).await.map_err(|error| {
                engine::CoreAgentIoError::Failed {
                    message: format!("store native MCP visible result: {error}"),
                }
            })?;
            let status = if is_error {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Succeeded
            };
            Ok(NativeMcpCallOutcome::Completed(ToolInvocationResult {
                duration_ms: None,
                output_bytes: Some(output_bytes),
                truncated: false,
                call_id: request.call.call_id.clone(),
                status,
                output_ref: Some(output_ref),
                model_visible_context_entries: vec![
                    ToolInvocationResult::tool_result_context_entry(
                        &request.call.call_id,
                        status,
                        visible_ref.clone(),
                    ),
                ],
                error_ref: is_error.then_some(visible_ref),
                effects: Vec::new(),
            }))
        }
        Err(message) => Err(engine::CoreAgentIoError::Failed { message }),
    }
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
        return Ok(match invoke_native_mcp_call(deps, &request).await? {
            NativeMcpCallOutcome::Completed(result) => {
                ToolInvokeCallActivityResult::Completed { result }
            }
            NativeMcpCallOutcome::NeedsApproval { subject } => {
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
        .await_environment_ready(&request, deadline, || ctx.record_heartbeat(Vec::new()))
        .await)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use engine::{
        AwaitMode, AwaitSpec, BlobRef, CoreAgentIoError, CoreAgentTools,
        PromiseControlArgumentCall, RemoteMcpApprovalPolicy, RemoteMcpCallRuntime,
        RemoteMcpCallTarget, RunId, SessionId, ToolBatchId, ToolCallId, ToolInvocationRequest,
        ToolName, TurnId, storage::InMemoryBlobStore,
    };
    use llm_runtime::secrets::{AbsentSecretResolver, SecretResolver};

    use super::*;
    use crate::{
        gateway::service::mcp_discovery::ConfiguratorTrustedHeaderPolicy,
        worker::mcp::{McpPrivateNetworkPolicy, NativeMcpInventoryResolver, NativeMcpRuntime},
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
                        engine::PromiseId::new("promise_1"),
                        engine::PromiseId::new("promise_2")
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
        /// The runtime defers on the batch's last call, as the hosted runtime
        /// does for an `await` sibling.
        DeferOnLastCall,
        Fail,
    }

    /// Tool runtime double: records every batch it receives and completes,
    /// defers, or fails it by script. It never understands native MCP calls,
    /// exactly like the hosted runtime.
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
                RuntimeScript::DeferOnLastCall => {
                    let (deferred, completed) = request
                        .calls
                        .split_last()
                        .expect("deferring batch has calls");
                    Ok(ToolBatchOutcome::Deferred {
                        batch_id: request.batch_id,
                        call_id: deferred.call_id.clone(),
                        completed_results: completed.iter().map(succeeded).collect(),
                        spec: AwaitSpec {
                            promise_ids: Vec::new(),
                            mode: AwaitMode::All,
                            deadline_at_ms: None,
                        },
                    })
                }
            }
        }
    }

    fn succeeded(call: &ToolInvocationRequest) -> ToolInvocationResult {
        let content_ref = BlobRef::from_bytes(call.call_id.as_str().as_bytes());
        ToolInvocationResult {
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
            tool_id: Some(ToolName::new(tool_name)),
            tool_name: ToolName::new(tool_name),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        }
    }

    async fn native_call(
        blobs: &dyn BlobStore,
        call_id: &str,
        approval_decision: Option<bool>,
    ) -> ToolInvocationRequest {
        let arguments_ref = blobs
            .put_bytes(b"{}".to_vec())
            .await
            .expect("native MCP arguments");
        ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new(call_id),
            tool_id: Some(ToolName::new("mcp_echo")),
            tool_name: ToolName::new("mcp_echo__hello"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: Some(RemoteMcpCallRuntime::Injected {
                target: RemoteMcpCallTarget {
                    server_id: "echo".to_owned(),
                    record_revision: 1,
                    server_label: "echo".to_owned(),
                    server_url: "https://echo.example.com/mcp".to_owned(),
                    allowed_tools: None,
                    approval: RemoteMcpApprovalPolicy::Always,
                    auth_ref: None,
                    auth_required: false,
                    allow_private_network: false,
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
                workspace_links: Vec::new(),
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                promise_id_base: 7,
                calls,
            },
        }
    }

    /// A native runtime over an empty registry: approval gating and rejection
    /// happen before any registry or network access, and an approved call
    /// fails at record validation instead of reaching the wire.
    fn native_runtime() -> Arc<NativeMcpRuntime> {
        let secrets: Arc<dyn SecretResolver> = Arc::new(AbsentSecretResolver);
        let private_networks =
            McpPrivateNetworkPolicy::parse(None).expect("private network policy");
        let trusted_header = ConfiguratorTrustedHeaderPolicy::default();
        let universe_id = uuid::Uuid::from_u128(7);
        let inventory = Arc::new(NativeMcpInventoryResolver::new(
            secrets.clone(),
            private_networks.clone(),
            trusted_header.clone(),
            universe_id,
        ));
        Arc::new(NativeMcpRuntime::new(
            Arc::new(mcp::InMemoryMcpRegistryStore::new()),
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
        native_mcp: Option<Arc<NativeMcpRuntime>>,
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
                native_mcp,
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
    async fn batches_without_native_calls_reach_the_runtime_unchanged() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(RuntimeScript::Complete, blobs, Some(native_runtime()));
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
    async fn mixed_batches_run_ungated_calls_and_park_once_for_every_gated_call() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(
            RuntimeScript::Complete,
            blobs.clone(),
            Some(native_runtime()),
        );
        let request = batch(vec![
            runtime_call("call_a", "work_report"),
            native_call(blobs.as_ref(), "call_m1", None).await,
            native_call(blobs.as_ref(), "call_m2", None).await,
        ]);

        let outcome = invoke_batch(&deps, request.clone()).await.expect("batch");

        // The runtime saw one batch: the same dispatch minus its MCP calls.
        let received = tools.received.lock().expect("recording lock");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].batch_id, request.request.batch_id);
        assert_eq!(received[0].promise_id_base, request.request.promise_id_base);
        assert_eq!(received[0].calls, vec![request.request.calls[0].clone()]);
        drop(received);
        let ToolBatchOutcome::AwaitingApproval {
            batch_id,
            completed_results,
            approvals,
        } = outcome
        else {
            panic!("expected an approval-gated batch, got {outcome:?}");
        };
        assert_eq!(batch_id, request.request.batch_id);
        assert_eq!(call_ids(&completed_results), vec!["call_a"]);
        assert_eq!(
            approvals
                .iter()
                .map(|approval| approval.call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["call_m1", "call_m2"]
        );
        for approval in &approvals {
            let engine::ApprovalSubject::McpToolCall {
                server_id,
                tool_name,
                ..
            } = &approval.subject;
            assert_eq!(server_id, "echo");
            assert_eq!(tool_name, "hello");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn decided_native_calls_complete_inside_the_batch_in_batch_order() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(
            RuntimeScript::Complete,
            blobs.clone(),
            Some(native_runtime()),
        );
        let request = batch(vec![
            native_call(blobs.as_ref(), "call_m1", Some(false)).await,
            runtime_call("call_a", "work_report"),
            native_call(blobs.as_ref(), "call_m2", Some(true)).await,
        ]);

        let outcome = invoke_batch(&deps, request).await.expect("batch");

        let ToolBatchOutcome::Completed { result } = outcome else {
            panic!("expected a completed batch, got {outcome:?}");
        };
        assert_eq!(
            call_ids(&result.results),
            vec!["call_m1", "call_a", "call_m2"]
        );
        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result.results[0])
                .await
                .contains("rejected")
        );
        assert_eq!(result.results[1].status, ToolCallStatus::Succeeded);
        // The approved call reached the executor and failed there (no such
        // server record); it never fell through to the inline runtime.
        assert_eq!(result.results[2].status, ToolCallStatus::Failed);
        let approved_error = error_text(blobs.as_ref(), &result.results[2]).await;
        assert!(!approved_error.contains("unknown tool"), "{approved_error}");
        assert!(result.results[2].duration_ms.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_stays_pending_while_a_sibling_waits_on_approval() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, tools) = deps(
            RuntimeScript::DeferOnLastCall,
            blobs.clone(),
            Some(native_runtime()),
        );
        let request = batch(vec![
            runtime_call("call_a", "work_report"),
            native_call(blobs.as_ref(), "call_m1", None).await,
            runtime_call("call_await", "concurrency.await"),
        ]);

        let outcome = invoke_batch(&deps, request).await.expect("batch");

        assert_eq!(
            received_call_ids(&tools),
            vec![vec!["call_a".to_owned(), "call_await".to_owned()]]
        );
        let ToolBatchOutcome::AwaitingApproval {
            completed_results,
            approvals,
            ..
        } = outcome
        else {
            panic!("expected an approval-gated batch, got {outcome:?}");
        };
        assert_eq!(call_ids(&completed_results), vec!["call_a"]);
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].call_id.as_str(), "call_m1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_batches_carry_native_results_in_batch_order() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(
            RuntimeScript::DeferOnLastCall,
            blobs.clone(),
            Some(native_runtime()),
        );
        let request = batch(vec![
            native_call(blobs.as_ref(), "call_m1", Some(false)).await,
            runtime_call("call_a", "work_report"),
            runtime_call("call_await", "concurrency.await"),
        ]);

        let outcome = invoke_batch(&deps, request).await.expect("batch");

        let ToolBatchOutcome::Deferred {
            call_id,
            completed_results,
            ..
        } = outcome
        else {
            panic!("expected a deferred batch, got {outcome:?}");
        };
        assert_eq!(call_id.as_str(), "call_await");
        assert_eq!(call_ids(&completed_results), vec!["call_m1", "call_a"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_failures_fail_only_the_runtime_calls() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(RuntimeScript::Fail, blobs.clone(), Some(native_runtime()));
        let request = batch(vec![
            runtime_call("call_a", "work_report"),
            native_call(blobs.as_ref(), "call_m1", Some(false)).await,
        ]);

        let outcome = invoke_batch(&deps, request).await.expect("batch");

        let ToolBatchOutcome::Completed { result } = outcome else {
            panic!("expected a completed batch, got {outcome:?}");
        };
        assert_eq!(call_ids(&result.results), vec!["call_a", "call_m1"]);
        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result.results[0])
                .await
                .contains("scripted runtime failure")
        );
        assert_eq!(result.results[1].status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result.results[1])
                .await
                .contains("rejected")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_calls_fail_as_tool_failures_when_the_worker_has_no_native_runtime() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let (deps, _tools) = deps(RuntimeScript::Complete, blobs.clone(), None);
        let request = batch(vec![
            runtime_call("call_a", "work_report"),
            native_call(blobs.as_ref(), "call_m1", None).await,
        ]);

        let outcome = invoke_batch(&deps, request).await.expect("batch");

        let ToolBatchOutcome::Completed { result } = outcome else {
            panic!("expected a completed batch, got {outcome:?}");
        };
        assert_eq!(call_ids(&result.results), vec!["call_a", "call_m1"]);
        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        assert_eq!(result.results[1].status, ToolCallStatus::Failed);
        assert!(
            error_text(blobs.as_ref(), &result.results[1])
                .await
                .contains("native MCP execution is unavailable on this worker")
        );
    }
}
