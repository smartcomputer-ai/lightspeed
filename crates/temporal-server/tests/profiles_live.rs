//! Live coverage for profile CRUD, application, and profile-provisioned
//! environments.

mod support;

use api::{
    AgentApiService, AgentProfileInput, ContextEntryKindView, InlineAgentProfile, InputItem,
    McpServerDeleteParams, McpServerInput, McpServerPutParams, McpServerStatus, ProfileApplyParams,
    ProfileCreateParams, ProfileDeleteParams, ProfileDocument, ProfileId, ProfileInstructions,
    ProfileListParams, ProfilePutParams, ProfileReadParams, ProfileSource, RemoteMcpApprovalPolicy,
    RunStartParams, RunStartSource, SessionConfig, SessionStartParams,
};
use api_projection::model_to_api;
use engine::SessionId;
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities, final_assistant_text, live_workflow_handle,
    read_session_view, require_storage_live_env, run_with_live_worker, wait_for_environment_status,
    wait_for_terminal_run,
};
use temporal_server::{default_model_from_env, gateway::GatewayAgentApi, pg_store_from_env};
use temporalio_client::{Client, WorkflowTerminateOptions};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_profiles_create_start_and_apply_idempotently() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_profiles_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_profile_provisions_environment_for_session() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_profile_provision_live_client).await
}

/// A `provision` profile creates one environment for the session it
/// starts, activates it while it is still provisioning, converges on retries
/// and repeated applies, and closes it with the session (or retains it).
async fn run_profile_provision_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    use std::collections::BTreeMap;

    use environment_protocol::shared::EnvironmentTransport;
    use environments::{
        EnvironmentConnectionSpec, EnvironmentProviderBindingId, EnvironmentProviderBindingStatus,
        EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProviderStore,
        PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };

    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store.clone())
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let provider_id = format!("fake-profile-{suffix}");
    let binding_id = format!("binding-profile-{suffix}");
    let profile_id = ProfileId::new(format!("live_provision_{suffix}"));

    // Register the in-process fake provider and bind it to this universe
    // directly through the store: the operator API is deployment-scoped and
    // this test drives one universe's gateway.
    store
        .put_provider(PutEnvironmentProvider {
            provider_id: EnvironmentProviderId::new(provider_id.clone()),
            display_name: Some("Live fake provider".to_owned()),
            controller_connection: EnvironmentConnectionSpec::new(
                "in-process",
                EnvironmentTransport::Provider {
                    provider_type: "fake".to_owned(),
                },
            ),
            metadata: BTreeMap::new(),
            updated_at_ms: 1,
        })
        .await?;
    store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id: store.config().universe_id,
            binding_id: EnvironmentProviderBindingId::new(binding_id.clone()),
            provider_id: EnvironmentProviderId::new(provider_id.clone()),
            status: EnvironmentProviderBindingStatus::Enabled,
            expected_revision: None,
            metadata: BTreeMap::new(),
            updated_at_ms: 1,
        })
        .await?;

    let provision_document = |retention: api::ProfileEnvironmentRetention| ProfileDocument {
        metadata: Default::default(),
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(api::FeaturesConfig {
                environments: Some(api::EnvironmentsFeature {
                    working_directory: None,
                    prompts: None,
                    version: api::CURRENT_FEATURE_VERSION,
                    providers: None,
                    registration_keys: None,
                    selection_tools: false,
                    jobs: false,
                    skills: None,
                }),
                ..api::FeaturesConfig::default()
            }),
            ..SessionConfig::default()
        }),
        instructions: None,
        environment: Some(api::ProfileEnvironment::Provision {
            provider_id: provider_id.clone(),
            template_id: "rust-v1".to_owned(),
            display_name: None,
            metadata: BTreeMap::from([("role".to_owned(), "sandbox".to_owned())]),
            retention,
            idle_policy: None,
            credentials: Vec::new(),
        }),
        retention: None,
    };
    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: Some("Live provisioning profile".to_owned()),
            description: None,
            document: provision_document(api::ProfileEnvironmentRetention::CloseWithSession),
        },
    })
    .await?;

    // A profile that provisions from an unknown provider fails before any
    // session exists.
    let rejected = api
        .start_session(SessionStartParams {
            metadata: Default::default(),
            session_id: Some(format!("{}_rejected", session_id.as_str())),
            display_name: None,
            config: None,
            environment: None,
            delete_after_close_ms: None,
            profile: Some(ProfileSource::Inline {
                profile: Box::new(api::InlineAgentProfile {
                    display_name: None,
                    description: None,
                    document: ProfileDocument {
                        environment: Some(api::ProfileEnvironment::Provision {
                            provider_id: format!("missing-{suffix}"),
                            template_id: "rust-v1".to_owned(),
                            display_name: None,
                            metadata: BTreeMap::new(),
                            retention: api::ProfileEnvironmentRetention::CloseWithSession,
                            idle_policy: None,
                            credentials: Vec::new(),
                        }),
                        ..provision_document(api::ProfileEnvironmentRetention::CloseWithSession)
                    },
                }),
            }),
        })
        .await;
    assert!(
        rejected.is_err(),
        "unknown provider must be rejected before start"
    );
    assert!(
        api.read_session(api::SessionReadParams {
            session_id: format!("{}_rejected", session_id.as_str()),
            run_limit: None,
        })
        .await
        .is_err(),
        "no session may exist after a pre-start rejection"
    );

    // Start: the environment is created and activated while still
    // provisioning (no reconciler has run yet).
    let start = |session_id: String| {
        api.start_session(SessionStartParams {
            metadata: Default::default(),
            session_id: Some(session_id),
            display_name: None,
            config: None,
            environment: None,
            delete_after_close_ms: None,
            profile: Some(ProfileSource::Named {
                profile_id: profile_id.clone(),
            }),
        })
    };
    start(session_id.as_str().to_owned()).await?;
    let started_view = read_session_view(&api, &session_id).await?;
    let active = started_view
        .active_environment_id
        .clone()
        .expect("profile provisioning activates the new environment");
    let listed = api
        .list_environments(api::EnvironmentListParams {
            metadata: Default::default(),
            origin_session_id: Some(session_id.as_str().to_owned()),
            ..api::EnvironmentListParams::default()
        })
        .await?
        .result
        .environments;
    assert_eq!(listed.len(), 1);
    let environment = &listed[0];
    assert_eq!(environment.environment_id, active);
    assert_eq!(
        environment.status,
        api::EnvironmentLifecycleStatusView::Provisioning
    );
    assert_eq!(
        environment.request_id,
        environments::EnvironmentProvisionRequestId::for_session(&session_id)
            .as_str()
            .to_owned()
    );
    let origin = environment
        .origin_session
        .as_ref()
        .expect("origin session provenance");
    assert_eq!(origin.session_id, session_id.as_str());
    assert_eq!(origin.profile_id.as_ref(), Some(&profile_id));
    assert!(origin.close_with_session);
    assert_eq!(
        environment.metadata.get("role").map(String::as_str),
        Some("sandbox")
    );

    // Retry the start and re-apply the profile: still exactly one environment.
    start(session_id.as_str().to_owned()).await?;
    let restarted_view = read_session_view(&api, &session_id).await?;
    assert_eq!(
        restarted_view.active_environment_id.as_deref(),
        Some(active.as_str())
    );
    let applied = api
        .apply_profile(ProfileApplyParams {
            session_id: session_id.as_str().to_owned(),
            profile: ProfileSource::Named {
                profile_id: profile_id.clone(),
            },
            expected_config_revision: None,
            expected_tools_revision: None,
        })
        .await?;
    assert!(!applied.result.applied.environment_provisioned);
    assert!(!applied.result.applied.active_environment_changed);
    assert_eq!(
        api.list_environments(api::EnvironmentListParams {
            metadata: Default::default(),
            origin_session_id: Some(session_id.as_str().to_owned()),
            ..api::EnvironmentListParams::default()
        })
        .await?
        .result
        .environments
        .len(),
        1
    );

    // Drive the reconciler: the fake provider brings the environment to ready.
    wait_for_environment_status(&api, &active, api::EnvironmentLifecycleStatusView::Ready).await?;

    // Closing the session closes the environment (eager close, then the
    // reconciler finishes it).
    api.close_session(api::SessionCloseParams {
        session_id: session_id.as_str().to_owned(),
        force: false,
    })
    .await?;
    wait_for_environment_status(&api, &active, api::EnvironmentLifecycleStatusView::Closed).await?;

    // The sweep alone (no eager close) also converges: an environment whose
    // origin session is already closed is picked up by reconciliation.
    let swept = api
        .create_environment(api::EnvironmentCreateParams {
            request_id: format!("sweep-{suffix}"),
            binding_id: binding_id.clone(),
            template_id: "rust-v1".to_owned(),
            display_name: None,
            metadata: BTreeMap::new(),
            idle_policy: None,
        })
        .await?
        .result
        .environment;
    sqlx::query(
        "UPDATE environments SET origin_session_id = $3, origin_close_with_session = true \
         WHERE universe_id = $1 AND environment_id = $2",
    )
    .bind(store.config().universe_id)
    .bind(&swept.environment_id)
    .bind(session_id.as_str())
    .execute(store.pool())
    .await?;
    wait_for_environment_status(
        &api,
        &swept.environment_id,
        api::EnvironmentLifecycleStatusView::Closed,
    )
    .await?;

    // `retain`: the environment outlives its session.
    let retained_session = format!("{}_retain", session_id.as_str());
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(retained_session.clone()),
        display_name: None,
        config: None,
        environment: None,
        delete_after_close_ms: None,
        profile: Some(ProfileSource::Inline {
            profile: Box::new(api::InlineAgentProfile {
                display_name: None,
                description: None,
                document: provision_document(api::ProfileEnvironmentRetention::Retain),
            }),
        }),
    })
    .await?;
    let retained_view = read_session_view(&api, &SessionId::new(retained_session.clone())).await?;
    let retained_environment = retained_view
        .active_environment_id
        .clone()
        .expect("retained environment activated");
    wait_for_environment_status(
        &api,
        &retained_environment,
        api::EnvironmentLifecycleStatusView::Ready,
    )
    .await?;
    api.close_session(api::SessionCloseParams {
        session_id: retained_session.clone(),
        force: false,
    })
    .await?;
    for _ in 0..5 {
        api.reconcile_environments_once().await?;
    }
    let retained = api
        .read_environment(api::EnvironmentReadParams {
            environment_id: retained_environment.clone(),
        })
        .await?
        .result
        .environment;
    assert_eq!(retained.status, api::EnvironmentLifecycleStatusView::Ready);
    assert!(
        !retained
            .origin_session
            .as_ref()
            .expect("origin")
            .close_with_session
    );
    api.close_environment(api::EnvironmentCloseParams {
        environment_id: retained_environment.clone(),
    })
    .await?;
    wait_for_environment_status(
        &api,
        &retained_environment,
        api::EnvironmentLifecycleStatusView::Closed,
    )
    .await?;

    let _ = api
        .delete_profile(api::ProfileDeleteParams {
            profile_id: profile_id.clone(),
        })
        .await;
    // Every environment above is closed, so the binding and provider can go.
    let _ = store
        .delete_provider_binding(
            store.config().universe_id,
            &EnvironmentProviderBindingId::new(binding_id.clone()),
        )
        .await;
    let _ = store
        .delete_provider(&EnvironmentProviderId::new(provider_id.clone()))
        .await;
    Ok(())
}

async fn run_profiles_live_client(
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
    let profile_id = ProfileId::new(format!("live_profile_{}", uuid::Uuid::new_v4().simple()));
    let server_id = format!("profile_crm_{}", uuid::Uuid::new_v4().simple());

    api.put_mcp_server(McpServerPutParams {
        server: McpServerInput {
            server_id: server_id.clone(),
            display_name: Some("Profile CRM".to_owned()),
            server_url: format!("https://{server_id}.example.com/mcp"),
            default_server_label: "profile_crm".to_owned(),
            description: Some("Profile live MCP server".to_owned()),
            allowed_tools: Some(vec!["lookup_customer".to_owned()]),
            execution: api::RemoteMcpExecution::Provider,
            exposure: api::RemoteMcpExposure::Inject,
            approval_default: RemoteMcpApprovalPolicy::Never,
            defer_loading_default: Some(true),
            allow_private_network: false,
            auth_policy: api::McpServerAuthPolicy::None,
            credential: None,
            status: McpServerStatus::Active,
        },
        expected_revision: None,
    })
    .await?;

    let created = api
        .create_profile(ProfileCreateParams {
            profile: AgentProfileInput {
                profile_id: profile_id.clone(),
                display_name: Some("Live profile".to_owned()),
                description: Some("Initial live profile".to_owned()),
                document: ProfileDocument {
                    metadata: Default::default(),
                    config: Some(SessionConfig {
                        features: Some(api::FeaturesConfig {
                            mcp: Some(api::McpFeature {
                                version: api::CURRENT_FEATURE_VERSION,
                                servers: vec![api::McpServerLink {
                                    server_id: server_id.clone(),
                                }],
                            }),
                            timers: Some(api::TimersFeature {
                                version: api::CURRENT_FEATURE_VERSION,
                            }),
                            ..api::FeaturesConfig::default()
                        }),
                        ..SessionConfig::default()
                    }),
                    instructions: Some(ProfileInstructions::Text {
                        text: "Use the profile instructions in this live test.".to_owned(),
                    }),
                    environment: None,
                    retention: None,
                },
            },
        })
        .await?;
    assert_eq!(created.result.profile.profile_id, profile_id);
    assert_eq!(created.result.profile.revision, 1);

    // Full-document put: re-send the created profile with a new description.
    let mut updated_input = AgentProfileInput {
        profile_id: profile_id.clone(),
        display_name: created.result.profile.display_name.clone(),
        description: created.result.profile.description.clone(),
        document: created.result.profile.document.clone(),
    };
    updated_input.description = Some("Updated live profile".to_owned());
    let updated = api
        .put_profile(ProfilePutParams {
            profile: updated_input,
            expected_revision: Some(1),
        })
        .await?;
    assert_eq!(updated.result.profile.revision, 2);
    assert_eq!(
        updated.result.profile.description.as_deref(),
        Some("Updated live profile")
    );

    let read = api
        .read_profile(ProfileReadParams {
            profile_id: profile_id.clone(),
        })
        .await?;
    assert_eq!(read.result.profile.revision, 2);
    let listed = api.list_profiles(ProfileListParams {}).await?;
    assert!(
        listed
            .result
            .profiles
            .iter()
            .any(|profile| profile.profile_id == profile_id)
    );

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        environment: None,
        delete_after_close_ms: None,
        profile: Some(ProfileSource::Named {
            profile_id: profile_id.clone(),
        }),
    })
    .await?;
    let session = read_session_view(&api, &session_id).await?;
    let config = session.config.as_ref().expect("session config");
    let features = config.features.as_ref().expect("session features");
    assert!(features.timers.is_some());
    assert!(features.web.is_none());
    assert_eq!(
        session
            .active_context
            .entries
            .iter()
            .filter(|entry| matches!(&entry.kind, ContextEntryKindView::Instructions))
            .count(),
        1,
        "profile instructions should replace the product fallback"
    );
    assert!(
        session.active_context.entries.iter().any(|entry| matches!(
            &entry.kind,
            ContextEntryKindView::Instructions
        ) && entry.preview.as_deref()
            == Some("Profile instructions")),
        "profile instructions should be projected"
    );

    let mcp_tools: Vec<_> = session
        .active_tools
        .tools
        .iter()
        .filter(|tool| matches!(tool.kind, api::ToolKindView::RemoteMcp { .. }))
        .collect();
    assert_eq!(mcp_tools.len(), 1);
    assert_eq!(mcp_tools[0].tool_id, "mcp_profile_crm");
    let api::ToolKindView::RemoteMcp { server_label, .. } = &mcp_tools[0].kind else {
        panic!("expected remote MCP tool kind");
    };
    assert_eq!(server_label, "profile_crm");

    let applied = api
        .apply_profile(ProfileApplyParams {
            session_id: session_id.as_str().to_owned(),
            profile: ProfileSource::Named {
                profile_id: profile_id.clone(),
            },
            expected_config_revision: Some(session.config_revision),
            expected_tools_revision: Some(session.active_tools.revision),
        })
        .await?;
    assert!(!applied.result.applied.config_changed);
    assert!(!applied.result.applied.instructions_changed);
    assert!(!applied.result.applied.active_environment_changed);

    let cleared = api
        .apply_profile(ProfileApplyParams {
            session_id: session_id.as_str().to_owned(),
            profile: ProfileSource::Inline {
                profile: Box::new(InlineAgentProfile {
                    display_name: Some("No profile instructions".to_owned()),
                    description: None,
                    document: ProfileDocument::default(),
                }),
            },
            expected_config_revision: Some(applied.result.session.config_revision),
            expected_tools_revision: Some(session.active_tools.revision),
        })
        .await?;
    assert!(cleared.result.applied.instructions_changed);
    let cleared_view = read_session_view(&api, &session_id).await?;
    let cleared_instructions = cleared_view
        .active_context
        .entries
        .iter()
        .filter(|entry| matches!(&entry.kind, ContextEntryKindView::Instructions))
        .collect::<Vec<_>>();
    assert_eq!(cleared_instructions.len(), 1);
    assert_ne!(
        cleared_instructions[0].preview.as_deref(),
        Some("Profile instructions")
    );

    let restored = api
        .apply_profile(ProfileApplyParams {
            session_id: session_id.as_str().to_owned(),
            profile: ProfileSource::Named {
                profile_id: profile_id.clone(),
            },
            expected_config_revision: Some(cleared.result.session.config_revision),
            expected_tools_revision: Some(cleared_view.active_tools.revision),
        })
        .await?;
    assert!(restored.result.applied.instructions_changed);
    let restored_view = read_session_view(&api, &session_id).await?;
    assert_eq!(
        restored_view
            .active_context
            .entries
            .iter()
            .filter(|entry| matches!(&entry.kind, ContextEntryKindView::Instructions))
            .count(),
        1
    );

    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "run after profile start".to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    let run = wait_for_terminal_run(&api, &session_id, &run.result.run.id).await?;
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    api.delete_profile(ProfileDeleteParams { profile_id })
        .await?;
    api.delete_mcp_server(McpServerDeleteParams { server_id })
        .await?;

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent profile live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}
