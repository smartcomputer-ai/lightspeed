#![allow(dead_code)]

use std::{
    env,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use api::{
    AgentApiService, ContextEntryKindView, ContextMessageRoleView, InputItem, RunStartParams,
    RunStartSource, RunStatus, SessionReadParams, SessionStatus,
};
use engine::{
    CoreAgentLlm, CoreAgentTools, ModelSelection, ProviderApiKind, SessionId, storage::BlobStore,
};
use temporal_server::{
    pg_store_from_env,
    worker::{
        ActivityState, AudioTranscoder, AudioTranscriber, FakeLlm, FakeRuntimeCounters, FakeTools,
        WorkerActivities, core_runtime, worker_with_activities,
    },
};
use temporal_workflow::{
    AgentAdmissionFailureKind, AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::{Client, WorkflowQueryOptions, WorkflowTerminateOptions};

pub static LIVE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Explicit test identity for direct in-process clients; never inherited by workers.
pub async fn local_request_context() -> anyhow::Result<access::RequestContext> {
    let store = pg_store_from_env().await?;
    let universe_id = store.config().universe_id;
    store.ensure_universe().await?;
    let access = store_pg::PgAccessStore::new(store.pool().clone());
    let principal = access.initialize_local_development(universe_id, 1).await?;
    Ok(temporal_server::gateway::authentication::local_context(
        &access,
        principal.id,
        access::AccessScope::Universe { universe_id },
    )
    .await?)
}

pub async fn run_with_live_worker<F, Fut>(
    activities: WorkerActivities,
    run_client: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Client, String, SessionId) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    run_with_live_worker_builder(move |_, _| async move { Ok(activities) }, run_client).await
}

pub async fn run_with_live_worker_builder<B, BuildFut, F, Fut>(
    build_activities: B,
    run_client: F,
) -> anyhow::Result<()>
where
    B: FnOnce(Client, String) -> BuildFut,
    BuildFut: Future<Output = anyhow::Result<WorkerActivities>>,
    F: FnOnce(Client, String, SessionId) -> Fut,
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
    let activities = build_activities(client.clone(), task_queue.clone()).await?;
    let mut worker =
        worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = worker.run();
    tokio::pin!(worker_future);

    let client_future = temporal_server::gateway::principal::with_request_context(
        local_request_context().await?,
        run_client(client, task_queue, session_id),
    );
    tokio::pin!(client_future);

    let client_result = tokio::select! {
        worker_result = worker_future.as_mut() => {
            return match worker_result {
                Ok(()) => Err(anyhow::anyhow!("Temporal worker stopped before the live test completed")),
                Err(error) => Err(error.context("Temporal worker failed")),
            };
        }
        client_result = client_future.as_mut() => client_result,
    };

    shutdown_worker();
    tokio::time::timeout(Duration::from_secs(10), worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 10 seconds"))??;
    client_result
}

/// Universe used by live tests: the one bound by `LIGHTSPEED_PG_UNIVERSE_ID`.
pub fn live_universe_id() -> anyhow::Result<uuid::Uuid> {
    temporal_server::universe_id_from_env()
}

/// Temporal workflow handle for a session, addressed by the composed
/// `{universe_id}/{session_id}` workflow id.
pub fn live_workflow_handle(
    client: &Client,
    session_id: &SessionId,
) -> anyhow::Result<temporalio_client::WorkflowHandle<Client, AgentSessionWorkflow>> {
    let workflow_id = temporal_workflow::compose_workflow_id(live_universe_id()?, session_id);
    Ok(client.get_workflow_handle::<AgentSessionWorkflow>(workflow_id))
}

pub async fn fake_worker_activities() -> anyhow::Result<WorkerActivities> {
    Ok(WorkerActivities::for_universe(
        live_universe_id()?,
        fake_activity_state().await?,
    ))
}

pub async fn fake_worker_activities_with_tool_rounds(
    tool_rounds_before_final: usize,
) -> anyhow::Result<WorkerActivities> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(FakeLlm::new(blobs.clone()).with_tool_rounds(tool_rounds_before_final))
        as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok(WorkerActivities::for_universe(
        live_universe_id()?,
        ActivityState::from_pg_store(store, llm, tools),
    ))
}

pub async fn fake_worker_activities_with_parallel_tool_calls(
    parallel_tool_calls: usize,
    failing_call: Option<usize>,
) -> anyhow::Result<WorkerActivities> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let mut llm = FakeLlm::new(blobs.clone()).with_parallel_tool_calls(parallel_tool_calls);
    if let Some(index) = failing_call {
        llm = llm.with_failing_parallel_call(index);
    }
    let llm = Arc::new(llm) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok(WorkerActivities::for_universe(
        live_universe_id()?,
        ActivityState::from_pg_store(store, llm, tools),
    ))
}

/// Worker whose fake LLM fails its next `transient_failures` generate calls
/// with a transient retryable error (tiny suggested delay), then behaves
/// normally. Pass exactly the retry budget to exercise exhaustion followed by
/// recovery on the next run.
pub async fn fake_worker_activities_with_transient_llm_failures(
    transient_failures: usize,
) -> anyhow::Result<WorkerActivities> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(FakeLlm::new(blobs.clone()).with_transient_failures(transient_failures))
        as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok(WorkerActivities::for_universe(
        live_universe_id()?,
        ActivityState::from_pg_store(store, llm, tools),
    ))
}

/// Worker for active-run-control live tests: slow generations and/or
/// slow tool calls so cancel/steer/queue can land mid-run, with shared
/// counters to prove what the runtime actually executed or abandoned.
pub async fn fake_worker_activities_for_run_control(
    generation_delay: Duration,
    tool_call_delay: Duration,
    tool_rounds_before_final: usize,
) -> anyhow::Result<(WorkerActivities, FakeRuntimeCounters)> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let counters = FakeRuntimeCounters::default();
    let llm = Arc::new(
        FakeLlm::new(blobs.clone())
            .with_tool_rounds(tool_rounds_before_final)
            .with_generation_delay(generation_delay)
            .with_counters(counters.clone()),
    ) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(
        FakeTools::new(blobs)
            .with_call_delay(tool_call_delay)
            .with_counters(counters.clone()),
    ) as Arc<dyn CoreAgentTools>;
    Ok((
        WorkerActivities::for_universe(
            live_universe_id()?,
            ActivityState::from_pg_store(store, llm, tools),
        ),
        counters,
    ))
}

/// Worker whose fake LLM hangs every generate call while `stalled` is set,
/// so provider activities run into Temporal's timeouts instead of failing;
/// clear the switch to let later runs succeed.
pub async fn fake_worker_activities_with_stall_switch(
    stalled: Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<(WorkerActivities, FakeRuntimeCounters)> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let counters = FakeRuntimeCounters::default();
    let llm = Arc::new(
        FakeLlm::new(blobs.clone())
            .with_stall_switch(stalled)
            .with_counters(counters.clone()),
    ) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok((
        WorkerActivities::for_universe(
            live_universe_id()?,
            ActivityState::from_pg_store(store, llm, tools),
        ),
        counters,
    ))
}

pub async fn fake_worker_activities_with_audio_transcriber(
    transcriber: Arc<dyn AudioTranscriber>,
) -> anyhow::Result<WorkerActivities> {
    fake_worker_activities_with_audio_preprocessors(transcriber, None).await
}

pub async fn fake_worker_activities_with_audio_preprocessors(
    transcriber: Arc<dyn AudioTranscriber>,
    transcoder: Option<Arc<dyn AudioTranscoder>>,
) -> anyhow::Result<WorkerActivities> {
    let mut state = fake_activity_state()
        .await?
        .with_audio_transcriber(transcriber);
    if let Some(transcoder) = transcoder {
        state = state.with_audio_transcoder(transcoder);
    }
    Ok(WorkerActivities::for_universe(live_universe_id()?, state))
}

pub async fn fake_activity_state() -> anyhow::Result<ActivityState> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(FakeLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok(ActivityState::from_pg_store(store, llm, tools))
}

pub fn final_assistant_text(run: &api::RunView) -> Option<&str> {
    run.entries.iter().rev().find_map(|entry| match entry.kind {
        ContextEntryKindView::Message {
            role: ContextMessageRoleView::Assistant,
        } => entry.text.as_deref(),
        _ => None,
    })
}

pub async fn wait_for_terminal_run(
    api: &temporal_server::gateway::GatewayAgentApi,
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
                run_limit: None,
            })
            .await?;
        if let Some(run) = session
            .result
            .session
            .runs
            .into_iter()
            .find(|run| run.id == run_id)
            && matches!(
                run.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
            )
        {
            return Ok(api
                .read_run(api::RunReadParams {
                    session_id: session_id.as_str().to_owned(),
                    run_id: run.id,
                })
                .await?
                .result
                .run);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn start_text_run(
    api: &temporal_server::gateway::GatewayAgentApi,
    session_id: &SessionId,
    text: &str,
) -> anyhow::Result<api::RunView> {
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
        .run)
}

pub async fn read_run(
    api: &temporal_server::gateway::GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<Option<api::RunView>> {
    let response = api
        .read_run(api::RunReadParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run_id.to_owned(),
        })
        .await?;
    Ok(Some(response.result.run))
}

pub async fn read_session_view(
    api: &temporal_server::gateway::GatewayAgentApi,
    session_id: &SessionId,
) -> anyhow::Result<api::SessionView> {
    Ok(api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?
        .result
        .session)
}

pub async fn wait_for_environment_status(
    api: &temporal_server::gateway::GatewayAgentApi,
    environment_id: &str,
    expected: api::EnvironmentLifecycleStatusView,
) -> anyhow::Result<api::EnvironmentView> {
    let started = Instant::now();
    loop {
        api.reconcile_environments_once().await?;
        let environment = api
            .read_environment(api::EnvironmentReadParams {
                environment_id: environment_id.to_owned(),
            })
            .await?
            .result
            .environment;
        if environment.status == expected {
            return Ok(environment);
        }
        if started.elapsed() > Duration::from_secs(20) {
            anyhow::bail!(
                "timed out waiting for environment {environment_id} to reach {expected:?}; current status is {:?}",
                environment.status
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn wait_until(
    what: &str,
    timeout: Duration,
    mut check: impl AsyncFnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        if check().await? {
            return Ok(());
        }
        if started.elapsed() > timeout {
            anyhow::bail!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn terminate_live_session(client: &Client, session_id: &SessionId, reason: &str) {
    if let Ok(handle) = live_workflow_handle(client, session_id) {
        let _ = handle
            .terminate(WorkflowTerminateOptions::builder().reason(reason).build())
            .await;
    }
}

pub async fn wait_for_admission_failure(
    client: &Client,
    session_id: &SessionId,
    kind: AgentAdmissionFailureKind,
) -> anyhow::Result<()> {
    let handle = live_workflow_handle(client, session_id)?;
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

pub async fn wait_for_session_status(
    api: &temporal_server::gateway::GatewayAgentApi,
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
                run_limit: None,
            })
            .await?;
        if session.result.session.status == expected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub fn require_storage_live_env() -> anyhow::Result<()> {
    if env::var("LIGHTSPEED_POSTGRES_URL")
        .or_else(|_| env::var("LIGHTSPEED_TEST_POSTGRES_URL"))
        .is_err()
    {
        anyhow::bail!(
            "temporal live test requires LIGHTSPEED_POSTGRES_URL or LIGHTSPEED_TEST_POSTGRES_URL"
        );
    }
    if env::var("LIGHTSPEED_PG_UNIVERSE_ID").is_err() {
        anyhow::bail!("temporal live test requires LIGHTSPEED_PG_UNIVERSE_ID");
    }
    Ok(())
}

pub fn require_openai_live_env() -> anyhow::Result<()> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
        anyhow::anyhow!("OPENAI_API_KEY must be set to run the OpenAI Agent live test")
    })?;
    if api_key.trim().is_empty() {
        anyhow::bail!("OPENAI_API_KEY is set but empty");
    }
    Ok(())
}

pub fn openai_live_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: env::var("LIGHTSPEED_OPENAI_MODEL")
            .or_else(|_| env::var("OPENAI_RESPONSES_MODEL"))
            .or_else(|_| env::var("OPENAI_LIVE_MODEL"))
            .or_else(|_| env::var("LIGHTSPEED_CHAT_MODEL"))
            .unwrap_or_else(|_| "gpt-5.5".to_owned()),
    }
}

pub fn openai_completions_live_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiCompletions,
        provider_id: "openai".to_owned(),
        model: env::var("LIGHTSPEED_OPENAI_MODEL")
            .or_else(|_| env::var("OPENAI_COMPLETIONS_MODEL"))
            .or_else(|_| env::var("OPENAI_LIVE_MODEL"))
            .or_else(|_| env::var("LIGHTSPEED_CHAT_MODEL"))
            .unwrap_or_else(|_| "gpt-5.5".to_owned()),
    }
}
