//! Live acceptance for addressing universes through the deployment API: a
//! universe is named when it is created, reached by UUID or handle, renamed
//! only explicitly, and purged under either name.

use std::sync::Arc;

use api::{
    AgentApiErrorKind, DeploymentApiService, DeploymentUniverseCreateParams,
    DeploymentUniverseDeleteParams, DeploymentUniverseHandlePutParams,
    DeploymentUniverseReadParams,
};
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    gateway::{
        GatewayDeploymentApi,
        request_context::{RequestContext, with_request_context as calling},
    },
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use uuid::Uuid;

fn deployment_caller() -> RequestContext {
    RequestContext::local(api::AccessScope::Deployment)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn universes_are_addressed_by_uuid_or_handle() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    if std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").is_err()
        && std::env::var("LIGHTSPEED_POSTGRES_URL").is_err()
    {
        anyhow::bail!(
            "LIGHTSPEED_TEST_POSTGRES_URL or LIGHTSPEED_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh"
        );
    }
    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let stores = DeploymentStores::from_env().await?;
    let pool = stores.pool().clone();
    let runtime = Arc::new(UniverseRuntime::new(
        connect_temporal(&temporal_target, &namespace).await?,
        format!("deployment-universes-live-{}", Uuid::new_v4().simple()),
        Some("http://127.0.0.1:18080".to_owned()),
        stores,
    )?);
    let operator = GatewayDeploymentApi::new(runtime);

    let named = Uuid::new_v4();
    let derived = Uuid::new_v4();
    let suffix = &Uuid::new_v4().simple().to_string()[..12];
    let handle = format!("live-{suffix}");
    let renamed = format!("live-renamed-{suffix}");

    let result = async {
        let create = |universe_id: Uuid, handle: Option<&str>| {
            calling(
                deployment_caller(),
                operator.create_universe(DeploymentUniverseCreateParams {
                    universe_id: universe_id.to_string(),
                    handle: handle.map(str::to_owned),
                }),
            )
        };
        let read = |universe: &str| {
            calling(
                deployment_caller(),
                operator.read_universe(DeploymentUniverseReadParams {
                    universe: universe.to_owned(),
                }),
            )
        };
        let rename = |universe: &str, handle: &str| {
            calling(
                deployment_caller(),
                operator.put_universe_handle(DeploymentUniverseHandlePutParams {
                    universe: universe.to_owned(),
                    handle: handle.to_owned(),
                }),
            )
        };

        // Named at creation, or by derivation from the UUID.
        let created = create(named, Some(&handle)).await?.result;
        assert!(created.created);
        assert_eq!(created.universe.handle, handle);
        let created = create(derived, None).await?.result;
        assert_eq!(
            created.universe.handle,
            store_pg::default_universe_handle(derived)
        );

        // Creating again reports the existing universe and never renames it.
        let again = create(named, Some("some-other-name")).await?.result;
        assert!(!again.created);
        assert_eq!(again.universe.handle, handle);

        // A name is refused when it is malformed or already taken.
        for refused in ["Research", "my.universe", &Uuid::new_v4().to_string()] {
            let error = create(Uuid::new_v4(), Some(refused)).await.unwrap_err();
            assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest, "{refused}");
        }
        let error = create(Uuid::new_v4(), Some(&handle)).await.unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::Conflict);

        // Either form reaches the same universe.
        for reference in [named.to_string(), handle.clone()] {
            let universe = read(&reference).await?.result.universe;
            assert_eq!(universe.universe_id, named.to_string());
            assert_eq!(universe.handle, handle);
        }
        for unknown in [Uuid::new_v4().to_string(), "nobody-here".to_owned()] {
            let error = read(&unknown).await.unwrap_err();
            assert_eq!(error.kind, AgentApiErrorKind::NotFound, "{unknown}");
        }

        // A rename addressed by the current handle releases it at once.
        let universe = rename(&handle, &renamed).await?.result.universe;
        assert_eq!(universe.universe_id, named.to_string());
        assert_eq!(universe.handle, renamed);
        assert_eq!(
            read(&handle).await.unwrap_err().kind,
            AgentApiErrorKind::NotFound
        );
        assert_eq!(
            read(&renamed).await?.result.universe.universe_id,
            named.to_string()
        );
        let error = rename(&derived.to_string(), &renamed).await.unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::Conflict);
        let error = rename(&derived.to_string(), "Not A Handle")
            .await
            .unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);

        // A purge addressed by handle removes that universe and frees its name.
        let purged = calling(
            deployment_caller(),
            operator.delete_universe(DeploymentUniverseDeleteParams {
                universe: renamed.clone(),
            }),
        )
        .await?
        .result;
        assert_eq!(purged.universe_id, named.to_string());
        assert_eq!(
            read(&renamed).await.unwrap_err().kind,
            AgentApiErrorKind::NotFound
        );
        let universe = rename(&derived.to_string(), &renamed)
            .await?
            .result
            .universe;
        assert_eq!(universe.universe_id, derived.to_string());
        anyhow::Ok(())
    }
    .await;

    for universe_id in [named, derived] {
        let _ = store_pg::delete_universe(&pool, universe_id).await;
    }
    result
}
