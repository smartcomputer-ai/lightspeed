//! Live engine-loop test proving the skill catalog flow works end to end on
//! the Anthropic Messages adapter: the catalog lowers as a user message, the
//! model picks the matching skill, reads its SKILL.md through the fs file
//! tool, and follows its migration preparation instructions.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use engine::{
    BlobRef, ContextConfig, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    CoreAgentCommand, CoreAgentEvent, ModelSelection, ProviderApiKind, RunConfig, RunStatus,
    SessionConfig, SessionId, WorkspaceLink, WorkspaceLinkAccess, WorkspaceLinkTarget,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_clients::anthropic::messages::{Client, Config};
use llm_runtime::{AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};
use tools::{
    fs::tools::ReadFileResult,
    fs::{FsPath, FsToolContext, LinkedVfsFileSystem},
    runtime::InlineToolRuntime,
    toolset::{ToolsetConfig, register_toolset},
};
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
    InlineFile, ResolvedWorkspaceLink, ResolvedWorkspaceLinkTarget, VfsCatalogError, VfsPath,
    VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore, create_inline_snapshot,
};

mod support;

use support::retrying_anthropic_messages_client;

const MIGRATION_FIRST_STEP: &str = "Create an immutable checkpoint of the matrix before migration.";

fn live_model() -> String {
    env_or_dotenv_var("ANTHROPIC_MESSAGES_MODEL")
        .or_else(|_| env_or_dotenv_var("ANTHROPIC_LIVE_MODEL"))
        .unwrap_or_else(|_| "claude-opus-5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("ANTHROPIC_API_KEY").expect(
        "ANTHROPIC_API_KEY must be set in env or root .env to run Anthropic skills live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "ANTHROPIC_API_KEY is set but empty"
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("ANTHROPIC_BASE_URL") {
        config.base_url = base_url;
    }
    Client::new(config).expect("Anthropic Messages client")
}

fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(root_dotenv_path()).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() == name {
            return Some(unquote_dotenv_value(value.trim()));
        }
    }
    None
}

fn root_dotenv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .join(".env")
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[derive(Default)]
struct LiveVfsCatalog {
    workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
}

#[async_trait]
impl VfsWorkspaceStore for LiveVfsCatalog {
    async fn create_workspace(
        &self,
        record: CreateVfsWorkspaceRecord,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let workspace = VfsWorkspaceRecord {
            workspace_id: record.workspace_id,
            display_name: record.display_name,
            base_snapshot_ref: record.base_snapshot_ref,
            head_snapshot_ref: record.head_snapshot_ref,
            head_totals: record.head_totals,
            revision: 0,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.created_at_ms,
        };
        self.workspaces
            .lock()
            .expect("workspace lock")
            .insert(workspace.workspace_id.clone(), workspace.clone());
        Ok(workspace)
    }

    async fn read_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        self.workspaces
            .lock()
            .expect("workspace lock")
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| VfsCatalogError::NotFound {
                kind: "workspace",
                id: workspace_id.to_string(),
            })
    }

    async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
        Ok(self
            .workspaces
            .lock()
            .expect("workspace lock")
            .values()
            .cloned()
            .collect())
    }

    async fn compare_and_set_head(
        &self,
        request: CompareAndSetVfsWorkspaceHead,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let mut workspaces = self.workspaces.lock().expect("workspace lock");
        let workspace =
            workspaces
                .get_mut(&request.workspace_id)
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                })?;
        if request
            .expected_revision
            .is_some_and(|revision| revision != workspace.revision)
        {
            return Err(VfsCatalogError::RevisionConflict {
                workspace_id: request.workspace_id,
                expected_revision: request.expected_revision.unwrap_or_default(),
                actual_revision: workspace.revision,
            });
        }
        if let Some(display_name) = request.display_name {
            workspace.display_name = Some(display_name);
        }
        workspace.head_snapshot_ref = request.new_head_snapshot_ref;
        workspace.head_totals = request.new_head_totals;
        workspace.revision += 1;
        workspace.updated_at_ms = request.updated_at_ms;
        Ok(workspace.clone())
    }

    async fn delete_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        self.workspaces
            .lock()
            .expect("workspace lock")
            .remove(workspace_id)
            .ok_or_else(|| VfsCatalogError::NotFound {
                kind: "workspace",
                id: workspace_id.to_string(),
            })
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_selects_and_reads_the_matching_skill() {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let vfs = Arc::new(LiveVfsCatalog::default());
    let session_id = SessionId::new("session-live-anthropic-skills");
    sessions
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let snapshot = create_inline_snapshot(blobs.as_ref(), None, CreateInlineSnapshotRequest::new(vec![
            InlineFile::new(
                "matrix-migration/SKILL.md",
                format!(
                    "---\nname: matrix-migration\ndescription: Use to prepare a matrix migration and protect the source data.\nshort_description: Matrix migration preparation\n---\n\n# Matrix migration preparation\n\nFor a one-line preparation step, use exactly this checklist item: {MIGRATION_FIRST_STEP}\n"
                )
                .into_bytes(),
            )
            .unwrap(),
            InlineFile::new(
                "deploy-review/SKILL.md",
                b"---\nname: deploy-review\ndescription: Use when reviewing deployment risks and rollout plans.\nshort_description: Deployment review\n---\n\nReview canary rollout and rollback plans before deployment.\n"
                    .to_vec(),
            )
            .unwrap(),
            InlineFile::new(
                "invoice-audit/SKILL.md",
                b"---\nname: invoice-audit\ndescription: Use when auditing invoice line items and payment status.\nshort_description: Invoice audit\n---\n\nMatch invoice line items to purchase orders before approving payment.\n"
                    .to_vec(),
            )
            .unwrap(),
        ]),
    )
    .await
    .expect("create skill snapshot");
    let workspace_links = vec![WorkspaceLink {
        path: "/skills/system".to_owned(),
        target: WorkspaceLinkTarget::Snapshot {
            snapshot_ref: snapshot.snapshot_ref.to_string(),
        },
        access: WorkspaceLinkAccess::ReadOnly,
    }];

    let linked_fs = LinkedVfsFileSystem::new(
        blobs.clone(),
        vfs.clone(),
        vec![ResolvedWorkspaceLink {
            path: VfsPath::parse("/skills/system").unwrap(),
            target: ResolvedWorkspaceLinkTarget::AvailableSnapshot {
                snapshot_ref: snapshot.snapshot_ref,
            },
            access: WorkspaceLinkAccess::ReadOnly,
        }],
    )
    .expect("linked fs");
    let fs_ctx = FsToolContext::new(Arc::new(linked_fs), blobs.clone()).with_cwd(FsPath::root());
    let model = ModelSelection {
        api_kind: ProviderApiKind::AnthropicMessages,
        provider_id: "anthropic".to_string(),
        model: live_model(),
    };
    let toolset = register_toolset(&ToolsetConfig::workspace()).expect("toolset");
    let tools = Arc::new(InlineToolRuntime::with_vfs_filesystem(
        fs_ctx,
        tools::runtime::ToolCatalog::default(),
    ));

    let llm = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new().with_generation_adapter(
            ProviderApiKind::AnthropicMessages,
            Arc::new(AnthropicMessagesLlmAdapter::new(
                retrying_anthropic_messages_client(live_client()),
                blobs.clone(),
            )),
        ),
    ));
    let stores = RunnerStores::new(sessions.clone(), blobs.clone()).with_vfs_catalog(vfs);
    let runner = SessionRunner::new(stores, llm).with_tools(tools);

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: session_config(model, workspace_links),
            },
            max_steps: None,
        })
        .await
        .expect("open session");
    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 11,
            command: CoreAgentCommand::ReplaceTools {
                expected_revision: Some(0),
                tools: toolset.tools,
            },
            max_steps: None,
        })
        .await
        .expect("replace tools");

    let input_ref = blobs
        .put_bytes(
            b"Read the relevant SKILL.md from the skill catalog and use it to write a one-line preparation step for a matrix migration."
                .to_vec(),
        )
        .await
        .expect("write prompt");
    let outcome = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 20,
            command: CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                notify_on_terminal: Vec::new(),
                submission_id: None,
                source: engine::RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: engine::ContentRef {
                            content_ref: input_ref,
                            media_type: None,
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                },
                run_config: run_config(),
            }),
            max_steps: Some(128),
        })
        .await
        .expect("drive live run");

    assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
    assert_eq!(
        outcome.state.runs.completed[0].status,
        RunStatus::Completed,
        "{}",
        run_failure_text(blobs.as_ref(), &outcome.state).await
    );

    let selected_call_id = selected_skill_read_call_id(blobs.as_ref(), &outcome.emitted_entries)
        .await
        .expect("expected model to read matrix-migration SKILL.md");
    let selected_call = outcome
        .emitted_entries
        .iter()
        .find_map(|entry| {
            let CoreAgentEvent::Tool(engine::ToolEvent::BatchStarted { calls, .. }) = &entry.event
            else {
                return None;
            };
            calls.iter().find(|call| call.call_id == selected_call_id)
        })
        .expect("admitted skill read call");
    assert_eq!(
        selected_call.tool_id.as_ref().map(|id| id.as_str()),
        Some("vfs.read_file")
    );
    assert_eq!(selected_call.tool_name.as_str(), "VfsRead");
    assert!(
        !read_paths(blobs.as_ref(), &outcome.emitted_entries)
            .await
            .iter()
            .any(|path| path.contains("deploy-review") || path.contains("invoice-audit")),
        "model read a decoy skill: {:?}",
        read_paths(blobs.as_ref(), &outcome.emitted_entries).await
    );

    let assistant_text = assistant_text(blobs.as_ref(), &outcome.emitted_entries).await;
    assert!(
        assistant_text.contains(MIGRATION_FIRST_STEP),
        "assistant did not follow the migration skill; assistant={assistant_text:?}"
    );
}

fn session_config(model: ModelSelection, workspace_links: Vec<WorkspaceLink>) -> SessionConfig {
    SessionConfig {
        model,
        generation: engine::GenerationConfig {
            max_output_tokens: Some(4096),
            reasoning_effort: None,
            tool_choice: None,
            parallel_tool_use: None,
            processing_tier: None,
        },
        limits: Default::default(),
        context: ContextConfig { compaction: None },
        features: engine::FeaturesConfig {
            vfs: Some(engine::VfsFeature {
                skills: Some(engine::VfsSkillsConfig {
                    roots: Some(
                        workspace_links
                            .iter()
                            .map(|link| link.path.clone())
                            .collect(),
                    ),
                }),
                workspace_links,
                tools: Some(engine::VfsToolSurface::ReadOnly),
                ..engine::VfsFeature::default()
            }),
            ..engine::FeaturesConfig::default()
        },
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(6),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
        max_tool_rounds: Some(3),
        model_override: None,
        max_output_tokens: None,
        provider_params: None,
        tool_choice: None,
    }
}

async fn selected_skill_read_call_id(
    blobs: &dyn BlobStore,
    entries: &[engine::CoreAgentEntry],
) -> Option<engine::ToolCallId> {
    for entry in entries {
        let CoreAgentEvent::Tool(engine::ToolEvent::CallCompleted { result, .. }) = &entry.event
        else {
            continue;
        };
        let Some(output_ref) = result.output_ref.as_ref() else {
            continue;
        };
        let read = read_file_result(blobs, output_ref).await?;
        if read.resolved_path.as_str().contains("matrix-migration") {
            return Some(result.call_id.clone());
        }
    }
    None
}

async fn read_paths(blobs: &dyn BlobStore, entries: &[engine::CoreAgentEntry]) -> Vec<String> {
    let mut paths = Vec::new();
    for entry in entries {
        let CoreAgentEvent::Tool(engine::ToolEvent::CallCompleted { result, .. }) = &entry.event
        else {
            continue;
        };
        let Some(output_ref) = result.output_ref.as_ref() else {
            continue;
        };
        if let Some(read) = read_file_result(blobs, output_ref).await {
            paths.push(read.resolved_path.as_str().to_owned());
        }
    }
    paths
}

async fn read_file_result(blobs: &dyn BlobStore, output_ref: &BlobRef) -> Option<ReadFileResult> {
    let bytes = blobs.read_bytes(output_ref).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn assistant_text(blobs: &dyn BlobStore, entries: &[engine::CoreAgentEntry]) -> String {
    let mut text = String::new();
    for entry in entries {
        if let CoreAgentEvent::Context(engine::ContextEvent::EntriesApplied { entries, .. }) =
            &entry.event
        {
            for item in entries {
                if matches!(
                    item.kind,
                    engine::ContextEntryKind::Message {
                        role: engine::ContextMessageRole::Assistant
                    }
                ) {
                    text.push_str(&support::content_text(blobs, &item.content).await);
                    text.push('\n');
                }
            }
        }
    }
    text
}

async fn run_failure_text(blobs: &dyn BlobStore, state: &engine::CoreAgentState) -> String {
    let Some(run) = state.runs.completed.first() else {
        return "run did not complete".to_owned();
    };
    let Some(failure) = run.failure.as_ref() else {
        return format!("run status was {:?}", run.status);
    };
    let Some(message_ref) = failure.message_ref.as_ref() else {
        return format!("run failed without message: {:?}", failure.kind);
    };
    blobs
        .read_text(message_ref)
        .await
        .unwrap_or_else(|error| format!("failed to read failure message {message_ref}: {error}"))
}
