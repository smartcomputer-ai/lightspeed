#![allow(dead_code)]

use std::{
    env,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use api::{AgentApiService, RunStatus, SessionItemView, SessionReadParams, SessionStatus};
use engine::{
    CoreAgentLlm, CoreAgentTools, ModelSelection, ProviderApiKind, SessionId, storage::BlobStore,
};
use temporal_server::{
    pg_store_from_env,
    worker::{
        ActivityState, AudioTranscoder, AudioTranscriber, FakeLlm, FakeTools, WorkerActivities,
        core_runtime, worker_with_activities,
    },
};
use temporal_workflow::{
    AgentAdmissionFailureKind, AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::{Client, WorkflowQueryOptions};

pub static LIVE_TEST_LOCK: Mutex<()> = Mutex::new(());

pub async fn run_with_live_worker<F, Fut>(
    activities: WorkerActivities,
    run_client: F,
) -> anyhow::Result<()>
where
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
    let mut worker =
        worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
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

pub async fn fake_worker_activities() -> anyhow::Result<WorkerActivities> {
    Ok(WorkerActivities::new(fake_activity_state().await?))
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
    Ok(WorkerActivities::new(state))
}

pub async fn fake_activity_state() -> anyhow::Result<ActivityState> {
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(FakeLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    Ok(ActivityState::from_pg_store(store, llm, tools))
}

pub fn final_assistant_text(run: &api::RunView) -> Option<&str> {
    run.items.iter().rev().find_map(|item| match item {
        SessionItemView::AssistantMessage { text, .. } => Some(text.as_str()),
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

pub async fn wait_for_admission_failure(
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
