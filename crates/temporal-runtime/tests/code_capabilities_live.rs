//! Code-mode integration with native MCP, registered environment jobs, and
//! real sub-agent sessions. Only model generation and the remote MCP service
//! are scripted; all Lightspeed workflows, activities, and JS are production.

mod support;

use std::{future::Future, sync::Arc, time::Duration};

use api::{AgentApiService, DeploymentApiService};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use harness::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError, CoreAgentLlm,
    LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus,
    ObservedToolCall, SessionId, ToolCallId, ToolName,
    storage::{BlobStore, ReadSessionEvents, SessionStore},
};
use serde_json::{Value, json};
use support::live::{
    LIVE_TEST_LOCK, require_storage_live_env, run_with_live_worker_builder, seed_agent_default,
    start_text_run, wait_for_terminal_run,
};
use temporal_runtime::{
    DeploymentStores, GatewayAuthMode, UniverseRuntime,
    environments::gateway::EnvironmentGatewayClientConfig,
    gateway::{
        DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayAgentApi, GatewayDeploymentApi, GatewayRoutes,
        GatewayState, gateway_router, request_context::with_request_context,
    },
    subagents::AgentApiSubagentRuntime,
    worker::{
        ActivityState, CodeWorkerActivities, SessionTools, WorkerActivities, code_worker,
        worker_runtime,
    },
};
use temporal_workflow::{
    CodeToolCallStatus, CodeToolScopeReport, ReducedSession, reduce_session_entries_from,
};
use tokio::sync::Mutex;

struct Tasks(Vec<tokio::task::AbortHandle>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

struct ScriptedLlm(Arc<store_pg::PgStore>);

#[async_trait]
impl CoreAgentLlm for ScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let user = request
            .request
            .context
            .entries
            .iter()
            .rev()
            .find(|entry| {
                matches!(
                    entry.kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::User
                    }
                )
            })
            .ok_or_else(|| io_error("missing scripted user input"))?;
        let input = self
            .0
            .read_text(&user.content.content_ref)
            .await
            .map_err(io_error)?;
        let complete = request
            .request
            .context
            .entries
            .iter()
            .any(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }));
        // Child sessions answer their actual brief. They have no code-mode grant.
        let child = input.starts_with("CHILD:");
        let value = if child {
            input.into_bytes()
        } else if complete {
            b"code capabilities complete".to_vec()
        } else {
            serde_json::to_vec(&json!({"code":input})).map_err(io_error)?
        };
        let reference = self.0.put_bytes(value).await.map_err(io_error)?;
        let (kind, finish, calls) = if child || complete {
            (
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                LlmFinish::Stop,
                vec![],
            )
        } else {
            let tool_id = test_support::scripted_tool_id(&request, "code_execute")
                .ok_or_else(|| io_error("code_execute is not advertised"))?;
            let call_id = ToolCallId::new("code-capabilities-call");
            let name = ToolName::new("code_execute");
            (
                ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                },
                LlmFinish::ToolCalls,
                vec![ObservedToolCall {
                    call_id,
                    tool_id: Some(tool_id),
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
                approval_requests: vec![],
                context_token_estimate: None,
            },
        })
    }
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

struct Fixture {
    api: Arc<GatewayAgentApi>,
    store: Arc<store_pg::PgStore>,
    session: SessionId,
    environment: Option<String>,
    root: std::path::PathBuf,
}

impl Fixture {
    async fn run(
        &self,
        mut features: api::FeaturesConfig,
        source: &str,
    ) -> anyhow::Result<(Value, CodeToolScopeReport, harness::CoreAgentState)> {
        features.code_mode = Some(api::CodeModeFeature {
            timeout_ms: 60_000,
            ..Default::default()
        });
        let profile = self
            .api
            .create_profile(api::ProfileCreateParams {
                profile: api::AgentProfileInput {
                    profile_id: api::ProfileId::new(format!("parent_{}", self.session)),
                    display_name: None,
                    description: None,
                    document: api::ProfileDocument {
                        config: Some(api::SessionConfig {
                            features: Some(features),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                },
            })
            .await?
            .result
            .profile;
        self.api
            .start_session(api::SessionStartParams {
                session_id: Some(self.session.to_string()),
                profile: Some(api::ProfileSource::Named {
                    profile_id: profile.profile_id,
                }),
                ..Default::default()
            })
            .await?;
        let run = start_text_run(&self.api, &self.session, source).await?;
        let completed = wait_for_terminal_run(&self.api, &self.session, &run.id).await?;
        anyhow::ensure!(
            completed.status == api::RunStatus::Completed,
            "{completed:#?}"
        );
        let calls: Vec<_> = completed
            .tool_batches
            .iter()
            .flat_map(|batch| &batch.calls)
            .collect();
        anyhow::ensure!(
            calls.len() == 1 && calls[0].tool_name == "code_execute",
            "expected a single outer code call: {calls:#?}"
        );
        let mut reduced = ReducedSession::default();
        let mut after = None;
        loop {
            let page = self
                .store
                .read_after(ReadSessionEvents {
                    session_id: self.session.clone(),
                    after,
                    limit: 1000,
                })
                .await?;
            reduced = reduce_session_entries_from(reduced, &page.entries)?;
            if page.complete {
                break;
            }
            after = page.next_after;
        }
        let state = reduced.core_state;
        let result = state.context.entries.iter().find(|entry| matches!(&entry.kind, ContextEntryKind::ToolResult { call_id, .. } if call_id.as_str() == "code-capabilities-call"))
            .ok_or_else(|| anyhow::anyhow!("missing outer code result"))?;
        let model: Value =
            serde_json::from_slice(&self.store.read_bytes(&result.content.content_ref).await?)?;
        anyhow::ensure!(model["status"] == "succeeded", "code failed: {model}");
        let report_ref = harness::BlobRef::parse(
            model["report_ref"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing detailed report"))?,
        )?;
        let detail: Value = serde_json::from_slice(&self.store.read_bytes(&report_ref).await?)?;
        let scope: CodeToolScopeReport = serde_json::from_value(detail["scope"].clone())?;
        anyhow::ensure!(
            scope.closed && scope.calls.values().all(|call| call.status.is_terminal()),
            "scope not settled: {scope:?}"
        );
        Ok((model, scope, state))
    }
}

async fn scenario<F, Fut>(environment: bool, check: F) -> anyhow::Result<()>
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    let client = temporal_workflow::connect_temporal(
        &std::env::var("TEMPORAL_ADDRESS")
            .unwrap_or_else(|_| temporal_workflow::DEFAULT_TEMPORAL_TARGET.into()),
        &std::env::var("TEMPORAL_NAMESPACE")
            .unwrap_or_else(|_| temporal_workflow::DEFAULT_TEMPORAL_NAMESPACE.into()),
    )
    .await?;
    let universe = uuid::Uuid::new_v4();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let base_url = format!("http://{address}");
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        format!("code-capabilities-{universe}"),
        Some(base_url.clone()),
        DeploymentStores::from_env().await?,
    )?);
    with_request_context(
        support::live::local_request_context_for(api::AccessScope::Deployment).await?,
        GatewayDeploymentApi::new(runtime.clone()).create_universe(
            api::DeploymentUniverseCreateParams {
                slug: None,
                universe_id: universe.to_string(),
            },
        ),
    )
    .await?;
    let gateway = EnvironmentGatewayClientConfig::new(
        &base_url,
        runtime.environment_gateway().deployment_token(),
    );
    let state = Arc::new(GatewayState::multi(
        GatewayAuthMode::Single {
            universe_id: universe,
        },
        runtime.clone(),
        base_url,
    ));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            gateway_router(state, DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayRoutes::ALL),
        )
        .await
    });
    let mut tasks = Tasks(vec![task.abort_handle()]);
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let context = support::live::local_request_context_for(api::AccessScope::Universe {
        universe_id: universe,
    })
    .await?;
    let result = with_request_context(context, Box::pin(async {
        let state = runtime.state_for(universe, false).await?;
        let store = state.store.clone();
        seed_agent_default(&store, &support::live::openai_live_model()).await?;
        let environment_id = if environment {
            let key = state.api.create_environment_registration_key(api::EnvironmentRegistrationKeyCreateParams {
                display_name:"Code capability test".into(), identity_mode:api::EnvironmentIdentityModeView::Ephemeral,
                max_active_environments:Some(1), ephemeral_disconnect_grace_ms:None, expires_at_ms:None,
            }).await?.result;
            let connect_url = format!("ws://{address}/environment-gateway/connect");
            let mut registration = environment_daemon::config::RegistrationConfig::new(connect_url.clone(), environment_daemon::upgrade::resolve_discovery_url(Some(&connect_url), None)?);
            registration.registration_key = Some(environment_protocol::shared::SecretString::new(key.secret.0));
            let daemon = environment_daemon::DaemonRuntime::new(environment_daemon::config::DaemonConfig {
                listen:None, cwd:root.clone(), fs_root:root.clone(), state_dir:root.join(".envd"), read_only_fs:false, registration:Some(registration), scrubbed_env:vec![],
            })?;
            let task = tokio::spawn(environment_daemon::server::run(daemon));
            tasks.0.push(task.abort_handle());
            Some(tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let records = state.api.list_environments(api::EnvironmentListParams::default()).await?.result.environments;
                    if let Some(record) = records.into_iter().find(|record| record.status == api::EnvironmentLifecycleStatusView::Ready) { return anyhow::Ok(record.environment_id); }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }).await??)
        } else { None };
        let code_queue = format!("code-capabilities-{universe}");
        let worker_code_queue = code_queue.clone();
        let worker_store = store.clone();
        let worker_gateway = gateway.clone();
        run_with_live_worker_builder(move |client, queue| async move {
            let api = Arc::new(GatewayAgentApi::builder(client.clone(), worker_store.clone()).with_task_queue(queue).with_code_task_queue(worker_code_queue.clone()).with_environment_gateway(worker_gateway.clone()).build());
            let hosted = Arc::new(SessionTools::from_pg_store(worker_store.clone()).with_environment_gateway(worker_gateway.clone()));
            let state = ActivityState::from_pg_store(worker_store.clone(), Arc::new(ScriptedLlm(worker_store.clone())), hosted.clone())
                .with_hosted_tools(hosted).with_workflow_tool_executions(client).with_code_task_queue(worker_code_queue)
                .with_subagent_runtime(Arc::new(AgentApiSubagentRuntime::new(api))).with_environment_gateway(worker_gateway)
                .with_native_mcp_from_pg_store(worker_store)?;
            Ok(WorkerActivities::for_universe(universe, state))
        }, move |client, queue, session| async move {
            let api = Arc::new(GatewayAgentApi::builder(client.clone(), store.clone()).with_task_queue(queue).with_code_task_queue(code_queue.clone()).with_environment_gateway(gateway).build());
            let worker_runtime = worker_runtime()?;
            let mut worker = code_worker(&worker_runtime, client, code_queue, CodeWorkerActivities::for_universe(universe, api.clone(), 2)?)?;
            let shutdown = worker.shutdown_handle();
            let worker_run = worker.run();
            tokio::pin!(worker_run);
            let body = Box::pin(async {
                let result = check(Fixture { api:api.clone(), store, session:session.clone(), environment:environment_id, root }).await;
                let _ = api.close_session(api::SessionCloseParams { session_id:session.to_string(), force:true }).await;
                result
            });
            let result = tokio::select! { result = body => result, result = &mut worker_run => Err(anyhow::anyhow!("code worker stopped early: {result:?}")) };
            shutdown();
            let stopped = tokio::time::timeout(Duration::from_secs(15), &mut worker_run).await;
            result?;
            stopped.map_err(|_| anyhow::anyhow!("code worker did not stop"))??;
            Ok(())
        }).await
    })).await;
    drop(tasks);
    let store = runtime.state_for(universe, false).await?.store.clone();
    runtime.evict(universe).await;
    // Only this freshly-created universe is removed. Delete its objects too.
    let objects = store_pg::list_universe_object_keys(store.pool(), universe).await?;
    let mut tx = store.pool().begin().await?;
    for query in [
        "DELETE FROM environments WHERE universe_id=$1",
        "DELETE FROM session_checkpoints WHERE universe_id=$1",
        "DELETE FROM cas_blob_edges WHERE universe_id=$1",
        "DELETE FROM universes WHERE universe_id=$1",
    ] {
        sqlx::query(query).bind(universe).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    let cleanup = store.delete_blob_objects(&objects).await;
    anyhow::ensure!(
        cleanup.failures.is_empty(),
        "object cleanup: {:?}",
        cleanup.failures
    );
    result
}

#[derive(Default)]
struct McpState {
    calls: Vec<Value>,
    lists: usize,
}

async fn mcp_rpc(
    State(state): State<Arc<Mutex<McpState>>>,
    Json(request): Json<Value>,
) -> Response {
    let id = request["id"].clone();
    let result = match request["method"].as_str() {
        Some("initialize") => {
            json!({"protocolVersion":"2025-11-25", "capabilities":{"tools":{}}, "serverInfo":{"name":"code-capabilities", "version":"1"}})
        }
        Some("notifications/initialized") => return StatusCode::ACCEPTED.into_response(),
        Some("tools/list") => {
            state.lock().await.lists += 1;
            json!({"tools":[{
                "name":"echo", "description":"Echo the supplied value", "inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false},
                "outputSchema":{"type":"object","properties":{"echo":{"type":"string"}},"required":["echo"]}, "annotations":{"readOnlyHint":true}
            }]})
        }
        Some("tools/call") => {
            state.lock().await.calls.push(request["params"].clone());
            if request["params"]["name"] != "echo" {
                return StatusCode::BAD_REQUEST.into_response();
            }
            let value = request["params"]["arguments"]["value"]
                .as_str()
                .unwrap_or_default();
            if value == "fail" {
                json!({"content":[{"type":"text","text":"requested failure"}],"isError":true})
            } else {
                json!({"content":[{"type":"text","text":format!("echo:{value}")}], "structuredContent":{"echo":value}, "isError":false})
            }
        }
        _ => return Json(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
        )
        .into_response(),
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal/PostgreSQL/MinIO and MCP private-network policy allowing loopback; run serially"]
async fn code_mode_discovers_and_calls_native_mcp_tools() -> anyhow::Result<()> {
    scenario(false, native_mcp_case).await
}

async fn native_mcp_case(fixture: Fixture) -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(McpState::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let app = Router::new()
        .route(
            "/mcp",
            post(mcp_rpc).delete(|| async { StatusCode::NO_CONTENT }),
        )
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let _tasks = Tasks(vec![server.abort_handle()]);
    for (id, exposure) in [
        ("injected", api::RemoteMcpExposure::Inject),
        ("discovered", api::RemoteMcpExposure::Search),
    ] {
        fixture
            .api
            .put_mcp_server(api::McpServerPutParams {
                server: api::McpServerInput {
                    server_id: id.into(),
                    display_name: None,
                    server_url: url.clone(),
                    default_server_label: id.into(),
                    description: None,
                    allowed_tools: None,
                    execution: api::RemoteMcpExecution::Native,
                    exposure,
                    approval: api::RemoteMcpApprovalPolicy::Never,
                    defer_loading: None,
                    allow_private_network: true,
                    auth_policy: api::McpServerAuthPolicy::None,
                    credential: None,
                    status: api::McpServerStatus::Active,
                },
                expected_revision: None,
            })
            .await?;
    }
    let features: api::FeaturesConfig = serde_json::from_value(
        json!({"mcp":{"servers":[{"serverId":"injected"},{"serverId":"discovered"}]}}),
    )?;
    let (model, scope, _) = fixture.run(features, r#"
            const found = await tools.mcp_find_tools({server:"discovered",query:"echo"});
            if (found.tools.length !== 1) throw new Error("missing discovered tool");
            const named = found.tools[0].name;
            const full = await tools.mcp_find_tools({server:"discovered",names:[named]});
            const definition = full.tools[0];
            if (definition.inputSchema.properties.value.type !== "string" || definition.outputSchema.properties.echo.type !== "string") throw new Error("lost MCP schemas");
            text({definition});
            const [direct, discovered] = await Promise.all([
                tools.mcp_injected__echo({value:"direct"}),
                tools.mcp_call({server:"discovered",tool:named,arguments:{value:"discovered"}})
            ]);
            let failed;
            try { await tools.mcp_call({server:"discovered",tool:named,arguments:{value:"fail"}}); }
            catch (error) { failed = {kind:error.kind, value:error.value}; }
            const recovered = await tools.mcp_call({server:"discovered",tool:named,arguments:{value:direct.structuredContent.echo + "+" + discovered.structuredContent.echo}});
            return {direct:direct.structuredContent.echo, discovered:discovered.structuredContent.echo, recovered:recovered.structuredContent.echo, failed, deferred:typeof tools.mcp_discovered__echo};
        "#).await?;
    let value = &model["return_value"];
    anyhow::ensure!(
        value["direct"] == "direct"
            && value["discovered"] == "discovered"
            && value["recovered"] == "direct+discovered",
        "{model}"
    );
    anyhow::ensure!(
        value["deferred"] == "undefined" && value["failed"]["kind"] == "tool_failed",
        "{model}"
    );
    anyhow::ensure!(
        model["output"][0]["definition"]["outputSchema"]["required"] == json!(["echo"]),
        "definition must reach outer model context: {model}"
    );
    anyhow::ensure!(
        scope.calls.len() == 6
            && scope
                .calls
                .values()
                .filter(|call| call.status == CodeToolCallStatus::Failed)
                .count()
                == 1,
        "{scope:?}"
    );
    let state = state.lock().await;
    anyhow::ensure!(
        state.lists > 0 && state.calls.len() == 4,
        "expected real MCP discovery and exactly four remote calls: {:?}",
        state.calls
    );
    let values: Vec<_> = state
        .calls
        .iter()
        .map(|call| call["arguments"]["value"].as_str().unwrap())
        .collect();
    anyhow::ensure!(
        values.contains(&"direct")
            && values.contains(&"discovered")
            && values.contains(&"fail")
            && values.contains(&"direct+discovered"),
        "{values:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal/PostgreSQL/MinIO; starts a registered environment daemon; run serially"]
async fn code_mode_runs_real_environment_jobs_and_subagents() -> anyhow::Result<()> {
    scenario(true, jobs_and_subagents_case).await
}

async fn jobs_and_subagents_case(fixture: Fixture) -> anyhow::Result<()> {
    let environment = fixture
        .environment
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing daemon"))?;
    let child = fixture
        .api
        .create_profile(api::ProfileCreateParams {
            profile: api::AgentProfileInput {
                profile_id: api::ProfileId::new("code_child"),
                display_name: None,
                description: None,
                document: api::ProfileDocument {
                    config: Some(api::SessionConfig::default()),
                    ..Default::default()
                },
            },
        })
        .await?
        .result
        .profile;
    let features = serde_json::from_value(json!({
        "environments":{"environments":[{"environmentId":environment,"default":true,"access":"jobs","workingDirectory":fixture.root}]},
        "subagents":{"agents":[{"profileId":child.profile_id}],"maxDepth":2,"maxDescendants":4,"maxConcurrent":2,"deadlineMs":60000}
    }))?;
    let (model, scope, state) = fixture.run(features, r#"
            const [job, agent] = await Promise.all([
                tools.job_run({argv:["/bin/sh","-c","printf joined-value > joined.txt; printf joined-value"],timeout_ms:10000}),
                tools.agent_run({agent:"code_child",input:"CHILD:joined",label:"joined child"})
            ]);
            const [submitted, spawned] = await Promise.all([
                tools.job_submit({jobs:[{job_id:"submitted",argv:["/bin/sh","-c","cat joined.txt > submitted.txt; printf submitted-value"],timeout_ms:10000}]}),
                tools.agent_spawn({agent:"code_child",input:"CHILD:spawned",label:"spawned child"})
            ]);
            const promises = [submitted.promises.submitted, spawned.promise];
            const waited = await tools.await({promises,mode:"all",timeout_ms:30000});
            text({job,agent,waited});
            return {job,agent,submitted,spawned,waited};
        "#).await?;
    let result = &model["return_value"];
    anyhow::ensure!(
        result["agent"]["status"] == "completed" && result["agent"]["output"] == "CHILD:joined",
        "{model}"
    );
    let job: tools::environment::jobs::ModelJobResult =
        serde_json::from_value(result["job"].clone())?;
    anyhow::ensure!(
        job.error.is_none()
            && job
                .output
                .iter()
                .any(|part| part.text.as_deref() == Some("joined-value")),
        "{job:?}"
    );
    let waited = result["waited"]["results"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing await results: {model}"))?;
    anyhow::ensure!(
        waited.len() == 2 && waited.iter().all(|value| value["status"] == "resolved"),
        "{model}"
    );
    let job_reply = waited
        .iter()
        .find(|reply| reply["promise_id"] == result["submitted"]["promises"]["submitted"])
        .ok_or_else(|| anyhow::anyhow!("submitted job promise was not resolved: {model}"))?;
    let submitted: tools::environment::jobs::ModelJobResult =
        serde_json::from_value(job_reply["output"].clone())?;
    anyhow::ensure!(
        submitted.error.is_none()
            && submitted
                .output
                .iter()
                .any(|part| part.text.as_deref() == Some("submitted-value")),
        "{submitted:?}"
    );
    for actual in [&job, &submitted] {
        anyhow::ensure!(
            actual
                .handle
                .as_ref()
                .is_some_and(|handle| handle.environment_id
                    == tools::environment::handles::environment_handle(environment))
                && actual.summary.as_ref().is_some_and(|summary| summary.status
                    == environment_protocol::data::jobs::JobStatus::Succeeded
                    && &summary.namespace == environment),
            "job must finish on the admitted environment: {actual:?}"
        );
    }
    let child_reply = waited
        .iter()
        .find(|reply| reply["promise_id"] == result["spawned"]["promise"])
        .ok_or_else(|| anyhow::anyhow!("spawned child promise was not resolved: {model}"))?;
    let spawned: tools::subagents::SubagentResultEnvelope =
        serde_json::from_value(child_reply["output"].clone())?;
    anyhow::ensure!(
        spawned.status == tools::subagents::SubagentResultStatus::Completed
            && spawned.output.as_deref() == Some("CHILD:spawned"),
        "{spawned:?}"
    );
    anyhow::ensure!(
        std::fs::read(fixture.root.join("joined.txt"))? == b"joined-value"
            && std::fs::read(fixture.root.join("submitted.txt"))? == b"joined-value",
        "daemon jobs must perform their dependent filesystem effects"
    );
    anyhow::ensure!(
        scope.calls.len() == 5
            && scope
                .calls
                .values()
                .all(|call| call.status == CodeToolCallStatus::Succeeded),
        "{scope:?}"
    );
    let durable = &state.code_tools.scopes[&scope.execution_id];
    let names: std::collections::BTreeSet<_> = durable
        .calls
        .values()
        .map(|call| call.spec.tool_id.as_str())
        .collect();
    anyhow::ensure!(
        names
            == [
                "env.job_run",
                "env.job_submit",
                "subagent.run",
                "subagent.spawn",
                "concurrency.await"
            ]
            .into_iter()
            .collect(),
        "{names:?}"
    );
    let children = fixture
        .api
        .list_sessions(api::SessionListParams {
            trees: vec![fixture.session.to_string()],
            subagent: Some(true),
            ..Default::default()
        })
        .await?
        .result
        .sessions;
    anyhow::ensure!(
        children.len() == 2,
        "expected real child sessions: {children:?}"
    );
    let child_ids: std::collections::BTreeSet<_> =
        children.iter().map(|child| child.id.as_str()).collect();
    anyhow::ensure!(
        child_ids
            == [
                result["agent"]["session_id"].as_str().unwrap(),
                spawned.session_id.as_str()
            ]
            .into_iter()
            .collect(),
        "replies must name the actual owned child sessions"
    );
    for child in children {
        let view = fixture
            .api
            .read_session(api::SessionReadParams {
                session_id: child.id,
                run_limit: None,
            })
            .await?
            .result
            .session;
        anyhow::ensure!(
            view.status == api::SessionStatus::Closed
                && view
                    .origin
                    .as_ref()
                    .is_some_and(|origin| origin.parent_session_id == fixture.session.as_str()),
            "child must be closed and owned by parent: {view:?}"
        );
        anyhow::ensure!(
            view.runs
                .iter()
                .any(|run| run.status == api::RunStatus::Completed),
            "child did not execute a real run: {view:?}"
        );
    }
    Ok(())
}
