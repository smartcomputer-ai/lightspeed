//! Real PostgreSQL, object-store and Temporal teardown, including restartable steps.
use api::{AgentApiErrorKind, AgentApiService, DeploymentApiService, UniverseDeletionState};
use bots::{BotStore, BotTriggerStore};
use channels::ChannelAccountStore;
use credentials::{CredentialGrantStore, SecretStore};
use environments::EnvironmentStore;
use futures_util::TryStreamExt;
use harness::{
    SessionId,
    storage::{BlobStore, CreateSession, SessionStore},
};
use mcp::McpRegistryStore;
use profiles::ProfileStore;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    gateway::{
        GatewayDeploymentApi,
        request_context::{RequestContext, with_request_context as calling},
    },
};
use temporal_workflow::{TemporalConnectionConfig, connect_temporal};
use temporalio_client::{UntypedWorkflow, WorkflowListOptions, WorkflowStartOptions};
use temporalio_common::data_converters::{PayloadConverter, RawValue};
use uuid::Uuid;
use vfs::VfsWorkspaceStore;

fn caller() -> RequestContext {
    RequestContext::local(api::AccessScope::Deployment)
}

async fn runtime() -> anyhow::Result<Arc<UniverseRuntime>> {
    anyhow::ensure!(
        std::env::var("LIGHTSPEED_POSTGRES_URL").is_ok()
            || std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").is_ok(),
        "source scripts/dev/env.sh before live tests"
    );
    let stores = DeploymentStores::from_env().await?;
    store_pg::PgStore::migrate(stores.pool()).await?;
    Ok(Arc::new(UniverseRuntime::new(
        connect_temporal(&TemporalConnectionConfig::from_env()?).await?,
        format!("universe-deletion-live-{}", Uuid::new_v4().simple()),
        Some("http://127.0.0.1:18080".into()),
        stores,
    )?))
}

async fn create(
    runtime: &Arc<UniverseRuntime>,
    id: Uuid,
    handle: Option<String>,
) -> anyhow::Result<()> {
    calling(
        caller(),
        GatewayDeploymentApi::new(runtime.clone()).create_universe(
            api::DeploymentUniverseCreateParams {
                universe_id: id.to_string(),
                handle,
                metadata: Default::default(),
                protected: false,
                expires_at_ms: None,
            },
        ),
    )
    .await?;
    Ok(())
}

async fn delete(
    runtime: &Arc<UniverseRuntime>,
    id: Uuid,
) -> anyhow::Result<api::DeploymentUniverseDeleteResponse> {
    Ok(calling(
        caller(),
        GatewayDeploymentApi::new(runtime.clone()).delete_universe(
            api::DeploymentUniverseDeleteParams {
                universe: id.to_string(),
            },
        ),
    )
    .await?)
}

async fn read(
    runtime: &Arc<UniverseRuntime>,
    id: Uuid,
) -> Result<api::DeploymentUniverseReadResponse, api::AgentApiError> {
    calling(
        caller(),
        GatewayDeploymentApi::new(runtime.clone()).read_universe(
            api::DeploymentUniverseReadParams {
                universe: id.to_string(),
            },
        ),
    )
    .await
}

async fn add_session(store: &store_pg::PgStore, name: &str) -> anyhow::Result<()> {
    store
        .create_session(CreateSession {
            session_id: SessionId::new(name),
            metadata: Default::default(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await?;
    Ok(())
}

async fn drain(runtime: &Arc<UniverseRuntime>, ids: &[Uuid]) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        runtime.delete_universes_once().await?;
        let mut pending = false;
        for id in ids {
            match read(runtime, *id).await {
                Err(error) if error.kind == AgentApiErrorKind::Gone => {}
                Err(error) => return Err(error.into()),
                Ok(_) => pending = true,
            }
        }
        if !pending {
            return Ok(());
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(45),
            "deletion did not finish: {:?}",
            store_pg::list_deleting_universes(runtime.stores().pool()).await?
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn assert_no_residue(runtime: &Arc<UniverseRuntime>, id: Uuid) -> anyhow::Result<()> {
    // Find every universe-owned table, including future additions. The permanent record
    // is the sole intentional survivor; it is not owned through a universe foreign key.
    let tables: Vec<String> = sqlx::query_scalar("SELECT table_name FROM information_schema.columns WHERE table_schema = current_schema() AND column_name = 'universe_id' AND table_name <> 'universes_deleted'").fetch_all(runtime.stores().pool()).await?;
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM \"{quoted}\" WHERE universe_id = $1"
        )))
        .bind(id)
        .fetch_one(runtime.stores().pool())
        .await?;
        anyhow::ensure!(count == 0, "{table} retained {count} rows for {id}");
    }
    let executions: Vec<_> = runtime
        .client()
        .list_workflows(
            format!("WorkflowId STARTS_WITH '{id}/' AND ExecutionStatus = 'Running'"),
            WorkflowListOptions::default(),
        )
        .try_collect()
        .await?;
    anyhow::ensure!(executions.is_empty(), "open executions survived deletion");
    if let Some(objects) = runtime.stores().object_store() {
        let prefix = store_pg::universe_cas_object_prefix(runtime.stores().object_prefix(), id);
        let entries: Vec<_> = objects
            .list(Some(&object_store::path::Path::from(prefix)))
            .try_collect()
            .await?;
        anyhow::ensure!(entries.is_empty(), "objects survived deletion");
    }
    Ok(())
}

async fn provision(
    runtime: &Arc<UniverseRuntime>,
    id: Uuid,
) -> anyhow::Result<(
    temporal_runtime::environments::fake_controller::FakeController,
    String,
    environments::EnvironmentId,
)> {
    use temporal_runtime::environments::fake_controller::{FAKE_TEMPLATE_ID, FakeController};
    let controller = FakeController::start().await?;
    let provider_id = format!("deletion-{}", id.simple());
    let operator = GatewayDeploymentApi::new(runtime.clone());
    calling(
        caller(),
        operator.put_environment_provider(api::DeploymentEnvironmentProviderPutParams {
            provider_id: provider_id.clone(),
            display_name: None,
            controller_endpoint: controller.controller_url().into(),
            data_route_base: None,
            credential: controller.credential().map(str::to_owned),
            clear_credential: false,
        }),
    )
    .await?;
    calling(
        caller(),
        operator.put_environment_provider_binding(api::DeploymentProviderBindingPutParams {
            universe: id.to_string(),
            provider_id: provider_id.clone(),
            enabled: true,
        }),
    )
    .await?;
    let state = runtime.state_for(id).await?;
    let environment = calling(
        RequestContext::local(api::AccessScope::Universe { universe_id: id }),
        state.api.create_environment(api::EnvironmentCreateParams {
            request_id: "provision".into(),
            environment_id: None,
            provider_id: provider_id.clone(),
            template_id: FAKE_TEMPLATE_ID.into(),
            display_name: None,
            metadata: Default::default(),
            idle: None,
        }),
    )
    .await?
    .environment;
    let environment_id = environments::EnvironmentId::new(environment.environment_id);
    let started = Instant::now();
    loop {
        if state.store.read_environment(&environment_id).await?.state
            == environments::EnvironmentState::Open
        {
            break;
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(30),
            "fake provision did not open"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(controller.target_count().await, 1);
    Ok((controller, provider_id, environment_id))
}

async fn remove_provider(
    runtime: &Arc<UniverseRuntime>,
    provider_id: String,
) -> anyhow::Result<()> {
    calling(
        caller(),
        GatewayDeploymentApi::new(runtime.clone()).delete_environment_provider(
            api::DeploymentEnvironmentProviderDeleteParams { provider_id },
        ),
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Temporal, PostgreSQL and MinIO; source scripts/dev/env.sh"]
async fn deletion_fences_then_resumes_every_step_and_removes_all_resource_kinds()
-> anyhow::Result<()> {
    let mut runtime = runtime().await?;
    let id = Uuid::new_v4();
    let replacement = Uuid::new_v4();
    let handle = format!("delete-{}", &id.simple().to_string()[..12]);
    create(&runtime, id, Some(handle.clone())).await?;
    let reconciler = tokio::spawn(runtime.clone().run_environment_reconciler());
    let (controller, provider_id, _) = provision(&runtime, id).await?;
    let state = runtime.state_for(id).await?;
    let store = &state.store;
    for name in ["open", "closed", "soft-deleted"] {
        add_session(store, name).await?;
    }
    // Lifecycle data is irrelevant to the cascade; rows in all states must disappear.
    sqlx::query("UPDATE sessions SET closed_at_ms = 2, closed_at_seq = 1, lifecycle_status = 'closed' WHERE universe_id = $1 AND session_id <> 'open'").bind(id).execute(runtime.stores().pool()).await?;
    sqlx::query("UPDATE sessions SET deleted_at_ms = 3 WHERE universe_id = $1 AND session_id = 'soft-deleted'").bind(id).execute(runtime.stores().pool()).await?;
    let bytes = vec![b'x'; 1024 * 1024];
    let object_ref = store.put_bytes(bytes.clone()).await?;
    store
        .create_workspace(vfs::CreateVfsWorkspaceRecord {
            workspace_id: vfs::VfsWorkspaceId::new("workspace"),
            display_name: None,
            base_snapshot_ref: None,
            head_snapshot_ref: object_ref,
            head_totals: Default::default(),
            created_at_ms: 1,
        })
        .await?;
    store
        .put_agent_profile(
            api::AgentProfileInput {
                profile_id: api::ProfileId::new("profile"),
                display_name: None,
                description: None,
                document: Default::default(),
            },
            None,
            1,
        )
        .await?;
    store
        .put_model_defaults(api::ModelDefaultsPutParams {
            slot: api::ModelDefaultSlot::AgentRun,
            model: Some(api::ModelConfig {
                provider_id: "openai".into(),
                api_kind: "openai:responses".into(),
                model: "gpt-5".into(),
            }),
            expected_revision: None,
        })
        .await?;
    store
        .put_secret(credentials::PutSecretRecord {
            secret_id: credentials::SecretId::new("authsec_deletion"),
            secret_kind: credentials::SECRET_KIND_STATIC_TOKEN.into(),
            value: credentials::SecretValue::new("fixture-token"),
            created_at_ms: 1,
        })
        .await?;
    store
        .create_grant(credentials::CreateCredentialGrantRecord {
            grant_id: credentials::CredentialGrantId::new("authgrant_deletion"),
            provider_id: "static".into(),
            provider_kind: credentials::CredentialProviderKind::StaticToken,
            exposure: credentials::CredentialGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: Some(credentials::SecretId::new("authsec_deletion")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: credentials::CredentialGrantStatus::Active,
            metadata: serde_json::json!({}),
            created_at_ms: 1,
        })
        .await?;
    store
        .put_server(
            mcp::PutMcpServerRecord {
                server_id: mcp::McpServerId::new("fixture"),
                display_name: None,
                server_url: "https://example.com/mcp".into(),
                transport: mcp::RemoteMcpTransport::StreamableHttp,
                default_server_label: "fixture".into(),
                description: None,
                allowed_tools: None,
                execution: mcp::McpExecution::Provider,
                exposure: mcp::McpExposure::Inject,
                approval: mcp::McpApprovalPolicy::Never,
                defer_loading: None,
                allow_private_network: false,
                auth_policy: mcp::McpServerAuthPolicy::None,
                auth_grant_id: None,
                headers: credentials::RequestHeaders::new(),
                status: mcp::McpServerStatus::Active,
                now_ms: 1,
            },
            None,
        )
        .await?;
    anyhow::ensure!(
        !store_pg::list_universe_object_keys(runtime.stores().pool(), id)
            .await?
            .is_empty(),
        "fixture must use configured object storage"
    );

    let bot = api::BotId::new("scheduled");
    let trigger = api::BotTriggerId::new("hourly");
    store
        .create_bot(
            bot.clone(),
            serde_json::from_value(serde_json::json!({"profileId":"profile", "enabled":true}))?,
            1,
        )
        .await?;
    store.put_bot_trigger(&bot, bots::BotTriggerWrite { trigger_id: trigger.clone(), document: serde_json::from_value(serde_json::json!({"kind":"schedule","cron":"@hourly","timezone":"UTC","summary":"tick","enabled":true}))?, secrets: Default::default(), cursor: None }, None, 1).await?;
    state.api.reconcile_bot_schedules_once().await?;
    store
        .create_channel_account(
            api::ChannelAccountId::new("telegram"),
            api::ChannelAccountDocument {
                provider: api::ChannelProvider::new("telegram"),
                provider_account_id: id.to_string(),
                display_name: "test".into(),
                credential_grant_id: None,
                settings: Default::default(),
                enabled: true,
            },
            1,
        )
        .await?;
    store
        .create_external_environment(environments::CreateExternalEnvironment {
            request_id: environments::EnvironmentProvisionRequestId::new("external-request"),
            environment_id: environments::EnvironmentId::new("external"),
            endpoint: "ws://127.0.0.1:9".parse()?,
            display_name: None,
            metadata: Default::default(),
            created_at_ms: 1,
        })
        .await?;
    // No worker is needed to open executions; Temporal records each as Running even
    // when queued. Cover every identity family and a plugin-defined workflow type.
    for (name, workflow_type) in [
        ("open", "AgentSessionWorkflow"),
        ("bot-scheduled", "BotControllerWorkflow"),
        ("chat-telegram-test", "ChannelConversationWorkflow"),
        ("envjob-external-group", "EnvironmentJobWorkflow"),
        ("wtx:sha256:subagent", "SubagentExecutionWorkflow"),
        ("wtx:sha256:code", "CodeExecutionWorkflow"),
        ("wtx:sha256:plugin", "PluginDeclaredWorkflow"),
    ] {
        runtime
            .client()
            .start_workflow(
                UntypedWorkflow::new(workflow_type),
                RawValue::from_value(&serde_json::json!({}), &PayloadConverter::default()),
                WorkflowStartOptions::new("universe-deletion-unpolled", format!("{id}/{name}"))
                    .build(),
            )
            .await?;
    }
    let other_process = self::runtime().await?;
    other_process.state_for(id).await?;
    let cached_at = Instant::now();
    assert_eq!(
        delete(&runtime, id).await?.state,
        UniverseDeletionState::Deleting
    );
    assert_eq!(
        delete(&runtime, id).await?.state,
        UniverseDeletionState::Deleting
    );
    assert!(read(&runtime, id).await?.universe.deletion.is_some());
    let tokens = temporal_runtime::config::RuntimeTokens::from_configuration(
        [(
            "LIGHTSPEED_API_TOKEN".into(),
            "deletion-test-token-at-least-32-characters".into(),
        )],
        |_| unreachable!(),
    )?;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        "Bearer deletion-test-token-at-least-32-characters".parse()?,
    );
    temporal_runtime::gateway::authentication::resolve_caller(&tokens, &headers)
        .expect("universe deletion does not revoke component credentials");
    assert!(
        store_pg::resolve_universe_reference(runtime.stores().pool(), &id.to_string())
            .await?
            .is_none()
    );

    assert!(
        store_pg::resolve_universe_reference(runtime.stores().pool(), &handle)
            .await?
            .is_none()
    );
    create(&runtime, replacement, Some(handle)).await?;
    assert_eq!(
        calling(
            caller(),
            GatewayDeploymentApi::new(runtime.clone()).create_universe(
                api::DeploymentUniverseCreateParams {
                    universe_id: id.to_string(),
                    handle: None,
                    metadata: Default::default(),
                    protected: false,
                    expires_at_ms: None,
                }
            )
        )
        .await
        .unwrap_err()
        .kind,
        AgentApiErrorKind::Conflict
    );

    // A fresh runtime after each step proves progress is durable and independent of
    // the serving process's caches. The worker fallback revalidation has a 30 s bound.
    let mut seen_steps = Vec::new();
    let teardown_started = Instant::now();
    loop {
        match read(&runtime, id).await {
            Ok(current) => {
                let step = current.universe.deletion.expect("deletion progress").step;
                if !seen_steps.contains(&step) {
                    seen_steps.push(step);
                }
            }
            Err(error) if error.kind == AgentApiErrorKind::Gone => break,
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            teardown_started.elapsed() < Duration::from_secs(45),
            "restarted deletion did not converge"
        );
        runtime.delete_universes_once().await?;
        runtime = self::runtime().await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    assert_eq!(
        seen_steps,
        vec![
            api::UniverseDeletionStep::Intake,
            api::UniverseDeletionStep::Workflows,
            api::UniverseDeletionStep::Environments,
            api::UniverseDeletionStep::Storage
        ]
    );
    tokio::time::sleep(Duration::from_secs(30).saturating_sub(cached_at.elapsed())).await;
    assert!(matches!(
        other_process.state_for(id).await,
        Err(temporal_runtime::universe::UniverseError::Unknown { .. })
    ));
    assert_no_residue(&runtime, id).await?;
    assert_eq!(
        controller.target_count().await,
        0,
        "provider must receive target/close"
    );
    let schedule = runtime
        .client()
        .get_schedule_handle(bots::bot_schedule_id(id, &bot, &trigger));
    assert!(
        schedule.describe(Default::default()).await.is_err(),
        "bot schedule survived deletion"
    );
    let gone = read(&runtime, id).await.unwrap_err();
    assert_eq!(gone.kind, AgentApiErrorKind::Gone);
    let record = gone.deleted_universe.expect("gone carries resource record");
    assert_eq!(record.summary["contained"]["sessions"], 3);
    assert_eq!(record.summary["contained"]["bots"], 1);
    assert_eq!(record.summary["contained"]["channelAccounts"], 1);
    assert_eq!(record.summary["contained"]["apiKeys"], 1);
    assert_eq!(record.summary["contained"]["workspaces"], 1);
    assert_eq!(record.summary["contained"]["profiles"], 1);
    assert_eq!(record.summary["contained"]["mcpServers"], 1);
    assert_eq!(record.summary["contained"]["environments"]["external"], 1);
    assert_eq!(
        record.summary["contained"]["environments"]["provisioned"],
        1
    );
    assert_eq!(record.summary["contained"]["blobBytes"], bytes.len());
    assert_eq!(
        delete(&runtime, id).await?.state,
        UniverseDeletionState::Deleted
    );
    assert_eq!(
        calling(
            caller(),
            GatewayDeploymentApi::new(runtime.clone()).create_universe(
                api::DeploymentUniverseCreateParams {
                    universe_id: id.to_string(),
                    handle: None,
                    metadata: Default::default(),
                    protected: false,
                    expires_at_ms: None,
                }
            )
        )
        .await
        .unwrap_err()
        .kind,
        AgentApiErrorKind::Conflict
    );
    let listed = calling(
        caller(),
        GatewayDeploymentApi::new(runtime.clone()).list_deleted_universes(Default::default()),
    )
    .await?;
    assert!(
        listed
            .universes
            .iter()
            .any(|record| record.universe_id == id.to_string())
    );
    delete(&runtime, replacement).await?;
    drain(&runtime, &[replacement]).await?;
    remove_provider(&runtime, provider_id).await?;
    reconciler.abort();
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Temporal and PostgreSQL; source scripts/dev/env.sh"]
async fn fifty_universes_leave_no_owned_rows_or_open_executions() -> anyhow::Result<()> {
    let runtime = runtime().await?;
    let mut ids = Vec::new();
    for _ in 0..50 {
        let id = Uuid::new_v4();
        create(&runtime, id, None).await?;
        add_session(&runtime.stores().store_for(id), "session").await?;
        runtime
            .client()
            .start_workflow(
                UntypedWorkflow::new("AgentSessionWorkflow"),
                RawValue::from_value(&serde_json::json!({}), &PayloadConverter::default()),
                WorkflowStartOptions::new("universe-deletion-unpolled", format!("{id}/session"))
                    .build(),
            )
            .await?;
        delete(&runtime, id).await?;
        ids.push(id);
    }
    drain(&runtime, &ids).await?;
    for id in ids {
        assert_no_residue(&runtime, id).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Temporal, PostgreSQL and fake environment provider; source scripts/dev/env.sh"]
async fn unreachable_provider_is_bounded_and_records_the_unconfirmed_machine() -> anyhow::Result<()>
{
    let runtime = runtime().await?;
    let id = Uuid::new_v4();
    create(&runtime, id, None).await?;
    let reconciler = tokio::spawn(runtime.clone().run_environment_reconciler());
    let (controller, provider_id, environment_id) = provision(&runtime, id).await?;
    let environment = runtime
        .stores()
        .store_for(id)
        .read_environment(&environment_id)
        .await?;
    let target_id = environment
        .provisioned()
        .unwrap()
        .provider_target_id
        .clone()
        .expect("provisioned target");
    controller.set_reachable(false);
    let started = Instant::now();
    delete(&runtime, id).await?;
    loop {
        runtime.delete_universes_once().await?;
        if matches!(read(&runtime, id).await, Err(error) if error.kind == AgentApiErrorKind::Gone) {
            break;
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(150),
            "unreachable provider blocked deletion beyond its bound"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let record = read(&runtime, id)
        .await
        .unwrap_err()
        .deleted_universe
        .unwrap();
    assert_eq!(
        record.summary["removed"]["environmentsUnconfirmed"],
        serde_json::json!([{"providerId": provider_id, "providerTargetId": target_id}])
    );
    assert_no_residue(&runtime, id).await?;
    controller.set_reachable(true);
    controller.end_target(environment_id.as_str()).await;
    remove_provider(&runtime, provider_id).await?;
    reconciler.abort();
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Temporal and PostgreSQL; runs two local registered daemons"]
async fn deletion_ends_watch_and_connected_daemon_and_refuses_disconnected_daemon()
-> anyhow::Result<()> {
    use environment_daemon::{
        DaemonRuntime,
        config::{DaemonConfig, OnClosed, RegistrationConfig},
        server::DaemonExit,
    };
    use environment_protocol::bytes::SecretString;
    use temporal_runtime::gateway::{GatewayRoutes, GatewayState, gateway_router};
    let stores = DeploymentStores::from_env().await?;
    store_pg::PgStore::migrate(stores.pool()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let connect = format!("ws://{}/environments/connect", listener.local_addr()?);
    let runtime = Arc::new(UniverseRuntime::new(
        connect_temporal(&TemporalConnectionConfig::from_env()?).await?,
        format!("deletion-daemon-{}", Uuid::new_v4().simple()),
        Some(base.clone()),
        stores,
    )?);
    let id = Uuid::new_v4();
    create(&runtime, id, None).await?;
    let invalidations = tokio::spawn(runtime.clone().run_universe_invalidations());
    let gateway_state = Arc::new(GatewayState::multi(
        temporal_runtime::GatewayAuthMode::Single { universe_id: id },
        runtime.clone(),
        base.clone(),
    ));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            gateway_router(gateway_state, 1024 * 1024, GatewayRoutes::ALL),
        )
        .await
    });
    let state = runtime.state_for(id).await?;
    let minted = calling(
        RequestContext::local(api::AccessScope::Universe { universe_id: id }),
        state.api.create_environment_registration_key(
            api::EnvironmentRegistrationKeyCreateParams {
                registration_key_id: None,
                display_name: "deletion daemons".into(),
                identity_mode: api::EnvironmentIdentityModeView::Persistent,
                max_active_environments: None,
                ephemeral_disconnect_grace_ms: None,
                offline_close_after_ms: None,
                declared_only: false,
                expires_at_ms: None,
            },
        ),
    )
    .await?;
    let sandbox = tempfile::tempdir()?;
    let config = |name: &str| -> anyhow::Result<DaemonConfig> {
        let root = sandbox.path().join(name);
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let mut registration = RegistrationConfig::new(
            connect.clone(),
            environment_daemon::upgrade::resolve_discovery_url(Some(&connect), None)?,
        );
        registration.registration_key = Some(SecretString::new(&minted.secret.0));
        registration.on_closed = OnClosed::Exit;
        Ok(DaemonConfig {
            listen: None,
            cwd: root.clone(),
            fs_root: root,
            state_dir: sandbox.path().join(format!("state-{name}")),
            read_only_fs: false,
            registration: Some(registration),
            scrubbed_env: Vec::new(),
        })
    };
    let first = DaemonRuntime::new(config("connected")?)?;
    let first_exit = tokio::spawn(environment_daemon::server::run(first));
    let second = DaemonRuntime::new(config("disconnected")?)?;
    let second_exit = tokio::spawn(environment_daemon::server::run(second.clone()));
    let started = Instant::now();
    loop {
        let environments = state
            .store
            .list_environments(environments::ListEnvironments::default())
            .await?
            .environments;
        if environments.len() == 2
            && environments.iter().all(|environment| {
                environment
                    .registered()
                    .is_some_and(|source| source.connected)
            })
        {
            break;
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(30),
            "daemons did not connect"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    second.shutdown().cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(10), second_exit).await??,
        DaemonExit::Shutdown
    ));
    let mut watch = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/universes/{id}/events?kinds=environment"
        ))
        .header("accept", "text/event-stream")
        .send()
        .await?;
    assert_eq!(watch.headers()["content-type"], "text/event-stream");
    let initial = tokio::time::timeout(Duration::from_secs(5), watch.chunk())
        .await??
        .expect("initial reset");
    assert!(String::from_utf8_lossy(&initial).contains("watch/reset"));
    delete(&runtime, id).await?;
    // No deletion pass is running yet: both actions must be caused by the fence hint.
    let first_exit = tokio::time::timeout(Duration::from_secs(10), first_exit).await??;
    assert!(
        matches!(first_exit, DaemonExit::EnvironmentClosed),
        "connected daemon did not receive registration/closed: {first_exit:?}"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while watch.chunk().await?.is_some() {}
        anyhow::Ok(())
    })
    .await??;
    let reconnect = DaemonRuntime::new(config("disconnected")?)?;
    let refused = tokio::time::timeout(
        Duration::from_secs(15),
        environment_daemon::server::run(reconnect),
    )
    .await?;
    assert!(
        matches!(
            refused,
            DaemonExit::Refused(_) | DaemonExit::EnvironmentClosed
        ),
        "a spent daemon reconnected: {refused:?}"
    );
    drain(&runtime, &[id]).await?;
    assert_no_residue(&runtime, id).await?;
    let record = read(&runtime, id)
        .await
        .unwrap_err()
        .deleted_universe
        .unwrap();
    assert_eq!(record.summary["contained"]["environments"]["registered"], 2);
    invalidations.abort();
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Temporal and PostgreSQL; exercises production workflow polling"]
async fn orphaned_subagent_and_environment_job_workflows_end_without_the_deletion_pass()
-> anyhow::Result<()> {
    use temporal_runtime::worker::{WorkerActivities, sessions_worker, worker_runtime};
    use temporal_workflow::{
        EnvironmentJobId, EnvironmentJobRecord, EnvironmentJobStartActivityRequest,
        EnvironmentJobStatus, EnvironmentJobWorkflow, EnvironmentJobWorkflowArgs,
        EnvironmentJobWorkflowInput, SubagentChildRef, SubagentExecutionInput,
        SubagentExecutionResume, SubagentExecutionWorkflow, SubagentWaitPolicy,
    };
    let runtime = runtime().await?;
    let unknown = Uuid::new_v4();
    let active = Uuid::new_v4();
    create(&runtime, active, None).await?;
    let state = runtime.state_for(active).await?;
    state
        .store
        .create_external_environment(environments::CreateExternalEnvironment {
            request_id: environments::EnvironmentProvisionRequestId::new("closed-job"),
            environment_id: environments::EnvironmentId::new("closed-job"),
            endpoint: "ws://127.0.0.1:9".parse()?,
            display_name: None,
            metadata: Default::default(),
            created_at_ms: 1,
        })
        .await?;
    calling(
        RequestContext::local(api::AccessScope::Universe {
            universe_id: active,
        }),
        state.api.close_environment(api::EnvironmentCloseParams {
            environment_id: "closed-job".into(),
        }),
    )
    .await?;
    let queue = format!("orphan-polling-{}", Uuid::new_v4().simple());
    let worker_runtime = worker_runtime()?;
    let mut worker = sessions_worker(
        &worker_runtime,
        runtime.client().clone(),
        queue.clone(),
        WorkerActivities::with_runtime(runtime.clone()),
    )?;
    let shutdown = worker.shutdown_handle();
    let worker_run = worker.run();
    tokio::pin!(worker_run);
    let body = async {
        let mut start: temporal_workflow::WorkflowToolStartArgs = serde_json::from_value(
            temporal_workflow::workflow_contract::export().manifest["vectors"]["startArgs"].clone(),
        )?;
        start.universe_id = unknown;
        start.invocation.session_universe_id = unknown;
        start.holder_workflow_id =
            temporal_workflow::compose_workflow_id(unknown, &start.invocation.session_id);
        let execution =
            temporal_workflow::compose_workflow_tool_execution_id(unknown, &start.execution_id);
        let subagent = runtime
            .client()
            .start_workflow(
                SubagentExecutionWorkflow::run,
                SubagentExecutionInput::Resume(SubagentExecutionResume {
                    start,
                    child: SubagentChildRef {
                        session_id: "child".into(),
                        run_id: 1,
                        agent_profile_id: "profile".into(),
                    },
                    deadline_at_ms: None,
                    wait: SubagentWaitPolicy {
                        liveness_check_interval_ms: 10,
                        continue_as_new_history_threshold: 1000,
                    },
                }),
                WorkflowStartOptions::new(queue.clone(), execution).build(),
            )
            .await?;
        let mut jobs = Vec::new();
        for universe_id in [unknown, active] {
            let job_id = EnvironmentJobId::new("job");
            let args = EnvironmentJobWorkflowArgs {
                universe_id,
                start: EnvironmentJobStartActivityRequest {
                    universe_id,
                    environment_id: "closed-job".into(),
                    job_group_id: "group".into(),
                    request_ref: harness::BlobRef::from_bytes(b"unused start"),
                },
                job_ids: vec![job_id.clone()],
                subscriptions: Vec::new(),
                started: true,
                jobs: vec![EnvironmentJobRecord {
                    job_id,
                    name: None,
                    status: EnvironmentJobStatus::Running,
                    depends_on: Vec::new(),
                    queue_key: None,
                }],
                resolutions: Default::default(),
                poll_ms: 10,
                poll_attempt: 0,
                workflow_tool: None,
            };
            jobs.push(
                runtime
                    .client()
                    .start_workflow(
                        EnvironmentJobWorkflow::run,
                        EnvironmentJobWorkflowInput::Job(args),
                        WorkflowStartOptions::new(
                            queue.clone(),
                            temporal_workflow::compose_environment_job_workflow_id(
                                universe_id,
                                "closed-job",
                                "group",
                            ),
                        )
                        .build(),
                    )
                    .await?,
            );
        }
        let result = subagent.get_result(Default::default()).await;
        assert!(
            matches!(
                &result,
                Err(temporalio_client::errors::WorkflowGetResultError::Failed(_))
            ),
            "orphan subagent did not end as failed: {result:?}"
        );
        assert!(result.unwrap_err().to_string().contains("unknown universe"));
        for job in jobs {
            let result = job.get_result(Default::default()).await;
            assert!(
                matches!(
                    &result,
                    Err(temporalio_client::errors::WorkflowGetResultError::Failed(_))
                ),
                "orphan job did not end as failed: {result:?}"
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("environment job stopped")
            );
        }
        anyhow::Ok(())
    };
    tokio::pin!(body);
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(30), &mut body) => result.map_err(anyhow::Error::from)?,
        result = &mut worker_run => Err(anyhow::anyhow!("worker stopped early: {result:?}")),
    };
    shutdown();
    tokio::time::timeout(Duration::from_secs(15), &mut worker_run).await??;
    result?;
    delete(&runtime, active).await?;
    drain(&runtime, &[active]).await?;
    Ok(())
}
