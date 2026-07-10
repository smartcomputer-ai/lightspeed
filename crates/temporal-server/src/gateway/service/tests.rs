use async_trait::async_trait;

use api::BlobPutItem;

use super::*;
use crate::gateway::service::prompts::{active_prompt_context_entries, prompt_report_ref};

#[test]
fn admission_failure_mapping_uses_gateway_error_kinds() {
    assert_eq!(
        map_admission_failure_to_api_error(&failure(AgentAdmissionFailureKind::RejectedCommand))
            .kind,
        AgentApiErrorKind::Rejected
    );
    let mut revision_conflict = failure(AgentAdmissionFailureKind::RejectedCommand);
    revision_conflict.rejection = Some(engine::CommandRejection::context_revision_conflict(1, 2));
    assert_eq!(
        map_admission_failure_to_api_error(&revision_conflict).kind,
        AgentApiErrorKind::Conflict
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
    let runtime = RuntimeEnvironment::new(
        record,
        tools::environment::EnvironmentToolContext::new(
            None,
            Arc::new(engine::storage::InMemoryBlobStore::new()),
        ),
    );

    let view = super::environments::session_environment_view(&runtime, Some("local"));

    assert_eq!(view.env_id, "local");
    assert_eq!(view.instance_id, "local");
    assert_eq!(view.state, SessionEnvironmentStateView::Attached);
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
    let target = super::environments::activation_target_for_environment_record(&record)
        .expect("activation target");

    let command = super::environments::activate_environment_command(target.clone());

    assert!(matches!(
        command,
        CoreAgentCommand::SetDefaultToolTarget { target: actual } if actual == target
    ));
}

#[test]
fn environment_deactivation_lowers_to_clear_env_target_command() {
    let command = super::environments::deactivate_environment_command();

    assert!(matches!(
        command,
        CoreAgentCommand::ClearDefaultToolTarget { namespace } if namespace == "env"
    ));
}

#[test]
fn invalid_environment_id_maps_to_invalid_request() {
    let error = super::environments::parse_environment_id("bad id".to_owned())
        .expect_err("invalid environment id");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn inactive_environment_cannot_be_activation_target() {
    let record = test_environment_record(
        "local",
        tools::environment::projection::EnvironmentStatus::Detached,
    );

    let error = super::environments::activation_target_for_environment_record(&record)
        .expect_err("detached environment");

    assert_eq!(error.kind, AgentApiErrorKind::Rejected);
}

#[test]
fn declared_mcp_link_materializes_remote_tool() {
    let tool_name = ToolName::new("mcp_crm");
    let active = BTreeMap::new();
    let record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    let link = engine::McpServerLink {
        server_id: "crm".to_owned(),
        allowed_tools: Some(vec!["lookup_customer".to_owned()]),
        approval: Some(engine::RemoteMcpApprovalPolicy::Never),
        defer_loading: Some(true),
        auth_grant_id: None,
    };

    let tool = mcp_api::mcp_tool_from_config_link(&link, &record, None)
        .expect("materialize MCP tool from config link");
    let desired = BTreeMap::from([(tool.name.clone(), tool)]);
    let patch = super::vfs_api::toolset_reconcile_patch(&active, empty_resolved_toolset(), desired);
    let tools = patch.apply_to(&active).expect("apply MCP patch");

    let tool = tools.get(&tool_name).expect("MCP tool");
    let engine::ToolKind::RemoteMcp(spec) = &tool.kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.server_label, "crm");
    assert_eq!(spec.allowed_tools, Some(vec!["lookup_customer".to_owned()]));
    assert_eq!(spec.approval, engine::RemoteMcpApprovalPolicy::Never);
    assert_eq!(spec.defer_loading, Some(true));
}

fn test_auth_grant_record(
    grant_id: &str,
    provider_kind: auth::AuthProviderKind,
    status: auth::AuthGrantStatus,
    audience: Option<&str>,
) -> auth::AuthGrantRecord {
    auth::CreateAuthGrantRecord {
        grant_id: auth::AuthGrantId::new(grant_id),
        provider_id: "static".to_owned(),
        provider_kind,
        principal: auth::PrincipalRef::universe_default(),
        display_name: None,
        subject_hint: None,
        scopes: Vec::new(),
        audience: audience.map(str::to_owned),
        access_token_secret: Some(auth::SecretId::new("authsec_1")),
        refresh_token_secret: None,
        oauth_client: None,
        expires_at_ms: None,
        status,
        metadata: serde_json::Value::Object(Default::default()),
        created_at_ms: 1,
    }
    .into_record()
}

fn mcp_config_link_with_grant(grant_id: &str) -> engine::McpServerLink {
    engine::McpServerLink {
        server_id: "crm".to_owned(),
        allowed_tools: None,
        approval: None,
        defer_loading: None,
        auth_grant_id: Some(grant_id.to_owned()),
    }
}

#[test]
fn mcp_link_with_grant_materializes_auth_ref_for_bearer_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let tool = mcp_api::mcp_tool_from_config_link(
        &mcp_config_link_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect("materialize MCP tool with grant");

    let engine::ToolKind::RemoteMcp(spec) = &tool.kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(
        spec.auth_ref,
        Some(engine::SecretRef {
            namespace: "auth_grant".to_owned(),
            id: "authgrant_1".to_owned(),
        })
    );
}

#[test]
fn mcp_link_rejects_revoked_grant() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Revoked,
        None,
    );

    let error = mcp_api::mcp_tool_from_config_link(
        &mcp_config_link_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("revoked grant must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_kind_incompatible_with_auth_policy() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredOAuth {
        resource: "https://crm.example.com".to_owned(),
        scopes_default: Vec::new(),
        protected_resource_metadata_url: None,
        authorization_server: None,
    };
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );

    let error = mcp_api::mcp_tool_from_config_link(
        &mcp_config_link_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("bearer grant must not satisfy OAuth policy");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_audience_that_does_not_cover_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::OptionalBearer;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://other.example.com"),
    );

    let error = mcp_api::mcp_tool_from_config_link(
        &mcp_config_link_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("audience mismatch must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_for_no_auth_server() {
    let record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );

    let error = mcp_api::mcp_tool_from_config_link(
        &mcp_config_link_with_grant("authgrant_1"),
        &record,
        Some(&grant),
    )
    .expect_err("grant on no-auth server must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
}

#[test]
fn mcp_link_requires_grant_for_required_auth_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    let mut link = mcp_config_link_with_grant("authgrant_1");
    link.auth_grant_id = None;

    let error = mcp_api::mcp_tool_from_config_link(&link, &record, None)
        .expect_err("missing grant must be rejected for required auth");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn toolset_reconcile_patch_preserves_declared_remote_mcp_tools() {
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
    let desired_mcp = BTreeMap::from([(
        remote_tool_name.clone(),
        test_remote_mcp_tool(remote_tool_name.clone()),
    )]);

    let patch = super::vfs_api::toolset_reconcile_patch(&active, toolset, desired_mcp);
    let tools = patch.apply_to(&active).expect("apply reconcile patch");

    assert!(tools.contains_key(&remote_tool_name));
    assert!(!tools.contains_key(&old_tool_name));
    assert!(tools.contains_key(&new_tool_name));
}

#[test]
fn toolset_reconcile_patch_removes_undeclared_remote_mcp_tools() {
    let remote_tool_name = ToolName::new("mcp_crm");
    let active = BTreeMap::from([(
        remote_tool_name.clone(),
        test_remote_mcp_tool(remote_tool_name.clone()),
    )]);

    let patch =
        super::vfs_api::toolset_reconcile_patch(&active, empty_resolved_toolset(), BTreeMap::new());
    let tools = patch.apply_to(&active).expect("apply reconcile patch");

    assert!(!tools.contains_key(&remote_tool_name));
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
    let config = engine_session_config_from_api(
        api::SessionConfig {
            generation: Some(api::GenerationConfig {
                max_output_tokens: Some(2048),
                reasoning_effort: Some("high".to_owned()),
                tool_choice: None,
                parallel_tool_use: None,
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config");

    assert_eq!(config.generation.max_output_tokens, Some(2048));
    assert_eq!(config.generation.reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn session_start_config_rejects_unknown_reasoning_effort() {
    let error = engine_session_config_from_api(
        api::SessionConfig {
            generation: Some(api::GenerationConfig {
                max_output_tokens: None,
                reasoning_effort: Some("hyper".to_owned()),
                tool_choice: None,
                parallel_tool_use: None,
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect_err("unknown reasoning effort must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn session_start_config_maps_tool_choice_and_parallel_tool_use() {
    let config = engine_session_config_from_api(
        api::SessionConfig {
            generation: Some(api::GenerationConfig {
                max_output_tokens: None,
                reasoning_effort: None,
                tool_choice: Some(api::ToolChoice::Specific {
                    tool_id: "web_fetch".to_owned(),
                }),
                parallel_tool_use: Some(false),
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config");

    assert_eq!(
        config.generation.tool_choice,
        Some(engine::ToolChoice::Specific {
            tool_name: ToolName::new("web_fetch")
        })
    );
    assert_eq!(config.generation.parallel_tool_use, Some(false));
}

#[test]
fn session_start_config_maps_provider_triggered_compaction() {
    let config = engine_session_config_from_api(
        api::SessionConfig {
            context: Some(api::ContextConfig {
                compaction: Some(api::CompactionPolicy::ProviderTriggered {
                    compact_threshold_tokens: Some(120_000),
                }),
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config");

    assert_eq!(
        config.context.compaction,
        Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens: Some(120_000)
        })
    );
}

#[test]
fn session_start_config_maps_provider_standalone_compaction() {
    let config = engine_session_config_from_api(
        api::SessionConfig {
            context: Some(api::ContextConfig {
                compaction: Some(api::CompactionPolicy::ProviderStandalone {
                    compact_threshold_tokens: Some(120_000),
                    target_tokens: Some(80_000),
                }),
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config");

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
    let session_config =
        engine_session_config_from_api(api::SessionConfig::default(), openai_model())
            .expect("session config");
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
            generation: Some(api::GenerationConfig {
                max_output_tokens: Some(1024),
                reasoning_effort: Some("medium".to_owned()),
                tool_choice: None,
                parallel_tool_use: None,
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
    assert_eq!(run_config.reasoning_effort.as_deref(), Some("medium"));
    assert!(run_config.tool_choice.is_none());
}

#[test]
fn run_start_config_maps_tool_choice() {
    let session_config =
        engine_session_config_from_api(api::SessionConfig::default(), openai_model())
            .expect("session config");
    let mut run_config = RunConfig::default();

    apply_run_start_config(
        &mut run_config,
        &session_config,
        Some(RunStartConfig {
            model: None,
            generation: Some(api::GenerationConfig {
                max_output_tokens: None,
                reasoning_effort: None,
                tool_choice: Some(api::ToolChoice::RequiredAny),
                parallel_tool_use: None,
            }),
            limits: None,
        }),
    )
    .expect("apply run config");

    assert_eq!(
        run_config.tool_choice.expect("tool choice"),
        engine::ToolChoice::RequiredAny
    );
}

#[test]
fn existing_run_submission_rejects_completed_duplicate_with_different_input() {
    let submission_id = SubmissionId::new("submit_retry");
    let run_config = RunConfig::default();
    let original_input = vec![test_user_message_input(BlobRef::from_bytes(b"original"))];
    let changed_input = vec![test_user_message_input(BlobRef::from_bytes(b"changed"))];
    let original_source = engine::RunRequestSource::Input {
        input: original_input.clone(),
    };
    let changed_source = engine::RunRequestSource::Input {
        input: changed_input,
    };
    let mut state = engine::CoreAgentState::new();
    state.runs.completed.push(engine::RunRecord {
        notify_on_terminal: Vec::new(),
        run_id: RunId::new(7),
        status: RunStatus::Completed,
        submission_id: Some(submission_id.clone()),
        origin: engine::RunOrigin::Requested,
        submission_digest: Some(engine::request_run_submission_digest(
            &original_source,
            &run_config,
        )),
        output_ref: None,
        failure: None,
    });

    assert!(matches!(
        existing_run_submission(&state, &submission_id, &changed_source, &run_config,),
        Some(ExistingRunSubmission::Reject)
    ));
    let Some(ExistingRunSubmission::ReturnRun { run_id, status }) =
        existing_run_submission(&state, &submission_id, &original_source, &run_config)
    else {
        panic!("identical duplicate should return existing completed run");
    };
    assert_eq!(run_id, RunId::new(7));
    assert_eq!(status, RunStatus::Completed);
}

#[test]
fn features_default_off_for_sessions() {
    // Secure by default: an empty config document grants nothing — no web
    // tools, no filesystem tools, no messaging/fleet/timers.
    let config = engine_session_config_from_api(api::SessionConfig::default(), openai_model())
        .expect("map config");

    assert_eq!(config.features, engine::FeaturesConfig::default());
    assert!(config.features.web.is_none());
    assert!(config.features.vfs.is_none());
}

#[test]
fn web_feature_grant_maps_search_and_fetch() {
    let config = engine_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                web: Some(api::WebFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    fetch: Some(api::WebFetchFeature {}),
                    search: Some(api::WebSearchFeature {
                        allowed_domains: None,
                        blocked_domains: Vec::new(),
                    }),
                }),
                ..api::FeaturesConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config");
    config.validate().expect("valid web grant for OpenAI");

    let web = config.features.web.expect("web feature");
    assert!(web.search.is_some());
    assert!(web.fetch.is_some());
}

#[test]
fn web_search_rejects_explicit_enable_for_non_openai_responses() {
    let config = engine_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                web: Some(api::WebFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    fetch: None,
                    search: Some(api::WebSearchFeature {
                        allowed_domains: None,
                        blocked_domains: Vec::new(),
                    }),
                }),
                ..api::FeaturesConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        ModelSelection {
            api_kind: ProviderApiKind::AnthropicMessages,
            provider_id: "anthropic".to_owned(),
            model: "claude-test".to_owned(),
        },
    )
    .expect("map config");

    let error = config
        .validate()
        .expect_err("web search enable should reject Anthropic");

    assert!(matches!(
        error,
        engine::DomainError::ProviderCompatibility(_)
    ));
}

#[test]
fn vfs_feature_grant_maps_tool_surfaces() {
    for (api_surface, engine_surface) in [
        (
            api::VfsToolSurface::ReadOnly,
            engine::VfsToolSurface::ReadOnly,
        ),
        (api::VfsToolSurface::Edit, engine::VfsToolSurface::Edit),
    ] {
        let config = engine_session_config_from_api(
            api::SessionConfig {
                features: Some(api::FeaturesConfig {
                    vfs: Some(api::VfsFeature {
                        version: api::CURRENT_FEATURE_VERSION,
                        tools: Some(api_surface),
                        prompts: None,
                        skills: None,
                    }),
                    ..api::FeaturesConfig::default()
                }),
                ..api::SessionConfig::default()
            },
            openai_model(),
        )
        .expect("map config");

        assert_eq!(
            config.features.vfs.expect("vfs feature").tools,
            Some(engine_surface)
        );
    }

    // A VFS grant without tools yields a VFS with no fs tool surface.
    let config = engine_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                vfs: Some(api::VfsFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    tools: None,
                    prompts: None,
                    skills: None,
                }),
                ..api::FeaturesConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config");

    assert_eq!(config.features.vfs.expect("vfs feature").tools, None);
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
async fn context_entry_input_from_api_accepts_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::Media {
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "image/png".to_owned(),
            kind: api::MediaKind::Image,
            name: None,
        },
    )
    .await
    .expect("session/context/append should accept supported media");

    assert_eq!(
        entry.kind,
        engine::ContextEntryKind::Message {
            role: engine::ContextMessageRole::User,
        }
    );
    assert_eq!(entry.content_ref, blob_ref);
    assert_eq!(entry.media_type.as_deref(), Some("image/png"));
    assert_eq!(entry.preview.as_deref(), Some("[image]"));
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
        BlobPutParams {
            blobs: vec![
                BlobPutItem {
                    bytes_base64: BASE64.encode(b"hello"),
                },
                BlobPutItem {
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
        BlobHasParams {
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

    let read = read_blob(
        &store,
        BlobReadParams {
            blob_ref: put.blobs[1].blob_ref.clone(),
        },
    )
    .await
    .expect("read blob");
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
        correlation_token: None,
        kind,
        message: "admission failed".to_owned(),
        rejection: None,
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

fn test_mcp_server_record(server_id: &str, status: mcp::McpServerStatus) -> mcp::McpServerRecord {
    mcp::PutMcpServerRecord {
        server_id: mcp::McpServerId::new(server_id),
        display_name: Some(format!("{server_id} MCP")),
        server_url: format!("https://{server_id}.example.com/mcp"),
        transport: mcp::RemoteMcpTransport::Auto,
        default_server_label: server_id.to_owned(),
        description: None,
        allowed_tools: None,
        approval_default: mcp::McpApprovalPolicy::ProviderDefault,
        defer_loading_default: None,
        auth_policy: mcp::McpServerAuthPolicy::None,
        status,
        now_ms: 1,
    }
    .into_record()
}

fn empty_resolved_toolset() -> ResolvedToolset {
    ResolvedToolset {
        tools: BTreeMap::new(),
        documents: Vec::new(),
        catalog: tools::runtime::ToolCatalog::new(),
        provider_params_patch: tools::toolset::ProviderParamsPatch::default(),
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

    async fn list_workspaces(&self) -> Result<Vec<vfs::VfsWorkspaceRecord>, vfs::VfsCatalogError> {
        Ok(Vec::new())
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
    assert_eq!(secret.secret_kind, auth::SECRET_KIND_OAUTH_CLIENT_SECRET);
    assert_eq!(secret.value.expose(), "shh-secret");
    assert_eq!(draft.client.client_secret, Some(secret.secret_id.clone()));
    assert_eq!(
        draft.client.token_endpoint_auth_method,
        auth::TokenEndpointAuthMethod::ClientSecretBasic
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
        auth::TokenEndpointAuthMethod::None
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
    let mut record = test_mcp_server_record("playground", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredOAuth {
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

    let mut bearer = test_mcp_server_record("bearer", mcp::McpServerStatus::Active);
    bearer.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
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
    let record = auth::CreateAuthFlowRecord {
        flow_id: auth::AuthFlowId::new("authflow_1"),
        client_id: auth::OAuthClientId::new("crm"),
        provider_id: "crm".to_owned(),
        provider_kind: auth::AuthProviderKind::McpOAuth,
        principal: auth::PrincipalRef::universe_default(),
        state_hash: auth::state_hash("state-1"),
        pkce_verifier_secret: auth::SecretId::new("authsec_pkce"),
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
