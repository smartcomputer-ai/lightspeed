//! Temporal worker process support and activity implementations.

mod activities;
mod bots;
mod channels;
mod fake;
pub(crate) mod mcp;
mod reaper;
mod secrets;
mod session_tools;

use temporalio_client::Client;
use temporalio_common::{telemetry::TelemetryOptions, worker::WorkerTaskTypes};
use temporalio_sdk::{Worker, WorkerOptions};
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

use temporal_workflow::{
    BotControllerWorkflow, BotTriggerFireWorkflow, ChannelConversationWorkflow,
};

pub use activities::{
    ActivityState, AudioTranscodeError, AudioTranscodeOutput, AudioTranscodeRequest,
    AudioTranscoder, AudioTranscriber, AudioTranscription, AudioTranscriptionError,
    AudioTranscriptionRequest, FfmpegAudioTranscoder, LlmActivityDeps, PreprocessActivityDeps,
    RuntimeProjectionActivityDeps, StorageActivityDeps, ToolActivityDeps, WorkerActivities,
    default_audio_transcoder_from_env, subagent_catalog_snapshot,
};
pub use bots::BotWorkerActivities;
pub use channels::ChannelWorkerActivities;
pub use fake::{FAKE_TRANSIENT_RETRY_AFTER, FakeLlm, FakeRuntimeCounters, FakeTools};
pub use reaper::{
    CasBlobSweeper, CasSweepStats, PromiseReaper, ReaperStats, SessionRetentionReaper,
    SessionRetentionReaperStats,
};
pub use secrets::{BrokerSecretResolver, StoredModelProviderResolver, StoredProviderKeyResolver};
pub use session_tools::{SessionTools, ToolCallExecution};
pub use temporal_workflow::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CANCEL_WORKFLOW_TOOL_EXECUTION,
    ACTIVITY_CHECK_WORKFLOW_TOOL_EXECUTION, ACTIVITY_CONTEXT_COMPACT,
    ACTIVITY_CREATE_OR_LOAD_SESSION, ACTIVITY_ENVIRONMENT_JOB_CANCEL,
    ACTIVITY_ENVIRONMENT_JOB_POLL, ACTIVITY_ENVIRONMENT_JOB_PREPARE_WORKFLOW_TOOL,
    ACTIVITY_ENVIRONMENT_JOB_START, ACTIVITY_LLM_GENERATE, ACTIVITY_MATERIALIZE_AWAIT_RESULT,
    ACTIVITY_PREPARE_JOINED_CONTEXT, ACTIVITY_PREPROCESS_RUN_INPUT, ACTIVITY_PUT_BLOB,
    ACTIVITY_READ_BLOB, ACTIVITY_RUNTIME_PROJECTION_REFRESH,
    ACTIVITY_START_WORKFLOW_TOOL_EXECUTION, ACTIVITY_SUBAGENT_CLOSE, ACTIVITY_SUBAGENT_PREPARE,
    ACTIVITY_SUBAGENT_RESOLVE, ACTIVITY_TOOL_INVOKE_BATCH, ACTIVITY_TOOL_INVOKE_CALL,
    ACTIVITY_TOOL_PREPARE_PROMISE_CONTROLS, ACTIVITY_VALIDATE_WORKFLOW_TOOL_REPLY,
    AgentSessionWorkflow, AppendEventsRequest, ContextCompactActivityRequest,
    CreateOrLoadSessionRequest, CreateOrLoadSessionResult, DEFAULT_TASK_QUEUE,
    DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, EnvironmentJobCancelActivityRequest,
    EnvironmentJobPollActivityRequest, EnvironmentJobPollActivityResult,
    EnvironmentJobStartActivityRequest, EnvironmentJobStartActivityResult, EnvironmentJobWorkflow,
    EnvironmentJobWorkflowArgs, FAKE_TOOL_NAME, LlmGenerateActivityRequest,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult, RuntimeProjectionRefreshActivityRequest,
    RuntimeProjectionRefreshActivityResult, SubagentExecutionWorkflow,
    ToolInvokeBatchActivityRequest, ToolInvokeCallActivityRequest,
    ToolPreparePromiseControlsActivityRequest, connect_temporal, default_run_config,
    default_session_config,
};
pub use temporal_workflow::{
    ACTIVITY_AWAIT_ENVIRONMENT_READY, AwaitEnvironmentReadyActivityRequest,
    AwaitEnvironmentReadyActivityResult, ToolInvokeCallActivityResult,
};

pub fn core_runtime() -> anyhow::Result<CoreRuntime> {
    CoreRuntime::new_assume_tokio(
        RuntimeOptions::builder()
            .telemetry_options(TelemetryOptions::builder().build())
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
    )
}

/// The `sessions` worker: session, environment-job, and sub-agent
/// workflows with their activities, polling every task type.
pub fn worker_with_activities(
    runtime: &CoreRuntime,
    client: Client,
    task_queue: String,
    activities: WorkerActivities,
) -> anyhow::Result<Worker> {
    sessions_worker(
        runtime,
        client,
        task_queue,
        activities,
        WorkerTaskTypes::all(),
    )
}

pub fn sessions_worker(
    runtime: &CoreRuntime,
    client: Client,
    task_queue: String,
    activities: WorkerActivities,
    task_types: WorkerTaskTypes,
) -> anyhow::Result<Worker> {
    let worker_options = WorkerOptions::new(task_queue)
        .register_workflow::<AgentSessionWorkflow>()
        .register_workflow::<EnvironmentJobWorkflow>()
        .register_workflow::<SubagentExecutionWorkflow>()
        .register_activities(activities)
        .task_types(task_types)
        .build();
    Worker::new(runtime, client, worker_options).map_err(|error| anyhow::anyhow!("{error}"))
}

/// The `bots` worker: bot controllers and trigger fires with their
/// activities.
pub fn bots_worker(
    runtime: &CoreRuntime,
    client: Client,
    task_queue: String,
    activities: BotWorkerActivities,
    task_types: WorkerTaskTypes,
) -> anyhow::Result<Worker> {
    let worker_options = WorkerOptions::new(task_queue)
        .register_workflow::<BotControllerWorkflow>()
        .register_workflow::<BotTriggerFireWorkflow>()
        .register_activities(activities)
        .task_types(task_types)
        .build();
    Worker::new(runtime, client, worker_options).map_err(|error| anyhow::anyhow!("{error}"))
}

/// The `channels` worker: conversation workflows with their core-side
/// activities.
pub fn channels_worker(
    runtime: &CoreRuntime,
    client: Client,
    task_queue: String,
    activities: ChannelWorkerActivities,
    task_types: WorkerTaskTypes,
) -> anyhow::Result<Worker> {
    let worker_options = WorkerOptions::new(task_queue)
        .register_workflow::<ChannelConversationWorkflow>()
        .register_activities(activities)
        .task_types(task_types)
        .build();
    Worker::new(runtime, client, worker_options).map_err(|error| anyhow::anyhow!("{error}"))
}
