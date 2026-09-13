//! Live coverage for universe isolation and API-key tenant scoping.

mod support;

use std::{env, sync::Arc, time::Duration};

use api::{
    AgentApiErrorKind, AgentApiService, AgentProfileInput, ProfileCreateParams, ProfileDocument,
    ProfileId, ProfileListParams, ProfileReadParams, SessionReadParams, SessionStartParams,
    SessionStatus,
};
use engine::SessionId;
use support::live::{LIVE_TEST_LOCK, require_storage_live_env};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    worker::{WorkerActivities, core_runtime, worker_with_activities},
};
use temporal_workflow::{
    AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::{WorkflowQueryOptions, WorkflowTerminateOptions};

/// Isolation: two universes served by ONE worker on ONE shared task
/// queue. The same client-chosen session id exists independently in both
/// universes (distinct composed workflow ids), reads and registry listings
/// never cross universes, and closing one universe's session leaves the
/// other's untouched.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, and Temporal"]
async fn temporal_live_two_universes_share_one_worker_with_isolation() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let task_queue = format!("lightspeed-agent-live-{}", uuid::Uuid::new_v4().simple());
    let temporal_target =
        env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace =
        env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let stores = DeploymentStores::from_env().await?;
    let universes = Arc::new(UniverseRuntime::new(
        client.clone(),
        task_queue.clone(),
        None,
        stores,
    )?);

    let activities = WorkerActivities::with_runtime(universes.clone());
    let mut worker =
        worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = worker.run();
    tokio::pin!(worker_future);

    let universe_a = uuid::Uuid::new_v4();
    let universe_b = uuid::Uuid::new_v4();
    let session_id = SessionId::new(format!("session_shared_{}", uuid::Uuid::new_v4().simple()));

    let client_future = async {
        let state_a = universes.state_for(universe_a, true).await?;
        let state_b = universes.state_for(universe_b, true).await?;
        let api_a = state_a.api.clone();
        let api_b = state_b.api.clone();

        // The same client-chosen session id starts independently in both
        // universes on the same queue, served by the same worker.
        for api in [api_a.as_ref(), api_b.as_ref()] {
            let started = api
                .start_session(SessionStartParams {
                    metadata: Default::default(),
                    session_id: Some(session_id.as_str().to_owned()),
                    display_name: None,
                    config: None,
                    profile: None,
                    delete_after_close_ms: None,
                })
                .await?;
            assert_eq!(started.result.session.status, SessionStatus::Idle);
        }

        // Distinct workflows: both composed ids are queryable and healthy.
        for universe_id in [universe_a, universe_b] {
            let workflow_id = temporal_workflow::compose_workflow_id(universe_id, &session_id);
            let handle = client.get_workflow_handle::<AgentSessionWorkflow>(workflow_id);
            let status = handle
                .query(
                    AgentSessionWorkflow::status,
                    (),
                    WorkflowQueryOptions::default(),
                )
                .await?;
            assert_eq!(status.last_error, None);
            assert_eq!(status.session_id, session_id.as_str());
        }

        // Registry isolation: a profile created in A is invisible in B, and a
        // read through B reports not-found (no existence leak).
        let profile_id = ProfileId::new(format!(
            "tenant.isolation.{}",
            uuid::Uuid::new_v4().simple()
        ));
        api_a
            .create_profile(ProfileCreateParams {
                profile: AgentProfileInput {
                    profile_id: profile_id.clone(),
                    display_name: Some("Tenant isolation".to_owned()),
                    description: None,
                    document: ProfileDocument::default(),
                },
            })
            .await?;
        let listed_b = api_b.list_profiles(ProfileListParams {}).await?;
        assert!(
            listed_b
                .result
                .profiles
                .iter()
                .all(|profile| profile.profile_id != profile_id),
            "universe B must not list universe A's profile"
        );
        let read_b = api_b
            .read_profile(ProfileReadParams {
                profile_id: profile_id.clone(),
            })
            .await;
        match read_b {
            Err(error) => assert_eq!(error.kind, AgentApiErrorKind::NotFound),
            Ok(_) => anyhow::bail!("universe B must not read universe A's profile"),
        }

        // Closing A's session leaves B's session open.
        api_a
            .close_session(api::SessionCloseParams {
                force: false,
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        let closed_a = api_a
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            })
            .await?;
        assert_eq!(closed_a.result.session.status, SessionStatus::Closed);
        let open_b = api_b
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            })
            .await?;
        assert_eq!(open_b.result.session.status, SessionStatus::Idle);

        // Cleanup: terminate both workflows.
        for universe_id in [universe_a, universe_b] {
            let workflow_id = temporal_workflow::compose_workflow_id(universe_id, &session_id);
            let handle = client.get_workflow_handle::<AgentSessionWorkflow>(workflow_id);
            let _ = handle
                .terminate(
                    WorkflowTerminateOptions::builder()
                        .reason("tenant isolation live test cleanup")
                        .build(),
                )
                .await;
        }
        anyhow::Ok(())
    };
    tokio::pin!(client_future);

    let client_result = tokio::select! {
        worker_result = worker_future.as_mut() => {
            return match worker_result {
                Ok(()) => Err(anyhow::anyhow!("Temporal worker stopped before the live test completed")),
                Err(error) => Err(error.context("Temporal worker failed")),
            };
        }
        client_result = client_future.as_mut() => client_result,
    };

    shutdown_worker();
    tokio::time::timeout(Duration::from_secs(10), worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 10 seconds"))??;
    client_result
}

/// `api-key` auth mode end to end over HTTP. Keys resolve to
/// their universe, foreign-universe reads miss, requests without/with bad
/// credentials fail closed, tenant headers are rejected in api-key mode, and
/// revocation takes effect immediately.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, and Temporal"]
async fn temporal_live_api_key_mode_scopes_requests() -> anyhow::Result<()> {
    use auth::ApiKeyStore as _;

    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let task_queue = format!("lightspeed-agent-live-{}", uuid::Uuid::new_v4().simple());
    let temporal_target =
        env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace =
        env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let stores = DeploymentStores::from_env().await?;
    let universes = Arc::new(UniverseRuntime::new(
        client.clone(),
        task_queue.clone(),
        None,
        stores.clone(),
    )?);

    // Two universes, one key each. Keys are minted below the API on purpose:
    // provisioning is out-of-band (server api-key create).
    let universe_a = uuid::Uuid::new_v4();
    let universe_b = uuid::Uuid::new_v4();
    universes.state_for(universe_a, true).await?;
    universes.state_for(universe_b, true).await?;
    let api_keys = store_pg::PgApiKeyStore::new(stores.pool().clone());
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let minted_a = auth::mint_api_key(
        universe_a,
        auth::PrincipalRef {
            kind: auth::PrincipalKind::ServiceAccount,
            id: Some("live-test".to_owned()),
        },
        Some("api-key live A".to_owned()),
        now_ms,
    );
    let minted_b = auth::mint_api_key(
        universe_b,
        auth::PrincipalRef::universe_default(),
        Some("api-key live B".to_owned()),
        now_ms,
    );
    for minted in [&minted_a, &minted_b] {
        api_keys
            .create_api_key(auth::CreateApiKey {
                key_hash: minted.key_hash.clone(),
                record: minted.record.clone(),
            })
            .await?;
    }

    let activities = WorkerActivities::with_runtime(universes.clone());
    let mut worker =
        worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = worker.run();
    tokio::pin!(worker_future);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let gateway_url = format!("http://{}/rpc", listener.local_addr()?);
    let gateway_state = Arc::new(temporal_server::gateway::GatewayState::multi(
        temporal_server::GatewayAuthMode::ApiKey,
        universes.clone(),
        format!("http://{}", listener.local_addr()?),
    ));
    let gateway = tokio::spawn({
        let gateway_state = gateway_state.clone();
        async move {
            let app = temporal_server::gateway::gateway_router(
                gateway_state,
                temporal_server::gateway::DEFAULT_MAX_REQUEST_BODY_BYTES,
                temporal_server::gateway::GatewayRoutes::ALL,
            );
            axum::serve(listener, app).await
        }
    });

    let http = reqwest::Client::new();
    let session_id = format!("session_key_{}", uuid::Uuid::new_v4().simple());
    let rpc = |method: &str, params: serde_json::Value| serde_json::json!({ "id": 1, "method": method, "params": params });
    let call = |bearer: Option<String>, body: serde_json::Value| {
        let http = http.clone();
        let gateway_url = gateway_url.clone();
        async move {
            let mut request = http.post(&gateway_url).json(&body);
            if let Some(bearer) = bearer {
                request = request.header("authorization", format!("Bearer {bearer}"));
            }
            let response: serde_json::Value = request.send().await?.json().await?;
            anyhow::Ok(response)
        }
    };
    let secret_a = minted_a.secret.expose().to_owned();
    let secret_b = minted_b.secret.expose().to_owned();

    let client_future = async {
        // Fail closed: no credential.
        let response = call(
            None,
            rpc(
                "session/read",
                serde_json::json!({ "sessionId": session_id }),
            ),
        )
        .await?;
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("error message")
                .contains("Authorization")
        );

        // Fail closed: unknown key.
        let response = call(
            Some("lsk_bogus".to_owned()),
            rpc(
                "session/read",
                serde_json::json!({ "sessionId": session_id }),
            ),
        )
        .await?;
        assert_eq!(
            response["error"]["message"]
                .as_str()
                .expect("error message"),
            "invalid api key"
        );

        // API-key-authenticated tenants can never mint more keys. Operator
        // dispatch is rejected by auth mode before the bearer can select a
        // universe.
        let response = call(
            Some(secret_a.clone()),
            rpc(
                "operator/api-keys/create",
                serde_json::json!({
                    "universeId": universe_a,
                    "displayName": "must not mint",
                    "principal": { "kind": "serviceAccount", "id": "blocked" }
                }),
            ),
        )
        .await?;
        assert_eq!(
            response["error"]["message"]
                .as_str()
                .expect("operator rejection message"),
            "operator methods are not available to api-key callers"
        );

        // Tenant headers are rejected in api-key mode.
        let response: serde_json::Value = http
            .post(&gateway_url)
            .header("authorization", format!("Bearer {secret_a}"))
            .header("x-lightspeed-universe", universe_b.to_string())
            .json(&rpc(
                "session/read",
                serde_json::json!({ "sessionId": session_id }),
            ))
            .send()
            .await?
            .json()
            .await?;
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("error message")
                .contains("x-lightspeed-universe")
        );

        // Key A starts a session in universe A.
        let response = call(
            Some(secret_a.clone()),
            rpc(
                "session/start",
                serde_json::json!({ "sessionId": session_id }),
            ),
        )
        .await?;
        assert!(
            response["error"].is_null(),
            "session/start via key A failed: {response}"
        );

        // Key A reads it; key B misses it (not-found, no existence leak).
        let response = call(
            Some(secret_a.clone()),
            rpc(
                "session/read",
                serde_json::json!({ "sessionId": session_id }),
            ),
        )
        .await?;
        assert!(
            response["error"].is_null(),
            "session/read via key A failed: {response}"
        );
        let response = call(
            Some(secret_b.clone()),
            rpc(
                "session/read",
                serde_json::json!({ "sessionId": session_id }),
            ),
        )
        .await?;
        assert_eq!(
            response["error"]["code"].as_i64().expect("error code"),
            -32004,
            "key B must not read universe A's session: {response}"
        );

        // The principal of the resolving key is stamped onto grants created
        // through it.
        let response = call(
            Some(secret_a.clone()),
            rpc(
                "auth/grants/import",
                serde_json::json!({ "token": "live-static-token", "displayName": "api-key live" }),
            ),
        )
        .await?;
        assert!(
            response["error"].is_null(),
            "auth/grants/import via key A failed: {response}"
        );
        let state_a = universes.state_for(universe_a, false).await?;
        let grants = auth::AuthGrantStore::list_grants(
            state_a.store.as_ref(),
            auth::ListAuthGrants { status: None },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        let grant = grants.iter().find(|grant| {
            grant.principal.id.as_deref() == Some("live-test")
                && grant.principal.kind == auth::PrincipalKind::ServiceAccount
        });
        assert!(
            grant.is_some(),
            "expected a grant stamped with key A's principal, got: {:?}",
            grants
                .iter()
                .map(|grant| grant.principal.clone())
                .collect::<Vec<_>>()
        );

        // Revocation takes effect immediately.
        assert!(
            api_keys
                .revoke_api_key(&minted_a.record.key_prefix, now_ms + 1)
                .await?
        );
        let response = call(
            Some(secret_a.clone()),
            rpc(
                "session/read",
                serde_json::json!({ "sessionId": session_id }),
            ),
        )
        .await?;
        assert_eq!(
            response["error"]["message"]
                .as_str()
                .expect("error message"),
            "invalid api key"
        );

        // Cleanup.
        let workflow_id = temporal_workflow::compose_workflow_id(
            universe_a,
            &SessionId::new(session_id.as_str()),
        );
        let handle = client.get_workflow_handle::<AgentSessionWorkflow>(workflow_id);
        let _ = handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("api-key live test cleanup")
                    .build(),
            )
            .await;
        anyhow::Ok(())
    };
    tokio::pin!(client_future);

    let client_result = tokio::select! {
        worker_result = worker_future.as_mut() => {
            return match worker_result {
                Ok(()) => Err(anyhow::anyhow!("Temporal worker stopped before the live test completed")),
                Err(error) => Err(error.context("Temporal worker failed")),
            };
        }
        client_result = client_future.as_mut() => client_result,
    };

    shutdown_worker();
    gateway.abort();
    tokio::time::timeout(Duration::from_secs(10), worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 10 seconds"))??;
    client_result
}
