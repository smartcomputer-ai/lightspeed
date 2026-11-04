//! Live coverage for bringing a session back after its execution stopped: each test
//! terminates the workflow at a different moment, restarts it from the log on the same
//! workflow id, and checks that nothing runs twice and the session serves the next run.
//!
//! ```bash
//! source scripts/dev/env.sh
//! cargo test -p temporal-runtime --test session_restart_live -- --ignored --test-threads=1
//! ```

mod support;

use std::{sync::Arc, time::Duration};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiService, ApprovalDecisionInput, ApprovalDecisionKind,
    RunApprovalsDecideParams, SessionConfig, SessionEventsReadParams, SessionExecutionFailureView,
    SessionLifecycleStatus, SessionReadParams, SessionRestartParams, SessionStartParams,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use harness::{
    ApprovalContinuation, ApprovalSubject, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    CoreAgentIoError, CoreAgentLlm, CoreAgentTools, LlmFinish, LlmGenerationFacts,
    LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, ObservedApprovalRequest,
    SessionId, storage::BlobStore,
};
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities, fake_worker_activities_for_run_control,
    final_assistant_text, live_universe_id, live_workflow_handle, require_storage_live_env,
    run_with_live_worker, start_text_run, terminate_live_session, wait_for_terminal_run,
    wait_until,
};
use temporal_runtime::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, FakeTools, WorkerActivities},
};
use temporalio_client::{Client, WorkflowDescribeOptions, WorkflowTerminateOptions};

async fn started_session(
    client: &Client,
    task_queue: String,
    session_id: &SessionId,
) -> anyhow::Result<GatewayAgentApi> {
    let store = pg_store_from_env().await?;
    let model = support::live::openai_live_model();
    support::live::seed_agent_default(&store, &model).await?;
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .build();
    api.start_session(session_start_params(session_id, Some(&model)))
        .await?;
    Ok(api)
}

fn session_start_params(
    session_id: &SessionId,
    model: Option<&harness::ModelSelection>,
) -> SessionStartParams {
    SessionStartParams {
        access: None,
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: model.map(|model| SessionConfig {
            model: Some(model_to_api(model)),
            ..SessionConfig::default()
        }),
        profile: None,
        delete_after_close_ms: None,
    }
}

/// A read-only workspace derives the VFS read tools, which gives the fake model a
/// function tool to call.
async fn started_session_with_a_tool(
    client: &Client,
    task_queue: String,
    session_id: &SessionId,
) -> anyhow::Result<GatewayAgentApi> {
    let store = pg_store_from_env().await?;
    let model = support::live::openai_live_model();
    support::live::seed_agent_default(&store, &model).await?;
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .build();
    let workspace = api
        .create_vfs_workspace(api::VfsWorkspaceCreateParams::default())
        .await?
        .result
        .workspace;
    let config = SessionConfig {
        model: Some(model_to_api(&model)),
        features: Some(api::FeaturesConfig {
            vfs: Some(api::VfsFeature {
                working_directory: None,
                version: api::CURRENT_FEATURE_VERSION,
                workspaces: vec![api::WorkspaceAttachment {
                    path: "/workspace".to_owned(),
                    workspace_id: Some(workspace.workspace_id),
                    snapshot_ref: None,
                    access: api::WorkspaceAccess::Read,
                }],
                prompts: None,
                skills: None,
            }),
            ..api::FeaturesConfig::default()
        }),
        ..SessionConfig::default()
    };
    api.start_session(SessionStartParams {
        config: Some(config),
        ..session_start_params(session_id, None)
    })
    .await?;
    Ok(api)
}

async fn temporal_run_id(client: &Client, session_id: &SessionId) -> anyhow::Result<String> {
    Ok(live_workflow_handle(client, session_id)?
        .describe(WorkflowDescribeOptions::default())
        .await?
        .run_id()
        .to_owned())
}

async fn terminate(client: &Client, session_id: &SessionId) -> anyhow::Result<()> {
    live_workflow_handle(client, session_id)?
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("restart live test")
                .build(),
        )
        .await?;
    Ok(())
}

async fn restart(api: &GatewayAgentApi, session_id: &SessionId) -> Result<bool, AgentApiError> {
    Ok(api
        .restart_session(SessionRestartParams {
            session_id: session_id.as_str().to_owned(),
        })
        .await?
        .result
        .restarted)
}

/// The failed run's message says what happened, rather than reading as a model error.
async fn assert_failed_by_restart(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<()> {
    let terminal = wait_for_terminal_run(api, session_id, run_id).await?;
    assert_eq!(terminal.status, api::RunStatus::Failed);
    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: None,
        })
        .await?
        .result
        .events;
    let failure = events.iter().find_map(|event| match &event.kind {
        api::SessionEventKindView::RunFailed { kind, message, .. } => Some((kind, message)),
        _ => None,
    });
    let (kind, message) = failure.expect("the run has a failure event");
    assert_eq!(*kind, api::RunFailureKindView::Internal);
    assert!(message.contains("restarted from its log"), "{message:?}");
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_restart_fails_a_run_cut_off_mid_generation() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::from_secs(3), Duration::ZERO, 0).await?;
    run_with_live_worker(
        activities,
        move |client, task_queue, session_id| async move {
            let api = started_session(&client, task_queue, &session_id).await?;
            let first_execution = temporal_run_id(&client, &session_id).await?;
            let run = start_text_run(&api, &session_id, "cut this generation off").await?;
            wait_until(
                "the generation to start",
                Duration::from_secs(15),
                async || Ok(counters.generations_started() >= 1),
            )
            .await?;

            terminate(&client, &session_id).await?;

            // A stopped session reports how, and a new run does not bring it back.
            let dead = api
                .read_session(SessionReadParams {
                    session_id: session_id.as_str().to_owned(),
                    run_limit: None,
                })
                .await?
                .result
                .session;
            assert_eq!(dead.lifecycle_status, SessionLifecycleStatus::Open);
            assert_eq!(
                dead.execution_failure,
                Some(SessionExecutionFailureView::Terminated)
            );
            let refused = start_text_run(&api, &session_id, "refused")
                .await
                .expect_err("a run start does not restart the session");
            let refused = refused
                .downcast::<AgentApiError>()
                .expect("a typed refusal");
            assert_eq!(refused.kind, AgentApiErrorKind::SessionExecutionStopped);

            assert!(restart(&api, &session_id).await?);
            // The workflow id is reused for a new execution.
            assert_ne!(
                temporal_run_id(&client, &session_id).await?,
                first_execution
            );

            assert_failed_by_restart(&api, &session_id, &run.id).await?;
            let generations = counters.generations_started();

            let next = start_text_run(&api, &session_id, "the session still works").await?;
            let next = wait_for_terminal_run(&api, &session_id, &next.id).await?;
            assert_eq!(next.status, api::RunStatus::Completed);
            assert_eq!(
                counters.generations_started(),
                generations + 1,
                "the interrupted generation was not issued again"
            );

            // Nothing is left to do for a running session.
            assert!(!restart(&api, &session_id).await?);
            terminate_live_session(&client, &session_id, "restart live test cleanup").await;
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_restart_answers_a_call_cut_off_mid_tool_and_runs_it_once()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::ZERO, Duration::from_secs(3), 1).await?;
    run_with_live_worker(
        activities,
        move |client, task_queue, session_id| async move {
            let api = started_session_with_a_tool(&client, task_queue, &session_id).await?;
            let run = start_text_run(&api, &session_id, "cut this tool off").await?;
            wait_until(
                "the tool call to start",
                Duration::from_secs(15),
                async || Ok(counters.tool_calls_started() >= 1),
            )
            .await?;

            terminate(&client, &session_id).await?;
            assert!(restart(&api, &session_id).await?);
            assert_failed_by_restart(&api, &session_id, &run.id).await?;

            // Past the old call's own delay, so a repeat would have shown by now.
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert_eq!(
                counters.tool_calls_started(),
                1,
                "a call the log cannot prove finished must not run again"
            );

            let next = start_text_run(&api, &session_id, "the transcript is well formed").await?;
            let next = wait_for_terminal_run(&api, &session_id, &next.id).await?;
            assert_eq!(next.status, api::RunStatus::Completed);
            terminate_live_session(&client, &session_id, "restart live test cleanup").await;
            Ok(())
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_a_queued_run_starts_after_a_restart() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let (activities, counters) =
        fake_worker_activities_for_run_control(Duration::from_secs(3), Duration::ZERO, 0).await?;
    run_with_live_worker(
        activities,
        move |client, task_queue, session_id| async move {
            let api = started_session(&client, task_queue, &session_id).await?;
            let first = start_text_run(&api, &session_id, "cut this off").await?;
            let queued = start_text_run(&api, &session_id, "queued behind it").await?;
            assert_eq!(queued.status, api::RunStatus::Queued);
            wait_until(
                "the generation to start",
                Duration::from_secs(15),
                async || Ok(counters.generations_started() >= 1),
            )
            .await?;

            terminate(&client, &session_id).await?;
            assert!(restart(&api, &session_id).await?);

            assert_failed_by_restart(&api, &session_id, &first.id).await?;
            let queued = wait_for_terminal_run(&api, &session_id, &queued.id).await?;
            assert_eq!(queued.status, api::RunStatus::Completed);
            terminate_live_session(&client, &session_id, "restart live test cleanup").await;
            Ok(())
        },
    )
    .await
}

/// Parks every run on a provider approval, then answers once it is decided.
struct ApprovalScriptedLlm {
    blobs: Arc<dyn BlobStore>,
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

#[async_trait]
impl CoreAgentLlm for ApprovalScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let decided = request.request.context.entries.iter().any(|entry| {
            matches!(entry.kind, ContextEntryKind::McpApprovalResponse { .. })
                && matches!(
                    &entry.source,
                    harness::ContextEntrySource::ApprovalDecision { run_id, .. }
                        if *run_id == request.run_id
                )
        });
        let facts = |finish, approval_requests| LlmGenerationFacts {
            duration_ms: None,
            provider_response_id: Some(format!(
                "scripted-{}-{}",
                request.run_id.as_u64(),
                request.turn_id.as_u64()
            )),
            finish,
            usage: None,
            tool_calls: Vec::new(),
            approval_requests,
            context_token_estimate: None,
        };
        if decided {
            let output = self
                .blobs
                .put_bytes(b"decided after the restart".to_vec())
                .await
                .map_err(io_error)?;
            return Ok(LlmGenerationResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                status: LlmGenerationStatus::Succeeded,
                failure_ref: None,
                context_entries: vec![ContextEntryInput {
                    kind: ContextEntryKind::Message {
                        role: ContextMessageRole::Assistant,
                    },
                    content: harness::ContentRef::text(output),
                    preview: None,
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                }],
                facts: facts(LlmFinish::Stop, Vec::new()),
            });
        }
        let provider_request_id = format!("mcpr_{}", request.run_id.as_u64());
        let arguments = format!("{{\"run\":{}}}", request.run_id.as_u64());
        let arguments_ref = self
            .blobs
            .put_bytes(arguments.clone().into_bytes())
            .await
            .map_err(io_error)?;
        let opaque_ref = self
            .blobs
            .put_bytes(
                serde_json::to_vec(&serde_json::json!({
                    "id": provider_request_id.clone(),
                    "type": "mcp_approval_request",
                    "server_label": "approval-test",
                    "name": "send",
                    "arguments": arguments,
                }))
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
                kind: ContextEntryKind::ProviderOpaque,
                content: harness::ContentRef {
                    content_ref: opaque_ref,
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some(
                        llm_clients::content::OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND
                            .to_owned(),
                    ),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: facts(
                LlmFinish::Stop,
                vec![ObservedApprovalRequest {
                    subject: ApprovalSubject::McpToolCall {
                        server_id: "approval-test".to_owned(),
                        server_label: "approval-test".to_owned(),
                        tool_name: "send".to_owned(),
                        arguments_ref,
                    },
                    continuation: ApprovalContinuation::ProviderMcp {
                        provider_request_id,
                    },
                }],
            ),
        })
    }
}

async fn wait_for_parked_approval(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<String> {
    let mut approval_id = None;
    wait_until("the run to park", Duration::from_secs(20), async || {
        let session = api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: None,
            })
            .await?
            .result
            .session;
        approval_id = session
            .runs
            .iter()
            .find(|run| run.id == run_id && run.status == api::RunStatus::Parked)
            .and_then(|run| run.pending_approvals.first())
            .map(|approval| approval.approval_id.clone());
        Ok(approval_id.is_some())
    })
    .await?;
    Ok(approval_id.expect("found above"))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_a_parked_run_stays_parked_across_a_restart() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(ApprovalScriptedLlm {
        blobs: blobs.clone(),
    }) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::for_universe(
        live_universe_id()?,
        ActivityState::from_pg_store(store, llm, tools),
    );
    run_with_live_worker(activities, |client, task_queue, session_id| async move {
        let api = started_session(&client, task_queue, &session_id).await?;
        let run = start_text_run(&api, &session_id, "ask first").await?;
        let approval_id = wait_for_parked_approval(&api, &session_id, &run.id).await?;

        terminate(&client, &session_id).await?;
        assert!(restart(&api, &session_id).await?);

        // Same approval, still waiting, and deciding it finishes the same run.
        assert_eq!(
            wait_for_parked_approval(&api, &session_id, &run.id).await?,
            approval_id
        );
        api.decide_run_approvals(RunApprovalsDecideParams {
            session_id: session_id.as_str().to_owned(),
            run_id: run.id.clone(),
            decisions: vec![ApprovalDecisionInput {
                approval_id,
                decision: ApprovalDecisionKind::Approve,
                note: None,
            }],
        })
        .await?;
        let terminal = wait_for_terminal_run(&api, &session_id, &run.id).await?;
        assert_eq!(terminal.status, api::RunStatus::Completed);
        assert_eq!(
            final_assistant_text(&terminal),
            Some("decided after the restart")
        );
        terminate_live_session(&client, &session_id, "restart live test cleanup").await;
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_a_retried_start_restarts_and_the_declared_path_never_does()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, |client, task_queue, session_id| async move {
        let api = started_session(&client, task_queue, &session_id).await?;

        // A session with no run history may never have finished setup.
        terminate(&client, &session_id).await?;
        let refused = restart(&api, &session_id)
            .await
            .expect_err("no run history");
        assert_eq!(refused.kind, AgentApiErrorKind::Rejected, "{refused}");
        assert!(refused.message.contains("no run history"), "{refused}");
        api.close_session(api::SessionCloseParams {
            session_id: session_id.as_str().to_owned(),
            force: true,
        })
        .await?;

        // A public start retried with its own id brings the session back.
        let retried = SessionId::new(format!("{session_id}-retried"));
        api.start_session(session_start_params(&retried, None))
            .await?;
        let run = start_text_run(&api, &retried, "finish once").await?;
        let run = wait_for_terminal_run(&api, &retried, &run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        let first_execution = temporal_run_id(&client, &retried).await?;
        terminate(&client, &retried).await?;
        api.start_session(session_start_params(&retried, None))
            .await?;
        assert_ne!(temporal_run_id(&client, &retried).await?, first_execution);
        let next = start_text_run(&api, &retried, "and again").await?;
        let next = wait_for_terminal_run(&api, &retried, &next.id).await?;
        assert_eq!(next.status, api::RunStatus::Completed);
        terminate_live_session(&client, &retried, "restart live test cleanup").await;

        // The declared path reports the stopped execution and leaves it stopped,
        // though an operator can still bring the session back.
        let declared = SessionId::new(format!("{session_id}-declared"));
        let declaration = || harness::DeclaredSessionWorkflowTools::v1(None, Vec::new());
        api.start_declared_session_for_workflow_with_profile(&declared, false, None, declaration())
            .await?;
        let run = start_text_run(&api, &declared, "finish once").await?;
        wait_for_terminal_run(&api, &declared, &run.id).await?;
        terminate(&client, &declared).await?;
        let stopped = api
            .start_declared_session_for_workflow_with_profile(&declared, false, None, declaration())
            .await
            .expect_err("the declared path must not restart");
        assert_eq!(stopped.kind, AgentApiErrorKind::SessionExecutionStopped);
        assert!(restart(&api, &declared).await?);
        let run = start_text_run(&api, &declared, "after the operator's restart").await?;
        let run = wait_for_terminal_run(&api, &declared, &run.id).await?;
        assert_eq!(run.status, api::RunStatus::Completed);
        terminate_live_session(&client, &declared, "restart live test cleanup").await;
        Ok(())
    })
    .await
}
