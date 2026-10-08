//! A real provider writes JavaScript against the advertised code tool contract.
//! Session admission, code tool effects, the interpreter, and model continuation
//! all use the production runtime over local Temporal and PostgreSQL.

mod support;

use std::{sync::Arc, time::Duration};

use api::AgentApiService;
use api_projection::model_to_api;
use harness::{
    BlobRef, ContextEntryKind, CoreAgentState, PromiseResolution, SessionId, WorkflowToolTarget,
    storage::{BlobStore, ReadSessionEvents, SessionStore},
};
use serde_json::{Value, json};
use support::live::{
    LIVE_TEST_LOCK, final_assistant_text, live_universe_id, openai_live_model,
    require_openai_live_env, require_storage_live_env, run_with_live_worker_builder,
    seed_agent_default, start_text_run, wait_for_terminal_run_with_timeout,
};
use temporal_runtime::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, CodeWorkerActivities, WorkerActivities, code_worker, worker_runtime},
};
use temporal_workflow::{
    CodeExecutionPhase, CodeExecutionWorkflow, CodeToolCallStatus, CodeToolScopeReport,
    ReducedSession, reduce_session_entries_from,
};
use temporalio_client::{Client, WorkflowQueryOptions};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal/PostgreSQL and OPENAI_API_KEY; costs real money; run serially"]
async fn real_model_composes_timer_effects_and_consumes_the_code_result() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    require_openai_live_env()?;
    let store = pg_store_from_env().await?;
    let model = openai_live_model();
    seed_agent_default(&store, &model).await?;
    let code_queue = format!("code-model-live-{}", uuid::Uuid::new_v4().simple());
    let activity_queue = code_queue.clone();
    let activity_store = store.clone();

    run_with_live_worker_builder(
        move |client, _| async move {
            let state = ActivityState::from_pg_store_with_default_runtime(activity_store)?
                .with_workflow_tool_executions(client)
                .with_code_task_queue(activity_queue);
            Ok(WorkerActivities::for_universe(live_universe_id()?, state))
        },
        move |client, session_queue, session_id| async move {
            let api = Arc::new(
                GatewayAgentApi::builder(client.clone(), store.clone())
                    .with_task_queue(session_queue)
                    .with_code_task_queue(code_queue.clone())
                    .build(),
            );
            let runtime = worker_runtime()?;
            let activities =
                CodeWorkerActivities::for_universe(live_universe_id()?, api.clone(), 1)?;
            let mut worker = code_worker(&runtime, client.clone(), code_queue, activities)?;
            let shutdown = worker.shutdown_handle();
            let worker_run = worker.run();
            tokio::pin!(worker_run);
            let body = Box::pin(async {
                let result = run_model_scenario(&api, &client, &store, &session_id, &model).await;
                let _ = api
                    .close_session(api::SessionCloseParams {
                        session_id: session_id.to_string(),
                        force: true,
                    })
                    .await;
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
    )
    .await
}

async fn run_model_scenario(
    api: &GatewayAgentApi,
    client: &Client,
    store: &store_pg::PgStore,
    session_id: &SessionId,
    model: &harness::ModelSelection,
) -> anyhow::Result<()> {
    api.start_session(api::SessionStartParams {
        session_id: Some(session_id.to_string()),
        display_name: None,
        access: None,
        metadata: Default::default(),
        profile: None,
        delete_after_close_ms: None,
        config: Some(api::SessionConfig {
            model: Some(model_to_api(model)),
            generation: Some(api::GenerationConfig {
                max_output_tokens: Some(2048),
                reasoning_effort: Some("low".into()),
                ..Default::default()
            }),
            limits: Some(api::LimitsConfig {
                max_turns: Some(3),
                max_tool_rounds: Some(1),
            }),
            features: Some(api::FeaturesConfig {
                code_mode: Some(api::CodeModeFeature {
                    allowed_tools: Some(vec![
                        "concurrency.sleep".into(),
                        "concurrency.await".into(),
                    ]),
                    ..Default::default()
                }),
                timers: Some(api::TimersFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
    })
    .await?;

    // Describe the task and the currently unrendered return contracts, rather
    // than handing the provider a prewritten script to copy.
    let run = start_text_run(
        api,
        session_id,
        "Use code_execute exactly once. Write JavaScript that creates two timer promises, \
         with delays 10 and 20 milliseconds, by calling tools.sleep concurrently through \
         Promise.all. Then call tools.await once, in all mode with a 5000 ms timeout, \
         to wait for both returned promise handles. sleep returns an object whose promise \
         field is the handle; await returns an object whose results array has a status \
         field on each item. Compute how many statuses equal 'resolved'. Emit only \
         {resolved: count} with text and return the same object. Do not call tools directly \
         outside code_execute. After inspecting the code execution result, reply with \
         exactly CODE_MODE_RESOLVED=<the actual count>. Do not claim a count without \
         executing the code.",
    )
    .await?;
    let run =
        wait_for_terminal_run_with_timeout(api, session_id, &run.id, Duration::from_secs(120))
            .await?;
    anyhow::ensure!(run.status == api::RunStatus::Completed, "{run:#?}");
    assert_eq!(
        final_assistant_text(&run).map(str::trim),
        Some("CODE_MODE_RESOLVED=2")
    );

    let state = read_state(store, session_id).await?;
    let model_calls: Vec<_> = state
        .context
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            ContextEntryKind::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(model_calls, vec![tools::code::CODE_EXECUTE_TOOL_NAME]);
    let invocations: Vec<_> = state
        .workflow_tools
        .start_requests
        .values()
        .filter(|call| call.tool_id.as_str() == tools::code::CODE_EXECUTE_WORKFLOW_TOOL_ID)
        .collect();
    assert_eq!(invocations.len(), 1, "the model must submit one script");
    let invocation = invocations[0];
    let binding = &state.workflow_tools.bindings[&invocation.tool_id];
    let WorkflowToolTarget::Start { start } = &binding.target else {
        anyhow::bail!("code tool did not start a workflow")
    };
    let execution_id =
        harness::workflow_tool_execution_id(&invocation.invocation_id, &start.recipe_fingerprint);
    let snapshot = client
        .get_workflow_handle::<CodeExecutionWorkflow>(execution_id)
        .query(
            CodeExecutionWorkflow::snapshot,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    assert_eq!(snapshot.phase, CodeExecutionPhase::Resolved);
    let Some(PromiseResolution::Resolved {
        payload_ref: Some(reference),
    }) = snapshot.resolution
    else {
        anyhow::bail!("code workflow did not return a report: {snapshot:?}")
    };
    let bytes = store.read_bytes(&reference).await?;
    let result: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["output_available"], true);
    assert_eq!(result["output"], json!([{"resolved": 2}]));
    assert_eq!(result["return_value"], json!({"resolved": 2}));
    assert_eq!(result["calls"], json!({"succeeded": 3}));
    assert!(result["error"].is_null(), "{result}");
    assert!(result["cleanup_error"].is_null(), "{result}");
    assert!(
        bytes.len() < 4096,
        "model report should retain selected output only"
    );
    assert!(result.get("scope").is_none());
    let report_ref = BlobRef::parse(
        result["report_ref"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing report_ref"))?,
    )?;
    let detailed: Value = serde_json::from_slice(&store.read_bytes(&report_ref).await?)?;
    let scope: CodeToolScopeReport = serde_json::from_value(detailed["scope"].clone())?;
    assert!(scope.closed);
    assert_eq!(scope.calls.len(), 3);
    assert!(
        scope
            .calls
            .values()
            .all(|call| call.status == CodeToolCallStatus::Succeeded)
    );
    let durable_scope = &state.code_tools.scopes[&scope.execution_id];
    let mut code_tool_names: Vec<_> = durable_scope
        .calls
        .values()
        .map(|call| call.spec.tool_id.as_str())
        .collect();
    code_tool_names.sort_unstable();
    assert_eq!(
        code_tool_names,
        [
            "concurrency.await",
            "concurrency.sleep",
            "concurrency.sleep"
        ]
    );
    let mut timer_delays = Vec::new();
    for call in durable_scope.calls.values() {
        if call.spec.tool_id.as_str() == "concurrency.sleep" {
            let arguments: tools::concurrency::SleepArgs =
                serde_json::from_slice(&store.read_bytes(&call.spec.arguments_ref).await?)?;
            timer_delays.push(arguments.ms);
        }
    }
    timer_delays.sort_unstable();
    assert_eq!(timer_delays, [10, 20]);
    Ok(())
}

async fn read_state(
    store: &store_pg::PgStore,
    session_id: &SessionId,
) -> anyhow::Result<CoreAgentState> {
    let mut reduced = ReducedSession::default();
    let mut after = None;
    loop {
        let page = store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
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
