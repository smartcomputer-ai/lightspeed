//! Temporal worker process support and activity implementations.

mod activities;
mod fake;
mod secrets;
mod session_tools;

use temporalio_client::Client;
use temporalio_common::{telemetry::TelemetryOptions, worker::WorkerTaskTypes};
use temporalio_sdk::{Worker, WorkerOptions};
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

use crate::{config::pg_store_from_env, fleet::AgentApiFleetRuntime, gateway::GatewayAgentApi};

pub use activities::{
    ActivityState, AudioTranscodeError, AudioTranscodeOutput, AudioTranscodeRequest,
    AudioTranscoder, AudioTranscriber, AudioTranscription, AudioTranscriptionError,
    AudioTranscriptionRequest, FfmpegAudioTranscoder, LlmActivityDeps, PreprocessActivityDeps,
    SkillCatalogActivityDeps, StorageActivityDeps, ToolActivityDeps, WorkerActivities,
};
pub use fake::{FakeLlm, FakeTools};
pub use secrets::{BrokerSecretResolver, StoredProviderKeyResolver};
pub use session_tools::SessionTools;
pub use temporal_workflow::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION,
    ACTIVITY_LLM_GENERATE, ACTIVITY_PREPROCESS_RUN_INPUT, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB,
    ACTIVITY_SKILL_CATALOG_REFRESH, ACTIVITY_TOOL_INVOKE_BATCH, AgentSessionWorkflow,
    AppendEventsRequest, ContextCompactActivityRequest, CreateOrLoadSessionRequest,
    CreateOrLoadSessionResult, DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, FAKE_TOOL_NAME, LlmGenerateActivityRequest,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult, SkillCatalogRefreshActivityRequest,
    SkillCatalogRefreshActivityResult, ToolInvokeBatchActivityRequest, connect_temporal,
    default_run_config, default_session_config,
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
        .register_activities(activities)
        .task_types(WorkerTaskTypes::all())
        .build();
    Worker::new(runtime, client, worker_options).map_err(|error| anyhow::anyhow!("{error}"))
}

pub async fn run_worker(config: WorkerServerConfig) -> anyhow::Result<()> {
    let runtime = core_runtime()?;
    let client = connect_temporal(&config.temporal_target, &config.namespace).await?;
    let store = pg_store_from_env().await?;
    let api = std::sync::Arc::new(
        GatewayAgentApi::builder(client.clone(), store.clone())
            .with_task_queue(config.task_queue.clone())
            .build(),
    );
    let fleet_runtime = std::sync::Arc::new(AgentApiFleetRuntime::new(api));
    let activities =
        WorkerActivities::from_pg_store_with_default_runtime_and_fleet(store, fleet_runtime)?;
    let mut worker =
        worker_with_activities(&runtime, client, config.task_queue.clone(), activities)?;
    tracing::info!(
        target: "temporal_server",
        temporal_target = %config.temporal_target,
        namespace = %config.namespace,
        task_queue = %config.task_queue,
        "temporal worker polling"
    );
    worker.run().await?;
    Ok(())
}
