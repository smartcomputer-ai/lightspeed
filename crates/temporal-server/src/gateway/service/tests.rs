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
    assert_eq!(
        map_admission_failure_to_api_error(&failure(
            AgentAdmissionFailureKind::UnsupportedAudioMime
        ))
        .kind,
        AgentApiErrorKind::UnsupportedAudioMime
    );
    assert_eq!(
        map_admission_failure_to_api_error(&failure(AgentAdmissionFailureKind::AudioBlobMissing))
            .kind,
        AgentApiErrorKind::InvalidRequest
    );
    assert_eq!(
        map_admission_failure_to_api_error(&failure(
            AgentAdmissionFailureKind::TranscriptionFailure
        ))
        .kind,
        AgentApiErrorKind::TranscriptionFailure
    );
    assert_eq!(
        map_admission_failure_to_api_error(&failure(AgentAdmissionFailureKind::TranscodeFailure))
            .kind,
        AgentApiErrorKind::TranscodeFailure
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
fn environment_view_maps_record_and_active_status() {
    let record = test_environment_record(
        "local",
        tools::environment::projection::EnvironmentStatus::Ready,
    );

    let view = environments::session_environment_view(&record, Some("local"));

    assert_eq!(view.env_id, "local");
    assert_eq!(view.kind, SessionEnvironmentKindView::AttachedHost);
    assert_eq!(view.status, SessionEnvironmentStatusView::Ready);
    assert!(view.capabilities.process_exec);
    assert_eq!(view.exec_target.expect("exec target").namespace, "env");
    assert_eq!(view.cwd.as_deref(), Some("/workspace"));
    assert!(view.active);
}

#[test]
fn environment_activation_lowers_to_default_env_target_command() {
    let record = test_environment_record(
        "local",
        tools::environment::projection::EnvironmentStatus::Ready,
    );
    let target =
        environments::activation_target_for_environment_record(&record).expect("activation target");

    let command = environments::activate_environment_command(target.clone());

    assert!(matches!(
        command,
        CoreAgentCommand::SetDefaultToolTarget { target: actual } if actual == target
    ));
}

#[test]
fn environment_deactivation_lowers_to_clear_env_target_command() {
    let command = environments::deactivate_environment_command();

    assert!(matches!(
        command,
        CoreAgentCommand::ClearDefaultToolTarget { namespace } if namespace == "env"
    ));
}

#[test]
fn invalid_environment_id_maps_to_invalid_request() {
    let error = environments::parse_environment_id("bad id".to_owned())
        .expect_err("invalid environment id");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn inactive_environment_cannot_be_activation_target() {
    let record = test_environment_record(
        "local",
        tools::environment::projection::EnvironmentStatus::Detached,
    );

    let error = environments::activation_target_for_environment_record(&record)
        .expect_err("detached environment");

    assert_eq!(error.kind, AgentApiErrorKind::Rejected);
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
        None,
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

fn test_auth_grant_record(
    grant_id: &str,
    provider_kind: auth_registry::AuthProviderKind,
    status: auth_registry::AuthGrantStatus,
    audience: Option<&str>,
) -> auth_registry::AuthGrantRecord {
    auth_registry::CreateAuthGrantRecord {
        grant_id: auth_registry::AuthGrantId::new(grant_id),
        provider_id: "static".to_owned(),
        provider_kind,
        principal: auth_registry::PrincipalRef::universe_default(),
        display_name: None,
        subject_hint: None,
        scopes: Vec::new(),
        audience: audience.map(str::to_owned),
        access_token_secret: Some(auth_registry::SecretId::new("authsec_1")),
        refresh_token_secret: None,
        oauth_client: None,
        expires_at_ms: None,
        status,
        metadata: serde_json::Value::Object(Default::default()),
        created_at_ms: 1,
    }
    .into_record()
}

fn mcp_link_params_with_grant(grant_id: &str) -> api::SessionMcpLinkParams {
    api::SessionMcpLinkParams {
        session_id: "session_1".to_owned(),
        server_id: "crm".to_owned(),
        tool_id: Some("mcp_crm".to_owned()),
        server_label: None,
        allowed_tools: None,
        approval: None,
        defer_loading: None,
        auth_grant_id: Some(grant_id.to_owned()),
    }
}

#[test]
fn mcp_link_with_grant_materializes_auth_ref_for_bearer_server() {
    let mut record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    record.auth_policy = mcp_registry::McpServerAuthPolicy::RequiredBearer;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth_registry::AuthProviderKind::StaticBearer,
        auth_registry::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let draft = session_mcp_link_from_record(
        mcp_link_params_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect("materialize MCP link draft with grant");

    assert_eq!(
        draft.spec.auth_ref,
        Some(engine::SecretRef {
            namespace: "auth_grant".to_owned(),
            id: "authgrant_1".to_owned(),
        })
    );
}

#[test]
fn mcp_link_rejects_revoked_grant() {
    let mut record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    record.auth_policy = mcp_registry::McpServerAuthPolicy::RequiredBearer;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth_registry::AuthProviderKind::StaticBearer,
        auth_registry::AuthGrantStatus::Revoked,
        None,
    );

    let error = session_mcp_link_from_record(
        mcp_link_params_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("revoked grant must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_kind_incompatible_with_auth_policy() {
    let mut record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    record.auth_policy = mcp_registry::McpServerAuthPolicy::RequiredOAuth {
        resource: "https://crm.example.com".to_owned(),
        scopes_default: Vec::new(),
        protected_resource_metadata_url: None,
        authorization_server: None,
    };
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth_registry::AuthProviderKind::StaticBearer,
        auth_registry::AuthGrantStatus::Active,
        None,
    );

    let error = session_mcp_link_from_record(
        mcp_link_params_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("bearer grant must not satisfy OAuth policy");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_audience_that_does_not_cover_server() {
    let mut record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    record.auth_policy = mcp_registry::McpServerAuthPolicy::OptionalBearer;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth_registry::AuthProviderKind::StaticBearer,
        auth_registry::AuthGrantStatus::Active,
        Some("https://other.example.com"),
    );

    let error = session_mcp_link_from_record(
        mcp_link_params_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("audience mismatch must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_for_no_auth_server() {
    let record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth_registry::AuthProviderKind::StaticBearer,
        auth_registry::AuthGrantStatus::Active,
        None,
    );

    let error = session_mcp_link_from_record(
        mcp_link_params_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("grant on no-auth server must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
}

#[test]
fn mcp_link_requires_grant_for_required_auth_server() {
    let mut record = test_mcp_server_record("crm", mcp_registry::McpServerStatus::Active);
    record.auth_policy = mcp_registry::McpServerAuthPolicy::RequiredBearer;
    let mut params = mcp_link_params_with_grant("authgrant_1");
    params.auth_grant_id = None;

    let error = session_mcp_link_from_record(params, &record, None)
        .expect_err("missing grant must be rejected for required auth");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
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
        provider_params_patch: tools::toolset::ProviderParamsPatch::default(),
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
    let params = config.turn.provider_params.expect("provider params");
    assert_eq!(params.api_kind, engine::ProviderApiKind::OpenAiResponses);
    assert_eq!(params.body["reasoning"]["effort"], "high");
    assert_eq!(params.body["reasoning"]["summary"], "auto");
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
    let params = run_config.provider_params.expect("provider params");
    assert_eq!(params.api_kind, engine::ProviderApiKind::OpenAiResponses);
    assert_eq!(params.body["reasoning"]["effort"], "medium");
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
fn existing_run_submission_rejects_completed_duplicate_with_different_input() {
    let submission_id = SubmissionId::new("submit_retry");
    let run_config = RunConfig::default();
    let original_input = vec![test_user_message_input(BlobRef::from_bytes(b"original"))];
    let changed_input = vec![test_user_message_input(BlobRef::from_bytes(b"changed"))];
    let mut state = engine::CoreAgentState::new();
    state.runs.completed.push(engine::RunRecord {
        run_id: RunId::new(7),
        status: RunStatus::Completed,
        submission_id: Some(submission_id.clone()),
        submission_digest: Some(run_submission_digest(&original_input, &run_config)),
        output_ref: None,
        failure: None,
    });

    assert!(matches!(
        existing_run_submission(&state, &submission_id, &changed_input, &run_config),
        Some(ExistingRunSubmission::Reject)
    ));
    let Some(ExistingRunSubmission::ReturnRun { run_id, status }) =
        existing_run_submission(&state, &submission_id, &original_input, &run_config)
    else {
        panic!("identical duplicate should return existing completed run");
    };
    assert_eq!(run_id, RunId::new(7));
    assert_eq!(status, RunStatus::Completed);
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
            filesystem: None,
            messaging: None,
            fleet: None,
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
            filesystem: None,
            messaging: None,
            fleet: None,
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
    });
    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: Some(true),
            web_fetch: None,
            filesystem: None,
            messaging: None,
            fleet: None,
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
fn filesystem_tools_default_to_edit_for_sessions() {
    let config = default_session_config(openai_model());

    assert_eq!(
        effective_filesystem_tool_mode(&config),
        FilesystemToolMode::Edit
    );
}

#[test]
fn filesystem_tools_can_be_configured_read_only_or_none() {
    let mut config = default_session_config(openai_model());
    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: None,
            web_fetch: None,
            filesystem: Some(api::FilesystemToolMode::ReadOnly),
            messaging: None,
            fleet: None,
        }),
    );

    assert_eq!(
        effective_filesystem_tool_mode(&config),
        FilesystemToolMode::ReadOnly
    );

    apply_tool_config(
        &mut config.tools,
        Some(ToolConfigInput {
            web_search: None,
            web_fetch: None,
            filesystem: Some(api::FilesystemToolMode::None),
            messaging: None,
            fleet: None,
        }),
    );

    assert_eq!(
        effective_filesystem_tool_mode(&config),
        FilesystemToolMode::None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_stores_text_as_user_message() {
    let store = engine::storage::InMemoryBlobStore::new();

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::Text {
            text: " [telegram] Alice (12:01): hi ".to_owned(),
        },
    )
    .await
    .expect("entry");

    assert_eq!(
        entry.kind,
        engine::ContextEntryKind::Message {
            role: engine::ContextMessageRole::User,
        }
    );
    assert_eq!(entry.media_type.as_deref(), Some("text/plain"));
    assert_eq!(
        store
            .read_text(&entry.content_ref)
            .await
            .expect("stored text"),
        "[telegram] Alice (12:01): hi"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_rejects_empty_text() {
    let store = engine::storage::InMemoryBlobStore::new();

    let error = context_entry_input_from_api(
        &store,
        &InputItem::Text {
            text: "   ".to_owned(),
        },
    )
    .await
    .expect_err("empty text must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_preserves_text_ref() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text("buffered room chatter").await;

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::TextRef {
            blob_ref: blob_ref.as_str().to_owned(),
        },
    )
    .await
    .expect("entry");

    assert_eq!(entry.content_ref, blob_ref);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_maps_image_media_to_user_message_entry() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store
        .put_bytes(vec![0x89, 0x50, 0x4e, 0x47])
        .await
        .expect("store image");

    let input = run_input_from_api(
        &store,
        &[
            InputItem::Text {
                text: "what is this?".to_owned(),
            },
            InputItem::Media {
                blob_ref: blob_ref.as_str().to_owned(),
                mime: "image/png".to_owned(),
                kind: api::MediaKind::Image,
                name: Some("photo.png".to_owned()),
            },
        ],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 2);
    let media = &input[1];
    assert_eq!(
        media.kind,
        engine::ContextEntryKind::Message {
            role: engine::ContextMessageRole::User,
        }
    );
    assert_eq!(media.content_ref, blob_ref);
    assert_eq!(media.media_type.as_deref(), Some("image/png"));
    assert_eq!(media.preview.as_deref(), Some("[image: photo.png]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_maps_document_media_to_user_message_entry() {
    let store = engine::storage::InMemoryBlobStore::new();
    let pdf_ref = store
        .put_bytes(b"%PDF-1.4 fake".to_vec())
        .await
        .expect("store pdf");
    let md_ref = store
        .put_bytes(b"# Notes".to_vec())
        .await
        .expect("store markdown");

    let input = run_input_from_api(
        &store,
        &[
            InputItem::Media {
                blob_ref: pdf_ref.as_str().to_owned(),
                mime: "application/pdf".to_owned(),
                kind: api::MediaKind::Document,
                name: Some("offer.pdf".to_owned()),
            },
            InputItem::Media {
                blob_ref: md_ref.as_str().to_owned(),
                mime: "text/markdown".to_owned(),
                kind: api::MediaKind::Document,
                name: Some("notes.md".to_owned()),
            },
        ],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 2);
    assert_eq!(input[0].media_type.as_deref(), Some("application/pdf"));
    assert_eq!(input[0].preview.as_deref(), Some("[document: offer.pdf]"));
    assert_eq!(input[1].media_type.as_deref(), Some("text/markdown"));
    assert_eq!(input[1].preview.as_deref(), Some("[document: notes.md]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_unsupported_document_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let docx = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                .to_owned(),
            kind: api::MediaKind::Document,
            name: None,
        }],
    )
    .await
    .expect_err("docx must be rejected");
    assert_eq!(docx.kind, AgentApiErrorKind::InvalidRequest);

    // Text documents must decode as UTF-8 at admission.
    let binary_ref = store
        .put_bytes(vec![0xff, 0xfe, 0x00])
        .await
        .expect("store binary blob");
    let binary = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: binary_ref.as_str().to_owned(),
            mime: "text/plain".to_owned(),
            kind: api::MediaKind::Document,
            name: None,
        }],
    )
    .await
    .expect_err("non-UTF-8 text document must be rejected");
    assert_eq!(binary.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_maps_audio_media_to_user_message_entry() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store
        .put_bytes(b"OggS fake voice note".to_vec())
        .await
        .expect("store audio");

    let input = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "audio/ogg".to_owned(),
            kind: api::MediaKind::Audio,
            name: Some("voice.ogg".to_owned()),
        }],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 1);
    assert_eq!(input[0].content_ref, blob_ref);
    assert_eq!(input[0].media_type.as_deref(), Some("audio/ogg"));
    assert_eq!(input[0].preview.as_deref(), Some("[audio: voice.ogg]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_unsupported_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let audio = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "audio/flac".to_owned(),
            kind: api::MediaKind::Audio,
            name: None,
        }],
    )
    .await
    .expect_err("unsupported audio mime must be rejected");
    assert_eq!(audio.kind, AgentApiErrorKind::UnsupportedAudioMime);

    let bad_mime = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "image/tiff".to_owned(),
            kind: api::MediaKind::Image,
            name: None,
        }],
    )
    .await
    .expect_err("unsupported image mime must be rejected");
    assert_eq!(bad_mime.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_accepts_transcodable_audio_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let input = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "audio/x-aac".to_owned(),
            kind: api::MediaKind::Audio,
            name: Some("clip.aac".to_owned()),
        }],
    )
    .await
    .expect("transcodable audio should be admitted");

    assert_eq!(input[0].content_ref, blob_ref);
    assert_eq!(input[0].media_type.as_deref(), Some("audio/aac"));
    assert_eq!(input[0].preview.as_deref(), Some("[audio: clip.aac]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_audio_over_byte_cap() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store
        .put_bytes(vec![0; 25 * 1024 * 1024 + 1])
        .await
        .expect("store large audio");

    let error = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "audio/ogg".to_owned(),
            kind: api::MediaKind::Audio,
            name: None,
        }],
    )
    .await
    .expect_err("oversized audio must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::AudioBlobTooLarge);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_missing_audio_blob() {
    let store = engine::storage::InMemoryBlobStore::new();

    let error = run_input_from_api(
        &store,
        &[InputItem::Media {
            blob_ref: BlobRef::from_bytes(b"missing-audio").as_str().to_owned(),
            mime: "audio/ogg".to_owned(),
            kind: api::MediaKind::Audio,
            name: None,
        }],
    )
    .await
    .expect_err("missing audio blob must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_rejects_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let error = context_entry_input_from_api(
        &store,
        &InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "image/png".to_owned(),
            kind: api::MediaKind::Image,
            name: None,
        },
    )
    .await
    .expect_err("media must be rejected in context/append");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
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
    }
}

fn test_user_message_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: engine::ContextEntryKind::Message {
            role: engine::ContextMessageRole::User,
        },
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
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

fn test_environment_record(
    env_id: &str,
    status: tools::environment::projection::EnvironmentStatus,
) -> tools::environment::projection::EnvironmentRecord {
    tools::environment::projection::EnvironmentRecord {
        env_id: env_id.to_owned(),
        kind: tools::environment::projection::EnvironmentKind::AttachedHost,
        capabilities: tools::environment::projection::EnvironmentCapabilities {
            fs_read: true,
            fs_write: true,
            process_exec: true,
            process_stdin: true,
            network: false,
            persistent: true,
            ..tools::environment::projection::EnvironmentCapabilities::default()
        },
        exec_target: Some(tools::targets::environment_target(env_id)),
        cwd: Some(FsPath::new("/workspace").expect("cwd")),
        status,
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

fn client_create_params() -> AuthClientCreateParams {
    serde_json::from_value(serde_json::json!({
        "clientId": "crm",
        "providerKind": "mcpOAuth",
        "authorizationEndpoint": "https://as.example.com/authorize",
        "tokenEndpoint": "https://as.example.com/token",
        "remoteClientId": "client-1",
        "clientSecret": "shh-secret",
        "audience": "https://crm.example.com/mcp"
    }))
    .expect("client create params")
}

#[test]
fn auth_client_drafts_encrypt_secret_and_default_to_basic_auth() {
    let draft = oauth_api::auth_client_create_draft(client_create_params(), 10)
        .expect("draft oauth client");

    let secret = draft.secret.expect("client secret drafted");
    assert_eq!(
        secret.secret_kind,
        auth_registry::SECRET_KIND_OAUTH_CLIENT_SECRET
    );
    assert_eq!(secret.value.expose(), "shh-secret");
    assert_eq!(draft.client.client_secret, Some(secret.secret_id.clone()));
    assert_eq!(
        draft.client.token_endpoint_auth_method,
        auth_registry::TokenEndpointAuthMethod::ClientSecretBasic
    );
    // Provider id defaults to the client id.
    assert_eq!(draft.client.provider_id, "crm");
}

#[test]
fn auth_client_drafts_without_secret_default_to_public_client() {
    let mut params = client_create_params();
    params.client_secret = None;

    let draft = oauth_api::auth_client_create_draft(params, 10).expect("draft oauth client");

    assert!(draft.secret.is_none());
    assert_eq!(
        draft.client.token_endpoint_auth_method,
        auth_registry::TokenEndpointAuthMethod::None
    );
}

#[test]
fn auth_client_drafts_reject_non_oauth_kinds() {
    let mut params = client_create_params();
    params.provider_kind = api::AuthProviderKind::StaticBearer;

    let error = oauth_api::auth_client_create_draft(params, 10)
        .expect_err("static bearer kind must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn mcp_oauth_client_drafts_require_an_audience() {
    let mut params = client_create_params();
    params.audience = None;

    let error = oauth_api::auth_client_create_draft(params, 10)
        .expect_err("mcp oauth without audience must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn oauth_redirect_uris_normalize_trailing_slashes() {
    assert_eq!(
        oauth_api::oauth_redirect_uri("http://127.0.0.1:18080"),
        "http://127.0.0.1:18080/auth/callback"
    );
    assert_eq!(
        oauth_api::oauth_redirect_uri("https://lightspeed.example.com/"),
        "https://lightspeed.example.com/auth/callback"
    );
}

#[test]
fn mcp_oauth_targets_come_from_oauth_policies_only() {
    let mut record = test_mcp_server_record("playground", mcp_registry::McpServerStatus::Active);
    record.auth_policy = mcp_registry::McpServerAuthPolicy::RequiredOAuth {
        resource: "https://playground.example.com/mcp".to_owned(),
        scopes_default: vec!["tools.run".to_owned()],
        protected_resource_metadata_url: Some(
            "https://playground.example.com/.well-known/oauth-protected-resource/mcp".to_owned(),
        ),
        authorization_server: Some("https://as.example.com".to_owned()),
    };

    let target = oauth_api::mcp_oauth_target_from_record(&record).expect("oauth target");

    assert_eq!(target.server_id, "playground");
    assert_eq!(target.server_url, "https://playground.example.com/mcp");
    assert_eq!(target.scopes_default, vec!["tools.run".to_owned()]);
    assert_eq!(
        target.authorization_server_hint.as_deref(),
        Some("https://as.example.com")
    );

    let mut bearer = test_mcp_server_record("bearer", mcp_registry::McpServerStatus::Active);
    bearer.auth_policy = mcp_registry::McpServerAuthPolicy::RequiredBearer;
    let error = oauth_api::mcp_oauth_target_from_record(&bearer)
        .expect_err("bearer servers cannot be logged into");
    assert_eq!(error.kind, AgentApiErrorKind::Rejected);
}

#[test]
fn cimd_config_requires_a_public_https_base_url() {
    assert!(oauth_api::cimd_config("http://127.0.0.1:18080").is_none());

    let cimd = oauth_api::cimd_config("https://lightspeed.example.com/").expect("cimd config");
    assert_eq!(
        cimd.client_id_url,
        "https://lightspeed.example.com/auth/client-metadata.json"
    );
}

#[test]
fn cimd_documents_declare_a_public_pkce_client() {
    let document = oauth_api::cimd_document("https://lightspeed.example.com");

    assert_eq!(
        document["client_id"],
        "https://lightspeed.example.com/auth/client-metadata.json"
    );
    assert_eq!(
        document["redirect_uris"][0],
        "https://lightspeed.example.com/auth/callback"
    );
    assert_eq!(document["token_endpoint_auth_method"], "none");
    assert_eq!(document["grant_types"][0], "authorization_code");
}

#[test]
fn auth_flow_views_carry_derived_status() {
    let record = auth_registry::CreateAuthFlowRecord {
        flow_id: auth_registry::AuthFlowId::new("authflow_1"),
        client_id: auth_registry::OAuthClientId::new("crm"),
        provider_id: "crm".to_owned(),
        provider_kind: auth_registry::AuthProviderKind::McpOAuth,
        principal: auth_registry::PrincipalRef::universe_default(),
        state_hash: auth_registry::state_hash("state-1"),
        pkce_verifier_secret: auth_registry::SecretId::new("authsec_pkce"),
        redirect_uri: "http://127.0.0.1:18080/auth/callback".to_owned(),
        scopes: Vec::new(),
        audience: Some("https://crm.example.com/mcp".to_owned()),
        expires_at_ms: 100,
        created_at_ms: 10,
    }
    .into_record();

    let pending = oauth_api::auth_flow_view(record.clone(), 50);
    assert_eq!(pending.status, api::AuthFlowStatus::Pending);
    assert!(pending.grant_id.is_none());

    let expired = oauth_api::auth_flow_view(record, 200);
    assert_eq!(expired.status, api::AuthFlowStatus::Expired);
}
