//! Live coverage for joined and detached sub-agent execution.

mod support;

use std::{env, future::Future, sync::Arc, time::Duration};

use api::{
    AgentApiService, AgentProfileInput, ContextEntryKindView, InputItem, ProfileCreateParams,
    ProfileDeleteParams, ProfileDocument, ProfileId, ProfileInstructions, ProfileReadParams,
    ProfileSource, RunStartParams, RunStartSource, SessionConfig, SessionEventsReadParams,
    SessionReadParams, SessionStartParams,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use engine::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError, CoreAgentLlm,
    CoreAgentTools, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, ModelSelection, ObservedToolCall, SessionId, ToolCallId, ToolName,
    storage::{BlobStore, SessionStore},
};
use support::live::{
    LIVE_TEST_LOCK, final_assistant_text, live_workflow_handle, require_storage_live_env,
    terminate_live_session, wait_for_terminal_run, wait_until,
};
use temporal_server::{
    default_model_from_env,
    gateway::GatewayAgentApi,
    pg_store_from_env,
    subagents::AgentApiSubagentRuntime,
    worker::{ActivityState, SessionTools, WorkerActivities, core_runtime, worker_with_activities},
};
use temporal_workflow::{
    AgentSessionWorkflow, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal,
};
use temporalio_client::{Client, WorkflowQueryOptions};
use tools::{
    concurrency::AWAIT_TOOL_NAME,
    subagents::{AGENT_RUN_TOOL_NAME, AGENT_SPAWN_TOOL_NAME},
};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_run_returns_child_result_inline() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_scripted_subagent_live_worker(run_agent_run_inline_live_client).await
}

/// A child that reads an image and links it by handle in its answer hands
/// it up: the parent's `agent_run` result is followed by the image as a
/// media entry, the run view attaches it to the call, and the envelope names
/// it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_run_hands_up_media_the_child_linked() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_scripted_subagent_live_worker(run_agent_run_media_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_run_fans_out_three_children() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_scripted_subagent_live_worker(run_agent_run_fan_out_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_run_rejects_over_root_limit() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    run_with_scripted_subagent_live_worker(run_agent_run_limit_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_spawn_await_and_parent_cancel_closes_child() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // Cancel while parked on a spawned child: the await resolves cancelled,
    // the run-scoped promise cascades to the execution, and the execution
    // closes the child session.
    run_with_scripted_subagent_live_worker(run_agent_spawn_cancel_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_run_inherits_parent_environment() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // `environment: { type: "inherit" }` on the child profile activates the
    // parent's provisioned environment in the child and never closes it.
    run_with_scripted_subagent_live_worker(run_agent_run_inherit_environment_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_agent_run_deadline_fails_the_reply_and_closes_the_child()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    // The grant's `deadlineMs` is enforced inside the execution: a child
    // that outlives it is closed and the parent's joined call fails with a
    // `deadline` envelope.
    run_with_scripted_subagent_live_worker(run_agent_run_deadline_live_client).await
}

#[derive(Clone)]
struct SubagentScriptedLlm {
    blobs: Arc<dyn BlobStore>,
}

/// Scripted parent/child model for the sub-agent live scenarios. Parent
/// scripts are user messages: `AGENT_RUN <profile> [<count>]` emits that many
/// `agent_run` calls in one batch; `AGENT_RUN_SLOW <profile>` emits one
/// joined `agent_run` of the slow child; `AGENT_SPAWN_SLOW <profile>` emits one
/// `agent_spawn` and then awaits its promise. Children answer their briefs:
/// `CHILD_TASK <n>` completes at once, `SLOW_CHILD` completes after 12 s.
impl SubagentScriptedLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn latest_text_for_kind(
        &self,
        request: &LlmGenerationRequest,
        matches_kind: impl Fn(&ContextEntryKind) -> bool,
    ) -> Result<Option<String>, CoreAgentIoError> {
        for entry in request.request.context.entries.iter().rev() {
            // Media entries carry bytes, not text; a real adapter lowers
            // them to provider blocks, the script looks past them.
            if matches_kind(&entry.kind) && !engine::media::is_media_content(&entry.content) {
                return self
                    .blobs
                    .read_text(&entry.content.content_ref)
                    .await
                    .map(Some)
                    .map_err(io_error);
            }
        }
        Ok(None)
    }

    async fn tool_calls_result(
        &self,
        request: &LlmGenerationRequest,
        calls: Vec<(&str, serde_json::Value)>,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mut context_entries = Vec::with_capacity(calls.len());
        let mut tool_calls = Vec::with_capacity(calls.len());
        for (index, (tool_name, arguments)) in calls.into_iter().enumerate() {
            if test_support::scripted_tool_id(request, tool_name).is_none() {
                return Err(CoreAgentIoError::Failed {
                    message: format!("scripted subagent test expected {tool_name} to be available"),
                });
            }
            let arguments_ref = self
                .blobs
                .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
                .await
                .map_err(io_error)?;
            let call_id = ToolCallId::new(format!(
                "{tool_name}_call_{}_{index}",
                request.turn_id.as_u64()
            ));
            let tool_name = ToolName::new(tool_name);
            context_entries.push(ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content: engine::ContentRef {
                    content_ref: arguments_ref.clone(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("subagent-script".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            });
            tool_calls.push(ObservedToolCall {
                call_id,
                tool_id: test_support::scripted_tool_id(request, tool_name.as_str()),
                tool_name,
                provider_kind: Some("subagent-script".to_owned()),
                arguments_ref,
                native_call_ref: None,
            });
        }
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries,
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("subagent-tools-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls,
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }

    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
        text: String,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let output_ref = self
            .blobs
            .put_bytes(
                serde_json::to_vec(&serde_json::json!([{ "type": "text", "text": text }]))
                    .map_err(io_error)?,
            )
            .await
            .map_err(io_error)?;
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                content: engine::ContentRef {
                    content_ref: output_ref,
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some(
                        engine::ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND.to_owned(),
                    ),
                },
                preview: Some("subagent scripted final".to_owned()),
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("subagent-final-{}", request.turn_id.as_u64())),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

fn parse_agent_run_script(text: &str) -> Option<(&str, usize)> {
    let mut parts = text.split_whitespace();
    if parts.next()? != "AGENT_RUN" {
        return None;
    }
    let profile = parts.next()?;
    let count = parts
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or(1);
    Some((profile, count))
}

fn parse_reply_promise(tool_result: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(tool_result).ok()?;
    // A reply-keyed acknowledgement shows the model one `promise` handle.
    value["promise"].as_str().map(ToOwned::to_owned)
}

#[async_trait]
impl CoreAgentLlm for SubagentScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let user_text = self
            .latest_text_for_kind(&request, |kind| {
                matches!(
                    kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::User
                    }
                )
            })
            .await?
            .unwrap_or_default();

        if let Some(tool_result) = self
            .latest_text_for_kind(&request, |kind| {
                matches!(kind, ContextEntryKind::ToolResult { .. })
            })
            .await?
        {
            if user_text.starts_with("AGENT_SPAWN_SLOW")
                && let Some(promise_id) = parse_reply_promise(&tool_result)
            {
                return self
                    .tool_calls_result(
                        &request,
                        vec![(
                            AWAIT_TOOL_NAME,
                            serde_json::json!({
                                "promises": [promise_id],
                                "mode": "all",
                                "timeout_ms": 60_000
                            }),
                        )],
                    )
                    .await;
            }
            if user_text.starts_with("CHILD_MEDIA") {
                // The child links the image it read by the handle the tool
                // result announced, plus one handle it never saw.
                let handles = engine::media::find_media_handles(&tool_result);
                let Some(handle) = handles.first() else {
                    return Err(CoreAgentIoError::Failed {
                        message: format!(
                            "child read_file announced no media handle: {tool_result}"
                        ),
                    });
                };
                return self
                    .final_result(
                        &request,
                        format!("Here is the render: ![render]({handle}) and not ours ![x](media:000000000000)"),
                    )
                    .await;
            }
            // Every tool result the parent sees ends its run, so the test
            // can read the sub-agent envelopes straight from the final text.
            let results = request
                .request
                .context
                .entries
                .iter()
                .filter(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }))
                .map(|entry| entry.content.content_ref.clone())
                .collect::<Vec<_>>();
            let mut texts = Vec::with_capacity(results.len());
            for content_ref in results {
                texts.push(self.blobs.read_text(&content_ref).await.map_err(io_error)?);
            }
            // Media a child handed up follows the result as media entries.
            let media = request
                .request
                .context
                .entries
                .iter()
                .filter(|entry| {
                    matches!(entry.source, engine::ContextEntrySource::Tool { .. })
                        && engine::media::is_media_content(&entry.content)
                })
                .map(|entry| engine::media::media_handle(&entry.content.content_ref))
                .collect::<Vec<_>>();
            let media_report = if media.is_empty() {
                String::new()
            } else {
                format!(" parent media: {}", media.join(" "))
            };
            return self
                .final_result(
                    &request,
                    format!("parent done: {}{media_report}", texts.join(" | ")),
                )
                .await;
        }
        if let Some(profile) = user_text.strip_prefix("AGENT_RUN_MEDIA ") {
            return self
                .tool_calls_result(
                    &request,
                    vec![(
                        AGENT_RUN_TOOL_NAME,
                        serde_json::json!({
                            "agent": profile.trim(),
                            "input": "CHILD_MEDIA",
                            "label": "media child"
                        }),
                    )],
                )
                .await;
        }
        if user_text.starts_with("CHILD_MEDIA") {
            return self
                .tool_calls_result(
                    &request,
                    vec![(
                        "vfs_read_file",
                        serde_json::json!({"path": "/workspace/render.png", "offset": null, "limit": null}),
                    )],
                )
                .await;
        }

        if let Some((profile, count)) = parse_agent_run_script(&user_text) {
            let calls = (0..count)
                .map(|index| {
                    (
                        AGENT_RUN_TOOL_NAME,
                        serde_json::json!({
                            "agent": profile,
                            "input": format!("CHILD_TASK {index}"),
                            "label": format!("child {index}")
                        }),
                    )
                })
                .collect();
            return self.tool_calls_result(&request, calls).await;
        }
        if let Some(profile) = user_text.strip_prefix("AGENT_RUN_SLOW ") {
            return self
                .tool_calls_result(
                    &request,
                    vec![(
                        AGENT_RUN_TOOL_NAME,
                        serde_json::json!({
                            "agent": profile.trim(),
                            "input": "SLOW_CHILD",
                            "label": "slow child"
                        }),
                    )],
                )
                .await;
        }
        if let Some(profile) = user_text.strip_prefix("AGENT_SPAWN_SLOW ") {
            return self
                .tool_calls_result(
                    &request,
                    vec![(
                        AGENT_SPAWN_TOOL_NAME,
                        serde_json::json!({
                            "agent": profile.trim(),
                            "input": "SLOW_CHILD",
                            "label": "slow child"
                        }),
                    )],
                )
                .await;
        }
        if let Some(index) = user_text.strip_prefix("CHILD_TASK ") {
            return self
                .final_result(&request, format!("child completed {}", index.trim()))
                .await;
        }
        if user_text.starts_with("SLOW_CHILD") {
            tokio::time::sleep(Duration::from_secs(12)).await;
            return self
                .final_result(&request, "slow child completed".to_owned())
                .await;
        }
        self.final_result(&request, "scripted run completed".to_owned())
            .await
    }
}

async fn run_with_scripted_subagent_live_worker<F, Fut>(run_client: F) -> anyhow::Result<()>
where
    F: FnOnce(
        Client,
        SessionId,
        Arc<GatewayAgentApi>,
        Arc<dyn BlobStore>,
        Arc<dyn SessionStore>,
        ModelSelection,
    ) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let task_queue = format!("lightspeed-agent-live-{}", uuid::Uuid::new_v4().simple());
    let session_id = SessionId::new(format!("session_live_{}", uuid::Uuid::new_v4().simple()));
    let temporal_target =
        env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace =
        env::var("TEMPORAL_NAMESPACE").unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());

    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store.clone())
            .with_task_queue(task_queue.clone())
            .with_default_model(model.clone())
            .build(),
    );

    let blobs_for_worker: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(SubagentScriptedLlm::new(blobs_for_worker.clone())) as Arc<dyn CoreAgentLlm>;
    let hosted = Arc::new(SessionTools::from_pg_store(store.clone()));
    let tools = hosted.clone() as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        store.config().universe_id,
        ActivityState::from_pg_store(store.clone(), llm, tools)
            .with_hosted_tools(hosted)
            .with_workflow_tool_executions(client.clone())
            .with_subagent_runtime(Arc::new(AgentApiSubagentRuntime::new(api.clone()))),
    );
    let mut worker =
        worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
    let shutdown_worker = worker.shutdown_handle();
    let worker_future = worker.run();
    tokio::pin!(worker_future);

    let blobs_for_client: Arc<dyn BlobStore> = store.clone();
    let sessions_for_client: Arc<dyn SessionStore> = store;
    let client_future = run_client(
        client.clone(),
        session_id,
        api,
        blobs_for_client,
        sessions_for_client,
        model,
    );
    tokio::pin!(client_future);

    let client_result = tokio::select! {
        worker_result = worker_future.as_mut() => {
            return match worker_result {
                Ok(()) => Err(anyhow::anyhow!("Temporal worker stopped before the live sub-agent test completed")),
                Err(error) => Err(error.context("Temporal worker failed")),
            };
        }
        client_result = client_future.as_mut() => client_result,
    };

    shutdown_worker();
    // A cancelled slow child may still be inside its scripted 12 s LLM
    // sleep; the worker drains that activity before it stops.
    tokio::time::timeout(Duration::from_secs(30), worker_future.as_mut())
        .await
        .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 30 seconds"))??;
    client_result
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

/// `features.subagents` granting exactly one agent profile.
fn subagents_features(profile_id: &ProfileId, max_descendants: u32) -> api::FeaturesConfig {
    subagents_features_with_deadline(profile_id, max_descendants, 120_000)
}

fn subagents_features_with_deadline(
    profile_id: &ProfileId,
    max_descendants: u32,
    deadline_ms: u64,
) -> api::FeaturesConfig {
    api::FeaturesConfig {
        subagents: Some(api::SubagentsFeature {
            version: api::CURRENT_FEATURE_VERSION,
            agents: vec![api::SubagentAgentRef {
                profile_id: profile_id.clone(),
            }],
            max_depth: 2,
            max_descendants,
            max_concurrent: 4,
            deadline_ms,
        }),
        ..api::FeaturesConfig::default()
    }
}

/// A child profile for the scripted worker: no tools, scripted instructions.
async fn create_child_profile(api: &GatewayAgentApi) -> anyhow::Result<ProfileId> {
    create_child_profile_with_config(api, SessionConfig::default()).await
}

async fn create_child_profile_with_config(
    api: &GatewayAgentApi,
    config: SessionConfig,
) -> anyhow::Result<ProfileId> {
    let profile_id = ProfileId::new(format!(
        "live_subagent_child_{}",
        uuid::Uuid::new_v4().simple()
    ));
    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: Some("Live child".to_owned()),
            description: Some("Answers CHILD_TASK briefs".to_owned()),
            document: ProfileDocument {
                metadata: Default::default(),
                config: Some(config),
                instructions: Some(ProfileInstructions::Text {
                    text: "You are a scripted live sub-agent.".to_owned(),
                }),
                environment: None,
                retention: None,
            },
        },
    })
    .await?;
    Ok(profile_id)
}

async fn start_subagent_parent(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    model: &ModelSelection,
    profile_id: &ProfileId,
    max_descendants: u32,
    script: &str,
) -> anyhow::Result<String> {
    start_subagent_parent_with_features(
        api,
        session_id,
        model,
        subagents_features(profile_id, max_descendants),
        script,
    )
    .await
}

async fn start_subagent_parent_with_features(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    model: &ModelSelection,
    features: api::FeaturesConfig,
    script: &str,
) -> anyhow::Result<String> {
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(model)),
            features: Some(features),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;
    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: script.to_owned(),
                }],
            },
            config: None,
        })
        .await?;
    Ok(run.result.run.id)
}

async fn list_children(
    sessions: &Arc<dyn SessionStore>,
    parent: &SessionId,
) -> anyhow::Result<Vec<engine::storage::SessionRecord>> {
    Ok(sessions
        .list_sessions(engine::storage::ListSessions {
            metadata: Default::default(),
            cursor: None,
            limit: 50,
            root_session_id: None,
            parent_session_id: Some(parent.clone()),
            exclude_closed: false,
        })
        .await?
        .sessions)
}

async fn wait_for_children_closed(
    sessions: &Arc<dyn SessionStore>,
    parent: &SessionId,
    expected: usize,
) -> anyhow::Result<Vec<engine::storage::SessionRecord>> {
    let mut children = Vec::new();
    wait_until(
        "sub-agent children to close",
        Duration::from_secs(60),
        async || {
            children = list_children(sessions, parent).await?;
            Ok(children.len() == expected
                && children.iter().all(|child| {
                    child.lifecycle_status == engine::storage::SessionLifecycleStatus::Closed
                }))
        },
    )
    .await?;
    Ok(children)
}

async fn cleanup_subagent_test(
    client: &Client,
    api: &GatewayAgentApi,
    profile_id: ProfileId,
    sessions: &[SessionId],
) {
    let _ = api.delete_profile(ProfileDeleteParams { profile_id }).await;
    for id in sessions {
        terminate_live_session(client, id, "subagent live test cleanup").await;
    }
}

async fn run_agent_run_media_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    blobs: Arc<dyn BlobStore>,
    _sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    // A workspace holding one PNG, linked read-only into the child's profile.
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(b"live-subagent-render");
    let handle = engine::media::media_handle(&engine::BlobRef::from_bytes(&png));
    let snapshot = vfs::create_inline_snapshot(
        blobs.as_ref(),
        None,
        vfs::CreateInlineSnapshotRequest::new(vec![vfs::InlineFile::new("/render.png", png)?]),
    )
    .await?;
    let workspace = api
        .create_vfs_workspace(api::VfsWorkspaceCreateParams {
            snapshot_ref: Some(snapshot.snapshot_ref.to_string()),
            ..Default::default()
        })
        .await?
        .result
        .workspace;
    let child_config: SessionConfig = serde_json::from_value(serde_json::json!({
        "features": {"vfs": {
            "tools": "readOnly",
            "workspaceLinks": [{
                "path": "/workspace",
                "target": {"type": "workspace", "workspaceId": workspace.workspace_id},
                "access": "readOnly"
            }]
        }}
    }))?;
    let profile_id = create_child_profile_with_config(api.as_ref(), child_config).await?;
    let run_id = start_subagent_parent(
        api.as_ref(),
        &session_id,
        &model,
        &profile_id,
        4,
        &format!("AGENT_RUN_MEDIA {profile_id}"),
    )
    .await?;

    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run_id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Completed);
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert!(
        parent_output.contains(&format!("\"handle\":\"{handle}\"")),
        "the envelope must name the handed-up image: {parent_output}"
    );
    assert!(
        parent_output.contains("\"name\":\"render.png\""),
        "the envelope must carry the file name: {parent_output}"
    );
    assert!(
        !parent_output.contains("\"handle\":\"media:000000000000\""),
        "a handle the child never saw must not be handed up: {parent_output}"
    );
    assert!(
        parent_output.ends_with(&format!(" parent media: {handle}")),
        "the parent must see exactly the handed-up image as a media entry: {parent_output}"
    );
    let agent_call = parent_run
        .tool_batches
        .iter()
        .flat_map(|batch| &batch.calls)
        .find(|call| call.tool_name == AGENT_RUN_TOOL_NAME)
        .expect("agent_run call");
    assert_eq!(agent_call.status, api::ToolItemStatus::Succeeded);
    assert_eq!(
        agent_call
            .media
            .iter()
            .map(|item| (item.handle.clone(), item.mime.clone(), item.name.clone()))
            .collect::<Vec<_>>(),
        vec![(
            handle.clone(),
            "image/png".to_owned(),
            Some("render.png".to_owned())
        )]
    );
    let parent_view = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?
        .result
        .session;
    let handles = parent_view
        .active_context
        .entries
        .iter()
        .filter_map(|entry| entry.content.media_handle.clone())
        .collect::<Vec<_>>();
    assert_eq!(handles, vec![handle]);

    api.delete_profile(ProfileDeleteParams { profile_id })
        .await?;
    terminate_live_session(&client, &session_id, "sub-agent media live cleanup").await;
    Ok(())
}

async fn run_agent_run_inline_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    _blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    let profile_id = create_child_profile(api.as_ref()).await?;
    let profile = api
        .read_profile(ProfileReadParams {
            profile_id: profile_id.clone(),
        })
        .await?
        .result
        .profile;
    let run_id = start_subagent_parent(
        api.as_ref(),
        &session_id,
        &model,
        &profile_id,
        16,
        &format!("AGENT_RUN {profile_id}"),
    )
    .await?;

    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run_id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Completed);
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert!(
        parent_output.contains("\"status\":\"completed\"")
            && parent_output.contains("child completed 0"),
        "expected the child's result envelope inline, got: {parent_output}"
    );
    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: Some(0),
        })
        .await?
        .result
        .events;
    let output = events
        .iter()
        .find_map(|event| match &event.kind {
            api::SessionEventKindView::RunCompleted {
                run_id: completed_run_id,
                output,
            } if completed_run_id == &run_id => output.clone(),
            _ => None,
        })
        .expect("completed run must retain its native output descriptor");
    assert_eq!(output.media_type.as_deref(), Some("application/json"));
    assert_eq!(
        output.provider_kind.as_deref(),
        Some(engine::ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND)
    );
    assert_eq!(parent_run.output.as_ref(), Some(&output));
    assert_eq!(parent_run.output_text.as_deref(), Some(parent_output));
    let agent_calls = parent_run
        .tool_batches
        .iter()
        .flat_map(|batch| &batch.calls)
        .filter(|call| call.tool_name == AGENT_RUN_TOOL_NAME)
        .collect::<Vec<_>>();
    assert_eq!(agent_calls.len(), 1, "expected one joined agent_run call");
    assert_eq!(agent_calls[0].tool_id.as_deref(), Some("subagent.run"));
    assert_eq!(agent_calls[0].status, api::ToolItemStatus::Succeeded);

    let children = wait_for_children_closed(&sessions, &session_id, 1).await?;
    let child = &children[0];
    let origin = child.origin.as_ref().expect("child origin");
    assert_eq!(origin.parent_session_id, session_id);
    assert_eq!(origin.root_session_id, session_id);
    assert_eq!(origin.depth, 1);
    assert_eq!(origin.profile_id, profile_id.as_str());
    assert_eq!(origin.profile_revision, profile.revision);
    assert_eq!(child.display_name.as_deref(), Some("child 0"));
    assert_eq!(child.source_session_id, None);

    let child_view = api
        .read_session(SessionReadParams {
            session_id: child.session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?
        .result
        .session;
    let child_origin = child_view.origin.expect("child origin view");
    assert_eq!(child_origin.parent_session_id, session_id.as_str());
    assert_eq!(child_origin.parent_run_id, run_id);
    assert_eq!(child_origin.agent.profile_id, profile_id);
    assert_eq!(child_view.status, api::SessionStatus::Closed);
    let listed = api
        .list_sessions(api::SessionListParams {
            metadata: Default::default(),
            cursor: None,
            limit: None,
            root_session_id: Some(session_id.as_str().to_owned()),
            parent_session_id: None,
            exclude_closed: false,
        })
        .await?;
    assert_eq!(listed.result.sessions.len(), 1);
    assert!(listed.result.sessions[0].origin.is_some());
    let parent_view = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?;
    assert!(parent_view.result.session.origin.is_none());
    // The grant publishes the sub-agent catalog as a context entry; the model
    // reads the menu from there, not from the schema.
    assert!(
        parent_view
            .result
            .session
            .active_context
            .entries
            .iter()
            .any(|entry| entry.key.as_deref()
                == Some(tools::catalog::SUBAGENT_CATALOG_CONTEXT_KEY)
                && matches!(entry.kind, ContextEntryKindView::Catalog { .. })),
        "expected a sub-agent catalog context entry on the parent"
    );

    let child_ids = children
        .iter()
        .map(|child| child.session_id.clone())
        .collect::<Vec<_>>();
    let mut all = vec![session_id];
    all.extend(child_ids);
    cleanup_subagent_test(&client, api.as_ref(), profile_id, &all).await;
    Ok(())
}

async fn run_agent_run_fan_out_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    _blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    let profile_id = create_child_profile(api.as_ref()).await?;
    let run_id = start_subagent_parent(
        api.as_ref(),
        &session_id,
        &model,
        &profile_id,
        16,
        &format!("AGENT_RUN {profile_id} 3"),
    )
    .await?;

    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run_id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Completed);
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    for index in 0..3 {
        assert!(
            parent_output.contains(&format!("child completed {index}")),
            "expected child {index} result inline, got: {parent_output}"
        );
    }
    let agent_calls = parent_run
        .tool_batches
        .iter()
        .flat_map(|batch| &batch.calls)
        .filter(|call| call.tool_name == AGENT_RUN_TOOL_NAME)
        .collect::<Vec<_>>();
    assert_eq!(
        agent_calls.len(),
        3,
        "expected three joined agent_run calls"
    );
    assert!(
        agent_calls
            .iter()
            .all(|call| call.status == api::ToolItemStatus::Succeeded)
    );
    let children = wait_for_children_closed(&sessions, &session_id, 3).await?;
    assert!(children.iter().all(|child| {
        child
            .origin
            .as_ref()
            .is_some_and(|origin| origin.depth == 1)
    }));

    let mut all = vec![session_id];
    all.extend(children.iter().map(|child| child.session_id.clone()));
    cleanup_subagent_test(&client, api.as_ref(), profile_id, &all).await;
    Ok(())
}

async fn run_agent_run_limit_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    _blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    let profile_id = create_child_profile(api.as_ref()).await?;
    // maxDescendants = 1: the second of two calls must be refused at the
    // reservation and surface as a failed call, not as a worker error.
    let run_id = start_subagent_parent(
        api.as_ref(),
        &session_id,
        &model,
        &profile_id,
        1,
        &format!("AGENT_RUN {profile_id} 2"),
    )
    .await?;

    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run_id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Completed);
    let agent_calls = parent_run
        .tool_batches
        .iter()
        .flat_map(|batch| &batch.calls)
        .filter(|call| call.tool_name == AGENT_RUN_TOOL_NAME)
        .collect::<Vec<_>>();
    assert_eq!(agent_calls.len(), 2);
    let succeeded = agent_calls
        .iter()
        .filter(|call| call.status == api::ToolItemStatus::Succeeded)
        .count();
    let failed = agent_calls
        .iter()
        .filter(|call| call.status == api::ToolItemStatus::Failed)
        .count();
    assert_eq!((succeeded, failed), (1, 1), "one child, one refusal");
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert!(
        parent_output.contains("maxDescendants"),
        "expected the refusal to name the limit, got: {parent_output}"
    );
    let children = wait_for_children_closed(&sessions, &session_id, 1).await?;

    let mut all = vec![session_id];
    all.extend(children.iter().map(|child| child.session_id.clone()));
    cleanup_subagent_test(&client, api.as_ref(), profile_id, &all).await;
    Ok(())
}

async fn run_agent_run_deadline_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    _blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    let profile_id = create_child_profile(api.as_ref()).await?;
    // The slow child takes 12 s; a 3 s grant deadline must cut it off.
    let run_id = start_subagent_parent_with_features(
        api.as_ref(),
        &session_id,
        &model,
        subagents_features_with_deadline(&profile_id, 16, 3_000),
        &format!("AGENT_RUN_SLOW {profile_id}"),
    )
    .await?;

    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run_id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Completed);
    let agent_calls = parent_run
        .tool_batches
        .iter()
        .flat_map(|batch| &batch.calls)
        .filter(|call| call.tool_name == AGENT_RUN_TOOL_NAME)
        .collect::<Vec<_>>();
    assert_eq!(agent_calls.len(), 1, "expected one joined agent_run call");
    assert_eq!(
        agent_calls[0].status,
        api::ToolItemStatus::Failed,
        "a deadline resolves the joined call as failed"
    );
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert!(
        parent_output.contains("\"status\":\"deadline\"")
            && !parent_output.contains("slow child completed"),
        "expected a deadline envelope without the child's output, got: {parent_output}"
    );

    // The execution closed the child well before its 12 s script finished.
    let children = wait_for_children_closed(&sessions, &session_id, 1).await?;
    assert_eq!(children[0].display_name.as_deref(), Some("slow child"));

    let mut all = vec![session_id];
    all.extend(children.iter().map(|child| child.session_id.clone()));
    cleanup_subagent_test(&client, api.as_ref(), profile_id, &all).await;
    Ok(())
}

async fn run_agent_run_inherit_environment_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    _blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    use std::collections::BTreeMap;

    use environment_protocol::shared::EnvironmentTransport;
    use environments::{
        EnvironmentConnectionSpec, EnvironmentProviderBindingId, EnvironmentProviderBindingStatus,
        EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProviderStore,
        PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };

    let store = pg_store_from_env().await?;
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let provider_id = format!("fake-inherit-{suffix}");
    let binding_id = format!("binding-inherit-{suffix}");
    store
        .put_provider(PutEnvironmentProvider {
            provider_id: EnvironmentProviderId::new(provider_id.clone()),
            display_name: Some("Live fake provider".to_owned()),
            controller_connection: EnvironmentConnectionSpec::new(
                "in-process",
                EnvironmentTransport::Provider {
                    provider_type: "fake".to_owned(),
                },
            ),
            metadata: BTreeMap::new(),
            updated_at_ms: 1,
        })
        .await?;
    store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id: store.config().universe_id,
            binding_id: EnvironmentProviderBindingId::new(binding_id.clone()),
            provider_id: EnvironmentProviderId::new(provider_id.clone()),
            status: EnvironmentProviderBindingStatus::Enabled,
            expected_revision: None,
            metadata: BTreeMap::new(),
            updated_at_ms: 1,
        })
        .await?;
    let environments_feature = api::EnvironmentsFeature {
        tools: Some(api::EnvironmentToolSurface::Edit),
        commands: true,
        working_directory: None,
        prompts: None,
        version: api::CURRENT_FEATURE_VERSION,
        providers: None,
        registration_keys: None,
        selection_tools: false,
        jobs: false,
        skills: None,
    };

    // Child profile: inherits whatever environment its parent has active.
    let child_profile_id = ProfileId::new(format!("live_inherit_child_{suffix}"));
    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: child_profile_id.clone(),
            display_name: Some("Inheriting child".to_owned()),
            description: Some("Answers CHILD_TASK briefs on the parent\'s environment".to_owned()),
            document: ProfileDocument {
                metadata: Default::default(),
                config: Some(SessionConfig {
                    features: Some(api::FeaturesConfig {
                        environments: Some(environments_feature.clone()),
                        ..api::FeaturesConfig::default()
                    }),
                    ..SessionConfig::default()
                }),
                instructions: Some(ProfileInstructions::Text {
                    text: "You are a scripted live sub-agent.".to_owned(),
                }),
                environment: Some(api::ProfileEnvironment::Inherit {}),
                retention: None,
            },
        },
    })
    .await?;

    let independent_environment = api
        .create_environment(api::EnvironmentCreateParams {
            request_id: format!("inherit-env-{suffix}"),
            binding_id: binding_id.clone(),
            template_id: "rust-v1".into(),
            display_name: None,
            metadata: BTreeMap::new(),
            idle_policy: None,
        })
        .await?
        .result
        .environment;
    // Parent selects an independently created environment and may run the child.
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: None,
        environment: None,
        delete_after_close_ms: None,
        profile: Some(ProfileSource::Inline {
            profile: Box::new(api::InlineAgentProfile {
                display_name: None,
                description: None,
                document: ProfileDocument {
                    metadata: Default::default(),
                    config: Some(SessionConfig {
                        model: Some(model_to_api(&model)),
                        features: Some(api::FeaturesConfig {
                            environments: Some(environments_feature),
                            ..subagents_features(&child_profile_id, 16)
                        }),
                        ..SessionConfig::default()
                    }),
                    instructions: None,
                    environment: Some(api::ProfileEnvironment::Existing {
                        environment_id: independent_environment.environment_id.clone(),
                    }),
                    retention: None,
                },
            }),
        }),
    })
    .await?;
    let parent_environment = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?
        .result
        .session
        .active_environment_id
        .expect("parent should have a provisioned environment");

    let run = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: format!("AGENT_RUN {child_profile_id}"),
                }],
            },
            config: None,
        })
        .await?;
    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run.result.run.id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Completed);
    let parent_output = final_assistant_text(&parent_run).expect("parent assistant output");
    assert!(
        parent_output.contains("child completed 0"),
        "expected the child result inline, got: {parent_output}"
    );

    let children = wait_for_children_closed(&sessions, &session_id, 1).await?;
    // The closed child no longer projects an active environment; its log
    // records the activation of exactly the parent's environment.
    let child_events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: children[0].session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: Some(0),
        })
        .await?
        .result
        .events;
    let inherited = child_events.iter().any(|event| {
        serde_json::to_string(&event.kind).is_ok_and(|json| {
            json.contains("activeEnvironmentChanged") && json.contains(parent_environment.as_str())
        })
    });
    assert!(
        inherited,
        "child should have activated the parent\'s environment {parent_environment}; events: {}",
        serde_json::to_string(&child_events)?
    );
    // The child never closes an inherited environment.
    let environment = api
        .read_environment(api::EnvironmentReadParams {
            environment_id: parent_environment.clone(),
        })
        .await?
        .result
        .environment;
    assert!(
        !matches!(
            environment.status,
            api::EnvironmentLifecycleStatusView::Closing
                | api::EnvironmentLifecycleStatusView::Closed
        ),
        "inherited environment must stay open after the child closes, got {:?}",
        environment.status
    );

    let mut all = vec![session_id];
    all.extend(children.iter().map(|child| child.session_id.clone()));
    cleanup_subagent_test(&client, api.as_ref(), child_profile_id, &all).await;
    api.close_environment(api::EnvironmentCloseParams {
        environment_id: independent_environment.environment_id,
    })
    .await?;
    Ok(())
}

async fn run_agent_spawn_cancel_live_client(
    client: Client,
    session_id: SessionId,
    api: Arc<GatewayAgentApi>,
    _blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn SessionStore>,
    model: ModelSelection,
) -> anyhow::Result<()> {
    let profile_id = create_child_profile(api.as_ref()).await?;
    let run_id = start_subagent_parent(
        api.as_ref(),
        &session_id,
        &model,
        &profile_id,
        16,
        &format!("AGENT_SPAWN_SLOW {profile_id}"),
    )
    .await?;

    // The parent spawns, then parks on await; the slow child is running.
    let parked = wait_until(
        "the parent to park on await",
        Duration::from_secs(30),
        async || {
            let handle = live_workflow_handle(&client, &session_id)?;
            let status = handle
                .query(
                    AgentSessionWorkflow::status,
                    (),
                    WorkflowQueryOptions::default(),
                )
                .await?;
            Ok(status
                .active_run
                .is_some_and(|run| run.status == engine::RunStatus::Parked))
        },
    )
    .await;
    if let Err(error) = parked {
        let view = api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            })
            .await?
            .result
            .session;
        let runs = serde_json::to_string_pretty(&view.runs)?;
        anyhow::bail!("{error}; parent runs: {runs}");
    }
    let mut children = Vec::new();
    wait_until(
        "the slow child to exist",
        Duration::from_secs(30),
        async || {
            children = list_children(&sessions, &session_id).await?;
            Ok(children.len() == 1)
        },
    )
    .await?;
    let child_id = children[0].session_id.clone();

    api.cancel_run(api::RunCancelParams {
        session_id: session_id.as_str().to_owned(),
        run_id: run_id.clone(),
    })
    .await?;
    let parent_run = wait_for_terminal_run(api.as_ref(), &session_id, &run_id).await?;
    assert_eq!(parent_run.status, api::RunStatus::Cancelled);
    let await_calls = parent_run
        .tool_batches
        .iter()
        .flat_map(|batch| &batch.calls)
        .filter(|call| call.tool_name == AWAIT_TOOL_NAME)
        .collect::<Vec<_>>();
    assert_eq!(await_calls.len(), 1, "expected one parked await call");
    assert_eq!(await_calls[0].tool_id.as_deref(), Some("concurrency.await"));
    assert_eq!(await_calls[0].status, api::ToolItemStatus::Succeeded);
    let await_result: temporal_workflow::MaterializedAwaitResult =
        serde_json::from_str(await_calls[0].output.as_deref().expect("await result"))?;
    assert_eq!(
        await_result.outcome,
        temporal_workflow::AwaitOutcome::Cancelled
    );

    // The cascade: run-scoped promise cancelled -> execution cancelled ->
    // child closed by the execution, well before its 12 s script finishes.
    let children = wait_for_children_closed(&sessions, &session_id, 1).await?;
    assert_eq!(children[0].session_id, child_id);

    cleanup_subagent_test(&client, api.as_ref(), profile_id, &[session_id, child_id]).await;
    Ok(())
}
