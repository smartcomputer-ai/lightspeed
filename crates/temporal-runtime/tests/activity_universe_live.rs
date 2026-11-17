//! Actual Temporal activity contexts must constrain the universe selected by
//! payloads before a runtime adapter can read or write another universe.
mod support;

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    worker::{WorkerActivities, worker_runtime},
};
use temporal_workflow::{
    EnvironmentJobCancelActivityRequest, SubagentLivenessActivityRequest, TemporalConnectionConfig,
    WorkflowActivities, connect_temporal,
};
use temporalio_client::WorkflowStartOptions;
use temporalio_common::protos::temporal::api::failure::v1::failure::FailureInfo;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityExecutionError, LocalActivityOptions, Worker, WorkerOptions, WorkflowContext,
    WorkflowResult,
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeArgs {
    universe: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refusal {
    kind: String,
    non_retryable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeResult {
    environment: Option<Refusal>,
    subagent: Option<Refusal>,
}

fn refusal(error: ActivityExecutionError) -> Refusal {
    let mut current = error.failure();
    while let Some(failure) = current {
        if let Some(FailureInfo::ApplicationFailureInfo(info)) = &failure.failure_info {
            return Refusal {
                kind: info.r#type.clone(),
                non_retryable: info.non_retryable,
            };
        }
        current = failure.cause.as_deref();
    }
    Refusal {
        kind: "unexpected non-application activity failure".into(),
        non_retryable: false,
    }
}

fn options() -> LocalActivityOptions {
    LocalActivityOptions::builder()
        .schedule_to_close_timeout(Duration::from_secs(10))
        .start_to_close_timeout(Duration::from_secs(5))
        .build()
}

// Deliberately omit production workflow admission checks: exercise the worker
// boundary independently with Temporal's actual originating workflow identity.
#[workflow]
#[derive(Default)]
pub struct ActivityUniverseProbe;

#[workflow_methods]
impl ActivityUniverseProbe {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: ProbeArgs,
    ) -> WorkflowResult<ProbeResult> {
        let environment = ctx
            .execute_local_activity(
                WorkflowActivities::environment_job_cancel,
                EnvironmentJobCancelActivityRequest {
                    universe_id: args.universe,
                    environment_id: "unused-environment".into(),
                    jobs: Vec::new(),
                    scope: Default::default(),
                    force: false,
                },
                options(),
            )
            .await
            .err()
            .map(refusal);
        let subagent = ctx
            .execute_local_activity(
                WorkflowActivities::subagent_liveness,
                SubagentLivenessActivityRequest {
                    universe_id: args.universe,
                    session_id: "never-started-child".into(),
                },
                options(),
            )
            .await
            .err()
            .map(refusal);
        Ok(ProbeResult {
            environment,
            subagent,
        })
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, migrated Postgres, object storage, and Temporal"]
async fn activity_payloads_cannot_change_the_originating_universe() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    support::live::require_storage_live_env()?;
    let runtime = worker_runtime()?;
    let client = connect_temporal(&TemporalConnectionConfig::from_env()?).await?;
    let queue = format!("activity-universe-{}", Uuid::new_v4().simple());
    let universes = Arc::new(UniverseRuntime::new(
        client.clone(),
        queue.clone(),
        None,
        DeploymentStores::from_env().await?,
    )?);
    let origin = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    for universe in [origin, foreign] {
        store_pg::create_universe(universes.stores().pool(), universe).await?;
    }
    let mut worker = Worker::new(
        &runtime,
        client.clone(),
        WorkerOptions::new(queue.clone())
            .register_workflow::<ActivityUniverseProbe>()?
            .register_activities(WorkerActivities::with_runtime(universes.clone()))
            .build(),
    )?;
    let shutdown = worker.shutdown_handle();
    let worker_future = async { worker.run().await.map_err(anyhow::Error::from) };
    tokio::pin!(worker_future);

    let client_future = async {
        let long_id = temporal_workflow::compose_environment_job_workflow_id(
            origin,
            &"e".repeat(128),
            "job-012345678901234567890123",
        );
        let cases = [
            (
                format!("{origin}/foreign-payload"),
                foreign,
                Some("ActivityUniverseMismatch"),
            ),
            // Refuse before lookup: even a nonexistent foreign universe must
            // yield the origin failure, not a store's unknown-universe result.
            (
                format!("{origin}/unknown-foreign-payload"),
                Uuid::new_v4(),
                Some("ActivityUniverseMismatch"),
            ),
            (
                format!("no-universe-{}", Uuid::new_v4()),
                foreign,
                Some("ActivityWorkflowIdentityInvalid"),
            ),
            (
                format!("{origin}/extra/separator"),
                foreign,
                Some("ActivityWorkflowIdentityInvalid"),
            ),
            (long_id, origin, None),
        ];
        for (workflow_id, requested, expected) in cases {
            let handle = client
                .start_workflow(
                    ActivityUniverseProbe::run,
                    ProbeArgs {
                        universe: requested,
                    },
                    WorkflowStartOptions::new(queue.clone(), workflow_id)
                        .execution_timeout(Duration::from_secs(45))
                        .build(),
                )
                .await?;
            let result = handle.get_result(Default::default()).await?;
            for (adapter, failure) in [
                ("environment job", result.environment),
                ("subagent", result.subagent),
            ] {
                match expected {
                    Some(kind) => {
                        let failure = failure.expect("foreign/malformed origin must fail");
                        assert_eq!(failure.kind, kind, "{adapter}");
                        assert!(failure.non_retryable, "{adapter}");
                    }
                    None => assert!(failure.is_none(), "{adapter}: {failure:?}"),
                }
            }
        }
        anyhow::Ok(())
    };
    tokio::pin!(client_future);
    let result = tokio::select! {
        result = worker_future.as_mut() => Err(anyhow::anyhow!("worker stopped before probe completed: {result:?}")),
        result = support::live::bounded_live_test("activity universe origin", Duration::from_secs(120), client_future.as_mut()) => result,
    };
    shutdown();
    let stopped = support::live::bounded_live_test(
        "activity origin worker shutdown",
        Duration::from_secs(10),
        worker_future.as_mut(),
    )
    .await;
    result.and(stopped)
}
