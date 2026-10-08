//! A real session worker with a parked outer tool and a separate workflow
//! receiver. Protocol and JavaScript-host tests share this production fixture.
#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::support::live::{live_universe_id, run_with_live_worker_builder, seed_agent_default};
use api::{AgentApiService, InputItem, RunStartParams, RunStartSource};
use async_trait::async_trait;
use harness::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError, CoreAgentLlm,
    CoreAgentState, FunctionToolSpec, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, ObservedToolCall, PromiseResolution, SessionId,
    ToolCallId, ToolKind, ToolName, ToolParallelism, ToolSpec, WorkflowToolCompletion,
    WorkflowToolDeclaration, WorkflowToolDefinition, WorkflowToolInvocation, WorkflowToolTarget,
    storage::{BlobStore, ReadSessionEvents, SessionStore},
};
use serde_json::Value;
use temporal_runtime::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, SessionTools, WorkerActivities},
};
use temporal_workflow::{
    AgentSessionWorkflow, CodeToolCallOutcome, CodeToolClient, CodeToolScopeReport,
    CodeToolScopeReportRequest, InvokeCodeToolRequest, OpenCodeToolScopeRequest, ReducedSession,
    compose_workflow_id, reduce_session_entries_from,
};
use temporalio_client::{
    Client, WorkflowExecuteUpdateOptions, WorkflowSignalOptions, WorkflowStartOptions,
    WorkflowTerminateOptions,
};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{SyncWorkflowContext, WorkerOptions, WorkflowContext, WorkflowResult};

pub(super) const OUTER: &str = "test_code_execute";
pub(super) const JOB: &str = "test_joined_job";
pub(super) const AGENT: &str = "test_joined_agent";
pub(super) const SUBMIT: &str = "test_job_submit";

// The test client supplies completions after observing durable admission. This
// receiver keeps the real cross-worker signal transport active without deciding
// when a code tool job or agent finishes.
#[workflow]
#[derive(Default)]
struct CodeToolReceiverWorkflow;

#[workflow_methods]
impl CodeToolReceiverWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>) -> WorkflowResult<()> {
        ctx.wait_condition(|_| false)
            .await
            .map_err(|_| temporalio_sdk::WorkflowTermination::cancelled())?;
        Ok(())
    }

    #[signal(name = "deliver_emission")]
    fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        _envelope: harness::EmissionEnvelope,
    ) {
    }
}

struct OuterCallLlm {
    blobs: Arc<dyn BlobStore>,
    generations: Arc<AtomicUsize>,
}

#[async_trait]
impl CoreAgentLlm for OuterCallLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.generations.fetch_add(1, Ordering::SeqCst);
        let has_result = request
            .request
            .context
            .entries
            .iter()
            .any(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }));
        let bytes = if has_result {
            b"runner completed".as_slice()
        } else {
            b"{}".as_slice()
        };
        let reference = self
            .blobs
            .put_bytes(bytes.to_vec())
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new("outer-call");
        let (kind, finish, calls) = if has_result {
            (
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                LlmFinish::Stop,
                Vec::new(),
            )
        } else {
            let name = ToolName::new(OUTER);
            (
                ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                },
                LlmFinish::ToolCalls,
                vec![ObservedToolCall {
                    call_id,
                    tool_id: test_support::scripted_tool_id(&request, OUTER),
                    tool_name: name,
                    provider_kind: None,
                    arguments_ref: reference.clone(),
                    native_call_ref: None,
                }],
            )
        };
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind,
                content: harness::ContentRef::text(reference),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: None,
                finish,
                usage: None,
                tool_calls: calls,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

pub(super) struct Fixture {
    pub(super) client: Client,
    pub(super) api: GatewayAgentApi,
    pub(super) store: Arc<store_pg::PgStore>,
    pub(super) session_id: SessionId,
    pub(super) run_id: String,
    pub(super) receiver_id: String,
    pub(super) execution_id: String,
    pub(super) bridge: CodeToolClient,
    pub(super) scope: CodeToolScopeReport,
    pub(super) outer: WorkflowToolInvocation,
    pub(super) generations: Arc<AtomicUsize>,
}

impl Fixture {
    pub(super) async fn state(&self) -> anyhow::Result<CoreAgentState> {
        let mut reduced = ReducedSession::default();
        let mut after = None;
        loop {
            let page = self
                .store
                .read_after(ReadSessionEvents {
                    session_id: self.session_id.clone(),
                    after,
                    limit: 1000,
                })
                .await?;
            reduced = reduce_session_entries_from(reduced, &page.entries)?;
            if page.complete {
                return Ok(reduced.core_state);
            }
            after = page.next_after;
        }
    }

    pub(super) async fn request(
        &self,
        request_id: &str,
        name: &str,
        args: Value,
    ) -> anyhow::Result<InvokeCodeToolRequest> {
        let binding_id = self
            .scope
            .bindings
            .iter()
            .find(|(_, exposed)| exposed.as_str() == name)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing code tool binding {name}: {:?}",
                    self.scope.bindings
                )
            })?;
        Ok(InvokeCodeToolRequest {
            execution_id: self.execution_id.clone(),
            request_id: request_id.to_owned(),
            binding_id,
            arguments_ref: self.store.put_bytes(serde_json::to_vec(&args)?).await?,
        })
    }

    pub(super) async fn invoke(
        &self,
        request_id: &str,
        name: &str,
        args: Value,
    ) -> anyhow::Result<CodeToolCallOutcome> {
        Ok(self
            .bridge
            .invoke(
                self.request(request_id, name, args).await?,
                Default::default(),
            )
            .await??)
    }

    // A different Temporal Update ID exercises session-owned deduplication,
    // independently of Temporal's own cached Update result.
    pub(super) async fn redeliver(
        &self,
        request: InvokeCodeToolRequest,
    ) -> anyhow::Result<CodeToolCallOutcome> {
        Ok(self
            .client
            .get_workflow_handle::<AgentSessionWorkflow>(compose_workflow_id(
                live_universe_id()?,
                &self.session_id,
            ))
            .execute_update(
                AgentSessionWorkflow::invoke_code_tool,
                request,
                WorkflowExecuteUpdateOptions::builder()
                    .update_id(uuid::Uuid::new_v4().to_string())
                    .build(),
            )
            .await??)
    }

    pub(super) async fn value(&self, outcome: &CodeToolCallOutcome) -> anyhow::Result<Value> {
        Ok(serde_json::from_slice(
            &self
                .store
                .read_bytes(outcome.output_ref.as_ref().expect("code tool output"))
                .await?,
        )?)
    }

    pub(super) async fn report(&self) -> anyhow::Result<CodeToolScopeReport> {
        Ok(self
            .bridge
            .report(
                CodeToolScopeReportRequest {
                    execution_id: self.execution_id.clone(),
                },
                Default::default(),
            )
            .await??)
    }

    pub(super) async fn invocation(
        &self,
        tool_name: &str,
    ) -> anyhow::Result<WorkflowToolInvocation> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let state = self.state().await?;
                if let Some(invocation) = state
                    .workflow_tools
                    .emissions
                    .values()
                    .find(|invocation| invocation.tool_id.as_str() == tool_name)
                {
                    return Ok(invocation.clone());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("code tool workflow {tool_name} was never admitted"))?
    }

    pub(super) async fn resolve(
        &self,
        invocation: &WorkflowToolInvocation,
        resolution: PromiseResolution,
    ) -> anyhow::Result<()> {
        let promise = invocation
            .completion_promises
            .as_ref()
            .and_then(|promises| promises.get(harness::REPLY_COMPLETION_KEY))
            .expect("receiver completion promise");
        let workflow_id = compose_workflow_id(live_universe_id()?, &self.session_id);
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(workflow_id.clone())
            .signal(
                AgentSessionWorkflow::deliver_emission,
                harness::EmissionEnvelope::source_resolution(
                    live_universe_id()?,
                    self.receiver_id.clone(),
                    &workflow_id,
                    promise.clone(),
                    resolution,
                ),
                WorkflowSignalOptions::default(),
            )
            .await?;
        Ok(())
    }

    pub(super) async fn resolve_value(&self, name: &str, value: Value) -> anyhow::Result<()> {
        let invocation = self.invocation(name).await?;
        let payload_ref = self.store.put_bytes(serde_json::to_vec(&value)?).await?;
        self.resolve(
            &invocation,
            PromiseResolution::Resolved {
                payload_ref: Some(payload_ref),
            },
        )
        .await
    }

    pub(super) async fn finish(&self) -> anyhow::Result<()> {
        assert_eq!(
            self.generations.load(Ordering::SeqCst),
            1,
            "code tool results never trigger a model turn"
        );
        self.resolve(
            &self.outer,
            PromiseResolution::Resolved {
                payload_ref: Some(self.store.put_bytes(b"{\"done\":true}".to_vec()).await?),
            },
        )
        .await?;
        let run =
            crate::support::live::wait_for_terminal_run(&self.api, &self.session_id, &self.run_id)
                .await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        assert_eq!(
            run.tool_batches.len(),
            1,
            "outer batch remains the only model batch"
        );
        assert_eq!(run.tool_batches[0].calls.len(), 1);
        assert_eq!(self.generations.load(Ordering::SeqCst), 2);
        crate::support::live::terminate_live_session(
            &self.client,
            &self.session_id,
            "code tool protocol test complete",
        )
        .await;
        Ok(())
    }
}

async fn declaration(
    blobs: &dyn BlobStore,
    name: &str,
    receiver_id: &str,
    joined: bool,
) -> anyhow::Result<WorkflowToolDeclaration> {
    Ok(WorkflowToolDeclaration::new(
        WorkflowToolDefinition {
            tool_id: harness::WorkflowToolId::new(name),
            revision: 1,
            semantic_type: format!("lightspeed.test.{name}.v1"),
            tool: ToolSpec {
                name: ToolName::new(name),
                execution: Default::default(),
                parallelism: ToolParallelism::ParallelSafe,
                kind: ToolKind::Function(FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: blobs.put_bytes(b"{\"type\":\"object\"}".to_vec()).await?,
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
            },
        },
        WorkflowToolTarget::Bound {
            receiver: harness::WorkflowEndpointRef {
                workflow_id: receiver_id.to_owned(),
                workflow_kind: "test_code_tool_runner".to_owned(),
            },
            dispatch: harness::BoundWorkflowToolDispatch::Push,
        },
        if joined {
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 120_000,
            }
        } else {
            WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: Some(120_000),
                max_promises: 1,
                key_source: harness::WorkflowToolCompletionKeySource::Reply,
            }
        },
    ))
}

pub(super) async fn scenario<F, Fut>(run: F) -> anyhow::Result<()>
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let store = pg_store_from_env().await?;
    seed_agent_default(&store, &crate::support::live::openai_live_model()).await?;
    let generations = Arc::new(AtomicUsize::new(0));
    let activity_store = store.clone();
    let activity_generations = generations.clone();
    run_with_live_worker_builder(
        move |client, _| async move {
            let hosted = Arc::new(SessionTools::from_pg_store(activity_store.clone()));
            let llm = Arc::new(OuterCallLlm {
                blobs: activity_store.clone(),
                generations: activity_generations,
            });
            Ok(WorkerActivities::for_universe(
                activity_store.config().universe_id,
                ActivityState::from_pg_store(activity_store, llm, hosted.clone())
                    .with_hosted_tools(hosted)
                    .with_workflow_tool_executions(client),
            ))
        },
        move |client, queue, session_id| async move {
            let runtime = temporal_runtime::worker::worker_runtime()?;
            let receiver_queue = format!("code-tool-receiver-{}", uuid::Uuid::new_v4().simple());
            let options = WorkerOptions::new(receiver_queue.clone()).register_workflow::<CodeToolReceiverWorkflow>()?.build();
            let mut receiver_worker = temporalio_sdk::Worker::new(&runtime, client.clone(), options)?;
            let shutdown_receiver = receiver_worker.shutdown_handle();
            let receiver_id = format!("code-tool-test-receiver-{}", uuid::Uuid::new_v4().simple());
            let receiver_handle = client.start_workflow(CodeToolReceiverWorkflow::run, (),
                WorkflowStartOptions::new(receiver_queue, receiver_id.clone()).build()).await?;
            let worker = receiver_worker.run();
            tokio::pin!(worker);
            let body = async {
            let api = GatewayAgentApi::builder(client.clone(), store.clone())
                .with_task_queue(queue)
                .build();
            let mut declarations = Vec::new();
            for (name, joined) in [(OUTER, true), (JOB, true), (AGENT, true), (SUBMIT, false)] {
                declarations.push(declaration(store.as_ref(), name, &receiver_id, joined).await?);
            }
            api.start_managed_session_for_workflow_with_profile(
                &session_id,
                false,
                None,
                harness::ManagedSessionWorkflowTools::v1(None, declarations),
            )
            .await?;
            api.put_session_config(api::SessionConfigPutParams {
                session_id: session_id.to_string(),
                expected_config_revision: None,
                config: api::SessionConfig {
                    features: Some(api::FeaturesConfig {
                        timers: Some(api::TimersFeature {
                            version: api::CURRENT_FEATURE_VERSION,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
            .await?;
            let run_id = api
                .start_run(RunStartParams {
                    notify_on_terminal: None,
                    submission_id: None,
                    session_id: session_id.to_string(),
                    source: RunStartSource::Input {
                        items: vec![InputItem::Text {
                            text: "run the simulated code tool".to_owned(),
                            provenance_ref: None,
                            origin: None,
                        }],
                    },
                    config: None,
                })
                .await?
                .result
                .run
                .id;
            let bridge = CodeToolClient::new(
                client.clone(),
                compose_workflow_id(live_universe_id()?, &session_id),
            );
            let outer = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let page = store
                        .read_after(ReadSessionEvents {
                            session_id: session_id.clone(),
                            after: None,
                            limit: 1000,
                        })
                        .await?;
                    let state =
                        temporal_workflow::reduce_session_entries(&page.entries)?.core_state;
                    if let Some(invocation) = state
                        .workflow_tools
                        .emissions
                        .values()
                        .find(|invocation| invocation.tool_id.as_str() == OUTER)
                        && state
                            .runs
                            .active
                            .as_ref()
                            .is_some_and(|run| run.parked_tool_batch.is_some())
                    {
                        return Ok::<_, anyhow::Error>(invocation.clone());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("outer joined call never parked"))??;
            let execution_id = format!("execution-{}", uuid::Uuid::new_v4().simple());
            let open = OpenCodeToolScopeRequest {
                execution_id: execution_id.clone(),
                parent_invocation_id: outer.invocation_id.clone(),
                allowed_tools: Some(["concurrency.sleep", "concurrency.await", JOB, AGENT, SUBMIT]
                    .into_iter()
                    .map(ToolName::new)
                    .collect::<BTreeSet<_>>()),
                max_calls: 32,
                max_in_flight: 8,
            };
            let scope = bridge
                .open_scope(open.clone(), Default::default())
                .await??;
            assert_eq!(scope, bridge.open_scope(open, Default::default()).await??);
            run(Fixture {
                client,
                api,
                store,
                session_id,
                run_id,
                receiver_id,
                execution_id,
                bridge,
                scope,
                outer,
                generations,
            })
            .await
            };
            tokio::pin!(body);
            let result = tokio::select! {
                result = &mut body => result,
                result = &mut worker => return Err(anyhow::anyhow!("code tool receiver stopped unexpectedly: {result:?}")),
            };
            let _ = receiver_handle.terminate(WorkflowTerminateOptions::default()).await;
            shutdown_receiver();
            tokio::time::timeout(Duration::from_secs(10), &mut worker).await??;
            result
        },
    )
    .await
}
