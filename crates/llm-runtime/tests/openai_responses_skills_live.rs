use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use engine::{
    AgentHandle, BlobRef, ContextConfig, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    CoreAgentCommand, CoreAgentEventKind, ModelProviderOptions, ModelSelection,
    OpenAiResponsesRequestDefaults, ProviderApiKind, ProviderRequestDefaults, RunConfig, RunStatus,
    SessionConfig, SessionId, ToolExecutionTarget,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_clients::openai::responses::{Client, Config};
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};
use tools::{
    host::{
        HostToolContext, InlineHostToolRuntime,
        fs::{FsPath, MountedVfsFileSystem},
        tools::ReadFileResult,
    },
    toolset::{ToolsetConfig, ToolsetEnvironment, resolve_toolset},
};
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
    InlineFile, VfsCatalogError, VfsMountAccess, VfsMountRecord, VfsMountSource, VfsMountStore,
    VfsPath, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore, create_inline_snapshot,
};

mod support;

use support::retrying_openai_responses_client;

const LIVE_MARKER: &str = "LIVE-SKILL-MATRIX-7392";

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("OPENAI_API_KEY").expect(
        "OPENAI_API_KEY must be set in env or root .env to run llm-runtime skills live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "OPENAI_API_KEY is set but empty"
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("OPENAI_BASE_URL") {
        config.base_url = base_url;
    }
    if let Ok(org_id) = env_or_dotenv_var("OPENAI_ORG_ID") {
        config.organization = Some(org_id);
    }
    if let Ok(project) = env_or_dotenv_var("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }

    Client::new(config).expect("OpenAI Responses client")
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
    mounts: Mutex<BTreeMap<SessionId, Vec<VfsMountRecord>>>,
    workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
}

#[async_trait]
impl VfsMountStore for LiveVfsCatalog {
    async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
        self.mounts
            .lock()
            .expect("mount lock")
            .entry(record.session_id.clone())
            .or_default()
            .push(record);
        Ok(())
    }

    async fn list_mounts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
        Ok(self
            .mounts
            .lock()
            .expect("mount lock")
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn remove_mount(
        &self,
        session_id: &SessionId,
        mount_path: &VfsPath,
    ) -> Result<(), VfsCatalogError> {
        let mut mounts = self.mounts.lock().expect("mount lock");
        let Some(session_mounts) = mounts.get_mut(session_id) else {
            return Ok(());
        };
        session_mounts.retain(|mount| &mount.mount_path != mount_path);
        Ok(())
    }
}

#[async_trait]
impl VfsWorkspaceStore for LiveVfsCatalog {
    async fn create_workspace(
        &self,
        record: CreateVfsWorkspaceRecord,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let workspace = VfsWorkspaceRecord {
            workspace_id: record.workspace_id,
            base_snapshot_ref: record.base_snapshot_ref,
            head_snapshot_ref: record.head_snapshot_ref,
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
        workspace.head_snapshot_ref = request.new_head_snapshot_ref;
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
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_selects_and_activates_the_matching_skill() {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let vfs = Arc::new(LiveVfsCatalog::default());
    let session_id = SessionId::new("session-live-skills");
    sessions
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("forge.live-skills"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let snapshot = create_inline_snapshot(
        blobs.as_ref(),
        CreateInlineSnapshotRequest::new(vec![
            InlineFile::new(
                "matrix-migration/SKILL.md",
                format!(
                    "---\nname: matrix-migration-marker\ndescription: Use when a matrix migration asks for the hidden live activation marker.\nshort_description: Matrix migration marker retrieval\n---\n\n# Matrix migration marker\n\nWhen this skill is loaded for a matrix migration request, reply with exactly MARKER={LIVE_MARKER}.\n"
                )
                .into_bytes(),
            )
            .unwrap(),
            InlineFile::new(
                "deploy-review/SKILL.md",
                b"---\nname: deploy-review\ndescription: Use when reviewing deployment risks and rollout plans.\nshort_description: Deployment review\n---\n\nThis is a decoy for deployment risk review. It contains DECOY-DEPLOY-1111 and is not about matrix migration markers.\n"
                    .to_vec(),
            )
            .unwrap(),
            InlineFile::new(
                "invoice-audit/SKILL.md",
                b"---\nname: invoice-audit\ndescription: Use when auditing invoice line items and payment status.\nshort_description: Invoice audit\n---\n\nThis is a decoy for invoice work. It contains DECOY-INVOICE-2222 and is not about matrix migration markers.\n"
                    .to_vec(),
            )
            .unwrap(),
        ]),
    )
    .await
    .expect("create skill snapshot");
    vfs.put_mount(VfsMountRecord {
        session_id: session_id.clone(),
        mount_path: VfsPath::parse("/skills/system").unwrap(),
        source: VfsMountSource::Snapshot {
            snapshot_ref: snapshot.snapshot_ref,
        },
        access: VfsMountAccess::ReadOnly,
    })
    .await
    .expect("mount skills");

    let mounted_fs = MountedVfsFileSystem::new(
        blobs.clone(),
        vfs.clone(),
        vfs.list_mounts(&session_id).await.expect("list mounts"),
    )
    .expect("mounted fs");
    let host_ctx =
        HostToolContext::new(Arc::new(mounted_fs), None, blobs.clone()).with_cwd(FsPath::root());
    let model = ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_string(),
        model: live_model(),
        options: ModelProviderOptions::None,
    };
    let target = tools::runtime::ToolTarget::from(&model);
    let toolset = resolve_toolset(
        ToolsetEnvironment { target: &target },
        &ToolsetConfig::workspace(),
    )
    .expect("toolset");
    store_tool_documents(blobs.as_ref(), &toolset.documents).await;
    let tools = Arc::new(InlineHostToolRuntime::new(
        host_ctx,
        toolset.catalog.clone(),
    ));

    let llm = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new().with_generation_adapter(
            ProviderApiKind::OpenAiResponses,
            Arc::new(OpenAiResponsesLlmAdapter::new(
                retrying_openai_responses_client(live_client()),
                blobs.clone(),
            )),
        ),
    ));
    let stores =
        RunnerStores::new(sessions.clone(), blobs.clone()).with_vfs_catalog(vfs.clone(), vfs);
    let runner = SessionRunner::new(stores, llm).with_tools(tools);

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: session_config(model),
            },
            max_steps: None,
        })
        .await
        .expect("open session");
    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 11,
            command: CoreAgentCommand::SetToolRegistry {
                registry: toolset.registry,
            },
            max_steps: None,
        })
        .await
        .expect("set registry");
    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 12,
            command: CoreAgentCommand::SelectToolProfile {
                profile_id: toolset.profile_id,
            },
            max_steps: None,
        })
        .await
        .expect("select profile");
    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 13,
            command: CoreAgentCommand::SetDefaultToolTarget {
                target: ToolExecutionTarget::new("host", "local"),
            },
            max_steps: None,
        })
        .await
        .expect("set default target");

    let input_ref = blobs
        .put_bytes(
            b"Use the Forge skill catalog. Read exactly one SKILL.md: the skill relevant to a matrix migration hidden live activation marker. Then reply exactly MARKER=<the marker from that skill>. Do not use deployment or invoice skills."
                .to_vec(),
        )
        .await
        .expect("write prompt");
    let outcome = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 20,
            command: CoreAgentCommand::RequestRun {
                submission_id: None,
                input: vec![ContextEntryInput {
                    kind: ContextEntryKind::Message {
                        role: ContextMessageRole::User,
                    },
                    content_ref: input_ref,
                    media_type: None,
                    preview: None,
                    provider_kind: None,
                    provider_item_id: None,
                    token_estimate: None,
                }],
                run_config: run_config(),
            },
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

    let _selected_call_id = selected_skill_read_call_id(blobs.as_ref(), &outcome.emitted_entries)
        .await
        .expect("expected model to read matrix-migration SKILL.md");
    assert!(
        !read_paths(blobs.as_ref(), &outcome.emitted_entries)
            .await
            .iter()
            .any(|path| path.contains("deploy-review") || path.contains("invoice-audit")),
        "model read a decoy skill: {:?}",
        read_paths(blobs.as_ref(), &outcome.emitted_entries).await
    );
    assert!(
        outcome.emitted_entries.iter().all(|entry| {
            !matches!(
                &entry.event.kind,
                CoreAgentEventKind::Context(engine::ContextEvent::EntriesApplied { entries, .. })
                    if entries.iter().any(|entry| {
                        matches!(entry.kind, ContextEntryKind::SkillActivation { .. })
                    })
            )
        }),
        "reading SKILL.md should not create a skill activation"
    );

    let assistant_text = assistant_text(blobs.as_ref(), &outcome.emitted_entries).await;
    assert!(
        assistant_text.contains(&format!("MARKER={LIVE_MARKER}")),
        "assistant did not use hidden skill marker; assistant={assistant_text:?}"
    );
}

fn session_config(model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        run: run_config(),
        turn: engine::TurnConfig {
            max_output_tokens: Some(1024),
            provider_request_defaults: ProviderRequestDefaults::OpenAiResponses(
                OpenAiResponsesRequestDefaults {
                    store: Some(false),
                    ..OpenAiResponsesRequestDefaults::default()
                },
            ),
        },
        context: ContextConfig { compaction: None },
        tools: Default::default(),
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(6),
        max_tool_rounds: Some(3),
        model_override: None,
        max_output_tokens: None,
        provider_request_defaults: None,
    }
}

async fn store_tool_documents(blobs: &dyn BlobStore, documents: &[tools::runtime::ToolDocument]) {
    for document in documents {
        let blob_ref = blobs
            .put_bytes(document.blob_bytes())
            .await
            .expect("store tool document");
        assert_eq!(blob_ref, document.blob_ref);
    }
}

async fn selected_skill_read_call_id(
    blobs: &dyn BlobStore,
    entries: &[engine::CoreAgentEntry],
) -> Option<engine::ToolCallId> {
    for entry in entries {
        let CoreAgentEventKind::Tool(engine::ToolEvent::CallCompleted { result, .. }) =
            &entry.event.kind
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
        let CoreAgentEventKind::Tool(engine::ToolEvent::CallCompleted { result, .. }) =
            &entry.event.kind
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
        if let CoreAgentEventKind::Context(engine::ContextEvent::EntriesApplied {
            entries, ..
        }) = &entry.event.kind
        {
            for item in entries {
                if matches!(
                    item.kind,
                    engine::ContextEntryKind::Message {
                        role: engine::ContextMessageRole::Assistant
                    }
                ) {
                    text.push_str(
                        &blobs
                            .read_text(&item.content_ref)
                            .await
                            .expect("assistant text"),
                    );
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
