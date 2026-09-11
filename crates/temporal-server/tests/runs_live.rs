//! Live coverage for run execution, retries, tool batches, cancellation,
//! steering, queueing, and long drive sequences.

mod support;

use std::time::Duration;

use api::{
    AgentApiErrorKind, AgentApiService, ContextEntryKindView, ContextMessageRoleView, InputItem,
    RunCancelParams, RunStartParams, RunStartSource, RunSteerParams, SessionConfig,
    SessionEventsReadParams, SessionStartParams,
};
use api_projection::model_to_api;
use engine::{ModelSelection, SessionId};
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities_for_run_control,
    fake_worker_activities_with_parallel_tool_calls, fake_worker_activities_with_tool_rounds,
    fake_worker_activities_with_transient_llm_failures, final_assistant_text, live_workflow_handle,
    read_run, require_storage_live_env, run_with_live_worker, start_text_run,
    terminate_live_session, wait_for_terminal_run, wait_until,
};
use temporal_server::{default_model_from_env, gateway::GatewayAgentApi, pg_store_from_env};
use temporal_workflow::LLM_RETRY_MAX_ATTEMPTS;
use temporalio_client::{Client, WorkflowDescribeOptions, WorkflowTerminateOptions};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_hosted_run_exceeds_128_drive_transitions() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Thirty model/tool rounds reproduce the incident's transition count
    // shape while remaining well below the default history rollover threshold.
    let activities = fake_worker_activities_with_tool_rounds(30).await?;
    run_with_live_worker(activities, run_unbounded_hosted_run_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_parallel_tool_batch_completes_per_call() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Three parallel-safe calls in one batch exercise the per-call activity
    // path: concurrent scheduling, completion-order resumes, and progressive
    // engine appends. One call fails terminally while its siblings succeed,
    // proving per-call independence. The terminal-run polling issues status
    // queries while the batch is in flight, which regresses the
    // workflow-waker class of bug (TMPRL1100) that batch-level activities
    // never hit.
    let activities = fake_worker_activities_with_parallel_tool_calls(3, Some(1)).await?;
    run_with_live_worker(activities, run_parallel_tool_batch_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_transient_llm_failures_retry_within_the_turn() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Two scripted transient provider failures precede a normal generation
    // so the typed retryable activity failure makes Temporal retry the
    // pending `llm_generate` activity with durable backoff, honoring the
    // scripted suggested delay. The run must complete with one successful
    // generation and no transcript trace of the retried attempts.
    let activities = fake_worker_activities_with_transient_llm_failures(2).await?;
    run_with_live_worker(activities, run_transient_llm_retry_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_exhausted_llm_retries_fail_the_run_not_the_session() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Exactly the bounded attempt budget of transient failures: the
    // first run exhausts `llm_generate`'s retry policy and fails at the
    // workflow boundary. The scripted budget is then consumed, so a second
    // run on the same session succeeds — proving exhaustion fails the run
    // while the session workflow survives for later runs.
    let attempts = usize::try_from(LLM_RETRY_MAX_ATTEMPTS).expect("attempt budget fits usize");
    let activities = fake_worker_activities_with_transient_llm_failures(attempts).await?;
    run_with_live_worker(activities, run_llm_retry_exhaustion_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_cancel_mid_generation_aborts_the_provider_call() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Cancel mid-generation: the fake provider sleeps inside generate. Cancelling the run
    // while that call is in flight must cancel the turn in the engine at
    // once (run reaches `cancelled`, no grace turn, no `failed`), abandon
    // the worker-side provider call through activity cancellation, and
    // leave the session serving a later run.
    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::from_secs(15), Duration::ZERO, 1).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_cancel_mid_generation_live_client(client, task_queue, session_id, counters)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_cancel_during_tool_batch_records_cancelled_calls() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Cancel during a tool batch: the fake tool sleeps inside the call. Cancelling the
    // run while the batch executes records the call as cancelled with the
    // well-known content, drains the run to `cancelled`, and abandons the
    // worker-side tool execution.
    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::ZERO, Duration::from_secs(15), 1).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_cancel_during_tool_batch_live_client(client, task_queue, session_id, counters)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_steering_lands_at_next_turn_boundary() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Steering: steer while the first (slow) generation is in flight. The
    // in-flight turn finishes untouched (its tool call runs), and the next
    // generation request carries the steering entry — the fake model echoes
    // it in the final answer. Steering a finished run is rejected.
    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::from_secs(4), Duration::ZERO, 1).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_steering_live_client(client, task_queue, session_id, counters)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_steering_during_final_turn_adds_a_turn() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Steering while a single-turn run is generating its final answer: the
    // steering is materialized while the call is in flight, and instead of
    // completing on that final output the run gets one more turn whose
    // request carries the steering — so "steer" never silently does
    // nothing.
    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::from_secs(4), Duration::ZERO, 1).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_steering_final_turn_live_client(client, task_queue, session_id, counters)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_queued_runs_return_promptly_and_run_in_order() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Queueing: runs started behind an active run return `queued` at
    // once, are visible in the session view, can be cancelled while queued,
    // and start in order after the active run ends. Cancelling the active
    // run leaves the queued one untouched and it runs next.
    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::from_secs(5), Duration::ZERO, 1).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_queue_live_client(client, task_queue, session_id, counters)
    })
    .await
}

fn run_control_session_config(model: &ModelSelection) -> SessionConfig {
    // The VFS read-only tool surface gives the fake model a function tool to
    // call, so runs have a tool-call turn followed by a final turn.
    SessionConfig {
        model: Some(model_to_api(model)),
        features: Some(api::FeaturesConfig {
            vfs: Some(api::VfsFeature {
                working_directory: None,
                version: api::CURRENT_FEATURE_VERSION,
                workspace_links: Vec::new(),
                tools: Some(api::VfsToolSurface::ReadOnly),
                prompts: None,
                skills: None,
            }),
            ..api::FeaturesConfig::default()
        }),
        ..SessionConfig::default()
    }
}

async fn run_control_api(
    client: &Client,
    task_queue: String,
    session_id: &SessionId,
    with_tools: bool,
) -> anyhow::Result<GatewayAgentApi> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();
    let config = if with_tools {
        run_control_session_config(&model)
    } else {
        SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }
    };
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(config),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;
    Ok(api)
}

async fn run_cancel_mid_generation_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    counters: temporal_server::worker::FakeRuntimeCounters,
) -> anyhow::Result<()> {
    let api = run_control_api(&client, task_queue, &session_id, false).await?;

    let run = start_text_run(&api, &session_id, "take your time").await?;
    assert_eq!(run.status, api::RunStatus::Running);
    wait_until(
        "the provider call to start",
        Duration::from_secs(10),
        async || Ok(counters.generations_started() >= 1),
    )
    .await?;

    let cancel_started = std::time::Instant::now();
    let cancelled = api
        .cancel_run(RunCancelParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run.id.clone(),
        })
        .await?
        .result
        .run;
    assert!(
        matches!(
            cancelled.status,
            api::RunStatus::Cancelling | api::RunStatus::Cancelled
        ),
        "cancel must be acknowledged while the provider call is in flight, got {:?}",
        cancelled.status
    );
    assert!(
        cancel_started.elapsed() < Duration::from_secs(10),
        "cancel must not wait for the in-flight generation ({:?})",
        cancel_started.elapsed()
    );

    let terminal = wait_for_terminal_run(&api, &session_id, &run.id).await?;
    assert_eq!(terminal.status, api::RunStatus::Cancelled);
    assert!(
        cancel_started.elapsed() < Duration::from_secs(12),
        "the run must reach cancelled before the abandoned generation would have finished"
    );

    // The worker abandons the in-flight provider call once the activity
    // cancellation reaches it through the heartbeat; no second generation
    // (no grace turn) is ever requested for the cancelled run.
    wait_until(
        "the provider call to be abandoned",
        Duration::from_secs(20),
        async || Ok(counters.generations_abandoned() >= 1),
    )
    .await?;
    assert_eq!(counters.generations_started(), 1);
    assert_eq!(counters.generations_completed(), 0);

    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: None,
        })
        .await?
        .result
        .events;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, api::SessionEventKindView::TurnCancelled { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, api::SessionEventKindView::RunFailed { .. }))
    );

    // The session keeps serving runs after a cancel.
    let next = start_text_run(&api, &session_id, "and again").await?;
    let next = wait_for_terminal_run_with_timeout(&api, &session_id, &next.id, 40).await?;
    assert_eq!(next.status, api::RunStatus::Completed);
    assert!(
        final_assistant_text(&next).is_some_and(|text| text.contains("Fake agent completed run"))
    );

    terminate_live_session(
        &client,
        &session_id,
        "cancel mid-generation live test cleanup",
    )
    .await;
    Ok(())
}

async fn run_cancel_during_tool_batch_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    counters: temporal_server::worker::FakeRuntimeCounters,
) -> anyhow::Result<()> {
    let api = run_control_api(&client, task_queue, &session_id, true).await?;

    let run = start_text_run(&api, &session_id, "call a slow tool").await?;
    wait_until(
        "the tool call to start",
        Duration::from_secs(15),
        async || Ok(counters.tool_calls_started() >= 1),
    )
    .await?;

    let cancel_started = std::time::Instant::now();
    let cancelled = api
        .cancel_run(RunCancelParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run.id.clone(),
        })
        .await?
        .result
        .run;
    assert!(matches!(
        cancelled.status,
        api::RunStatus::Cancelling | api::RunStatus::Cancelled
    ));
    let terminal = wait_for_terminal_run(&api, &session_id, &run.id).await?;
    assert_eq!(terminal.status, api::RunStatus::Cancelled);
    assert!(
        cancel_started.elapsed() < Duration::from_secs(12),
        "the run must reach cancelled before the abandoned tool call would have finished"
    );

    // The cancelled call has a model-visible result so the conversation
    // stays well-formed for the next run.
    let cancelled_results = terminal
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                ContextEntryKindView::ToolResult { is_error: true, .. }
            ) && entry
                .text
                .as_deref()
                .is_some_and(|text| text.contains("tool call cancelled"))
        })
        .count();
    assert_eq!(cancelled_results, 1, "run entries: {:?}", terminal.entries);
    assert!(
        terminal.tool_batches.iter().any(|batch| {
            batch
                .calls
                .iter()
                .any(|call| call.status == api::ToolItemStatus::Cancelled)
        }),
        "tool batches: {:?}",
        terminal.tool_batches
    );

    wait_until(
        "the tool call to be abandoned",
        Duration::from_secs(20),
        async || Ok(counters.tool_calls_abandoned() >= 1),
    )
    .await?;
    // Exactly one generation (the tool-call turn); no second turn ran.
    assert_eq!(counters.generations_started(), 1);

    let next = start_text_run(&api, &session_id, "now answer normally").await?;
    let next = wait_for_terminal_run_with_timeout(&api, &session_id, &next.id, 40).await?;
    assert_eq!(next.status, api::RunStatus::Completed);

    terminate_live_session(
        &client,
        &session_id,
        "cancel during tool batch live test cleanup",
    )
    .await;
    Ok(())
}

async fn run_steering_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    counters: temporal_server::worker::FakeRuntimeCounters,
) -> anyhow::Result<()> {
    let api = run_control_api(&client, task_queue, &session_id, true).await?;

    let run = start_text_run(&api, &session_id, "do the task").await?;
    wait_until(
        "the first generation to start",
        Duration::from_secs(10),
        async || Ok(counters.generations_started() >= 1),
    )
    .await?;

    let steered = api
        .steer_run(RunSteerParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run.id.clone(),
            items: vec![InputItem::Text {
                origin: None,
                text: "also mention the moon".to_owned(),
            }],
        })
        .await?
        .result;
    assert_eq!(steered.steering_id, "steering_1");
    assert_eq!(steered.run.status, api::RunStatus::Running);
    // Admitted while the first generation was still in flight.
    assert_eq!(counters.generations_completed(), 0);

    let terminal = wait_for_terminal_run_with_timeout(&api, &session_id, &run.id, 40).await?;
    assert_eq!(terminal.status, api::RunStatus::Completed);
    let text = final_assistant_text(&terminal).expect("final answer");
    assert!(
        text.contains("Steering received: also mention the moon"),
        "final answer must reflect the steering delivered at the next turn: {text}"
    );
    // The in-flight turn finished untouched: its tool call ran, then the
    // final turn saw the steering. Two generations, one tool call.
    assert_eq!(counters.generations_started(), 2);
    assert_eq!(counters.generations_abandoned(), 0);
    assert_eq!(counters.tool_calls_completed(), 1);
    assert!(terminal.entries.iter().any(|entry| matches!(
        entry.source,
        Some(api::ContextEntrySourceView::Steering { .. })
    )));

    let late = api
        .steer_run(RunSteerParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run.id.clone(),
            items: vec![InputItem::Text {
                origin: None,
                text: "too late".to_owned(),
            }],
        })
        .await
        .expect_err("steering a finished run is rejected");
    assert_eq!(late.kind, AgentApiErrorKind::Rejected);

    terminate_live_session(&client, &session_id, "steering live test cleanup").await;
    Ok(())
}

async fn run_steering_final_turn_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    counters: temporal_server::worker::FakeRuntimeCounters,
) -> anyhow::Result<()> {
    // No tools: the fake model answers in one turn.
    let api = run_control_api(&client, task_queue, &session_id, false).await?;
    let run = start_text_run(&api, &session_id, "answer directly").await?;
    wait_until(
        "the generation to start",
        Duration::from_secs(10),
        async || Ok(counters.generations_started() >= 1),
    )
    .await?;
    let steered = api
        .steer_run(RunSteerParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run.id.clone(),
            items: vec![InputItem::Text {
                origin: None,
                text: "one more thing".to_owned(),
            }],
        })
        .await?
        .result;
    assert_eq!(steered.run.status, api::RunStatus::Running);

    let terminal = wait_for_terminal_run_with_timeout(&api, &session_id, &run.id, 40).await?;
    assert_eq!(terminal.status, api::RunStatus::Completed);
    assert_eq!(
        counters.generations_started(),
        2,
        "the run must take one more turn for the unconsumed steering"
    );
    let text = final_assistant_text(&terminal).expect("final answer");
    assert!(
        text.contains("Steering received: one more thing"),
        "the extra turn must carry the steering: {text}"
    );
    // The steering materializes after the in-flight turn: it sits between
    // the first answer and the extra turn's answer.
    let kinds = terminal
        .entries
        .iter()
        .map(|entry| match (&entry.kind, entry.source.as_ref()) {
            (
                ContextEntryKindView::Message {
                    role: ContextMessageRoleView::User,
                },
                Some(api::ContextEntrySourceView::Steering { .. }),
            ) => "steer",
            (
                ContextEntryKindView::Message {
                    role: ContextMessageRoleView::User,
                },
                _,
            ) => "user",
            (
                ContextEntryKindView::Message {
                    role: ContextMessageRoleView::Assistant,
                },
                _,
            ) => "assistant",
            _ => "other",
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "steer", "assistant"],
        "entries: {kinds:?}"
    );

    terminate_live_session(
        &client,
        &session_id,
        "steering final turn live test cleanup",
    )
    .await;
    Ok(())
}

async fn run_queue_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    counters: temporal_server::worker::FakeRuntimeCounters,
) -> anyhow::Result<()> {
    let api = run_control_api(&client, task_queue, &session_id, false).await?;

    let first = start_text_run(&api, &session_id, "first").await?;
    assert_eq!(first.status, api::RunStatus::Running);

    let queued_at = std::time::Instant::now();
    let second = start_text_run(&api, &session_id, "second").await?;
    assert_eq!(second.status, api::RunStatus::Queued);
    assert!(
        queued_at.elapsed() < Duration::from_secs(3),
        "a queued run must be acknowledged without waiting for the active run ({:?})",
        queued_at.elapsed()
    );
    let third = start_text_run(&api, &session_id, "third").await?;
    assert_eq!(third.status, api::RunStatus::Queued);

    // Queued runs are part of the session view with their input.
    let queued_view = read_run(&api, &session_id, &second.id)
        .await?
        .expect("queued run in session view");
    assert_eq!(queued_view.status, api::RunStatus::Queued);
    assert!(matches!(
        &queued_view.source,
        api::RunViewSource::Input { items }
            if matches!(items.first(), Some(InputItem::Text { text, .. }) if text == "second")
    ));

    // Steering a queued run is rejected; it has no turn yet.
    let steer_queued = api
        .steer_run(RunSteerParams {
            session_id: session_id.as_str().to_owned(),
            run_id: second.id.clone(),
            items: vec![InputItem::Text {
                origin: None,
                text: "nope".to_owned(),
            }],
        })
        .await
        .expect_err("steering a queued run is rejected");
    assert_eq!(steer_queued.kind, AgentApiErrorKind::Rejected);

    // Cancel a queued run: dequeued as cancelled, nothing else changes.
    let cancelled_second = api
        .cancel_run(RunCancelParams {
            session_id: session_id.as_str().to_owned(),
            run_id: second.id.clone(),
        })
        .await?
        .result
        .run;
    assert_eq!(cancelled_second.status, api::RunStatus::Cancelled);
    assert!(
        read_run(&api, &session_id, &first.id)
            .await?
            .is_some_and(|run| run.status == api::RunStatus::Running)
    );

    // Cancel the active run while another is queued: only the active run is
    // cancelled, the queued one starts next and completes.
    let cancelled_first = api
        .cancel_run(RunCancelParams {
            session_id: session_id.as_str().to_owned(),
            run_id: first.id.clone(),
        })
        .await?
        .result
        .run;
    assert!(matches!(
        cancelled_first.status,
        api::RunStatus::Cancelling | api::RunStatus::Cancelled
    ));
    let first_terminal = wait_for_terminal_run(&api, &session_id, &first.id).await?;
    assert_eq!(first_terminal.status, api::RunStatus::Cancelled);
    let third_terminal =
        wait_for_terminal_run_with_timeout(&api, &session_id, &third.id, 40).await?;
    assert_eq!(third_terminal.status, api::RunStatus::Completed);
    assert!(
        final_assistant_text(&third_terminal).is_some_and(|text| text.contains(&format!(
            "completed run {}",
            third.id.trim_start_matches("run_")
        )))
    );

    // Another queued run on an idle session starts immediately.
    let fourth = start_text_run(&api, &session_id, "fourth").await?;
    assert_eq!(fourth.status, api::RunStatus::Running);
    let fourth_terminal =
        wait_for_terminal_run_with_timeout(&api, &session_id, &fourth.id, 40).await?;
    assert_eq!(fourth_terminal.status, api::RunStatus::Completed);

    // Exactly one generation per executed run (first, third, fourth); the
    // cancelled first run never got a second turn. Whether the worker
    // abandoned the first generation or it completed (and was discarded)
    // depends on heartbeat timing with this short delay and is covered by
    // the mid-generation cancel scenario.
    assert_eq!(counters.generations_started(), 3);
    assert_eq!(
        counters.generations_completed() + counters.generations_abandoned(),
        3
    );

    terminate_live_session(&client, &session_id, "queue live test cleanup").await;
    Ok(())
}

/// `wait_for_terminal_run` with a longer budget, for scenarios whose fake
/// provider sleeps on purpose.
async fn wait_for_terminal_run_with_timeout(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
    timeout_secs: u64,
) -> anyhow::Result<api::RunView> {
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(timeout_secs) {
            anyhow::bail!("timed out waiting for run {run_id} to finish");
        }
        if let Some(run) = read_run(api, session_id, run_id).await?
            && matches!(
                run.status,
                api::RunStatus::Completed | api::RunStatus::Failed | api::RunStatus::Cancelled
            )
        {
            return Ok(run);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_parallel_tool_batch_live_client(
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

    // The VFS tool surface derives parallel-safe function tools (vfs reads),
    // so the fake model's three calls form one concurrent per-call group.
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(api::FeaturesConfig {
                vfs: Some(api::VfsFeature {
                    working_directory: None,
                    version: api::CURRENT_FEATURE_VERSION,
                    workspace_links: Vec::new(),
                    tools: Some(api::VfsToolSurface::ReadOnly),
                    prompts: None,
                    skills: None,
                }),
                ..api::FeaturesConfig::default()
            }),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let started = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "run a parallel tool batch".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert_eq!(run.status, api::RunStatus::Completed);
    assert!(
        final_assistant_text(&run).is_some_and(|text| text.contains("Fake agent completed run"))
    );

    let mut results = run
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            ContextEntryKindView::ToolResult { call_id, is_error } => {
                Some((call_id.clone(), *is_error))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    results.sort();
    assert_eq!(
        results,
        vec![
            ("agent_call_1_0".to_owned(), false),
            ("agent_call_1_1".to_owned(), true),
            ("agent_call_1_2".to_owned(), false),
        ],
        "every call of the parallel batch must reach its own terminal result: \
         the scripted failure fails alone and its siblings succeed"
    );

    // Completion events carry the executing runtime's output accounting:
    // the succeeded calls report the bytes they produced, uncut by the
    // projection budget; the scripted failure has no output to account for.
    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: None,
        })
        .await?
        .result
        .events;
    let mut completions = events
        .iter()
        .filter_map(|event| match &event.kind {
            api::SessionEventKindView::ToolCallCompleted {
                call_id,
                status,
                output_bytes,
                truncated,
                ..
            } => Some((call_id.clone(), *status, *output_bytes, *truncated)),
            _ => None,
        })
        .collect::<Vec<_>>();
    completions.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(completions.len(), 3, "{completions:?}");
    for (call_id, status, output_bytes, truncated) in &completions {
        assert!(!truncated, "{call_id} was not cut by the projection budget");
        match status {
            api::ToolItemStatus::Succeeded => assert!(
                output_bytes.is_some_and(|bytes| bytes > 0),
                "{call_id} reports the bytes it produced: {output_bytes:?}"
            ),
            api::ToolItemStatus::Failed => {
                assert_eq!(*output_bytes, None, "{call_id} produced no output")
            }
            other => panic!("{call_id} ended with {other:?}"),
        }
    }

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("parallel tool batch live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_transient_llm_retry_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model)
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: None,
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let started = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "retry through transient provider failures".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert_eq!(
        run.status,
        api::RunStatus::Completed,
        "transient provider failures within the retry budget must not fail the run"
    );
    assert!(
        final_assistant_text(&run).is_some_and(|text| text.contains("Fake agent completed run"))
    );
    // Intermediate transient attempts are runtime facts, not session events:
    // the transcript holds exactly one successful assistant generation.
    let assistant_messages = run
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                ContextEntryKindView::Message {
                    role: ContextMessageRoleView::Assistant
                }
            )
        })
        .count();
    assert_eq!(assistant_messages, 1);
    assert!(
        run.entries.iter().all(|entry| !entry
            .text
            .as_deref()
            .unwrap_or_default()
            .contains("scripted transient provider failure")),
        "transient attempts must not leak into model context"
    );

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("transient llm retry live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_llm_retry_exhaustion_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model)
        .build();

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: None,
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    let first = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "exhaust the provider retry budget".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let first_run = wait_for_terminal_run(&api, &session_id, &first.result.run.id).await?;
    assert_eq!(
        first_run.status,
        api::RunStatus::Failed,
        "exhausted provider retries must fail the run with a terminal generation result"
    );
    // The failure event carries the engine's classification, not only text.
    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: None,
        })
        .await?
        .result
        .events;
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            api::SessionEventKindView::RunFailed { run_id, kind, .. }
                if run_id.as_str() == first_run.id.as_str()
                    && *kind == api::RunFailureKindView::ModelFailure
        )),
        "runFailed names the model failure kind: {:?}",
        events
            .iter()
            .filter(|event| matches!(event.kind, api::SessionEventKindView::RunFailed { .. }))
            .collect::<Vec<_>>()
    );

    // The session workflow survived exhaustion, and the scripted transient
    // budget is consumed, so the next run on the same session succeeds.
    let second = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "recover after the provider outage".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let second_run = wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    assert_eq!(
        second_run.status,
        api::RunStatus::Completed,
        "a run after provider recovery must succeed on the surviving session workflow"
    );
    assert!(
        final_assistant_text(&second_run)
            .is_some_and(|text| text.contains("Fake agent completed run"))
    );

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("llm retry exhaustion live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_unbounded_hosted_run_live_client(
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
            features: Some(api::FeaturesConfig {
                vfs: Some(api::VfsFeature {
                    working_directory: None,
                    version: api::CURRENT_FEATURE_VERSION,
                    workspace_links: Vec::new(),
                    tools: None,
                    prompts: None,
                    skills: None,
                }),
                ..api::FeaturesConfig::default()
            }),
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

    let started = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "complete thirty verification tool rounds".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert!(
        final_assistant_text(&run).is_some_and(|text| text.contains("Fake agent completed run"))
    );
    let final_temporal_run_id = live_workflow_handle(&client, &session_id)?
        .describe(WorkflowDescribeOptions::default())
        .await?
        .run_id()
        .to_owned();
    assert_eq!(
        final_temporal_run_id, initial_temporal_run_id,
        "step count alone must not continue as new"
    );

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("unbounded hosted run live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}
