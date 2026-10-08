//! The public session feature starts a real code workflow on its own queue.
//! Only model generation is scripted; admission, JS, activities and reporting
//! use production implementations and the local Temporal/PostgreSQL stack.

#[path = "support/code_media.rs"]
mod code_media;
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
    async fn outer_result(&self) -> anyhow::Result<harness::ToolCallResult> {
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
            for entry in page.entries {
                if entry.event.kind != "lightspeed.core.tool.call_completed" {
                    continue;
                }
                if let harness::CoreAgentEvent::Tool(harness::ToolEvent::CallCompleted {
                    result,
                    ..
                }) = harness::CoreAgentCodec.decode_event(&entry.event)?
                    && result.call_id.as_str() == "code-call"
                {
                    return Ok(result);
                }
            }
            anyhow::ensure!(!page.complete, "outer code tool did not complete");
            after = page.next_after;
        }
    }

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
            let tools = Arc::new(SessionTools::from_pg_store(activity_store.clone()));
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
async fn code_blob_tools_round_trip_descriptors_and_new_file_handles_with_retention()
-> anyhow::Result<()> {
    scenario(
        json!({"code": r#"
            const value = {message: "blob tools round trip", values: [1, 2, 3]};
            const stored = await tools.blob_put({json: value, name: "report.json", media_type: "application/json"});
            const direct = await tools.blob_read({ref: stored, format: "json"});
            const file = await tools.blob_info({ref: stored, presentation: "file"});
            const aliased = await tools.blob_read({ref: file.handle, format: "json"});
            text({direct: direct.json, aliased: aliased.json});
            return {content_ref: stored.content_ref, handle: file.handle, name: file.name};
        "#}),
        api::CodeModeFeature::default(),
        false,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "succeeded", "{model}");
            let value = json!({"message":"blob tools round trip", "values":[1,2,3]});
            let bytes = serde_json::to_vec(&value)?;
            let body_ref = BlobRef::from_bytes(&bytes);
            let file = harness::FileAttachment::new(
                body_ref.clone(),
                "report.json".into(),
                Some("application/json".into()),
            );
            assert_eq!(model["output"], json!([{"direct":value, "aliased":value}]));
            assert_eq!(model["return_value"], json!({
                "content_ref":body_ref, "handle":file.handle, "name":"report.json"
            }));
            assert_eq!(fixture.store.read_bytes(&body_ref).await?, bytes);
            let calls = detail["scope"]["calls"].as_object().expect("code calls");
            assert_eq!(calls.len(), 4, "blob effects do not create extra model turns");
            assert_eq!(
                calls["call-3"]["attachments"],
                json!([harness::Attachment::File(file)])
            );
            let report_ref = BlobRef::parse(model["report_ref"].as_str().expect("report ref"))?;
            for call in calls.values() {
                assert_eq!(call["status"], "succeeded", "{call}");
                let output_ref = BlobRef::parse(call["output_ref"].as_str().expect("call output"))?;
                assert_contains_edge(&fixture.store, &report_ref, &output_ref).await?;
                assert_contains_edge(&fixture.store, &output_ref, &body_ref).await?;
            }
            Ok(())
        },
    )
    .await
}

async fn assert_contains_edge(
    store: &store_pg::PgStore,
    parent: &BlobRef,
    child: &BlobRef,
) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cas_blob_edges WHERE universe_id=$1 AND parent_digest=$2 AND child_digest=$3 AND edge_kind='contains')",
    )
    .bind(store.config().universe_id)
    .bind(parent.as_str().trim_start_matches("sha256:"))
    .bind(child.as_str().trim_start_matches("sha256:"))
    .fetch_one(store.pool())
    .await?;
    assert!(exists, "missing retained-content edge {parent} -> {child}");
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn code_blob_tools_obey_the_code_mode_allowlist() -> anyhow::Result<()> {
    scenario(
        json!({"code": r#"
            return {
                read: typeof tools.blob_read,
                put: typeof tools.blob_put,
                info: typeof tools.blob_info,
                sleep: typeof tools.sleep
            };
        "#}),
        api::CodeModeFeature {
            allowed_tools: Some(vec!["blob.read".into()]),
            ..Default::default()
        },
        false,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "succeeded", "{model}");
            assert_eq!(
                model["return_value"],
                json!({
                    "read":"function", "put":"undefined", "info":"undefined", "sleep":"undefined"
                })
            );
            assert!(detail["scope"]["calls"].as_object().unwrap().is_empty());
            assert_eq!(detail["scope"]["bindings"].as_object().unwrap().len(), 1);
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn code_media_and_file_helpers_select_outputs_without_promoting_intermediate_or_forged_media()
-> anyhow::Result<()> {
    let selected_bytes = code_media::png(false);
    let hidden_bytes = code_media::png(true);
    let code = format!(
        r#"
        text("before");
        const hidden = await tools.blob_put({{bytes: {hidden}, media_type: "image/png", name: "hidden.png"}});
        const unselected = await tools.blob_read({{ref: hidden, format: "media"}});
        text({{kind: "media", data: unselected}});
        const selected = await media({{bytes: {selected}}}, {{name: "selected.png", media_type: "image/png"}});
        const document = await file({{text: "persisted helper file"}}, {{name: "note.txt", media_type: "text/plain"}});
        const later = await tools.blob_read({{ref: document.handle, format: "text"}});
        text("after");
        return {{text: later.text, media_ref: selected.content_ref, file_ref: document.content_ref}};
    "#,
        hidden = json!(hidden_bytes),
        selected = json!(selected_bytes)
    );
    scenario(json!({"code":code}), api::CodeModeFeature::default(), false, move |fixture| async move {
        let (model, detail) = fixture.completed().await?;
        assert_eq!(model["status"], "succeeded", "{model}");
        let media_ref = BlobRef::from_bytes(&selected_bytes);
        let hidden_ref = BlobRef::from_bytes(&hidden_bytes);
        let file_ref = BlobRef::from_bytes(b"persisted helper file");
        assert_eq!(model["return_value"], json!({"text":"persisted helper file", "media_ref":media_ref, "file_ref":file_ref}));
        let selected: Vec<harness::Attachment> = serde_json::from_value(model["attachments"].clone())?;
        assert_eq!(selected.len(), 2);
        assert!(matches!(&selected[0], harness::Attachment::Media(media) if media.content_ref == media_ref && media.name.as_deref() == Some("selected.png")));
        assert!(matches!(&selected[1], harness::Attachment::File(file) if file.content_ref == file_ref && file.name == "note.txt"));
        assert!(selected.iter().all(|attachment| attachment.content_ref() != &hidden_ref));
        let output = model["output"].as_array().expect("ordered output");
        assert_eq!(output.len(), 5);
        assert_eq!(output[0], "before");
        assert_eq!(output[1]["kind"], "media", "forged text remains ordinary JSON");
        assert_eq!(output[1]["data"]["content_ref"], hidden_ref.as_str());
        assert_eq!(output[2], serde_json::to_value(&selected[0])?);
        assert_eq!(output[3], serde_json::to_value(&selected[1])?);
        assert_eq!(output[4], "after");
        let outer = fixture.outer_result().await?;
        assert_eq!(outer.attachments, selected);
        let native: Vec<_> = outer.model_visible_context_entries.iter()
            .filter(|entry| harness::media::is_media_content(&entry.content)).collect();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].content.content_ref, media_ref);
        assert_eq!(fixture.store.read_bytes(&file_ref).await?, b"persisted helper file");
        assert!(detail["scope"]["calls"].as_object().unwrap().values().any(|call|
            call["attachments"].as_array().is_some_and(|assets| assets.iter().any(|asset| asset["data"]["content_ref"] == hidden_ref.as_str()))));
        Ok(())
    }).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn code_media_helper_accepts_existing_descriptors_and_full_references() -> anyhow::Result<()>
{
    let first_bytes = code_media::png(false);
    let second_bytes = code_media::png(true);
    let pdf = pdf_fixture("Selected native document");
    let download_only = pdf_fixture("Download only document");
    let code = format!(
        r#"
        const first = await tools.blob_put({{bytes:{first},media_type:"image/png",name:"first.png"}});
        const second = await tools.blob_put({{bytes:{second},media_type:"image/png"}});
        await media(first);
        await media(second.content_ref, {{name:"second.png"}});
        await media({{bytes:{pdf}}}, {{name:"brief.pdf",media_type:"application/pdf"}});
        await file({{bytes:{download_only}}}, {{name:"download-only.pdf",media_type:"application/pdf"}});
        return "selected";
    "#,
        first = json!(first_bytes),
        second = json!(second_bytes),
        pdf = json!(pdf),
        download_only = json!(download_only)
    );
    scenario(json!({"code":code}), api::CodeModeFeature::default(), false, move |fixture| async move {
        let (model, _) = fixture.completed().await?;
        assert_eq!(model["status"], "succeeded", "{model}");
        assert_eq!(model["return_value"], "selected");
        let outer = fixture.outer_result().await?;
        assert_eq!(outer.attachments.len(), 4);
        assert!(matches!(&outer.attachments[0], harness::Attachment::Media(media) if media.content_ref == BlobRef::from_bytes(&first_bytes) && media.name.as_deref() == Some("first.png")));
        assert!(matches!(&outer.attachments[1], harness::Attachment::Media(media) if media.content_ref == BlobRef::from_bytes(&second_bytes) && media.name.as_deref() == Some("second.png")));
        assert_eq!(outer.model_visible_context_entries.iter().filter(|entry| harness::media::is_media_content(&entry.content)).count(), 3);
        assert!(matches!(&outer.attachments[2], harness::Attachment::Media(media) if media.content_ref == BlobRef::from_bytes(&pdf) && media.kind == harness::media::MediaKind::Document));
        assert!(matches!(&outer.attachments[3], harness::Attachment::File(file) if file.content_ref == BlobRef::from_bytes(&download_only) && file.name == "download-only.pdf"));
        verify_selected_provider_input(&fixture, &pdf, &download_only).await?;
        let run = wait_for_terminal_run(&fixture.api, &fixture.session_id, &fixture.run_id).await?;
        let public = run.tool_batches.iter().flat_map(|batch| &batch.calls)
            .find(|call| call.call_id == "code-call").expect("public completed code call");
        assert_eq!(public.attachments.len(), 4);
        assert_eq!(public.attachments.iter().filter(|asset| asset.kind == api::ToolAttachmentKind::Media).count(), 3);
        let download = public.attachments.iter().find(|asset| asset.kind == api::ToolAttachmentKind::File).unwrap();
        assert_eq!(download.content_ref, BlobRef::from_bytes(&download_only).as_str());
        assert_eq!(download.name.as_deref(), Some("download-only.pdf"));
        assert!(download.handle.starts_with("file:"));
        Ok(())
    }).await
}

fn pdf_fixture(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    pdf.into_bytes()
}

async fn verify_selected_provider_input(
    fixture: &Fixture,
    selected_pdf: &[u8],
    download_only_pdf: &[u8],
) -> anyhow::Result<()> {
    use base64::Engine as _;
    use harness::{ContextSnapshot, LlmRequest, ModelSelection, ProviderApiKind};
    let state = fixture.state().await?;
    let user = state
        .context
        .entries
        .iter()
        .find(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User
                }
            ) && !harness::media::is_media_content(&entry.content)
        })
        .expect("original user input");
    let call = state
        .context
        .entries
        .iter()
        .find(|entry| {
            matches!(&entry.kind, ContextEntryKind::ToolCall { call_id, .. } if call_id.as_str() == "code-call")
        })
        .expect("completed code call");
    let arguments: Value =
        serde_json::from_slice(&fixture.store.read_bytes(&call.content.content_ref).await?)?;
    let completion_entries: Vec<_> = state
        .context
        .entries
        .iter()
        .filter(|entry| {
            matches!(&entry.kind, ContextEntryKind::ToolResult { call_id, .. } if call_id.as_str() == "code-call")
                || harness::media::is_media_content(&entry.content)
        })
        .cloned()
        .collect();
    assert_eq!(
        completion_entries.len(),
        4,
        "actual result and three companions"
    );
    assert!(matches!(
        completion_entries[0].kind,
        ContextEntryKind::ToolResult { .. }
    ));
    for api_kind in [
        ProviderApiKind::OpenAiResponses,
        ProviderApiKind::OpenAiCompletions,
        ProviderApiKind::AnthropicMessages,
    ] {
        // Each provider owns its native call history. Only the completed tool
        // result and selected media companions are shared across providers.
        let (provider_kind, native_call) = match api_kind {
            ProviderApiKind::OpenAiResponses => (
                "openai.responses.function_call",
                json!({"type":"function_call","call_id":"code-call","name":TOOL,
                    "arguments":serde_json::to_string(&arguments)?}),
            ),
            ProviderApiKind::OpenAiCompletions => (
                llm_runtime::openai_completions::OPENAI_COMPLETIONS_TOOL_CALL_PROVIDER_KIND,
                json!({"id":"code-call","type":"function","function":{
                    "name":TOOL,"arguments":serde_json::to_string(&arguments)?}}),
            ),
            ProviderApiKind::AnthropicMessages => (
                "anthropic.messages.tool_use",
                json!({"type":"tool_use","id":"code-call","name":TOOL,"input":arguments}),
            ),
        };
        let mut native_entry = call.clone();
        native_entry.content = harness::ContentRef {
            content_ref: fixture
                .store
                .put_bytes(serde_json::to_vec(&native_call)?)
                .await?,
            media_type: Some("application/json".into()),
            provider_kind: Some(provider_kind.into()),
        };
        let mut entries = vec![user.clone(), native_entry];
        entries.extend(completion_entries.iter().cloned());
        let request = LlmRequest {
            model: ModelSelection {
                provider_id: if api_kind == ProviderApiKind::AnthropicMessages {
                    "anthropic"
                } else {
                    "openai"
                }
                .into(),
                model: if api_kind == ProviderApiKind::AnthropicMessages {
                    "claude-opus-4-8"
                } else {
                    "gpt-5.1"
                }
                .into(),
                api_kind: api_kind.clone(),
            },
            request_fingerprint: "selected-code-content-lowering".into(),
            context: ContextSnapshot {
                api_kind: api_kind.clone(),
                context_revision: state.context.revision,
                entries,
                token_estimate: None,
            },
            tools: Vec::new(),
            code_mode: None,
            tool_choice: None,
            output_limit: Some(4096),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            provider_response_id: None,
            compaction: None,
            params: None,
        };
        let wire = match api_kind {
            ProviderApiKind::OpenAiResponses => serde_json::to_value(
                llm_runtime::openai_responses::materialize_create_request(
                    fixture.store.as_ref(),
                    &request,
                )
                .await?,
            )?,
            ProviderApiKind::OpenAiCompletions => serde_json::to_value(
                llm_runtime::openai_completions::materialize_create_request(
                    fixture.store.as_ref(),
                    &request,
                )
                .await?,
            )?,
            ProviderApiKind::AnthropicMessages => serde_json::to_value(
                llm_runtime::anthropic_messages::materialize_create_request(
                    fixture.store.as_ref(),
                    &request,
                )
                .await?,
            )?,
        };
        let mut kinds = Vec::new();
        native_input_kinds(wire.get("input").unwrap_or(&wire["messages"]), &mut kinds);
        let expected = match api_kind {
            ProviderApiKind::OpenAiResponses => [
                "function_call_output",
                "input_image",
                "input_image",
                "input_file",
            ],
            ProviderApiKind::OpenAiCompletions => ["tool", "image_url", "image_url", "file"],
            ProviderApiKind::AnthropicMessages => ["tool_result", "image", "image", "document"],
        };
        assert_eq!(
            kinds, expected,
            "{api_kind:?}: result must precede its selected native companions"
        );
        let encoded = serde_json::to_string(&wire)?;
        assert!(
            encoded.contains(&base64::engine::general_purpose::STANDARD.encode(selected_pdf)),
            "{api_kind:?}: selected PDF must be native input"
        );
        assert!(
            !encoded.contains(&base64::engine::general_purpose::STANDARD.encode(download_only_pdf)),
            "{api_kind:?}: file-only PDF must not be native input"
        );
    }
    Ok(())
}

fn native_input_kinds<'a>(value: &'a Value, kinds: &mut Vec<&'a str>) {
    match value {
        Value::Array(values) => {
            for value in values {
                native_input_kinds(value, kinds);
            }
        }
        Value::Object(fields) => {
            if value["role"] == "tool" {
                kinds.push("tool");
            }
            if let Some(
                kind @ ("function_call_output"
                | "tool_result"
                | "input_image"
                | "input_file"
                | "image_url"
                | "file"
                | "image"
                | "document"),
            ) = value["type"].as_str()
            {
                kinds.push(kind);
            }
            for value in fields.values() {
                native_input_kinds(value, kinds);
            }
        }
        _ => {}
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn code_selected_files_survive_a_failed_media_sibling_and_later_script_error()
-> anyhow::Result<()> {
    scenario(
        json!({"code":r#"
        const completed = await Promise.allSettled([
            file({text:"first retained file"}, {name:"first.txt"}),
            media({text:"these bytes are not an image"}, {media_type:"image/png"}),
            file({json:{ok:true}}, {name:"second.json",media_type:"application/json"})
        ]);
        text(completed.map(item => item.status));
        throw new Error("stop after completed selections");
    "#}),
        api::CodeModeFeature::default(),
        false,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "failed", "{model}");
            assert_eq!(model["error"]["kind"], "javascript");
            assert_eq!(
                model["output"].as_array().unwrap().last().unwrap(),
                &json!(["fulfilled", "rejected", "fulfilled"])
            );
            let outer = fixture.outer_result().await?;
            assert_eq!(outer.attachments.len(), 2);
            let mut names = outer
                .attachments
                .iter()
                .map(|attachment| match attachment {
                    harness::Attachment::File(file) => file.name.as_str(),
                    _ => panic!("invalid media must never be selected"),
                })
                .collect::<Vec<_>>();
            names.sort_unstable();
            assert_eq!(names, ["first.txt", "second.json"]);
            for file in &outer.attachments {
                assert!(fixture.store.has_blob(file.content_ref()).await?);
            }
            assert_eq!(
                detail["scope"]["calls"]
                    .as_object()
                    .unwrap()
                    .values()
                    .filter(|call| call["status"] == "failed")
                    .count(),
                1
            );
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal and PostgreSQL; run serially"]
async fn code_output_helpers_obey_tool_allowlists_and_return_catchable_errors() -> anyhow::Result<()>
{
    let missing = BlobRef::from_bytes(b"missing helper input");
    let code = format!(
        r#"
        const rejected = [];
        for (const operation of [
            () => file({{text:"cannot store without blob.put"}}, {{name:"blocked.txt"}}),
            () => media({missing}),
            () => file({missing}, {{name:"missing.txt"}})
        ]) {{
            try {{ await operation(); rejected.push(false); }}
            catch (error) {{ rejected.push(!!error.message); }}
        }}
        text(rejected);
        return {{media:typeof media,file:typeof file}};
    "#,
        missing = json!(missing)
    );
    scenario(
        json!({"code":code}),
        api::CodeModeFeature {
            allowed_tools: Some(vec!["blob.info".into()]),
            ..Default::default()
        },
        false,
        |fixture| async move {
            let (model, detail) = fixture.completed().await?;
            assert_eq!(model["status"], "succeeded", "{model}");
            assert_eq!(model["output"], json!([[true, true, true]]));
            assert_eq!(
                model["return_value"],
                json!({"media":"function","file":"function"})
            );
            assert!(fixture.outer_result().await?.attachments.is_empty());
            let calls = detail["scope"]["calls"].as_object().unwrap();
            assert_eq!(
                calls.len(),
                1,
                "ungranted helper effects are never dispatched"
            );
            assert_eq!(calls.values().next().unwrap()["status"], "failed");
            Ok(())
        },
    )
    .await
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
