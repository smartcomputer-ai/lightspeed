use async_trait::async_trait;

use super::*;
use crate::gateway::service::prompts::{active_prompt_context_entries, prompt_report_ref};

#[test]
fn admission_failure_mapping_uses_gateway_error_kinds() {
    assert_eq!(
        map_admission_failure_to_api_error(&failure(AgentAdmissionFailureKind::InvalidCommand))
            .kind,
        AgentApiErrorKind::InvalidRequest
    );
    assert_eq!(
        map_admission_failure_to_api_error(&failure(AgentAdmissionFailureKind::RejectedCommand))
            .kind,
        AgentApiErrorKind::Rejected
    );
}

#[test]
fn skill_list_response_marks_active_catalog_entries() {
    let catalog_ref = BlobRef::from_bytes(b"catalog");
    let catalog = test_skill_catalog(
        &catalog_ref,
        vec![
            test_skill_metadata("skill:review", "review", true),
            test_skill_metadata("skill:deploy", "deploy", false),
        ],
    );
    let activation = direct_activation(
        "skill:review",
        &catalog_ref,
        &BlobRef::from_bytes(b"review-body"),
        ApiSkillActivationScope::Run,
    );

    let response = skill_list_response(Some(&catalog_ref), Some(&catalog), &[&activation]);

    assert_eq!(response.catalog_ref.as_deref(), Some(catalog_ref.as_str()));
    assert_eq!(response.skills.len(), 2);
    assert_eq!(response.skills[0].skill_id, "skill:review");
    assert!(response.skills[0].enabled);
    assert!(response.skills[0].active);
    assert_eq!(response.skills[1].skill_id, "skill:deploy");
    assert!(!response.skills[1].enabled);
    assert!(!response.skills[1].active);
}

#[test]
fn skill_active_response_exposes_activation_sources_and_metadata() {
    let catalog_ref = BlobRef::from_bytes(b"catalog");
    let context_ref = BlobRef::from_bytes(b"direct-body");
    let catalog = test_skill_catalog(
        &catalog_ref,
        vec![
            test_skill_metadata("skill:review", "review", true),
            test_skill_metadata("skill:deploy", "deploy", true),
        ],
    );
    let direct = direct_activation(
        "skill:review",
        &catalog_ref,
        &context_ref,
        ApiSkillActivationScope::Session,
    );
    let run_scoped = direct_activation(
        "skill:deploy",
        &catalog_ref,
        &BlobRef::from_bytes(b"deploy-body"),
        ApiSkillActivationScope::Run,
    );

    let response =
        skill_active_response(Some(&catalog_ref), Some(&catalog), &[&direct, &run_scoped]);

    assert_eq!(response.catalog_ref.as_deref(), Some(catalog_ref.as_str()));
    assert_eq!(response.activations.len(), 2);
    assert_eq!(response.activations[0].name.as_deref(), Some("review"));
    assert_eq!(
        response.activations[0].source,
        ApiSkillActivationSource::DirectContext {
            context_ref: context_ref.as_str().to_owned()
        }
    );
    assert_eq!(
        response.activations[0].scope,
        ApiSkillActivationScope::Session
    );
    assert_eq!(response.activations[1].name.as_deref(), Some("deploy"));
    assert_eq!(response.activations[1].scope, ApiSkillActivationScope::Run);
}

#[test]
fn active_skill_ids_after_upsert_replaces_same_skill_only() {
    let catalog_ref = BlobRef::from_bytes(b"catalog");
    let other = direct_activation(
        "skill:deploy",
        &catalog_ref,
        &BlobRef::from_bytes(b"deploy-body"),
        ApiSkillActivationScope::Run,
    );
    let mut state = engine::CoreAgentState::new();
    state.context.entries = vec![
        direct_activation(
            "skill:review",
            &catalog_ref,
            &BlobRef::from_bytes(b"old-body"),
            ApiSkillActivationScope::Run,
        ),
        other,
    ];

    let ids = active_skill_ids_after_upsert(&state, SkillId::new("skill:review"));

    assert_eq!(
        ids,
        vec![SkillId::new("skill:deploy"), SkillId::new("skill:review")]
    );
}

#[test]
fn active_skill_ids_after_remove_drops_selected_skill() {
    let catalog_ref = BlobRef::from_bytes(b"catalog");
    let review = direct_activation(
        "skill:review",
        &catalog_ref,
        &BlobRef::from_bytes(b"review-body"),
        ApiSkillActivationScope::Run,
    );
    let deploy = direct_activation(
        "skill:deploy",
        &catalog_ref,
        &BlobRef::from_bytes(b"deploy-body"),
        ApiSkillActivationScope::Session,
    );
    let mut state = engine::CoreAgentState::new();
    state.context.entries = vec![review, deploy];

    let remaining = active_skill_ids_after_remove(&state, &SkillId::new("skill:review"));

    assert_eq!(remaining, vec![SkillId::new("skill:deploy")]);
}

#[test]
fn mcp_link_materializes_remote_tool_patch() {
    let tool_name = ToolName::new("mcp_crm");
    let tools = BTreeMap::new();
    let record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    let draft = session_mcp_link_from_record(
        api::SessionMcpLinkParams {
            session_id: "session_1".to_owned(),
            server_id: "crm".to_owned(),
            tool_id: Some(tool_name.as_str().to_owned()),
            server_label: None,
            allowed_tools: Some(vec!["lookup_customer".to_owned()]),
            approval: Some(api::RemoteMcpApprovalPolicy::Never),
            defer_loading: Some(true),
            auth_grant_id: None,
        },
        &record,
    )
    .expect("materialize MCP link draft");

    let patch = apply_session_mcp_link(&tools, draft).expect("apply MCP link");
    let tools = patch.apply_to(&tools).expect("apply MCP patch");

    let tool = tools.get(&tool_name).expect("MCP tool");
    let engine::ToolKind::RemoteMcp(spec) = &tool.kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.server_label, "crm");
    assert_eq!(spec.allowed_tools, Some(vec!["lookup_customer".to_owned()]));
    assert_eq!(spec.approval, engine::RemoteMcpApprovalPolicy::Never);
    assert_eq!(linked_session_mcp(&tools)[0].tool_id, tool_name.as_str());
}

#[test]
fn standard_toolset_patch_preserves_remote_mcp_links() {
    let remote_tool_name = ToolName::new("mcp_crm");
    let old_tool_name = ToolName::new("old_tool");
    let new_tool_name = ToolName::new("new_tool");
    let active = BTreeMap::from([
        (
            remote_tool_name.clone(),
            test_remote_mcp_tool(remote_tool_name.clone()),
        ),
        (
            old_tool_name.clone(),
            test_function_tool(old_tool_name.clone()),
        ),
    ]);
    let toolset = ResolvedToolset {
        tools: BTreeMap::from([(
            new_tool_name.clone(),
            test_function_tool(new_tool_name.clone()),
        )]),
        documents: Vec::new(),
        catalog: tools::runtime::ToolCatalog::new(),
        provider_request_defaults_patch: tools::toolset::ProviderRequestDefaultsPatch::default(),
    };

    let patch = super::vfs_api::standard_toolset_patch(&active, toolset);
    let tools = patch.apply_to(&active).expect("apply standard tool patch");

    assert!(tools.contains_key(&remote_tool_name));
    assert!(!tools.contains_key(&old_tool_name));
    assert!(tools.contains_key(&new_tool_name));
}

#[test]
fn session_tools_update_patch_accepts_remote_mcp_tool() {
    let update = api::SessionToolsUpdateInput::Patch {
        upsert: vec![api_remote_mcp_tool("mcp_crm", "crm")],
        remove: Vec::new(),
    };

    let tools_api::CoreToolUpdate::Patch(patch) =
        tools_api::core_tool_update_from_api(update).expect("convert tool update")
    else {
        panic!("expected tool patch");
    };
    patch
        .validate_for(&BTreeMap::new())
        .expect("validate tool patch");

    assert_eq!(patch.upsert.len(), 1);
    assert_eq!(patch.upsert[0].name, ToolName::new("mcp_crm"));
    let engine::ToolKind::RemoteMcp(remote_mcp) = &patch.upsert[0].kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(remote_mcp.server_label, "crm");
}

#[test]
fn session_tools_update_replace_rejects_duplicate_tool_ids() {
    let update = api::SessionToolsUpdateInput::Replace {
        tools: vec![
            api_remote_mcp_tool("mcp_crm", "crm"),
            api_remote_mcp_tool("mcp_crm", "crm_alt"),
        ],
    };

    let error = tools_api::core_tool_update_from_api(update).expect_err("duplicate tool id");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn prompt_report_ref_reads_prompt_provider_metadata() {
    let prompt_ref = BlobRef::from_bytes(b"prompt");
    let report_ref = BlobRef::from_bytes(b"prompt-report");
    let input = tools::prompts::prompt_source_instructions_context_input(
        prompt_ref,
        report_ref.clone(),
        "prompt instructions: instructions.md",
    );
    let entry = ContextEntry {
        entry_id: engine::ContextEntryId::new(1),
        key: Some(ContextEntryKey::new(format!(
            "{}.0000.project",
            tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
        ))),
        kind: input.kind,
        source: engine::ContextEntrySource::ContextEdit,
        content_ref: input.content_ref,
        media_type: input.media_type,
        preview: input.preview,
        provider_kind: input.provider_kind,
        provider_item_id: input.provider_item_id,
        token_estimate: input.token_estimate,
    };
    let mut state = engine::CoreAgentState::new();
    state.context.entries = vec![entry];

    let active_entries = active_prompt_context_entries(&state);

    assert_eq!(active_entries.len(), 1);
    assert_eq!(
        prompt_report_ref(active_entries[0]).expect("prompt report ref"),
        Some(report_ref)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn read_skill_doc_for_activation_reads_cataloged_vfs_bytes() {
    let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
    let skill_body = "---\nname: review\ndescription: Use when testing review.\n---\nsecret body\n";
    let snapshot = vfs::create_inline_snapshot(
        blobs.as_ref(),
        vfs::CreateInlineSnapshotRequest::new(vec![
            vfs::InlineFile::new("review/SKILL.md", skill_body.as_bytes().to_vec()).unwrap(),
        ]),
    )
    .await
    .expect("create skill snapshot");
    let workspace_store = Arc::new(EmptyWorkspaceStore);
    let mount = VfsMountRecord {
        session_id: SessionId::new("session_1"),
        mount_path: VfsPath::parse("/skills/system").unwrap(),
        source: VfsMountSource::Snapshot {
            snapshot_ref: snapshot.snapshot_ref.clone(),
        },
        access: VfsMountAccess::ReadOnly,
    };
    let skill = test_skill_metadata_with_snapshot(
        "skill:review",
        "review",
        true,
        snapshot.snapshot_ref.clone(),
    );

    let body = read_skill_doc_for_activation_from_vfs(blobs, workspace_store, vec![mount], &skill)
        .await
        .expect("read skill doc");

    assert_eq!(body, skill_body);
}

#[tokio::test(flavor = "current_thread")]
async fn read_skill_doc_for_activation_rejects_host_locations() {
    let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
    let workspace_store = Arc::new(EmptyWorkspaceStore);
    let mut skill = test_skill_metadata("skill:host", "host", true);
    skill.location = SkillLocation::HostFilesystem {
        target: engine::ToolExecutionTarget::new("host", "vm-1"),
        root_path: "/skills".to_owned(),
        skill_dir_path: "/skills/host".to_owned(),
        skill_doc_path: "/skills/host/SKILL.md".to_owned(),
    };

    let error = read_skill_doc_for_activation_from_vfs(blobs, workspace_store, Vec::new(), &skill)
        .await
        .expect_err("host location should not read through VFS");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn session_start_config_maps_reasoning_and_max_output_tokens() {
    let mut config = default_session_config(openai_model());

    apply_generation_config(
        &mut config,
        Some(GenerationConfig {
            max_output_tokens: Some(2048),
            reasoning_effort: Some(ReasoningEffort::High),
            tool_choice: None,
        }),
    )
    .expect("apply config");

    assert_eq!(config.turn.max_output_tokens, Some(2048));
    let ProviderRequestDefaults::OpenAiResponses(defaults) = config.turn.provider_request_defaults
    else {
        panic!("expected OpenAI Responses defaults");
    };
    let reasoning = defaults.reasoning.expect("reasoning");
    assert_eq!(reasoning.effort.as_deref(), Some("high"));
    assert_eq!(reasoning.summary.as_deref(), Some("auto"));
}

#[test]
fn session_start_config_maps_tool_choice() {
    let mut config = default_session_config(openai_model());

    apply_generation_config(
        &mut config,
        Some(GenerationConfig {
            max_output_tokens: None,
            reasoning_effort: None,
            tool_choice: Some(ToolChoiceConfig {
                mode: ToolChoiceModeConfig::Specific {
                    tool_id: "web_fetch".to_owned(),
                },
                disable_parallel_tool_use: Some(true),
            }),
        }),
    )
    .expect("apply config");

    let tool_choice = config.turn.tool_choice.expect("tool choice");
    assert_eq!(tool_choice.disable_parallel_tool_use, Some(true));
    assert_eq!(
        tool_choice.mode,
        engine::ToolChoiceMode::Specific {
            tool_name: ToolName::new("web_fetch")
        }
    );
}

#[test]
fn session_start_config_maps_provider_triggered_compaction() {
    let mut config = default_session_config(openai_model());

    apply_context_config(
        &mut config.context,
        Some(ApiContextConfigInput {
            compaction: Some(CompactionPolicyInput::ProviderTriggered {
                compact_threshold_tokens: Some(120_000),
            }),
            ..ApiContextConfigInput::default()
        }),
    );

    assert_eq!(
        config.context.compaction,
        Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens: Some(120_000)
        })
    );
}

#[test]
fn session_start_config_maps_provider_standalone_compaction() {
    let mut config = default_session_config(openai_model());

    apply_context_config(
        &mut config.context,
        Some(ApiContextConfigInput {
            compaction: Some(CompactionPolicyInput::ProviderStandalone {
                compact_threshold_tokens: Some(120_000),
                target_tokens: Some(80_000),
            }),
            ..ApiContextConfigInput::default()
        }),
    );

    assert_eq!(
        config.context.compaction,
        Some(CompactionPolicy::ProviderStandalone {
            compact_threshold_tokens: Some(120_000),
            target_tokens: Some(80_000),
        })
    );
}

#[test]
fn run_start_config_maps_model_and_generation_overrides() {
    let session_config = default_session_config(openai_model());
    let mut run_config = RunConfig::default();

    apply_run_start_config(
        &mut run_config,
        &session_config,
        Some(RunStartConfig {
            model: Some(ModelConfig {
                provider_id: "openai".to_owned(),
                api_kind: "openai:responses".to_owned(),
                model: "gpt-5.5-mini".to_owned(),
            }),
            generation: Some(GenerationConfig {
                max_output_tokens: Some(1024),
                reasoning_effort: Some(ReasoningEffort::Medium),
                tool_choice: None,
            }),
            limits: None,
        }),
    )
    .expect("apply run config");

    assert_eq!(
        run_config
            .model_override
            .as_ref()
            .map(|model| model.model.as_str()),
        Some("gpt-5.5-mini")
    );
    assert_eq!(run_config.max_output_tokens, Some(1024));
    let ProviderRequestDefaults::OpenAiResponses(defaults) = run_config
        .provider_request_defaults
        .expect("request defaults")
    else {
        panic!("expected OpenAI Responses defaults");
    };
    assert_eq!(
        defaults.reasoning.expect("reasoning").effort.as_deref(),
        Some("medium")
    );
    assert!(run_config.tool_choice.is_none());
}

#[test]
fn run_start_config_maps_tool_choice() {
    let session_config = default_session_config(openai_model());
    let mut run_config = session_config.run.clone();

    apply_run_start_config(
        &mut run_config,
        &session_config,
        Some(RunStartConfig {
            model: None,
            generation: Some(GenerationConfig {
                max_output_tokens: None,
                reasoning_effort: None,
                tool_choice: Some(ToolChoiceConfig {
                    mode: ToolChoiceModeConfig::RequiredAny,
                    disable_parallel_tool_use: None,
                }),
            }),
            limits: None,
        }),
    )
    .expect("apply run config");

    assert_eq!(
        run_config.tool_choice.expect("tool choice").mode,
        engine::ToolChoiceMode::RequiredAny
    );
}

#[test]
fn web_search_defaults_on_for_openai_responses_sessions() {
    let config = default_session_config(openai_model());

    assert!(effective_web_search_enabled(&config));
}

#[test]
fn web_search_can_be_disabled_in_session_tools_config() {
    let mut config = default_session_config(openai_model());
    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: Some(false),
            web_fetch: None,
            host: None,
        }),
    );

    assert!(!effective_web_search_enabled(&config));
}

#[test]
fn web_fetch_defaults_on_and_can_be_disabled() {
    let mut config = default_session_config(openai_model());

    assert!(effective_web_fetch_enabled(&config));

    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: None,
            web_fetch: Some(false),
            host: None,
        }),
    );

    assert!(!effective_web_fetch_enabled(&config));
}

#[test]
fn web_search_rejects_explicit_enable_for_non_openai_responses() {
    let mut config = default_session_config(ModelSelection {
        api_kind: ProviderApiKind::AnthropicMessages,
        provider_id: "anthropic".to_owned(),
        model: "claude-test".to_owned(),
        options: ModelProviderOptions::None,
    });
    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: Some(true),
            web_fetch: None,
            host: None,
        }),
    );

    let error = config
        .validate_provider_compatibility()
        .expect_err("web search enable should reject Anthropic");

    assert!(matches!(
        error,
        engine::DomainError::ProviderCompatibility(_)
    ));
}

#[test]
fn host_tools_default_to_edit_for_sessions() {
    let config = default_session_config(openai_model());

    assert_eq!(effective_host_tool_mode(&config), HostToolMode::Edit);
}

#[test]
fn host_tools_can_be_configured_read_only_or_none() {
    let mut config = default_session_config(openai_model());
    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: None,
            web_fetch: None,
            host: Some(api::HostToolMode::ReadOnly),
        }),
    );

    assert_eq!(effective_host_tool_mode(&config), HostToolMode::ReadOnly);

    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: None,
            web_fetch: None,
            host: Some(api::HostToolMode::None),
        }),
    );

    assert_eq!(effective_host_tool_mode(&config), HostToolMode::None);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_preserves_single_text_ref() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text("hello from cas").await;

    let input = run_input_from_api(
        &store,
        &[InputItem::TextRef {
            blob_ref: blob_ref.as_str().to_owned(),
        }],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 1);
    assert_eq!(input[0].content_ref, blob_ref);
    assert_eq!(
        input[0].kind,
        engine::ContextEntryKind::Message {
            role: engine::ContextMessageRole::User,
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_stores_text_and_preserves_refs() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text(" second ").await;

    let input = run_input_from_api(
        &store,
        &[
            InputItem::Text {
                text: " first ".to_owned(),
            },
            InputItem::TextRef {
                blob_ref: blob_ref.as_str().to_owned(),
            },
        ],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 2);
    assert_ne!(input[0].content_ref, blob_ref);
    assert_eq!(input[1].content_ref, blob_ref);
    assert_eq!(
        store
            .read_text(&input[0].content_ref)
            .await
            .expect("stored input"),
        "first"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blob_api_helpers_put_get_and_check_many() {
    let store = engine::storage::InMemoryBlobStore::new();

    let put = put_blobs(
        &store,
        BlobPutManyParams {
            blobs: vec![
                BlobPutParams {
                    bytes_base64: BASE64.encode(b"hello"),
                },
                BlobPutParams {
                    bytes_base64: BASE64.encode(b"world"),
                },
            ],
        },
    )
    .await
    .expect("put blobs");
    assert_eq!(put.blobs.len(), 2);
    assert_eq!(put.blobs[0].bytes, 5);

    let has = has_blobs(
        &store,
        BlobHasManyParams {
            blob_refs: vec![
                put.blobs[0].blob_ref.clone(),
                BlobRef::from_bytes(b"missing").as_str().to_owned(),
            ],
        },
    )
    .await
    .expect("has blobs");
    assert_eq!(
        has.blobs.iter().map(|item| item.exists).collect::<Vec<_>>(),
        vec![true, false]
    );

    let read = get_blob(
        &store,
        BlobGetParams {
            blob_ref: put.blobs[1].blob_ref.clone(),
        },
    )
    .await
    .expect("get blob");
    assert_eq!(read.bytes_base64, BASE64.encode(b"world"));
}

#[tokio::test(flavor = "current_thread")]
async fn vfs_snapshot_api_helpers_commit_and_read_manifest() {
    let store = engine::storage::InMemoryBlobStore::new();
    let snapshot = vfs::create_inline_snapshot(
        &store,
        vfs::CreateInlineSnapshotRequest::new(vec![
            vfs::InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
        ]),
    )
    .await
    .expect("create snapshot");
    let manifest = serde_json::to_value(snapshot.manifest).expect("manifest json");

    let committed = commit_vfs_snapshot(
        &store,
        VfsSnapshotCommitParams {
            manifest: manifest.clone(),
        },
    )
    .await
    .expect("commit snapshot");
    assert_eq!(committed.files, 1);
    assert_eq!(committed.bytes, 6);

    let read = read_vfs_snapshot(
        &store,
        VfsSnapshotReadParams {
            snapshot_ref: committed.snapshot_ref,
        },
    )
    .await
    .expect("read snapshot");
    assert_eq!(read.manifest, manifest);
}

#[tokio::test(flavor = "current_thread")]
async fn vfs_snapshot_commit_rejects_missing_file_blob_refs() {
    let store = engine::storage::InMemoryBlobStore::new();
    let missing_ref = BlobRef::from_bytes(b"missing");
    let manifest = vfs::VfsSnapshotManifest {
        schema_version: vfs::VFS_SNAPSHOT_SCHEMA_VERSION.to_owned(),
        root: vfs::VfsDirectory {
            entries: BTreeMap::from([(
                "missing.txt".to_owned(),
                vfs::VfsEntry::File(vfs::VfsFile {
                    blob_ref: missing_ref,
                    size_bytes: 7,
                    media_type: None,
                    executable: false,
                }),
            )]),
        },
        totals: vfs::VfsTotals { files: 1, bytes: 7 },
    };

    let error = commit_vfs_snapshot(
        &store,
        VfsSnapshotCommitParams {
            manifest: serde_json::to_value(manifest).expect("manifest json"),
        },
    )
    .await
    .expect_err("missing blob should fail");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    assert!(error.message.contains("missing blob"));
}

fn failure(kind: AgentAdmissionFailureKind) -> AgentAdmissionFailure {
    AgentAdmissionFailure {
        submission_id: Some(SubmissionId::new("submit_test")),
        kind,
        message: "admission failed".to_owned(),
    }
}

fn openai_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: "gpt-5.5".to_owned(),
        options: ModelProviderOptions::None,
    }
}

fn test_skill_catalog(_catalog_ref: &BlobRef, skills: Vec<SkillMetadata>) -> SkillCatalogSnapshot {
    SkillCatalogSnapshot::new(None, skills, Vec::new())
}

fn test_skill_metadata(skill_id: &str, name: &str, enabled: bool) -> SkillMetadata {
    let snapshot_ref = BlobRef::from_bytes(b"skills-snapshot");
    test_skill_metadata_with_snapshot(skill_id, name, enabled, snapshot_ref)
}

fn test_skill_metadata_with_snapshot(
    skill_id: &str,
    name: &str,
    enabled: bool,
    snapshot_ref: BlobRef,
) -> SkillMetadata {
    SkillMetadata {
        skill_id: SkillId::new(skill_id),
        name: name.to_owned(),
        description: format!("Use when testing {name}."),
        short_description: Some(format!("{name} skill")),
        source: tools::skills::SkillSource::Snapshot {
            root_id: "system".to_owned(),
            snapshot_ref: snapshot_ref.clone(),
        },
        scope: tools::skills::SkillScope::Global,
        target: None,
        enabled,
        trust: tools::skills::SkillTrustLevel::System,
        interface: None,
        dependencies: tools::skills::SkillDependencies::default(),
        location: SkillLocation::MountedSnapshot {
            source_snapshot_ref: snapshot_ref,
            source_mount_path: VfsPath::parse("/skills/system").unwrap(),
            skill_dir_path: VfsPath::parse(format!("/skills/system/{name}")).unwrap(),
            skill_doc_path: VfsPath::parse(format!("/skills/system/{name}/SKILL.md")).unwrap(),
        },
        skill_doc_ref: None,
    }
}

fn direct_activation(
    skill_id: &str,
    catalog_ref: &BlobRef,
    context_ref: &BlobRef,
    scope: ApiSkillActivationScope,
) -> ContextEntry {
    let skill_id = SkillId::new(skill_id);
    let input = skill_activation_context_input(
        skill_id.clone(),
        catalog_ref.clone(),
        context_ref.clone(),
        scope,
        None,
    );
    ContextEntry {
        entry_id: engine::ContextEntryId::new(1),
        key: Some(skill_activation_context_key(&skill_id)),
        kind: input.kind,
        source: engine::ContextEntrySource::ContextEdit,
        content_ref: input.content_ref,
        media_type: input.media_type,
        preview: input.preview,
        provider_kind: input.provider_kind,
        provider_item_id: input.provider_item_id,
        token_estimate: input.token_estimate,
    }
}

fn test_mcp_server_record(
    server_id: &str,
    status: mcp_registry::McpServerStatus,
) -> mcp_registry::McpServerRecord {
    mcp_registry::CreateMcpServerRecord {
        server_id: mcp_registry::McpServerId::new(server_id),
        display_name: Some(format!("{server_id} MCP")),
        server_url: format!("https://{server_id}.example.com/mcp"),
        transport: mcp_registry::RemoteMcpTransport::Auto,
        default_server_label: server_id.to_owned(),
        description: None,
        allowed_tools: None,
        approval_default: mcp_registry::McpApprovalPolicy::ProviderDefault,
        defer_loading_default: None,
        auth_policy: mcp_registry::McpServerAuthPolicy::None,
        status,
        created_at_ms: 1,
    }
    .into_record()
}

fn api_remote_mcp_tool(tool_id: &str, server_label: &str) -> api::ToolView {
    api::ToolView {
        tool_id: tool_id.to_owned(),
        kind: api::ToolKindView::RemoteMcp {
            server_label: server_label.to_owned(),
            server_url: format!("https://{server_label}.example.com/mcp"),
            description_ref: None,
            allowed_tools: None,
            approval: api::RemoteMcpApprovalPolicy::ProviderDefault,
            defer_loading: None,
            auth_ref: None,
        },
        parallelism: api::ToolParallelismView::ParallelSafe,
        target_requirement: api::ToolTargetRequirementView::None,
    }
}

fn test_remote_mcp_tool(tool_name: ToolName) -> engine::ToolSpec {
    engine::ToolSpec {
        name: tool_name,
        kind: engine::ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
            server_label: "crm".to_owned(),
            server_url: "https://crm.example.com/mcp".to_owned(),
            description_ref: None,
            allowed_tools: None,
            approval: engine::RemoteMcpApprovalPolicy::ProviderDefault,
            defer_loading: None,
            auth_ref: None,
        }),
        parallelism: engine::ToolParallelism::ParallelSafe,
        target_requirement: engine::ToolTargetRequirement::None,
    }
}

fn test_function_tool(tool_name: ToolName) -> engine::ToolSpec {
    engine::ToolSpec {
        name: tool_name,
        kind: engine::ToolKind::Function(engine::FunctionToolSpec {
            model_name: None,
            description_ref: None,
            input_schema_ref: BlobRef::from_bytes(b"schema"),
            output_schema_ref: None,
            strict: Some(true),
            provider_options_ref: None,
        }),
        parallelism: engine::ToolParallelism::Exclusive,
        target_requirement: engine::ToolTargetRequirement::None,
    }
}

struct EmptyWorkspaceStore;

#[async_trait]
impl VfsWorkspaceStore for EmptyWorkspaceStore {
    async fn create_workspace(
        &self,
        _record: vfs::CreateVfsWorkspaceRecord,
    ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
        Err(workspace_not_found("create"))
    }

    async fn read_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
        Err(workspace_not_found(workspace_id.as_str()))
    }

    async fn compare_and_set_head(
        &self,
        _request: vfs::CompareAndSetVfsWorkspaceHead,
    ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
        Err(workspace_not_found("compare_and_set"))
    }

    async fn delete_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
        Err(workspace_not_found(workspace_id.as_str()))
    }
}

fn workspace_not_found(id: &str) -> vfs::VfsCatalogError {
    vfs::VfsCatalogError::NotFound {
        kind: "workspace",
        id: id.to_owned(),
    }
}
