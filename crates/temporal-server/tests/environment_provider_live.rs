mod support;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use api::{
    AgentApiService, AgentProfileInput, AuthProviderConfigInput, AuthProviderCreateParams,
    EnvironmentProviderCapabilitiesView, EnvironmentProviderHeartbeatParams,
    EnvironmentProviderImplementationView, EnvironmentProviderKindView,
    EnvironmentProviderRegisterParams, HostControllerConnectionView, HostTargetAttachRequestView,
    HostTargetCreateRequestView, HostTransportView, InputItem, ProfileCreateParams,
    ProfileDeleteParams, ProfileDocument, ProfileEnvironment, ProfileId, ProfileSource,
    RunStartParams, RunStartSource, RunStatus, SandboxTargetSpecView, SessionConfigInput,
    SessionEnvironmentAttachParams, SessionEnvironmentCloseParams, SessionEnvironmentCreateParams,
    SessionEnvironmentCredentialBindParams, SessionEnvironmentCredentialListParams,
    SessionEnvironmentCredentialSourceView, SessionEnvironmentCredentialUnbindParams,
    SessionEnvironmentListParams, SessionJobCancelParams, SessionJobCancelScopeView,
    SessionJobCreateParams, SessionJobDependencyInput, SessionJobDependencyPolicyView,
    SessionJobHandleInput, SessionJobHandleView, SessionJobListParams, SessionJobReadEntryView,
    SessionJobReadParams, SessionJobStartSpecInput, SessionJobStatusView, SessionStartParams,
    VfsMountAccess as ApiVfsMountAccess, VfsMountPutParams, VfsMountSourceInput,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use engine::{
    ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextMessageRole, CoreAgentIoError,
    CoreAgentLlm, CoreAgentTools, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, ModelSelection, ObservedToolCall, ProviderApiKind,
    ToolCallId, ToolName, storage::BlobStore,
};
use futures::{SinkExt, StreamExt};
use host_protocol::{
    control::{
        handshake::{ControllerCapabilities, ControllerInitializeResponse},
        methods::{
            ATTACH_TARGET_METHOD, CLOSE_TARGET_METHOD, CREATE_TARGET_METHOD,
            INITIALIZE_METHOD as CONTROL_INITIALIZE_METHOD, LIST_TARGETS_METHOD,
        },
        targets::{
            AttachTargetResponse, CloseTargetResponse, CreateTargetResponse, HostTargetStatus,
            HostTargetSummary, ListTargetsResponse,
        },
    },
    data::{
        handshake::{InitializeResponse, InitializedParams},
        methods::{
            INITIALIZE_METHOD as DATA_INITIALIZE_METHOD, INITIALIZED_METHOD, PROCESS_READ_METHOD,
            PROCESS_START_METHOD,
        },
        process::{
            ProcessOutputChunk, ProcessOutputStream, ReadProcessResponse, StartProcessParams,
            StartProcessResponse,
        },
    },
    shared::{
        ByteChunk, CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionId,
        HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport, ImplementationInfo,
    },
};
use serde_json::{Value, json};
use support::live::{LIVE_TEST_LOCK, final_assistant_text, require_storage_live_env};
use temporal_server::{
    gateway::{DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayAgentApi, gateway_router},
    pg_store_from_env,
    worker::{ActivityState, SessionTools, WorkerActivities},
};
use temporal_workflow::AgentSessionWorkflow;
use temporalio_client::{Client, WorkflowTerminateOptions};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const ATTACH_TARGET_ID: &str = "attach-target";
const CREATED_TARGET_ID: &str = "created-target";
const PROCESS_STDOUT: &str = "fake provider stdout\n";
const BRIDGE_FILE_NAME: &str = "bridge-agent.txt";
const BRIDGE_FILE_MARKER: &str = "LIGHTSPEED_BRIDGE_AGENT_MARKER";
const BRIDGE_VFS_SKILL_MARKER: &str = "LIGHTSPEED_BRIDGE_VFS_SKILL_MARKER";
const BRIDGE_JOB_FILE_NAME: &str = "job-live.txt";
const BRIDGE_JOB_MARKER: &str = "LIGHTSPEED_BRIDGE_JOB_MARKER";
const BRIDGE_API_JOB_FILE_NAME: &str = "api-job-live.txt";
const BRIDGE_API_JOB_MARKER: &str = "LIGHTSPEED_BRIDGE_API_JOB_MARKER";
const BRIDGE_CREDENTIAL_FILE_NAME: &str = "credential-live.txt";
const BRIDGE_CREDENTIAL_ENV_NAME: &str = "P87_LIVE_TOKEN";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_fake_provider_create_attach_and_process_tool() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let provider = FakeHostProvider::start().await?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(ExecCommandLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store, llm, tools),
    );

    support::live::run_with_live_worker(activities, |client, task_queue, session_id| async move {
        run_fake_provider_client(client, task_queue, session_id, provider).await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_profile_attaches_host_environment() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let provider = FakeHostProvider::start().await?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(ExecCommandLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store, llm, tools),
    );

    support::live::run_with_live_worker(activities, |client, task_queue, session_id| async move {
        run_profile_environment_client(client, task_queue, session_id, provider).await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env and target/debug/host-bridge"]
async fn temporal_live_host_bridge_agent_reads_local_filesystem() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let bridge_bin = host_bridge_binary_path()?;
    let bridge_root = tempfile::tempdir()?;
    let bridge_root = bridge_root.path().canonicalize()?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(BridgeFileLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store, llm, tools),
    );

    support::live::run_with_live_worker(activities, |client, task_queue, session_id| async move {
        run_host_bridge_client(client, task_queue, session_id, bridge_bin, bridge_root).await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env and target/debug/host-bridge"]
async fn temporal_live_host_bridge_environment_jobs_round_trip() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let bridge_bin = host_bridge_binary_path()?;
    let bridge_root = tempfile::tempdir()?;
    let bridge_root = bridge_root.path().canonicalize()?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(BridgeJobsLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store, llm, tools),
    );

    support::live::run_with_live_worker(activities, |client, task_queue, session_id| async move {
        run_host_bridge_jobs_client(client, task_queue, session_id, bridge_bin, bridge_root).await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env and target/debug/host-bridge"]
async fn temporal_live_host_bridge_environment_credential_injection() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let bridge_bin = host_bridge_binary_path()?;
    let bridge_root = tempfile::tempdir()?;
    let bridge_root = bridge_root.path().canonicalize()?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(ExecCommandLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store, llm, tools),
    );

    support::live::run_with_live_worker(activities, |client, task_queue, session_id| async move {
        run_host_bridge_credential_client(client, task_queue, session_id, bridge_bin, bridge_root)
            .await
    })
    .await
}

async fn run_host_bridge_client(
    client: Client,
    task_queue: String,
    session_id: engine::SessionId,
    bridge_bin: PathBuf,
    bridge_root: PathBuf,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let blob_store: Arc<dyn BlobStore> = store.clone();
    let model = fake_model();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store)
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .with_max_steps_per_input(32)
            .build(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let gateway_url = format!("http://{}/rpc", listener.local_addr()?);
    let gateway = tokio::spawn({
        let api = api.clone();
        async move {
            let app = gateway_router(
                std::sync::Arc::new(temporal_server::gateway::GatewayState::for_api(api)),
                DEFAULT_MAX_REQUEST_BODY_BYTES,
            );
            axum::serve(listener, app).await
        }
    });

    let provider_id = format!("host-bridge-{}", uuid::Uuid::new_v4().simple());
    let bridge = SpawnedBridge::start(&bridge_bin, &gateway_url, &provider_id, &bridge_root)?;

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(api_projection::model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let skill_snapshot = vfs::create_inline_snapshot(
        blob_store.as_ref(),
        vfs::CreateInlineSnapshotRequest::new(vec![
            vfs::InlineFile::new(
                "SKILL.md",
                format!("{BRIDGE_VFS_SKILL_MARKER}\n").into_bytes(),
            )
            .expect("inline skill"),
        ]),
    )
    .await?;
    api.put_vfs_mount(VfsMountPutParams {
        session_id: session_id.as_str().to_owned(),
        mount_path: "/skills".to_owned(),
        source: VfsMountSourceInput::Snapshot {
            snapshot_ref: skill_snapshot.snapshot_ref.as_str().to_owned(),
        },
        access: ApiVfsMountAccess::ReadOnly,
    })
    .await?;

    let attached =
        wait_for_bridge_attach(api.as_ref(), &session_id, &provider_id, "bridge-local").await?;
    assert_eq!(
        attached.result.active_env_id.as_deref(),
        Some("bridge-local")
    );
    assert_eq!(
        attached.result.environment.cwd.as_deref(),
        Some(path_str(&bridge_root)?)
    );

    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    text: "write a file through the host bridge, then read it back".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = support::live::wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    assert_eq!(
        run.status,
        RunStatus::Completed,
        "host bridge run did not complete: {run:#?}"
    );
    let Some(text) = final_assistant_text(&run) else {
        anyhow::bail!("host bridge run missing final assistant message: {run:#?}");
    };
    assert!(
        text.contains(BRIDGE_FILE_MARKER),
        "final answer did not include marker from bridge file read: {text}"
    );
    assert!(
        text.contains(BRIDGE_VFS_SKILL_MARKER),
        "final answer did not include marker from VFS /skills read: {text}"
    );

    let local_file = bridge_root.join(BRIDGE_FILE_NAME);
    let local_contents = tokio::fs::read_to_string(&local_file).await?;
    assert!(
        local_contents.contains(BRIDGE_FILE_MARKER),
        "bridge command did not write marker to local file {}: {local_contents}",
        local_file.display()
    );

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "bridge-local".to_owned(),
        force: false,
        close_target: Some(false),
    })
    .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("host bridge live test cleanup")
                .build(),
        )
        .await;
    drop(bridge);
    gateway.abort();
    Ok(())
}

async fn run_host_bridge_jobs_client(
    client: Client,
    task_queue: String,
    session_id: engine::SessionId,
    bridge_bin: PathBuf,
    bridge_root: PathBuf,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = fake_model();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store)
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .with_max_steps_per_input(64)
            .build(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let gateway_url = format!("http://{}/rpc", listener.local_addr()?);
    let gateway = tokio::spawn({
        let api = api.clone();
        async move {
            let app = gateway_router(
                std::sync::Arc::new(temporal_server::gateway::GatewayState::for_api(api)),
                DEFAULT_MAX_REQUEST_BODY_BYTES,
            );
            axum::serve(listener, app).await
        }
    });

    let provider_id = format!("host-bridge-jobs-{}", uuid::Uuid::new_v4().simple());
    let bridge = SpawnedBridge::start(&bridge_bin, &gateway_url, &provider_id, &bridge_root)?;

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(api_projection::model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let attached =
        wait_for_bridge_attach(api.as_ref(), &session_id, &provider_id, "bridge-local").await?;
    assert_eq!(
        attached.result.active_env_id.as_deref(),
        Some("bridge-local")
    );

    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    text: "start, list, wait for, and read a durable environment job".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = support::live::wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    assert_eq!(
        run.status,
        RunStatus::Completed,
        "host bridge jobs run did not complete: {run:#?}"
    );
    let Some(text) = final_assistant_text(&run) else {
        anyhow::bail!("host bridge jobs run missing final assistant message: {run:#?}");
    };
    assert!(
        text.contains(BRIDGE_JOB_MARKER),
        "final answer did not include marker from job output: {text}"
    );
    assert!(
        text.contains("job_wait outcome: Satisfied"),
        "final answer did not include a satisfied job_wait result: {text}"
    );

    let local_file = bridge_root.join(BRIDGE_JOB_FILE_NAME);
    let local_contents = tokio::fs::read_to_string(&local_file).await?;
    assert!(
        local_contents.contains(BRIDGE_JOB_MARKER),
        "bridge job did not write marker to local file {}: {local_contents}",
        local_file.display()
    );

    let api_command = format!(
        "printf '{}\\n' > {} && printf '{}\\n'",
        BRIDGE_API_JOB_MARKER, BRIDGE_API_JOB_FILE_NAME, BRIDGE_API_JOB_MARKER
    );
    let created = api
        .create_session_jobs(SessionJobCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            request_id: "api_job_round_trip".to_owned(),
            jobs: vec![SessionJobStartSpecInput {
                name: Some("api-live-job".to_owned()),
                job_id: None,
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), api_command],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(10_000),
                depends_on: Vec::new(),
                dependency_policy: SessionJobDependencyPolicyView::AllSucceeded,
                queue_key: None,
            }],
        })
        .await?;
    assert_eq!(created.result.env_id, "bridge-local");
    assert_eq!(created.result.jobs.len(), 1);
    let api_job = created.result.jobs[0].handle.clone();

    let listed = api
        .list_session_jobs(SessionJobListParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            limit: Some(10),
        })
        .await?;
    assert!(
        listed
            .result
            .jobs
            .iter()
            .any(|record| record.handle.job_id == api_job.job_id),
        "session/jobs/list did not return API-created job: {:?}",
        listed.result.jobs
    );

    let mut api_job_output = None;
    let started = Instant::now();
    while started.elapsed() <= Duration::from_secs(10) {
        let read = api
            .read_session_jobs(SessionJobReadParams {
                session_id: session_id.as_str().to_owned(),
                jobs: vec![SessionJobHandleInput {
                    session_id: Some(api_job.session_id.clone()),
                    env_id: Some(api_job.env_id.clone()),
                    job_id: api_job.job_id.clone(),
                }],
                output_bytes: Some(4096),
                after_seq: None,
                include_artifacts: false,
            })
            .await?;
        let entry = read.result.jobs.into_iter().next().expect("job read entry");
        if entry
            .summary
            .as_ref()
            .is_some_and(|summary| summary.status == SessionJobStatusView::Succeeded)
        {
            let output = entry
                .output_chunks
                .into_iter()
                .filter_map(|chunk| BASE64_STANDARD.decode(chunk.data_base64).ok())
                .filter_map(|bytes| String::from_utf8(bytes).ok())
                .collect::<Vec<_>>()
                .join("");
            api_job_output = Some(output);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(api_job_output) = api_job_output else {
        anyhow::bail!("session/jobs/read did not observe API job completion");
    };
    assert!(
        api_job_output.contains(BRIDGE_API_JOB_MARKER),
        "session/jobs/read output did not include API job marker: {api_job_output}"
    );

    let api_local_file = bridge_root.join(BRIDGE_API_JOB_FILE_NAME);
    let api_local_contents = tokio::fs::read_to_string(&api_local_file).await?;
    assert!(
        api_local_contents.contains(BRIDGE_API_JOB_MARKER),
        "API job did not write marker to local file {}: {api_local_contents}",
        api_local_file.display()
    );

    run_api_job_queue_live_check(api.as_ref(), &session_id, &bridge_root).await?;
    run_api_job_parallel_live_check(api.as_ref(), &session_id, &bridge_root).await?;
    run_api_job_dag_live_check(api.as_ref(), &session_id, &bridge_root).await?;
    run_api_job_retry_live_check(api.as_ref(), &session_id, &bridge_root).await?;

    let cancel_created = api
        .create_session_jobs(SessionJobCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            request_id: "api_job_cancel".to_owned(),
            jobs: vec![SessionJobStartSpecInput {
                name: Some("api-cancel-job".to_owned()),
                job_id: None,
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(60_000),
                depends_on: Vec::new(),
                dependency_policy: SessionJobDependencyPolicyView::AllSucceeded,
                queue_key: None,
            }],
        })
        .await?;
    let cancel_job = cancel_created.result.jobs[0].handle.clone();
    let cancelled = api
        .cancel_session_jobs(SessionJobCancelParams {
            session_id: session_id.as_str().to_owned(),
            jobs: vec![SessionJobHandleInput {
                session_id: Some(cancel_job.session_id.clone()),
                env_id: Some(cancel_job.env_id.clone()),
                job_id: cancel_job.job_id.clone(),
            }],
            scope: SessionJobCancelScopeView::Job,
            force: true,
        })
        .await?;
    let cancel_status = cancelled.result.jobs[0]
        .summary
        .as_ref()
        .map(|summary| summary.status);
    assert!(
        matches!(
            cancel_status,
            Some(SessionJobStatusView::CancelRequested | SessionJobStatusView::Cancelled)
        ),
        "session/jobs/cancel returned unexpected status: {:?}",
        cancelled.result.jobs
    );
    let cancelled_read = wait_for_session_jobs_terminal(
        api.as_ref(),
        &session_id,
        std::slice::from_ref(&cancel_job),
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(
        cancelled_read[0]
            .summary
            .as_ref()
            .map(|summary| summary.status),
        Some(SessionJobStatusView::Cancelled),
        "cancelled job did not reach Cancelled: {:?}",
        cancelled_read
    );

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "bridge-local".to_owned(),
        force: false,
        close_target: Some(false),
    })
    .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("host bridge jobs live test cleanup")
                .build(),
        )
        .await;
    drop(bridge);
    gateway.abort();
    Ok(())
}

async fn run_host_bridge_credential_client(
    client: Client,
    task_queue: String,
    session_id: engine::SessionId,
    bridge_bin: PathBuf,
    bridge_root: PathBuf,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = fake_model();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store)
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .with_max_steps_per_input(32)
            .build(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let gateway_url = format!("http://{}/rpc", listener.local_addr()?);
    let gateway = tokio::spawn({
        let api = api.clone();
        async move {
            let app = gateway_router(
                std::sync::Arc::new(temporal_server::gateway::GatewayState::for_api(api)),
                DEFAULT_MAX_REQUEST_BODY_BYTES,
            );
            axum::serve(listener, app).await
        }
    });

    let provider_id = format!("host-bridge-credential-{}", uuid::Uuid::new_v4().simple());
    let credential_provider_id = format!("p87-credential-{}", uuid::Uuid::new_v4().simple());
    let secret_value = format!("p87-live-secret-{}", uuid::Uuid::new_v4().simple());
    let bridge = SpawnedBridge::start(&bridge_bin, &gateway_url, &provider_id, &bridge_root)?;

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(api_projection::model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let attached =
        wait_for_bridge_attach(api.as_ref(), &session_id, &provider_id, "bridge-local").await?;
    assert_eq!(
        attached.result.active_env_id.as_deref(),
        Some("bridge-local")
    );

    let provider = api
        .create_auth_provider(AuthProviderCreateParams {
            provider_id: Some(credential_provider_id.clone()),
            display_name: Some("P87 live credential".to_owned()),
            config: AuthProviderConfigInput::ModelApiKey {},
            credential: Some(secret_value.clone()),
        })
        .await?;
    assert_eq!(provider.result.provider.provider_id, credential_provider_id);
    assert!(provider.result.provider.has_credential);

    let bound = api
        .bind_session_environment_credential(SessionEnvironmentCredentialBindParams {
            session_id: session_id.as_str().to_owned(),
            env_id: "bridge-local".to_owned(),
            env_name: BRIDGE_CREDENTIAL_ENV_NAME.to_owned(),
            source: SessionEnvironmentCredentialSourceView::AuthProviderCredential {
                provider_id: credential_provider_id.clone(),
            },
        })
        .await?;
    assert_eq!(bound.result.credential.env_name, BRIDGE_CREDENTIAL_ENV_NAME);

    let listed = api
        .list_session_environment_credentials(SessionEnvironmentCredentialListParams {
            session_id: session_id.as_str().to_owned(),
            env_id: "bridge-local".to_owned(),
        })
        .await?;
    assert!(
        listed
            .result
            .credentials
            .iter()
            .any(|credential| credential.env_name == BRIDGE_CREDENTIAL_ENV_NAME),
        "credential binding was not listed after bind: {:?}",
        listed.result.credentials
    );

    let command = format!(
        "printf '%s\\n' \"${}\" > {}; printf '%s\\n' \"${}\"",
        BRIDGE_CREDENTIAL_ENV_NAME, BRIDGE_CREDENTIAL_FILE_NAME, BRIDGE_CREDENTIAL_ENV_NAME
    );
    let created = api
        .create_session_jobs(SessionJobCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            request_id: "p87_credential_injection".to_owned(),
            jobs: vec![SessionJobStartSpecInput {
                name: Some("p87-credential-injection".to_owned()),
                job_id: None,
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), command],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(10_000),
                depends_on: Vec::new(),
                dependency_policy: SessionJobDependencyPolicyView::AllSucceeded,
                queue_key: None,
            }],
        })
        .await?;
    let handle = created.result.jobs[0].handle.clone();
    let entries = wait_for_session_jobs_terminal(
        api.as_ref(),
        &session_id,
        std::slice::from_ref(&handle),
        Duration::from_secs(10),
    )
    .await?;
    ensure_job_statuses(
        &entries,
        SessionJobStatusView::Succeeded,
        "credential injection job",
    )?;
    let output = session_job_output_text(&entries[0]);
    assert!(
        output.contains("<redacted>"),
        "credential value was not redacted from job output: {output:?}"
    );
    assert!(
        !output.contains(&secret_value),
        "credential value leaked through job output: {output:?}"
    );

    let credential_file = bridge_root.join(BRIDGE_CREDENTIAL_FILE_NAME);
    let credential_contents = tokio::fs::read_to_string(&credential_file).await?;
    assert_eq!(
        credential_contents,
        format!("{secret_value}\n"),
        "credential env was not injected into bridge job file {}",
        credential_file.display()
    );

    let unbound = api
        .unbind_session_environment_credential(SessionEnvironmentCredentialUnbindParams {
            session_id: session_id.as_str().to_owned(),
            env_id: "bridge-local".to_owned(),
            env_name: BRIDGE_CREDENTIAL_ENV_NAME.to_owned(),
        })
        .await?;
    assert_eq!(
        unbound.result.credential.env_name,
        BRIDGE_CREDENTIAL_ENV_NAME
    );

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "bridge-local".to_owned(),
        force: false,
        close_target: Some(false),
    })
    .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("host bridge credential live test cleanup")
                .build(),
        )
        .await;
    drop(bridge);
    gateway.abort();
    Ok(())
}

async fn run_api_job_queue_live_check(
    api: &GatewayAgentApi,
    session_id: &engine::SessionId,
    bridge_root: &std::path::Path,
) -> anyhow::Result<()> {
    let queue_file_name = "api-queue-order.txt";
    let queue_file = bridge_root.join(queue_file_name);
    let mut first = api_shell_job("queue-1", format!("printf 1 >> {queue_file_name}"));
    let mut second = api_shell_job("queue-2", format!("printf 2 >> {queue_file_name}"));
    let mut third = api_shell_job("queue-3", format!("printf 3 >> {queue_file_name}"));
    first.queue_key = Some("api_live_queue".to_owned());
    second.queue_key = Some("api_live_queue".to_owned());
    third.queue_key = Some("api_live_queue".to_owned());

    let created = api
        .create_session_jobs(SessionJobCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            request_id: "api_live_queue".to_owned(),
            jobs: vec![first, second, third],
        })
        .await?;
    let handles = created
        .result
        .jobs
        .iter()
        .map(|job| job.handle.clone())
        .collect::<Vec<_>>();
    let entries =
        wait_for_session_jobs_terminal(api, session_id, &handles, Duration::from_secs(15)).await?;
    ensure_job_statuses(
        &entries,
        SessionJobStatusView::Succeeded,
        "queue-keyed jobs",
    )?;
    let contents = tokio::fs::read_to_string(&queue_file).await?;
    assert_eq!(
        contents, "123",
        "queue-keyed jobs did not execute serially in accepted order"
    );
    Ok(())
}

async fn run_api_job_parallel_live_check(
    api: &GatewayAgentApi,
    session_id: &engine::SessionId,
    bridge_root: &std::path::Path,
) -> anyhow::Result<()> {
    let order_file_name = "api-parallel-order.txt";
    let order_file = bridge_root.join(order_file_name);
    let created = api
        .create_session_jobs(SessionJobCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            request_id: "api_live_parallel".to_owned(),
            jobs: vec![
                api_shell_job(
                    "parallel-a",
                    format!(
                        "printf 'a-start\\n' >> {order_file_name}; sleep 1; printf 'a-end\\n' >> {order_file_name}"
                    ),
                ),
                api_shell_job(
                    "parallel-b",
                    format!(
                        "printf 'b-start\\n' >> {order_file_name}; sleep 1; printf 'b-end\\n' >> {order_file_name}"
                    ),
                ),
            ],
        })
        .await?;
    let handles = created
        .result
        .jobs
        .iter()
        .map(|job| job.handle.clone())
        .collect::<Vec<_>>();
    let entries =
        wait_for_session_jobs_terminal(api, session_id, &handles, Duration::from_secs(15)).await?;
    ensure_job_statuses(&entries, SessionJobStatusView::Succeeded, "parallel jobs")?;

    let contents = tokio::fs::read_to_string(&order_file).await?;
    let lines = contents.lines().collect::<Vec<_>>();
    let a_start = line_index(&lines, "a-start")?;
    let b_start = line_index(&lines, "b-start")?;
    let a_end = line_index(&lines, "a-end")?;
    let b_end = line_index(&lines, "b-end")?;
    let latest_start = a_start.max(b_start);
    let earliest_end = a_end.min(b_end);
    assert!(
        latest_start < earliest_end,
        "parallel jobs did not overlap; order file was: {contents:?}"
    );
    Ok(())
}

async fn run_api_job_dag_live_check(
    api: &GatewayAgentApi,
    session_id: &engine::SessionId,
    bridge_root: &std::path::Path,
) -> anyhow::Result<()> {
    let dag_file_name = "api-dag-order.txt";
    let dag_file = bridge_root.join(dag_file_name);
    let checkout = api_shell_job("checkout", format!("printf A >> {dag_file_name}"));
    let mut build = api_shell_job("build", format!("printf B >> {dag_file_name}"));
    build.depends_on = vec![SessionJobDependencyInput {
        job_id: None,
        name: Some("checkout".to_owned()),
    }];
    let mut tests = api_shell_job("tests", format!("printf C >> {dag_file_name}"));
    tests.depends_on = vec![SessionJobDependencyInput {
        job_id: None,
        name: Some("build".to_owned()),
    }];

    let created = api
        .create_session_jobs(SessionJobCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            request_id: "api_live_dag".to_owned(),
            jobs: vec![checkout, build, tests],
        })
        .await?;
    let final_handle = created
        .result
        .jobs
        .last()
        .expect("created DAG final job")
        .handle
        .clone();
    let entries = wait_for_session_jobs_terminal(
        api,
        session_id,
        std::slice::from_ref(&final_handle),
        Duration::from_secs(15),
    )
    .await?;
    ensure_job_statuses(
        &entries,
        SessionJobStatusView::Succeeded,
        "dependency DAG final job",
    )?;
    let contents = tokio::fs::read_to_string(&dag_file).await?;
    assert_eq!(
        contents, "ABC",
        "dependency DAG did not execute in dependency order"
    );
    Ok(())
}

async fn run_api_job_retry_live_check(
    api: &GatewayAgentApi,
    session_id: &engine::SessionId,
    bridge_root: &std::path::Path,
) -> anyhow::Result<()> {
    let retry_file_name = "api-retry-count.txt";
    let retry_file = bridge_root.join(retry_file_name);
    let params = SessionJobCreateParams {
        session_id: session_id.as_str().to_owned(),
        env_id: Some("bridge-local".to_owned()),
        request_id: "api_live_retry".to_owned(),
        jobs: vec![api_shell_job(
            "retry",
            format!("printf R >> {retry_file_name}"),
        )],
    };

    let first = api.create_session_jobs(params.clone()).await?;
    let second = api.create_session_jobs(params).await?;
    assert_eq!(
        first.result.jobs[0].handle.job_id, second.result.jobs[0].handle.job_id,
        "retry-stable API start did not return the same job id"
    );
    let handle = first.result.jobs[0].handle.clone();
    let entries = wait_for_session_jobs_terminal(
        api,
        session_id,
        std::slice::from_ref(&handle),
        Duration::from_secs(10),
    )
    .await?;
    ensure_job_statuses(&entries, SessionJobStatusView::Succeeded, "retry job")?;

    let listed = api
        .list_session_jobs(SessionJobListParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-local".to_owned()),
            limit: Some(200),
        })
        .await?;
    let matching_records = listed
        .result
        .jobs
        .iter()
        .filter(|record| record.handle.job_id == handle.job_id)
        .count();
    assert_eq!(
        matching_records, 1,
        "retry-stable API start inserted duplicate registry rows: {:?}",
        listed.result.jobs
    );

    let contents = tokio::fs::read_to_string(&retry_file).await?;
    assert_eq!(
        contents, "R",
        "retry-stable API start executed the job more than once"
    );
    Ok(())
}

fn api_shell_job(name: &str, shell: impl Into<String>) -> SessionJobStartSpecInput {
    SessionJobStartSpecInput {
        name: Some(name.to_owned()),
        job_id: None,
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), shell.into()],
        cwd: None,
        env: BTreeMap::new(),
        stdin: None,
        timeout_ms: Some(10_000),
        depends_on: Vec::new(),
        dependency_policy: SessionJobDependencyPolicyView::AllSucceeded,
        queue_key: None,
    }
}

async fn wait_for_session_jobs_terminal(
    api: &GatewayAgentApi,
    session_id: &engine::SessionId,
    handles: &[SessionJobHandleView],
    timeout: Duration,
) -> anyhow::Result<Vec<SessionJobReadEntryView>> {
    let started = Instant::now();
    loop {
        let read = api
            .read_session_jobs(SessionJobReadParams {
                session_id: session_id.as_str().to_owned(),
                jobs: handles.iter().map(session_job_handle_input).collect(),
                output_bytes: Some(4096),
                after_seq: None,
                include_artifacts: false,
            })
            .await?;
        if read.result.jobs.len() != handles.len() {
            anyhow::bail!(
                "session/jobs/read returned {} entries for {} handles",
                read.result.jobs.len(),
                handles.len()
            );
        }
        for entry in &read.result.jobs {
            if let Some(error) = entry.error.as_deref() {
                anyhow::bail!("session/jobs/read returned entry error: {error}");
            }
        }
        if read.result.jobs.iter().all(|entry| {
            entry
                .summary
                .as_ref()
                .is_some_and(|summary| is_terminal_job_status(summary.status))
        }) {
            return Ok(read.result.jobs);
        }
        if started.elapsed() > timeout {
            anyhow::bail!(
                "session jobs did not reach terminal status within {:?}: {:?}",
                timeout,
                job_status_debug(&read.result.jobs)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn session_job_handle_input(handle: &SessionJobHandleView) -> SessionJobHandleInput {
    SessionJobHandleInput {
        session_id: Some(handle.session_id.clone()),
        env_id: Some(handle.env_id.clone()),
        job_id: handle.job_id.clone(),
    }
}

fn is_terminal_job_status(status: SessionJobStatusView) -> bool {
    matches!(
        status,
        SessionJobStatusView::Succeeded
            | SessionJobStatusView::Failed
            | SessionJobStatusView::Cancelled
            | SessionJobStatusView::TimedOut
            | SessionJobStatusView::DependencyFailed
            | SessionJobStatusView::Interrupted
            | SessionJobStatusView::Lost
    )
}

fn ensure_job_statuses(
    entries: &[SessionJobReadEntryView],
    expected: SessionJobStatusView,
    label: &str,
) -> anyhow::Result<()> {
    let statuses = job_status_debug(entries);
    if entries.iter().all(|entry| {
        entry
            .summary
            .as_ref()
            .is_some_and(|summary| summary.status == expected)
    }) {
        return Ok(());
    }
    anyhow::bail!("{label} did not all finish as {expected:?}: {statuses:?}")
}

fn session_job_output_text(entry: &SessionJobReadEntryView) -> String {
    entry
        .output_chunks
        .iter()
        .filter_map(|chunk| BASE64_STANDARD.decode(&chunk.data_base64).ok())
        .filter_map(|bytes| String::from_utf8(bytes).ok())
        .collect::<Vec<_>>()
        .join("")
}

fn job_status_debug(entries: &[SessionJobReadEntryView]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry.summary.as_ref() {
            Some(summary) => format!("{}:{:?}", summary.job_id, summary.status),
            None => format!("missing-summary:{:?}", entry.error),
        })
        .collect()
}

fn line_index(lines: &[&str], expected: &str) -> anyhow::Result<usize> {
    lines
        .iter()
        .position(|line| *line == expected)
        .ok_or_else(|| anyhow::anyhow!("missing {expected:?} in {lines:?}"))
}

async fn run_fake_provider_client(
    client: Client,
    task_queue: String,
    session_id: engine::SessionId,
    provider: FakeHostProvider,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = fake_model();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(32)
        .build();
    let provider_id = format!("fake-provider-{}", uuid::Uuid::new_v4().simple());

    let registered = api
        .register_environment_provider(EnvironmentProviderRegisterParams {
            provider_id: provider_id.clone(),
            provider_kind: EnvironmentProviderKindView::Bridge,
            controller_connection: HostControllerConnectionView {
                endpoint: provider.endpoint().to_owned(),
                transport: HostTransportView::WebSocket,
            },
            capabilities: EnvironmentProviderCapabilitiesView::default(),
            implementation: EnvironmentProviderImplementationView {
                name: "client-supplied-placeholder".to_owned(),
                version: None,
            },
            lease_ttl_ms: 60_000,
            display_name: Some("fake host provider".to_owned()),
            metadata: BTreeMap::new(),
        })
        .await?;
    assert!(registered.result.provider.capabilities.create_target);
    assert!(registered.result.provider.capabilities.attach_target);
    assert_eq!(
        registered.result.provider.implementation.name,
        "fake-host-provider"
    );
    assert_eq!(provider.controller_initialize_count(), 1);

    let heartbeat = api
        .heartbeat_environment_provider(EnvironmentProviderHeartbeatParams {
            provider_id: provider_id.clone(),
            lease_ttl_ms: None,
            observed_targets: Vec::new(),
        })
        .await?;
    assert_eq!(heartbeat.result.targets.len(), 1);
    assert_eq!(heartbeat.result.targets[0].target_id, ATTACH_TARGET_ID);
    assert_eq!(provider.list_targets_count(), 1);

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(api_projection::model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
        profile: None,
    })
    .await?;

    let attached = api
        .attach_session_environment(SessionEnvironmentAttachParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-env".to_owned()),
            provider_id: provider_id.clone(),
            request: HostTargetAttachRequestView::Target {
                target_id: ATTACH_TARGET_ID.to_owned(),
            },
            activate: true,
        })
        .await?;
    assert_eq!(attached.result.active_env_id.as_deref(), Some("bridge-env"));
    assert_eq!(provider.attach_count(), 1);

    let first = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    text: "run a command in the attached provider target".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let first_run =
        support::live::wait_for_terminal_run(&api, &session_id, &first.result.run.id).await?;
    assert_eq!(
        first_run.status,
        RunStatus::Completed,
        "first run did not complete: {first_run:#?}"
    );
    let Some(first_text) = final_assistant_text(&first_run) else {
        anyhow::bail!("first run missing final assistant message: {first_run:#?}");
    };
    assert!(first_text.contains(PROCESS_STDOUT));

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "bridge-env".to_owned(),
        force: false,
        close_target: Some(false),
    })
    .await?;
    assert_eq!(
        provider.close_count(),
        0,
        "bridge detach should not close target when close_target=false"
    );

    let created = api
        .create_session_environment(SessionEnvironmentCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("sandbox-env".to_owned()),
            provider_id: provider_id.clone(),
            request: HostTargetCreateRequestView::Sandbox {
                spec: SandboxTargetSpecView {
                    image: Some("fake-image".to_owned()),
                    cwd: Some("/workspace".to_owned()),
                    ..SandboxTargetSpecView::default()
                },
            },
            activate: true,
        })
        .await?;
    assert_eq!(created.result.active_env_id.as_deref(), Some("sandbox-env"));
    assert_eq!(provider.create_count(), 1);

    let second = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    text: "run a command in the created provider target".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let second_run =
        support::live::wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    assert_eq!(
        second_run.status,
        RunStatus::Completed,
        "second run did not complete: {second_run:#?}"
    );
    let Some(second_text) = final_assistant_text(&second_run) else {
        anyhow::bail!("second run missing final assistant message: {second_run:#?}");
    };
    assert!(second_text.contains(PROCESS_STDOUT));

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "sandbox-env".to_owned(),
        force: false,
        close_target: None,
    })
    .await?;
    assert_eq!(provider.close_count(), 1);
    assert_eq!(provider.process_start_count(), 2);
    assert_eq!(
        provider.process_cwds(),
        vec![Some("/workspace".to_owned()), Some("/workspace".to_owned())]
    );

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("fake provider live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_profile_environment_client(
    client: Client,
    task_queue: String,
    session_id: engine::SessionId,
    provider: FakeHostProvider,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = fake_model();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(32)
        .build();
    let provider_id = format!("profile-provider-{}", uuid::Uuid::new_v4().simple());
    let profile_id = ProfileId::new(format!("profile_env_{}", uuid::Uuid::new_v4().simple()));

    api.register_environment_provider(EnvironmentProviderRegisterParams {
        provider_id: provider_id.clone(),
        provider_kind: EnvironmentProviderKindView::Bridge,
        controller_connection: HostControllerConnectionView {
            endpoint: provider.endpoint().to_owned(),
            transport: HostTransportView::WebSocket,
        },
        capabilities: EnvironmentProviderCapabilitiesView::default(),
        implementation: EnvironmentProviderImplementationView {
            name: "client-supplied-placeholder".to_owned(),
            version: None,
        },
        lease_ttl_ms: 60_000,
        display_name: Some("profile fake host provider".to_owned()),
        metadata: BTreeMap::new(),
    })
    .await?;

    api.heartbeat_environment_provider(EnvironmentProviderHeartbeatParams {
        provider_id: provider_id.clone(),
        lease_ttl_ms: None,
        observed_targets: Vec::new(),
    })
    .await?;

    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: Some("Profile environment".to_owned()),
            description: Some("Attach fake host provider target".to_owned()),
            document: ProfileDocument {
                config: Some(SessionConfigInput {
                    model: Some(api_projection::model_to_api(&model)),
                    ..SessionConfigInput::default()
                }),
                instructions: None,
                mounts: Vec::new(),
                mcp: Vec::new(),
                environments: vec![ProfileEnvironment {
                    env_id: "profile-env".to_owned(),
                    provider_id: provider_id.clone(),
                    target_id: ATTACH_TARGET_ID.to_owned(),
                    activate: true,
                }],
            },
        },
    })
    .await?;

    let started = api
        .start_session(SessionStartParams {
            session_id: Some(session_id.as_str().to_owned()),
            cwd: None,
            config: None,
            profile: Some(ProfileSource::Named {
                profile_id: profile_id.clone(),
            }),
        })
        .await?;
    assert_eq!(started.result.session.id, session_id.as_str());
    assert_eq!(provider.attach_count(), 1);

    let environments = api
        .list_session_environments(SessionEnvironmentListParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?;
    assert_eq!(
        environments.result.active_env_id.as_deref(),
        Some("profile-env")
    );
    assert_eq!(environments.result.environments.len(), 1);

    let run = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    text: "run a command in the profile attached provider target".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = support::live::wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    assert_eq!(
        run.status,
        RunStatus::Completed,
        "profile environment run did not complete: {run:#?}"
    );
    let Some(text) = final_assistant_text(&run) else {
        anyhow::bail!("profile environment run missing final assistant message: {run:#?}");
    };
    assert!(text.contains(PROCESS_STDOUT));

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "profile-env".to_owned(),
        force: false,
        close_target: Some(false),
    })
    .await?;
    api.delete_profile(ProfileDeleteParams { profile_id })
        .await?;

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("profile environment live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

fn fake_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "fake".to_owned(),
        model: "fake-env-tool-model".to_owned(),
    }
}

struct ExecCommandLlm {
    blobs: Arc<dyn BlobStore>,
}

impl ExecCommandLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "exec_command")
        {
            return Err(io_error("planned request did not expose exec_command"));
        }
        let arguments = json!({
            "argv": ["fake-provider-command"],
            "yield_time_ms": 1,
            "max_output_bytes": 4096
        });
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("env_call_{}_{}", request.run_id, request.turn_id));
        let tool_name = ToolName::new("exec_command");
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
                preview: Some(format!("exec_command({arguments})")),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-tool-{}", request.turn_id)),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fake".to_owned()),
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
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let tool_output = if let Some(entry) = current_run_tool_result(request) {
            self.blobs
                .read_text(&entry.content_ref)
                .await
                .map_err(io_error)?
        } else {
            "no tool result".to_owned()
        };
        let text = format!("Fake provider run completed with output:\n{tool_output}");
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
                preview: Some("fake provider final answer".to_owned()),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-final-{}", request.turn_id)),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[async_trait]
impl CoreAgentLlm for ExecCommandLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if current_run_tool_result(&request).is_some() {
            self.final_result(&request).await
        } else {
            self.tool_call_result(&request).await
        }
    }
}

struct BridgeFileLlm {
    blobs: Arc<dyn BlobStore>,
}

impl BridgeFileLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn exec_write_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "exec_command")
        {
            return Err(io_error("planned request did not expose exec_command"));
        }
        let command = format!(
            "printf '{} from exec_command\\n' > {} && printf 'wrote {}\\n'",
            BRIDGE_FILE_MARKER, BRIDGE_FILE_NAME, BRIDGE_FILE_NAME
        );
        self.tool_call_result(
            request,
            "exec_command",
            json!({
                "argv": ["/bin/sh", "-c", command],
                "timeout_ms": 5000,
                "max_output_bytes": 4096
            }),
            "bridge_exec_write",
        )
        .await
    }

    async fn read_file_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "read_file")
        {
            return Err(io_error("planned request did not expose read_file"));
        }
        self.tool_call_result(
            request,
            "read_file",
            json!({
                "path": BRIDGE_FILE_NAME,
                "offset": 1,
                "limit": 20
            }),
            "bridge_read_file",
        )
        .await
    }

    async fn read_vfs_skill_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "read_file")
        {
            return Err(io_error("planned request did not expose read_file"));
        }
        self.tool_call_result(
            request,
            "read_file",
            json!({
                "path": "/skills/SKILL.md",
                "offset": 1,
                "limit": 20
            }),
            "bridge_read_vfs_skill",
        )
        .await
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
        tool_name: &str,
        arguments: Value,
        label: &str,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("{label}_{}_{}", request.run_id, request.turn_id));
        let tool_name = ToolName::new(tool_name);
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
                preview: Some(format!("{}({arguments})", tool_name.as_str())),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-{label}-{}", request.turn_id)),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fake".to_owned()),
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
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mut text = String::from("Host bridge local filesystem test completed.\n");
        for entry in current_run_tool_results(request) {
            let output = self
                .blobs
                .read_text(&entry.content_ref)
                .await
                .map_err(io_error)?;
            text.push_str("\n--- tool result ---\n");
            text.push_str(&output);
        }
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
                preview: Some("host bridge final answer".to_owned()),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-host-bridge-final-{}", request.turn_id)),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[async_trait]
impl CoreAgentLlm for BridgeFileLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        match current_run_tool_results(&request).len() {
            0 => self.exec_write_result(&request).await,
            1 => self.read_file_result(&request).await,
            2 => self.read_vfs_skill_result(&request).await,
            _ => self.final_result(&request).await,
        }
    }
}

struct BridgeJobsLlm {
    blobs: Arc<dyn BlobStore>,
}

impl BridgeJobsLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn start_job_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.require_tool(request, "job_start")?;
        let command = format!(
            "printf '{}\\n' > {} && printf '{}\\n'",
            BRIDGE_JOB_MARKER, BRIDGE_JOB_FILE_NAME, BRIDGE_JOB_MARKER
        );
        self.tool_call_result(
            request,
            "job_start",
            json!({
                "jobs": [{
                    "name": "live-job",
                    "argv": ["/bin/sh", "-c", command],
                    "timeout_ms": 10000
                }]
            }),
            "bridge_job_start",
        )
        .await
    }

    async fn list_jobs_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.require_tool(request, "job_list")?;
        let handle = self.job_handle_from_results(request).await?;
        self.tool_call_result(
            request,
            "job_list",
            json!({
                "session_id": handle.session_id,
                "limit": 10
            }),
            "bridge_job_list",
        )
        .await
    }

    async fn wait_job_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.require_tool(request, "job_wait")?;
        let handle = self.job_handle_from_results(request).await?;
        self.tool_call_result(
            request,
            "job_wait",
            json!({
                "jobs": [handle.json_arg()],
                "timeout_ms": 15000,
                "output_bytes": 4096
            }),
            "bridge_job_wait",
        )
        .await
    }

    async fn read_job_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.require_tool(request, "job_read")?;
        let handle = self.job_handle_from_results(request).await?;
        self.tool_call_result(
            request,
            "job_read",
            json!({
                "jobs": [handle.json_arg()],
                "output_bytes": 4096
            }),
            "bridge_job_read",
        )
        .await
    }

    fn require_tool(
        &self,
        request: &LlmGenerationRequest,
        name: &str,
    ) -> Result<(), CoreAgentIoError> {
        if request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == name)
        {
            return Ok(());
        }
        Err(io_error(format!("planned request did not expose {name}")))
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
        tool_name: &str,
        arguments: Value,
        label: &str,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("{label}_{}_{}", request.run_id, request.turn_id));
        let tool_name = ToolName::new(tool_name);
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
                preview: Some(format!("{}({arguments})", tool_name.as_str())),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-{label}-{}", request.turn_id)),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fake".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
                context_token_estimate: None,
            },
        })
    }

    async fn job_handle_from_results(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<BridgeJobHandle, CoreAgentIoError> {
        for entry in current_run_tool_results(request).into_iter().rev() {
            let output = self
                .blobs
                .read_text(&entry.content_ref)
                .await
                .map_err(io_error)?;
            for line in output.lines() {
                if let Some(handle) = BridgeJobHandle::parse(line) {
                    return Ok(handle);
                }
            }
        }
        Err(io_error("job_start result did not include a job handle"))
    }

    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mut text = String::from("Host bridge durable job test completed.\n");
        for entry in current_run_tool_results(request) {
            let output = self
                .blobs
                .read_text(&entry.content_ref)
                .await
                .map_err(io_error)?;
            text.push_str("\n--- tool result ---\n");
            text.push_str(&output);
        }
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
                preview: Some("host bridge jobs final answer".to_owned()),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!(
                    "fake-host-bridge-jobs-final-{}",
                    request.turn_id
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
impl CoreAgentLlm for BridgeJobsLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        match current_run_tool_results(&request).len() {
            0 => self.start_job_result(&request).await,
            1 => self.list_jobs_result(&request).await,
            2 => self.wait_job_result(&request).await,
            3 => self.read_job_result(&request).await,
            _ => self.final_result(&request).await,
        }
    }
}

struct BridgeJobHandle {
    session_id: String,
    env_id: String,
    job_id: String,
}

impl BridgeJobHandle {
    fn parse(line: &str) -> Option<Self> {
        let (handle, _) = line.split_once(':')?;
        let mut parts = handle.trim().split('/');
        let session_id = parts.next()?.to_owned();
        let env_id = parts.next()?.to_owned();
        let job_id = parts.next()?.to_owned();
        if parts.next().is_some() || session_id.is_empty() || env_id.is_empty() || job_id.is_empty()
        {
            return None;
        }
        Some(Self {
            session_id,
            env_id,
            job_id,
        })
    }

    fn json_arg(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "env_id": self.env_id,
            "job_id": self.job_id
        })
    }
}

fn current_run_tool_result(request: &LlmGenerationRequest) -> Option<&engine::ContextEntry> {
    current_run_tool_results(request).into_iter().next()
}

fn current_run_tool_results(request: &LlmGenerationRequest) -> Vec<&engine::ContextEntry> {
    request
        .request
        .context
        .entries
        .iter()
        .rev()
        .filter(|entry| {
            matches!(
                (&entry.source, &entry.kind),
                (
                    ContextEntrySource::Tool { run_id, .. },
                    ContextEntryKind::ToolResult { .. }
                ) if *run_id == request.run_id
            )
        })
        .collect()
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

struct SpawnedBridge {
    child: Child,
}

impl SpawnedBridge {
    fn start(
        bridge_bin: &PathBuf,
        gateway_url: &str,
        provider_id: &str,
        root: &PathBuf,
    ) -> anyhow::Result<Self> {
        let child = Command::new(bridge_bin)
            .arg("--gateway-url")
            .arg(gateway_url)
            .arg("--provider-id")
            .arg(provider_id)
            .arg("--target-id")
            .arg("local")
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--cwd")
            .arg(root)
            .arg("--fs-root")
            .arg(root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                anyhow::anyhow!("spawn host-bridge binary {}: {error}", bridge_bin.display())
            })?;
        Ok(Self { child })
    }
}

impl Drop for SpawnedBridge {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn wait_for_bridge_attach(
    api: &GatewayAgentApi,
    session_id: &engine::SessionId,
    provider_id: &str,
    env_id: &str,
) -> anyhow::Result<api::AgentApiOutcome<api::SessionEnvironmentAttachResponse>> {
    let started = Instant::now();
    let mut last_error = None;
    loop {
        if started.elapsed() > Duration::from_secs(30) {
            anyhow::bail!(
                "timed out waiting to attach host bridge provider {provider_id}; last error: {}",
                last_error.unwrap_or_else(|| "none".to_owned())
            );
        }
        match api
            .attach_session_environment(SessionEnvironmentAttachParams {
                session_id: session_id.as_str().to_owned(),
                env_id: Some(env_id.to_owned()),
                provider_id: provider_id.to_owned(),
                request: HostTargetAttachRequestView::Target {
                    target_id: "local".to_owned(),
                },
                activate: true,
            })
            .await
        {
            Ok(response) => return Ok(response),
            Err(error) => {
                last_error = Some(error.to_string());
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

fn host_bridge_binary_path() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("HOST_BRIDGE_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("HOST_BRIDGE_BIN does not exist: {}", path.display());
    }

    let current_exe = std::env::current_exe()?;
    let target_dir = current_exe
        .parent()
        .and_then(|deps| deps.parent())
        .ok_or_else(|| anyhow::anyhow!("cannot infer target dir from {}", current_exe.display()))?;
    let binary = target_dir.join("host-bridge");
    if binary.exists() {
        return Ok(binary);
    }
    anyhow::bail!(
        "host-bridge binary not found at {}; run `cargo build -p host-bridge` or set HOST_BRIDGE_BIN",
        binary.display()
    );
}

fn path_str(path: &std::path::Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

struct FakeHostProvider {
    endpoint: String,
    state: Arc<FakeHostProviderState>,
    server: JoinHandle<()>,
}

impl FakeHostProvider {
    async fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let state = Arc::new(FakeHostProviderState::default());
        let server_state = state.clone();
        let server_endpoint = endpoint.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(handle_connection(
                    stream,
                    server_state.clone(),
                    server_endpoint.clone(),
                ));
            }
        });
        Ok(Self {
            endpoint,
            state,
            server,
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn controller_initialize_count(&self) -> usize {
        self.state
            .controller_initialize_count
            .load(Ordering::SeqCst)
    }

    fn list_targets_count(&self) -> usize {
        self.state.list_targets_count.load(Ordering::SeqCst)
    }

    fn attach_count(&self) -> usize {
        self.state.attach_count.load(Ordering::SeqCst)
    }

    fn create_count(&self) -> usize {
        self.state.create_count.load(Ordering::SeqCst)
    }

    fn close_count(&self) -> usize {
        self.state.close_count.load(Ordering::SeqCst)
    }

    fn process_start_count(&self) -> usize {
        self.state
            .process_starts
            .lock()
            .expect("process starts")
            .len()
    }

    fn process_cwds(&self) -> Vec<Option<String>> {
        self.state
            .process_starts
            .lock()
            .expect("process starts")
            .iter()
            .map(|params| params.cwd.as_ref().map(|cwd| cwd.as_str().to_owned()))
            .collect()
    }
}

impl Drop for FakeHostProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Default)]
struct FakeHostProviderState {
    controller_initialize_count: AtomicUsize,
    list_targets_count: AtomicUsize,
    attach_count: AtomicUsize,
    create_count: AtomicUsize,
    close_count: AtomicUsize,
    process_starts: Mutex<Vec<StartProcessParams>>,
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: Arc<FakeHostProviderState>,
    endpoint: String,
) {
    let Ok(mut socket) = accept_async(stream).await else {
        return;
    };
    while let Some(message) = socket.next().await {
        let Ok(message) = message else {
            return;
        };
        let Ok(value) = websocket_json(message) else {
            continue;
        };
        let Some(id) = value.get("id").cloned() else {
            if value.get("method").and_then(Value::as_str) == Some(INITIALIZED_METHOD) {
                let _ = serde_json::from_value::<InitializedParams>(
                    value.get("params").cloned().unwrap_or(Value::Null),
                );
            }
            continue;
        };
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let response = match handle_request(method, params, state.as_ref(), &endpoint).await {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": "internal",
                    "message": message
                }
            }),
        };
        if socket
            .send(Message::Text(response.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn websocket_json(message: Message) -> anyhow::Result<Value> {
    match message {
        Message::Text(text) => Ok(serde_json::from_str(&text)?),
        Message::Binary(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Message::Close(_) => anyhow::bail!("websocket closed"),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
            anyhow::bail!("control frame")
        }
    }
}

async fn handle_request(
    method: &str,
    params: Value,
    state: &FakeHostProviderState,
    endpoint: &str,
) -> Result<Value, String> {
    match method {
        CONTROL_INITIALIZE_METHOD => {
            state
                .controller_initialize_count
                .fetch_add(1, Ordering::SeqCst);
            result_value(ControllerInitializeResponse {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                capabilities: ControllerCapabilities {
                    list_targets: true,
                    create_target: true,
                    attach_target: true,
                    get_target: true,
                    close_target: true,
                },
                implementation: ImplementationInfo {
                    name: "fake-host-provider".to_owned(),
                    version: Some("test".to_owned()),
                },
            })
        }
        LIST_TARGETS_METHOD => {
            state.list_targets_count.fetch_add(1, Ordering::SeqCst);
            result_value(ListTargetsResponse {
                targets: vec![target_summary(ATTACH_TARGET_ID)],
            })
        }
        ATTACH_TARGET_METHOD => {
            state.attach_count.fetch_add(1, Ordering::SeqCst);
            result_value(AttachTargetResponse {
                target: target_summary(ATTACH_TARGET_ID),
                connection: connection_spec(endpoint, ATTACH_TARGET_ID),
            })
        }
        CREATE_TARGET_METHOD => {
            state.create_count.fetch_add(1, Ordering::SeqCst);
            result_value(CreateTargetResponse {
                target: target_summary(CREATED_TARGET_ID),
                connection: connection_spec(endpoint, CREATED_TARGET_ID),
            })
        }
        CLOSE_TARGET_METHOD => {
            state.close_count.fetch_add(1, Ordering::SeqCst);
            result_value(CloseTargetResponse {
                target_id: HostTargetId::new(
                    params
                        .get("targetId")
                        .and_then(Value::as_str)
                        .unwrap_or(CREATED_TARGET_ID),
                ),
                status: HostTargetStatus::Closed,
            })
        }
        DATA_INITIALIZE_METHOD => result_value(InitializeResponse {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            connection_id: HostConnectionId::new("fake-data-connection"),
            capabilities: host_capabilities(),
            default_cwd: Some("/workspace".to_owned()),
            implementation: ImplementationInfo {
                name: "fake-host-data".to_owned(),
                version: Some("test".to_owned()),
            },
        }),
        PROCESS_START_METHOD => {
            let params: StartProcessParams =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            let process_id = params.process_id.clone();
            state
                .process_starts
                .lock()
                .map_err(|error| error.to_string())?
                .push(params);
            result_value(StartProcessResponse { process_id })
        }
        PROCESS_READ_METHOD => result_value(ReadProcessResponse {
            chunks: vec![ProcessOutputChunk {
                seq: 1,
                stream: ProcessOutputStream::Stdout,
                chunk: ByteChunk::new(PROCESS_STDOUT.as_bytes()),
            }],
            next_seq: 2,
            exited: true,
            exit_code: Some(0),
            closed: true,
            failure: None,
        }),
        other => Err(format!("unsupported fake host method: {other}")),
    }
}

fn result_value(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn target_summary(target_id: &str) -> HostTargetSummary {
    HostTargetSummary {
        target_id: HostTargetId::new(target_id),
        display_name: Some(target_id.to_owned()),
        status: HostTargetStatus::Ready,
        scope: HostScope::Default,
        capabilities: host_capabilities(),
        default_cwd: Some(HostPath::new("/workspace").expect("host cwd")),
        metadata: BTreeMap::new(),
    }
}

fn connection_spec(endpoint: &str, target_id: &str) -> HostConnectionSpec {
    HostConnectionSpec {
        target_id: HostTargetId::new(target_id),
        endpoint: endpoint.to_owned(),
        transport: HostTransport::WebSocket,
        scope: HostScope::Default,
        default_cwd: Some(HostPath::new("/workspace").expect("host cwd")),
        capabilities: host_capabilities(),
    }
}

fn host_capabilities() -> HostCapabilities {
    HostCapabilities {
        filesystem_read: true,
        filesystem_write: true,
        process_start: true,
        process_stdin: true,
        process_terminate: true,
        process_output_polling: true,
        process_output_notifications: false,
        process_pty: false,
        job_start: true,
        job_list: true,
        job_read: true,
        job_cancel: true,
        job_wait_hint: false,
        job_dependencies: true,
        job_queue_keys: true,
    }
}
