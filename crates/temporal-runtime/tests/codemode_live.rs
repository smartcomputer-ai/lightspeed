//! Native JavaScript, CAS artifacts, and the production session Update bridge.
//! A small receiver supplies workflow-tool replies without provider credentials.

#[path = "support/code_tools.rs"]
mod code_tool_fixture;
mod support;

use std::time::{Duration, Instant};

use code_tool_fixture::{AGENT, Fixture, JOB, SUBMIT, scenario};
use codemode::{Cancellation, ExecutionErrorKind};
use harness::{PromiseResolution, storage::BlobStore};
use serde_json::json;
use support::live::{LIVE_TEST_LOCK, live_universe_id, require_storage_live_env};
use temporal_runtime::code::{CodeRunError, CodeRunReport, CodeRunner, CodeToolCatalog};
use temporal_workflow::{
    CodeExecutionDescriptor, CodeExecutionLimits, CodeToolCallStatus, compose_workflow_id,
};

fn limits() -> CodeExecutionLimits {
    CodeExecutionLimits {
        timeout_ms: 30_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_stack_bytes: 512 * 1024,
        max_source_bytes: 32 * 1024,
        max_catalog_bytes: 64 * 1024,
        max_request_bytes: 8 * 1024,
        max_result_bytes: 8 * 1024,
        max_output_bytes: 32 * 1024,
        max_tool_calls: 32,
        max_outstanding_tool_calls: 8,
    }
}

async fn descriptor(
    fixture: &Fixture,
    source: &str,
    limits: CodeExecutionLimits,
) -> anyhow::Result<CodeExecutionDescriptor> {
    Ok(CodeExecutionDescriptor {
        execution_id: fixture.execution_id.clone(),
        session_workflow_id: compose_workflow_id(live_universe_id()?, &fixture.session_id),
        source_ref: fixture.store.put_bytes(source.as_bytes().to_vec()).await?,
        catalog_ref: fixture
            .store
            .put_bytes(serde_json::to_vec(&CodeToolCatalog::from_scope(
                &fixture.scope,
            ))?)
            .await?,
        limits,
    })
}

fn runner(fixture: &Fixture) -> anyhow::Result<CodeRunner> {
    Ok(CodeRunner::new(
        fixture.client.clone(),
        fixture.store.clone(),
        1,
        Duration::from_secs(10),
    )?)
}

async fn execute(
    runner: &CodeRunner,
    descriptor: CodeExecutionDescriptor,
    cancellation: Cancellation,
) -> anyhow::Result<CodeRunReport> {
    let started = Instant::now();
    let report = runner.run_once(descriptor, cancellation).await?;
    // Diagnostics from this local run, not a latency budget or benchmark.
    eprintln!(
        "code live timing: startup={}us, engine={}us, bridge_run={}us, calls={}",
        report.execution.metrics.startup_micros,
        report.execution.metrics.elapsed_micros,
        started.elapsed().as_micros(),
        report.execution.metrics.tool_calls,
    );
    assert_eq!(report.cleanup_error, None, "scope cleanup must finish");
    let scope = report.scope.as_ref().expect("authoritative scope report");
    assert!(scope.closed);
    assert!(scope.calls.values().all(|call| call.status.is_terminal()));
    Ok(report)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn javascript_loops_parallel_calls_and_durable_waits_use_session_tools() -> anyhow::Result<()>
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let source = r#"
            const loop = [];
            const ordinaryMs = [];
            for (const ms of [5, 10]) {
                const started = Date.now();
                loop.push(await tools.sleep({ms}));
                ordinaryMs.push(Date.now() - started);
            }
            const parallel = await Promise.all([5, 10].map(ms => tools.sleep({ms})));
            const timers = await tools.await({
                promises: [...loop, ...parallel].map(value => value.promise),
                mode: "all", timeout_ms: 5000,
            });
            const [job, agent] = await Promise.all([
                tools.test_joined_job({}), tools.test_joined_agent({}),
            ]);
            const submitted = await tools.test_job_submit({});
            const waited = await tools.await({
                promises: [submitted.promise], mode: "all", timeout_ms: 5000,
            });
            let caught = null;
            try {
                await tools.await({promises: ["promise_999999999"], mode: "all", timeout_ms: 1000});
            } catch (error) {
                caught = error.kind;
            }
            text({ordinaryMs});
            text({job, agent});
            return {
                timers: timers.results.map(result => result.status),
                submitted: waited.results[0].status,
                caught,
            };
        "#;
        let runner = runner(&fixture)?;
        let input = descriptor(&fixture, source, limits()).await?;
        let replies = async {
            // Neither reply is sent until both joined calls are admitted.
            // Serial execution of the guest or Update handlers would deadlock.
            let (job, agent) =
                tokio::try_join!(fixture.invocation(JOB), fixture.invocation(AGENT))?;
            assert_ne!(job.tool_call_id, agent.tool_call_id);
            fixture.resolve_value(JOB, json!({"job": "done"})).await?;
            fixture
                .resolve_value(AGENT, json!({"agent": "done"}))
                .await?;
            fixture.invocation(SUBMIT).await?;
            support::live::wait_until(
                "JavaScript awaits the submitted durable promise",
                Duration::from_secs(10),
                async || {
                    let scope = fixture.report().await?;
                    Ok(scope.calls.len() == 9
                        && scope
                            .calls
                            .values()
                            .any(|call| call.status == CodeToolCallStatus::Waiting))
                },
            )
            .await?;
            fixture
                .resolve_value(SUBMIT, json!({"submitted": "done"}))
                .await
        };
        let (report, ()) =
            tokio::try_join!(execute(&runner, input, Cancellation::default()), replies,)?;
        assert_eq!(report.execution.error, None);
        assert_eq!(
            report.execution.return_value,
            Some(json!({
                "timers": ["resolved", "resolved", "resolved", "resolved"],
                "submitted": "resolved", "caught": "tool_failed",
            }))
        );
        assert_eq!(report.execution.output.len(), 2);
        assert_eq!(
            report.execution.output[1],
            json!({
                "job": {"job": "done"}, "agent": {"agent": "done"},
            })
        );
        eprintln!(
            "ordinary tool Update round trips (ms): {}",
            report.execution.output[0]["ordinaryMs"]
        );
        assert!(report.execution.pending_request_ids.is_empty());
        let scope = report.scope.unwrap();
        assert_eq!(scope.calls.len(), 10);
        assert_eq!(
            scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Failed)
                .count(),
            1
        );
        assert_eq!(
            scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Succeeded)
                .count(),
            9
        );
        fixture.finish().await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn javascript_failure_keeps_successes_and_cancels_unfinished_siblings() -> anyhow::Result<()>
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let runner = runner(&fixture)?;
        let input = descriptor(
            &fixture,
            r#"
            const completed = await tools.sleep({ms: 0});
            text({completed: typeof completed.promise === "string"});
            return await Promise.all([
                tools.test_joined_job({}), tools.test_joined_agent({}),
            ]);
        "#,
            limits(),
        )
        .await?;
        let fail_one = async {
            let (job, _agent) =
                tokio::try_join!(fixture.invocation(JOB), fixture.invocation(AGENT))?;
            fixture
                .resolve(
                    &job,
                    PromiseResolution::Failed {
                        error_ref: Some(
                            fixture.store.put_bytes(b"test job failed".to_vec()).await?,
                        ),
                    },
                )
                .await
        };
        let (report, ()) =
            tokio::try_join!(execute(&runner, input, Cancellation::default()), fail_one,)?;
        assert_eq!(
            report.execution.error.unwrap().kind,
            ExecutionErrorKind::Javascript
        );
        assert_eq!(report.execution.output, vec![json!({"completed": true})]);
        assert_eq!(report.execution.return_value, None);
        assert_eq!(report.execution.pending_request_ids.len(), 1);
        let scope = report.scope.unwrap();
        assert_eq!(scope.calls.len(), 3);
        for status in [
            CodeToolCallStatus::Succeeded,
            CodeToolCallStatus::Failed,
            CodeToolCallStatus::Cancelled,
        ] {
            assert_eq!(
                scope
                    .calls
                    .values()
                    .filter(|call| call.status == status)
                    .count(),
                1
            );
        }
        fixture.finish().await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn javascript_cancellation_reconciles_pending_durable_calls() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let runner = runner(&fixture)?;
        let input = descriptor(
            &fixture,
            r#"
            await tools.sleep({ms: 0});
            text("before cancellation");
            return await tools.test_joined_job({});
        "#,
            limits(),
        )
        .await?;
        let cancellation = Cancellation::default();
        let cancel = async {
            fixture.invocation(JOB).await?;
            cancellation.cancel();
            Ok::<_, anyhow::Error>(())
        };
        let (report, ()) =
            tokio::try_join!(execute(&runner, input, cancellation.clone()), cancel,)?;
        assert_eq!(
            report.execution.error.unwrap().kind,
            ExecutionErrorKind::Cancelled
        );
        assert_eq!(report.execution.output, vec![json!("before cancellation")]);
        assert_eq!(report.execution.pending_request_ids.len(), 1);
        let scope = report.scope.unwrap();
        assert_eq!(scope.calls.len(), 2);
        assert_eq!(
            scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Succeeded)
                .count(),
            1
        );
        assert_eq!(
            scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Cancelled)
                .count(),
            1
        );
        fixture.finish().await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn javascript_result_limit_preserves_successful_authoritative_outcome() -> anyhow::Result<()>
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let runner = runner(&fixture)?;
        let mut limits = limits();
        limits.max_result_bytes = 1024;
        let input = descriptor(
            &fixture,
            r#"
            try {
                await tools.test_joined_job({});
                return {caught: null};
            } catch (error) {
                return {caught: error.kind};
            }
        "#,
            limits,
        )
        .await?;
        let payload = json!({"large": "x".repeat(8192)});
        let (report, ()) = tokio::try_join!(
            execute(&runner, input, Cancellation::default()),
            fixture.resolve_value(JOB, payload.clone()),
        )?;
        assert_eq!(report.execution.error, None);
        assert_eq!(
            report.execution.return_value,
            Some(json!({"caught": "payload_too_large"}))
        );
        let scope = report.scope.unwrap();
        assert_eq!(scope.calls.len(), 1);
        let call = scope.calls.values().next().unwrap();
        assert_eq!(call.status, CodeToolCallStatus::Succeeded);
        assert_eq!(fixture.value(call).await?, payload);
        fixture.finish().await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual local interpreter timing diagnostic"]
async fn fresh_quickjs_runtime_timing() -> anyhow::Result<()> {
    let mut startup = Vec::new();
    let mut roundtrip = Vec::new();
    for _ in 0..32 {
        let limits = limits();
        let started = Instant::now();
        let mut execution = codemode::start(
            codemode::ExecutionInput {
                source: "return 1 + 1;".to_owned(),
                bindings: Vec::new(),
                limits: codemode::ExecutionLimits {
                    timeout_ms: limits.timeout_ms,
                    max_memory_bytes: limits.max_memory_bytes,
                    max_stack_bytes: limits.max_stack_bytes,
                    max_source_bytes: limits.max_source_bytes,
                    max_catalog_bytes: limits.max_catalog_bytes,
                    max_request_bytes: limits.max_request_bytes,
                    max_result_bytes: limits.max_result_bytes,
                    max_output_bytes: limits.max_output_bytes,
                    max_tool_calls: limits.max_tool_calls,
                    max_outstanding_tool_calls: limits.max_outstanding_tool_calls,
                },
            },
            Cancellation::default(),
        )?;
        let Some(codemode::ExecutionEvent::Finished(report)) = execution.next_event().await else {
            anyhow::bail!("pure JavaScript did not produce its terminal report");
        };
        roundtrip.push(started.elapsed().as_micros());
        assert_eq!(report.error, None);
        assert_eq!(report.return_value, Some(json!(2)));
        startup.push(report.metrics.startup_micros);
    }
    startup.sort_unstable();
    roundtrip.sort_unstable();
    eprintln!(
        "32 fresh local test runtimes: initialize+prelude+compile p50={}us p95={}us; thread+execute+report p50={}us p95={}us",
        startup[16], startup[30], roundtrip[16], roundtrip[30],
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn invalid_preparation_closes_the_unused_scope() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    for invalid_source in [true, false] {
        scenario(|fixture| async move {
            let runner = runner(&fixture)?;
            let mut input =
                descriptor(&fixture, "return await tools.sleep({ms: 0});", limits()).await?;
            if invalid_source {
                input.source_ref = fixture.store.put_bytes(vec![0xff]).await?;
            } else {
                input.catalog_ref = fixture
                    .store
                    .put_bytes(serde_json::to_vec(&json!({
                        "version": 1,
                        "bindings": [{"name": "sleep", "binding_id": "not-admitted"}],
                    }))?)
                    .await?;
            }
            let error = runner
                .run_once(input, Cancellation::default())
                .await
                .unwrap_err();
            if invalid_source {
                assert!(matches!(error, CodeRunError::Preparation(_)), "{error:?}");
            } else {
                assert!(matches!(error, CodeRunError::Catalog(_)), "{error:?}");
            }
            let scope = fixture.report().await?;
            assert!(scope.closed);
            assert!(
                scope.calls.is_empty(),
                "invalid input must not execute source"
            );
            fixture.finish().await
        })
        .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn dropped_runner_caller_still_closes_and_reconciles_the_scope() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let runner = runner(&fixture)?;
        let input = descriptor(
            &fixture,
            r#"
            await tools.sleep({ms: 0});
            return await tools.test_joined_job({});
        "#,
            limits(),
        )
        .await?;
        let running =
            tokio::spawn(async move { runner.run_once(input, Cancellation::default()).await });
        fixture.invocation(JOB).await?;
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        support::live::wait_until(
            "dropped runner caller leaves its supervised scope cleanup active",
            Duration::from_secs(15),
            async || {
                let scope = fixture.report().await?;
                Ok(scope.closed && scope.calls.values().all(|call| call.status.is_terminal()))
            },
        )
        .await?;
        let scope = fixture.report().await?;
        assert_eq!(scope.calls.len(), 2);
        assert_eq!(
            scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Succeeded)
                .count(),
            1
        );
        assert_eq!(
            scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Cancelled)
                .count(),
            1
        );
        fixture.finish().await
    })
    .await
}
