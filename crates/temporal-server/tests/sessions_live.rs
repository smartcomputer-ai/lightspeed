//! Live coverage for managed-session lifecycle, admission, context, checkpoints,
//! and provider-backed happy paths.

mod support;

use api::{
    AgentApiErrorKind, AgentApiService, ContextAppendEntry, ContextAppendParams,
    ContextAppendStatus, ContextEntryKindView, ContextMessageRoleView, FeaturesConfig,
    InitializeParams, InlineAgentProfile, InputItem, ProfileDocument, ProfileInstructions,
    ProfileSource, RunListParams, RunStartParams, RunStartSource, SessionConfig,
    SessionConfigPutParams, SessionDeleteParams, SessionEventsReadParams, SessionLifecycleStatus,
    SessionListParams, SessionReadParams, SessionStartParams, SessionStatus, TimersFeature,
};
use api_projection::model_to_api;
use engine::{
    CoreAgentCommand, SessionId,
    storage::{BlobStore, SessionStore},
};
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities, final_assistant_text, live_workflow_handle,
    openai_completions_live_model, openai_live_model, read_session_view, require_openai_live_env,
    require_storage_live_env, run_with_live_worker, start_text_run, wait_for_admission_failure,
    wait_for_session_status, wait_for_terminal_run,
};
use temporal_server::{
    default_model_from_env, gateway::GatewayAgentApi, pg_store_from_env, worker::WorkerActivities,
};
use temporal_workflow::{AgentAdmission, AgentAdmissionFailureKind, AgentSessionWorkflow};
use temporalio_client::{
    Client, WorkflowDescribeOptions, WorkflowSignalOptions, WorkflowTerminateOptions,
};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_session_start_then_run_start_completes_fake_runs() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_fake_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres + MinIO env"]
async fn temporal_live_session_checkpoints_and_bounded_run_reads() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_checkpoint_and_bounded_reads_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_session_lifecycle_list_and_closed_only_delete() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_lifecycle_delete_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_continue_as_new_completes_later_fake_run() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_continue_as_new_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_run_start_missing_session_returns_not_found() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_missing_session_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_admission_failures_do_not_poison_workflow() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_admission_failure_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_context_append_is_idempotent_and_projected() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_context_append_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, Temporal, and OPENAI_API_KEY (costs real money)"]
async fn temporal_live_session_start_then_run_start_completes_openai_run() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    require_openai_live_env()?;

    let activities = WorkerActivities::from_env().await?;
    run_with_live_worker(activities, run_openai_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, Temporal, and OPENAI_API_KEY (costs real money)"]
async fn temporal_live_openai_completions_tool_call_round_trip() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    require_openai_live_env()?;

    let activities = WorkerActivities::from_env().await?;
    run_with_live_worker(activities, |client, queue, session_id| {
        run_builtin_tool_live_client(client, queue, session_id, openai_completions_live_model())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, Temporal, and OPENAI_API_KEY (costs real money)"]
async fn temporal_live_openai_responses_tool_call_round_trip() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    require_openai_live_env()?;
    let activities = WorkerActivities::from_env().await?;
    run_with_live_worker(activities, |client, queue, session_id| {
        run_builtin_tool_live_client(client, queue, session_id, openai_live_model())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, Temporal, and ANTHROPIC_API_KEY (costs real money)"]
async fn temporal_live_anthropic_messages_tool_call_round_trip() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    anyhow::ensure!(
        std::env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.trim().is_empty()),
        "ANTHROPIC_API_KEY must be set to run the Anthropic live test"
    );
    let model = engine::ModelSelection {
        api_kind: engine::ProviderApiKind::AnthropicMessages,
        provider_id: "anthropic".to_owned(),
        model: std::env::var("ANTHROPIC_MESSAGES_MODEL")
            .or_else(|_| std::env::var("ANTHROPIC_LIVE_MODEL"))
            .unwrap_or_else(|_| "claude-opus-5".to_owned()),
    };
    let activities = WorkerActivities::from_env().await?;
    run_with_live_worker(activities, |client, queue, session_id| {
        run_builtin_tool_live_client(client, queue, session_id, model)
    })
    .await
}

async fn run_checkpoint_and_bounded_reads_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client, store.clone())
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: Some("Checkpoint and bounded reads live test".to_owned()),
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let mut run_ids = Vec::new();
    for index in 0..21 {
        let started = start_text_run(&api, &session_id, &format!("bounded run {index}")).await?;
        wait_for_terminal_run(&api, &session_id, &started.id).await?;
        run_ids.push(started.id);
    }

    let default_page = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?
        .result;
    assert_eq!(default_page.session.runs.len(), 20);
    assert!(default_page.has_older_runs);
    assert_eq!(
        default_page
            .session
            .runs
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        run_ids[1..]
            .iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    let older_page = api
        .list_runs(RunListParams {
            session_id: session_id.as_str().to_owned(),
            cursor: default_page.next_run_cursor,
            limit: None,
        })
        .await?
        .result;
    assert_eq!(older_page.runs.len(), 1);
    assert_eq!(older_page.runs[0].id, run_ids[0]);
    assert!(!older_page.has_older_runs);
    assert!(older_page.next_cursor.is_none());

    let limited_page = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: Some(3),
        })
        .await?
        .result;
    assert_eq!(limited_page.session.runs.len(), 3);
    assert!(limited_page.has_older_runs);
    assert_eq!(
        limited_page
            .session
            .runs
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        run_ids[18..]
            .iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    let record = store
        .load_session(&session_id)
        .await?
        .expect("live session exists");
    let checkpoint = store
        .load_checkpoint(&session_id)
        .await?
        .expect("terminal cadence writes a checkpoint");
    assert!(checkpoint.through_seq < record.head.as_ref().expect("session head").seq);
    let checkpoint_bytes = store.read_bytes(&checkpoint.state_ref).await?;
    assert_eq!(checkpoint_bytes.len() as u64, checkpoint.byte_len);
    assert!(!checkpoint_bytes.is_empty());

    sqlx::query(
        "UPDATE session_checkpoints SET format_version = 999 WHERE universe_id = $1 AND session_id = $2",
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .execute(store.pool())
    .await?;
    let fallback = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: Some(1),
        })
        .await?
        .result;
    assert_eq!(fallback.session.runs.len(), 1);
    assert_eq!(fallback.session.runs[0].id, run_ids[20]);

    let fallback_record = store
        .load_session(&session_id)
        .await?
        .expect("fallback live session exists");
    let repaired = store
        .load_checkpoint(&session_id)
        .await?
        .expect("full replay repairs a rejected checkpoint");
    assert_eq!(repaired.format_version, 1);
    assert_eq!(
        repaired.through_seq,
        fallback_record.head.as_ref().expect("fallback head").seq
    );
    assert!(repaired.through_seq > checkpoint.through_seq);

    let repair_run = start_text_run(&api, &session_id, "checkpoint repair").await?;
    wait_for_terminal_run(&api, &session_id, &repair_run.id).await?;
    let repaired_record = store
        .load_session(&session_id)
        .await?
        .expect("repaired live session exists");
    let after_one_more_run = store
        .load_checkpoint(&session_id)
        .await?
        .expect("repaired checkpoint remains available");
    assert_eq!(after_one_more_run.through_seq, repaired.through_seq);
    assert!(
        after_one_more_run.through_seq < repaired_record.head.as_ref().expect("repaired head").seq
    );

    // Run detail is a single complete projection: partial pages would lose
    // cross-event state such as tool batches and approval pairs.
    let detail = api
        .read_run(api::RunReadParams {
            session_id: session_id.as_str().to_owned(),
            run_id: repair_run.id.clone(),
        })
        .await?
        .result;
    assert_eq!(detail.run.id, repair_run.id);
    assert_eq!(detail.run.status, api::RunStatus::Completed);
    assert!(detail.run.started_at_ms.is_some());
    assert!(detail.run.completed_at_ms.is_some());
    assert!(matches!(
        detail.run.source,
        api::RunViewSource::Input { .. }
    ));
    assert!(detail.run.entries.iter().any(|entry| {
        matches!(
            entry.kind,
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::Assistant,
            }
        )
    }));

    api.close_session(api::SessionCloseParams {
        session_id: session_id.as_str().to_owned(),
        force: false,
    })
    .await?;
    wait_for_session_status(&api, &session_id, SessionStatus::Closed).await?;
    api.delete_session(SessionDeleteParams {
        session_id: session_id.as_str().to_owned(),
        cascade: false,
    })
    .await?;
    assert!(store.load_checkpoint(&session_id).await?.is_none());
    Ok(())
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
        .build();

    let initialized = api.initialize(InitializeParams::default()).await?;
    assert_eq!(initialized.result.server_info.name, "lightspeed-agent");
    assert!(initialized.result.capabilities.history_read);
    assert!(initialized.result.capabilities.event_log);

    let started = api
        .start_session(SessionStartParams {
            metadata: Default::default(),
            session_id: Some(session_id.as_str().to_owned()),
            display_name: None,
            config: Some(SessionConfig {
                model: Some(model_to_api(&model)),
                ..SessionConfig::default()
            }),
            profile: None,
            environment: None,
            delete_after_close_ms: None,
        })
        .await?;
    assert_eq!(started.result.session.id, session_id.as_str());
    let started_view = read_session_view(&api, &session_id).await?;
    assert!(
        !started_view
            .active_context
            .entries
            .iter()
            .any(
                |entry| entry.key.as_deref() == Some(tools::catalog::VFS_CATALOG_CONTEXT_KEY)
                    && matches!(entry.kind, ContextEntryKindView::Catalog { .. })
            )
    );

    let mut enabled_config = started_view.config.clone().expect("started session config");
    let mut enabled_features = enabled_config.features.unwrap_or_default();
    enabled_features.vfs = Some(api::VfsFeature {
        working_directory: None,
        version: api::CURRENT_FEATURE_VERSION,
        workspace_links: Vec::new(),
        tools: None,
        prompts: None,
        skills: None,
    });
    enabled_features.environments = Some(api::EnvironmentsFeature {
        tools: Some(api::EnvironmentToolSurface::Edit),
        commands: true,
        working_directory: None,
        prompts: None,
        version: api::CURRENT_FEATURE_VERSION,
        providers: None,
        registration_keys: None,
        selection_tools: false,
        jobs: false,
        skills: None,
    });
    enabled_config.features = Some(enabled_features);
    let enabled = api
        .put_session_config(SessionConfigPutParams {
            session_id: session_id.as_str().to_owned(),
            expected_config_revision: Some(started.result.session.config_revision),
            config: enabled_config,
        })
        .await?;
    let enabled_view = read_session_view(&api, &session_id).await?;
    assert!(
        enabled_view
            .active_context
            .entries
            .iter()
            .any(
                |entry| entry.key.as_deref() == Some(tools::catalog::VFS_CATALOG_CONTEXT_KEY)
                    && matches!(entry.kind, ContextEntryKindView::Catalog { .. })
            )
    );
    let selection_tool_ids = [
        "environment.list",
        "environment.activate",
        "environment.deactivate",
    ];
    assert!(
        enabled_view
            .active_tools
            .tools
            .iter()
            .any(|tool| { tool.tool_id == "environment.read" })
    );
    assert!(selection_tool_ids.iter().all(|name| {
        enabled_view
            .active_tools
            .tools
            .iter()
            .all(|tool| tool.tool_id != *name)
    }));

    let mut selection_config = enabled_view.config.clone().expect("enabled session config");
    selection_config
        .features
        .as_mut()
        .and_then(|features| features.environments.as_mut())
        .expect("environment feature")
        .selection_tools = true;
    let selection_enabled = api
        .put_session_config(SessionConfigPutParams {
            session_id: session_id.as_str().to_owned(),
            expected_config_revision: Some(enabled.result.session.config_revision),
            config: selection_config,
        })
        .await?;
    let selection_enabled_view = read_session_view(&api, &session_id).await?;
    assert!(selection_tool_ids.iter().all(|name| {
        selection_enabled_view
            .active_tools
            .tools
            .iter()
            .any(|tool| tool.tool_id == *name)
    }));
    assert!(
        selection_enabled_view
            .active_tools
            .tools
            .iter()
            .any(|tool| { tool.tool_id == "environment.read" })
    );

    let mut disabled_config = selection_enabled_view
        .config
        .clone()
        .expect("enabled session config");
    if let Some(features) = disabled_config.features.as_mut() {
        features.vfs = None;
        features.environments = None;
    }
    api.put_session_config(SessionConfigPutParams {
        session_id: session_id.as_str().to_owned(),
        expected_config_revision: Some(selection_enabled.result.session.config_revision),
        config: disabled_config,
    })
    .await?;
    let disabled_view = read_session_view(&api, &session_id).await?;
    assert!(
        !disabled_view
            .active_context
            .entries
            .iter()
            .any(
                |entry| entry.key.as_deref() == Some(tools::catalog::VFS_CATALOG_CONTEXT_KEY)
                    && matches!(entry.kind, ContextEntryKindView::Catalog { .. })
            )
    );

    let first = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "hello temporal agent".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let first_run = wait_for_terminal_run(&api, &session_id, &first.result.run.id).await?;
    let first_output = final_assistant_text(&first_run).expect("first assistant output");
    assert!(first_output.contains("Fake agent completed run"));

    let second = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: Some("live-retry-1".to_owned()),
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "second session-start input".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let second_run = wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    let second_output = final_assistant_text(&second_run).expect("second assistant output");
    assert!(second_output.contains("Fake agent completed run"));

    // Retried session/runs/start with the same submission id and input returns the
    // original run instead of starting a second one.
    let retried = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: Some("live-retry-1".to_owned()),
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "second session-start input".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    assert_eq!(retried.result.run.id, second.result.run.id);

    // Same submission id with different input is a typed rejection.
    let mismatch = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: Some("live-retry-1".to_owned()),
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "different input".to_owned(),
                }],
            },
            config: None,
        })
        .await;
    let mismatch_error = mismatch.expect_err("duplicate submission with different input fails");
    assert_eq!(mismatch_error.kind, api::AgentApiErrorKind::Rejected);

    // Retried session/start with the same session id returns the session.
    let restarted = api
        .start_session(SessionStartParams {
            metadata: Default::default(),
            session_id: Some(session_id.as_str().to_owned()),
            display_name: None,
            config: None,
            profile: None,
            environment: None,
            delete_after_close_ms: None,
        })
        .await?;
    assert_eq!(restarted.result.session.id, session_id.as_str());

    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?;
    assert!(read.result.session.runs.len() >= 2);

    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
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
            direction: Default::default(),
            before: None,
            wait_ms: Some(1_000),
            session_id: session_id.as_str().to_owned(),
            after: head_cursor,
            limit: Some(64),
        })
        .await?;
    assert!(parked.result.events.is_empty());
    assert!(parked.result.complete);
    assert!(parked_started.elapsed() >= std::time::Duration::from_millis(900));

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_lifecycle_delete_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client, store)
        .with_task_queue(task_queue)
        .with_default_model(model)
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: Some("Lifecycle delete live test".to_owned()),
        config: None,
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let listed = api.list_sessions(SessionListParams::default()).await?;
    let open_summary = listed
        .result
        .sessions
        .iter()
        .find(|summary| summary.id == session_id.as_str())
        .expect("started session is listed");
    assert_eq!(open_summary.lifecycle_status, SessionLifecycleStatus::Open);

    let delete_open = api
        .delete_session(SessionDeleteParams {
            session_id: session_id.as_str().to_owned(),
            cascade: false,
        })
        .await
        .expect_err("open session deletion must be rejected");
    assert_eq!(delete_open.kind, AgentApiErrorKind::Rejected);

    api.close_session(api::SessionCloseParams {
        session_id: session_id.as_str().to_owned(),
        force: false,
    })
    .await?;

    let listed = api.list_sessions(SessionListParams::default()).await?;
    let closed_summary = listed
        .result
        .sessions
        .iter()
        .find(|summary| summary.id == session_id.as_str())
        .expect("closed session remains listed");
    assert_eq!(
        closed_summary.lifecycle_status,
        SessionLifecycleStatus::Closed
    );

    let deleted = api
        .delete_session(SessionDeleteParams {
            session_id: session_id.as_str().to_owned(),
            cascade: false,
        })
        .await?;
    assert_eq!(
        deleted.result.session.lifecycle_status,
        SessionLifecycleStatus::Closed
    );

    let listed = api.list_sessions(SessionListParams::default()).await?;
    assert!(
        listed
            .result
            .sessions
            .iter()
            .all(|summary| summary.id != session_id.as_str())
    );
    let read_deleted = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await
        .expect_err("deleted session must not be readable");
    assert_eq!(read_deleted.kind, AgentApiErrorKind::NotFound);
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
        .with_continue_as_new_history_threshold(1)
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let initial_temporal_run_id = live_workflow_handle(&client, &session_id)?
        .describe(WorkflowDescribeOptions::default())
        .await?
        .run_id()
        .to_owned();

    let first = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "first run before continue as new".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let first_run_id = first.result.run.id.clone();
    let first_run = wait_for_terminal_run(&api, &session_id, &first_run_id).await?;
    assert_eq!(first_run.id, first_run_id);
    let continued_temporal_run_id = live_workflow_handle(&client, &session_id)?
        .describe(WorkflowDescribeOptions::default())
        .await?
        .run_id()
        .to_owned();
    assert_ne!(
        continued_temporal_run_id, initial_temporal_run_id,
        "the active Lightspeed run should cross a Temporal execution boundary"
    );

    let second = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "second run after continue as new".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let second_run = wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    let second_output = final_assistant_text(&second_run).expect("second assistant output");
    assert!(second_output.contains("Fake agent completed run"));

    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?;
    assert!(
        read.result.session.runs.len() >= 2,
        "projected session should include runs committed before and after continue-as-new"
    );

    let handle = live_workflow_handle(&client, &session_id)?;
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
        .build();

    let error = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "this should not create a session".to_owned(),
                }],
            },
            config: None,
        })
        .await
        .expect_err("missing session session/runs/start should fail");
    assert!(matches!(error.kind, AgentApiErrorKind::NotFound));
    Ok(())
}

async fn run_context_append_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store.clone())
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let first_text = "[telegram:group Engineering] Alice (12:01): the deploy looks stuck";
    let second_text = "[telegram:group Engineering] Bob (12:02): restarting the worker now";
    let borrowed = store.put_bytes(first_text.as_bytes().to_vec()).await?;
    sqlx::query("UPDATE cas_blobs SET created_at_ms = 1, touched_at_ms = 1 WHERE universe_id = $1 AND digest = $2")
        .bind(store.config().universe_id)
        .bind(borrowed.as_str().trim_start_matches("sha256:"))
        .execute(store.pool())
        .await?;
    let appended = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: vec![
                ContextAppendEntry {
                    key: "channel.room.msg-1".to_owned(),
                    item: InputItem::TextRef {
                        origin: None,
                        blob_ref: borrowed.to_string(),
                    },
                },
                ContextAppendEntry {
                    key: "channel.room.msg-2".to_owned(),
                    item: InputItem::Text {
                        origin: None,
                        text: second_text.to_owned(),
                    },
                },
            ],
        })
        .await?;
    assert_eq!(
        appended
            .result
            .results
            .iter()
            .map(|result| (result.key.as_str(), result.status))
            .collect::<Vec<_>>(),
        vec![
            ("channel.room.msg-1", ContextAppendStatus::Applied),
            ("channel.room.msg-2", ContextAppendStatus::Applied)
        ]
    );
    assert!(
        store
            .blob_timestamps(&borrowed)
            .await?
            .expect("admitted blob")
            .1
            > 1,
        "admission must refresh a borrowed ref before signaling the workflow"
    );
    let first_revision = appended.result.context_revision;

    // Room events are visible as ordinary user-message context items.
    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?;
    let context_texts: Vec<&str> = read
        .result
        .session
        .active_context
        .entries
        .iter()
        .filter_map(|entry| match entry.kind {
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::User,
            } => entry.text.as_deref(),
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
                        origin: None,
                        text: first_text.to_owned(),
                    },
                },
                ContextAppendEntry {
                    key: "channel.room.msg-2".to_owned(),
                    item: InputItem::Text {
                        origin: None,
                        text: second_text.to_owned(),
                    },
                },
            ],
        })
        .await?;
    assert_eq!(
        replayed
            .result
            .results
            .iter()
            .map(|result| (result.key.as_str(), result.status))
            .collect::<Vec<_>>(),
        vec![
            ("channel.room.msg-1", ContextAppendStatus::Unchanged),
            ("channel.room.msg-2", ContextAppendStatus::Unchanged)
        ]
    );
    assert_eq!(replayed.result.context_revision, first_revision);

    // Same key with different content upserts in place.
    let edited = api
        .append_context(ContextAppendParams {
            session_id: session_id.as_str().to_owned(),
            entries: vec![ContextAppendEntry {
                key: "channel.room.msg-2".to_owned(),
                item: InputItem::Text {
                    origin: None,
                    text: "[telegram:group Engineering] Bob (12:02): edited message".to_owned(),
                },
            }],
        })
        .await?;
    assert_eq!(
        edited
            .result
            .results
            .iter()
            .map(|result| (result.key.as_str(), result.status))
            .collect::<Vec<_>>(),
        vec![("channel.room.msg-2", ContextAppendStatus::Applied)]
    );
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
                    origin: None,
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
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "summarize the room".to_owned(),
                }],
            },
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
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let handle = live_workflow_handle(&client, &session_id)?;
    handle
        .signal(
            AgentSessionWorkflow::submit_admissions,
            vec![AgentAdmission {
                // No run is active, so admission rejects this command; the
                // session must keep serving later admissions regardless.
                command: CoreAgentCommand::RequestRunSteering {
                    run_id: engine::RunId::new(1),
                    input: Vec::new(),
                },
                correlation_token: None,
            }],
            WorkflowSignalOptions::default(),
        )
        .await?;
    wait_for_admission_failure(
        &client,
        &session_id,
        AgentAdmissionFailureKind::RejectedCommand,
    )
    .await?;

    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "valid run after malformed command".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    handle
        .signal(
            AgentSessionWorkflow::submit_admissions,
            vec![AgentAdmission {
                command: CoreAgentCommand::CloseSession { force: false },
                correlation_token: None,
            }],
            WorkflowSignalOptions::default(),
        )
        .await?;
    wait_for_session_status(&api, &session_id, SessionStatus::Closed).await?;

    let error = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "run after close should be rejected".to_owned(),
                }],
            },
            config: None,
        })
        .await
        .expect_err("closed session session/runs/start should be rejected");
    assert!(matches!(error.kind, AgentApiErrorKind::Rejected));
    let session = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
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

async fn run_openai_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let instructions = "You are Agent in a live integration test. Do not call tools for this test. Reply with the exact phrase requested by the user.";
    let model = openai_live_model();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: Some(ProfileSource::Inline {
            profile: Box::new(InlineAgentProfile {
                display_name: Some("OpenAI live test".to_owned()),
                description: None,
                document: ProfileDocument {
                    instructions: Some(ProfileInstructions::Text {
                        text: instructions.to_owned(),
                    }),
                    ..ProfileDocument::default()
                },
            }),
        }),
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "Reply with exactly: real llm agent ok".to_owned(),
                }],
            },
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

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent openai live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_builtin_tool_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    model: engine::ModelSelection,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(FeaturesConfig {
                timers: Some(TimersFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                }),
                ..FeaturesConfig::default()
            }),
            ..SessionConfig::default()
        }),
        profile: Some(ProfileSource::Inline {
            profile: Box::new(InlineAgentProfile {
                display_name: Some("Built-in tool live test".to_owned()),
                description: None,
                document: ProfileDocument {
                    instructions: Some(ProfileInstructions::Text {
                        text: "You are in a live tool integration test. You must use the requested tools before replying, then follow the exact-output instruction.".to_owned(),
                    }),
                    ..ProfileDocument::default()
                },
            }),
        }),
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text { origin: None,
                    text: "Call sleep with delay_ms=1, await the returned promise, then reply exactly: temporal tool ok".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(
        output.to_lowercase().contains("temporal tool ok"),
        "expected completion marker: {output}"
    );
    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: Some(0),
        })
        .await?
        .result
        .events;
    let content = events
        .iter()
        .find_map(|event| match &event.kind {
            api::SessionEventKindView::RunCompleted { run_id, output } if run_id == &run.id => {
                output.clone()
            }
            _ => None,
        })
        .expect("completed run output descriptor");
    if model.api_kind == engine::ProviderApiKind::OpenAiCompletions {
        assert_eq!(content.media_type.as_deref(), Some("application/json"));
        assert_eq!(
            content.provider_kind.as_deref(),
            Some("openai.completions.message")
        );
    }
    assert_eq!(run.output.as_ref(), Some(&content));
    assert_eq!(run.output_text.as_deref(), Some(output));
    assert!(
        run.entries.iter().any(|entry| matches!(
            &entry.kind,
            ContextEntryKindView::ToolCall { name, .. } if name == tools::concurrency::SLEEP_TOOL_NAME
        )),
        "expected sleep tool call in run entries: {:?}",
        run.entries
    );
    for (name, id) in [
        ("sleep", "concurrency.sleep"),
        ("await", "concurrency.await"),
    ] {
        let call = run
            .tool_batches
            .iter()
            .flat_map(|batch| &batch.calls)
            .find(|call| call.tool_name == name)
            .unwrap_or_else(|| panic!("expected {name} call"));
        assert_eq!(call.tool_id.as_deref(), Some(id));
        assert_eq!(call.status, api::ToolItemStatus::Succeeded);
    }
    assert!(
        run.entries.iter().any(|entry| matches!(
            &entry.kind,
            ContextEntryKindView::ToolResult {
                is_error: false,
                ..
            }
        )),
        "expected successful tool result"
    );

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent built-in tool live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_session_metadata_is_stamped_filtered_and_replaced() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_session_metadata_live_client).await
}

async fn run_session_metadata_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    use std::collections::BTreeMap;

    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client, store)
        .with_task_queue(task_queue)
        .with_default_model(model)
        .build();
    let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());
    // The job value is unique per run so the filter isolates this session
    // in the shared live universe.
    let job = BTreeMap::from([
        pair("source", "harbor"),
        pair("job", &format!("job-{}", session_id.as_str())),
    ]);

    let started = api
        .start_session(SessionStartParams {
            session_id: Some(session_id.as_str().to_owned()),
            display_name: Some("Metadata live test".to_owned()),
            metadata: job.clone(),
            config: None,
            profile: None,
            environment: None,
            delete_after_close_ms: None,
        })
        .await?;
    assert_eq!(started.result.session.id, session_id.as_str());

    // Containment: the full map finds it, an extra pair does not.
    let listed = api
        .list_sessions(SessionListParams {
            metadata: job.clone(),
            ..Default::default()
        })
        .await?;
    assert_eq!(
        listed
            .result
            .sessions
            .iter()
            .map(|summary| summary.id.as_str())
            .collect::<Vec<_>>(),
        vec![session_id.as_str()]
    );
    assert_eq!(listed.result.sessions[0].metadata, job);
    let mut extra = job.clone();
    extra.insert("trial".to_owned(), "9".to_owned());
    let listed = api
        .list_sessions(SessionListParams {
            metadata: extra,
            ..Default::default()
        })
        .await?;
    assert!(listed.result.sessions.is_empty());
    let read = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?;
    assert_eq!(read.result.session.metadata, job);

    // Put replaces the whole map without touching updatedAtMs.
    let before = read.result.session.updated_at_ms;
    let replaced = api
        .put_session_metadata(api::SessionMetadataPutParams {
            session_id: session_id.as_str().to_owned(),
            metadata: BTreeMap::from([pair("owner", "live")]),
        })
        .await?;
    assert_eq!(
        replaced.result.session.metadata,
        BTreeMap::from([pair("owner", "live")])
    );
    assert_eq!(replaced.result.session.updated_at_ms, before);
    let listed = api
        .list_sessions(SessionListParams {
            metadata: job.clone(),
            ..Default::default()
        })
        .await?;
    assert!(listed.result.sessions.is_empty());

    // The registration bounds apply at start and at put.
    let reserved = api
        .start_session(SessionStartParams {
            session_id: Some(format!("{}-reserved", session_id.as_str())),
            display_name: None,
            metadata: BTreeMap::from([pair("lightspeed.owner", "x")]),
            config: None,
            profile: None,
            environment: None,
            delete_after_close_ms: None,
        })
        .await
        .expect_err("reserved prefix is rejected at start");
    assert_eq!(reserved.kind, AgentApiErrorKind::InvalidRequest);
    let oversized = api
        .put_session_metadata(api::SessionMetadataPutParams {
            session_id: session_id.as_str().to_owned(),
            metadata: BTreeMap::from([pair("k", &"v".repeat(257))]),
        })
        .await
        .expect_err("oversized value is rejected at put");
    assert_eq!(oversized.kind, AgentApiErrorKind::InvalidRequest);

    api.close_session(api::SessionCloseParams {
        session_id: session_id.as_str().to_owned(),
        force: true,
    })
    .await?;
    Ok(())
}
