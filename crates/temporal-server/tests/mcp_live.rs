//! End-to-end MCP acceptance coverage over a real Temporal worker and local
//! Stateless Streamable HTTP servers. Provider-hosted MCP has separate live
//! suites in `llm-runtime`; this suite owns Lightspeed-native execution and
//! the universe/session control plane.

mod support;

use std::{collections::BTreeSet, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use api::{
    AgentApiService, ApprovalDecisionInput, ApprovalDecisionKind, ApprovalDecisionStatus,
    FeaturesConfig, InputItem, McpServerDeleteParams, McpServerInput, McpServerLink,
    McpServerListParams, McpServerPutParams, McpServerReadParams, McpServerStatus,
    McpServerToolsDiscoverParams, McpServerToolsDiscoverResponse, RemoteMcpApprovalPolicy,
    RemoteMcpExecution, RemoteMcpExposure, RunApprovalsDecideParams, RunLimitsConfig,
    RunStartConfig, RunStartParams, RunStartSource, SessionConfig, SessionConfigPutParams,
    SessionEventsReadParams, SessionReadParams, SessionStartParams, TimersFeature,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use engine::{
    ApprovalContinuation, ApprovalSubject, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    CoreAgentIoError, CoreAgentLlm, CoreAgentTools, LlmFinish, LlmGenerationFacts,
    LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, ObservedApprovalRequest,
    ObservedToolCall, SessionId, ToolCallId, ToolKind, ToolName, storage::BlobStore,
};
use serde_json::{Value, json};
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities, final_assistant_text, live_universe_id,
    live_workflow_handle, read_run, read_session_view, require_storage_live_env,
    run_with_live_worker, wait_for_terminal_run,
};
use temporal_server::{
    default_model_from_env,
    gateway::{DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayAgentApi, GatewayState, gateway_router},
    pg_store_from_env,
    worker::{ActivityState, FakeTools, SessionTools, WorkerActivities},
};
use temporalio_client::{Client, WorkflowTerminateOptions};
use tokio::sync::Mutex;
use tools::concurrency::{AWAIT_TOOL_NAME, SLEEP_TOOL_NAME};

const LARGE_TOOL_COUNT: usize = 45;
/// The binary assets the `rich` fixture tool returns; the model must see
/// both as media named by their handles.
const RICH_PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nrich-fixture-png";
const RICH_PDF_BYTES: &[u8] = b"%PDF-1.4 rich-fixture-pdf";
const LARGE_PAGE_SIZE: usize = 15;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra and LIGHTSPEED_MCP_PRIVATE_NETWORKS allowing loopback"]
async fn mcp_live_native_configuration_and_server_size_matrix() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let fixture = LiveMcpFixture::start().await?;
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let ids = MatrixServerIds {
        small: format!("small_{suffix}"),
        large: format!("large_{suffix}"),
        selected: format!("selected_{suffix}"),
    };
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(MatrixScriptedLlm {
        blobs: blobs.clone(),
        ids: ids.clone(),
    }) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    let state = ActivityState::from_pg_store(store.clone(), llm, tools)
        .with_native_mcp_from_pg_store(store)?;
    let activities = WorkerActivities::for_universe(live_universe_id()?, state);
    let fixture_for_client = fixture.clone();
    let ids_for_client = ids.clone();

    let result = run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_matrix_client(
            client,
            task_queue,
            session_id,
            fixture_for_client,
            ids_for_client,
        )
    })
    .await;

    result?;
    fixture.assert_calls().await
}

#[derive(Clone)]
struct MatrixServerIds {
    small: String,
    large: String,
    selected: String,
}

#[derive(Clone)]
struct MatrixScriptedLlm {
    blobs: Arc<dyn BlobStore>,
    ids: MatrixServerIds,
}

#[async_trait]
impl CoreAgentLlm for MatrixScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let latest = request
            .request
            .context
            .entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.kind {
                ContextEntryKind::ToolResult { call_id, .. } => Some((
                    call_id.as_str().to_owned(),
                    entry.content.content_ref.clone(),
                )),
                _ => None,
            });
        let Some((call_id, result_ref)) = latest else {
            return self
                .tool_call(
                    &request,
                    &format!("mcp_{}__echo", self.ids.small),
                    "matrix_inject_echo",
                    json!({"value": "hello"}),
                )
                .await;
        };
        let result = self.blobs.read_text(&result_ref).await.map_err(io_error)?;
        match call_id.as_str() {
            "matrix_inject_echo" => {
                require_contains(&result, "small-echo:hello", &call_id)?;
                self.tool_call(
                    &request,
                    &format!("mcp_{}__rich", self.ids.small),
                    "matrix_inject_rich",
                    json!({}),
                )
                .await
            }
            "matrix_inject_rich" => {
                require_contains(&result, "rich-text", &call_id)?;
                // The image and the PDF the tool returned are announced by
                // position and handle, and follow the result as media
                // entries the model can see; the audio clip is dropped with
                // a note.
                let png_handle =
                    engine::media::media_handle(&engine::BlobRef::from_bytes(RICH_PNG_BYTES));
                let pdf_handle =
                    engine::media::media_handle(&engine::BlobRef::from_bytes(RICH_PDF_BYTES));
                require_contains(
                    &result,
                    &format!("[image 1 · {png_handle} · image/png"),
                    &call_id,
                )?;
                require_contains(
                    &result,
                    &format!("[document 1 · {pdf_handle} · application/pdf · brief.pdf"),
                    &call_id,
                )?;
                require_contains(
                    &result,
                    "[audio 1 omitted: audio/wav is not supported]",
                    &call_id,
                )?;
                let media = request
                    .request
                    .context
                    .entries
                    .iter()
                    .filter(|entry| {
                        matches!(entry.source, engine::ContextEntrySource::Tool { .. })
                            && engine::media::is_media_content(&entry.content)
                    })
                    .map(|entry| {
                        (
                            engine::media::media_handle(&entry.content.content_ref),
                            entry.content.media_type.clone().unwrap_or_default(),
                            entry.preview.clone().unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>();
                if media
                    != vec![
                        (png_handle, "image/png".to_owned(), "[image]".to_owned()),
                        (
                            pdf_handle,
                            "application/pdf".to_owned(),
                            "[document: brief.pdf]".to_owned(),
                        ),
                    ]
                {
                    return Err(CoreAgentIoError::Failed {
                        message: format!(
                            "unexpected media entries after the rich result: {media:?}"
                        ),
                    });
                }
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "matrix_browse_1",
                    json!({"server": self.ids.large}),
                )
                .await
            }
            "matrix_browse_1" => {
                let page = find_page(&result, "catalog_tool_000")?;
                let cursor =
                    page["nextCursor"]
                        .as_u64()
                        .ok_or_else(|| CoreAgentIoError::Failed {
                            message: format!("first MCP browse page did not continue: {result}"),
                        })?;
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "matrix_browse_2",
                    json!({"server": self.ids.large, "cursor": cursor}),
                )
                .await
            }
            "matrix_browse_2" => {
                let page = find_page(&result, "catalog_tool_044")?;
                if !page["nextCursor"].is_null() {
                    return Err(CoreAgentIoError::Failed {
                        message: format!("second MCP browse page did not finish: {result}"),
                    });
                }
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "matrix_oversized",
                    json!({"server": self.ids.large, "query": "catalog_tool_044"}),
                )
                .await
            }
            "matrix_oversized" => {
                let page = find_page(&result, "catalog_tool_044")?;
                let tool = page["tools"]
                    .as_array()
                    .and_then(|tools| tools.iter().find(|tool| tool["name"] == "catalog_tool_044"))
                    .ok_or_else(|| CoreAgentIoError::Failed {
                        message: format!("oversized MCP hit is absent: {result}"),
                    })?;
                if tool["truncated"].as_str().is_none()
                    || serde_json::to_vec(tool).map_err(io_error)?.len() > 8 * 1024
                {
                    return Err(CoreAgentIoError::Failed {
                        message: format!("oversized MCP hit was not bounded: {result}"),
                    });
                }
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "matrix_detail",
                    json!({"server": self.ids.large, "names": ["catalog_tool_044"]}),
                )
                .await
            }
            "matrix_detail" => {
                let page = find_page(&result, "catalog_tool_044")?;
                let tool = &page["tools"][0];
                if tool.get("truncated").is_some()
                    || tool["description"].as_str().map(str::len) != Some(16_000)
                    || tool["inputSchema"]["properties"]["value"]["type"] != "string"
                {
                    return Err(CoreAgentIoError::Failed {
                        message: format!("MCP detail did not return the full definition: {result}"),
                    });
                }
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "matrix_filtered",
                    json!({"server": self.ids.selected}),
                )
                .await
            }
            "matrix_filtered" => {
                assert_find_page(&result, 1, None, "catalog_tool_039")?;
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "matrix_selected",
                    json!({"server": self.ids.selected, "query": "value"}),
                )
                .await
            }
            "matrix_selected" => {
                assert_find_page(&result, 1, None, "catalog_tool_039")?;
                require_contains(&result, "inputSchema", &call_id)?;
                self.tool_call(
                    &request,
                    "mcp_call",
                    "matrix_approved_call",
                    json!({
                        "server": self.ids.large,
                        "tool": "catalog_tool_039",
                        "arguments": {"value": "approved"}
                    }),
                )
                .await
            }
            "matrix_approved_call" => {
                require_contains(&result, "large-call:catalog_tool_039:approved", &call_id)?;
                self.final_result(&request, "native MCP matrix complete")
                    .await
            }
            other => Err(CoreAgentIoError::Failed {
                message: format!("unexpected MCP matrix tool result {other}: {result}"),
            }),
        }
    }
}

impl MatrixScriptedLlm {
    async fn tool_call(
        &self,
        request: &LlmGenerationRequest,
        tool: &str,
        call_id: &str,
        arguments: Value,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(call_id);
        let tool_name = ToolName::new(tool);
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content: engine::ContentRef {
                    content_ref: arguments_ref.clone(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("mcp-live-matrix".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("mcp-matrix-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_id: test_support::scripted_tool_id(request, tool_name.as_str()),
                    tool_name,
                    provider_kind: Some("mcp-live-matrix".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }

    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
        text: &str,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let content_ref = self
            .blobs
            .put_bytes(text.as_bytes().to_vec())
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
                    content_ref,
                    media_type: Some("text/plain".to_owned()),
                    provider_kind: Some("mcp-live-matrix".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!(
                    "mcp-matrix-final-{}",
                    request.turn_id.as_u64()
                )),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

async fn run_matrix_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    fixture: LiveMcpFixture,
    ids: MatrixServerIds,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();

    put_fixture_server(
        &api,
        &ids.small,
        &fixture.small_url,
        RemoteMcpExposure::Inject,
        Some(vec!["echo".to_owned(), "rich".to_owned()]),
        RemoteMcpApprovalPolicy::Never,
    )
    .await?;
    put_fixture_server(
        &api,
        &ids.large,
        &fixture.large_url,
        RemoteMcpExposure::Search,
        None,
        RemoteMcpApprovalPolicy::Always,
    )
    .await?;
    put_fixture_server(
        &api,
        &ids.selected,
        &fixture.large_url,
        RemoteMcpExposure::Search,
        Some(vec!["catalog_tool_039".to_owned()]),
        RemoteMcpApprovalPolicy::Never,
    )
    .await?;

    assert_eq!(discover_count(&api, &ids.small).await?, 2);
    assert_eq!(discover_count(&api, &ids.large).await?, LARGE_TOOL_COUNT);
    fixture.set_large_tool_count(LARGE_TOOL_COUNT + 1).await;
    // The public discovery boundary deliberately rate-limits one server id;
    // wait out that admission cooldown before proving the second read is live.
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    assert_eq!(
        discover_count(&api, &ids.large).await?,
        LARGE_TOOL_COUNT + 1,
        "discovery must read the server again instead of a stored inventory"
    );
    fixture.set_large_tool_count(LARGE_TOOL_COUNT).await;

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(FeaturesConfig {
                mcp: Some(api::McpFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    servers: vec![
                        McpServerLink {
                            server_id: ids.small.clone(),
                        },
                        McpServerLink {
                            server_id: ids.large.clone(),
                        },
                        McpServerLink {
                            server_id: ids.selected.clone(),
                        },
                    ],
                }),
                ..FeaturesConfig::default()
            }),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;
    let started = api
        .start_run(RunStartParams {
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "Exercise the native MCP matrix".to_owned(),
                }],
            },
            submission_id: None,
            config: Some(RunStartConfig {
                limits: Some(RunLimitsConfig {
                    max_turns: Some(12),
                    max_tool_rounds: Some(12),
                }),
                ..RunStartConfig::default()
            }),
            notify_on_terminal: None,
        })
        .await?;
    let pending = wait_for_pending_approval(&api, &session_id, &started.result.run.id).await?;
    match &pending.subject {
        api::ApprovalSubjectView::McpToolCall {
            server_id,
            tool_name,
            ..
        } => {
            assert_eq!(server_id, &ids.large);
            assert_eq!(tool_name, "catalog_tool_039");
        }
    }
    api.decide_run_approvals(RunApprovalsDecideParams {
        session_id: session_id.as_str().to_owned(),
        run_id: started.result.run.id.clone(),
        decisions: vec![ApprovalDecisionInput {
            approval_id: pending.approval_id,
            decision: ApprovalDecisionKind::Approve,
            note: Some("MCP matrix approval".to_owned()),
        }],
    })
    .await?;
    let terminal = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert_eq!(
        final_assistant_text(&terminal),
        Some("native MCP matrix complete")
    );
    // The run view attaches the media the rich call handed the model to that
    // call, by handle, and the session's context carries the entries with
    // their handles for clients to resolve `media:` links against.
    let rich_call = terminal
        .tool_batches
        .iter()
        .flat_map(|batch| batch.calls.iter())
        .find(|call| call.call_id == "matrix_inject_rich")
        .expect("rich call in run view");
    let png_handle = engine::media::media_handle(&engine::BlobRef::from_bytes(RICH_PNG_BYTES));
    let pdf_handle = engine::media::media_handle(&engine::BlobRef::from_bytes(RICH_PDF_BYTES));
    assert_eq!(
        rich_call
            .media
            .iter()
            .map(|item| (item.handle.clone(), item.mime.clone(), item.name.clone()))
            .collect::<Vec<_>>(),
        vec![
            (png_handle.clone(), "image/png".to_owned(), None),
            (
                pdf_handle.clone(),
                "application/pdf".to_owned(),
                Some("brief.pdf".to_owned())
            ),
        ]
    );
    let session_view = read_session_view(&api, &session_id).await?;
    let handles = session_view
        .active_context
        .entries
        .iter()
        .filter_map(|entry| entry.content.media_handle.clone())
        .collect::<Vec<_>>();
    assert_eq!(handles, vec![png_handle, pdf_handle]);

    for server_id in [&ids.small, &ids.large, &ids.selected] {
        api.delete_mcp_server(McpServerDeleteParams {
            server_id: server_id.clone(),
        })
        .await?;
    }
    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("MCP live matrix cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn put_fixture_server(
    api: &GatewayAgentApi,
    server_id: &str,
    server_url: &str,
    exposure: RemoteMcpExposure,
    allowed_tools: Option<Vec<String>>,
    approval_default: RemoteMcpApprovalPolicy,
) -> anyhow::Result<()> {
    api.put_mcp_server(McpServerPutParams {
        server: McpServerInput {
            server_id: server_id.to_owned(),
            display_name: Some(format!("MCP fixture {server_id}")),
            server_url: server_url.to_owned(),
            default_server_label: server_id.to_owned(),
            description: Some("Local Streamable HTTP MCP acceptance fixture".to_owned()),
            allowed_tools,
            execution: RemoteMcpExecution::Native,
            exposure,
            approval_default,
            defer_loading_default: None,
            allow_private_network: true,
            auth_policy: api::McpServerAuthPolicy::None,
            credential: None,
            status: McpServerStatus::Active,
        },
        expected_revision: None,
    })
    .await?;
    Ok(())
}

async fn discover_count(api: &GatewayAgentApi, server_id: &str) -> anyhow::Result<usize> {
    let response = api
        .discover_mcp_server_tools(McpServerToolsDiscoverParams {
            server_id: server_id.to_owned(),
        })
        .await?;
    match response.result {
        McpServerToolsDiscoverResponse::Success { tools } => Ok(tools.len()),
        McpServerToolsDiscoverResponse::Failure { code, message, .. } => {
            anyhow::bail!("MCP discovery failed with {code:?}: {message}")
        }
    }
}

async fn wait_for_pending_approval(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<api::PendingApprovalView> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let session = api
                .read_session(SessionReadParams {
                    session_id: session_id.as_str().to_owned(),
                    run_limit: None,
                })
                .await?
                .result
                .session;
            if let Some(run) = session.runs.iter().find(|run| run.id == run_id) {
                if let Some(approval) = run.pending_approvals.first() {
                    return Ok(approval.clone());
                }
                if matches!(
                    run.status,
                    api::RunStatus::Completed | api::RunStatus::Failed | api::RunStatus::Cancelled
                ) {
                    anyhow::bail!(
                        "MCP matrix run became terminal before approval: {:?}",
                        run.status
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for MCP approval"))?
}

fn assert_find_page(
    text: &str,
    expected_len: usize,
    next_cursor: Option<u64>,
    expected_name: &str,
) -> Result<(), CoreAgentIoError> {
    let value = find_page(text, expected_name)?;
    let tools = value["tools"]
        .as_array()
        .ok_or_else(|| CoreAgentIoError::Failed {
            message: format!("MCP find result has no tools array: {text}"),
        })?;
    if tools.len() != expected_len || value["nextCursor"].as_u64() != next_cursor {
        return Err(CoreAgentIoError::Failed {
            message: format!(
                "unexpected MCP find page: len={}, next={:?}, expected name={expected_name:?}, value={text}",
                tools.len(),
                value["nextCursor"].as_u64()
            ),
        });
    }
    Ok(())
}

fn find_page(text: &str, expected_name: &str) -> Result<Value, CoreAgentIoError> {
    if text.len() > 64 * 1024 {
        return Err(CoreAgentIoError::Failed {
            message: format!("MCP find result exceeded 64 KiB: {} bytes", text.len()),
        });
    }
    let value: Value = serde_json::from_str(text).map_err(io_error)?;
    let tools = value["tools"]
        .as_array()
        .ok_or_else(|| CoreAgentIoError::Failed {
            message: format!("MCP find result has no tools array: {text}"),
        })?;
    if !expected_name.is_empty()
        && !tools
            .iter()
            .any(|tool| tool["name"].as_str() == Some(expected_name))
    {
        return Err(CoreAgentIoError::Failed {
            message: format!("MCP find page did not contain {expected_name:?}: {text}"),
        });
    }
    Ok(value)
}

fn require_contains(text: &str, needle: &str, step: &str) -> Result<(), CoreAgentIoError> {
    if text.contains(needle) {
        Ok(())
    } else {
        Err(CoreAgentIoError::Failed {
            message: format!("MCP matrix step {step} expected {needle:?}, got {text}"),
        })
    }
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

#[derive(Clone)]
struct LiveMcpFixture {
    state: Arc<Mutex<FixtureState>>,
    server_task: Arc<FixtureTask>,
    small_url: String,
    large_url: String,
}

struct FixtureTask(tokio::task::JoinHandle<()>);

impl Drop for FixtureTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Default)]
struct FixtureState {
    large_tool_count: usize,
    calls: Vec<String>,
}

impl LiveMcpFixture {
    async fn start() -> anyhow::Result<Self> {
        let state = Arc::new(Mutex::new(FixtureState {
            large_tool_count: LARGE_TOOL_COUNT,
            calls: Vec::new(),
        }));
        let app = Router::new()
            .route("/:kind", post(fixture_handler).delete(fixture_delete))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            state,
            server_task: Arc::new(FixtureTask(task)),
            small_url: format!("http://{address}/small"),
            large_url: format!("http://{address}/large"),
        })
    }

    async fn set_large_tool_count(&self, count: usize) {
        self.state.lock().await.large_tool_count = count;
    }

    async fn assert_calls(&self) -> anyhow::Result<()> {
        let calls = self.state.lock().await.calls.clone();
        let expected = BTreeSet::from([
            "small:echo".to_owned(),
            "small:rich".to_owned(),
            "large:catalog_tool_039".to_owned(),
        ]);
        let actual = calls.into_iter().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            actual == expected,
            "unexpected MCP fixture calls: {actual:?}"
        );
        let _keep_alive = &self.server_task;
        Ok(())
    }
}

async fn fixture_handler(
    Path(kind): Path<String>,
    State(state): State<Arc<Mutex<FixtureState>>>,
    Json(request): Json<Value>,
) -> Response {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    match request.get("method").and_then(Value::as_str) {
        Some("server/discover") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "Method not found"}
        }))
        .into_response(),
        Some("initialize") => {
            let mut response = Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": format!("lightspeed-{kind}-fixture"), "version": "1"}
                }
            }))
            .into_response();
            response.headers_mut().insert(
                "mcp-session-id",
                HeaderValue::from_static("fixture-session"),
            );
            response
        }
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("tools/list") => fixture_tools_list(&kind, state, id, &request).await,
        Some("tools/call") => fixture_tool_call(&kind, state, id, &request).await,
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn fixture_tools_list(
    kind: &str,
    state: Arc<Mutex<FixtureState>>,
    id: Value,
    request: &Value,
) -> Response {
    if kind == "small" {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [
                {
                    "name": "echo",
                    "description": "Echo a value",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"value": {"type": "string"}},
                        "required": ["value"],
                        "additionalProperties": false
                    },
                    "annotations": {"readOnlyHint": true}
                },
                {
                    "name": "rich",
                    "description": "Return text, structured data, and an image",
                    "inputSchema": {"type": "object", "additionalProperties": false}
                }
            ]}
        }))
        .into_response();
    }
    if kind != "large" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let count = state.lock().await.large_tool_count;
    let offset = request
        .pointer("/params/cursor")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let end = (offset + LARGE_PAGE_SIZE).min(count);
    let tools = (offset..end)
        .map(|index| {
            let description = if index == 44 {
                "é".repeat(8_000)
            } else {
                format!("Catalog fixture operation {index:03} {}", "d".repeat(2_000))
            };
            json!({
                "name": format!("catalog_tool_{index:03}"),
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"],
                    "additionalProperties": false
                }
            })
        })
        .collect::<Vec<_>>();
    let mut result = json!({"tools": tools});
    if end < count {
        result["nextCursor"] = Value::String(end.to_string());
    }
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

async fn fixture_tool_call(
    kind: &str,
    state: Arc<Mutex<FixtureState>>,
    id: Value,
    request: &Value,
) -> Response {
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let value = request
        .pointer("/params/arguments/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    state.lock().await.calls.push(format!("{kind}:{name}"));
    let result = match (kind, name) {
        ("small", "echo") => json!({
            "content": [{"type": "text", "text": format!("small-echo:{value}")}]
        }),
        ("small", "rich") => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD;
            json!({
                "content": [
                    {"type": "text", "text": "rich-text"},
                    {"type": "image", "data": b64.encode(RICH_PNG_BYTES), "mimeType": "image/png"},
                    {"type": "audio", "data": b64.encode(b"RIFF"), "mimeType": "audio/wav"},
                    {"type": "resource", "resource": {
                        "uri": "fixture://docs/brief.pdf",
                        "mimeType": "application/pdf",
                        "blob": b64.encode(RICH_PDF_BYTES)
                    }}
                ],
                "structuredContent": {"kind": "rich", "count": 1}
            })
        }
        ("large", name) if name.starts_with("catalog_tool_") => json!({
            "content": [{
                "type": "text",
                "text": format!("large-call:{name}:{value}")
            }]
        }),
        _ => json!({
            "content": [{"type": "text", "text": "unknown fixture tool"}],
            "isError": true
        }),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

async fn fixture_delete() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn temporal_live_mcp_approval_approve_and_reject_continue_runs() -> anyhow::Result<()> {
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
        support::live::live_universe_id()?,
        ActivityState::from_pg_store(store, llm, tools),
    );
    run_with_live_worker(activities, run_approval_live_client).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, npm install, and LIGHTSPEED_MCP_PRIVATE_NETWORKS allowing loopback"]
async fn temporal_live_native_mcp_search_call_and_approval() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    let server_id = format!("native_configurator_{}", uuid::Uuid::new_v4().simple());
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(NativeMcpScriptedLlm {
        blobs: blobs.clone(),
        server_id: server_id.clone(),
        selected_tool: "lightspeed_models_list".to_owned(),
    }) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    let state = ActivityState::from_pg_store(store.clone(), llm, tools)
        .with_native_mcp_from_pg_store(store)?;
    let activities = WorkerActivities::for_universe(support::live::live_universe_id()?, state);
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_native_mcp_live_client(client, task_queue, session_id, server_id)
    })
    .await
}

/// A model turn that mixes an `await` (batch-unit execution) with an injected
/// native MCP call whose server gates every call on approval. The batch must
/// park once on the MCP approval while the await stays pending, then complete
/// every call through its intended runtime after the decision.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, npm install, and LIGHTSPEED_MCP_PRIVATE_NETWORKS allowing loopback"]
async fn temporal_live_native_mcp_mixed_await_batch_parks_once_and_completes() -> anyhow::Result<()>
{
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    let server_id = format!("native_mixed_{}", uuid::Uuid::new_v4().simple());
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(MixedBatchScriptedLlm {
        blobs: blobs.clone(),
        server_id: server_id.clone(),
        selected_tool: "lightspeed_models_list".to_owned(),
    }) as Arc<dyn CoreAgentLlm>;
    // The hosted runtime is required: `sleep` and `await` are real tools here
    // and the mixed batch must take the production batch-unit path.
    let hosted = Arc::new(SessionTools::from_pg_store(store.clone()));
    let tools: Arc<dyn CoreAgentTools> = hosted.clone();
    let state = ActivityState::from_pg_store(store.clone(), llm, tools)
        .with_hosted_tools(hosted)
        .with_native_mcp_from_pg_store(store)?;
    let activities = WorkerActivities::for_universe(support::live::live_universe_id()?, state);
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_mixed_batch_live_client(client, task_queue, session_id, server_id)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra, npm install, and LIGHTSPEED_MCP_PRIVATE_NETWORKS allowing loopback"]
async fn temporal_live_mcp_and_session_links_materialize() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let activities = fake_worker_activities().await?;
    run_with_live_worker(activities, run_mcp_live_client).await
}

#[derive(Clone)]
struct ApprovalScriptedLlm {
    blobs: Arc<dyn BlobStore>,
}

#[derive(Clone)]
struct NativeMcpScriptedLlm {
    blobs: Arc<dyn BlobStore>,
    server_id: String,
    selected_tool: String,
}

#[async_trait]
impl CoreAgentLlm for NativeMcpScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let latest_result = request
            .request
            .context
            .entries
            .iter()
            .rev()
            .find_map(|entry| {
                let ContextEntryKind::ToolResult { call_id, .. } = &entry.kind else {
                    return None;
                };
                Some((call_id.clone(), entry.content.content_ref.clone()))
            });
        match latest_result {
            None => {
                self.tool_call(
                    &request,
                    "mcp_find_tools",
                    "native_find",
                    serde_json::json!({"server": self.server_id, "query": "models"}),
                )
                .await
            }
            Some((call_id, result_ref)) if call_id.as_str() == "native_find" => {
                let result = self.blobs.read_text(&result_ref).await.map_err(io_error)?;
                if !result.contains(&self.selected_tool) {
                    return Err(CoreAgentIoError::Failed {
                        message: format!(
                            "native MCP search did not return {}: {result}",
                            self.selected_tool
                        ),
                    });
                }
                self.tool_call(
                    &request,
                    "mcp_call",
                    "native_call",
                    serde_json::json!({
                        "server": self.server_id,
                        "tool": self.selected_tool,
                        "arguments": {}
                    }),
                )
                .await
            }
            Some((_call_id, result_ref)) => {
                let result = self.blobs.read_text(&result_ref).await.map_err(io_error)?;
                self.final_result(&request, format!("native MCP completed: {result}"))
                    .await
            }
        }
    }
}

impl NativeMcpScriptedLlm {
    async fn tool_call(
        &self,
        request: &LlmGenerationRequest,
        tool: &str,
        call_id: &str,
        arguments: serde_json::Value,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if test_support::scripted_tool_id(request, tool).is_none() {
            return Err(CoreAgentIoError::Failed {
                message: format!("native MCP scripted model expected {tool}"),
            });
        }
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(call_id);
        let tool_name = ToolName::new(tool);
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content: engine::ContentRef {
                    content_ref: arguments_ref.clone(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("native-mcp-script".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("native-mcp-{}", request.turn_id.as_u64())),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_id: test_support::scripted_tool_id(request, tool_name.as_str()),
                    tool_name,
                    provider_kind: Some("native-mcp-script".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
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
        let content_ref = self
            .blobs
            .put_bytes(text.into_bytes())
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
                    content_ref,
                    media_type: Some("text/plain".to_owned()),
                    provider_kind: Some("native-mcp-script".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!(
                    "native-mcp-final-{}",
                    request.turn_id.as_u64()
                )),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[async_trait]
impl CoreAgentLlm for ApprovalScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let decision = request
            .request
            .context
            .entries
            .iter()
            .rev()
            .find_map(|entry| {
                let ContextEntryKind::McpApprovalResponse { approve, .. } = entry.kind else {
                    return None;
                };
                match &entry.source {
                    engine::ContextEntrySource::ApprovalDecision { run_id, .. }
                        if *run_id == request.run_id =>
                    {
                        Some(approve)
                    }
                    _ => None,
                }
            });
        if let Some(approve) = decision {
            let output_ref = self
                .blobs
                .put_bytes(
                    format!("approval continuation observed: approve={approve}").into_bytes(),
                )
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
                    content: engine::ContentRef {
                        content_ref: output_ref,
                        media_type: Some("text/plain".to_owned()),
                        provider_kind: Some("approval-script".to_owned()),
                    },
                    preview: None,
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                }],
                facts: LlmGenerationFacts {
                    duration_ms: None,
                    provider_response_id: Some(format!(
                        "approval-final-{}-{}",
                        request.run_id.as_u64(),
                        request.turn_id.as_u64()
                    )),
                    finish: LlmFinish::Stop,
                    usage: None,
                    tool_calls: Vec::new(),
                    approval_requests: Vec::new(),
                    context_token_estimate: None,
                },
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
                content: engine::ContentRef {
                    content_ref: opaque_ref,
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some(
                        engine::OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND.to_owned(),
                    ),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!("approval-request-{}", request.run_id.as_u64())),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: vec![ObservedApprovalRequest {
                    subject: ApprovalSubject::McpToolCall {
                        server_id: "approval-test".to_owned(),
                        server_label: "approval-test".to_owned(),
                        tool_name: "send".to_owned(),
                        arguments_ref,
                    },
                    continuation: ApprovalContinuation::OpenAiMcp {
                        provider_request_id,
                    },
                }],
                context_token_estimate: None,
            },
        })
    }
}

async fn run_approval_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client, store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .build();
    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    for (index, decision) in [ApprovalDecisionKind::Approve, ApprovalDecisionKind::Reject]
        .into_iter()
        .enumerate()
    {
        let started = api
            .start_run(RunStartParams {
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input {
                    items: vec![InputItem::Text {
                        origin: None,
                        text: format!("approval test {index}"),
                    }],
                },
                submission_id: None,
                config: None,
                notify_on_terminal: None,
            })
            .await?;
        let pending =
            wait_for_temporal_pending_approval(&api, &session_id, &started.result.run.id).await?;
        assert_eq!(pending.approval_id, format!("approval_{}", index + 1));
        let decided = api
            .decide_run_approvals(RunApprovalsDecideParams {
                session_id: session_id.as_str().to_owned(),
                run_id: started.result.run.id.clone(),
                decisions: vec![ApprovalDecisionInput {
                    approval_id: pending.approval_id,
                    decision,
                    note: (decision == ApprovalDecisionKind::Reject)
                        .then(|| "operator declined this call".to_owned()),
                }],
            })
            .await?;
        assert_eq!(decided.result.results.len(), 1);
        assert_eq!(
            decided.result.results[0].status,
            ApprovalDecisionStatus::Decided
        );

        let terminal = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
        let output = final_assistant_text(&terminal).expect("approval continuation output");
        assert!(output.contains(match decision {
            ApprovalDecisionKind::Approve => "approve=true",
            ApprovalDecisionKind::Reject => "approve=false",
        }));
        assert!(terminal.pending_approvals.is_empty());
    }

    let events = api
        .read_session_events(SessionEventsReadParams {
            direction: Default::default(),
            before: None,
            session_id: session_id.as_str().to_owned(),
            after: None,
            limit: Some(500),
            wait_ms: None,
        })
        .await?;
    let decisions = events
        .result
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                api::SessionEventKindView::ApprovalDecided { .. }
            )
        })
        .count();
    assert_eq!(decisions, 2);
    Ok(())
}

async fn wait_for_temporal_pending_approval(
    api: &GatewayAgentApi,
    session_id: &SessionId,
    run_id: &str,
) -> anyhow::Result<api::PendingApprovalView> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let session = api
                .read_session(SessionReadParams {
                    session_id: session_id.as_str().to_owned(),
                    run_limit: None,
                })
                .await?
                .result
                .session;
            if let Some(run) = session.runs.iter().find(|run| run.id == run_id)
                && run.status == api::RunStatus::Parked
                && let Some(approval) = run.pending_approvals.first()
            {
                return Ok(approval.clone());
            }
            if let Some(run) = session.runs.iter().find(|run| run.id == run_id)
                && matches!(
                    run.status,
                    api::RunStatus::Completed | api::RunStatus::Failed | api::RunStatus::Cancelled
                )
            {
                let detail = read_run(api, session_id, run_id).await?;
                anyhow::bail!(
                    "run became terminal before approval: status={:?}, output={:?}, batches={:?}",
                    run.status,
                    detail.as_ref().and_then(final_assistant_text),
                    detail.as_ref().map(|run| &run.tool_batches)
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for pending approval"))?
}

async fn run_native_mcp_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    server_id: String,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store)
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .build(),
    );
    let configurator = LiveConfigurator::start(api.clone()).await?;
    let selected_tool = "lightspeed_models_list";
    assert!(expected_configurator_tool_names()?.contains(selected_tool));
    api.put_mcp_server(McpServerPutParams {
        server: McpServerInput {
            server_id: server_id.clone(),
            display_name: Some("Native Configurator".to_owned()),
            server_url: configurator.mcp_url.clone(),
            default_server_label: "native_configurator".to_owned(),
            description: Some("Read and configure this Lightspeed universe".to_owned()),
            allowed_tools: None,
            execution: api::RemoteMcpExecution::Native,
            exposure: api::RemoteMcpExposure::Search,
            approval_default: RemoteMcpApprovalPolicy::Always,
            defer_loading_default: None,
            allow_private_network: true,
            auth_policy: api::McpServerAuthPolicy::None,
            credential: None,
            status: McpServerStatus::Active,
        },
        expected_revision: None,
    })
    .await?;

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(FeaturesConfig {
                mcp: Some(api::McpFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    servers: vec![api::McpServerLink {
                        server_id: server_id.clone(),
                    }],
                }),
                ..FeaturesConfig::default()
            }),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;
    let started = api
        .start_run(RunStartParams {
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "List the configured models through the Configurator MCP".to_owned(),
                }],
            },
            submission_id: None,
            config: None,
            notify_on_terminal: None,
        })
        .await?;
    let pending =
        wait_for_temporal_pending_approval(&api, &session_id, &started.result.run.id).await?;
    match &pending.subject {
        api::ApprovalSubjectView::McpToolCall {
            server_id: actual_server,
            tool_name,
            ..
        } => {
            assert_eq!(actual_server, &server_id);
            assert_eq!(tool_name, selected_tool);
        }
    }
    api.decide_run_approvals(RunApprovalsDecideParams {
        session_id: session_id.as_str().to_owned(),
        run_id: started.result.run.id.clone(),
        decisions: vec![ApprovalDecisionInput {
            approval_id: pending.approval_id,
            decision: ApprovalDecisionKind::Approve,
            note: None,
        }],
    })
    .await?;
    let terminal = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    let output = final_assistant_text(&terminal).expect("native MCP final output");
    assert!(output.contains("native MCP completed"));

    api.delete_mcp_server(McpServerDeleteParams { server_id })
        .await?;
    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("native MCP live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_mcp_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store)
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .build(),
    );
    let configurator = LiveConfigurator::start(api.clone()).await?;
    let server_id = format!("crm_{}", uuid::Uuid::new_v4().simple());
    let selected_tool = "lightspeed_models_list".to_owned();

    let created = api
        .put_mcp_server(McpServerPutParams {
            server: McpServerInput {
                server_id: server_id.clone(),
                display_name: Some("CRM".to_owned()),
                server_url: configurator.mcp_url.clone(),
                default_server_label: "crm".to_owned(),
                description: Some("CRM MCP server".to_owned()),
                allowed_tools: Some(vec![selected_tool.clone()]),
                execution: api::RemoteMcpExecution::Provider,
                exposure: api::RemoteMcpExposure::Inject,
                approval_default: RemoteMcpApprovalPolicy::Never,
                defer_loading_default: Some(true),
                allow_private_network: true,
                auth_policy: api::McpServerAuthPolicy::None,
                credential: None,
                status: McpServerStatus::Active,
            },
            expected_revision: None,
        })
        .await?;
    assert_eq!(created.result.server.server_id, server_id);
    assert_eq!(created.result.server.revision, 1);

    let read = api
        .read_mcp_server(McpServerReadParams {
            server_id: server_id.clone(),
        })
        .await?;
    assert_eq!(read.result.server.default_server_label, "crm");

    let discovered = api
        .discover_mcp_server_tools(McpServerToolsDiscoverParams {
            server_id: server_id.clone(),
        })
        .await?;
    let tools = match discovered.result {
        McpServerToolsDiscoverResponse::Success { tools } => tools,
        McpServerToolsDiscoverResponse::Failure { code, message, .. } => {
            anyhow::bail!("Configurator discovery failed with {code:?}: {message}")
        }
    };
    let discovered_names: BTreeSet<_> = tools.into_iter().map(|tool| tool.name).collect();
    assert_eq!(
        discovered_names,
        expected_configurator_tool_names()?,
        "live discovery must match the generated Configurator registry"
    );
    assert!(discovered_names.contains(&selected_tool));

    let listed = api
        .list_mcp_servers(McpServerListParams {
            status: Some(McpServerStatus::Active),
        })
        .await?;
    assert!(
        listed
            .result
            .servers
            .iter()
            .any(|server| server.server_id == server_id)
    );

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;

    // Link declaratively: put the session config back with the MCP server
    // declared in features.mcp, merged into the existing config document.
    let session = api
        .read_session(SessionReadParams {
            session_id: session_id.as_str().to_owned(),
            run_limit: None,
        })
        .await?
        .result
        .session;
    let mut linked_config = session.config.clone().expect("session config");
    let mut features = linked_config.features.clone().unwrap_or_default();
    features.mcp = Some(api::McpFeature {
        version: api::CURRENT_FEATURE_VERSION,
        servers: vec![api::McpServerLink {
            server_id: server_id.clone(),
        }],
    });
    linked_config.features = Some(features);
    let linked = api
        .put_session_config(SessionConfigPutParams {
            session_id: session_id.as_str().to_owned(),
            expected_config_revision: Some(session.config_revision),
            config: linked_config.clone(),
        })
        .await?;
    let linked_view = read_session_view(&api, &session_id).await?;
    // The toolset entry is named after the model-facing server label, not
    // the record id: `crm` links as `mcp_crm`.
    let tool_id = "mcp_crm".to_owned();
    assert!(
        linked_view
            .active_tools
            .tools
            .iter()
            .any(|tool| tool.tool_id == tool_id),
        "declared MCP tool should materialize into the session toolset"
    );

    let mcp_tools: Vec<_> = linked_view
        .active_tools
        .tools
        .iter()
        .filter(|tool| matches!(tool.kind, api::ToolKindView::RemoteMcp { .. }))
        .collect();
    assert_eq!(mcp_tools.len(), 1);
    let tool = mcp_tools[0];
    assert_eq!(tool.tool_id, tool_id);
    let api::ToolKindView::RemoteMcp {
        server_label,
        allowed_tools,
        approval,
        defer_loading,
        ..
    } = &tool.kind
    else {
        panic!("expected remote MCP tool kind");
    };
    assert_eq!(server_label, "crm");
    assert_eq!(allowed_tools, &Some(vec![selected_tool]));
    assert_eq!(*approval, RemoteMcpApprovalPolicy::Never);
    assert_eq!(*defer_loading, Some(true));

    // Unlink declaratively: put the config again without the server.
    let mut unlinked_config = linked_config;
    if let Some(features) = unlinked_config.features.as_mut() {
        features.mcp = None;
    }
    api.put_session_config(SessionConfigPutParams {
        session_id: session_id.as_str().to_owned(),
        expected_config_revision: Some(linked.result.session.config_revision),
        config: unlinked_config,
    })
    .await?;
    let unlinked_view = read_session_view(&api, &session_id).await?;
    assert!(
        unlinked_view
            .active_tools
            .tools
            .iter()
            .all(|tool| tool.tool_id != tool_id),
        "undeclared MCP tool should be removed from the session toolset"
    );
    assert!(
        unlinked_view
            .active_tools
            .tools
            .iter()
            .all(|tool| !matches!(tool.kind, api::ToolKindView::RemoteMcp { .. })),
        "no remote MCP tools should remain after undeclaring"
    );

    let deleted = api
        .delete_mcp_server(McpServerDeleteParams { server_id })
        .await?;
    assert_eq!(deleted.result.server.default_server_label, "crm");

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent MCP live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

struct LiveConfigurator {
    child: tokio::process::Child,
    gateway_task: tokio::task::JoinHandle<()>,
    mcp_url: String,
}

impl LiveConfigurator {
    async fn start(api: Arc<GatewayAgentApi>) -> anyhow::Result<Self> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("temporal-server crate must be inside the workspace")
            .to_owned();
        let tsx = repo_root.join("node_modules/.bin/tsx");
        if !tsx.is_file() {
            anyhow::bail!(
                "Configurator live test requires npm install (missing {})",
                tsx.display()
            );
        }

        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let gateway_address = gateway_listener.local_addr()?;
        let gateway = gateway_router(
            Arc::new(GatewayState::for_api(api)),
            DEFAULT_MAX_REQUEST_BODY_BYTES,
            temporal_server::gateway::GatewayRoutes::ALL,
        );
        let gateway_task = tokio::spawn(async move {
            let _ = axum::serve(gateway_listener, gateway).await;
        });

        let configurator_port = reserve_loopback_port()?;
        let mut child = tokio::process::Command::new(&tsx)
            .arg("platform/configurator-mcp/src/bin.ts")
            .current_dir(&repo_root)
            .env("LIGHTSPEED_AUTH_MODE", "single")
            .env("LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST", "127.0.0.1")
            .env(
                "LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT",
                configurator_port.to_string(),
            )
            .env(
                "LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL",
                format!("http://{gateway_address}/rpc"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let base_url = format!("http://127.0.0.1:{configurator_port}");
        let health_url = format!("{base_url}/health");
        let http = reqwest::Client::new();
        let mut ready = false;
        for _ in 0..100 {
            if child.try_wait()?.is_some() {
                anyhow::bail!("Configurator exited before becoming ready");
            }
            if http
                .get(&health_url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !ready {
            anyhow::bail!("Configurator did not become ready at {health_url}");
        }

        Ok(Self {
            child,
            gateway_task,
            mcp_url: format!("{base_url}/mcp"),
        })
    }
}

impl Drop for LiveConfigurator {
    fn drop(&mut self) {
        self.gateway_task.abort();
        let _ = self.child.start_kill();
    }
}

fn reserve_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn expected_configurator_tool_names() -> anyhow::Result<BTreeSet<String>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("temporal-server crate must be inside the workspace")
        .to_owned();
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(
        repo_root.join("crates/api/contract/methods.json"),
    )?)?;
    let filter: serde_json::Value = serde_json::from_slice(&std::fs::read(
        repo_root.join("platform/configurator-mcp/tool-filter.json"),
    )?)?;
    let excluded: BTreeSet<_> = filter["excludeMethods"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Configurator filter has no excludeMethods array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("Configurator filter contains a non-string method"))
        })
        .collect::<anyhow::Result<_>>()?;
    manifest["methods"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("API method manifest has no methods array"))?
        .iter()
        .filter(|entry| entry["scope"].as_str() == Some("universe"))
        .filter_map(|entry| entry["method"].as_str())
        .filter(|method| !excluded.contains(*method))
        .map(|method| {
            let suffix: String = method
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                        character
                    } else {
                        '_'
                    }
                })
                .collect();
            Ok(format!("lightspeed_{suffix}"))
        })
        .collect()
}

/// Scripted model for the mixed-batch scenario: schedule a timer, then emit
/// one turn holding both the `await` on that timer and an injected native MCP
/// call, then summarize every tool result it can see.
#[derive(Clone)]
struct MixedBatchScriptedLlm {
    blobs: Arc<dyn BlobStore>,
    server_id: String,
    selected_tool: String,
}

const MIXED_SLEEP_CALL_ID: &str = "mixed_sleep";
const MIXED_AWAIT_CALL_ID: &str = "mixed_await";
const MIXED_MCP_CALL_ID: &str = "mixed_mcp";

fn parse_timer_promise(sleep_result: &str) -> Option<String> {
    // "Timer scheduled for N ms (promise promise_1). Await it with ..."
    let start = sleep_result.find("(promise ")? + "(promise ".len();
    let end = sleep_result[start..].find(')')? + start;
    Some(sleep_result[start..end].to_owned())
}

#[async_trait]
impl CoreAgentLlm for MixedBatchScriptedLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let tool_results = request
            .request
            .context
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                ContextEntryKind::ToolResult { call_id, .. } => {
                    Some((call_id.clone(), entry.content.content_ref.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        match tool_results.as_slice() {
            [] => {
                self.tool_calls(
                    &request,
                    vec![(SLEEP_TOOL_NAME, MIXED_SLEEP_CALL_ID, json!({"ms": 1500}))],
                )
                .await
            }
            [(call_id, result_ref)] if call_id.as_str() == MIXED_SLEEP_CALL_ID => {
                let text = self.blobs.read_text(result_ref).await.map_err(io_error)?;
                let promise =
                    parse_timer_promise(&text).ok_or_else(|| CoreAgentIoError::Failed {
                        message: format!("sleep result did not name a promise: {text}"),
                    })?;
                let injected = self.injected_tool_name(&request)?;
                self.tool_calls(
                    &request,
                    vec![
                        (
                            AWAIT_TOOL_NAME,
                            MIXED_AWAIT_CALL_ID,
                            json!({"promises": [promise], "mode": "all", "timeout_ms": 60_000}),
                        ),
                        (injected.as_str(), MIXED_MCP_CALL_ID, json!({})),
                    ],
                )
                .await
            }
            results => {
                let mut texts = Vec::with_capacity(results.len());
                for (call_id, result_ref) in results {
                    let text = self.blobs.read_text(result_ref).await.map_err(io_error)?;
                    texts.push(format!("{call_id}={text}"));
                }
                self.final_result(
                    &request,
                    format!("mixed native MCP batch completed: {}", texts.join(" | ")),
                )
                .await
            }
        }
    }
}

impl MixedBatchScriptedLlm {
    /// The injected call name is `<server tool>__<remote tool>`; the server
    /// tool is found by its record id so the test never hard-codes the
    /// label-derived tool name.
    fn injected_tool_name(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<String, CoreAgentIoError> {
        request
            .request
            .tools
            .iter()
            .find(|tool| {
                matches!(&tool.kind, ToolKind::RemoteMcp(spec) if spec.server_id == self.server_id)
            })
            .map(|tool| format!("{}__{}", tool.name, self.selected_tool))
            .ok_or_else(|| CoreAgentIoError::Failed {
                message: format!(
                    "mixed batch scripted model expected the {} MCP server tool",
                    self.server_id
                ),
            })
    }

    async fn tool_calls(
        &self,
        request: &LlmGenerationRequest,
        calls: Vec<(&str, &str, Value)>,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let mut context_entries = Vec::with_capacity(calls.len());
        let mut tool_calls = Vec::with_capacity(calls.len());
        for (tool, call_id, arguments) in calls {
            let arguments_ref = self
                .blobs
                .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
                .await
                .map_err(io_error)?;
            let call_id = ToolCallId::new(call_id);
            let tool_name = ToolName::new(tool);
            context_entries.push(ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content: engine::ContentRef {
                    content_ref: arguments_ref.clone(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("mixed-batch-script".to_owned()),
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
                provider_kind: Some("mixed-batch-script".to_owned()),
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
                provider_response_id: Some(format!("mixed-batch-{}", request.turn_id.as_u64())),
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
        let content_ref = self
            .blobs
            .put_bytes(text.into_bytes())
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
                    content_ref,
                    media_type: Some("text/plain".to_owned()),
                    provider_kind: Some("mixed-batch-script".to_owned()),
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: Some(format!(
                    "mixed-batch-final-{}",
                    request.turn_id.as_u64()
                )),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

async fn run_mixed_batch_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    server_id: String,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store)
            .with_task_queue(task_queue)
            .with_default_model(model.clone())
            .build(),
    );
    let configurator = LiveConfigurator::start(api.clone()).await?;
    let selected_tool = "lightspeed_models_list";
    assert!(expected_configurator_tool_names()?.contains(selected_tool));
    api.put_mcp_server(McpServerPutParams {
        server: McpServerInput {
            server_id: server_id.clone(),
            display_name: Some("Native Mixed Configurator".to_owned()),
            server_url: configurator.mcp_url.clone(),
            default_server_label: "native_mixed".to_owned(),
            description: Some("Read this Lightspeed universe".to_owned()),
            allowed_tools: Some(vec![selected_tool.to_owned()]),
            execution: api::RemoteMcpExecution::Native,
            exposure: api::RemoteMcpExposure::Inject,
            approval_default: RemoteMcpApprovalPolicy::Always,
            defer_loading_default: None,
            allow_private_network: true,
            auth_policy: api::McpServerAuthPolicy::None,
            credential: None,
            status: McpServerStatus::Active,
        },
        expected_revision: None,
    })
    .await?;

    api.start_session(SessionStartParams {
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            features: Some(FeaturesConfig {
                timers: Some(TimersFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                }),
                mcp: Some(api::McpFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    servers: vec![api::McpServerLink {
                        server_id: server_id.clone(),
                    }],
                }),
                ..FeaturesConfig::default()
            }),
            ..SessionConfig::default()
        }),
        profile: None,
        environment: None,
        delete_after_close_ms: None,
    })
    .await?;
    let started = api
        .start_run(RunStartParams {
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "Schedule a timer, then await it while listing models".to_owned(),
                }],
            },
            submission_id: None,
            config: None,
            notify_on_terminal: None,
        })
        .await?;

    // The mixed batch parks on the MCP approval; the await sibling neither
    // fails nor defers ahead of the decision.
    let pending =
        wait_for_temporal_pending_approval(&api, &session_id, &started.result.run.id).await?;
    match &pending.subject {
        api::ApprovalSubjectView::McpToolCall {
            server_id: actual_server,
            tool_name,
            ..
        } => {
            assert_eq!(actual_server, &server_id);
            assert_eq!(tool_name, selected_tool);
        }
    }
    api.decide_run_approvals(RunApprovalsDecideParams {
        session_id: session_id.as_str().to_owned(),
        run_id: started.result.run.id.clone(),
        decisions: vec![ApprovalDecisionInput {
            approval_id: pending.approval_id,
            decision: ApprovalDecisionKind::Approve,
            note: None,
        }],
    })
    .await?;

    let terminal = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    let output = final_assistant_text(&terminal).expect("mixed batch final output");
    assert!(
        output.contains("mixed native MCP batch completed"),
        "unexpected final output: {output}"
    );
    for call_id in [MIXED_SLEEP_CALL_ID, MIXED_AWAIT_CALL_ID, MIXED_MCP_CALL_ID] {
        let call = terminal
            .tool_batches
            .iter()
            .flat_map(|batch| &batch.calls)
            .find(|call| call.call_id == call_id)
            .expect("mixed batch call");
        assert_eq!(call.status, api::ToolItemStatus::Succeeded, "{call:?}");
        assert!(
            call.tool_id.is_some(),
            "mixed batch call must retain its admitted identity"
        );
        assert!(
            output.contains(&format!("{call_id}=")),
            "final output lacks the {call_id} result: {output}"
        );
    }
    assert!(
        !output.contains("unknown tool") && !output.contains("requires batch-unit execution"),
        "a call fell through to the wrong runtime: {output}"
    );
    assert!(terminal.pending_approvals.is_empty());

    api.delete_mcp_server(McpServerDeleteParams { server_id })
        .await?;
    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("native MCP mixed batch live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}
