mod support;

use std::{env, future::Future, sync::Arc, time::Duration};

use api::{
    AgentApiErrorKind, AgentApiService, AgentProfileInput, AgentProfileUpdatePatch,
    ContextAppendEntry, ContextAppendParams, FieldPatch, InitializeParams, InputItem,
    McpServerCreateParams, McpServerDeleteParams, McpServerListParams, McpServerReadParams,
    McpServerStatus, ProfileApplyParams, ProfileCreateParams, ProfileDeleteParams, ProfileDocument,
    ProfileId, ProfileInstructions, ProfileListParams, ProfileMcpLink, ProfileReadParams,
    ProfileSource, ProfileUpdateParams, RemoteMcpApprovalPolicy, RemoteMcpTransport,
    RunStartParams, SessionConfigInput, SessionEventsReadParams, SessionItemView,
    SessionMcpLinkParams, SessionMcpListParams, SessionMcpUnlinkParams, SessionReadParams,
    SessionStartParams, SessionStatus, ToolConfigInput,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use engine::{
    CommandCodec, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentCodec,
    CoreAgentCommand, CoreAgentIoError, CoreAgentLlm, CoreAgentTools, DynamicCommand, LlmFinish,
    LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus,
    ModelSelection, ObservedToolCall, RunId, SessionId, ToolBatchId, ToolCallId, ToolCallStatus,
    ToolInvocationRequest, ToolName, TurnId,
    storage::{BlobStore, ListSessionLinks, SessionLinkDirection, SessionStore},
};
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities, final_assistant_text, openai_live_model,
    require_openai_live_env, require_storage_live_env, run_with_live_worker,
    wait_for_admission_failure, wait_for_session_status, wait_for_terminal_run,
};
use temporal_server::{
    default_model_from_env,
    fleet::{
        AgentApiFleetRuntime, FLEET_CHILD_RELATIONSHIP, FleetInvocationContext, FleetService,
        FleetToolExecutor,
    },
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, SessionTools, WorkerActivities, core_runtime, worker_with_activities},
};
use temporal_workflow::{
    AgentAdmission, AgentAdmissionFailureKind, AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::{
    Client, WorkflowQueryOptions, WorkflowSignalOptions, WorkflowTerminateOptions,
};
use tools::fleet::{
    AGENT_SEND_TOOL_NAME, AGENT_SPAWN_TOOL_NAME, AGENT_WAIT_TOOL_NAME, AgentSendOutput,
    AgentSpawnOutput, PROFILE_LIST_TOOL_NAME, PROFILE_READ_TOOL_NAME, ProfileListOutput,
    ProfileReadOutput,
};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_session_start_then_run_start_completes_fake_runs() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_fake_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_continue_as_new_completes_later_fake_run() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_continue_as_new_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_run_start_missing_session_returns_not_found() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_missing_session_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_admission_failures_do_not_poison_workflow() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_admission_failure_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_context_append_is_idempotent_and_projected() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_context_append_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_outbox_enqueue_read_ack_round_trip() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_outbox_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_mcp_registry_and_session_links_materialize() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_mcp_registry_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_profiles_create_start_and_apply_idempotently() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_profiles_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_fleet_executor_spawns_child_workflow_and_run() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_fleet_spawn_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_fleet_executor_spawns_profile_child() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_fleet_profile_spawn_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_fleet_executor_lists_and_reads_profiles() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_fleet_profile_tools_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_agent_wait_parks_until_child_run_completes() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_scripted_fleet_live_worker(run_fleet_wait_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_agent_send_to_parent_wakes_idle_parent() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_scripted_fleet_live_worker(run_fleet_send_report_back_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh, Postgres, Temporal, and OPENAI_API_KEY (costs real money)"]
async fn temporal_live_session_start_then_run_start_completes_openai_run() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    require_openai_live_env()?;

    let activities = WorkerActivities::from_env().await?;
    run_with_live_worker(activities, run_openai_live_client).await
}

#[derive(Clone)]
struct FleetWaitScriptedLlm {
    blobs: Arc<dyn BlobStore>,
}

impl FleetWaitScriptedLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn latest_text_for_kind(
        &self,
        request: &LlmGenerationRequest,
        matches_kind: impl Fn(&ContextEntryKind) -> bool,
    ) -> Result<Option<String>, CoreAgentIoError> {
        for entry in request.request.context.entries.iter().rev() {
            if matches_kind(&entry.kind) {
                return self
                    .blobs
                    .read_text(&entry.content_ref)
                    .await
                    .map(Some)
                    .map_err(io_error);
            }
        }
        Ok(None)
    }

    async fn wait_tool_call_result(
        &self,
        request: &LlmGenerationRequest,
        target_session_id: &str,
        run_id: &str,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == AGENT_WAIT_TOOL_NAME)
        {
            return Err(CoreAgentIoError::Failed {
                message: "scripted wait test expected agent_wait to be available".to_owned(),
            });
        }

        let arguments = serde_json::json!({
            "waits": [{
                "target_session_id": target_session_id,
                "run_id": run_id
            }],
            "mode": "all",
            "timeout_ms": 15_000
        });
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("agent_wait_call_{}", request.turn_id.as_u64()));
        let tool_name = ToolName::new(AGENT_WAIT_TOOL_NAME);
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content_ref: arguments_ref.clone(),
                media_type: Some("application/json".to_owned()),
                preview: Some(format!("{tool_name}({arguments})")),
                provider_kind: Some("fleet-wait-script".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fleet-wait-tool-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fleet-wait-script".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
                context_token_estimate: None,
            },
        })
    }

    async fn send_parent_tool_call_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == AGENT_SEND_TOOL_NAME)
        {
            return Err(CoreAgentIoError::Failed {
                message: "scripted send test expected agent_send to be available".to_owned(),
            });
        }

        let arguments = serde_json::json!({
            "to": {
                "kind": "parent"
            },
            "text": "mode i live result"
        });
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("agent_send_call_{}", request.turn_id.as_u64()));
        let tool_name = ToolName::new(AGENT_SEND_TOOL_NAME);
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content_ref: arguments_ref.clone(),
                media_type: Some("application/json".to_owned()),
                preview: Some(format!("{tool_name}({arguments})")),
                provider_kind: Some("fleet-wait-script".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fleet-send-tool-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fleet-wait-script".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
                context_token_estimate: None,
            },
        })
    }

    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
        text: String,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let output_ref = self
            .blobs
            .put_bytes(text.into_bytes())
            .await
            .map_err(io_error)?;
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                content_ref: output_ref,
                media_type: Some("text/plain".to_owned()),
                preview: Some("fleet wait scripted final".to_owned()),
                provider_kind: Some("fleet-wait-script".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!(
                    "fleet-wait-final-{}",
                    request.turn_id.as_u64()
                )),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[async_trait]
impl CoreAgentLlm for FleetWaitScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if let Some(tool_result) = self
            .latest_text_for_kind(&request, |kind| {
                matches!(kind, ContextEntryKind::ToolResult { .. })
            })
            .await?
        {
            if tool_result.contains("agent_wait resolved") {
                let final_output = self
                    .latest_text_for_kind(&request, |kind| {
                        matches!(
                            kind,
                            ContextEntryKind::Message {
                                role: ContextMessageRole::User
                            }
                        )
                    })
                    .await?
                    .unwrap_or_default();
                return self
                    .final_result(
                        &request,
                        format!(
                            "wait completed after agent_wait: {tool_result}\n\nagent final output: {final_output}"
                        ),
                    )
                    .await;
            }
            if tool_result.contains("Delivered message") {
                return self
                    .final_result(
                        &request,
                        format!("reported to parent after agent_send: {tool_result}"),
                    )
                    .await;
            }
            return self
                .final_result(&request, format!("tool completed: {tool_result}"))
                .await;
        }

        let user_text = self
            .latest_text_for_kind(&request, |kind| {
                matches!(
                    kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::User
                    }
                )
            })
            .await?
            .unwrap_or_default();

        if user_text.starts_with("SLOW_CHILD") {
            tokio::time::sleep(Duration::from_secs(5)).await;
            return self
                .final_result(&request, "slow child completed".to_owned())
                .await;
        }

        if user_text.contains("REPORT_TO_PARENT") {
            return self.send_parent_tool_call_result(&request).await;
        }

        if user_text.contains("mode i live result") {
            return self
                .final_result(&request, "parent received mode i live result".to_owned())
                .await;
        }

        if let Some((target_session_id, run_id)) = parse_wait_script(&user_text) {
            return self
                .wait_tool_call_result(&request, target_session_id, run_id)
                .await;
        }

        self.final_result(&request, "scripted run completed".to_owned())
            .await
    }
}

fn parse_wait_script(text: &str) -> Option<(&str, &str)> {
    let mut parts = text.split_whitespace();
    if parts.next()? != "WAIT_FOR_CHILD" {
        return None;
    }
    let target_session_id = parts.next()?;
    let run_id = parts.next()?;
    Some((target_session_id, run_id))
}

async fn run_with_scripted_fleet_live_worker<F, Fut>(run_client: F) -> anyhow::Result<()>
where
    F: FnOnce(
        Client,
        SessionId,
        Arc<GatewayAgentApi>,
        Arc<dyn BlobStore>,
        Arc<dyn SessionStore>,
        ModelSelection,
    ) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let task_queue = format!("lightspeed-agent-live-{}", uuid::Uuid::new_v4().simple());
    let session_id = SessionId::new(format!("session_live_{}", uuid::Uuid::new_v4().simple()));
    let temporal_target =
        env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace =
        env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());

    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store.clone())
            .with_task_queue(task_queue.clone())
            .with_default_model(model.clone())
            .with_max_steps_per_input(128)
            .build(),
    );

    let blobs_for_worker: Arc<dyn BlobStore> = store.clone();
    let llm =
        Arc::new(FleetWaitScriptedLlm::new(blobs_for_worker.clone())) as Arc<dyn CoreAgentLlm>;
    let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
    let tools = Arc::new(SessionTools::from_pg_store_with_fleet_runtime(
        store.clone(),
        fleet_runtime,
    )) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::new(ActivityState::from_pg_store(store.clone(), llm, tools));
    let mut worker =
        worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = worker.run();
    tokio::pin!(worker_future);

    let blobs_for_client: Arc<dyn BlobStore> = store.clone();
    let sessions_for_client: Arc<dyn SessionStore> = store;
    let client_future = run_client(
        client.clone(),
        session_id,
        api,
        blobs_for_client,
        sessions_for_client,
        model,
    );
    tokio::pin!(client_future);

    let client_result = loop {
        tokio::select! {
            worker_result = worker_future.as_mut() => {
                return match worker_result {
                    Ok(()) => Err(anyhow::anyhow!("Temporal worker stopped before the live wait test completed")),
                    Err(error) => Err(error.context("Temporal worker failed")),
                };
            }
            client_result = client_future.as_mut() => break client_result,
        }
    };

    shutdown_worker();
    tokio::time::timeout(Duration::from_secs(10), worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 10 seconds"))??;
    client_result
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

async fn run_fake_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();

    let initialized = api.initialize(InitializeParams::default()).await?;
    assert_eq!(initialized.result.server_info.name, "lightspeed-agent");
    assert!(initialized.result.capabilities.history_read);
    assert!(initialized.result.capabilities.event_log);

    let started = api
        .start_session(SessionStartParams {
            session_id: Some(session_id.as_str().to_owned()),
            cwd: None,
            config: Some(SessionConfigInput {
                model: Some(model_to_api(&model)),
                ..SessionConfigInput::default()
            }),
            profile: None,
        })
        .await?;
    assert_eq!(started.result.session.id, session_id.as_str());

    let first = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "hello temporal agent".to_owned(),
            }],
            config: None,
        })
        .await?;
    let first_run = wait_for_terminal_run(&api, &session_id, &first.result.run.id).await?;
    let first_output = final_assistant_text(&first_run).expect("first assistant output");
    assert!(first_output.contains("Fake agent completed run"));

    let second = api
        .start_run(RunStartParams {
            submission_id: Some("live-retry-1".to_owned()),
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "second session-start input".to_owned(),
            }],
            config: None,
        })
        .await?;
    let second_run = wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    let second_output = final_assistant_text(&second_run).expect("second assistant output");
    assert!(second_output.contains("Fake agent completed run"));

    // Retried run/start with the same submission id and input returns the
    // original run instead of starting a second one.
    let retried = api
        .start_run(RunStartParams {
            submission_id: Some("live-retry-1".to_owned()),
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "second session-start input".to_owned(),
            }],
            config: None,
        })
        .await?;
    assert_eq!(retried.result.run.id, second.result.run.id);

    // Same submission id with different input is a typed rejection.
    let mismatch = api
        .start_run(RunStartParams {
            submission_id: Some("live-retry-1".to_owned()),
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "different input".to_owned(),
            }],
            config: None,
        })
        .await;
    let mismatch_error = mismatch.expect_err("duplicate submission with different input fails");
    assert_eq!(mismatch_error.kind, api::AgentApiErrorKind::Rejected);

    // Retried session/start with the same session id returns the session.
    let restarted = api
        .start_session(SessionStartParams {
            session_id: Some(session_id.as_str().to_owned()),
            cwd: None,
            config: None,
            profile: None,
        })
        .await?;
    assert_eq!(restarted.result.session.id, session_id.as_str());

    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    assert!(read.result.session.runs.len() >= 2);

    let events = api
        .read_session_events(SessionEventsReadParams {
            wait_ms: Some(2_000),
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(64),
        })
        .await?;
    assert!(!events.result.events.is_empty());

    // Long-poll at the head: no new events, so the read parks until the
    // wait elapses and returns an empty page with no cursor movement.
    let head_cursor = events.result.head_cursor;
    let parked_started = std::time::Instant::now();
    let parked = api
        .read_session_events(SessionEventsReadParams {
            wait_ms: Some(1_000),
            session_id: session_id.as_str().to_owned(),
            after: head_cursor,
            limit: Some(64),
        })
        .await?;
    assert!(parked.result.events.is_empty());
    assert!(parked.result.complete);
    assert!(parked_started.elapsed() >= std::time::Duration::from_millis(900));

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_fleet_spawn_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store.clone())
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .with_max_steps_per_input(128)
            .build(),
    );

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            tools: Some(ToolConfigInput {
                fleet: Some(true),
                ..ToolConfigInput::default()
            }),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let blobs: Arc<dyn BlobStore> = store.clone();
    let sessions: Arc<dyn SessionStore> = store.clone();
    let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
    let service = FleetService::new(sessions, fleet_runtime);
    let executor = FleetToolExecutor::new(blobs.clone(), service);
    let arguments_ref = blobs
        .put_bytes(br#"{"input":"child live task"}"#.to_vec())
        .await?;
    let observed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id.clone(),
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                call_id: ToolCallId::new("call_live_1"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_live_1"),
                tool_name: ToolName::new(AGENT_SPAWN_TOOL_NAME),
                arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(result.status, ToolCallStatus::Succeeded);
    let output_ref = result.output_ref.expect("spawn output ref");
    let output: AgentSpawnOutput = serde_json::from_slice(&blobs.read_bytes(&output_ref).await?)?;
    let child_run_id = output
        .child_run_id
        .as_deref()
        .expect("spawn should start child run");
    let child_session_id = SessionId::try_new(output.child_session_id.clone())?;

    let child = api
        .read_session(SessionReadParams {
            session_id: child_session_id.as_str().to_owned(),
        })
        .await?;
    assert_eq!(child.result.session.id, child_session_id.as_str());
    assert_eq!(
        child
            .result
            .session
            .config
            .as_ref()
            .expect("child config")
            .tools
            .fleet,
        true
    );

    let run = wait_for_terminal_run(api.as_ref(), &child_session_id, child_run_id).await?;
    let output_text = final_assistant_text(&run).expect("child assistant output");
    assert!(output_text.contains("Fake agent completed run"));

    let send_args = serde_json::json!({
        "to": {
            "kind": "session",
            "target_session_id": child_session_id.as_str()
        },
        "text": "child follow-up live task"
    });
    let arguments_ref = blobs.put_bytes(serde_json::to_vec(&send_args)?).await?;
    let send_result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id.clone(),
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(2),
                call_id: ToolCallId::new("call_live_2"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_live_2"),
                tool_name: ToolName::new(AGENT_SEND_TOOL_NAME),
                arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(send_result.status, ToolCallStatus::Succeeded);
    let output_ref = send_result.output_ref.expect("send output ref");
    let send_output: AgentSendOutput =
        serde_json::from_slice(&blobs.read_bytes(&output_ref).await?)?;
    assert_eq!(
        send_output.target_session_id.as_deref(),
        Some(child_session_id.as_str())
    );
    let send_run_id = send_output.run_id.expect("send run id");
    assert_ne!(send_run_id, child_run_id);
    let follow_up_run =
        wait_for_terminal_run(api.as_ref(), &child_session_id, &send_run_id).await?;
    let output_text = final_assistant_text(&follow_up_run).expect("follow-up assistant output");
    assert!(output_text.contains("Fake agent completed run"));

    let child_handle =
        client.get_workflow_handle::<AgentSessionWorkflow>(child_session_id.as_str());
    let status = child_handle
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(status.last_error, None);

    for id in [session_id.as_str(), child_session_id.as_str()] {
        let handle = client.get_workflow_handle::<AgentSessionWorkflow>(id);
        let _ = handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("fleet live test cleanup")
                    .build(),
            )
            .await;
    }
    Ok(())
}

async fn run_fleet_profile_spawn_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store.clone())
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .with_max_steps_per_input(128)
            .build(),
    );
    let profile_id = ProfileId::new(format!(
        "live_profile_child_{}",
        uuid::Uuid::new_v4().simple()
    ));

    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: Some("Live child profile".to_owned()),
            description: Some("Fleet live profile child".to_owned()),
            document: ProfileDocument {
                config: Some(SessionConfigInput {
                    tools: Some(ToolConfigInput {
                        fleet: Some(true),
                        web_fetch: Some(false),
                        ..ToolConfigInput::default()
                    }),
                    ..SessionConfigInput::default()
                }),
                instructions: Some(ProfileInstructions::Text {
                    text: "You are a profile-spawned live child.".to_owned(),
                }),
                mounts: Vec::new(),
                mcp: Vec::new(),
                environments: Vec::new(),
            },
        },
    })
    .await?;

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            tools: Some(ToolConfigInput {
                fleet: Some(true),
                ..ToolConfigInput::default()
            }),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let blobs: Arc<dyn BlobStore> = store.clone();
    let sessions: Arc<dyn SessionStore> = store.clone();
    let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
    let service = FleetService::new(sessions.clone(), fleet_runtime);
    let executor = FleetToolExecutor::new(blobs.clone(), service);
    let spawn_args = serde_json::json!({
        "input": "profile child live task",
        "profile": {
            "kind": "named",
            "profileId": profile_id.as_str()
        }
    });
    let arguments_ref = blobs.put_bytes(serde_json::to_vec(&spawn_args)?).await?;
    let observed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id.clone(),
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                call_id: ToolCallId::new("call_profile_spawn"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_profile_spawn"),
                tool_name: ToolName::new(AGENT_SPAWN_TOOL_NAME),
                arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(result.status, ToolCallStatus::Succeeded);
    let output_ref = result.output_ref.expect("spawn output ref");
    let output: AgentSpawnOutput = serde_json::from_slice(&blobs.read_bytes(&output_ref).await?)?;
    let child_run_id = output
        .child_run_id
        .as_deref()
        .expect("spawn should start child run");
    let child_session_id = SessionId::try_new(output.child_session_id.clone())?;

    let child_record = sessions
        .load_session(&child_session_id)
        .await?
        .expect("profile child session record");
    assert_eq!(child_record.source_session_id, None);
    assert_eq!(child_record.source_seq, None);

    let child = api
        .read_session(SessionReadParams {
            session_id: child_session_id.as_str().to_owned(),
        })
        .await?;
    let child_config = child.result.session.config.as_ref().expect("child config");
    assert!(child_config.tools.fleet);
    assert!(!child_config.tools.web_fetch);
    assert!(
        child
            .result
            .session
            .active_context
            .items
            .iter()
            .any(|item| matches!(item, SessionItemView::SystemEvent { text, .. } if text == "Profile instructions")),
        "profile instructions should be projected"
    );

    let links = sessions
        .list_links(ListSessionLinks {
            session_id: session_id.clone(),
            direction: SessionLinkDirection::Outgoing,
            relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
            limit: 10,
        })
        .await?;
    let link = links
        .iter()
        .find(|link| link.to_session_id == child_session_id)
        .expect("fleet child link");
    assert_eq!(
        link.metadata["profile"],
        serde_json::to_value(ProfileSource::Named {
            profile_id: profile_id.clone()
        })?
    );

    let run = wait_for_terminal_run(api.as_ref(), &child_session_id, child_run_id).await?;
    let output_text = final_assistant_text(&run).expect("child assistant output");
    assert!(output_text.contains("Fake agent completed run"));

    api.delete_profile(ProfileDeleteParams { profile_id })
        .await?;

    for id in [session_id.as_str(), child_session_id.as_str()] {
        let handle = client.get_workflow_handle::<AgentSessionWorkflow>(id);
        let _ = handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("fleet profile live test cleanup")
                    .build(),
            )
            .await;
    }
    Ok(())
}

async fn run_fleet_profile_tools_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client, store.clone())
            .with_task_queue(task_queue)
            .with_default_model(model)
            .with_max_steps_per_input(128)
            .build(),
    );
    let profile_id = ProfileId::new(format!(
        "live_profile_tools_{}",
        uuid::Uuid::new_v4().simple()
    ));

    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: Some("Live profile tools".to_owned()),
            description: Some("Fleet profile discovery live test".to_owned()),
            document: ProfileDocument {
                config: Some(SessionConfigInput {
                    tools: Some(ToolConfigInput {
                        fleet: Some(true),
                        web_fetch: Some(false),
                        ..ToolConfigInput::default()
                    }),
                    ..SessionConfigInput::default()
                }),
                instructions: Some(ProfileInstructions::Text {
                    text: "Profile read tools should return this document.".to_owned(),
                }),
                mounts: Vec::new(),
                mcp: Vec::new(),
                environments: Vec::new(),
            },
        },
    })
    .await?;

    let blobs: Arc<dyn BlobStore> = store.clone();
    let sessions: Arc<dyn SessionStore> = store.clone();
    let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
    let service = FleetService::new(sessions, fleet_runtime);
    let executor = FleetToolExecutor::new(blobs.clone(), service);
    let observed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;

    let list_arguments_ref = blobs.put_bytes(br#"{}"#.to_vec()).await?;
    let list_result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id.clone(),
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                call_id: ToolCallId::new("call_profile_list"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_profile_list"),
                tool_name: ToolName::new(PROFILE_LIST_TOOL_NAME),
                arguments_ref: list_arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(list_result.status, ToolCallStatus::Succeeded);
    let list_output_ref = list_result.output_ref.expect("profile list output ref");
    let list_output: ProfileListOutput =
        serde_json::from_slice(&blobs.read_bytes(&list_output_ref).await?)?;
    assert!(
        list_output
            .profiles
            .iter()
            .any(|profile| profile.profile_id == profile_id)
    );

    let read_args = serde_json::json!({ "profile_id": profile_id.as_str() });
    let read_arguments_ref = blobs.put_bytes(serde_json::to_vec(&read_args)?).await?;
    let read_result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id,
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(2),
                call_id: ToolCallId::new("call_profile_read"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_profile_read"),
                tool_name: ToolName::new(PROFILE_READ_TOOL_NAME),
                arguments_ref: read_arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(read_result.status, ToolCallStatus::Succeeded);
    let read_output_ref = read_result.output_ref.expect("profile read output ref");
    let read_output: ProfileReadOutput =
        serde_json::from_slice(&blobs.read_bytes(&read_output_ref).await?)?;
    assert_eq!(read_output.profile.profile_id, profile_id);
    assert_eq!(
        read_output.profile.description.as_deref(),
        Some("Fleet profile discovery live test")
    );
    assert!(read_output.profile.document.instructions.is_some());

    api.delete_profile(ProfileDeleteParams { profile_id })
        .await?;
    Ok(())
}

async fn run_fleet_wait_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            tools: Some(ToolConfigInput {
                fleet: Some(true),
                ..ToolConfigInput::default()
            }),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
    let service = FleetService::new(sessions, fleet_runtime);
    let executor = FleetToolExecutor::new(blobs.clone(), service);
    let arguments_ref = blobs
        .put_bytes(br#"{"input":"SLOW_CHILD wait target"}"#.to_vec())
        .await?;
    let observed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let spawn_result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id.clone(),
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                call_id: ToolCallId::new("call_wait_spawn"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_wait_spawn"),
                tool_name: ToolName::new(AGENT_SPAWN_TOOL_NAME),
                arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(spawn_result.status, ToolCallStatus::Succeeded);
    let output_ref = spawn_result.output_ref.expect("spawn output ref");
    let output: AgentSpawnOutput = serde_json::from_slice(&blobs.read_bytes(&output_ref).await?)?;
    let child_run_id = output
        .child_run_id
        .as_deref()
        .expect("spawn should start child run");
    let child_session_id = SessionId::try_new(output.child_session_id.clone())?;

    let parent_run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: format!("WAIT_FOR_CHILD {child_session_id} {child_run_id}"),
            }],
            config: None,
        })
        .await?;

    wait_for_active_waits(&client, &session_id, 1).await?;

    let child_run = wait_for_terminal_run(api.as_ref(), &child_session_id, child_run_id).await?;
    let child_output = final_assistant_text(&child_run).expect("child assistant output");
    assert_eq!(child_output, "slow child completed");

    let parent_run =
        wait_for_terminal_run(api.as_ref(), &session_id, &parent_run.result.run.id).await?;
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert!(
        parent_output.contains("wait completed after agent_wait"),
        "expected parent to observe agent_wait result, got: {parent_output}"
    );
    assert!(
        parent_output.contains("outcome terminal"),
        "expected terminal wait result in parent output, got: {parent_output}"
    );
    assert!(
        parent_output.contains("slow child completed"),
        "expected child final output in parent output, got: {parent_output}"
    );

    let parent_handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let parent_status = parent_handle
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(parent_status.active_waits, 0);
    assert_eq!(parent_status.last_error, None);

    let child_handle =
        client.get_workflow_handle::<AgentSessionWorkflow>(child_session_id.as_str());
    let child_status = child_handle
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(child_status.last_error, None);

    for id in [session_id.as_str(), child_session_id.as_str()] {
        let handle = client.get_workflow_handle::<AgentSessionWorkflow>(id);
        let _ = handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("fleet wait live test cleanup")
                    .build(),
            )
            .await;
    }
    Ok(())
}

async fn run_fleet_send_report_back_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            tools: Some(ToolConfigInput {
                fleet: Some(true),
                ..ToolConfigInput::default()
            }),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let parent_status = client
        .get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str())
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(parent_status.active_run, None);
    assert!(parent_status.queued_runs.is_empty());
    assert!(parent_status.completed_runs.is_empty());

    let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
    let service = FleetService::new(sessions, fleet_runtime);
    let executor = FleetToolExecutor::new(blobs.clone(), service);
    let spawn_args = serde_json::json!({
        "input": "REPORT_TO_PARENT live task",
        "lifecycle": {
            "close_on_terminal": true
        },
        "report_back": {
            "instructions": "Send exactly: mode i live result"
        }
    });
    let arguments_ref = blobs.put_bytes(serde_json::to_vec(&spawn_args)?).await?;
    let observed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let spawn_result = executor
        .invoke(
            FleetInvocationContext {
                parent_session_id: session_id.clone(),
                parent_run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                call_id: ToolCallId::new("call_mode_i_spawn"),
                observed_at_ms,
            },
            &ToolInvocationRequest {
                call_id: ToolCallId::new("call_mode_i_spawn"),
                tool_name: ToolName::new(AGENT_SPAWN_TOOL_NAME),
                arguments_ref,
                execution_target: None,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(spawn_result.status, ToolCallStatus::Succeeded);
    let output_ref = spawn_result.output_ref.expect("spawn output ref");
    let output: AgentSpawnOutput = serde_json::from_slice(&blobs.read_bytes(&output_ref).await?)?;
    let child_run_id = output
        .child_run_id
        .as_deref()
        .expect("spawn should start child run");
    let child_session_id = SessionId::try_new(output.child_session_id.clone())?;

    let child_run = wait_for_terminal_run(api.as_ref(), &child_session_id, child_run_id).await?;
    let child_output = final_assistant_text(&child_run).expect("child assistant output");
    assert!(
        child_output.contains("reported to parent after agent_send"),
        "expected child to send report to parent, got: {child_output}"
    );

    let parent_run = wait_for_terminal_run_with_assistant_output(
        api.as_ref(),
        &session_id,
        "mode i live result",
    )
    .await?;
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert_eq!(parent_output, "parent received mode i live result");
    wait_for_session_status(api.as_ref(), &child_session_id, SessionStatus::Closed).await?;

    let parent_status = client
        .get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str())
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(parent_status.last_error, None);
    assert_eq!(parent_status.completed_runs.len(), 1);

    let child_status = client
        .get_workflow_handle::<AgentSessionWorkflow>(child_session_id.as_str())
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(child_status.last_error, None);

    for id in [session_id.as_str(), child_session_id.as_str()] {
        let handle = client.get_workflow_handle::<AgentSessionWorkflow>(id);
        let _ = handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("fleet send report-back live test cleanup")
                    .build(),
            )
            .await;
    }
    Ok(())
}

async fn wait_for_terminal_run_with_assistant_output(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    marker: &str,
) -> anyhow::Result<api::RunView> {
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(30) {
            anyhow::bail!(
                "timed out waiting for session {session_id} to produce assistant output containing {marker:?}"
            );
        }
        let session = api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        for run in session.result.session.runs {
            if matches!(
                run.status,
                api::RunStatus::Completed | api::RunStatus::Failed | api::RunStatus::Cancelled
            ) && final_assistant_text(&run).is_some_and(|text| text.contains(marker))
            {
                return Ok(run);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_active_waits(
    client: &Client,
    session_id: &SessionId,
    expected: usize,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    loop {
        if started.elapsed() > Duration::from_secs(10) {
            anyhow::bail!(
                "timed out waiting for session {session_id} to report {expected} active wait(s)"
            );
        }
        let status = handle
            .query(
                AgentSessionWorkflow::status,
                (),
                WorkflowQueryOptions::default(),
            )
            .await?;
        if status.active_waits == expected {
            return Ok(());
        }
        if let Some(error) = status.last_error {
            anyhow::bail!("session {session_id} reported workflow error while waiting: {error}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run_continue_as_new_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .with_continue_as_new_history_threshold(1)
        .build();

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    api.start_run(RunStartParams {
        submission_id: None,
        session_id: session_id.as_str().to_owned(),
        input: vec![InputItem::Text {
            text: "first run before continue as new".to_owned(),
        }],
        config: None,
    })
    .await?;

    let second = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "second run after continue as new".to_owned(),
            }],
            config: None,
        })
        .await?;
    let second_run = wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    let second_output = final_assistant_text(&second_run).expect("second assistant output");
    assert!(second_output.contains("Fake agent completed run"));

    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    assert!(
        read.result.session.runs.len() >= 2,
        "projected session should include runs committed before and after continue-as-new"
    );

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent continue-as-new live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_missing_session_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client, store)
        .with_task_queue(task_queue)
        .with_default_model(model)
        .with_max_steps_per_input(128)
        .build();

    let error = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "this should not create a session".to_owned(),
            }],
            config: None,
        })
        .await
        .expect_err("missing session run/start should fail");
    assert!(matches!(error.kind, AgentApiErrorKind::NotFound));
    Ok(())
}

async fn run_outbox_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    use messaging::{EnqueueOutboundMessage, OutboundOrigin, OutboundPayload, OutboxStore};

    let store = pg_store_from_env().await?;
    store.initialize().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store.clone())
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    let enqueue = |text: &str| EnqueueOutboundMessage {
        session_id: session_id.clone(),
        run_id: Some(engine::RunId::new(1)),
        origin: OutboundOrigin::ToolCall,
        payload: OutboundPayload::Send {
            text: text.to_owned(),
            reply_to: None,
        },
        created_at_ms: now_ms,
    };
    let first = OutboxStore::enqueue(store.as_ref(), enqueue("first message")).await?;
    let second = OutboxStore::enqueue(store.as_ref(), enqueue("second message")).await?;

    // Read pending after the first entry's predecessor: both visible.
    let page = api
        .read_outbox(api::OutboxReadParams {
            after: Some(first.seq.saturating_sub(1)),
            limit: Some(16),
            wait_ms: Some(1_000),
        })
        .await?;
    let read: Vec<&str> = page
        .result
        .entries
        .iter()
        .map(|entry| entry.outbox_id.as_str())
        .collect();
    assert!(read.contains(&first.outbox_id.as_str()));
    assert!(read.contains(&second.outbox_id.as_str()));
    assert!(page.result.next_after >= second.seq);

    // Delivered entries disappear from pending reads.
    let acked = api
        .ack_outbox(api::OutboxAckParams {
            outbox_id: first.outbox_id.clone(),
            result: api::OutboundAckInput::Delivered {
                channel_message_id: Some("tg-100".to_owned()),
            },
        })
        .await?;
    assert_eq!(acked.result.status, api::OutboundStatusView::Delivered);

    let page = api
        .read_outbox(api::OutboxReadParams {
            after: Some(first.seq.saturating_sub(1)),
            limit: Some(16),
            wait_ms: None,
        })
        .await?;
    assert!(
        page.result
            .entries
            .iter()
            .all(|entry| entry.outbox_id != first.outbox_id)
    );

    // Retryable failure keeps the entry pending with attempts counted.
    let failed = api
        .ack_outbox(api::OutboxAckParams {
            outbox_id: second.outbox_id.clone(),
            result: api::OutboundAckInput::Failed {
                error: "bridge offline".to_owned(),
                retryable: true,
            },
        })
        .await?;
    assert_eq!(failed.result.status, api::OutboundStatusView::Pending);
    assert_eq!(failed.result.attempts, 1);

    let unknown = api
        .ack_outbox(api::OutboxAckParams {
            outbox_id: "outbox_missing".to_owned(),
            result: api::OutboundAckInput::Delivered {
                channel_message_id: None,
            },
        })
        .await;
    assert_eq!(
        unknown.expect_err("missing outbox id fails").kind,
        AgentApiErrorKind::NotFound
    );

    Ok(())
}

async fn run_context_append_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let first_text = "[telegram:group Engineering] Alice (12:01): the deploy looks stuck";
    let second_text = "[telegram:group Engineering] Bob (12:02): restarting the worker now";
    let appended = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: vec![
                ContextAppendEntry {
                    key: "channel.room.msg-1".to_owned(),
                    item: InputItem::Text {
                        text: first_text.to_owned(),
                    },
                },
                ContextAppendEntry {
                    key: "channel.room.msg-2".to_owned(),
                    item: InputItem::Text {
                        text: second_text.to_owned(),
                    },
                },
            ],
        })
        .await?;
    assert_eq!(
        appended.result.applied_keys,
        vec!["channel.room.msg-1", "channel.room.msg-2"]
    );
    assert!(appended.result.unchanged_keys.is_empty());
    let first_revision = appended.result.context_revision;

    // Room events are visible as ordinary user-message context items.
    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    let context_texts: Vec<&str> = read
        .result
        .session
        .active_context
        .items
        .iter()
        .filter_map(|item| match item {
            SessionItemView::UserMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(context_texts.contains(&first_text));
    assert!(context_texts.contains(&second_text));

    // Re-sending the same batch is a no-op: keys are the idempotency handle.
    let replayed = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: vec![
                ContextAppendEntry {
                    key: "channel.room.msg-1".to_owned(),
                    item: InputItem::Text {
                        text: first_text.to_owned(),
                    },
                },
                ContextAppendEntry {
                    key: "channel.room.msg-2".to_owned(),
                    item: InputItem::Text {
                        text: second_text.to_owned(),
                    },
                },
            ],
        })
        .await?;
    assert!(replayed.result.applied_keys.is_empty());
    assert_eq!(
        replayed.result.unchanged_keys,
        vec!["channel.room.msg-1", "channel.room.msg-2"]
    );
    assert_eq!(replayed.result.context_revision, first_revision);

    // Same key with different content upserts in place.
    let edited = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: vec![ContextAppendEntry {
                key: "channel.room.msg-2".to_owned(),
                item: InputItem::Text {
                    text: "[telegram:group Engineering] Bob (12:02): edited message".to_owned(),
                },
            }],
        })
        .await?;
    assert_eq!(edited.result.applied_keys, vec!["channel.room.msg-2"]);
    assert!(edited.result.context_revision > first_revision);

    // Invalid input is rejected at admission with a typed error.
    let empty = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: Vec::new(),
        })
        .await;
    assert_eq!(
        empty.expect_err("empty append must fail").kind,
        AgentApiErrorKind::InvalidRequest
    );
    let blank_item = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: vec![ContextAppendEntry {
                key: "channel.room.msg-3".to_owned(),
                item: InputItem::Text {
                    text: "   ".to_owned(),
                },
            }],
        })
        .await;
    assert_eq!(
        blank_item.expect_err("blank item must fail").kind,
        AgentApiErrorKind::InvalidRequest
    );

    // A run started after the appends completes normally with the room
    // context present in the session.
    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "summarize the room".to_owned(),
            }],
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    Ok(())
}

async fn run_admission_failure_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    handle
        .signal(
            AgentSessionWorkflow::submit_admission,
            AgentAdmission {
                command: DynamicCommand::new("lightspeed.core.command", 999, serde_json::json!({})),
            },
            WorkflowSignalOptions::default(),
        )
        .await?;
    wait_for_admission_failure(
        &client,
        &session_id,
        AgentAdmissionFailureKind::InvalidCommand,
    )
    .await?;

    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "valid run after malformed command".to_owned(),
            }],
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    let close_command = CoreAgentCodec.encode_command(&CoreAgentCommand::CloseSession)?;
    handle
        .signal(
            AgentSessionWorkflow::submit_admission,
            AgentAdmission {
                command: close_command,
            },
            WorkflowSignalOptions::default(),
        )
        .await?;
    wait_for_session_status(&api, &session_id, SessionStatus::Closed).await?;

    let error = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "run after close should be rejected".to_owned(),
            }],
            config: None,
        })
        .await
        .expect_err("closed session run/start should be rejected");
    assert!(matches!(error.kind, AgentApiErrorKind::Rejected));
    let session = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    assert_eq!(session.result.session.status, SessionStatus::Closed);

    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent admission failure live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_mcp_registry_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();
    let server_id = format!("crm_{}", uuid::Uuid::new_v4().simple());

    let created = api
        .create_mcp_server(McpServerCreateParams {
            server_id: server_id.clone(),
            display_name: Some("CRM".to_owned()),
            server_url: format!("https://{server_id}.example.com/mcp"),
            transport: RemoteMcpTransport::Auto,
            default_server_label: "crm".to_owned(),
            description: Some("CRM MCP server".to_owned()),
            allowed_tools: Some(vec!["lookup_customer".to_owned()]),
            approval_default: RemoteMcpApprovalPolicy::Never,
            defer_loading_default: Some(true),
            auth_policy: api::McpServerAuthPolicy::None,
            status: McpServerStatus::Active,
        })
        .await?;
    assert_eq!(created.result.server.server_id, server_id);

    let read = api
        .read_mcp_server(McpServerReadParams {
            server_id: server_id.clone(),
        })
        .await?;
    assert_eq!(read.result.server.default_server_label, "crm");

    let listed = api
        .list_mcp_servers(McpServerListParams {
            status: Some(McpServerStatus::Active),
        })
        .await?;
    assert!(
        listed
            .result
            .servers
            .iter()
            .any(|server| server.server_id == server_id)
    );

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let linked = api
        .link_session_mcp(SessionMcpLinkParams {
            session_id: session_id.as_str().to_owned(),
            server_id: server_id.clone(),
            tool_id: Some("mcp_crm".to_owned()),
            server_label: None,
            allowed_tools: Some(vec!["lookup_customer".to_owned()]),
            approval: Some(RemoteMcpApprovalPolicy::Never),
            defer_loading: Some(true),
            auth_grant_id: None,
        })
        .await?;
    assert_eq!(linked.result.link.tool_id, "mcp_crm");
    assert_eq!(linked.result.link.server_label, "crm");
    assert_eq!(
        linked.result.link.allowed_tools,
        Some(vec!["lookup_customer".to_owned()])
    );

    let session_links = api
        .list_session_mcp(SessionMcpListParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    assert_eq!(session_links.result.links.len(), 1);
    assert_eq!(session_links.result.links[0].tool_id, "mcp_crm");

    let unlinked = api
        .unlink_session_mcp(SessionMcpUnlinkParams {
            session_id: session_id.as_str().to_owned(),
            tool_id: "mcp_crm".to_owned(),
        })
        .await?;
    assert!(unlinked.result.links.is_empty());

    let deleted = api
        .delete_mcp_server(McpServerDeleteParams { server_id })
        .await?;
    assert_eq!(deleted.result.server.default_server_label, "crm");

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent MCP live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_profiles_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();
    let profile_id = ProfileId::new(format!("live_profile_{}", uuid::Uuid::new_v4().simple()));
    let server_id = format!("profile_crm_{}", uuid::Uuid::new_v4().simple());

    api.create_mcp_server(McpServerCreateParams {
        server_id: server_id.clone(),
        display_name: Some("Profile CRM".to_owned()),
        server_url: format!("https://{server_id}.example.com/mcp"),
        transport: RemoteMcpTransport::Auto,
        default_server_label: "profile_crm".to_owned(),
        description: Some("Profile live MCP server".to_owned()),
        allowed_tools: Some(vec!["lookup_customer".to_owned()]),
        approval_default: RemoteMcpApprovalPolicy::Never,
        defer_loading_default: Some(true),
        auth_policy: api::McpServerAuthPolicy::None,
        status: McpServerStatus::Active,
    })
    .await?;

    let created = api
        .create_profile(ProfileCreateParams {
            profile: AgentProfileInput {
                profile_id: profile_id.clone(),
                display_name: Some("Live profile".to_owned()),
                description: Some("Initial live profile".to_owned()),
                document: ProfileDocument {
                    config: Some(SessionConfigInput {
                        tools: Some(ToolConfigInput {
                            fleet: Some(true),
                            web_fetch: Some(false),
                            ..ToolConfigInput::default()
                        }),
                        ..SessionConfigInput::default()
                    }),
                    instructions: Some(ProfileInstructions::Text {
                        text: "Use the profile instructions in this live test.".to_owned(),
                    }),
                    mounts: Vec::new(),
                    mcp: vec![ProfileMcpLink {
                        server_id: server_id.clone(),
                        tool_id: Some("mcp_profile_crm".to_owned()),
                        server_label: None,
                        allowed_tools: Some(vec!["lookup_customer".to_owned()]),
                        approval: Some(RemoteMcpApprovalPolicy::Never),
                        defer_loading: Some(true),
                        auth_grant_id: None,
                    }],
                    environments: Vec::new(),
                },
            },
        })
        .await?;
    assert_eq!(created.result.profile.profile_id, profile_id);
    assert_eq!(created.result.profile.revision, 1);

    let updated = api
        .update_profile(ProfileUpdateParams {
            profile_id: profile_id.clone(),
            expected_revision: Some(1),
            patch: AgentProfileUpdatePatch {
                description: Some(FieldPatch::Set("Updated live profile".to_owned())),
                ..AgentProfileUpdatePatch::default()
            },
        })
        .await?;
    assert_eq!(updated.result.profile.revision, 2);
    assert_eq!(
        updated.result.profile.description.as_deref(),
        Some("Updated live profile")
    );

    let read = api
        .read_profile(ProfileReadParams {
            profile_id: profile_id.clone(),
        })
        .await?;
    assert_eq!(read.result.profile.revision, 2);
    let listed = api.list_profiles(ProfileListParams {}).await?;
    assert!(
        listed
            .result
            .profiles
            .iter()
            .any(|profile| profile.profile_id == profile_id)
    );

    let started = api
        .start_session(SessionStartParams {
            session_id: Some(session_id.as_str().to_owned()),
            cwd: None,
            config: Some(SessionConfigInput {
                model: Some(model_to_api(&model)),
                ..SessionConfigInput::default()
            }),
            profile: Some(ProfileSource::Named {
                profile_id: profile_id.clone(),
            }),
        })
        .await?;
    let session = &started.result.session;
    let config = session.config.as_ref().expect("session config");
    assert!(config.tools.fleet);
    assert!(!config.tools.web_fetch);
    assert!(
        session
            .active_context
            .items
            .iter()
            .any(|item| matches!(item, SessionItemView::SystemEvent { text, .. } if text == "Profile instructions")),
        "profile instructions should be projected"
    );

    let linked = api
        .list_session_mcp(SessionMcpListParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    assert_eq!(linked.result.links.len(), 1);
    assert_eq!(linked.result.links[0].tool_id, "mcp_profile_crm");
    assert_eq!(linked.result.links[0].server_label, "profile_crm");

    let applied = api
        .apply_profile(ProfileApplyParams {
            session_id: session_id.as_str().to_owned(),
            profile: ProfileSource::Named {
                profile_id: profile_id.clone(),
            },
            expected_config_revision: Some(session.config_revision),
            expected_tools_revision: Some(session.active_tools.revision),
        })
        .await?;
    assert!(!applied.result.applied.config_changed);
    assert!(!applied.result.applied.instructions_changed);
    assert_eq!(applied.result.applied.mounts_changed, 0);
    assert_eq!(applied.result.applied.mcp_changed, 0);
    assert_eq!(applied.result.applied.environments_changed, 0);

    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "run after profile start".to_owned(),
            }],
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    api.delete_profile(ProfileDeleteParams { profile_id })
        .await?;
    api.delete_mcp_server(McpServerDeleteParams { server_id })
        .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent profile live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_openai_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let instructions = "You are Agent in a live integration test. Do not call tools for this test. Reply with the exact phrase requested by the user.";
    let instructions_ref = store.put_bytes(instructions.as_bytes().to_vec()).await?;
    let model = openai_live_model();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_instructions_ref(instructions_ref)
        .with_max_steps_per_input(128)
        .build();

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "Reply with exactly: real llm agent ok".to_owned(),
            }],
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("OpenAI assistant output");
    let normalized = output.to_lowercase();
    assert!(
        normalized.contains("real llm agent ok"),
        "expected real LLM marker in output: {output}"
    );
    assert!(
        !output.contains("Fake agent completed run"),
        "expected OpenAI-backed output, got fake output: {output}"
    );

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent openai live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}
