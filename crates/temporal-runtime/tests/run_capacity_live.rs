//! Live coverage for reported input capacity and model switching on the production
//! wiring: a universe's activities reach the same discovery cache as its gateway only
//! through `UniverseRuntime`, so other suites' workers resolve no capacity at all.

mod support;

use std::{env, sync::Arc, time::Duration};

use api::{
    AccessScope, AgentApiService, InputItem, ModelConfig, RunStartConfig, RunStartParams,
    RunStartSource, RunStatus, SessionConfig, SessionStartParams,
};
use harness::SessionId;
use support::live::{
    LIVE_TEST_LOCK, read_session_view, require_storage_live_env, start_text_run,
    wait_for_terminal_run_with_timeout,
};
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    gateway::request_context::{RequestContext, with_request_context},
    worker::{WorkerActivities, sessions_worker, worker_runtime},
};
use temporal_workflow::{
    AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::WorkflowTerminateOptions;

const RUN_BUDGET: Duration = Duration::from_secs(120);

/// A session on the Anthropic route learns its capacity from the provider's model list
/// when a run is prepared, with nothing declared. A run can then name another model on
/// the route, and the next one go back to the session's own.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, Postgres, Temporal, and ANTHROPIC_API_KEY (costs real money)"]
async fn temporal_live_anthropic_run_learns_capacity_and_switches_models() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    anyhow::ensure!(
        env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.trim().is_empty()),
        "ANTHROPIC_API_KEY must be set"
    );

    let task_queue = format!("lightspeed-agent-live-{}", uuid::Uuid::new_v4().simple());
    let temporal_target =
        env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace =
        env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let runtime = worker_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let universes = Arc::new(UniverseRuntime::new(
        client.clone(),
        task_queue.clone(),
        None,
        DeploymentStores::from_env().await?,
    )?);
    let activities = WorkerActivities::with_runtime(universes.clone());
    let mut worker = sessions_worker(&runtime, client.clone(), task_queue, activities)?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = async { worker.run().await.map_err(anyhow::Error::from) };
    tokio::pin!(worker_future);

    let universe_id = uuid::Uuid::new_v4();
    let session_id = SessionId::new(format!("run_capacity_{}", uuid::Uuid::new_v4().simple()));
    let session_model = |model: &str| ModelConfig {
        provider_id: "anthropic".to_owned(),
        api_kind: "anthropic:messages".to_owned(),
        model: model.to_owned(),
    };

    let client_future = with_request_context(
        RequestContext::local(AccessScope::Universe { universe_id }),
        async {
            let api = universes.state_for(universe_id, true).await?.api.clone();
            api.start_session(SessionStartParams {
                access: None,
                metadata: Default::default(),
                session_id: Some(session_id.as_str().to_owned()),
                display_name: None,
                config: Some(SessionConfig {
                    model: Some(session_model(
                        &env::var("ANTHROPIC_LIVE_MODEL")
                            .unwrap_or_else(|_| "claude-opus-5".to_owned()),
                    )),
                    ..SessionConfig::default()
                }),
                profile: None,
                delete_after_close_ms: None,
                integration: None,
            })
            .await?;

            let first = start_text_run(&api, &session_id, "Reply with the word one.").await?;
            let first =
                wait_for_terminal_run_with_timeout(&api, &session_id, &first.id, RUN_BUDGET)
                    .await?;
            assert_eq!(first.status, RunStatus::Completed);
            let reported = read_session_view(&api, &session_id)
                .await?
                .active_context
                .compaction
                .expect("an open session reports compaction")
                .input_limit_tokens;
            assert!(
                reported.is_some_and(|limit| limit > 0),
                "the run should carry what the provider reported, got {reported:?}"
            );

            let other = api
                .start_run(RunStartParams {
                    notify_on_terminal: None,
                    submission_id: None,
                    session_id: session_id.as_str().to_owned(),
                    source: RunStartSource::Input {
                        items: vec![InputItem::Text {
                            provenance_ref: None,
                            origin: None,
                            text: "Reply with the word two.".to_owned(),
                        }],
                    },
                    config: Some(RunStartConfig {
                        model: Some(session_model("claude-sonnet-5-5")),
                        ..RunStartConfig::default()
                    }),
                })
                .await?
                .result
                .run;
            let other =
                wait_for_terminal_run_with_timeout(&api, &session_id, &other.id, RUN_BUDGET)
                    .await?;
            assert_eq!(other.status, RunStatus::Completed);

            let back = start_text_run(&api, &session_id, "Reply with the word three.").await?;
            let back =
                wait_for_terminal_run_with_timeout(&api, &session_id, &back.id, RUN_BUDGET).await?;
            assert_eq!(back.status, RunStatus::Completed);
            anyhow::Ok(())
        },
    );
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

    let workflow_id = temporal_workflow::compose_workflow_id(universe_id, &session_id);
    let _ = client
        .get_workflow_handle::<AgentSessionWorkflow>(workflow_id)
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("run capacity live test cleanup")
                .build(),
        )
        .await;
    shutdown_worker();
    tokio::time::timeout(Duration::from_secs(10), worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 10 seconds"))??;
    client_result
}
