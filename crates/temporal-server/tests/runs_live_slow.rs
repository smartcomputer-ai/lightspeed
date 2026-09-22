//! Live tests that deliberately wait out production activity budgets. They
//! share the Temporal/PostgreSQL stack with the ordinary live suites and are kept
//! in their own binary so the ordinary live suite stays fast:
//!
//! ```bash
//! source scripts/dev/env.sh
//! cargo test -p temporal-server --test runs_live_slow -- --ignored --test-threads=1
//! ```

mod support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use api::{
    AgentApiService, InputItem, RunStartParams, RunStartSource, RunStatus, SessionReadParams,
    SessionStartParams,
};
use engine::SessionId;
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities_with_stall_switch, final_assistant_text,
    live_workflow_handle, require_storage_live_env, run_with_live_worker, wait_for_terminal_run,
};
use temporal_server::{
    default_model_from_env, gateway::GatewayAgentApi, pg_store_from_env,
    worker::FakeRuntimeCounters,
};
use temporal_workflow::{LLM_SCHEDULE_TO_CLOSE, LLM_START_TO_CLOSE};
use temporalio_client::{Client, WorkflowDescribeOptions, WorkflowTerminateOptions};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra; slow: waits out the full LLM schedule-to-close budget (~21 minutes)"]
async fn temporal_live_llm_activity_timeout_fails_the_run_not_the_session() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // The fake provider hangs inside generate (heartbeating normally), so
    // every `llm_generate` attempt ends in a start-to-close timeout and the
    // schedule-to-close budget ends the retries with a pure timeout chain —
    // no `llm_provider_transient` failure anywhere in it. That is the shape
    // a worker outage produces. The run must fail with a terminal generation
    // result while the session workflow survives for the next run.
    let stalled = Arc::new(AtomicBool::new(true));
    let (activities, counters) = fake_worker_activities_with_stall_switch(stalled.clone()).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_llm_timeout_live_client(client, task_queue, session_id, stalled, counters)
    })
    .await
}

async fn run_llm_timeout_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    stalled: Arc<AtomicBool>,
    counters: FakeRuntimeCounters,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(default_model_from_env())
        .build();

    api.start_session(SessionStartParams {
        access: None,
        execution: None,
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: None,
        profile: None,
        delete_after_close_ms: None,
    })
    .await?;
    let handle = live_workflow_handle(&client, &session_id)?;
    let initial_temporal_run_id = handle
        .describe(WorkflowDescribeOptions::default())
        .await?
        .run_id()
        .to_owned();

    let first = start_text_run(&api, &session_id, "hang until the activity budget expires").await?;
    let started = Instant::now();
    let budget = LLM_SCHEDULE_TO_CLOSE + LLM_START_TO_CLOSE;
    let first_run = loop {
        if started.elapsed() > budget {
            anyhow::bail!(
                "run {} did not finish within {budget:?} after the provider activity budget",
                first
            );
        }
        let session = api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            })
            .await?;
        if let Some(run) = session
            .result
            .session
            .runs
            .into_iter()
            .find(|run| run.id == first)
            && matches!(
                run.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
            )
        {
            break run;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    assert_eq!(
        first_run.status,
        RunStatus::Failed,
        "a provider activity that times out must fail the run with a terminal generation result"
    );
    assert!(
        started.elapsed() >= LLM_SCHEDULE_TO_CLOSE - LLM_START_TO_CLOSE,
        "the run failed after {:?}, before the activity budget could have expired",
        started.elapsed()
    );
    assert!(
        counters.generations_started() >= 2,
        "Temporal must have retried the timed-out attempt (started {})",
        counters.generations_started()
    );
    assert_eq!(counters.generations_completed(), 0);

    // The session workflow is the same execution — it neither failed nor was
    // it recreated — and it serves the next run once the provider recovers.
    let description = handle.describe(WorkflowDescribeOptions::default()).await?;
    assert_eq!(
        description.run_id(),
        initial_temporal_run_id,
        "the session workflow must be the original execution"
    );
    stalled.store(false, Ordering::SeqCst);
    let second = start_text_run(&api, &session_id, "recover after the outage").await?;
    let second_run = wait_for_terminal_run(&api, &session_id, &second).await?;
    assert_eq!(
        second_run.status,
        RunStatus::Completed,
        "a run after the outage must succeed on the surviving session workflow"
    );
    assert!(
        final_assistant_text(&second_run)
            .is_some_and(|text| text.contains("Fake agent completed run"))
    );

    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("llm activity timeout live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn start_text_run(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    text: &str,
) -> anyhow::Result<String> {
    Ok(api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: text.to_owned(),
                }],
            },
            config: None,
        })
        .await?
        .result
        .run
        .id)
}
