//! Minimal live environment control-plane acceptance test.
//!
//! This deliberately uses the in-process provider with real Postgres and the
//! real deployment reconciler. It covers the durable provider and ingress seams
//! without requiring an Incus installation; the separate Incus live smoke test
//! owns backend connectivity and topology.

mod support;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use api::{
    AgentApiService, EnvironmentCloseParams, EnvironmentCreateParams, EnvironmentIngressPutParams,
    EnvironmentLifecycleStatusView, EnvironmentListParams, EnvironmentProviderBindingStatusView,
    EnvironmentTemplateListParams, OperatorApiService, OperatorEnvironmentAdoptParams,
    OperatorEnvironmentProviderConnection, OperatorEnvironmentProviderDeleteParams,
    OperatorEnvironmentProviderPutParams, OperatorEnvironmentProviderTransport,
    OperatorProviderBindingPutParams, OperatorUniverseCreateParams, SessionConfig,
    SessionStartParams,
};
use api_projection::model_to_api;
use engine::SessionId;
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities, read_session_view, require_storage_live_env,
    run_with_live_worker, wait_for_environment_status,
};
use temporal_server::{
    DeploymentStores, UniverseRuntime, default_model_from_env,
    gateway::{GatewayAgentApi, GatewayOperatorApi},
    pg_store_from_env,
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use temporalio_client::Client;
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn environment_provider_lifecycle_and_adoption_round_trip() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    require_live_env()?;

    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let stores = DeploymentStores::from_env().await?;
    let pool = stores.pool().clone();
    let suffix = Uuid::new_v4().simple().to_string();
    let provider_id = format!("fake-live-{suffix}");
    let universe_a = Uuid::new_v4();
    let universe_b = Uuid::new_v4();
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        format!("environment-provider-live-{suffix}"),
        Some("http://127.0.0.1:18080".to_owned()),
        stores,
    )?);
    let operator = GatewayOperatorApi::new(runtime.clone());
    let reconciler = tokio::spawn(runtime.clone().run_environment_reconciler());

    let result = async {
        for universe_id in [universe_a, universe_b] {
            operator
                .create_universe(OperatorUniverseCreateParams {
                    universe_id: universe_id.to_string(),
                })
                .await?;
        }
        operator
            .put_environment_provider(OperatorEnvironmentProviderPutParams {
                provider_id: provider_id.clone(),
                display_name: Some("Live fake provider".to_owned()),
                controller_connection: OperatorEnvironmentProviderConnection {
                    endpoint: "in-process".to_owned(),
                    transport: OperatorEnvironmentProviderTransport::Provider {
                        provider_type: "fake".to_owned(),
                    },
                },
                metadata: BTreeMap::new(),
            })
            .await?;
        for (universe_id, binding_id) in [(universe_a, "primary-a"), (universe_b, "primary-b")] {
            operator
                .put_environment_provider_binding(OperatorProviderBindingPutParams {
                    universe_id: universe_id.to_string(),
                    binding_id: binding_id.to_owned(),
                    provider_id: provider_id.clone(),
                    status: EnvironmentProviderBindingStatusView::Enabled,
                    metadata: BTreeMap::new(),
                    expected_revision: None,
                })
                .await?;
        }

        let state_a = runtime.state_for(universe_a, false).await?;
        let state_b = runtime.state_for(universe_b, false).await?;
        let templates = state_a
            .api
            .list_environment_templates(EnvironmentTemplateListParams {
                binding_id: Some("primary-a".to_owned()),
            })
            .await?;
        assert_eq!(templates.result.templates.len(), 1);
        assert_eq!(templates.result.templates[0].template_id, "rust-v1");

        let created = state_a
            .api
            .create_environment(EnvironmentCreateParams {
                request_id: format!("create-{suffix}"),
                binding_id: "primary-a".to_owned(),
                template_id: "rust-v1".to_owned(),
                display_name: Some("Provisioned live VM".to_owned()),
                metadata: BTreeMap::new(),
                idle_policy: None,
            })
            .await?
            .result
            .environment;
        let create_retry = state_a
            .api
            .create_environment(EnvironmentCreateParams {
                request_id: format!("create-{suffix}"),
                binding_id: "primary-a".to_owned(),
                template_id: "rust-v1".to_owned(),
                display_name: None,
                metadata: BTreeMap::new(),
                idle_policy: None,
            })
            .await?
            .result
            .environment;
        assert_eq!(create_retry.environment_id, created.environment_id);

        let adopted = operator
            .adopt_environment(OperatorEnvironmentAdoptParams {
                universe_id: universe_b.to_string(),
                request_id: format!("adopt-{suffix}"),
                binding_id: "primary-b".to_owned(),
                source_target: "legacy/hand-built-vm".to_owned(),
                take_ownership: true,
                display_name: Some("Adopted live VM".to_owned()),
                metadata: BTreeMap::new(),
            })
            .await?
            .result
            .environment;
        assert_eq!(adopted.incarnation.template_id, None);
        let adopt_retry = operator
            .adopt_environment(OperatorEnvironmentAdoptParams {
                universe_id: universe_b.to_string(),
                request_id: format!("adopt-{suffix}"),
                binding_id: "primary-b".to_owned(),
                source_target: "legacy/hand-built-vm".to_owned(),
                take_ownership: true,
                display_name: None,
                metadata: BTreeMap::new(),
            })
            .await?
            .result
            .environment;
        assert_eq!(adopt_retry.environment_id, adopted.environment_id);

        let created_ready = wait_for_status(
            state_a.api.as_ref(),
            &created.environment_id,
            EnvironmentLifecycleStatusView::Ready,
        )
        .await?;
        let adopted_ready = wait_for_status(
            state_b.api.as_ref(),
            &adopted.environment_id,
            EnvironmentLifecycleStatusView::Ready,
        )
        .await?;
        assert!(created_ready.incarnation.provider_target_id.is_some());
        assert!(adopted_ready.incarnation.provider_target_id.is_some());

        let listed_a = state_a
            .api
            .list_environments(EnvironmentListParams::default())
            .await?;
        let listed_b = state_b
            .api
            .list_environments(EnvironmentListParams::default())
            .await?;
        assert_eq!(listed_a.result.environments.len(), 1);
        assert_eq!(listed_b.result.environments.len(), 1);

        let ingress = state_a
            .api
            .put_environment_ingress(EnvironmentIngressPutParams {
                environment_id: created.environment_id.clone(),
                enabled: true,
            })
            .await?;
        assert_eq!(
            ingress.result.environment.public_endpoint.as_deref(),
            Some("https://fake.env.test")
        );
        state_a
            .api
            .put_environment_ingress(EnvironmentIngressPutParams {
                environment_id: created.environment_id.clone(),
                enabled: false,
            })
            .await?;

        for (api, environment_id) in [
            (state_a.api.as_ref(), created.environment_id.as_str()),
            (state_b.api.as_ref(), adopted.environment_id.as_str()),
        ] {
            api.close_environment(EnvironmentCloseParams {
                environment_id: environment_id.to_owned(),
            })
            .await?;
            wait_for_status(api, environment_id, EnvironmentLifecycleStatusView::Closed).await?;
        }
        anyhow::Ok(())
    }
    .await;

    reconciler.abort();
    let _ = reconciler.await;
    runtime.evict(universe_a).await;
    runtime.evict(universe_b).await;
    let _ = store_pg::delete_universe(&pool, universe_a).await;
    let _ = store_pg::delete_universe(&pool, universe_b).await;
    let _ = operator
        .delete_environment_provider(OperatorEnvironmentProviderDeleteParams { provider_id })
        .await;
    result
}

async fn wait_for_status(
    api: &temporal_server::gateway::GatewayAgentApi,
    environment_id: &str,
    expected: EnvironmentLifecycleStatusView,
) -> anyhow::Result<api::EnvironmentView> {
    let started = std::time::Instant::now();
    loop {
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
        if started.elapsed() > Duration::from_secs(10) {
            anyhow::bail!(
                "timed out waiting for environment {environment_id} to reach {expected:?}; current status is {:?}",
                environment.status
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn require_live_env() -> anyhow::Result<()> {
    if std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").is_err()
        && std::env::var("LIGHTSPEED_POSTGRES_URL").is_err()
    {
        anyhow::bail!(
            "LIGHTSPEED_TEST_POSTGRES_URL or LIGHTSPEED_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_environment_power_intent_converges_and_wakes_on_use() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_environment_power_live_client).await
}

/// Power intent is recorded through the API, converged by the lifecycle
/// reconciler against the provider, and a powered-down environment wakes
/// transparently when a session selects it. Idle policy round-trips and the
/// power reaper treats an environment without a reachable daemon as
/// untouchable.
async fn run_environment_power_live_client(
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
    let provider_id = format!("fake-power-{suffix}");
    let binding_id = format!("binding-power-{suffix}");
    store
        .put_provider(PutEnvironmentProvider {
            provider_id: EnvironmentProviderId::new(provider_id.clone()),
            display_name: Some("Live fake provider (power)".to_owned()),
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

    let policy = api::EnvironmentIdlePolicyView {
        pause_after_ms: Some(60_000),
        suspend_after_ms: None,
        stop_after_ms: Some(3_600_000),
        close_after_ms: None,
    };
    let created = api
        .create_environment(api::EnvironmentCreateParams {
            request_id: format!("power-{suffix}"),
            binding_id: binding_id.clone(),
            template_id: "rust-v1".to_owned(),
            display_name: Some("Power VM".to_owned()),
            metadata: BTreeMap::new(),
            idle_policy: Some(policy.clone()),
        })
        .await?
        .result
        .environment;
    let environment_id = created.environment_id.clone();
    assert_eq!(
        created.desired_power,
        api::EnvironmentPowerStateView::Running
    );
    assert_eq!(created.idle_policy.as_ref(), Some(&policy));
    assert!(created.incarnation.power_states.is_empty());

    // Before the provider reported power support, power changes are refused.
    let premature = api
        .put_environment_power(api::EnvironmentPowerPutParams {
            environment_id: environment_id.clone(),
            power: api::EnvironmentPowerStateView::Paused,
        })
        .await
        .expect_err("power change before observation is rejected");
    assert_eq!(premature.kind, api::AgentApiErrorKind::Rejected);

    let ready = wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Ready,
    )
    .await?;
    assert_eq!(
        ready.incarnation.power_states,
        vec![
            api::EnvironmentPowerStateView::Running,
            api::EnvironmentPowerStateView::Paused,
            api::EnvironmentPowerStateView::Suspended,
            api::EnvironmentPowerStateView::Stopped,
        ]
    );

    // A malformed idle policy is rejected; a valid replacement and a clear
    // round-trip.
    let bad_policy = api
        .put_environment_idle_policy(api::EnvironmentIdlePolicyPutParams {
            environment_id: environment_id.clone(),
            idle_policy: Some(api::EnvironmentIdlePolicyView {
                pause_after_ms: Some(10),
                stop_after_ms: Some(5),
                ..api::EnvironmentIdlePolicyView::default()
            }),
        })
        .await
        .expect_err("non-monotone idle policy is rejected");
    assert_eq!(bad_policy.kind, api::AgentApiErrorKind::InvalidRequest);
    let cleared = api
        .put_environment_idle_policy(api::EnvironmentIdlePolicyPutParams {
            environment_id: environment_id.clone(),
            idle_policy: None,
        })
        .await?
        .result
        .environment;
    assert!(cleared.idle_policy.is_none());
    let restored = api
        .put_environment_idle_policy(api::EnvironmentIdlePolicyPutParams {
            environment_id: environment_id.clone(),
            idle_policy: Some(policy.clone()),
        })
        .await?
        .result
        .environment;
    assert_eq!(restored.idle_policy.as_ref(), Some(&policy));

    // The reaper sees the candidate but cannot reach a daemon through the
    // fake provider, so it leaves the environment alone.
    let stats = api.reap_idle_environments_once().await?;
    assert_eq!(stats.candidates, 1);
    assert_eq!(stats.unreachable, 1);
    assert_eq!(stats.powered_down, 0);
    assert_eq!(stats.closed, 0);

    // Pause intent: recorded immediately, converged by the reconciler.
    let paused_intent = api
        .put_environment_power(api::EnvironmentPowerPutParams {
            environment_id: environment_id.clone(),
            power: api::EnvironmentPowerStateView::Paused,
        })
        .await?
        .result
        .environment;
    assert_eq!(
        paused_intent.desired_power,
        api::EnvironmentPowerStateView::Paused
    );
    assert_eq!(
        paused_intent.status,
        api::EnvironmentLifecycleStatusView::Ready
    );
    let paused = wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Paused,
    )
    .await?;
    assert_eq!(paused.desired_power, api::EnvironmentPowerStateView::Paused);
    // Paused is a filterable lifecycle status.
    assert!(
        api.list_environments(api::EnvironmentListParams {
            metadata: Default::default(),
            status: Some(api::EnvironmentLifecycleStatusView::Paused),
            ..api::EnvironmentListParams::default()
        })
        .await?
        .result
        .environments
        .iter()
        .any(|environment| environment.environment_id == environment_id)
    );

    // Wake-on-use: activating the paused environment for a session admits it
    // as intent and flips desired power back to running; the reconciler then
    // brings it to ready.
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(api::FeaturesConfig {
                environments: Some(api::EnvironmentsFeature {
                    tools: Some(api::EnvironmentToolSurface::Edit),
                    commands: true,
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
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;
    let started_view = read_session_view(&api, &session_id).await?;
    assert!(started_view.active_environment_id.is_none());
    api.activate_session_environment(api::SessionEnvironmentActivateParams {
        session_id: session_id.as_str().to_owned(),
        environment_id: environment_id.clone(),
    })
    .await?;
    let activated_view = read_session_view(&api, &session_id).await?;
    assert_eq!(
        activated_view.active_environment_id.as_deref(),
        Some(environment_id.as_str())
    );
    let woken = api
        .read_environment(api::EnvironmentReadParams {
            environment_id: environment_id.clone(),
        })
        .await?
        .result
        .environment;
    assert_eq!(woken.desired_power, api::EnvironmentPowerStateView::Running);
    wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Ready,
    )
    .await?;

    // Suspend and stop are ordinary intents on a provider that supports them.
    api.put_environment_power(api::EnvironmentPowerPutParams {
        environment_id: environment_id.clone(),
        power: api::EnvironmentPowerStateView::Suspended,
    })
    .await?;
    wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Suspended,
    )
    .await?;
    // Wake-on-use through the jobs API: creating a job against the
    // suspended environment fails typed `environment_not_ready` (never a
    // generic rejection) and flips desired power back to running — the
    // retry-with-backoff contract polling automations lean on.
    let job_not_ready = api
        .create_environment_jobs(api::EnvironmentJobCreateParams {
            environment_id: environment_id.clone(),
            request_id: format!("wake-probe-{suffix}"),
            jobs: vec![api::SessionJobStartSpecInput {
                name: Some("wake-probe".to_owned()),
                job_id: None,
                argv: vec!["true".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: None,
                depends_on: Vec::new(),
                dependency_policy: api::SessionJobDependencyPolicyView::default(),
                queue_key: None,
            }],
        })
        .await
        .expect_err("jobs/create against a suspended environment is not ready");
    assert_eq!(
        job_not_ready.kind,
        api::AgentApiErrorKind::EnvironmentNotReady
    );
    let waking = api
        .read_environment(api::EnvironmentReadParams {
            environment_id: environment_id.clone(),
        })
        .await?
        .result
        .environment;
    assert_eq!(
        waking.desired_power,
        api::EnvironmentPowerStateView::Running
    );

    api.put_environment_power(api::EnvironmentPowerPutParams {
        environment_id: environment_id.clone(),
        power: api::EnvironmentPowerStateView::Stopped,
    })
    .await?;
    wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Offline,
    )
    .await?;
    api.put_environment_power(api::EnvironmentPowerPutParams {
        environment_id: environment_id.clone(),
        power: api::EnvironmentPowerStateView::Running,
    })
    .await?;
    wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Ready,
    )
    .await?;

    // External environments have no power control.
    let external = api
        .create_external_environment(api::EnvironmentExternalCreateParams {
            request_id: format!("power-external-{suffix}"),
            connection: api::EnvironmentConnectionView {
                endpoint: format!("ws://127.0.0.1:1/{suffix}"),
                transport: api::EnvironmentConnectionTransportView::WebSocket,
            },
            display_name: None,
            metadata: BTreeMap::new(),
        })
        .await?
        .result
        .environment;
    let external_rejected = api
        .put_environment_power(api::EnvironmentPowerPutParams {
            environment_id: external.environment_id.clone(),
            power: api::EnvironmentPowerStateView::Paused,
        })
        .await
        .expect_err("external environments have no power control");
    assert_eq!(external_rejected.kind, api::AgentApiErrorKind::Rejected);
    let external_policy_rejected = api
        .put_environment_idle_policy(api::EnvironmentIdlePolicyPutParams {
            environment_id: external.environment_id.clone(),
            idle_policy: Some(policy.clone()),
        })
        .await
        .expect_err("external environments have no idle policy");
    assert_eq!(
        external_policy_rejected.kind,
        api::AgentApiErrorKind::InvalidRequest
    );

    // Closing wins over power intent.
    api.close_session(api::SessionCloseParams {
        session_id: session_id.as_str().to_owned(),
        force: false,
    })
    .await?;
    api.close_environment(api::EnvironmentCloseParams {
        environment_id: environment_id.clone(),
    })
    .await?;
    wait_for_environment_status(
        &api,
        &environment_id,
        api::EnvironmentLifecycleStatusView::Closed,
    )
    .await?;
    let closed_rejected = api
        .put_environment_power(api::EnvironmentPowerPutParams {
            environment_id: environment_id.clone(),
            power: api::EnvironmentPowerStateView::Running,
        })
        .await
        .expect_err("closed environments cannot change power");
    assert_eq!(closed_rejected.kind, api::AgentApiErrorKind::InvalidRequest);
    api.close_environment(api::EnvironmentCloseParams {
        environment_id: external.environment_id.clone(),
    })
    .await?;
    wait_for_environment_status(
        &api,
        &external.environment_id,
        api::EnvironmentLifecycleStatusView::Closed,
    )
    .await?;

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
