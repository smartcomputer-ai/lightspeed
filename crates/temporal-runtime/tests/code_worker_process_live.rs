//! Kill only the code worker process launched by this test, then recover its
//! workflow on a replacement process. The model is scripted; all worker roles,
//! storage, JavaScript, effects, heartbeat timeout and replay are production.

mod support;

use std::{process::Stdio, sync::Arc, time::Duration};

use api::AgentApiService;
use async_trait::async_trait;
use harness::{
    BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError,
    CoreAgentLlm, CoreAgentState, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, ObservedToolCall, PromiseResolution, SessionId,
    ToolCallId, ToolName, WorkflowToolTarget,
    storage::{BlobStore, ReadSessionEvents, SessionStore},
};
use serde_json::{Value, json};
use support::live::{
    LIVE_TEST_LOCK, require_storage_live_env, run_with_live_worker_builder, seed_agent_default,
    start_text_run, wait_until,
};
use temporal_runtime::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, SessionTools, WorkerActivities},
};
use temporal_workflow::{
    ACTIVITY_CODE_RUN, CodeExecutionWorkflow, ReducedSession, reduce_session_entries_from,
};
use temporalio_client::{WorkflowExecutionStatus, WorkflowQueryOptions};
use temporalio_common::protos::temporal::api::{
    enums::v1::TimeoutType, failure::v1::failure::FailureInfo,
    history::v1::history_event::Attributes,
};
use temporalio_sdk::workflow_replayer::{WorkflowReplayer, WorkflowReplayerOptions};
use tokio::process::{Child, Command};

struct CodeCallLlm(Arc<store_pg::PgStore>);

#[async_trait]
impl CoreAgentLlm for CodeCallLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let completed = request
            .request
            .context
            .entries
            .iter()
            .any(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }));
        let bytes = if completed {
            b"worker-loss report received".to_vec()
        } else {
            serde_json::to_vec(&json!({
                "code": "text(await tools.sleep({ms:1})); while (true) {}"
            }))
            .unwrap()
        };
        let reference =
            self.0
                .put_bytes(bytes)
                .await
                .map_err(|error| CoreAgentIoError::Failed {
                    message: error.to_string(),
                })?;
        let name = ToolName::new("code_execute");
        let call_id = ToolCallId::new("code-process-loss");
        let calls = if completed {
            Vec::new()
        } else {
            let tool_id = test_support::scripted_tool_id(&request, name.as_str());
            assert!(tool_id.is_some(), "code mode must advertise code_execute");
            vec![ObservedToolCall {
                call_id: call_id.clone(),
                tool_id,
                tool_name: name.clone(),
                provider_kind: None,
                arguments_ref: reference.clone(),
                native_call_ref: None,
            }]
        };
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: if completed {
                    ContextEntryKind::Message {
                        role: ContextMessageRole::Assistant,
                    }
                } else {
                    ContextEntryKind::ToolCall { call_id, name }
                },
                content: harness::ContentRef::text(reference),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: None,
                finish: if completed {
                    LlmFinish::Stop
                } else {
                    LlmFinish::ToolCalls
                },
                usage: None,
                tool_calls: calls,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

struct CodeProcess {
    child: Child,
    log: tempfile::NamedTempFile,
}

impl CodeProcess {
    fn start(session_queue: &str, code_queue: &str) -> anyhow::Result<Self> {
        let log = tempfile::NamedTempFile::new()?;
        let child = Command::new(env!("CARGO_BIN_EXE_lightspeed-runtime"))
            .args([
                "--roles",
                "code",
                "--task-queue",
                session_queue,
                "--code-task-queue",
                code_queue,
                "--code-max-concurrent-executions",
                "1",
            ])
            .env("LIGHTSPEED_AUTH_MODE", "authenticated")
            // Code does not use environment tools. These child-local values
            // satisfy the shared deployment client's routing configuration.
            .env("LIGHTSPEED_ENVIRONMENT_GATEWAY_URL", "http://127.0.0.1:1")
            .env(
                "LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN",
                "unused-code-test-route",
            )
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(log.as_file().try_clone()?)
            .stderr(log.as_file().try_clone()?)
            .kill_on_drop(true)
            .spawn()?;
        Ok(Self { child, log })
    }

    fn ensure_running(&mut self) -> anyhow::Result<()> {
        if let Some(status) = self.child.try_wait()? {
            let log = std::fs::read_to_string(self.log.path())?;
            anyhow::bail!("owned code worker exited with {status}: {log}");
        }
        Ok(())
    }

    async fn kill_and_reap(&mut self) -> anyhow::Result<()> {
        self.child.kill().await?;
        self.child.wait().await?;
        Ok(())
    }
}

async fn session_state(
    store: &store_pg::PgStore,
    session_id: &SessionId,
) -> anyhow::Result<CoreAgentState> {
    let mut reduced = ReducedSession::default();
    let mut after = None;
    loop {
        let page = store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; launches and kills its own code worker; run serially"]
async fn killed_code_worker_recovers_on_another_process_without_replaying_javascript()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    let store = pg_store_from_env().await?;
    seed_agent_default(&store, &support::live::openai_live_model()).await?;
    let activity_store = store.clone();
    let code_queue = format!("code-process-live-{}", uuid::Uuid::new_v4().simple());
    let activity_queue = code_queue.clone();
    run_with_live_worker_builder(
        move |client, _| async move {
            let tools = Arc::new(SessionTools::new(activity_store.clone(), activity_store.clone()));
            let llm = Arc::new(CodeCallLlm(activity_store.clone()));
            Ok(WorkerActivities::for_universe(activity_store.config().universe_id,
                ActivityState::from_pg_store(activity_store, llm, tools.clone())
                    .with_hosted_tools(tools).with_workflow_tool_executions(client)
                    .with_code_task_queue(activity_queue)))
        },
        move |client, session_queue, session_id| async move {
            let api = GatewayAgentApi::builder(client.clone(), store.clone())
                .with_task_queue(session_queue.clone()).with_code_task_queue(code_queue.clone()).build();
            let mut original = CodeProcess::start(&session_queue, &code_queue)?;
            api.start_session(api::SessionStartParams {
                session_id: Some(session_id.to_string()), display_name: None, access: None,
                metadata: Default::default(), profile: None, delete_after_close_ms: None,
                config: Some(api::SessionConfig { features: Some(api::FeaturesConfig {
                    code_mode: Some(api::CodeModeFeature { timeout_ms: 90_000, ..Default::default() }),
                    timers: Some(api::TimersFeature { version: api::CURRENT_FEATURE_VERSION }),
                    ..Default::default()
                }), ..Default::default() }),
            }).await?;
            let result = async {
                let run = start_text_run(&api, &session_id, "run one effect then compute").await?;
                wait_until("completed effect before process loss", Duration::from_secs(30), async || {
                    original.ensure_running()?;
                    let state = session_state(&store, &session_id).await?;
                    Ok(state.code_tools.scopes.values().any(|scope| {
                        !scope.closed && scope.calls.len() == 1 && scope.calls.values().all(|call| {
                            matches!(&call.status, harness::CodeToolCallStatus::Completed { result } if result.status == harness::ToolCallStatus::Succeeded)
                        })
                    }))
                }).await?;
                let state = session_state(&store, &session_id).await?;
                let invocation = state.workflow_tools.start_requests.values().find(|request| {
                    request.tool_id.as_str() == tools::code::CODE_EXECUTE_WORKFLOW_TOOL_ID
                }).ok_or_else(|| anyhow::anyhow!("code invocation missing"))?;
                let WorkflowToolTarget::Start { start } = &state.workflow_tools.bindings[&invocation.tool_id].target else {
                    anyhow::bail!("code invocation must have a workflow start recipe")
                };
                let execution_id = harness::workflow_tool_execution_id(&invocation.invocation_id, &start.recipe_fingerprint);
                original.kill_and_reap().await?;
                let mut replacement = CodeProcess::start(&session_queue, &code_queue)?;
                let recovered = async {
                    wait_until("worker-loss recovery", Duration::from_secs(50), async || {
                        replacement.ensure_running()?;
                        let status = api.read_run(api::RunReadParams { session_id: session_id.to_string(), run_id: run.id.clone() }).await?.result.run.status;
                        anyhow::ensure!(!matches!(status, api::RunStatus::Failed | api::RunStatus::Cancelled), "parent run unexpectedly {status:?}");
                        Ok(status == api::RunStatus::Completed)
                    }).await?;
                    let handle = client.get_workflow_handle::<CodeExecutionWorkflow>(execution_id);
                    let snapshot = handle.query(CodeExecutionWorkflow::snapshot, (), WorkflowQueryOptions::default()).await?;
                    let Some(PromiseResolution::Resolved { payload_ref: Some(reference) }) = snapshot.resolution else {
                        anyhow::bail!("replacement worker did not publish a recovery report")
                    };
                    let model: Value = serde_json::from_slice(&store.read_bytes(&reference).await?)?;
                    assert_eq!(model["status"], "interrupted", "{model}");
                    assert_eq!(model["interruption"], "activity_timed_out", "{model}");
                    assert_eq!(model["output_available"], false, "lost process has no runner receipt");
                    assert_eq!(model["calls"], json!({"succeeded": 1}));
                    assert!(model["cleanup_error"].is_null(), "{model}");
                    let report_ref = BlobRef::parse(model["report_ref"].as_str().unwrap())?;
                    let detail: Value = serde_json::from_slice(&store.read_bytes(&report_ref).await?)?;
                    assert_eq!(detail["scope"]["closed"], true);
                    assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 1);

                    wait_until("recovered workflow closure", Duration::from_secs(15), async || {
                        Ok(handle.describe(Default::default()).await?.status() == WorkflowExecutionStatus::Completed)
                    }).await?;
                    let events = handle.fetch_history(Default::default()).into_events().await?;
                    let scheduled: Vec<_> = events.iter().filter_map(|event| {
                        match &event.attributes {
                            Some(Attributes::ActivityTaskScheduledEventAttributes(attributes))
                                if attributes.activity_type.as_ref().is_some_and(|kind| kind.name == ACTIVITY_CODE_RUN) => Some((event.event_id, attributes)),
                            _ => None,
                        }
                    }).collect();
                    assert_eq!(scheduled.len(), 1, "one JS activity scheduled across process restart");
                    assert_eq!(scheduled[0].1.retry_policy.as_ref().unwrap().maximum_attempts, 1);
                    let started: Vec<_> = events.iter().filter_map(|event| match &event.attributes {
                        Some(Attributes::ActivityTaskStartedEventAttributes(attributes)) if attributes.scheduled_event_id == scheduled[0].0 => Some(attributes),
                        _ => None,
                    }).collect();
                    assert_eq!(started.len(), 1, "replacement must never start another JS attempt");
                    assert_eq!(started[0].attempt, 1);
                    let timed_out = events.iter().any(|event| match &event.attributes {
                        Some(Attributes::ActivityTaskTimedOutEventAttributes(attributes)) if attributes.scheduled_event_id == scheduled[0].0 => {
                            matches!(attributes.failure.as_ref().and_then(|failure| failure.failure_info.as_ref()), Some(FailureInfo::TimeoutFailureInfo(info)) if info.timeout_type == TimeoutType::Heartbeat as i32)
                        },
                        _ => false,
                    });
                    assert!(timed_out, "process loss must be detected by the real heartbeat timeout");
                    WorkflowReplayer::new(WorkflowReplayerOptions::new()
                        .register_workflow::<CodeExecutionWorkflow>()?.build())?
                        .replay_workflow(handle.fetch_history(Default::default())).await?;
                    Ok::<_, anyhow::Error>(())
                }.await;
                let stopped = replacement.kill_and_reap().await;
                recovered.and(stopped)
            }.await;
            let _ = api.close_session(api::SessionCloseParams { session_id: session_id.to_string(), force: true }).await;
            result
        },
    ).await
}
