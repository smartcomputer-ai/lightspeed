//! The public session feature starts a real code workflow on its own queue.
//! Only model generation is scripted; admission, JS, activities and reporting
//! use production implementations and the local Temporal/PostgreSQL stack.

mod support;

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use api::AgentApiService;
use async_trait::async_trait;
use harness::{
    BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError,
    CoreAgentLlm, CoreAgentState, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, ObservedToolCall, PromiseResolution, SessionId,
    ToolCallId, ToolName, WorkflowToolTarget,
    storage::{BlobStore, ReadSessionEvents, SessionStore},
};
use serde_json::{Value, json};
use support::live::{
    LIVE_TEST_LOCK, live_universe_id, require_storage_live_env, run_with_live_worker_builder,
    seed_agent_default, start_text_run, wait_for_terminal_run, wait_until,
};
use temporal_runtime::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{
        ActivityState, CodeWorkerActivities, SessionTools, WorkerActivities, code_worker,
        worker_runtime,
    },
};
use temporal_workflow::{
    ACTIVITY_CODE_FINALIZE, ACTIVITY_CODE_PREPARE, ACTIVITY_CODE_RUN, CodeExecutionDescriptor,
    CodeExecutionPhase, CodeExecutionSnapshot, CodeExecutionWorkflow, CodeFinalizeActivityRequest,
    CodePrepareActivityRequest, CodePrepareActivityResult, CodeRunActivityResult, ReducedSession,
    reduce_session_entries_from,
};
use temporalio_client::{Client, WorkflowExecutionStatus, WorkflowQueryOptions};
use temporalio_common::protos::temporal::api::history::v1::history_event::Attributes;
use temporalio_macros::activities;
use temporalio_sdk::{
    ApplicationFailure, Worker, WorkerOptions,
    activities::{ActivityContext, ActivityError},
    workflow_replayer::{WorkflowReplayer, WorkflowReplayerOptions},
};

const TOOL: &str = "code_execute";

struct CodeCallLlm {
    store: Arc<store_pg::PgStore>,
    arguments: Value,
    generations: Arc<AtomicUsize>,
}

#[async_trait]
impl CoreAgentLlm for CodeCallLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.generations.fetch_add(1, Ordering::SeqCst);
        let has_result = request
            .request
            .context
            .entries
            .iter()
            .any(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }));
        let bytes = if has_result {
            b"code workflow completed".to_vec()
        } else {
            serde_json::to_vec(&self.arguments).unwrap()
        };
        let reference =
            self.store
                .put_bytes(bytes)
                .await
                .map_err(|error| CoreAgentIoError::Failed {
                    message: error.to_string(),
                })?;
        let (kind, finish, calls) = if has_result {
            (
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                LlmFinish::Stop,
                Vec::new(),
            )
        } else {
            let tool_id = test_support::scripted_tool_id(&request, TOOL);
            assert!(
                tool_id.is_some(),
                "feature must advertise its ordinary workflow tool"
            );
            let call_id = ToolCallId::new("code-call");
            let name = ToolName::new(TOOL);
            (
                ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                },
                LlmFinish::ToolCalls,
                vec![ObservedToolCall {
                    call_id,
                    tool_id,
                    tool_name: name,
                    provider_kind: None,
                    arguments_ref: reference.clone(),
                    native_call_ref: None,
                }],
            )
        };
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind,
                content: harness::ContentRef::text(reference),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: None,
                finish,
                usage: None,
                tool_calls: calls,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Fault {
    #[default]
    None,
    LoseRunReceipt,
    RetryLifecycleReceipts,
    MissingRunReport,
    PauseFinalization,
}

#[derive(Default)]
struct ActivityProbe {
    fault: Fault,
    prepare_attempts: AtomicUsize,
    run_attempts: AtomicUsize,
    finalize_attempts: AtomicUsize,
    release_finalization: tokio::sync::Notify,
}

/// Fail after real work or hold a receipt at a known boundary. Production
/// activity implementations still perform every admission, effect and cleanup.
pub struct FaultActivities {
    inner: Arc<CodeWorkerActivities>,
    probe: Arc<ActivityProbe>,
}

#[activities]
impl FaultActivities {
    #[activity(name = ACTIVITY_CODE_PREPARE)]
    pub async fn prepare(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CodePrepareActivityRequest,
    ) -> Result<CodePrepareActivityResult, ActivityError> {
        let attempt = self.probe.prepare_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(ctx.info().attempt as usize, attempt);
        let result = self.inner.clone().__code_prepare(ctx, request).await?;
        if self.probe.fault == Fault::RetryLifecycleReceipts && attempt == 1 {
            return Err(lost_receipt("prepare"));
        }
        Ok(result)
    }

    #[activity(name = ACTIVITY_CODE_RUN)]
    pub async fn run(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CodeExecutionDescriptor,
    ) -> Result<CodeRunActivityResult, ActivityError> {
        self.probe.run_attempts.fetch_add(1, Ordering::SeqCst);
        let mut result = self.inner.clone().__code_run(ctx, request).await?;
        match self.probe.fault {
            Fault::LoseRunReceipt => return Err(lost_receipt("run")),
            Fault::MissingRunReport => {
                result.report_ref = BlobRef::from_bytes(uuid::Uuid::new_v4().as_bytes());
            }
            _ => {}
        }
        Ok(result)
    }

    #[activity(name = ACTIVITY_CODE_FINALIZE)]
    pub async fn finalize(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CodeFinalizeActivityRequest,
    ) -> Result<PromiseResolution, ActivityError> {
        let attempt = self.probe.finalize_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if self.probe.fault == Fault::RetryLifecycleReceipts {
            assert_eq!(ctx.info().attempt as usize, attempt);
        }
        let result = self
            .inner
            .clone()
            .__code_finalize(ctx.clone(), request)
            .await?;
        if self.probe.fault == Fault::RetryLifecycleReceipts && attempt == 1 {
            return Err(lost_receipt("finalize"));
        }
        if self.probe.fault == Fault::PauseFinalization && attempt == 1 {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    () = self.probe.release_finalization.notified() => break,
                    _ = tick.tick() => { ctx.record_heartbeat(()).await?; }
                }
            }
        }
        Ok(result)
    }
}

fn lost_receipt(stage: &str) -> ActivityError {
    ActivityError::application(ApplicationFailure::new(anyhow::anyhow!(
        "simulated lost {stage} receipt"
    )))
}

struct Fixture {
    client: Client,
    api: Arc<GatewayAgentApi>,
    store: Arc<store_pg::PgStore>,
    session_id: SessionId,
    run_id: String,
    generations: Arc<AtomicUsize>,
    probe: Arc<ActivityProbe>,
}

impl Fixture {
    async fn state(&self) -> anyhow::Result<CoreAgentState> {
        let mut reduced = ReducedSession::default();
        let mut after = None;
        loop {
            let page = self
                .store
                .read_after(ReadSessionEvents {
                    session_id: self.session_id.clone(),
                    after,
                    limit: 1000,
                })
                .await?;
            reduced = reduce_session_entries_from(reduced, &page.entries)?;
            if page.complete {
                return Ok(reduced.core_state);
            }
            after = page.next_after;
        }
    }

    async fn execution_id(&self) -> anyhow::Result<String> {
        let state = self.state().await?;
        let invocation = state
            .workflow_tools
            .start_requests
            .values()
            .find(|call| call.tool_id.as_str() == tools::code::CODE_EXECUTE_WORKFLOW_TOOL_ID)
            .ok_or_else(|| anyhow::anyhow!("code tool was not started"))?;
        let binding = &state.workflow_tools.bindings[&invocation.tool_id];
        let WorkflowToolTarget::Start { start } = &binding.target else {
            anyhow::bail!("code tool must start its own workflow")
        };
        Ok(harness::workflow_tool_execution_id(
            &invocation.invocation_id,
            &start.recipe_fingerprint,
        ))
    }

    async fn snapshot(&self) -> anyhow::Result<CodeExecutionSnapshot> {
        Ok(self
            .client
            .get_workflow_handle::<CodeExecutionWorkflow>(self.execution_id().await?)
            .query(
                CodeExecutionWorkflow::snapshot,
                (),
                WorkflowQueryOptions::default(),
            )
            .await?)
    }

    async fn report(&self) -> anyhow::Result<(Value, Value)> {
        let snapshot = self.snapshot().await?;
        let Some(PromiseResolution::Resolved {
            payload_ref: Some(reference),
        }) = snapshot.resolution
        else {
            anyhow::bail!("workflow has no resolved report: {snapshot:?}")
        };
        let model: Value = serde_json::from_slice(&self.store.read_bytes(&reference).await?)?;
        let reference = BlobRef::parse(
            model["report_ref"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing detailed report ref: {model}"))?,
        )?;
        let detail = serde_json::from_slice(&self.store.read_bytes(&reference).await?)?;
        Ok((model, detail))
    }

    async fn verify_history(&self) -> anyhow::Result<()> {
        let handle = self
            .client
            .get_workflow_handle::<CodeExecutionWorkflow>(self.execution_id().await?);
        wait_until(
            "code workflow history completion",
            Duration::from_secs(15),
            async || {
                Ok(handle.describe(Default::default()).await?.status()
                    != WorkflowExecutionStatus::Running)
            },
        )
        .await?;
        let events = handle
            .fetch_history(Default::default())
            .into_events()
            .await?;
        let scheduled = events
            .iter()
            .filter_map(|event| match &event.attributes {
                Some(Attributes::ActivityTaskScheduledEventAttributes(activity)) => Some(activity),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            scheduled
                .iter()
                .filter(
                    |activity| activity.activity_type.as_ref().unwrap().name == ACTIVITY_CODE_RUN
                )
                .count(),
            1,
            "one source attempt must schedule exactly one RunCode activity"
        );
        for activity in &scheduled {
            let name = &activity.activity_type.as_ref().unwrap().name;
            let expected = if name == ACTIVITY_CODE_RUN { 1 } else { 3 };
            assert_eq!(
                activity.retry_policy.as_ref().unwrap().maximum_attempts,
                expected,
                "{name}"
            );
        }
        let calls_before = self.state().await?.code_tools.scopes;
        let attempts_before = self.probe.run_attempts.load(Ordering::SeqCst);
        WorkflowReplayer::new(
            WorkflowReplayerOptions::new()
                .register_workflow::<CodeExecutionWorkflow>()?
                .build(),
        )?
        .replay_workflow(handle.fetch_history(Default::default()))
        .await?;
        assert_eq!(
            self.probe.run_attempts.load(Ordering::SeqCst),
            attempts_before
        );
        assert_eq!(
            self.state().await?.code_tools.scopes,
            calls_before,
            "replaying the supervisor must not redispatch any effect"
        );
        Ok(())
    }

    async fn completed(&self) -> anyhow::Result<(Value, Value)> {
        let run = wait_for_terminal_run(&self.api, &self.session_id, &self.run_id).await?;
        assert_eq!(run.status, api::RunStatus::Completed, "{run:?}");
        assert_eq!(
            self.generations.load(Ordering::SeqCst),
            2,
            "code tool calls must not become model turns"
        );
        let report = self.report().await?;
        self.verify_history().await?;
        assert_eq!(report.1["scope"]["closed"], true);
        assert!(report.1["cleanup_error"].is_null(), "{}", report.1);
        Ok(report)
    }
}

async fn scenario<F, Fut>(
    arguments: Value,
    feature: api::CodeModeFeature,
    lose_receipt: bool,
    check: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    scenario_with_fault(
        arguments,
        feature,
        if lose_receipt {
            Fault::LoseRunReceipt
        } else {
            Fault::None
        },
        check,
    )
    .await
}

async fn scenario_with_fault<F, Fut>(
    arguments: Value,
    feature: api::CodeModeFeature,
    fault: Fault,
    check: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    let store = pg_store_from_env().await?;
    seed_agent_default(&store, &support::live::openai_live_model()).await?;
    let code_queue = format!("code-live-{}", uuid::Uuid::new_v4().simple());
    let activity_queue = code_queue.clone();
    let activity_store = store.clone();
    let generations = Arc::new(AtomicUsize::new(0));
    let activity_generations = generations.clone();
    run_with_live_worker_builder(
        move |client, _| async move {
            let tools = Arc::new(SessionTools::new(activity_store.clone(), activity_store.clone()));
            let llm = Arc::new(CodeCallLlm { store: activity_store.clone(), arguments, generations: activity_generations });
            Ok(WorkerActivities::for_universe(activity_store.config().universe_id,
                ActivityState::from_pg_store(activity_store, llm, tools.clone())
                    .with_hosted_tools(tools).with_workflow_tool_executions(client)
                    .with_code_task_queue(activity_queue)))
        },
        move |client, queue, session_id| async move {
            let api = Arc::new(GatewayAgentApi::builder(client.clone(), store.clone())
                .with_task_queue(queue).with_code_task_queue(code_queue.clone()).build());
            let activities = CodeWorkerActivities::for_universe(live_universe_id()?, api.clone(), 2)?;
            let runtime = worker_runtime()?;
            let probe = Arc::new(ActivityProbe { fault, ..Default::default() });
            let mut worker = if fault != Fault::None {
                Worker::new(&runtime, client.clone(), WorkerOptions::new(code_queue)
                    .register_workflow::<CodeExecutionWorkflow>()?
                    .register_activities(FaultActivities { inner: Arc::new(activities), probe: probe.clone() }).build())?
            } else { code_worker(&runtime, client.clone(), code_queue, activities)? };
            let shutdown = worker.shutdown_handle();
            let worker_run = worker.run();
            tokio::pin!(worker_run);
            let body = Box::pin(async {
                api.start_session(api::SessionStartParams {
                    session_id: Some(session_id.to_string()), display_name: None, access: None,
                    metadata: Default::default(), profile: None, delete_after_close_ms: None,
                    config: Some(api::SessionConfig { features: Some(api::FeaturesConfig {
                        code_mode: Some(feature), timers: Some(api::TimersFeature { version: api::CURRENT_FEATURE_VERSION }),
                        ..Default::default()
                    }), ..Default::default() }),
                }).await?;
                let run = start_text_run(&api, &session_id, "compose the granted tools using JavaScript").await?;
                let result = check(Fixture { client, api: api.clone(), store, session_id: session_id.clone(), run_id: run.id, generations, probe }).await;
                let _ = api.close_session(api::SessionCloseParams { session_id: session_id.to_string(), force: true }).await;
                result
            });
            let result = tokio::select! {
                result = body => result,
                result = &mut worker_run => Err(anyhow::anyhow!("code worker stopped early: {result:?}")),
            };
            shutdown();
            let stopped = tokio::time::timeout(Duration::from_secs(15), &mut worker_run).await;
            result?;
            stopped.map_err(|_| anyhow::anyhow!("code worker did not stop"))??;
            Ok(())
        },
    ).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn admitted_code_tool_runs_on_separate_queue_and_returns_selected_output()
-> anyhow::Result<()> {
    scenario(json!({"code": r#"
        const promises = [];
        for (let round = 0; round < 2; round++) {
            const batch = await Promise.all([1, 2].map(ms => tools.sleep({ms})));
            promises.push(...batch.map(item => item.promise));
        }
        const results = await tools.await({promises, mode: "all", timeout_ms: 5000});
        text({count: results.results.length});
        return {statuses: results.results.map(item => item.status), recursive: typeof tools.code_execute};
    "#}), api::CodeModeFeature::default(), false, |fixture| async move {
        let (model, detail) = fixture.completed().await?;
        assert_eq!(model["status"], "succeeded", "{model}");
        assert_eq!(model["output_available"], true);
        assert_eq!(model["output"], json!([{"count":4}]));
        assert_eq!(model["return_value"], json!({"statuses":["resolved","resolved","resolved","resolved"],"recursive":"undefined"}));
        assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 5);
        let snapshot = fixture.snapshot().await?;
        assert_eq!(snapshot.phase, CodeExecutionPhase::Resolved);
        let recovered = fixture.client.get_workflow_handle::<CodeExecutionWorkflow>(fixture.execution_id().await?)
            .query(CodeExecutionWorkflow::workflow_tool_recovery, (), WorkflowQueryOptions::default()).await?;
        assert_eq!(recovered.resolutions.get(harness::REPLY_COMPLETION_KEY), snapshot.resolution.as_ref());
        Ok(())
    }).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn script_failure_returns_partial_effects_to_the_parent_model() -> anyhow::Result<()> {
    scenario(
        json!({"code":"text(await tools.sleep({ms:1})); throw new Error('stop after effect');"}),
        api::CodeModeFeature::default(),
        false,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "failed");
            assert_eq!(model["error"]["kind"], "javascript");
            assert_eq!(model["output"].as_array().unwrap().len(), 1);
            assert_eq!(detail["scope"]["calls"]["call-1"]["status"], "succeeded");
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn code_deadline_returns_a_report_without_repeating_completed_effects() -> anyhow::Result<()>
{
    scenario(
        json!({"code":"text(await tools.sleep({ms:1})); for (;;) {}", "timeout_ms":1500}),
        api::CodeModeFeature::default(),
        false,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "failed", "{model}");
            assert_eq!(model["error"]["kind"], "timed_out");
            assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 1);
            assert_eq!(model["output"].as_array().unwrap().len(), 1);
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn lost_runner_receipt_recovers_effect_outcomes_without_reexecuting_source()
-> anyhow::Result<()> {
    scenario(
        json!({"code":"text(await tools.sleep({ms:1})); return 'done';"}),
        api::CodeModeFeature::default(),
        true,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "interrupted", "{model}");
            assert_eq!(model["output_available"], false);
            assert_eq!(model["interruption"], "activity_failed");
            assert_eq!(fixture.probe.run_attempts.load(Ordering::SeqCst), 1);
            assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 1);
            assert_eq!(detail["scope"]["calls"]["call-1"]["status"], "succeeded");
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn empty_capability_selection_allows_only_local_computation() -> anyhow::Result<()> {
    scenario(json!({"code":"return {value:6*7, sleep:typeof tools.sleep, recursive:typeof tools.code_execute};"}),
        api::CodeModeFeature { allowed_tools: Some(Vec::new()), ..Default::default() }, false, |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "succeeded", "{model}");
            assert_eq!(model["return_value"], json!({"value":42,"sleep":"undefined","recursive":"undefined"}));
            assert!(detail["scope"]["calls"].as_object().unwrap().is_empty());
            assert!(detail["scope"]["bindings"].as_object().unwrap().is_empty());
            Ok(())
        }).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn cancelling_parent_run_closes_the_code_scope_and_pending_wait() -> anyhow::Result<()> {
    scenario(
        json!({"code":r#"
        const timer = await tools.sleep({ms:60000});
        text({waiting:timer.promise});
        try {
            await tools.await({promises:[timer.promise], mode:"all", timeout_ms:60000});
        } finally {
            // Keep the guest alive if its wait observes parent cancellation
            // before the code workflow does; native cancellation must stop it.
            while (true) {}
        }
    "#}),
        api::CodeModeFeature {
            timeout_ms: 90_000,
            ..Default::default()
        },
        false,
        |fixture| async move {
            wait_until(
                "code tool durable wait",
                Duration::from_secs(20),
                async || {
                    let state = fixture.state().await?;
                    Ok(state.code_tools.scopes.values().any(|scope| {
                        scope.calls.values().any(|call| {
                            matches!(call.status, harness::CodeToolCallStatus::Waiting { .. })
                        })
                    }))
                },
            )
            .await?;
            fixture
                .api
                .cancel_run(api::RunCancelParams {
                    session_id: fixture.session_id.to_string(),
                    run_id: fixture.run_id.clone(),
                })
                .await?;
            wait_until(
                "code workflow cancellation cleanup",
                Duration::from_secs(40),
                async || {
                    let snapshot = fixture.snapshot().await?;
                    anyhow::ensure!(
                        snapshot.phase != CodeExecutionPhase::Resolved,
                        "code finished before cancellation was observed: {snapshot:?}"
                    );
                    Ok(snapshot.phase == CodeExecutionPhase::Cancelled)
                },
            )
            .await?;
            let (model, detail) = fixture.report().await?;
            assert_eq!(model["status"], "cancelled", "{model}");
            assert_eq!(
                model["output_available"], true,
                "graceful stop must preserve its receipt: {model}"
            );
            assert_eq!(
                model["output"].as_array().unwrap().len(),
                1,
                "output before cancellation must survive: {model}"
            );
            assert_eq!(detail["scope"]["closed"], true);
            assert!(
                detail["scope"]["calls"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|call| !matches!(call["status"].as_str(), Some("pending" | "waiting")))
            );
            assert_eq!(fixture.generations.load(Ordering::SeqCst), 1);
            fixture.verify_history().await?;
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn lifecycle_receipts_retry_without_reopening_or_reexecuting_source() -> anyhow::Result<()> {
    scenario_with_fault(
        json!({"code":"text(await tools.sleep({ms:1})); return 42;"}),
        api::CodeModeFeature::default(),
        Fault::RetryLifecycleReceipts,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "succeeded", "{model}");
            assert_eq!(model["return_value"], 42);
            assert_eq!(model["output"].as_array().unwrap().len(), 1);
            assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 1);
            assert_eq!(fixture.state().await?.code_tools.scopes.len(), 1);
            assert_eq!(fixture.probe.prepare_attempts.load(Ordering::SeqCst), 2);
            assert_eq!(fixture.probe.run_attempts.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.probe.finalize_attempts.load(Ordering::SeqCst), 2);
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn missing_runner_report_still_closes_scope_and_retains_known_effects() -> anyhow::Result<()>
{
    scenario_with_fault(
        json!({"code":"text(await tools.sleep({ms:1})); return 42;"}),
        api::CodeModeFeature::default(),
        Fault::MissingRunReport,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "interrupted", "{model}");
            assert_eq!(model["output_available"], false);
            assert!(model["report_unavailable"].is_string());
            assert_eq!(detail["scope"]["calls"]["call-1"]["status"], "succeeded");
            assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 1);
            assert_eq!(fixture.probe.run_attempts.load(Ordering::SeqCst), 1);
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn cancellation_during_finalization_rewrites_report_without_reexecuting_source()
-> anyhow::Result<()> {
    scenario_with_fault(
        json!({"code":"text(await tools.sleep({ms:1})); return 42;"}),
        api::CodeModeFeature::default(),
        Fault::PauseFinalization,
        |fixture| async move {
            wait_until("first finalization", Duration::from_secs(20), async || {
                Ok(fixture.probe.finalize_attempts.load(Ordering::SeqCst) == 1)
            })
            .await?;
            let handle = fixture
                .client
                .get_workflow_handle::<CodeExecutionWorkflow>(fixture.execution_id().await?);
            handle.cancel(Default::default()).await?;
            // The cancellation event precedes the held activity completion in
            // server history. Avoid sleeps or racing a naturally finished script.
            let events = handle
                .fetch_history(Default::default())
                .into_events()
                .await?;
            assert!(events.iter().any(|event| matches!(
                event.attributes,
                Some(Attributes::WorkflowExecutionCancelRequestedEventAttributes(
                    _
                ))
            )));
            fixture.probe.release_finalization.notify_one();
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "cancelled", "{model}");
            assert_eq!(model["output_available"], true);
            assert_eq!(model["output"].as_array().unwrap().len(), 1);
            assert_eq!(model["return_value"], 42);
            assert_eq!(detail["scope"]["calls"].as_object().unwrap().len(), 1);
            assert_eq!(
                fixture.snapshot().await?.phase,
                CodeExecutionPhase::Cancelled
            );
            assert_eq!(fixture.probe.run_attempts.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.probe.finalize_attempts.load(Ordering::SeqCst), 2);
            assert_eq!(
                handle.describe(Default::default()).await?.status(),
                WorkflowExecutionStatus::Canceled
            );
            Ok(())
        },
    )
    .await
}
