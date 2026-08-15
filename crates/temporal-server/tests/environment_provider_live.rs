//! Minimal live environment control-plane acceptance test.
//!
//! This deliberately uses the in-process provider with real Postgres and the
//! real deployment reconciler. It covers Lightspeed's durable P118/P121 seams
//! without requiring an Incus installation; the separate Incus live smoke test
//! owns backend connectivity and topology.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use api::{
    AgentApiService, EnvironmentCloseParams, EnvironmentCreateParams, EnvironmentIngressPutParams,
    EnvironmentLifecycleStatusView, EnvironmentListParams, EnvironmentProviderBindingStatusView,
    EnvironmentTemplateListParams, OperatorApiService, OperatorEnvironmentAdoptParams,
    OperatorEnvironmentProviderConnection, OperatorEnvironmentProviderDeleteParams,
    OperatorEnvironmentProviderPutParams, OperatorEnvironmentProviderTransport,
    OperatorProviderBindingPutParams, OperatorUniverseCreateParams,
};
use temporal_server::{DeploymentStores, UniverseRuntime, gateway::GatewayOperatorApi};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires npm run dev -- infra or compatible Temporal + Postgres env"]
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
            "LIGHTSPEED_TEST_POSTGRES_URL or LIGHTSPEED_POSTGRES_URL must be set; run npm run dev -- infra and source dev/env.sh"
        );
    }
    Ok(())
}
