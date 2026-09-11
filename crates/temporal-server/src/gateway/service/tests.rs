use api::BlobPutItem;

use super::*;
use crate::gateway::service::prompts::active_prompt_context_entries;
use tools::skills::SkillLocation;
use vfs::VfsPath;

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
fn managed_session_retry_requires_the_durable_creation_fingerprint() {
    let universe_id = uuid::Uuid::from_u128(1);
    let declaration = engine::ManagedSessionWorkflowTools::v1(
        Some(engine::WorkflowEndpointRef {
            workflow_id: "global controller/work-1".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        }),
        Vec::new(),
    );
    let mut state = engine::CoreAgentState::new();
    state.workflow_tools.session_universe_id = Some(universe_id);
    state.workflow_tools.managed_creation_fingerprint = Some(
        declaration
            .creation_fingerprint(universe_id)
            .expect("creation fingerprint"),
    );
    validate_managed_session_retry(&state, universe_id, &declaration).expect("matching retry");

    let conflicting = engine::ManagedSessionWorkflowTools::v1(
        Some(engine::WorkflowEndpointRef {
            workflow_id: "another controller".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        }),
        Vec::new(),
    );
    assert_eq!(
        validate_managed_session_retry(&state, universe_id, &conflicting)
            .expect_err("conflicting retry")
            .kind,
        AgentApiErrorKind::Conflict
    );
    assert_eq!(
        validate_managed_session_retry(&engine::CoreAgentState::new(), universe_id, &declaration,)
            .expect_err("standalone session cannot become managed")
            .kind,
        AgentApiErrorKind::Conflict
    );
}

#[test]
fn legacy_subagent_bindings_retain_their_immutable_deadline_ceiling() {
    const LEGACY_CEILING_MS: u64 = 4 * 60 * 60 * 1_000;
    let tool = tools::definitions::register(
        "subagent.run",
        Default::default(),
        engine::ToolParallelism::ParallelSafe,
        Default::default(),
    );
    let tool_id = WorkflowToolId::new(tools::subagents::AGENT_RUN_WORKFLOW_TOOL_ID);
    let binding = engine::WorkflowToolBinding::admit(
        uuid::Uuid::from_u128(1),
        engine::WorkflowToolDefinition {
            tool_id: tool_id.clone(),
            revision: 1,
            semantic_type: tools::subagents::AGENT_RUN_WORKFLOW_SEMANTIC_TYPE.to_owned(),
            tool,
        },
        WorkflowToolTarget::Bound {
            receiver: WorkflowEndpointRef {
                workflow_id: "legacy-subagent-execution".to_owned(),
                workflow_kind: "subagent.execution".to_owned(),
            },
            dispatch: BoundWorkflowToolDispatch::Push,
        },
        WorkflowToolCompletion::Joined {
            reply_schema_ref: None,
            deadline_after_ms: LEGACY_CEILING_MS,
        },
    )
    .expect("legacy subagent binding");
    let mut state = engine::CoreAgentState::new();
    state.workflow_tools.bindings.insert(tool_id, binding);
    let mut features = engine::FeaturesConfig {
        subagents: Some(engine::SubagentsFeature {
            agents: vec![engine::SubagentAgentConfig {
                profile_id: "reviewer".to_owned(),
            }],
            limits: engine::SubagentLimits {
                deadline_ms: LEGACY_CEILING_MS,
                ..engine::SubagentLimits::default()
            },
            ..engine::SubagentsFeature::default()
        }),
        ..engine::FeaturesConfig::default()
    };

    validate_subagent_deadline_for_existing_bindings(&state, &features)
        .expect("legacy ceiling remains valid");
    features
        .subagents
        .as_mut()
        .expect("subagents")
        .limits
        .deadline_ms += 1;
    assert_eq!(
        validate_subagent_deadline_for_existing_bindings(&state, &features)
            .expect_err("deadline above the durable binding must fail")
            .kind,
        AgentApiErrorKind::InvalidRequest
    );
}

#[test]
fn managed_workflow_tools_api_maps_bound_promise_function_tools() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let reply_schema_ref = BlobRef::from_bytes(br#"{"type":"string"}"#);
    let declaration = managed_workflow_tools_from_api(ManagedSessionWorkflowToolsInput {
        version: 1,
        lifecycle_controller: Some(WorkflowEndpointInput {
            workflow_id: "controller-1".to_owned(),
            workflow_kind: "order.workflow".to_owned(),
        }),
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                tool_id: "accept-order".to_owned(),
                revision: 2,
                semantic_type: "orders.accepted.v1".to_owned(),
                tool: WorkflowToolSpecInput {
                    name: "accept_order".to_owned(),
                    kind: WorkflowToolKindInput::Function {
                        description_ref: None,
                        input_schema_ref: input_schema_ref.as_str().to_owned(),
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    },
                    parallelism: ToolParallelismView::ParallelSafe,
                },
            },
            target: WorkflowToolTargetInput::Bound {
                receiver: WorkflowEndpointInput {
                    workflow_id: "receiver-1".to_owned(),
                    workflow_kind: "order.receiver".to_owned(),
                },
                dispatch: BoundWorkflowToolDispatchInput::Push,
            },
            completion: WorkflowToolCompletionInput::Promises {
                reply_schema_ref: Some(reply_schema_ref.as_str().to_owned()),
                deadline_after_ms: Some(30_000),
                max_promises: 4,
                key_source: WorkflowToolCompletionKeySourceInput::ArrayIndices {
                    pointer: "/orders".to_owned(),
                    prefix: "order-".to_owned(),
                },
            },
        }],
    })
    .expect("map managed workflow tools");

    assert_eq!(
        declaration.lifecycle_controller,
        Some(WorkflowEndpointRef {
            workflow_id: "controller-1".to_owned(),
            workflow_kind: "order.workflow".to_owned(),
        })
    );
    let tool = &declaration.tools[0];
    assert_eq!(
        tool.completion,
        WorkflowToolCompletion::Promises {
            reply_schema_ref: Some(reply_schema_ref),
            deadline_after_ms: Some(30_000),
            max_promises: 4,
            key_source: WorkflowToolCompletionKeySource::ArrayIndices {
                pointer: "/orders".to_owned(),
                prefix: "order-".to_owned(),
            },
        }
    );
    assert_eq!(
        tool.target,
        WorkflowToolTarget::Bound {
            receiver: WorkflowEndpointRef {
                workflow_id: "receiver-1".to_owned(),
                workflow_kind: "order.receiver".to_owned(),
            },
            dispatch: BoundWorkflowToolDispatch::Push,
        }
    );
    assert_eq!(tool.definition.tool.name.as_str(), "accept_order");
    let ToolKind::Function(function) = &tool.definition.tool.kind else {
        panic!("API workflow tool must map to a function tool");
    };
    assert_eq!(function.input_schema_ref, input_schema_ref);
    assert_eq!(function.strict, Some(true));
}

#[test]
fn managed_workflow_tools_api_maps_bound_accepted_function_tools() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let declaration = managed_workflow_tools_from_api(ManagedSessionWorkflowToolsInput {
        version: 1,
        lifecycle_controller: None,
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                tool_id: "channel-noop".to_owned(),
                revision: 1,
                semantic_type: "channels.noop.v1".to_owned(),
                tool: WorkflowToolSpecInput {
                    name: "channel_noop".to_owned(),
                    kind: WorkflowToolKindInput::Function {
                        description_ref: None,
                        input_schema_ref: input_schema_ref.as_str().to_owned(),
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    },
                    parallelism: ToolParallelismView::ParallelSafe,
                },
            },
            target: WorkflowToolTargetInput::Bound {
                receiver: WorkflowEndpointInput {
                    workflow_id: "channels/session-1".to_owned(),
                    workflow_kind: "channels.session".to_owned(),
                },
                dispatch: BoundWorkflowToolDispatchInput::Push,
            },
            completion: WorkflowToolCompletionInput::Accepted,
        }],
    })
    .expect("map Accepted workflow tool");

    assert_eq!(
        declaration.tools[0].completion,
        WorkflowToolCompletion::Accepted
    );
    assert_eq!(
        declaration.tools[0].target,
        WorkflowToolTarget::Bound {
            receiver: WorkflowEndpointRef {
                workflow_id: "channels/session-1".to_owned(),
                workflow_kind: "channels.session".to_owned(),
            },
            dispatch: BoundWorkflowToolDispatch::Push,
        }
    );
}

#[test]
fn managed_workflow_tools_api_maps_joined_completion() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let reply_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let declaration = managed_workflow_tools_from_api(ManagedSessionWorkflowToolsInput {
        version: 1,
        lifecycle_controller: None,
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                tool_id: "message-send".to_owned(),
                revision: 1,
                semantic_type: "channels.receipt.v1".to_owned(),
                tool: WorkflowToolSpecInput {
                    name: "message_send".to_owned(),
                    kind: WorkflowToolKindInput::Function {
                        description_ref: None,
                        input_schema_ref: input_schema_ref.as_str().to_owned(),
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    },
                    parallelism: ToolParallelismView::ParallelSafe,
                },
            },
            target: WorkflowToolTargetInput::Bound {
                receiver: WorkflowEndpointInput {
                    workflow_id: "channels/session-1".to_owned(),
                    workflow_kind: "channels.session".to_owned(),
                },
                dispatch: BoundWorkflowToolDispatchInput::Push,
            },
            completion: WorkflowToolCompletionInput::Joined {
                reply_schema_ref: Some(reply_schema_ref.as_str().to_owned()),
                deadline_after_ms: 30_000,
            },
        }],
    })
    .expect("map Joined workflow tool");

    assert_eq!(
        declaration.tools[0].completion,
        WorkflowToolCompletion::Joined {
            reply_schema_ref: Some(reply_schema_ref),
            deadline_after_ms: 30_000,
        }
    );
}

#[test]
fn managed_workflow_tools_api_maps_start_targets_and_reply_completion() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let recipe_bytes = br#"{"workflowType":"ChannelsSendWorkflow","taskQueue":"channels"}"#;
    let recipe_ref = BlobRef::from_bytes(recipe_bytes);
    let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(recipe_bytes);
    let declaration = managed_workflow_tools_from_api(ManagedSessionWorkflowToolsInput {
        version: 1,
        lifecycle_controller: None,
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                tool_id: "channel-send".to_owned(),
                revision: 1,
                semantic_type: "channels.send.v1".to_owned(),
                tool: WorkflowToolSpecInput {
                    name: "channel_send".to_owned(),
                    kind: WorkflowToolKindInput::Function {
                        description_ref: None,
                        input_schema_ref: input_schema_ref.as_str().to_owned(),
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    },
                    parallelism: ToolParallelismView::Exclusive,
                },
            },
            target: WorkflowToolTargetInput::Start {
                start: WorkflowStartRefInput {
                    recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                    revision: 3,
                    recipe_ref: recipe_ref.as_str().to_owned(),
                    recipe_fingerprint: recipe_fingerprint.clone(),
                },
            },
            completion: WorkflowToolCompletionInput::Promises {
                reply_schema_ref: None,
                deadline_after_ms: Some(60_000),
                max_promises: 1,
                key_source: WorkflowToolCompletionKeySourceInput::Reply,
            },
        }],
    })
    .expect("map start workflow tool");

    let tool = &declaration.tools[0];
    assert_eq!(
        tool.target,
        WorkflowToolTarget::Start {
            start: WorkflowStartRef {
                recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                revision: 3,
                recipe_ref,
                recipe_fingerprint,
            }
        }
    );
    assert!(matches!(
        tool.completion,
        WorkflowToolCompletion::Promises {
            max_promises: 1,
            key_source: WorkflowToolCompletionKeySource::Reply,
            ..
        }
    ));
}

#[test]
fn managed_workflow_tools_api_rejects_an_empty_management_document() {
    assert_eq!(
        managed_workflow_tools_from_api(ManagedSessionWorkflowToolsInput {
            version: 1,
            lifecycle_controller: None,
            tools: Vec::new(),
        })
        .expect_err("empty managed creation must fail")
        .kind,
        AgentApiErrorKind::InvalidRequest
    );
}

#[test]
fn run_terminal_notification_derives_destination_from_controller() {
    let controller = WorkflowEndpointRef {
        workflow_id: "controller-workflow-1".to_owned(),
        workflow_kind: "order.workflow".to_owned(),
    };
    assert_eq!(
        run_terminal_notify_intents(
            Some(&controller),
            Some(RunTerminalNotificationInput {
                token: "terminal-token-1".to_owned(),
            }),
            Vec::new(),
        )
        .expect("derive controller notification"),
        vec![engine::RunTerminalNotifyIntent {
            holder_workflow_id: "controller-workflow-1".to_owned(),
            token: "terminal-token-1".to_owned(),
        }]
    );

    assert_eq!(
        run_terminal_notify_intents(
            None,
            Some(RunTerminalNotificationInput {
                token: "terminal-token-1".to_owned(),
            }),
            Vec::new(),
        )
        .expect_err("notification without controller must fail")
        .kind,
        AgentApiErrorKind::InvalidRequest
    );
}

#[test]
fn skill_list_response_exposes_readable_locations() {
    let catalog_ref = BlobRef::from_bytes(b"catalog");
    let mut catalog = test_skill_catalog(
        &catalog_ref,
        vec![
            test_skill_metadata("skill:review", "review", true),
            test_skill_metadata("skill:deploy", "deploy", false),
        ],
    );
    catalog.warnings.push(tools::skills::SkillLoadWarning::new(
        "workspace",
        Some("/skills/broken".into()),
        tools::skills::SkillLoadWarningKind::MissingSkillDoc,
    ));
    let response = skill_list_response(Some(&catalog_ref), Some(&catalog));

    assert_eq!(response.catalogs[0].source, api::SkillCatalogSource::Vfs);
    assert_eq!(
        response.catalogs[0].availability,
        api::SkillCatalogAvailability::Available
    );
    assert_eq!(
        response.catalogs[0].warnings,
        vec!["workspace /skills/broken: missing SKILL.md"]
    );
    assert_eq!(
        response.catalogs[0].catalog_ref.as_deref(),
        Some(catalog_ref.as_str())
    );
    assert_eq!(response.catalogs[0].skills.len(), 2);
    assert_eq!(response.catalogs[0].skills[0].skill_id, "skill:review");
    assert!(response.catalogs[0].skills[0].enabled);
    assert_eq!(
        response.catalogs[0].skills[0].location,
        SkillLocationView {
            skill_dir_path: "/skills/system/review".into(),
            skill_doc_path: "/skills/system/review/SKILL.md".into(),
        }
    );
    assert_eq!(response.catalogs[0].skills[1].skill_id, "skill:deploy");
    assert!(!response.catalogs[0].skills[1].enabled);
}

#[test]
fn environment_activation_lowers_to_active_environment_command() {
    let environment_id = engine::EnvironmentId::new("local");
    let command = super::environments::activate_environment_command(environment_id.clone());

    assert!(matches!(
        command,
        CoreAgentCommand::SetActiveEnvironment { environment_id: actual }
            if actual == environment_id
    ));
}

#[test]
fn environment_deactivation_lowers_to_clear_active_environment_command() {
    let command = super::environments::deactivate_environment_command();

    assert!(matches!(command, CoreAgentCommand::ClearActiveEnvironment));
}

#[test]
fn declared_mcp_link_materializes_remote_tool() {
    let tool_name = ToolName::new("mcp_crm");
    let active = BTreeMap::new();
    let mut record = test_mcp_server_record("durable-crm-server", mcp::McpServerStatus::Active);
    record.default_server_label = "crm".to_owned();
    record.allowed_tools = Some(vec!["lookup_customer".to_owned()]);
    record.approval_default = mcp::McpApprovalPolicy::Never;
    record.defer_loading_default = Some(true);
    let link = engine::McpServerLink {
        server_id: "durable-crm-server".to_owned(),
    };

    let tool = mcp_api::mcp_tool_from_config_link(&link, &record, None)
        .expect("materialize MCP tool from config link");
    let desired = BTreeMap::from([(tool.name.clone(), tool)]);
    let patch =
        super::session_toolset::toolset_reconcile_patch(&active, empty_resolved_toolset(), desired);
    let tools = patch.apply_to(&active).expect("apply MCP patch");

    let tool = tools.get(&tool_name).expect("MCP tool");
    let engine::ToolKind::RemoteMcp(spec) = &tool.kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.server_id, "durable-crm-server");
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
        exposure: auth::AuthGrantExposure::Brokered,
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

#[test]
fn grant_leases_require_creation_time_retrievable_exposure() {
    let brokered = test_auth_grant_record(
        "authgrant_brokered",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );
    let mut retrievable = brokered.clone();
    retrievable.exposure = auth::AuthGrantExposure::Retrievable;

    let error = require_retrievable_grant(&brokered).expect_err("brokered grant must reject");
    assert_eq!(error.kind, AgentApiErrorKind::Rejected);
    require_retrievable_grant(&retrievable).expect("retrievable grant accepted");
}

fn mcp_config_link() -> engine::McpServerLink {
    engine::McpServerLink {
        server_id: "crm".to_owned(),
    }
}

#[test]
fn mcp_server_put_enforces_required_and_optional_binding_states() {
    let mut required = test_mcp_server_put("crm", mcp::McpServerStatus::Active);
    required.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    let error = mcp_api::validate_mcp_server_credential(&required, None)
        .expect_err("active required-auth server must have a binding");
    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);

    required.status = mcp::McpServerStatus::NeedsAuthConfig;
    mcp_api::validate_mcp_server_credential(&required, None)
        .expect("pre-login required-auth server may be unbound");

    let mut optional = test_mcp_server_put("public", mcp::McpServerStatus::Active);
    optional.auth_policy = mcp::McpServerAuthPolicy::OptionalBearer;
    mcp_api::validate_mcp_server_credential(&optional, None)
        .expect("optional-auth server may be active and unbound");
}

#[test]
fn mcp_link_with_grant_materializes_auth_ref_for_bearer_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let tool = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, Some(&grant))
        .expect("materialize MCP tool with grant");

    let engine::ToolKind::RemoteMcp(spec) = &tool.kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(
        spec.auth_ref,
        Some(engine::SecretRef {
            namespace: "mcp_server".to_owned(),
            id: "crm".to_owned(),
        })
    );
}

#[test]
fn mcp_link_rejects_revoked_grant() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Revoked,
        None,
    );

    let error = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, Some(&grant))
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
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );

    let error = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, Some(&grant))
        .expect_err("bearer grant must not satisfy OAuth policy");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_link_rejects_grant_audience_that_does_not_cover_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::OptionalBearer;
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://other.example.com"),
    );

    let error = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, Some(&grant))
        .expect_err("audience mismatch must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_server_rejects_grant_audience_that_does_not_cover_oauth_resource() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredOAuth {
        resource: "https://resource.example.com/mcp".to_owned(),
        scopes_default: Vec::new(),
        protected_resource_metadata_url: None,
        authorization_server: None,
    };
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::McpOAuth,
        auth::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let error = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, Some(&grant))
        .expect_err("grant audience must cover the OAuth resource as well as the server URL");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn two_server_ids_can_share_an_endpoint_with_distinct_credentials() {
    let mut work = test_mcp_server_record("crm_work", mcp::McpServerStatus::Active);
    work.server_url = "https://crm.example.com/mcp".to_owned();
    work.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    work.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_work"));
    let work_grant = test_auth_grant_record(
        "authgrant_work",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let mut personal = test_mcp_server_record("crm_personal", mcp::McpServerStatus::Active);
    personal.server_url = work.server_url.clone();
    personal.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    personal.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_personal"));
    let personal_grant = test_auth_grant_record(
        "authgrant_personal",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let mut work_link = mcp_config_link();
    work_link.server_id = "crm_work".to_owned();
    let mut personal_link = mcp_config_link();
    personal_link.server_id = "crm_personal".to_owned();
    let work_tool =
        mcp_api::mcp_tool_from_config_link(&work_link, &work, Some(&work_grant)).expect("work");
    let personal_tool =
        mcp_api::mcp_tool_from_config_link(&personal_link, &personal, Some(&personal_grant))
            .expect("personal");
    let desired = BTreeMap::from([
        (work_tool.name.clone(), work_tool),
        (personal_tool.name.clone(), personal_tool),
    ]);

    let tools = super::session_toolset::toolset_reconcile_patch(
        &BTreeMap::new(),
        empty_resolved_toolset(),
        desired,
    )
    .apply_to(&BTreeMap::new())
    .expect("both identities may coexist when their server labels differ");
    assert_eq!(tools.len(), 2);
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

    let error = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, Some(&grant))
        .expect_err("grant on no-auth server must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
}

#[test]
fn mcp_link_requires_grant_for_required_auth_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;

    let error = mcp_api::mcp_tool_from_config_link(&mcp_config_link(), &record, None)
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
    let toolset = RegisteredToolset {
        tools: BTreeMap::from([(
            new_tool_name.clone(),
            test_function_tool(new_tool_name.clone()),
        )]),
    };
    let desired_mcp = BTreeMap::from([(
        remote_tool_name.clone(),
        test_remote_mcp_tool(remote_tool_name.clone()),
    )]);

    let patch = super::session_toolset::toolset_reconcile_patch(&active, toolset, desired_mcp);
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

    let patch = super::session_toolset::toolset_reconcile_patch(
        &active,
        empty_resolved_toolset(),
        BTreeMap::new(),
    );
    let tools = patch.apply_to(&active).expect("apply reconcile patch");

    assert!(!tools.contains_key(&remote_tool_name));
}

#[test]
fn toolset_reconcile_patch_tracks_every_mcp_policy_transition() {
    let remote_tool_name = ToolName::new("mcp_crm");
    let find_tool_name = ToolName::new("mcp_find_tools");
    let call_tool_name = ToolName::new("mcp_call");

    let mut inject_all = test_remote_mcp_tool(remote_tool_name.clone());
    let engine::ToolKind::RemoteMcp(spec) = &mut inject_all.kind else {
        unreachable!("test helper must produce a remote MCP tool");
    };
    spec.execution = engine::RemoteMcpExecution::Native;
    let mut active = BTreeMap::from([(remote_tool_name.clone(), inject_all)]);

    let mut search_selected = test_remote_mcp_tool(remote_tool_name.clone());
    let engine::ToolKind::RemoteMcp(spec) = &mut search_selected.kind else {
        unreachable!("test helper must produce a remote MCP tool");
    };
    spec.record_revision = 2;
    spec.execution = engine::RemoteMcpExecution::Native;
    spec.exposure = engine::RemoteMcpExposure::Search;
    spec.allowed_tools = Some(vec!["lookup_customer".to_owned()]);
    let desired = BTreeMap::from([
        (remote_tool_name.clone(), search_selected),
        (
            find_tool_name.clone(),
            test_function_tool(find_tool_name.clone()),
        ),
        (
            call_tool_name.clone(),
            test_function_tool(call_tool_name.clone()),
        ),
    ]);
    active =
        super::session_toolset::toolset_reconcile_patch(&active, empty_resolved_toolset(), desired)
            .apply_to(&active)
            .expect("switch inject-all to search-selected");
    assert!(active.contains_key(&find_tool_name));
    assert!(active.contains_key(&call_tool_name));
    let engine::ToolKind::RemoteMcp(spec) = &active[&remote_tool_name].kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.exposure, engine::RemoteMcpExposure::Search);
    assert_eq!(spec.allowed_tools, Some(vec!["lookup_customer".to_owned()]));

    let mut search_other_selection = active[&remote_tool_name].clone();
    let engine::ToolKind::RemoteMcp(spec) = &mut search_other_selection.kind else {
        unreachable!("expected remote MCP tool");
    };
    spec.record_revision = 3;
    spec.allowed_tools = Some(vec!["create_customer".to_owned()]);
    let desired = BTreeMap::from([
        (remote_tool_name.clone(), search_other_selection),
        (find_tool_name.clone(), active[&find_tool_name].clone()),
        (call_tool_name.clone(), active[&call_tool_name].clone()),
    ]);
    active =
        super::session_toolset::toolset_reconcile_patch(&active, empty_resolved_toolset(), desired)
            .apply_to(&active)
            .expect("change selected search tools");
    let engine::ToolKind::RemoteMcp(spec) = &active[&remote_tool_name].kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.allowed_tools, Some(vec!["create_customer".to_owned()]));

    let mut inject_selected = active[&remote_tool_name].clone();
    let engine::ToolKind::RemoteMcp(spec) = &mut inject_selected.kind else {
        unreachable!("expected remote MCP tool");
    };
    spec.record_revision = 4;
    spec.exposure = engine::RemoteMcpExposure::Inject;
    let desired = BTreeMap::from([(remote_tool_name.clone(), inject_selected)]);
    active =
        super::session_toolset::toolset_reconcile_patch(&active, empty_resolved_toolset(), desired)
            .apply_to(&active)
            .expect("switch search to inject-selected");
    assert!(!active.contains_key(&find_tool_name));
    assert!(!active.contains_key(&call_tool_name));

    let mut inject_all = active[&remote_tool_name].clone();
    let engine::ToolKind::RemoteMcp(spec) = &mut inject_all.kind else {
        unreachable!("expected remote MCP tool");
    };
    spec.record_revision = 5;
    spec.allowed_tools = None;
    let desired = BTreeMap::from([(remote_tool_name.clone(), inject_all)]);
    active =
        super::session_toolset::toolset_reconcile_patch(&active, empty_resolved_toolset(), desired)
            .apply_to(&active)
            .expect("switch inject-selected to inject-all");
    let engine::ToolKind::RemoteMcp(spec) = &active[&remote_tool_name].kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.exposure, engine::RemoteMcpExposure::Inject);
    assert_eq!(spec.allowed_tools, None);
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
        content: input.content,
        preview: input.preview,
        origin: input.origin,
        provenance_ref: input.provenance_ref,
        token_estimate: input.token_estimate,
        supersedes: None,
    };
    let mut state = engine::CoreAgentState::new();
    state.context.entries = vec![entry];

    let active_entries = active_prompt_context_entries(&state);

    assert_eq!(active_entries.len(), 1);
    assert_eq!(active_entries[0].provenance_ref.as_ref(), Some(&report_ref));
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
                processing_tier: None,
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
                processing_tier: None,
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
                processing_tier: None,
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
                processing_tier: None,
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
                processing_tier: None,
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
fn run_start_without_overrides_keeps_session_defaults_out_of_run_config() {
    let mut session_config =
        engine_session_config_from_api(api::SessionConfig::default(), openai_model())
            .expect("session config");
    session_config.generation.max_output_tokens = Some(4096);
    session_config.generation.reasoning_effort = Some("high".to_owned());
    session_config.limits.max_turns = Some(12);
    session_config.limits.max_tool_rounds = Some(3);

    let run_config = run_config_for_start(&session_config, None).expect("run config");

    assert_eq!(run_config, RunConfig::default());
}

#[test]
fn session_processing_tier_maps_all_openai_tiers() {
    for (tier, expected) in [
        (
            api::ModelProcessingTier::Standard,
            engine::ModelProcessingTier::Standard,
        ),
        (
            api::ModelProcessingTier::Fast,
            engine::ModelProcessingTier::Fast,
        ),
        (
            api::ModelProcessingTier::Flex,
            engine::ModelProcessingTier::Flex,
        ),
    ] {
        let config = engine_session_config_from_api(
            api::SessionConfig {
                generation: Some(api::GenerationConfig {
                    processing_tier: Some(tier),
                    ..api::GenerationConfig::default()
                }),
                ..api::SessionConfig::default()
            },
            openai_model(),
        )
        .expect("OpenAI session processing tier");
        assert_eq!(config.generation.processing_tier, Some(expected));
    }
}

#[test]
fn run_processing_tier_overrides_the_session_default() {
    let session_config = engine_session_config_from_api(
        api::SessionConfig {
            generation: Some(api::GenerationConfig {
                processing_tier: Some(api::ModelProcessingTier::Standard),
                ..api::GenerationConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("session config");

    let run_config = run_config_for_start(
        &session_config,
        Some(RunStartConfig {
            generation: Some(api::GenerationConfig {
                processing_tier: Some(api::ModelProcessingTier::Fast),
                ..api::GenerationConfig::default()
            }),
            ..RunStartConfig::default()
        }),
    )
    .expect("run processing tier override");

    assert_eq!(
        run_config.processing_tier,
        Some(engine::ModelProcessingTier::Fast)
    );
}

#[test]
fn session_processing_tier_rejects_compatible_custom_providers() {
    let mut model = openai_model();
    model.provider_id = "deepseek".to_owned();
    model.api_kind = ProviderApiKind::OpenAiCompletions;
    let error = engine_session_config_from_api(
        api::SessionConfig {
            generation: Some(api::GenerationConfig {
                processing_tier: Some(api::ModelProcessingTier::Fast),
                ..api::GenerationConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        model,
    )
    .expect_err("custom providers must not inherit OpenAI billing tiers");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[test]
fn run_start_records_only_explicit_limit_overrides() {
    let mut session_config =
        engine_session_config_from_api(api::SessionConfig::default(), openai_model())
            .expect("session config");
    session_config.limits.max_turns = Some(12);
    session_config.limits.max_tool_rounds = Some(3);

    let run_config = run_config_for_start(
        &session_config,
        Some(RunStartConfig {
            model: None,
            generation: None,
            limits: Some(api::RunLimitsConfig {
                max_turns: Some(4),
                max_tool_rounds: None,
            }),
        }),
    )
    .expect("run config");

    assert_eq!(run_config.max_turns, Some(4));
    assert_eq!(run_config.max_tool_rounds, None);
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
        submission_digest: Some(engine::request_run_submission_digest(
            &original_source,
            &run_config,
            &[],
        )),
        source: engine::RunSource::Input {
            input: original_input,
        },
        first_seq: engine::EventSeq::new(1),
        terminal_seq: engine::EventSeq::new(1),
        accepted_at_ms: 1,
        started_at_ms: Some(1),
        completed_at_ms: 1,
        usage: None,
        output: None,
        failure: None,
    });

    assert!(matches!(
        existing_run_submission(&state, &submission_id, &changed_source, &run_config, &[]),
        Some(ExistingRunSubmission::Reject)
    ));
    let Some(ExistingRunSubmission::ReturnRun { run_id }) =
        existing_run_submission(&state, &submission_id, &original_source, &run_config, &[])
    else {
        panic!("identical duplicate should return existing completed run");
    };
    assert_eq!(run_id, RunId::new(7));
    assert!(matches!(
        existing_run_submission(
            &state,
            &submission_id,
            &original_source,
            &run_config,
            &[engine::RunTerminalNotifyIntent {
                holder_workflow_id: "controller-1".to_owned(),
                token: "changed-token".to_owned(),
            }],
        ),
        Some(ExistingRunSubmission::Reject)
    ));
}

#[test]
fn features_default_off_for_sessions() {
    // Secure by default: an empty config document grants nothing — no web
    // tools, no filesystem tools, no Fleet/timers.
    let config = engine_session_config_from_api(api::SessionConfig::default(), openai_model())
        .expect("map config");

    assert_eq!(config.features, engine::FeaturesConfig::default());
    assert!(config.features.web.is_none());
    assert!(config.features.vfs.is_none());
}

#[test]
fn environment_tool_subgrants_are_default_off_and_map_explicit_opt_in() {
    let default_feature: api::EnvironmentsFeature =
        serde_json::from_value(serde_json::json!({})).expect("empty environment feature");
    assert!(!default_feature.selection_tools);
    assert!(!default_feature.jobs);

    let config = engine_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                environments: Some(api::EnvironmentsFeature {
                    working_directory: None,
                    prompts: None,
                    version: api::CURRENT_FEATURE_VERSION,
                    providers: None,
                    registration_keys: None,
                    selection_tools: true,
                    jobs: true,
                    skills: None,
                }),
                ..api::FeaturesConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map environment jobs grant");

    let environments = config.features.environments.expect("environment feature");
    assert!(environments.selection_tools);
    assert!(environments.jobs);
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
fn web_search_rejects_explicit_enable_for_unsupported_api_kind() {
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
            api_kind: ProviderApiKind::OpenAiCompletions,
            provider_id: "openai".to_owned(),
            model: "gpt-test".to_owned(),
        },
    )
    .expect("map config");

    let error = config
        .validate()
        .expect_err("web search must reject OpenAI Completions");
    assert!(matches!(
        error,
        engine::DomainError::ProviderCompatibility(_)
    ));
}

#[test]
fn web_search_accepts_anthropic_messages() {
    let config = engine_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                web: Some(api::WebFeature {
                    version: api::CURRENT_FEATURE_VERSION,
                    fetch: Some(api::WebFetchFeature {}),
                    search: Some(api::WebSearchFeature {
                        allowed_domains: Some(vec!["docs.anthropic.com".to_owned()]),
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

    config
        .validate()
        .expect("Anthropic web tools are supported");
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
                        working_directory: None,
                        version: api::CURRENT_FEATURE_VERSION,
                        workspace_links: Vec::new(),
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
                    working_directory: None,
                    version: api::CURRENT_FEATURE_VERSION,
                    workspace_links: Vec::new(),
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

#[test]
fn profile_and_session_grants_derive_vfs_transfer_tools() {
    for environments in [false, true] {
        for surface in [None, Some("none"), Some("readOnly"), Some("edit")] {
            let mut features = serde_json::json!({});
            if let Some(surface) = surface {
                features["vfs"] = serde_json::json!({});
                if surface != "none" {
                    features["vfs"]["tools"] = serde_json::json!(surface);
                }
            }
            if environments {
                // Selection tools and jobs are independent of transfer availability.
                features["environments"] = serde_json::json!({});
            }
            let profile: api::ProfileDocument =
                serde_json::from_value(serde_json::json!({"config": {"features": features}}))
                    .unwrap();
            let config =
                engine_session_config_from_api(profile.config.unwrap(), openai_model()).unwrap();
            let toolset_config =
                GatewayAgentApi::session_toolset_config(&config, environments, false);
            let registered = tools::toolset::register_toolset(&toolset_config).unwrap();
            for (id, expected) in [
                (
                    "vfs.materialize",
                    environments && matches!(surface, Some("readOnly" | "edit")),
                ),
                ("vfs.capture", environments && surface == Some("edit")),
            ] {
                assert_eq!(
                    registered.tools.contains_key(&ToolName::new(id)),
                    expected,
                    "surface={surface:?}, environments={environments}, tool={id}"
                );
            }
            assert!(
                !registered
                    .tools
                    .contains_key(&ToolName::new("env.vfs_materialize"))
            );
            assert!(
                !registered
                    .tools
                    .contains_key(&ToolName::new("env.vfs_capture"))
            );
            for api_kind in [
                ProviderApiKind::OpenAiResponses,
                ProviderApiKind::AnthropicMessages,
                ProviderApiKind::OpenAiCompletions,
            ] {
                let visible = tools::runtime::ToolCatalog::from_registrations(
                    &registered.tools,
                    &tools::runtime::ToolTarget::api_kind(api_kind),
                )
                .unwrap();
                for (name, id) in [
                    ("vfs_materialize", "vfs.materialize"),
                    ("vfs_capture", "vfs.capture"),
                ] {
                    assert_eq!(
                        visible.get(&ToolName::new(name)).is_some(),
                        registered.tools.contains_key(&ToolName::new(id))
                    );
                    if let Some(binding) = visible.get(&ToolName::new(name)) {
                        assert_eq!(binding.logical_id, id);
                    }
                }
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_stores_text_as_user_message() {
    let store = engine::storage::InMemoryBlobStore::new();

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::Text {
            origin: None,
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
    assert_eq!(entry.content.media_type.as_deref(), Some("text/plain"));
    assert_eq!(
        store
            .read_text(&entry.content.content_ref)
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
            origin: None,
            text: "   ".to_owned(),
        },
    )
    .await
    .expect_err("empty text must be rejected");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_stores_catalog_with_its_title() {
    let store = engine::storage::InMemoryBlobStore::new();

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::Catalog {
            title: " Bot directory ".to_owned(),
            text: "- infra: accepts events addressed by you\n".to_owned(),
        },
    )
    .await
    .expect("entry");

    assert_eq!(
        entry.kind,
        engine::ContextEntryKind::Catalog {
            title: "Bot directory".to_owned(),
        }
    );
    assert_eq!(entry.preview.as_deref(), Some("Bot directory"));
    assert_eq!(
        store
            .read_text(&entry.content.content_ref)
            .await
            .expect("stored text"),
        "- infra: accepts events addressed by you"
    );

    let error = context_entry_input_from_api(
        &store,
        &InputItem::Catalog {
            title: "  ".to_owned(),
            text: "x".to_owned(),
        },
    )
    .await
    .expect_err("a catalog needs a title");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_catalog_items() {
    let store = engine::storage::InMemoryBlobStore::new();

    let error = run_input_from_api(
        &store,
        &[InputItem::Catalog {
            title: "Bot directory".to_owned(),
            text: "- infra".to_owned(),
        }],
    )
    .await
    .expect_err("catalogs are context, not run input");

    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn context_entry_input_from_api_preserves_text_ref() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text("buffered room chatter").await;

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::TextRef {
            origin: None,
            blob_ref: blob_ref.as_str().to_owned(),
        },
    )
    .await
    .expect("entry");

    assert_eq!(entry.content.content_ref, blob_ref);
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
                origin: None,
                text: "what is this?".to_owned(),
            },
            InputItem::Media {
                origin: None,
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
    assert_eq!(media.content.content_ref, blob_ref);
    assert_eq!(media.content.media_type.as_deref(), Some("image/png"));
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
                origin: None,
                blob_ref: pdf_ref.as_str().to_owned(),
                mime: "application/pdf".to_owned(),
                kind: api::MediaKind::Document,
                name: Some("offer.pdf".to_owned()),
            },
            InputItem::Media {
                origin: None,
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
    assert_eq!(
        input[0].content.media_type.as_deref(),
        Some("application/pdf")
    );
    assert_eq!(input[0].preview.as_deref(), Some("[document: offer.pdf]"));
    assert_eq!(
        input[1].content.media_type.as_deref(),
        Some("text/markdown")
    );
    assert_eq!(input[1].preview.as_deref(), Some("[document: notes.md]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_unsupported_document_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let docx = run_input_from_api(
        &store,
        &[InputItem::Media {
            origin: None,
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
            origin: None,
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
            origin: None,
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "audio/ogg".to_owned(),
            kind: api::MediaKind::Audio,
            name: Some("voice.ogg".to_owned()),
        }],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 1);
    assert_eq!(input[0].content.content_ref, blob_ref);
    assert_eq!(input[0].content.media_type.as_deref(), Some("audio/ogg"));
    assert_eq!(input[0].preview.as_deref(), Some("[audio: voice.ogg]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_unsupported_media() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

    let audio = run_input_from_api(
        &store,
        &[InputItem::Media {
            origin: None,
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
            origin: None,
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
            origin: None,
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "audio/x-aac".to_owned(),
            kind: api::MediaKind::Audio,
            name: Some("clip.aac".to_owned()),
        }],
    )
    .await
    .expect("transcodable audio should be admitted");

    assert_eq!(input[0].content.content_ref, blob_ref);
    assert_eq!(input[0].content.media_type.as_deref(), Some("audio/aac"));
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
            origin: None,
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
            origin: None,
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
            origin: None,
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
    assert_eq!(entry.content.content_ref, blob_ref);
    assert_eq!(entry.content.media_type.as_deref(), Some("image/png"));
    assert_eq!(entry.preview.as_deref(), Some("[image]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_preserves_single_text_ref() {
    let store = engine::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text("hello from cas").await;

    let input = run_input_from_api(
        &store,
        &[InputItem::TextRef {
            origin: None,
            blob_ref: blob_ref.as_str().to_owned(),
        }],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 1);
    assert_eq!(input[0].content.content_ref, blob_ref);
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
                origin: None,
                text: " first ".to_owned(),
            },
            InputItem::TextRef {
                origin: None,
                blob_ref: blob_ref.as_str().to_owned(),
            },
        ],
    )
    .await
    .expect("input");

    assert_eq!(input.len(), 2);
    assert_ne!(input[0].content.content_ref, blob_ref);
    assert_eq!(input[1].content.content_ref, blob_ref);
    assert_eq!(
        store
            .read_text(&input[0].content.content_ref)
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
        None,
        vfs::CreateInlineSnapshotRequest::new(vec![
            vfs::InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
        ]),
    )
    .await
    .expect("create snapshot");
    let manifest = serde_json::to_value(snapshot.manifest).expect("manifest json");

    let committed = commit_vfs_snapshot(
        &store,
        None,
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
        None,
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
        content: engine::ContentRef::text(content_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }
}

fn test_skill_catalog(_catalog_ref: &BlobRef, skills: Vec<SkillMetadata>) -> SkillCatalogSnapshot {
    SkillCatalogSnapshot::new(skills, Vec::new())
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
        skill_id: tools::skills::SkillId::new(skill_id),
        name: name.to_owned(),
        description: format!("Use when testing {name}."),
        short_description: Some(format!("{name} skill")),
        source: tools::skills::SkillSource::Snapshot {
            root_id: "system".to_owned(),
            snapshot_ref: snapshot_ref.clone(),
        },
        scope: tools::skills::SkillScope::Global,
        enabled,
        trust: tools::skills::SkillTrustLevel::System,
        interface: None,
        dependencies: tools::skills::SkillDependencies::default(),
        location: SkillLocation::LinkedSnapshot {
            source_snapshot_ref: snapshot_ref,
            source_link_path: VfsPath::parse("/skills/system").unwrap(),
            skill_dir_path: VfsPath::parse(format!("/skills/system/{name}")).unwrap(),
            skill_doc_path: VfsPath::parse(format!("/skills/system/{name}/SKILL.md")).unwrap(),
        },
    }
}

fn test_mcp_server_record(server_id: &str, status: mcp::McpServerStatus) -> mcp::McpServerRecord {
    test_mcp_server_put(server_id, status).into_record()
}

fn test_mcp_server_put(server_id: &str, status: mcp::McpServerStatus) -> mcp::PutMcpServerRecord {
    mcp::PutMcpServerRecord {
        server_id: mcp::McpServerId::new(server_id),
        display_name: Some(format!("{server_id} MCP")),
        server_url: format!("https://{server_id}.example.com/mcp"),
        transport: mcp::RemoteMcpTransport::StreamableHttp,
        default_server_label: server_id.to_owned(),
        description: None,
        allowed_tools: None,
        execution: mcp::McpExecution::Provider,
        exposure: mcp::McpExposure::Inject,
        approval_default: mcp::McpApprovalPolicy::Never,
        defer_loading_default: None,
        allow_private_network: false,
        auth_policy: mcp::McpServerAuthPolicy::None,
        auth_grant_id: None,
        status,
        now_ms: 1,
    }
}

fn empty_resolved_toolset() -> RegisteredToolset {
    RegisteredToolset {
        tools: BTreeMap::new(),
    }
}

fn test_remote_mcp_tool(tool_name: ToolName) -> engine::ToolSpec {
    engine::ToolSpec {
        name: tool_name,
        execution: Default::default(),
        kind: engine::ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
            server_id: "crm".to_owned(),
            record_revision: 1,
            server_label: "crm".to_owned(),
            server_url: "https://crm.example.com/mcp".to_owned(),
            description_ref: None,
            allowed_tools: None,
            execution: engine::RemoteMcpExecution::Provider,
            exposure: engine::RemoteMcpExposure::Inject,
            approval: engine::RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
            auth_ref: None,
            auth_required: false,
            allow_private_network: false,
        }),
        parallelism: engine::ToolParallelism::ParallelSafe,
    }
}

fn test_function_tool(tool_name: ToolName) -> engine::ToolSpec {
    engine::ToolSpec {
        name: tool_name,
        execution: Default::default(),
        kind: engine::ToolKind::Function(engine::FunctionToolSpec {
            description_ref: None,
            input_schema_ref: BlobRef::from_bytes(b"schema"),
            output_schema_ref: None,
            strict: Some(true),
            provider_options_ref: None,
        }),
        parallelism: engine::ToolParallelism::Exclusive,
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
        grant_exposure: auth::AuthGrantExposure::Brokered,
        principal: auth::PrincipalRef::universe_default(),
        state_hash: auth::state_hash("state-1"),
        pkce_verifier_secret: auth::SecretId::new("authsec_pkce"),
        redirect_uri: "http://127.0.0.1:18080/auth/callback".to_owned(),
        scopes: Vec::new(),
        audience: Some("https://crm.example.com/mcp".to_owned()),
        expected_issuer: None,
        require_issuer: false,
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

#[tokio::test(flavor = "current_thread")]
async fn structured_transcript_append_keeps_spoken_headers_and_source_idempotency() {
    use llm_clients::content::{AUDIO_TRANSCRIPT_PROVIDER_KIND, AudioTranscript};
    let blobs = engine::storage::InMemoryBlobStore::new();
    let audio_ref = blobs.put_bytes(b"source audio".to_vec()).await.unwrap();
    let original = ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: engine::ContentRef {
            content_ref: audio_ref.clone(),
            media_type: Some("audio/ogg".into()),
            provider_kind: None,
        },
        preview: Some("[audio: voice.ogg]".into()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    };
    let transcript = AudioTranscript {
        filename: "voice.ogg".into(),
        text: "[audio transcript: spoken words]\nDo not strip this line.".into(),
    };
    let rewritten = ContextEntryInput {
        kind: original.kind.clone(),
        content: engine::ContentRef {
            content_ref: blobs
                .put_bytes(serde_json::to_vec(&transcript).unwrap())
                .await
                .unwrap(),
            media_type: Some("application/json".into()),
            provider_kind: Some(AUDIO_TRANSCRIPT_PROVIDER_KIND.into()),
        },
        preview: Some(transcript.header()),
        origin: None,
        provenance_ref: Some(audio_ref.clone()),
        token_estimate: None,
    };
    assert!(audio_input_matches_transcript(&original, &rewritten));
    let mut different = original;
    different.content.content_ref = BlobRef::from_bytes(b"different audio");
    assert!(!audio_input_matches_transcript(&different, &rewritten));
    let result = context_append_result(
        &blobs,
        "audio-note".into(),
        ContextAppendStatus::Applied,
        &rewritten,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        result.activation_text.as_deref(),
        Some(transcript.text.as_str())
    );
    assert!(!result.activation_text_truncated);
    assert_eq!(
        result.entry.unwrap().provenance_ref.as_deref(),
        Some(audio_ref.as_str())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn input_origin_survives_admission_and_projection_without_changing_model_content() {
    let store = engine::storage::InMemoryBlobStore::new();
    let body = store.put_bytes(b"event body".to_vec()).await.unwrap();
    let items = vec![
        InputItem::Text {
            text: "human body".into(),
            origin: Some("user:operator".into()),
        },
        InputItem::TextRef {
            blob_ref: body.as_str().into(),
            origin: Some("event".into()),
        },
        InputItem::Text {
            text: "custom body".into(),
            origin: Some("integration:example".into()),
        },
        InputItem::Text {
            text: "unknown body".into(),
            origin: None,
        },
    ];
    let entries = run_input_from_api(&store, &items).await.unwrap();
    let projector = api_projection::CoreAgentProjector::new(&store);
    let inputs = projector.project_input_entries(&entries).await.unwrap();
    let accepted = api_projection::project_context_entry_inputs(&entries);
    for (i, expected) in [
        Some("user:operator"),
        Some("event"),
        Some("integration:example"),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(entries[i].origin.as_deref(), expected);
        assert_eq!(accepted[i].origin.as_deref(), expected);
        let InputItem::Text { origin, text } = &inputs[i] else {
            panic!("expected text input");
        };
        assert_eq!(origin.as_deref(), expected);
        assert_eq!(
            text,
            &store
                .read_text(&entries[i].content.content_ref)
                .await
                .unwrap()
        );
        let appended = context_entry_input_from_api(&store, &items[i])
            .await
            .unwrap();
        assert_eq!(appended, entries[i]);
        let entry = engine::ContextEntry {
            entry_id: engine::ContextEntryId::new(i as u64 + 1),
            key: None,
            kind: entries[i].kind.clone(),
            content: entries[i].content.clone(),
            source: engine::ContextEntrySource::Steering {
                run_id: engine::RunId::new(1),
                steering_id: engine::SteeringId::new(1),
                input_index: i as u32,
            },
            origin: entries[i].origin.clone(),
            preview: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };
        let view = projector.project_context_entry(&entry, None).await.unwrap();
        assert_eq!(view.origin.as_deref(), expected);
        assert!(matches!(
            view.kind,
            api::ContextEntryKindView::Message {
                role: api::ContextMessageRoleView::User
            }
        ));
        assert!(matches!(
            view.source,
            Some(api::ContextEntrySourceView::Steering { .. })
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn input_origin_rejects_blank_and_oversized_values() {
    let store = engine::storage::InMemoryBlobStore::new();
    for origin in ["".to_owned(), "   ".to_owned(), "x".repeat(201)] {
        let item = InputItem::Text {
            text: "hello".into(),
            origin: Some(origin),
        };
        assert_eq!(
            run_input_from_api(&store, std::slice::from_ref(&item))
                .await
                .unwrap_err()
                .kind,
            AgentApiErrorKind::InvalidRequest
        );
        assert_eq!(
            context_entry_input_from_api(&store, &item)
                .await
                .unwrap_err()
                .kind,
            AgentApiErrorKind::InvalidRequest
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn skill_list_reads_latest_structured_provenance() {
    let blobs = engine::storage::InMemoryBlobStore::new();
    let mut state = engine::CoreAgentState::new();
    assert!(
        super::skills::skill_list_from_context(&blobs, &state)
            .await
            .unwrap()
            .catalogs
            .is_empty()
    );
    for (id, name) in [(1, "old"), (2, "current")] {
        let catalog = SkillCatalogSnapshot::new(
            vec![test_skill_metadata("skill:review", name, true)],
            Vec::new(),
        );
        let source_ref = blobs
            .put_bytes(serde_json::to_vec(&catalog).unwrap())
            .await
            .unwrap();
        let input = tools::skills::skill_catalog_context_input(&blobs, &catalog, source_ref)
            .await
            .unwrap();
        state.context.entries.push(ContextEntry {
            entry_id: engine::ContextEntryId::new(id),
            key: Some(ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY)),
            kind: input.kind,
            source: engine::ContextEntrySource::ContextEdit,
            content: input.content,
            preview: input.preview,
            origin: input.origin,
            provenance_ref: input.provenance_ref,
            token_estimate: input.token_estimate,
            supersedes: (id == 2).then(|| engine::ContextEntryId::new(1)),
        });
    }
    assert!(
        super::skills::skill_list_from_context(&blobs, &state)
            .await
            .unwrap()
            .catalogs
            .is_empty()
    );
    let mut config =
        engine_session_config_from_api(api::SessionConfig::default(), openai_model()).unwrap();
    config.features.vfs = Some(engine::VfsFeature {
        skills: Some(engine::VfsSkillsConfig::default()),
        ..Default::default()
    });
    state.lifecycle.config = Some(config);
    let response = super::skills::skill_list_from_context(&blobs, &state)
        .await
        .unwrap();
    assert_eq!(response.catalogs[0].skills[0].name, "current");
    assert_eq!(
        response.catalogs[0].catalog_ref.as_deref(),
        state.context.entries[1]
            .provenance_ref
            .as_ref()
            .map(BlobRef::as_str)
    );
    use tools::skills::environment::{
        ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY, EnvironmentSkill, EnvironmentSkillAvailability,
        EnvironmentSkillCatalog,
    };
    let vfs = response.catalogs[0].clone();
    let mut environment = EnvironmentSkillCatalog::unavailable("machine");
    environment.availability = EnvironmentSkillAvailability::Stale;
    environment.warnings = vec!["scan incomplete".into()];
    environment.skills.push(EnvironmentSkill {
        skill_id: tools::skills::SkillId::new("environment:review"),
        name: "current".into(),
        description: "same named skill".into(),
        short_description: None,
        skill_dir_path: "/skills/system/current".into(),
        skill_doc_path: "/skills/system/current/SKILL.md".into(),
    });
    let reference = blobs
        .put_bytes(serde_json::to_vec(&environment).unwrap())
        .await
        .unwrap();
    let mut entry = state.context.entries[1].clone();
    entry.entry_id = engine::ContextEntryId::new(3);
    entry.key = Some(ContextEntryKey::new(ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY));
    entry.supersedes = None;
    entry.provenance_ref = Some(reference.clone());
    state.context.entries.push(entry);
    state.environment.active_environment_id = Some(engine::EnvironmentId::new("machine"));
    let response = super::skills::skill_list_from_context(&blobs, &state)
        .await
        .unwrap();
    assert_eq!(response.catalogs.len(), 2);
    assert_eq!(response.catalogs[0], vfs);
    let projected = &response.catalogs[1];
    assert_eq!(
        projected.source,
        api::SkillCatalogSource::Environment {
            environment_id: "machine".into()
        }
    );
    assert_eq!(projected.catalog_ref.as_deref(), Some(reference.as_str()));
    assert_eq!(projected.availability, api::SkillCatalogAvailability::Stale);
    assert_eq!(projected.warnings, environment.warnings);
    assert_eq!(projected.skills[0].name, vfs.skills[0].name);
    assert_eq!(projected.skills[0].location, vfs.skills[0].location);
    let json = serde_json::to_value(&response).unwrap();
    assert!(json["catalogs"][1].get("contextKey").is_none());
    assert!(
        json["catalogs"][1]["skills"][0]["location"]
            .get("environmentId")
            .is_none()
    );
    for selection in [Some(engine::EnvironmentId::new("other")), None] {
        state.environment.active_environment_id = selection;
        assert_eq!(
            super::skills::skill_list_from_context(&blobs, &state)
                .await
                .unwrap()
                .catalogs,
            vec![vfs.clone()]
        );
    }
    state.environment.active_environment_id = Some(engine::EnvironmentId::new("machine"));
    state
        .lifecycle
        .config
        .as_mut()
        .unwrap()
        .features
        .vfs
        .as_mut()
        .unwrap()
        .skills = None;
    let disabled = super::skills::skill_list_from_context(&blobs, &state)
        .await
        .unwrap();
    assert_eq!(disabled.catalogs.len(), 1);
    assert_eq!(
        disabled.catalogs[0].source,
        api::SkillCatalogSource::Environment {
            environment_id: "machine".into()
        }
    );
    state.context.entries.retain(|entry| {
        entry.key.as_ref().unwrap().as_str() == ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY
    });
    environment.skills.clear();
    environment.availability = EnvironmentSkillAvailability::Unavailable;
    state.context.entries[0].provenance_ref = Some(
        blobs
            .put_bytes(serde_json::to_vec(&environment).unwrap())
            .await
            .unwrap(),
    );
    let response = super::skills::skill_list_from_context(&blobs, &state)
        .await
        .unwrap();
    assert_eq!(response.catalogs.len(), 1);
    assert!(response.catalogs[0].skills.is_empty());
    assert_eq!(
        response.catalogs[0].availability,
        api::SkillCatalogAvailability::Unavailable
    );
}
