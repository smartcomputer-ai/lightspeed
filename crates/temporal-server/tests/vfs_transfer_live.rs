//! Hosted transfers through Temporal, PostgreSQL/MinIO and a registered envd.
//! The model is scripted; admission, provider tool naming, dispatch, gateway
//! routing, daemon operations and workspace publication use production code.

mod support;

use std::{path::Path, sync::Arc, time::Duration};

use api::{AgentApiService, OperatorApiService};
use async_trait::async_trait;
use engine::{
    BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError,
    CoreAgentLlm, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ObservedToolCall, ProviderApiKind, SessionId, ToolCallId, ToolName,
    storage::BlobStore,
};
use environment_daemon::{
    DaemonRuntime,
    config::{DaemonConfig, RegistrationConfig},
};
use environment_protocol::{
    data::{
        handshake::InitializeParams,
        inventory::{ScanContent, ScanDigestAlgorithm, ScanParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, EnvironmentPath, SecretString},
};
use environments::EnvironmentStore;
use serde_json::json;
use support::live::{
    LIVE_TEST_LOCK, require_storage_live_env, run_with_live_worker, wait_for_terminal_run,
};
use temporal_server::{
    DeploymentStores, GatewayAuthMode, UniverseRuntime,
    gateway::{
        DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayAgentApi, GatewayOperatorApi, GatewayRoutes,
        GatewayState, gateway_router,
    },
    worker::{ActivityState, SessionTools, WorkerActivities},
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use temporalio_client::WorkflowTerminateOptions;
use uuid::Uuid;

const FILE_SIZE: usize = 10 * 1024 * 1024 + 13;

struct Tasks(Vec<tokio::task::AbortHandle>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres + MinIO env; Linux or macOS"]
async fn temporal_live_vfs_transfers_follow_profile_grants_and_publish_large_files()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    anyhow::ensure!(
        cfg!(any(target_os = "linux", target_os = "macos")),
        "transfer backend requires Linux or macOS"
    );
    let client = connect_temporal(
        &std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.into()),
        &std::env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.into()),
    )
    .await?;
    let stores = DeploymentStores::from_env().await?;
    let universe_id = Uuid::new_v4();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let base_url = format!("http://{address}");
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        format!("transfer-live-{universe_id}"),
        Some(base_url.clone()),
        stores,
    )?);
    GatewayOperatorApi::new(runtime.clone())
        .create_universe(api::OperatorUniverseCreateParams {
            universe_id: universe_id.to_string(),
        })
        .await?;
    // The sourced development URL names the normal gateway. This fixture
    // binds an ephemeral port, while retaining the deployment's route token.
    let gateway_config = temporal_server::environment_gateway::EnvironmentGatewayClientConfig::new(
        &base_url,
        runtime.environment_gateway().deployment_token(),
    );
    let state = Arc::new(GatewayState::multi(
        GatewayAuthMode::Single { universe_id },
        runtime.clone(),
        base_url,
    ));
    let gateway = tokio::spawn(async move {
        axum::serve(
            listener,
            gateway_router(state, DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayRoutes::ALL),
        )
        .await
    });
    let mut tasks = Tasks(vec![gateway.abort_handle()]);
    let sandbox = tempfile::tempdir()?;
    let root = sandbox.path().canonicalize()?;
    let result = async {
        let state = runtime.state_for(universe_id, false).await?;
        let key = state.api.create_environment_registration_key(api::EnvironmentRegistrationKeyCreateParams {
            display_name: "VFS transfer live".into(),
            identity_mode: api::EnvironmentIdentityModeView::Ephemeral,
            max_active_environments: Some(1),
            ephemeral_disconnect_grace_ms: None,
            expires_at_ms: None,
        }).await?.result;
        let connect_url = format!("ws://{address}/environment-gateway/connect");
        let mut registration = RegistrationConfig::new(connect_url.clone(),
            environment_daemon::upgrade::resolve_discovery_url(Some(&connect_url), None)?);
        registration.registration_key = Some(SecretString::new(key.secret.0));
        let daemon = DaemonRuntime::new(DaemonConfig {
            listen: None, cwd: root.clone(), fs_root: root.clone(), state_dir: root.join(".envd"),
            read_only_fs: false, registration: Some(registration), scrubbed_env: Vec::new(),
        })?;
        let daemon = tokio::spawn(environment_daemon::server::run(daemon));
        tasks.0.push(daemon.abort_handle());
        let environment = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let environments = state.api.list_environments(api::EnvironmentListParams::default()).await?.result.environments;
                if let Some(environment) = environments.into_iter().find(|e| e.status == api::EnvironmentLifecycleStatusView::Ready) {
                    return anyhow::Ok(environment);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await??;
        let hosted = Arc::new(SessionTools::from_pg_store(state.store.clone())
            .with_environment_gateway(gateway_config.clone()));
        let activities = WorkerActivities::for_universe(universe_id,
            ActivityState::from_pg_store(state.store.clone(), Arc::new(TransferLlm { blobs: state.store.clone() }), hosted.clone())
                .with_hosted_tools(hosted).with_environment_gateway(gateway_config.clone()));
        let gateway_config = gateway_config.clone();
        let store = state.store.clone();
        run_with_live_worker(activities, |client, queue, base_session| async move {
            let api = GatewayAgentApi::builder(client.clone(), store.clone())
                .with_task_queue(queue).with_environment_gateway(gateway_config.clone()).build();
            // All provider surfaces execute both directions. Additional sessions
            // test the public grant boundary using the same profile path.
            for (index, (provider, mode)) in [
                (ProviderApiKind::OpenAiResponses, "edit"),
                (ProviderApiKind::AnthropicMessages, "edit"),
                (ProviderApiKind::OpenAiCompletions, "edit"),
                (ProviderApiKind::OpenAiResponses, "readonly"),
                (ProviderApiKind::OpenAiResponses, "sourcing"),
                (ProviderApiKind::OpenAiResponses, "noenv"),
            ].into_iter().enumerate() {
                let session = SessionId::new(format!("{base_session}_{index}_{mode}"));
                let case = run_case(&api, store.as_ref(), &session, provider, mode, index, &root, &environment.environment_id).await;
                let handle = client.get_workflow_handle::<temporal_workflow::AgentSessionWorkflow>(
                    temporal_workflow::compose_workflow_id(universe_id, &session));
                let _ = handle.terminate(WorkflowTerminateOptions::builder().reason("transfer live cleanup").build()).await;
                case?;
            }
            // Scan negotiation and conditional observations use the same reverse route.
            let record = store.read_environment(&engine::EnvironmentId::new(&environment.environment_id)).await?;
            let connection = gateway_config.connection_for(universe_id, &record);
            let mut remote = environment_client::EnvironmentDataClient::connect(&connection.endpoint, gateway_config.connect_options("transfer-live-scan")).await?;
            let initialized = remote.initialize(&InitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION, client_name: "transfer-live-scan".into(),
                scope: connection.scope, resume_connection_id: None,
            }).await?;
            anyhow::ensure!(initialized.capabilities.filesystem_scan && initialized.capabilities.filesystem_transfer);
            let mut query = ScanParams {
            follow_symlinks: false,
                roots: vec![EnvironmentPath::new("./capture-source")?], include_patterns: vec!["new.bin".into()],
                read_content: false, digest_algorithm: Some(ScanDigestAlgorithm::Sha256), limits: Default::default(), if_none_match: None,
            };
            let scan = remote.scan(&query).await?;
            anyhow::ensure!(scan.complete && scan.diagnostics.is_empty());
            assert!(scan.entries.iter().any(|entry| matches!(&entry.content, ScanContent::File { digest: Some(digest), .. }
                if digest == BlobRef::from_bytes(&std::fs::read(root.join("capture-source/new.bin")).unwrap()).as_str())));
            query.if_none_match = Some(scan.fingerprint.expect("complete scan fingerprint"));
            let unchanged = remote.scan(&query).await?;
            anyhow::ensure!(unchanged.unchanged && unchanged.entries.is_empty());
            std::fs::write(root.join("capture-source/new.bin"), b"changed after observation")?;
            let changed = remote.scan(&query).await?;
            anyhow::ensure!(changed.complete && !changed.unchanged && changed.fingerprint != query.if_none_match);
            remote.close().await?;
            anyhow::Ok(())
        }).await
    }.await;
    drop(tasks);
    let store = runtime.state_for(universe_id, false).await?.store.clone();
    runtime.evict(universe_id).await;
    cleanup_fixture(&store).await?;
    result
}

async fn cleanup_fixture(store: &store_pg::PgStore) -> anyhow::Result<()> {
    let universe_id = store.config().universe_id;
    let object_keys = store_pg::list_universe_object_keys(store.pool(), universe_id).await?;
    // Remove restrictive holders before the test universe's cascading delete.
    let mut tx = store.pool().begin().await?;
    for query in [
        "DELETE FROM environments WHERE universe_id=$1",
        "DELETE FROM vfs_workspaces WHERE universe_id=$1",
        "DELETE FROM vfs_snapshots WHERE universe_id=$1",
        "DELETE FROM session_checkpoints WHERE universe_id=$1",
        "DELETE FROM cas_blob_edges WHERE universe_id=$1",
        "DELETE FROM universes WHERE universe_id=$1",
    ] {
        sqlx::query(query)
            .bind(universe_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let cleanup = store.delete_blob_objects(&object_keys).await;
    anyhow::ensure!(
        cleanup.failures.is_empty(),
        "test object cleanup failed: {:?}",
        cleanup.failures
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_case(
    api: &GatewayAgentApi,
    store: &store_pg::PgStore,
    session: &SessionId,
    provider: ProviderApiKind,
    mode: &str,
    index: usize,
    root: &Path,
    environment: &str,
) -> anyhow::Result<()> {
    let original = vec![0x30 + index as u8; FILE_SIZE];
    let captured = vec![0x80 + index as u8; FILE_SIZE];
    if mode == "edit" {
        anyhow::ensure!(
            !store.has_blob(&BlobRef::from_bytes(&captured)).await?,
            "the first capture must transfer content absent from CAS"
        );
    }
    let mut files = vec![
        vfs::InlineFile::new("/input/data.bin", original.clone())?,
        vfs::InlineFile::new("/input/run.sh", b"#!/bin/sh\nprintf live".to_vec())?.executable(true),
        vfs::InlineFile::new("/keep.txt", b"keep VFS sibling".to_vec())?,
        vfs::InlineFile::new("/output/obsolete.txt", b"remove on capture".to_vec())?,
    ];
    for (name, body) in [
        ("010-first.txt", "VFS first"),
        ("020-second.md", "VFS second"),
        ("instructions.md", "VFS last"),
        ("nested/deep/hidden.md", "must not load"),
        ("ignore.json", "must not load"),
    ] {
        files.push(vfs::InlineFile::new(
            format!("/.agents/prompts/{name}"),
            body.as_bytes().to_vec(),
        )?);
    }
    // More than one inventory page, with repeated content.
    for i in 0..130 {
        files.push(vfs::InlineFile::new(
            format!("/input/page-{i:03}.txt"),
            b"page\n".to_vec(),
        )?);
    }
    let snapshot = vfs::create_inline_snapshot(
        store,
        Some(store),
        vfs::CreateInlineSnapshotRequest::new(files),
    )
    .await?;
    let workspace = api
        .create_vfs_workspace(api::VfsWorkspaceCreateParams {
            snapshot_ref: Some(snapshot.snapshot_ref.to_string()),
            ..Default::default()
        })
        .await?
        .result
        .workspace;
    std::fs::create_dir_all(root.join("tree"))?;
    std::fs::write(root.join("tree/obsolete.txt"), b"remove on materialize")?;
    std::fs::write(root.join("keep.txt"), b"keep environment sibling")?;
    std::fs::create_dir_all(root.join("capture-source"))?;
    std::fs::write(root.join("capture-source/shared.bin"), &original)?;
    std::fs::write(root.join("capture-source/new.bin"), &captured)?;
    let prompts = root.join(".agents/prompts");
    std::fs::create_dir_all(prompts.join("nested/deep"))?;
    for (name, body) in [
        ("010-first.txt", "Environment first"),
        ("020-second.md", "Environment second"),
        ("instructions.md", "Environment last"),
        ("nested/deep/hidden.md", "must not load"),
        ("ignore.json", "must not load"),
    ] {
        std::fs::write(prompts.join(name), body)?;
    }
    let mut features = json!({"vfs": {
        "workspaceLinks": [{"path":"/workspace","target":{"type":"workspace","workspaceId":workspace.workspace_id},"access":"readWrite"}]
    }});
    if mode != "sourcing" {
        features["vfs"]["tools"] = json!(if mode == "readonly" {
            "readOnly"
        } else {
            "edit"
        });
    } else {
        features["vfs"]["skills"] = json!({"roots": ["/workspace"]});
        features["vfs"]["prompts"] = json!({});
    }
    if mode != "noenv" {
        features["environments"] = if mode == "sourcing" {
            json!({})
        } else {
            json!({"tools":"edit"})
        };
    }
    if mode == "sourcing" {
        features["environments"]["workingDirectory"] = json!(root);
        features["environments"]["prompts"] = json!({"roots":[".agents/prompts"]});
    }
    let mut model = temporal_server::default_model_from_env();
    model.api_kind = provider;
    let profile = api.create_profile(api::ProfileCreateParams { profile: api::AgentProfileInput {
        profile_id: api::ProfileId::new(format!("profile_{session}")), display_name: None, description: None,
        document: api::ProfileDocument {
            config: Some(serde_json::from_value(json!({"model": api_projection::model_to_api(&model), "features": features}))?),
            environment: (mode != "noenv").then(|| api::ProfileEnvironment::Existing { environment_id: environment.into() }),
            ..Default::default()
        },
    }}).await?.result.profile;
    api.start_session(api::SessionStartParams {
        session_id: Some(session.to_string()),
        profile: Some(api::ProfileSource::Named {
            profile_id: profile.profile_id,
        }),
        ..Default::default()
    })
    .await?;
    let run = api
        .start_run(api::RunStartParams {
            session_id: session.to_string(),
            source: api::RunStartSource::Input {
                items: vec![api::InputItem::Text {
                    origin: None,
                    text: "run transfer checks".into(),
                }],
            },
            config: None,
            submission_id: None,
            notify_on_terminal: None,
        })
        .await?
        .result
        .run;
    let run = wait_for_terminal_run(api, session, &run.id).await?;
    anyhow::ensure!(
        run.status == api::RunStatus::Completed,
        "transfer run failed: {run:?}"
    );
    let calls: Vec<_> = run.tool_batches.iter().flat_map(|b| &b.calls).collect();
    assert_eq!(
        calls.len(),
        match mode {
            "edit" => 4,
            "readonly" => 2,
            _ => 0,
        }
    );
    for call in &calls {
        assert_eq!(call.status, api::ToolItemStatus::Succeeded, "{call:?}");
        assert_eq!(
            call.tool_id.as_deref(),
            Some(if call.tool_name == "vfs_capture" {
                "vfs.capture"
            } else {
                "vfs.materialize"
            })
        );
    }
    if matches!(mode, "edit" | "readonly") {
        assert_eq!(std::fs::read(root.join("tree/data.bin"))?, original);
        assert!(!root.join("tree/obsolete.txt").exists());
        assert_eq!(
            std::fs::read(root.join("keep.txt"))?,
            b"keep environment sibling"
        );
        assert!(
            calls[1]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("transferred 0 bytes")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(root.join("tree/run.sh"))?
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
    }
    let head = api
        .read_vfs_workspace(api::VfsWorkspaceReadParams {
            workspace_id: workspace.workspace_id,
        })
        .await?
        .result
        .workspace;
    let manifest =
        vfs::read_snapshot_manifest(store, &BlobRef::parse(&head.head_snapshot_ref)?).await?;
    assert_eq!(
        vfs::read_snapshot_file(store, &manifest, &vfs::VfsPath::parse("/keep.txt")?).await?,
        b"keep VFS sibling"
    );
    if mode == "edit" {
        assert_eq!(
            vfs::read_snapshot_file(store, &manifest, &vfs::VfsPath::parse("/output/new.bin")?)
                .await?,
            captured
        );
        assert_eq!(
            vfs::read_snapshot_file(
                store,
                &manifest,
                &vfs::VfsPath::parse("/output/shared.bin")?
            )
            .await?,
            original
        );
        assert!(
            vfs::read_snapshot_file(
                store,
                &manifest,
                &vfs::VfsPath::parse("/output/obsolete.txt")?
            )
            .await
            .is_err()
        );
        assert!(
            calls[2]
                .effects
                .iter()
                .any(|effect| effect.kind.contains("workspace")),
            "capture must publish a workspace effect"
        );
        let layout: String = sqlx::query_scalar(
            "SELECT storage_kind FROM cas_blobs WHERE universe_id=$1 AND blob_ref=$2",
        )
        .bind(store.config().universe_id)
        .bind(BlobRef::from_bytes(&captured).as_str())
        .fetch_one(store.pool())
        .await?;
        assert_eq!(
            layout, "object",
            "capture must exercise streamed object storage"
        );
    } else {
        assert_eq!(head.head_snapshot_ref, snapshot.snapshot_ref.as_str());
    }
    Ok(())
}

struct TransferLlm {
    blobs: Arc<dyn BlobStore>,
}

#[async_trait]
impl CoreAgentLlm for TransferLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mode = request.session_id.as_str().rsplit('_').next().unwrap();
        for (name, expected) in [
            ("vfs_materialize", matches!(mode, "edit" | "readonly")),
            ("vfs_capture", mode == "edit"),
        ] {
            assert_eq!(
                test_support::scripted_tool_id(&request, name).is_some(),
                expected,
                "{mode}: {name}"
            );
        }
        if mode == "sourcing" {
            let mut vfs_prompts = Vec::new();
            let mut environment_prompts = Vec::new();
            for entry in &request.request.context.entries {
                let key = entry.key.as_ref().map(|key| key.as_str()).unwrap_or("");
                if key.starts_with("instructions.100.prompts") {
                    vfs_prompts.push(
                        self.blobs
                            .read_text(&entry.content.content_ref)
                            .await
                            .unwrap(),
                    );
                } else if key == "instructions.110.environment" {
                    environment_prompts.push(
                        self.blobs
                            .read_text(&entry.content.content_ref)
                            .await
                            .unwrap(),
                    );
                }
            }
            assert_eq!(vfs_prompts, vec!["VFS first", "VFS second", "VFS last"]);
            assert_eq!(
                environment_prompts,
                vec!["Environment first\n\nEnvironment second\n\nEnvironment last"]
            );
        }
        let step = request
            .request
            .context
            .entries
            .iter()
            .filter(|e| matches!(e.kind, ContextEntryKind::ToolResult { .. }))
            .count();
        let total = match mode {
            "edit" => 4,
            "readonly" => 2,
            _ => 0,
        };
        let mut calls = Vec::new();
        let (kind, bytes, media_type) = if step < total {
            let (name, arguments) = if step < 2 {
                (
                    "vfs_materialize",
                    json!({"source_vfs_path":"/workspace/input","destination_environment_path":"./tree"}),
                )
            } else {
                (
                    "vfs_capture",
                    json!({"source_environment_path":"./capture-source","destination_vfs_path":"/workspace/output"}),
                )
            };
            let bytes = serde_json::to_vec(&arguments).unwrap();
            let arguments_ref = self
                .blobs
                .put_bytes(bytes.clone())
                .await
                .map_err(io_error)?;
            let call_id = ToolCallId::new(format!("transfer-{step}"));
            let tool_name = ToolName::new(name);
            calls.push(ObservedToolCall {
                call_id: call_id.clone(),
                tool_id: test_support::scripted_tool_id(&request, name),
                tool_name: tool_name.clone(),
                provider_kind: None,
                arguments_ref,
                native_call_ref: None,
            });
            (
                ContextEntryKind::ToolCall {
                    call_id,
                    name: tool_name,
                },
                bytes,
                "application/json",
            )
        } else {
            (
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                b"transfer live complete".to_vec(),
                "text/plain",
            )
        };
        let content_ref = self.blobs.put_bytes(bytes).await.map_err(io_error)?;
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind,
                content: engine::ContentRef {
                    content_ref,
                    media_type: Some(media_type.into()),
                    provider_kind: None,
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                finish: if calls.is_empty() {
                    LlmFinish::Stop
                } else {
                    LlmFinish::ToolCalls
                },
                tool_calls: calls,
                duration_ms: None,
                provider_response_id: None,
                usage: None,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}
