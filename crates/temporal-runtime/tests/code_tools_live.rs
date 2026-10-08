//! Real Temporal Update transport with a simulated code runner. No interpreter,
//! provider credentials, or execution environment is needed for this protocol
//! proof; tools use production session activities and deterministic reductions.

#[path = "support/code_tools.rs"]
mod code_tool_fixture;
mod support;

use api::AgentApiService;
use code_tool_fixture::{AGENT, JOB, SUBMIT, scenario};
use harness::{BlobRef, PromiseResolution, storage::BlobStore};
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};
use support::live::{LIVE_TEST_LOCK, live_universe_id, require_storage_live_env};
use temporal_workflow::{
    AgentSessionWorkflow, CloseCodeToolScopeRequest, CodeToolCallStatus, CodeToolRejectionKind,
    InvokeCodeToolRequest, compose_workflow_id,
};
use temporalio_client::WorkflowStartUpdateOptions;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn code_tool_updates_execute_parallel_dependent_joined_and_submit_wait_calls()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let first_request = fixture.request("sleep-a", "sleep", json!({"ms": 300})).await?;
        let (first, second) = tokio::try_join!(
            fixture.bridge.invoke(first_request.clone(), Default::default()),
            fixture.bridge.invoke(fixture.request("sleep-b", "sleep", json!({"ms": 500})).await?, Default::default()),
        )?;
        let (first, second) = (first?, second?);
        assert_eq!(first.status, CodeToolCallStatus::Succeeded);
        assert_eq!(first, fixture.bridge.invoke(first_request.clone(), Default::default()).await??);
        let conflict = InvokeCodeToolRequest { arguments_ref: fixture.store.put_bytes(b"{\"ms\":1}".to_vec()).await?, ..first_request };
        assert_eq!(fixture.bridge.invoke(conflict, Default::default()).await?.unwrap_err().kind, CodeToolRejectionKind::Conflict);
        let first_value = fixture.value(&first).await?;
        let second_value = fixture.value(&second).await?;
        let waited = fixture.invoke("await-timers", "await", json!({"promises":[first_value["promise"],second_value["promise"]],"mode":"all","timeout_ms":5000})).await?;
        assert_eq!(fixture.value(&waited).await?["outcome"], "terminal");

        let job_request = fixture.request("job", JOB, json!({})).await?;
        let joined_calls = async {
            let (job, duplicate, agent) = tokio::try_join!(fixture.redeliver(job_request.clone()), fixture.redeliver(job_request.clone()), fixture.invoke("agent", AGENT, json!({})))?;
            assert_eq!(job, duplicate, "duplicate in-flight requests share the canonical result");
            assert_eq!(fixture.value(&job).await?, json!({"job":"done"}));
            assert_eq!(fixture.value(&agent).await?, json!({"agent":"done"}));
            Ok::<_, anyhow::Error>(())
        };
        let resolve_joined = async {
            // Both invocations must be admitted before either reply. This
            // fails if the session serially blocks admission on the first wait.
            let (job, agent) = tokio::try_join!(fixture.invocation(JOB), fixture.invocation(AGENT))?;
            assert_ne!(job.tool_call_id, agent.tool_call_id);
            fixture.resolve_value(JOB, json!({"job":"done"})).await?;
            fixture.resolve_value(AGENT, json!({"agent":"done"})).await
        };
        tokio::try_join!(joined_calls, resolve_joined)?;
        let submitted = fixture.invoke("submit", SUBMIT, json!({})).await?;
        assert_eq!(submitted.status, CodeToolCallStatus::Succeeded);
        let child = fixture.invocation(SUBMIT).await?;
        let promise = child.completion_promises.as_ref().unwrap()[harness::REPLY_COMPLETION_KEY].as_str();
        let wait = fixture.invoke("await-submitted", "await", json!({"promises":[promise],"mode":"all","timeout_ms":5000}));
        let reply = async {
            support::live::wait_until("code tool submitted wait", Duration::from_secs(10), async || {
                Ok(fixture.report().await?.calls.get("await-submitted").is_some_and(|call| call.status == CodeToolCallStatus::Waiting))
            }).await?;
            fixture.resolve_value(SUBMIT, json!({"submitted":"done"})).await
        };
        let (waited, ()) = tokio::try_join!(wait, reply)?;
        assert_eq!(fixture.value(&waited).await?["results"][0]["status"], "resolved");
        let malformed = fixture.invoke("invalid-await", "await", json!({"promises":["promise_999999999"],"mode":"all","timeout_ms":1000})).await?;
        assert_eq!(malformed.status, CodeToolCallStatus::Failed, "a bad promise wait fails only its code tool call");
        let denied = InvokeCodeToolRequest { execution_id: fixture.execution_id.clone(), request_id: "guessed".to_owned(),
            binding_id: "guessed-admin-tool".to_owned(), arguments_ref: BlobRef::from_bytes(b"{}") };
        assert_eq!(fixture.bridge.invoke(denied, Default::default()).await?.unwrap_err().kind, CodeToolRejectionKind::PermissionDenied);
        let report = fixture.bridge.close_scope(CloseCodeToolScopeRequest { execution_id: fixture.execution_id.clone(), cancel_pending: false }, Default::default()).await??;
        assert!(report.closed);
        assert_eq!(report.calls.len(), 8);
        assert!(report.calls.values().all(|call| call.status.is_terminal()));
        assert_eq!(fixture.bridge.invoke(fixture.request("late", "sleep", json!({"ms":0})).await?, Default::default()).await?.unwrap_err().kind,
            CodeToolRejectionKind::ScopeClosed);
        fixture.finish().await
    }).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn code_tool_failure_preserves_siblings_and_scope_cancellation_reports_outcomes()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let failed_request = fixture.request("failed-job", JOB, json!({})).await?;
        let pending_request = fixture.request("pending-agent", AGENT, json!({})).await?;
        // Leave the agent's Update without a client waiter, as after runner
        // loss. The session still owns the admitted effect and cleanup.
        let abandoned = fixture
            .client
            .get_workflow_handle::<AgentSessionWorkflow>(compose_workflow_id(
                live_universe_id()?,
                &fixture.session_id,
            ))
            .start_update(
                AgentSessionWorkflow::invoke_code_tool,
                pending_request,
                WorkflowStartUpdateOptions::builder()
                    .update_id(uuid::Uuid::new_v4().to_string())
                    .build(),
            )
            .await?;
        let calls = async {
            let failed = fixture
                .bridge
                .invoke(failed_request.clone(), Default::default())
                .await??;
            assert_eq!(failed.status, CodeToolCallStatus::Failed);
            Ok::<_, anyhow::Error>(())
        };
        let cleanup = async {
            let (job, _agent) =
                tokio::try_join!(fixture.invocation(JOB), fixture.invocation(AGENT))?;
            let error_ref = fixture
                .store
                .put_bytes(b"simulated job failure".to_vec())
                .await?;
            fixture
                .resolve(
                    &job,
                    PromiseResolution::Failed {
                        error_ref: Some(error_ref),
                    },
                )
                .await?;
            support::live::wait_until(
                "failed sibling recorded",
                Duration::from_secs(10),
                async || {
                    Ok(fixture
                        .report()
                        .await?
                        .calls
                        .get("failed-job")
                        .is_some_and(|call| call.status == CodeToolCallStatus::Failed))
                },
            )
            .await?;
            assert_eq!(
                fixture.report().await?.calls["pending-agent"].status,
                CodeToolCallStatus::Waiting,
                "one rejection does not cancel a sibling"
            );
            fixture
                .bridge
                .close_scope(
                    CloseCodeToolScopeRequest {
                        execution_id: fixture.execution_id.clone(),
                        cancel_pending: true,
                    },
                    Default::default(),
                )
                .await??;
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(calls, cleanup)?;
        let recovered = abandoned.get_result(Default::default()).await??;
        assert_eq!(recovered.status, CodeToolCallStatus::Cancelled);
        let report = fixture.report().await?;
        assert!(report.closed && report.cancel_requested);
        assert_eq!(report.calls.len(), 2);
        assert_eq!(
            fixture
                .bridge
                .invoke(failed_request, Default::default())
                .await??,
            report.calls["failed-job"]
        );
        fixture.finish().await
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn forced_session_close_settles_code_tool_updates_and_completes_workflow()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    scenario(|fixture| async move {
        let session =
            fixture
                .client
                .get_workflow_handle::<AgentSessionWorkflow>(compose_workflow_id(
                    live_universe_id()?,
                    &fixture.session_id,
                ));
        let pending = session
            .start_update(
                AgentSessionWorkflow::invoke_code_tool,
                fixture
                    .request("interrupted-agent", AGENT, json!({}))
                    .await?,
                WorkflowStartUpdateOptions::builder()
                    .update_id(uuid::Uuid::new_v4().to_string())
                    .build(),
            )
            .await?;
        fixture.invocation(AGENT).await?;
        fixture
            .api
            .close_session(api::SessionCloseParams {
                session_id: fixture.session_id.as_str().to_owned(),
                force: true,
            })
            .await?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            pending.get_result(Default::default()),
        )
        .await???;
        assert_eq!(outcome.status, CodeToolCallStatus::Unavailable);
        assert!(outcome.error_ref.is_some());
        // Settling the last Update must wake the main loop to finish closing,
        // even when there is no further signal, activity, or timer to wake it.
        tokio::time::timeout(
            Duration::from_secs(10),
            session.get_result(Default::default()),
        )
        .await??;
        let state = fixture.state().await?;
        assert_eq!(state.lifecycle.status, harness::CoreAgentStatus::Closed);
        let scope = &state.code_tools.scopes[&fixture.execution_id];
        assert!(scope.closed && scope.cancel_requested);
        assert!(scope.calls.values().all(|call| call.status.is_terminal()));
        assert_eq!(fixture.generations.load(Ordering::SeqCst), 1);
        Ok(())
    })
    .await
}
