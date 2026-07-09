//! Temporal worker process support and activity implementations.

mod activities;
mod fake;
mod reaper;
mod secrets;
mod session_tools;

use std::sync::Arc;

use temporalio_client::Client;
use temporalio_common::{telemetry::TelemetryOptions, worker::WorkerTaskTypes};
use temporalio_sdk::{Worker, WorkerOptions};
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

use crate::{config::DeploymentStores, universe::UniverseRuntime};

pub use activities::{
    ActivityState, AudioTranscodeError, AudioTranscodeOutput, AudioTranscodeRequest,
    AudioTranscoder, AudioTranscriber, AudioTranscription, AudioTranscriptionError,
    AudioTranscriptionRequest, FfmpegAudioTranscoder, LlmActivityDeps, PreprocessActivityDeps,
    RuntimeProjectionActivityDeps, StorageActivityDeps, ToolActivityDeps, WorkerActivities,
    default_audio_transcoder_from_env,
};
pub use fake::{FakeLlm, FakeTools};
pub use reaper::{PromiseReaper, ReaperStats};
pub use secrets::{BrokerSecretResolver, StoredProviderKeyResolver};
pub use session_tools::SessionTools;
pub use temporal_workflow::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CANCEL_PROMISE_SOURCE, ACTIVITY_CHECK_PROMISE_SOURCE,
    ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION, ACTIVITY_ENVIRONMENT_JOB_CANCEL,
    ACTIVITY_ENVIRONMENT_JOB_POLL, ACTIVITY_LLM_GENERATE, ACTIVITY_PREPROCESS_RUN_INPUT,
    ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB, ACTIVITY_RUNTIME_PROJECTION_REFRESH,
    ACTIVITY_TOOL_INVOKE_BATCH, AgentSessionWorkflow, AppendEventsRequest,
    ContextCompactActivityRequest, CreateOrLoadSessionRequest, CreateOrLoadSessionResult,
    DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET,
    EnvironmentJobCancelActivityRequest, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult, EnvironmentJobWorkflow, EnvironmentJobWorkflowArgs,
    FAKE_TOOL_NAME, LlmGenerateActivityRequest, PreprocessRunInputActivityRequest,
    PreprocessRunInputActivityResult, PutBlobRequest, ReadBlobRequest, ReadBlobResult,
    RuntimeProjectionRefreshActivityRequest, RuntimeProjectionRefreshActivityResult,
    ToolInvokeBatchActivityRequest, connect_temporal, default_run_config, default_session_config,
};

#[derive(Clone, Debug)]
pub struct WorkerServerConfig {
    pub task_queue: String,
    pub temporal_target: String,
    pub namespace: String,
}

pub fn core_runtime() -> anyhow::Result<CoreRuntime> {
    CoreRuntime::new_assume_tokio(
        RuntimeOptions::builder()
            .telemetry_options(TelemetryOptions::builder().build())
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
    )
}

pub fn worker_with_activities(
    runtime: &CoreRuntime,
    client: Client,
    task_queue: String,
    activities: WorkerActivities,
) -> anyhow::Result<Worker> {
    let worker_options = WorkerOptions::new(task_queue)
        .register_workflow::<AgentSessionWorkflow>()
        .register_workflow::<EnvironmentJobWorkflow>()
        .register_activities(activities)
        .task_types(WorkerTaskTypes::all())
        .build();
    Worker::new(runtime, client, worker_options).map_err(|error| anyhow::anyhow!("{error}"))
}

pub async fn run_worker(config: WorkerServerConfig) -> anyhow::Result<()> {
    let runtime = core_runtime()?;
    let client = connect_temporal(&config.temporal_target, &config.namespace).await?;
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(crate::config::blob_cache_from_env()?);
    let reaper_stores = stores.clone();
    // The worker serves every universe of the deployment regardless of the
    // gateway's auth mode; per-universe state resolves lazily from the
    // universe-composed workflow id of each activity task.
    let universes = Arc::new(UniverseRuntime::new(
        client.clone(),
        config.task_queue.clone(),
        None,
        stores,
    )?);
    let activities = WorkerActivities::with_runtime(universes);
    let mut worker = worker_with_activities(
        &runtime,
        client.clone(),
        config.task_queue.clone(),
        activities,
    )?;
    let reaper = PromiseReaper::new(client.clone(), reaper_stores);
    let reaper_task = tokio::spawn(reaper.run_forever());
    tracing::info!(
        target: "temporal_server",
        temporal_target = %config.temporal_target,
        namespace = %config.namespace,
        task_queue = %config.task_queue,
        "temporal worker polling"
    );
    let result = worker.run().await;
    reaper_task.abort();
    result?;
    Ok(())
}
