use std::{
    env,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use api::{
    AgentApiErrorKind, AgentApiService, ContextAppendEntry, ContextAppendParams, InitializeParams,
    InputItem, McpServerCreateParams,
    McpServerDeleteParams, McpServerListParams, McpServerReadParams, McpServerStatus,
    RemoteMcpApprovalPolicy, RemoteMcpTransport, RunStartParams, RunStatus, SessionConfigInput,
    SessionEventsReadParams, SessionItemView, SessionMcpLinkParams, SessionMcpListParams,
    SessionMcpUnlinkParams, SessionReadParams, SessionStartParams, SessionStatus,
};
use api_projection::model_to_api;
use engine::{
    CommandCodec, CoreAgentCodec, CoreAgentCommand, CoreAgentLlm, CoreAgentTools, DynamicCommand,
    ModelSelection, ProviderApiKind, SessionId, storage::BlobStore,
};
use temporal_server::{
    default_model_from_env,
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, FakeLlm, FakeTools, WorkerActivities},
};
use temporal_workflow::{
    AgentAdmission, AgentAdmissionFailureKind, AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::{
    Client, WorkflowQueryOptions, WorkflowSignalOptions, WorkflowTerminateOptions,
};
use temporalio_common::{telemetry::TelemetryOptions, worker::WorkerTaskTypes};
use temporalio_sdk::{Worker, WorkerOptions};
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

static LIVE_TEST_LOCK: Mutex<()> = Mutex::new(());

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
#[ignore = "requires local/up.sh, Postgres, Temporal, and OPENAI_API_KEY (costs real money)"]
async fn temporal_live_session_start_then_run_start_completes_openai_run() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    require_openai_live_env()?;

    let activities = WorkerActivities::from_env().await?;
    run_with_live_worker(activities, run_openai_live_client).await
}

async fn run_with_live_worker<F, Fut>(
    activities: WorkerActivities,
    run_client: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Client, String, SessionId) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let task_queue = format!("forge-agent-live-{}", uuid::Uuid::new_v4().simple());
    let session_id = SessionId::new(format!("session_live_{}", uuid::Uuid::new_v4().simple()));
    let temporal_target =
        env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace =
        env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());

    let runtime = CoreRuntime::new_assume_tokio(
        RuntimeOptions::builder()
            .telemetry_options(TelemetryOptions::builder().build())
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
    )?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let worker_options = WorkerOptions::new(task_queue.clone())
        .register_workflow::<AgentSessionWorkflow>()
        .register_activities(activities)
        .task_types(WorkerTaskTypes::all())
        .build();
    let mut worker = Worker::new(&runtime, client.clone(), worker_options)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = worker.run();
    tokio::pin!(worker_future);

    let client_future = run_client(client, task_queue, session_id);
    tokio::pin!(client_future);

    let client_result = loop {
        tokio::select! {
            worker_result = worker_future.as_mut() => {
                return match worker_result {
                    Ok(()) => Err(anyhow::anyhow!("Temporal worker stopped before the live test completed")),
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

async fn fake_worker_activities() -> anyhow::Result<WorkerActivities> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(FakeLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok(WorkerActivities::new(ActivityState::from_pg_store(
        store, llm, tools,
    )))
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
    assert_eq!(initialized.result.server_info.name, "forge-agent");
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
    })
    .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    handle
        .signal(
            AgentSessionWorkflow::submit_admission,
            AgentAdmission {
                command: DynamicCommand::new("forge.core.command", 999, serde_json::json!({})),
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
    wait_for_admission_failure(
        &client,
        &session_id,
        AgentAdmissionFailureKind::RejectedCommand,
    )
    .await?;

    let status = handle
        .query(
            AgentSessionWorkflow::status,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(status.last_error, None);

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

fn final_assistant_text(run: &api::RunView) -> Option<&str> {
    run.items.iter().rev().find_map(|item| match item {
        SessionItemView::AssistantMessage { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

async fn wait_for_terminal_run(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<api::RunView> {
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(30) {
            anyhow::bail!("timed out waiting for run {run_id} to finish");
        }
        let session = api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        if let Some(run) = session
            .result
            .session
            .runs
            .into_iter()
            .find(|run| run.id == run_id)
        {
            if matches!(
                run.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
            ) {
                return Ok(run);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_admission_failure(
    client: &Client,
    session_id: &SessionId,
    kind: AgentAdmissionFailureKind,
) -> anyhow::Result<()> {
    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(30) {
            anyhow::bail!("timed out waiting for admission failure {kind:?}");
        }
        let status = handle
            .query(
                AgentSessionWorkflow::status,
                (),
                WorkflowQueryOptions::default(),
            )
            .await?;
        if status
            .admission_failures
            .iter()
            .any(|failure| failure.kind == kind)
        {
            assert_eq!(status.last_error, None);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_session_status(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    expected: SessionStatus,
) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(30) {
            anyhow::bail!("timed out waiting for session status {expected:?}");
        }
        let session = api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        if session.result.session.status == expected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn require_storage_live_env() -> anyhow::Result<()> {
    if env::var("FORGE_POSTGRES_URL")
        .or_else(|_| env::var("FORGE_TEST_POSTGRES_URL"))
        .is_err()
    {
        anyhow::bail!("temporal live test requires FORGE_POSTGRES_URL or FORGE_TEST_POSTGRES_URL");
    }
    if env::var("FORGE_PG_UNIVERSE_ID").is_err() {
        anyhow::bail!("temporal live test requires FORGE_PG_UNIVERSE_ID");
    }
    Ok(())
}

fn require_openai_live_env() -> anyhow::Result<()> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
        anyhow::anyhow!("OPENAI_API_KEY must be set to run the OpenAI Agent live test")
    })?;
    if api_key.trim().is_empty() {
        anyhow::bail!("OPENAI_API_KEY is set but empty");
    }
    Ok(())
}

fn openai_live_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: env::var("FORGE_OPENAI_MODEL")
            .or_else(|_| env::var("OPENAI_RESPONSES_MODEL"))
            .or_else(|_| env::var("OPENAI_LIVE_MODEL"))
            .or_else(|_| env::var("FORGE_CHAT_MODEL"))
            .unwrap_or_else(|_| "gpt-5.5".to_owned()),
    }
}
