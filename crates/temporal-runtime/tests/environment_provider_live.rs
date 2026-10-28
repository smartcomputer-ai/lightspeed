//! Live environment provider acceptance tests.
//!
//! These use the fake controller, served on loopback and registered like
//! any provider, with real Postgres and the real reconciler. They cover the
//! provider seams without requiring an Incus installation: create until
//! accepted and opened by one handshake, close until gone, the
//! synchronous power and ingress pass-throughs, wake on a data route dial,
//! the display sweep's provider-ended close, and per-row backoff while a
//! provider is down. The separate Incus live smoke test owns backend
//! connectivity and topology.

mod support;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use api::{
    AgentApiService, DeploymentApiService, DeploymentEnvironmentProviderDeleteParams,
    DeploymentEnvironmentProviderPutParams, DeploymentProviderBindingDeleteParams,
    DeploymentProviderBindingPutParams, DeploymentUniverseCreateParams, EnvironmentCloseParams,
    EnvironmentCloseReasonView, EnvironmentCreateParams, EnvironmentIngressPutParams,
    EnvironmentListParams, EnvironmentPowerPutParams, EnvironmentPowerStateView,
    EnvironmentSourceView, EnvironmentStateView, EnvironmentStatusView,
    EnvironmentTemplateListParams, EnvironmentView,
};
use environment_client::conformance::data_route::expect_refusal;
use environment_protocol::{
    control::target::TargetOwner,
    ids::{EnvironmentId as ProtocolEnvironmentId, ProviderTargetId, UniverseId},
    routes::{DataRouteBase, DataRouteRefusal, provider_data_route},
};
use environments::EnvironmentStore;
use support::live::wait_for_environment_state;
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    environments::fake_controller::{
        FAKE_INGRESS_ENDPOINT, FAKE_TEMPLATE_ID, FakeController, FakeControllerOptions,
    },
    gateway::{
        GatewayAgentApi, GatewayDeploymentApi,
        request_context::{RequestContext, with_request_context as calling},
    },
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use uuid::Uuid;

async fn live_runtime(name: &str) -> anyhow::Result<(Arc<UniverseRuntime>, sqlx::PgPool)> {
    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let stores = DeploymentStores::from_env().await?;
    let pool = stores.pool().clone();
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        format!("{name}-{}", Uuid::new_v4().simple()),
        Some("http://127.0.0.1:18080".to_owned()),
        stores,
    )?);
    Ok((runtime, pool))
}

fn deployment_caller() -> RequestContext {
    RequestContext::local(api::AccessScope::Deployment)
}

fn universe_caller(universe_id: Uuid) -> RequestContext {
    RequestContext::local(api::AccessScope::Universe { universe_id })
}

/// Create a universe, register `controller` as `provider_id` and bind it.
async fn provider_universe(
    operator: &GatewayDeploymentApi,
    universe_id: Uuid,
    provider_id: &str,
    controller: &FakeController,
) -> anyhow::Result<()> {
    calling(
        deployment_caller(),
        operator.create_universe(DeploymentUniverseCreateParams {
            slug: None,
            universe_id: universe_id.to_string(),
        }),
    )
    .await?;
    calling(
        deployment_caller(),
        operator.put_environment_provider(DeploymentEnvironmentProviderPutParams {
            provider_id: provider_id.to_owned(),
            display_name: Some("Live fake provider".to_owned()),
            controller_endpoint: controller.controller_url().to_owned(),
            data_route_base: None,
            credential: controller.credential().map(str::to_owned),
            clear_credential: false,
        }),
    )
    .await?;
    calling(
        deployment_caller(),
        operator.put_environment_provider_binding(DeploymentProviderBindingPutParams {
            universe_id: universe_id.to_string(),
            provider_id: provider_id.to_owned(),
            enabled: true,
        }),
    )
    .await?;
    Ok(())
}

/// Remove what [`provider_universe`] made, best effort.
async fn cleanup(
    runtime: &UniverseRuntime,
    operator: &GatewayDeploymentApi,
    pool: &sqlx::PgPool,
    universe_id: Uuid,
    provider_id: &str,
) {
    let _ = calling(
        deployment_caller(),
        operator.delete_environment_provider_binding(DeploymentProviderBindingDeleteParams {
            universe_id: universe_id.to_string(),
            provider_id: provider_id.to_owned(),
        }),
    )
    .await;
    runtime.evict(universe_id).await;
    let _ = store_pg::delete_universe(pool, universe_id).await;
    let _ = calling(
        deployment_caller(),
        operator.delete_environment_provider(DeploymentEnvironmentProviderDeleteParams {
            provider_id: provider_id.to_owned(),
        }),
    )
    .await;
}

fn create_params(request_id: &str, provider_id: &str) -> EnvironmentCreateParams {
    EnvironmentCreateParams {
        request_id: request_id.to_owned(),
        provider_id: provider_id.to_owned(),
        template_id: FAKE_TEMPLATE_ID.to_owned(),
        display_name: Some("Provisioned live VM".to_owned()),
        metadata: BTreeMap::new(),
        idle: None,
    }
}

/// Poll a read until `done` holds, with the background reconciler doing
/// the work.
async fn wait_for(
    api: &GatewayAgentApi,
    environment_id: &str,
    what: &str,
    done: impl Fn(&EnvironmentView) -> bool,
) -> anyhow::Result<EnvironmentView> {
    let started = std::time::Instant::now();
    loop {
        let environment = api
            .read_environment(api::EnvironmentReadParams {
                environment_id: environment_id.to_owned(),
            })
            .await?
            .result
            .environment;
        if done(&environment) {
            return Ok(environment);
        }
        if started.elapsed() > Duration::from_secs(15) {
            anyhow::bail!(
                "timed out waiting for environment {environment_id} to {what}; it is {:?} ({:?})",
                environment.state,
                environment.state_message
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn target_id(environment: &EnvironmentView) -> Option<String> {
    match &environment.source {
        EnvironmentSourceView::Provisioned { target_id, .. } => target_id.clone(),
        _ => None,
    }
}

fn public_endpoint(environment: &EnvironmentView) -> Option<String> {
    match &environment.source {
        EnvironmentSourceView::Provisioned {
            public_endpoint, ..
        } => public_endpoint.clone(),
        _ => None,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn environment_provider_lifecycle_round_trip() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    require_live_env()?;
    let (runtime, pool) = live_runtime("environment-provider-live").await?;
    let operator = GatewayDeploymentApi::new(runtime.clone());
    let suffix = Uuid::new_v4().simple().to_string();
    let guarded_id = format!("fake-live-{suffix}");
    let open_id = format!("fake-open-{suffix}");
    let universe_a = Uuid::new_v4();
    let universe_b = Uuid::new_v4();
    let guarded = FakeController::start().await?;
    let open = FakeController::start_with(FakeControllerOptions {
        credential: false,
        ..FakeControllerOptions::default()
    })
    .await?;
    let reconciler = tokio::spawn(runtime.clone().run_environment_reconciler());

    let result = async {
        provider_universe(&operator, universe_a, &guarded_id, &guarded).await?;
        provider_universe(&operator, universe_b, &open_id, &open).await?;

        // Registration ran the handshake; nothing it advertised is stored,
        // and the data route base is derived from the controller endpoint.
        let read = |provider_id: String| {
            calling(
                deployment_caller(),
                operator.read_environment_provider(api::DeploymentEnvironmentProviderReadParams {
                    provider_id,
                }),
            )
        };
        let guarded_view = read(guarded_id.clone()).await?.result.provider;
        assert!(guarded_view.has_credential);
        assert_eq!(guarded_view.data_route_base, None);
        let effective = guarded_view
            .effective_data_route_base
            .clone()
            .expect("a ws:// controller has a derivable data route base");
        assert!(effective.ends_with("/routes"), "{effective}");
        assert!(!read(open_id.clone()).await?.result.provider.has_credential);
        // A controller URL that names no controller is refused outright, and
        // so is a guarded controller registered without its credential.
        for (controller_endpoint, credential) in [
            (
                guarded.controller_url().replace("/control", "/elsewhere"),
                guarded.credential().map(str::to_owned),
            ),
            (guarded.controller_url().to_owned(), None),
        ] {
            let refused = calling(
                deployment_caller(),
                operator.put_environment_provider(DeploymentEnvironmentProviderPutParams {
                    provider_id: format!("{guarded_id}-refused"),
                    display_name: None,
                    controller_endpoint,
                    data_route_base: None,
                    credential,
                    clear_credential: false,
                }),
            )
            .await
            .expect_err("a definitive handshake refusal is refused");
            assert_eq!(refused.kind, api::AgentApiErrorKind::InvalidRequest);
        }
        let exclusive = calling(
            deployment_caller(),
            operator.put_environment_provider(DeploymentEnvironmentProviderPutParams {
                provider_id: guarded_id.clone(),
                display_name: None,
                controller_endpoint: guarded.controller_url().to_owned(),
                data_route_base: None,
                credential: Some("token".to_owned()),
                clear_credential: true,
            }),
        )
        .await
        .expect_err("credential and clearCredential exclude each other");
        assert_eq!(exclusive.kind, api::AgentApiErrorKind::InvalidRequest);

        let state_a = runtime.state_for(universe_a, false).await?;
        let state_b = runtime.state_for(universe_b, false).await?;
        let caller_a = universe_caller(universe_a);
        let caller_b = universe_caller(universe_b);
        let api_a = state_a.api.as_ref();
        let api_b = state_b.api.as_ref();

        let templates = calling(
            caller_a.clone(),
            api_a.list_environment_templates(EnvironmentTemplateListParams {
                provider_id: Some(guarded_id.clone()),
            }),
        )
        .await?
        .result
        .templates;
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].template_id, FAKE_TEMPLATE_ID);
        assert_eq!(templates[0].provider_id, guarded_id);
        assert_eq!(templates[0].power_states.len(), 4);
        assert!(templates[0].public_ingress);

        // Create until accepted, open after one handshake.
        let created = calling(
            caller_a.clone(),
            api_a.create_environment(create_params(&format!("create-{suffix}"), &guarded_id)),
        )
        .await?
        .result
        .environment;
        assert_eq!(created.state, EnvironmentStateView::Provisioning);
        let retried = calling(
            caller_a.clone(),
            api_a.create_environment(create_params(&format!("create-{suffix}"), &guarded_id)),
        )
        .await?
        .result
        .environment;
        assert_eq!(retried.environment_id, created.environment_id);
        let environment_id = created.environment_id.clone();
        let opened = calling(
            caller_a.clone(),
            wait_for(api_a, &environment_id, "open", |environment| {
                environment.state == EnvironmentStateView::Open
            }),
        )
        .await?;
        assert_eq!(opened.status, EnvironmentStatusView::Ready);
        assert_eq!(
            opened.daemon.as_ref().map(|daemon| daemon.protocol_version),
            Some(environment_protocol::ENVD_PROTOCOL_VERSION)
        );
        let target = target_id(&opened).expect("an open environment has a target");

        // A provider without a credential works the same way.
        let open_created = calling(
            caller_b.clone(),
            api_b.create_environment(create_params(&format!("open-{suffix}"), &open_id)),
        )
        .await?
        .result
        .environment;
        calling(
            caller_b.clone(),
            wait_for(api_b, &open_created.environment_id, "open", |environment| {
                environment.state == EnvironmentStateView::Open
            }),
        )
        .await?;

        // Pages of one; each universe sees only its own environments.
        let second = calling(
            caller_a.clone(),
            api_a.create_environment(create_params(&format!("second-{suffix}"), &guarded_id)),
        )
        .await?
        .result
        .environment;
        calling(
            caller_a.clone(),
            wait_for(api_a, &second.environment_id, "open", |environment| {
                environment.state == EnvironmentStateView::Open
            }),
        )
        .await?;
        let first_page = calling(
            caller_a.clone(),
            api_a.list_environments(EnvironmentListParams {
                limit: Some(1),
                ..EnvironmentListParams::default()
            }),
        )
        .await?
        .result;
        assert_eq!(first_page.environments.len(), 1);
        assert!(first_page.next_after.is_some());
        let next_page = calling(
            caller_a.clone(),
            api_a.list_environments(EnvironmentListParams {
                limit: Some(1),
                after: first_page.next_after.clone(),
                ..EnvironmentListParams::default()
            }),
        )
        .await?
        .result;
        assert_eq!(next_page.environments.len(), 1);
        assert_ne!(
            next_page.environments[0].environment_id,
            first_page.environments[0].environment_id
        );
        // The last page carries no cursor.
        assert_eq!(next_page.next_after, None);
        let listed_b = calling(
            caller_b.clone(),
            api_b.list_environments(EnvironmentListParams::default()),
        )
        .await?
        .result;
        assert_eq!(listed_b.environments.len(), 1);

        // Power is a synchronous pass-through: the answer is recorded.
        let paused = calling(
            caller_a.clone(),
            api_a.put_environment_power(EnvironmentPowerPutParams {
                environment_id: environment_id.clone(),
                power: EnvironmentPowerStateView::Paused,
            }),
        )
        .await?
        .result
        .environment;
        assert_eq!(paused.state, EnvironmentStateView::Open);
        assert_eq!(paused.status, EnvironmentStatusView::Paused);
        // The data route dial is the wake signal: a paused target refuses
        // `waking` while it resumes, then upgrades.
        let route = provider_data_route(
            &DataRouteBase::new(effective.clone())?,
            &TargetOwner {
                universe_id: UniverseId::new(universe_a.to_string())?,
                environment_id: ProtocolEnvironmentId::new(environment_id.clone())?,
            },
            &ProviderTargetId::new(target.clone())?,
        );
        let options = || {
            environment_client::ConnectOptions::new(
                temporal_runtime::environments::gateway::runtime_client_info(),
            )
            .with_bearer_token(guarded.credential().expect("guarded credential"))
        };
        assert_eq!(
            expect_refusal(&route, options())
                .await
                .map_err(anyhow::Error::msg)?,
            DataRouteRefusal::Waking {}
        );
        let (client, _) = environment_client::DataClient::connect_initialized(&route, options())
            .await
            .expect("the woken target upgrades");
        let _ = client.close().await;

        // Ingress is a synchronous pass-through too; the endpoint is cached.
        let ingress = calling(
            caller_a.clone(),
            api_a.put_environment_ingress(EnvironmentIngressPutParams {
                environment_id: environment_id.clone(),
                enabled: true,
            }),
        )
        .await?
        .result
        .environment;
        assert_eq!(
            public_endpoint(&ingress).as_deref(),
            Some(FAKE_INGRESS_ENDPOINT)
        );
        let removed = calling(
            caller_a.clone(),
            api_a.put_environment_ingress(EnvironmentIngressPutParams {
                environment_id: environment_id.clone(),
                enabled: false,
            }),
        )
        .await?
        .result
        .environment;
        assert_eq!(public_endpoint(&removed), None);

        // External environments have no provider to pass through to.
        let external = calling(
            caller_a.clone(),
            api_a.create_external_environment(api::EnvironmentExternalCreateParams {
                request_id: format!("external-{suffix}"),
                endpoint: format!("ws://127.0.0.1:1/{suffix}"),
                display_name: None,
                metadata: BTreeMap::new(),
            }),
        )
        .await?
        .result
        .environment;
        let external_power = calling(
            caller_a.clone(),
            api_a.put_environment_power(EnvironmentPowerPutParams {
                environment_id: external.environment_id.clone(),
                power: EnvironmentPowerStateView::Paused,
            }),
        )
        .await
        .expect_err("external environments have no power control");
        assert_eq!(external_power.kind, api::AgentApiErrorKind::Rejected);
        let closed_external = calling(
            caller_a.clone(),
            api_a.close_environment(EnvironmentCloseParams {
                environment_id: external.environment_id.clone(),
            }),
        )
        .await?
        .result
        .environment;
        assert_eq!(closed_external.state, EnvironmentStateView::Closed);

        // A target the provider removed on its own ends its environment at
        // the next display sweep.
        assert!(guarded.end_target(&second.environment_id).await);
        calling(caller_a.clone(), api_a.reconcile_environments_once()).await?;
        let ended = calling(
            caller_a.clone(),
            wait_for(api_a, &second.environment_id, "end", |environment| {
                environment.state == EnvironmentStateView::Closed
            }),
        )
        .await?;
        assert_eq!(
            ended.close_reason,
            Some(EnvironmentCloseReasonView::ProviderEnded)
        );

        // Close until gone.
        for (api, caller, environment_id) in [
            (api_a, &caller_a, environment_id.as_str()),
            (api_b, &caller_b, open_created.environment_id.as_str()),
        ] {
            calling(caller.clone(), async {
                api.close_environment(EnvironmentCloseParams {
                    environment_id: environment_id.to_owned(),
                })
                .await?;
                let closed = wait_for(api, environment_id, "close", |environment| {
                    environment.state == EnvironmentStateView::Closed
                })
                .await?;
                assert_eq!(
                    closed.close_reason,
                    Some(EnvironmentCloseReasonView::Requested)
                );
                anyhow::Ok(())
            })
            .await?;
        }
        assert_eq!(guarded.target_count().await, 0);
        assert_eq!(open.target_count().await, 0);
        let closed_power = calling(
            caller_a.clone(),
            api_a.put_environment_power(EnvironmentPowerPutParams {
                environment_id: environment_id.clone(),
                power: EnvironmentPowerStateView::Running,
            }),
        )
        .await
        .expect_err("a closed environment has no power control");
        assert_eq!(closed_power.kind, api::AgentApiErrorKind::Rejected);
        anyhow::Ok(())
    }
    .await;

    reconciler.abort();
    let _ = reconciler.await;
    cleanup(&runtime, &operator, &pool, universe_a, &guarded_id).await;
    cleanup(&runtime, &operator, &pool, universe_b, &open_id).await;
    result
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn environment_provider_outage_backs_off_per_row() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    require_live_env()?;
    let (runtime, pool) = live_runtime("environment-provider-outage-live").await?;
    let operator = GatewayDeploymentApi::new(runtime.clone());
    let suffix = Uuid::new_v4().simple().to_string();
    let provider_id = format!("fake-outage-{suffix}");
    let universe_id = Uuid::new_v4();
    let controller = FakeController::start().await?;

    let result = async {
        provider_universe(&operator, universe_id, &provider_id, &controller).await?;
        let state = runtime.state_for(universe_id, false).await?;
        let caller = universe_caller(universe_id);
        let api = state.api.as_ref();
        controller.set_reachable(false);
        let created = calling(
            caller.clone(),
            api.create_environment(create_params(&format!("outage-{suffix}"), &provider_id)),
        )
        .await?
        .result
        .environment;
        let environment_id = environments::EnvironmentId::new(created.environment_id.clone());
        // Each pass that cannot reach the provider counts one attempt and
        // pushes the row's next attempt out; the row stays provisioning.
        for expected in 1..=2_u32 {
            calling(caller.clone(), api.reconcile_environments_once()).await?;
            let record = state.store.read_environment(&environment_id).await?;
            assert_eq!(record.state, environments::EnvironmentState::Provisioning);
            let source = record.provisioned().expect("provisioned");
            assert_eq!(source.attempts, expected);
            assert!(
                source.next_attempt_at_ms.expect("scheduled") > record.updated_at_ms,
                "the next attempt is backed off"
            );
            assert!(
                record
                    .state_message
                    .as_deref()
                    .is_some_and(|message| message.contains("unreachable")),
                "{:?}",
                record.state_message
            );
        }
        controller.set_reachable(true);
        calling(
            caller.clone(),
            wait_for_environment_state(api, &created.environment_id, EnvironmentStateView::Open),
        )
        .await?;
        let opened = state.store.read_environment(&environment_id).await?;
        assert_eq!(opened.state_message, None);
        assert_eq!(opened.provisioned().expect("provisioned").attempts, 0);

        // A close while the provider is down waits for it; nothing is
        // abandoned.
        controller.set_reachable(false);
        calling(
            caller.clone(),
            api.close_environment(EnvironmentCloseParams {
                environment_id: created.environment_id.clone(),
            }),
        )
        .await?;
        calling(caller.clone(), api.reconcile_environments_once()).await?;
        let closing = state.store.read_environment(&environment_id).await?;
        assert_eq!(closing.state, environments::EnvironmentState::Closing);
        assert_eq!(closing.provisioned().expect("provisioned").attempts, 1);
        controller.set_reachable(true);
        calling(
            caller.clone(),
            wait_for_environment_state(api, &created.environment_id, EnvironmentStateView::Closed),
        )
        .await?;
        assert_eq!(controller.target_count().await, 0);
        anyhow::Ok(())
    }
    .await;

    cleanup(&runtime, &operator, &pool, universe_id, &provider_id).await;
    result
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
