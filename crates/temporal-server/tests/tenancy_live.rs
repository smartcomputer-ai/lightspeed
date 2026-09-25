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
        // Direct service calls carry an explicit caller, one per universe.
        let caller_a = temporal_server::gateway::request_context::RequestContext::local(
            api::AccessScope::Universe {
                universe_id: universe_a,
            },
        );
        let caller_b = temporal_server::gateway::request_context::RequestContext::local(
            api::AccessScope::Universe {
                universe_id: universe_b,
            },
        );
        use temporal_server::gateway::request_context::with_request_context as calling;

        // The same client-chosen session id starts independently in both
        // universes on the same queue, served by the same worker.
        for (api, caller) in [(api_a.as_ref(), &caller_a), (api_b.as_ref(), &caller_b)] {
            let started = calling(
                caller.clone(),
                api.start_session(SessionStartParams {
                    access: None,
                    metadata: Default::default(),
                    session_id: Some(session_id.as_str().to_owned()),
                    display_name: None,
                    config: None,
                    profile: None,
                    delete_after_close_ms: None,
                }),
            )
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
        calling(
            caller_a.clone(),
            api_a.create_profile(ProfileCreateParams {
                profile: AgentProfileInput {
                    profile_id: profile_id.clone(),
                    display_name: Some("Tenant isolation".to_owned()),
                    description: None,
                    document: ProfileDocument::default(),
                },
            }),
        )
        .await?;
        let listed_b = calling(caller_b.clone(), api_b.list_profiles(ProfileListParams {})).await?;
        assert!(
            listed_b
                .result
                .profiles
                .iter()
                .all(|profile| profile.profile_id != profile_id),
            "universe B must not list universe A's profile"
        );
        let read_b = calling(
            caller_b.clone(),
            api_b.read_profile(ProfileReadParams {
                profile_id: profile_id.clone(),
            }),
        )
        .await;
        match read_b {
            Err(error) => assert_eq!(error.kind, AgentApiErrorKind::NotFound),
            Ok(_) => anyhow::bail!("universe B must not read universe A's profile"),
        }

        // Closing A's session leaves B's session open.
        calling(
            caller_a.clone(),
            api_a.close_session(api::SessionCloseParams {
                force: false,
                session_id: session_id.as_str().to_owned(),
            }),
        )
        .await?;
        let closed_a = calling(
            caller_a.clone(),
            api_a.read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            }),
        )
        .await?;
        assert_eq!(closed_a.result.session.status, SessionStatus::Closed);
        let open_b = calling(
            caller_b.clone(),
            api_b.read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            }),
        )
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
