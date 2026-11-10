use api::BlobPutItem;
use harness::ToolName;

use super::*;
use tools::prompts::active_prompt_instruction_entries as active_prompt_context_entries;
use tools::skills::SkillLocation;
use vfs::VfsPath;

fn active_entry(kind: ContextEntryKind) -> harness::ContextEntry {
    harness::ContextEntry {
        origin: Some("user:ada".to_owned()),
        entry_id: harness::ContextEntryId::new(3),
        key: None,
        kind,
        source: harness::ContextEntrySource::RunInput {
            run_id: harness::RunId::new(1),
            input_index: 0,
        },
        content: harness::ContentRef::text(BlobRef::from_bytes(b"original")),
        preview: None,
        provenance_ref: None,
        token_estimate: Some(harness::TokenEstimate {
            tokens: 900,
            quality: harness::TokenEstimateQuality::ProviderCounted,
        }),
        supersedes: None,
    }
}

fn text_item(text: &str) -> InputItem {
    InputItem::Text {
        origin: None,
        provenance_ref: None,
        text: text.to_owned(),
    }
}

#[test]
fn replacements_keep_the_entry_kind_and_origin() {
    let tool_result = ContextEntryKind::ToolResult {
        call_id: harness::ToolCallId::try_new("call_1").expect("call id"),
        is_error: false,
    };
    let active = active_entry(tool_result.clone());
    let item = text_item("[tool result removed by operator]");
    let input = replacement_input(
        &active,
        input::user_message_input(BlobRef::from_bytes(b"placeholder")),
        &item,
    )
    .expect("tool result takes text");
    assert_eq!(input.kind, tool_result);
    assert_eq!(input.origin.as_deref(), Some("user:ada"));
    assert_eq!(
        input.token_estimate,
        Some(harness::TokenEstimate {
            tokens: 9,
            quality: harness::TokenEstimateQuality::Estimated,
        })
    );

    let media = InputItem::Media {
        origin: None,
        blob_ref: BlobRef::from_bytes(b"png").as_str().to_owned(),
        mime: "image/png".to_owned(),
        kind: api::MediaKind::Image,
        name: None,
    };
    let failure = replacement_input(
        &active,
        input::user_message_input(BlobRef::from_bytes(b"png")),
        &media,
    )
    .expect_err("tool result refuses media");
    assert_eq!(failure.kind, InputAdmissionFailureKind::AdmissionRejected);

    let assistant = active_entry(ContextEntryKind::Message {
        role: ContextMessageRole::Assistant,
    });
    let failure = replacement_input(
        &assistant,
        input::user_message_input(BlobRef::from_bytes(b"x")),
        &item,
    )
    .expect_err("assistant output is not replaceable");
    assert_eq!(failure.kind, InputAdmissionFailureKind::AdmissionRejected);
}

#[test]
fn admission_failure_mapping_uses_gateway_error_kinds() {
    assert_eq!(
        failure(AgentAdmissionFailureKind::RejectedCommand)
            .api_error()
            .kind,
        AgentApiErrorKind::Rejected
    );
    let with_rejection = |rejection| AgentAdmissionFailure {
        rejection: Some(rejection),
        ..failure(AgentAdmissionFailureKind::RejectedCommand)
    };
    use harness::CommandRejectionKind as Kind;
    assert_eq!(
        with_rejection(harness::CommandRejection::context_revision_conflict(1, 2))
            .api_error()
            .kind,
        AgentApiErrorKind::Conflict
    );
    for (kind, expected) in [
        (Kind::Invalid, AgentApiErrorKind::InvalidRequest),
        (
            Kind::ProviderCompatibility,
            AgentApiErrorKind::InvalidRequest,
        ),
        (Kind::RevisionConflict, AgentApiErrorKind::Conflict),
        (Kind::Conflict, AgentApiErrorKind::Conflict),
        (Kind::Internal, AgentApiErrorKind::Internal),
        (Kind::ActiveWork, AgentApiErrorKind::Rejected),
        (Kind::SessionLifecycle, AgentApiErrorKind::Rejected),
        (Kind::Refused, AgentApiErrorKind::Rejected),
    ] {
        let error = with_rejection(harness::CommandRejection::new(kind, "the reason")).api_error();
        assert_eq!(error.kind, expected);
        assert_eq!(error.message, "the reason");
    }
}

#[test]
fn declared_workflow_tools_api_maps_bound_promise_function_tools() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let reply_schema_ref = BlobRef::from_bytes(br#"{"type":"string"}"#);
    let declaration = declared_workflow_tools_from_api(SessionIntegrationInput {
        version: 1,
        lifecycle_controller: Some(WorkflowEndpointInput {
            workflow_id: "controller-1".to_owned(),
            workflow_kind: "order.workflow".to_owned(),
        }),
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                workflow_tool_id: "accept-order".to_owned(),
                schema_revision: 2,
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
    .expect("map declared workflow tools");

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
    assert_eq!(tool.definition.tool.id.as_str(), "accept_order");
    let ToolKind::Function(function) = &tool.definition.tool.kind else {
        panic!("API workflow tool must map to a function tool");
    };
    assert_eq!(function.input_schema_ref, input_schema_ref);
    assert_eq!(function.strict, Some(true));
}

#[test]
fn declared_workflow_tools_api_maps_bound_accepted_function_tools() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let declaration = declared_workflow_tools_from_api(SessionIntegrationInput {
        version: 1,
        lifecycle_controller: None,
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                workflow_tool_id: "channel-noop".to_owned(),
                schema_revision: 1,
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
fn declared_workflow_tools_api_maps_joined_completion() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let reply_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let declaration = declared_workflow_tools_from_api(SessionIntegrationInput {
        version: 1,
        lifecycle_controller: None,
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                workflow_tool_id: "message-send".to_owned(),
                schema_revision: 1,
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
                deadline_after_ms: Some(30_000),
            },
        }],
    })
    .expect("map Joined workflow tool");

    assert_eq!(
        declaration.tools[0].completion,
        WorkflowToolCompletion::Joined {
            reply_schema_ref: Some(reply_schema_ref),
            deadline_after_ms: Some(30_000),
        }
    );
}

#[test]
fn declared_workflow_tools_api_maps_start_targets_and_reply_completion() {
    let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
    let recipe_bytes = br#"{"workflowType":"ChannelsSendWorkflow","taskQueue":"channels"}"#;
    let recipe_ref = BlobRef::from_bytes(recipe_bytes);
    let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(recipe_bytes);
    let declaration = declared_workflow_tools_from_api(SessionIntegrationInput {
        version: 1,
        lifecycle_controller: None,
        tools: vec![WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                workflow_tool_id: "channel-send".to_owned(),
                schema_revision: 1,
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
                    recipe_revision: 3,
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
                recipe_revision: 3,
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
fn declared_workflow_tools_api_rejects_an_empty_integration_document() {
    assert_eq!(
        declared_workflow_tools_from_api(SessionIntegrationInput {
            version: 1,
            lifecycle_controller: None,
            tools: Vec::new(),
        })
        .expect_err("empty declared creation must fail")
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
        vec![harness::RunTerminalNotifyIntent {
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
    let environment_id = harness::EnvironmentId::new("local");
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
fn declared_mcp_attachment_materializes_remote_tool() {
    let tool_id = ToolId::new("mcp_crm");
    let active = BTreeMap::new();
    let mut record = test_mcp_server_record("durable-crm-server", mcp::McpServerStatus::Active);
    record.default_server_label = "crm".to_owned();
    record.allowed_tools = Some(vec!["lookup_customer".to_owned()]);
    record.approval = mcp::McpApprovalPolicy::Never;
    record.defer_loading = Some(true);
    let attachment = harness::McpServerAttachment {
        server_id: "durable-crm-server".to_owned(),
        tools: None,
    };

    let tool = mcp_api::mcp_tool_from_config_attachment(&attachment, &record, None)
        .expect("materialize MCP tool from config attachment");
    let desired = BTreeMap::from([(tool.id.clone(), tool)]);
    let patch = temporal_workflow::session_toolset_patch(&active, &desired);
    let tools = patch.apply_to(&active).expect("apply MCP patch");

    let tool = tools.get(&tool_id).expect("MCP tool");
    let harness::ToolKind::RemoteMcp(spec) = &tool.kind else {
        panic!("expected remote MCP tool");
    };
    assert_eq!(spec.server_id, "durable-crm-server");
    assert_eq!(spec.server_label, "crm");
    assert_eq!(spec.allowed_tools, Some(vec!["lookup_customer".to_owned()]));
    assert_eq!(spec.approval, harness::RemoteMcpApprovalPolicy::Never);
    assert_eq!(spec.defer_loading, Some(true));
    // The workflow selects activity options from this alone: a remote call that
    // may have had an effect is never retried.
    assert_eq!(
        tool.execution,
        harness::ToolExecutionSpec::new(harness::ToolExecutionClass::RemoteInteractive, false)
    );
}

#[test]
fn mcp_attachment_subsets_constrain_every_execution_and_exposure_mode() {
    for (execution, exposure) in [
        (mcp::McpExecution::Provider, mcp::McpExposure::Inject),
        (mcp::McpExecution::Runtime, mcp::McpExposure::Inject),
        (mcp::McpExecution::Runtime, mcp::McpExposure::Search),
    ] {
        let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
        record.execution = execution;
        record.exposure = exposure;
        record.allowed_tools = Some(vec!["search".into(), "delete_customer".into()]);
        let attachment = harness::McpServerAttachment {
            server_id: "crm".into(),
            tools: Some(vec!["search".into()]),
        };
        for allowlist in [record.allowed_tools.clone(), None] {
            record.allowed_tools = allowlist;
            let tool =
                mcp_api::mcp_tool_from_config_attachment(&attachment, &record, None).unwrap();
            let harness::ToolKind::RemoteMcp(spec) = tool.kind else {
                panic!("expected remote MCP tool");
            };
            assert_eq!(spec.allowed_tools, Some(vec!["search".into()]));
        }
        // Record edits must not let a retained session attachment restore revoked tools.
        record.allowed_tools = Some(vec!["delete_customer".into()]);
        let error =
            mcp_api::mcp_tool_from_config_attachment(&attachment, &record, None).unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);

        let unrestricted_attachment = harness::McpServerAttachment {
            tools: None,
            ..attachment
        };
        let tool =
            mcp_api::mcp_tool_from_config_attachment(&unrestricted_attachment, &record, None)
                .unwrap();
        let harness::ToolKind::RemoteMcp(spec) = tool.kind else {
            panic!("expected remote MCP tool");
        };
        assert_eq!(spec.allowed_tools, record.allowed_tools);
    }
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
        created_by: Attribution::Local,
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

fn header_import_params(exposure: api::AuthGrantExposure) -> AuthGrantImportParams {
    serde_json::from_value(serde_json::json!({
        "exposure": exposure,
        "headers": [
            {"name": "X-Upstream-Url", "value": "https://tracker.example.com"},
            {"name": "X-Upstream-Token", "value": "upstream-secret"}
        ]
    }))
    .expect("header import params")
}

#[test]
fn custom_header_imports_are_validated_before_any_write() {
    let error = auth_api::auth_grant_import_draft(
        header_import_params(api::AuthGrantExposure::Retrievable),
        10,
    )
    .err()
    .expect("retrievable header credentials are rejected");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);

    let mut both = header_import_params(api::AuthGrantExposure::Brokered);
    both.token = Some("also-a-token".to_owned());
    let error = auth_api::auth_grant_import_draft(both, 10)
        .err()
        .expect("token and headers together are rejected");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);

    let mut neither = header_import_params(api::AuthGrantExposure::Brokered);
    neither.headers.clear();
    let error = auth_api::auth_grant_import_draft(neither, 10)
        .err()
        .expect("an import needs a token or headers");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);

    let mut reserved = header_import_params(api::AuthGrantExposure::Brokered);
    reserved.headers[0].name = "Mcp-Session-Id".to_owned();
    let error = auth_api::auth_grant_import_draft(reserved, 10)
        .err()
        .expect("reserved headers are rejected");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    assert!(!error.message.contains("upstream-secret"));
}

#[tokio::test(flavor = "current_thread")]
async fn custom_header_imports_record_names_but_never_values() {
    let mut params = header_import_params(api::AuthGrantExposure::Brokered);
    // A caller cannot misstate the names: the server records them itself.
    params.metadata = Some(serde_json::json!({"headerNames": ["Forged"], "team": "ops"}));
    let Ok(draft) = crate::gateway::request_context::with_request_context(
        crate::gateway::request_context::RequestContext::local(api::AccessScope::Deployment),
        async { auth_api::auth_grant_import_draft(params, 10) },
    )
    .await
    else {
        panic!("header import draft");
    };

    assert_eq!(
        draft.grant.provider_kind,
        auth::AuthProviderKind::StaticHeaders
    );
    assert_eq!(
        draft.grant.metadata,
        serde_json::json!({"headerNames": ["X-Upstream-Url", "X-Upstream-Token"], "team": "ops"})
    );
    assert!(!draft.grant.metadata.to_string().contains("upstream-secret"));

    let view = auth_api::auth_grant_view(draft.grant.into_record());
    assert_eq!(
        view.header_names,
        vec!["X-Upstream-Url", "X-Upstream-Token"]
    );
    let wire = serde_json::to_string(&view).expect("serialize view");
    assert!(wire.contains("\"headerNames\""));
    assert!(!wire.contains("upstream-secret"));
}

#[test]
fn token_consumers_refuse_to_bind_custom_header_grants() {
    let headers = test_auth_grant_record(
        "authgrant_headers",
        auth::AuthProviderKind::StaticHeaders,
        auth::AuthGrantStatus::Active,
        None,
    );
    let error = auth_api::require_token_grant(&headers, "a model provider")
        .expect_err("header grants cannot back a model provider");
    assert_eq!(error.kind, AgentApiErrorKind::Rejected);
    assert!(error.message.contains("custom-headers"));
    let bearer = test_auth_grant_record(
        "authgrant_bearer",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );
    auth_api::require_token_grant(&bearer, "an environment variable")
        .expect("token grants bind as before");
}

#[test]
fn only_custom_header_grants_expose_header_names() {
    let mut bearer = test_auth_grant_record(
        "authgrant_bearer",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );
    bearer.metadata = serde_json::json!({"headerNames": ["X-Not-Headers"]});
    assert!(auth_api::auth_grant_view(bearer).header_names.is_empty());
}

#[test]
fn header_policies_bind_only_custom_header_grants() {
    let mut record = test_mcp_server_record("tracker", mcp::McpServerStatus::Active);
    record.execution = mcp::McpExecution::Runtime;
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredHeaders;
    let headers = test_auth_grant_record(
        "authgrant_headers",
        auth::AuthProviderKind::StaticHeaders,
        auth::AuthGrantStatus::Active,
        None,
    );
    let bearer = test_auth_grant_record(
        "authgrant_bearer",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );
    auth_api::validate_mcp_grant_for_server(&record, &headers)
        .expect("header policy accepts a header grant");
    let error = auth_api::validate_mcp_grant_for_server(&record, &bearer)
        .expect_err("header policy rejects a bearer grant");
    assert_eq!(error.kind, AgentApiErrorKind::Rejected);

    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    let error = auth_api::validate_mcp_grant_for_server(&record, &headers)
        .expect_err("bearer policy rejects a header grant");
    assert_eq!(error.kind, AgentApiErrorKind::Rejected);

    let mut unbound = test_mcp_server_put("tracker", mcp::McpServerStatus::Active);
    unbound.execution = mcp::McpExecution::Runtime;
    unbound.auth_policy = mcp::McpServerAuthPolicy::RequiredHeaders;
    let error = mcp_api::validate_mcp_server_credential(&unbound, None)
        .expect_err("active required-headers server must have a binding");
    assert_eq!(error.kind, AgentApiErrorKind::Rejected);
}

fn mcp_config_attachment() -> harness::McpServerAttachment {
    harness::McpServerAttachment {
        server_id: "crm".to_owned(),
        tools: None,
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
fn mcp_attachment_leaves_url_and_credential_out_of_durable_state() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.server_url = "https://crm.example.com/s/PATH_SECRET/mcp?api_key=QUERY_SECRET".to_owned();
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    record.allow_private_network = true;
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://crm.example.com"),
    );

    let tool =
        mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, Some(&grant))
            .expect("materialize MCP tool with grant");

    let durable = serde_json::to_value(&tool).expect("durable spec");
    let spec = &durable["kind"]["remote_mcp"];
    assert_eq!(spec["server_id"], "crm");
    for fact in [
        "server_url",
        "allow_private_network",
        "auth_ref",
        "auth_required",
    ] {
        assert!(spec.get(fact).is_none(), "{fact} is durable: {spec}");
    }
    assert!(!durable.to_string().contains("SECRET"));
}

#[test]
fn mcp_attachment_rejects_revoked_grant() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Revoked,
        None,
    );

    let error =
        mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, Some(&grant))
            .expect_err("revoked grant must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_attachment_rejects_grant_kind_incompatible_with_auth_policy() {
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

    let error =
        mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, Some(&grant))
            .expect_err("bearer grant must not satisfy OAuth policy");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
}

#[test]
fn mcp_attachment_rejects_grant_audience_that_does_not_cover_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::OptionalBearer;
    record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        Some("https://other.example.com"),
    );

    let error =
        mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, Some(&grant))
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

    let error =
        mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, Some(&grant))
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

    let mut work_attachment = mcp_config_attachment();
    work_attachment.server_id = "crm_work".to_owned();
    let mut personal_attachment = mcp_config_attachment();
    personal_attachment.server_id = "crm_personal".to_owned();
    let work_tool =
        mcp_api::mcp_tool_from_config_attachment(&work_attachment, &work, Some(&work_grant))
            .expect("work");
    let personal_tool = mcp_api::mcp_tool_from_config_attachment(
        &personal_attachment,
        &personal,
        Some(&personal_grant),
    )
    .expect("personal");
    let desired = BTreeMap::from([
        (work_tool.id.clone(), work_tool),
        (personal_tool.id.clone(), personal_tool),
    ]);

    let tools = temporal_workflow::session_toolset_patch(&BTreeMap::new(), &desired)
        .apply_to(&BTreeMap::new())
        .expect("both identities may coexist when their server labels differ");
    assert_eq!(tools.len(), 2);
}

#[test]
fn mcp_attachment_rejects_grant_for_no_auth_server() {
    let record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    let grant = test_auth_grant_record(
        "authgrant_1",
        auth::AuthProviderKind::StaticBearer,
        auth::AuthGrantStatus::Active,
        None,
    );

    let error =
        mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, Some(&grant))
            .expect_err("grant on no-auth server must be rejected");

    assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
}

#[test]
fn mcp_attachment_requires_grant_for_required_auth_server() {
    let mut record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    record.auth_policy = mcp::McpServerAuthPolicy::RequiredBearer;

    let error = mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, None)
        .expect_err("missing grant must be rejected for required auth");

    assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
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
        entry_id: harness::ContextEntryId::new(1),
        key: Some(ContextEntryKey::new(format!(
            "{}.0000.project",
            tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
        ))),
        kind: input.kind,
        source: harness::ContextEntrySource::ContextEdit,
        content: input.content,
        preview: input.preview,
        origin: input.origin,
        provenance_ref: input.provenance_ref,
        token_estimate: input.token_estimate,
        supersedes: None,
    };
    let mut state = harness::CoreAgentState::new();
    state.context.entries = vec![entry];

    let active_entries = active_prompt_context_entries(&state);

    assert_eq!(active_entries.len(), 1);
    assert_eq!(active_entries[0].provenance_ref.as_ref(), Some(&report_ref));
}

#[test]
fn session_start_config_maps_reasoning_and_max_output_tokens() {
    let config = harness_session_config_from_api(
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
    let error = harness_session_config_from_api(
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
    let config = harness_session_config_from_api(
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
        Some(harness::ToolChoice::Specific {
            tool_id: ToolId::new("web_fetch")
        })
    );
    assert_eq!(config.generation.parallel_tool_use, Some(false));
}

#[test]
fn session_start_config_maps_provider_triggered_compaction() {
    let config = harness_session_config_from_api(
        api::SessionConfig {
            context: Some(api::ContextConfig {
                input_limit_tokens: None,
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
    let config = harness_session_config_from_api(
        api::SessionConfig {
            context: Some(api::ContextConfig {
                input_limit_tokens: None,
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
        harness_session_config_from_api(api::SessionConfig::default(), openai_model())
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
        harness_session_config_from_api(api::SessionConfig::default(), openai_model())
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
        harness::ToolChoice::RequiredAny
    );
}

#[test]
fn run_start_without_overrides_keeps_session_defaults_out_of_run_config() {
    let mut session_config =
        harness_session_config_from_api(api::SessionConfig::default(), openai_model())
            .expect("session config");
    session_config.generation.max_output_tokens = Some(4096);
    session_config.generation.reasoning_effort = Some("high".to_owned());
    session_config.limits.max_turns = Some(12);

    let run_config = run_config_for_start(&session_config, None).expect("run config");

    assert_eq!(run_config, RunConfig::default());
}

#[test]
fn session_processing_tier_maps_all_openai_tiers() {
    for (tier, expected) in [
        (
            api::ModelProcessingTier::Standard,
            harness::ModelProcessingTier::Standard,
        ),
        (
            api::ModelProcessingTier::Fast,
            harness::ModelProcessingTier::Fast,
        ),
        (
            api::ModelProcessingTier::Flex,
            harness::ModelProcessingTier::Flex,
        ),
    ] {
        let config = harness_session_config_from_api(
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
    let session_config = harness_session_config_from_api(
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
        Some(harness::ModelProcessingTier::Fast)
    );
}

#[test]
fn session_processing_tier_rejects_compatible_custom_providers() {
    let mut model = openai_model();
    model.provider_id = "deepseek".to_owned();
    model.api_kind = ProviderApiKind::OpenAiCompletions;
    let error = harness_session_config_from_api(
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
        harness_session_config_from_api(api::SessionConfig::default(), openai_model())
            .expect("session config");
    session_config.limits.max_turns = Some(12);

    let run_config = run_config_for_start(
        &session_config,
        Some(RunStartConfig {
            model: None,
            generation: None,
            limits: Some(api::RunLimitsConfig { max_turns: Some(4) }),
        }),
    )
    .expect("run config");

    assert_eq!(run_config.max_turns, Some(4));
}

#[test]
fn existing_run_submission_rejects_completed_duplicate_with_different_input() {
    let submission_id = SubmissionId::new("submit_retry");
    let run_config = RunConfig::default();
    let original_input = vec![test_user_message_input(BlobRef::from_bytes(b"original"))];
    let changed_input = vec![test_user_message_input(BlobRef::from_bytes(b"changed"))];
    let original_source = harness::RunRequestSource::Input {
        input: original_input.clone(),
    };
    let changed_source = harness::RunRequestSource::Input {
        input: changed_input,
    };
    let mut state = harness::CoreAgentState::new();
    state.runs.finished.push(harness::RunRecord {
        notify_on_terminal: Vec::new(),
        run_id: RunId::new(7),
        status: RunStatus::Completed,
        submission_id: Some(submission_id.clone()),
        submission_digest: harness::request_run_submission_digest(
            &original_source,
            &run_config,
            &[],
        ),
        source: harness::RunSource::Input {
            input: original_input,
        },
        first_seq: harness::EventSeq::new(1),
        terminal_seq: harness::EventSeq::new(1),
        accepted_at_ms: 1,
        started_at_ms: Some(1),
        finished_at_ms: 1,
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
            &[harness::RunTerminalNotifyIntent {
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
    let config = harness_session_config_from_api(api::SessionConfig::default(), openai_model())
        .expect("map config");

    assert_eq!(config.features, harness::FeaturesConfig::default());
    assert!(config.features.web.is_none());
    assert!(config.features.vfs.is_none());
}

fn environments_feature(attachments: Vec<api::EnvironmentAttachment>) -> api::EnvironmentsFeature {
    api::EnvironmentsFeature {
        version: api::CURRENT_FEATURE_VERSION,
        selection: true,
        prompts: None,
        skills: None,
        environments: attachments,
    }
}

fn environment_attachment(id: Option<&str>, inherit: bool) -> api::EnvironmentAttachment {
    api::EnvironmentAttachment {
        environment_id: id.map(str::to_owned),
        inherit,
        default: false,
        access: api::EnvironmentAccess::Jobs,
        working_directory: Some("/srv".to_owned()),
    }
}

#[test]
fn environment_attachments_map_to_grants_and_reject_inherit_in_session_config() {
    let default_feature: api::EnvironmentsFeature =
        serde_json::from_value(serde_json::json!({})).expect("empty environment feature");
    assert!(!default_feature.selection);
    assert!(default_feature.environments.is_empty());

    let config = harness_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                environments: Some(environments_feature(vec![environment_attachment(
                    Some("env_a"),
                    false,
                )])),
                ..api::FeaturesConfig::default()
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map environment attachment");
    let environments = config.features.environments.expect("environment feature");
    assert!(environments.selection);
    assert_eq!(
        environments.tool_access(),
        Some(harness::EnvironmentAccess::Jobs)
    );
    assert_eq!(
        environments
            .attachment("env_a")
            .unwrap()
            .working_directory
            .as_deref(),
        Some("/srv")
    );

    for attachment in [
        environment_attachment(None, true),
        environment_attachment(Some("env_a"), true),
        environment_attachment(None, false),
    ] {
        let error = harness_session_config_from_api(
            api::SessionConfig {
                features: Some(api::FeaturesConfig {
                    environments: Some(environments_feature(vec![attachment])),
                    ..api::FeaturesConfig::default()
                }),
                ..api::SessionConfig::default()
            },
            openai_model(),
        )
        .expect_err("inherit and malformed attachments are rejected in session config");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    }
}

#[test]
fn web_feature_grant_maps_search_and_fetch() {
    let config = harness_session_config_from_api(
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
    let config = harness_session_config_from_api(
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
        harness::DomainError::ProviderCompatibility(_)
    ));
}

#[test]
fn web_search_accepts_anthropic_messages() {
    let config = harness_session_config_from_api(
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
fn vfs_attachments_derive_the_tool_surface() {
    fn vfs(workspaces: Vec<api::WorkspaceAttachment>) -> api::SessionConfig {
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                vfs: Some(api::VfsFeature {
                    working_directory: None,
                    version: api::CURRENT_FEATURE_VERSION,
                    workspaces,
                    prompts: None,
                    skills: None,
                }),
                ..api::FeaturesConfig::default()
            }),
            ..api::SessionConfig::default()
        }
    }
    fn attachment(path: &str, access: api::WorkspaceAccess) -> api::WorkspaceAttachment {
        api::WorkspaceAttachment {
            path: path.to_owned(),
            workspace_id: Some("ws_1".to_owned()),
            snapshot_ref: None,
            access,
        }
    }
    for (workspaces, expected) in [
        (vec![], None),
        (
            vec![attachment("/ref", api::WorkspaceAccess::Read)],
            Some(harness::WorkspaceAccess::Read),
        ),
        (
            vec![
                attachment("/ref", api::WorkspaceAccess::Read),
                attachment("/workspace", api::WorkspaceAccess::Edit),
            ],
            Some(harness::WorkspaceAccess::Edit),
        ),
    ] {
        let config =
            harness_session_config_from_api(vfs(workspaces), openai_model()).expect("map config");
        assert_eq!(
            config.features.vfs.expect("vfs feature").tool_access(),
            expected
        );
    }

    let both = api::WorkspaceAttachment {
        snapshot_ref: Some(format!("sha256:{}", "a".repeat(64))),
        ..attachment("/both", api::WorkspaceAccess::Read)
    };
    let neither = api::WorkspaceAttachment {
        workspace_id: None,
        ..attachment("/neither", api::WorkspaceAccess::Read)
    };
    for invalid in [both, neither] {
        let error = harness_session_config_from_api(vfs(vec![invalid]), openai_model())
            .expect_err("an attachment names exactly one resource");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    }
}

#[test]
fn profile_and_session_grants_derive_vfs_transfer_tools() {
    for environments in [false, true] {
        for surface in [None, Some("none"), Some("read"), Some("edit")] {
            let mut features = serde_json::json!({});
            if let Some(surface) = surface {
                features["vfs"] = serde_json::json!({});
                if surface != "none" {
                    features["vfs"]["workspaces"] = serde_json::json!([
                        {"path":"/workspace","workspaceId":"ws_1","access":surface}
                    ]);
                }
            }
            if environments {
                // Selection and jobs are independent of transfer availability.
                features["environments"] = serde_json::json!({
                    "environments":[{"environmentId":"env_a","access":"edit"}]
                });
            }
            let profile: api::ProfileDocument =
                serde_json::from_value(serde_json::json!({"config": {"features": features}}))
                    .unwrap();
            let config =
                harness_session_config_from_api(profile.config.unwrap(), openai_model()).unwrap();
            let toolset_config =
                GatewayAgentApi::session_toolset_config(&config, environments, false);
            let registered = tools::toolset::register_toolset(&toolset_config).unwrap();
            for (id, expected) in [
                (
                    "vfs.materialize",
                    environments && matches!(surface, Some("read" | "edit")),
                ),
                ("vfs.capture", environments && surface == Some("edit")),
            ] {
                assert_eq!(
                    registered.tools.contains_key(&ToolId::new(id)),
                    expected,
                    "surface={surface:?}, environments={environments}, tool={id}"
                );
            }
            assert!(
                !registered
                    .tools
                    .contains_key(&ToolId::new("env.vfs_materialize"))
            );
            assert!(
                !registered
                    .tools
                    .contains_key(&ToolId::new("env.vfs_capture"))
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
                        registered.tools.contains_key(&ToolId::new(id))
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
    let store = harness::storage::InMemoryBlobStore::new();

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::Text {
            provenance_ref: None,
            origin: None,
            text: " [telegram] Alice (12:01): hi ".to_owned(),
        },
    )
    .await
    .expect("entry");

    assert_eq!(
        entry.kind,
        harness::ContextEntryKind::Message {
            role: harness::ContextMessageRole::User,
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
    let store = harness::storage::InMemoryBlobStore::new();

    let error = context_entry_input_from_api(
        &store,
        &InputItem::Text {
            provenance_ref: None,
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
    let store = harness::storage::InMemoryBlobStore::new();

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
        harness::ContextEntryKind::Catalog {
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
    let store = harness::storage::InMemoryBlobStore::new();

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
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text("buffered room chatter").await;

    let entry = context_entry_input_from_api(
        &store,
        &InputItem::TextRef {
            provenance_ref: None,
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
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store
        .put_bytes(vec![0x89, 0x50, 0x4e, 0x47])
        .await
        .expect("store image");

    let input = run_input_from_api(
        &store,
        &[
            InputItem::Text {
                provenance_ref: None,
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
        harness::ContextEntryKind::Message {
            role: harness::ContextMessageRole::User,
        }
    );
    assert_eq!(media.content.content_ref, blob_ref);
    assert_eq!(media.content.media_type.as_deref(), Some("image/png"));
    assert_eq!(media.preview.as_deref(), Some("[image: photo.png]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_maps_document_media_to_user_message_entry() {
    let store = harness::storage::InMemoryBlobStore::new();
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
async fn run_input_from_api_bounds_media_names() {
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(b"png".to_vec()).await.unwrap();
    let input = |name: String| {
        vec![InputItem::Media {
            origin: None,
            blob_ref: blob_ref.as_str().to_owned(),
            mime: "image/png".to_owned(),
            kind: api::MediaKind::Image,
            name: Some(name),
        }]
    };
    run_input_from_api(&store, &input("é".repeat(256)))
        .await
        .expect("a name at the limit");
    let error = run_input_from_api(&store, &input("é".repeat(257)))
        .await
        .expect_err("a longer name");
    assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_unsupported_document_media() {
    let store = harness::storage::InMemoryBlobStore::new();
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
async fn run_input_from_api_rejects_unprepared_audio() {
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store
        .put_bytes(b"OggS fake voice note".to_vec())
        .await
        .expect("store audio");

    let item = InputItem::Media {
        origin: None,
        blob_ref: blob_ref.to_string(),
        mime: "audio/ogg".into(),
        kind: api::MediaKind::Audio,
        name: Some("voice.ogg".into()),
    };
    let run_error = run_input_from_api(&store, std::slice::from_ref(&item))
        .await
        .expect_err("audio requires transcription");
    let append_error = context_entry_input_from_api(&store, &item)
        .await
        .expect_err("context audio requires transcription");
    assert_eq!(run_error.kind, AgentApiErrorKind::InvalidRequest);
    assert_eq!(append_error, run_error);
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_rejects_unsupported_media() {
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store.put_bytes(vec![1, 2, 3]).await.expect("store blob");

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
async fn context_entry_input_from_api_accepts_media() {
    let store = harness::storage::InMemoryBlobStore::new();
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
        harness::ContextEntryKind::Message {
            role: harness::ContextMessageRole::User,
        }
    );
    assert_eq!(entry.content.content_ref, blob_ref);
    assert_eq!(entry.content.media_type.as_deref(), Some("image/png"));
    assert_eq!(entry.preview.as_deref(), Some("[image]"));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_preserves_single_text_ref() {
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text("hello from cas").await;

    let input = run_input_from_api(
        &store,
        &[InputItem::TextRef {
            provenance_ref: None,
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
        harness::ContextEntryKind::Message {
            role: harness::ContextMessageRole::User,
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_from_api_stores_text_and_preserves_refs() {
    let store = harness::storage::InMemoryBlobStore::new();
    let blob_ref = store.insert_text(" second ").await;

    let input = run_input_from_api(
        &store,
        &[
            InputItem::Text {
                provenance_ref: None,
                origin: None,
                text: " first ".to_owned(),
            },
            InputItem::TextRef {
                provenance_ref: None,
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

fn config_with_limit(input_limit_tokens: Option<u32>) -> harness::SessionConfig {
    harness_session_config_from_api(
        api::SessionConfig {
            context: Some(api::ContextConfig {
                input_limit_tokens,
                compaction: None,
            }),
            ..api::SessionConfig::default()
        },
        openai_model(),
    )
    .expect("map config")
}

/// Input whose entries carry the given text sizes, as admission stored them.
async fn text_input(
    store: &harness::storage::InMemoryBlobStore,
    sizes: &[usize],
) -> Vec<ContextEntryInput> {
    let items: Vec<_> = sizes
        .iter()
        .map(|size| InputItem::Text {
            provenance_ref: None,
            origin: None,
            text: "x".repeat(*size),
        })
        .collect();
    run_input_from_api(store, &items).await.expect("input")
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_text_is_refused_against_the_documents_limit() {
    let store = harness::storage::InMemoryBlobStore::new();
    let config = config_with_limit(Some(1_000));
    let no_lookup = async { panic!("a declared limit needs no lookup") };
    let input = text_input(&store, &[4_000]).await;
    refuse_oversized_text(&store, &input, &config, &config.model, no_lookup)
        .await
        .expect("exactly the window fits");

    let input = text_input(&store, &[4_001]).await;
    let error = refuse_oversized_text(&store, &input, &config, &config.model, async { None })
        .await
        .expect_err("over the window");
    assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
    assert!(error.message.contains("about 1001 tokens"));
    assert!(error.message.contains("window of 1000 tokens"));
}

/// Together they overflow, but compaction can still rescue them a chunk at a time.
#[tokio::test(flavor = "current_thread")]
async fn each_text_entry_is_judged_alone() {
    let store = harness::storage::InMemoryBlobStore::new();
    let config = config_with_limit(Some(1_000));
    let input = text_input(&store, &[3_000, 3_000, 3_000]).await;
    refuse_oversized_text(&store, &input, &config, &config.model, async { None })
        .await
        .expect("each entry fits");

    let input = text_input(&store, &[3_000, 5_000, 3_000]).await;
    refuse_oversized_text(&store, &input, &config, &config.model, async { None })
        .await
        .expect_err("the second does not");
}

#[tokio::test(flavor = "current_thread")]
async fn the_reported_limit_is_read_only_for_large_text() {
    let store = harness::storage::InMemoryBlobStore::new();
    let config = config_with_limit(None);
    let window = 200_000;
    let small = text_input(&store, &[60_000]).await;
    refuse_oversized_text(&store, &small, &config, &config.model, async {
        panic!("short text needs no lookup")
    })
    .await
    .expect("short text");

    let large = text_input(&store, &[4 * window + 1]).await;
    let error = refuse_oversized_text(&store, &large, &config, &config.model, async {
        Some(window as u32)
    })
    .await
    .expect_err("over the reported window");
    assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);

    refuse_oversized_text(&store, &large, &config, &config.model, async { None })
        .await
        .expect("an unknown limit refuses nothing");
}

/// A document's limit says nothing about the model a run overrides it with.
#[tokio::test(flavor = "current_thread")]
async fn a_declared_limit_does_not_cover_an_override_model() {
    let store = harness::storage::InMemoryBlobStore::new();
    let config = config_with_limit(Some(1_000));
    let other = harness::ModelSelection {
        model: "another-model".into(),
        ..config.model.clone()
    };
    let input = text_input(&store, &[8_000]).await;
    refuse_oversized_text(&store, &input, &config, &other, async { None })
        .await
        .expect("no limit is known for the other model");
}

#[tokio::test(flavor = "current_thread")]
async fn text_documents_count_as_text_and_binary_media_does_not() {
    let store = harness::storage::InMemoryBlobStore::new();
    let config = config_with_limit(Some(1_000));
    let bytes = "x".repeat(8_000);
    let blob_ref = store.insert_text(&bytes).await;
    let media = |mime: &str, kind| InputItem::Media {
        origin: None,
        blob_ref: blob_ref.as_str().to_owned(),
        mime: mime.to_owned(),
        kind,
        name: None,
    };

    let input = run_input_from_api(&store, &[media("text/markdown", MediaKind::Document)])
        .await
        .expect("input");
    refuse_oversized_text(&store, &input, &config, &config.model, async { None })
        .await
        .expect_err("a text document reaches the model as text");

    let input = run_input_from_api(
        &store,
        &[
            media("application/pdf", MediaKind::Document),
            media("image/png", MediaKind::Image),
        ],
    )
    .await
    .expect("input");
    refuse_oversized_text(&store, &input, &config, &config.model, async { None })
        .await
        .expect("bytes say little about what a PDF or image costs");
}

#[tokio::test(flavor = "current_thread")]
async fn blob_api_helpers_put_get_and_check_many() {
    let store = harness::storage::InMemoryBlobStore::new();

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
    let store = harness::storage::InMemoryBlobStore::new();
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
    let store = harness::storage::InMemoryBlobStore::new();
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
        preparation_error: None,
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
        kind: harness::ContextEntryKind::Message {
            role: harness::ContextMessageRole::User,
        },
        content: harness::ContentRef::text(content_ref),
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
        location: SkillLocation::AttachedSnapshot {
            source_snapshot_ref: snapshot_ref,
            source_attachment_path: VfsPath::parse("/skills/system").unwrap(),
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
        approval: mcp::McpApprovalPolicy::Never,
        defer_loading: None,
        allow_private_network: false,
        auth_policy: mcp::McpServerAuthPolicy::None,
        auth_grant_id: None,
        status,
        now_ms: 1,
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
        created_by: Attribution::Local,
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
async fn input_origin_survives_admission_and_projection_without_changing_model_content() {
    let store = harness::storage::InMemoryBlobStore::new();
    let body = store.put_bytes(b"event body".to_vec()).await.unwrap();
    let items = vec![
        InputItem::Text {
            provenance_ref: None,
            text: "human body".into(),
            origin: Some("user:operator".into()),
        },
        InputItem::TextRef {
            provenance_ref: None,
            blob_ref: body.as_str().into(),
            origin: Some("event".into()),
        },
        InputItem::Text {
            provenance_ref: None,
            text: "custom body".into(),
            origin: Some("integration:example".into()),
        },
        InputItem::Text {
            provenance_ref: None,
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
        let InputItem::Text { origin, text, .. } = &inputs[i] else {
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
        let entry = harness::ContextEntry {
            entry_id: harness::ContextEntryId::new(i as u64 + 1),
            key: None,
            kind: entries[i].kind.clone(),
            content: entries[i].content.clone(),
            source: harness::ContextEntrySource::Steering {
                run_id: harness::RunId::new(1),
                steering_id: harness::SteeringId::new(1),
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
    let store = harness::storage::InMemoryBlobStore::new();
    for origin in ["".to_owned(), "   ".to_owned(), "x".repeat(201)] {
        let item = InputItem::Text {
            provenance_ref: None,
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

/// Every field below reaches a jsonb column, and a display name reaches a text column,
/// and neither can hold U+0000. The caller is told here rather than having the log
/// quietly change what it sent.
#[tokio::test(flavor = "current_thread")]
async fn a_nul_in_a_callers_own_field_is_an_invalid_request() {
    let store = harness::storage::InMemoryBlobStore::new();
    let image = store.put_bytes(b"png".to_vec()).await.unwrap();
    let text_with_origin = InputItem::Text {
        provenance_ref: None,
        text: "hello".into(),
        origin: Some("a\0".to_owned()),
    };
    let media_with_name = InputItem::Media {
        origin: None,
        blob_ref: image.as_str().to_owned(),
        mime: "image/png".to_owned(),
        kind: api::MediaKind::Image,
        name: Some("a\0.png".to_owned()),
    };
    for item in [&text_with_origin, &media_with_name] {
        let error = run_input_from_api(&store, std::slice::from_ref(item))
            .await
            .unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest, "{error:?}");
        assert!(error.message.contains("NUL"), "{error:?}");
    }
    let catalog = InputItem::Catalog {
        title: "cat\0alog".to_owned(),
        text: "body".to_owned(),
    };
    for item in [&text_with_origin, &media_with_name, &catalog] {
        let error = context_entry_input_from_api(&store, item)
            .await
            .unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest, "{error:?}");
        assert!(error.message.contains("NUL"), "{error:?}");
    }
    assert_eq!(
        input::reject_nul("displayName", "bad\0name")
            .unwrap_err()
            .kind,
        AgentApiErrorKind::InvalidRequest
    );
    input::reject_nul("displayName", "fine \u{FFFD} name").expect("only NUL is refused");
}

#[tokio::test(flavor = "current_thread")]
async fn skill_list_reads_latest_structured_provenance() {
    let blobs = harness::storage::InMemoryBlobStore::new();
    let mut state = harness::CoreAgentState::new();
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
            entry_id: harness::ContextEntryId::new(id),
            key: Some(ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY)),
            kind: input.kind,
            source: harness::ContextEntrySource::ContextEdit,
            content: input.content,
            preview: input.preview,
            origin: input.origin,
            provenance_ref: input.provenance_ref,
            token_estimate: input.token_estimate,
            supersedes: (id == 2).then(|| harness::ContextEntryId::new(1)),
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
        harness_session_config_from_api(api::SessionConfig::default(), openai_model()).unwrap();
    config.features.vfs = Some(harness::VfsFeature {
        skills: Some(harness::VfsSkillsConfig::default()),
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
        root: "/skills/system".into(),
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
    entry.entry_id = harness::ContextEntryId::new(3);
    entry.key = Some(ContextEntryKey::new(ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY));
    entry.supersedes = None;
    entry.provenance_ref = Some(reference.clone());
    state.context.entries.push(entry);
    state.environment.active_environment_id = Some(harness::EnvironmentId::new("machine"));
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
    for selection in [Some(harness::EnvironmentId::new("other")), None] {
        state.environment.active_environment_id = selection;
        assert_eq!(
            super::skills::skill_list_from_context(&blobs, &state)
                .await
                .unwrap()
                .catalogs,
            vec![vfs.clone()]
        );
    }
    state.environment.active_environment_id = Some(harness::EnvironmentId::new("machine"));
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

#[test]
fn environment_access_ladder_derives_the_union_tool_surface() {
    fn config_for(attachments: serde_json::Value) -> harness::SessionConfig {
        let features = serde_json::json!({
            "vfs":{"workspaces":[{"path":"/workspace","workspaceId":"ws_1","access":"edit"}]},
            "environments":{"skills":{},"prompts":{},"environments":attachments}
        });
        harness_session_config_from_api(
            serde_json::from_value(serde_json::json!({"features":features})).unwrap(),
            openai_model(),
        )
        .unwrap()
    }
    fn assert_surface(config: &harness::SessionConfig, access: Option<harness::EnvironmentAccess>) {
        let registered = tools::toolset::register_toolset(
            &GatewayAgentApi::session_toolset_config(config, true, false),
        )
        .unwrap();
        let edit = access.is_some_and(|access| access.allows_edit());
        let exec = access.is_some_and(|access| access.allows_exec());
        for (id, granted) in [
            ("env.read_file", access.is_some()),
            ("env.grep", access.is_some()),
            ("env.glob", access.is_some()),
            ("env.list_dir", access.is_some()),
            ("env.write_file", edit),
            ("env.edit_file", edit),
            ("env.apply_patch", edit),
            ("env.run_process", exec),
            ("env.continue_process", exec),
            ("vfs.materialize", edit),
            ("vfs.capture", access.is_some()),
        ] {
            assert_eq!(
                registered.tools.contains_key(&ToolId::new(id)),
                granted,
                "{id} under {access:?}"
            );
        }
        let absent = tools::toolset::register_toolset(&GatewayAgentApi::session_toolset_config(
            config, false, false,
        ))
        .unwrap();
        assert!(!absent.tools.contains_key(&ToolId::new("env.read_file")));
        assert!(!absent.tools.contains_key(&ToolId::new("env.run_process")));
    }

    // No attachment: the feature is granted but installs no machine tools.
    assert_surface(&config_for(serde_json::json!([])), None);
    for (access, level) in [
        ("read", harness::EnvironmentAccess::Read),
        ("edit", harness::EnvironmentAccess::Edit),
        ("exec", harness::EnvironmentAccess::Exec),
        ("jobs", harness::EnvironmentAccess::Jobs),
    ] {
        assert_surface(
            &config_for(serde_json::json!([{"environmentId":"env_a","access":access}])),
            Some(level),
        );
    }
    // Two attachments install the union; the narrower one is enforced at
    // execution, not by hiding tools.
    assert_surface(
        &config_for(serde_json::json!([
            {"environmentId":"env_a","access":"read"},
            {"environmentId":"env_b","access":"exec"}
        ])),
        Some(harness::EnvironmentAccess::Exec),
    );
}

#[test]
fn a_missing_resource_names_only_its_kind_and_id() {
    assert_eq!(
        super::authorization::not_found(&ResourceRef::McpServer("crm".into())).message,
        "MCP server not found: crm"
    );
    assert_eq!(
        super::authorization::not_found(&ResourceRef::Environment("prod-1".into())).message,
        "environment not found: prod-1"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn text_inputs_preserve_generic_provenance_at_all_input_boundaries() {
    let store = harness::storage::InMemoryBlobStore::new();
    let source = store
        .put_bytes(b"source document or recording".to_vec())
        .await
        .unwrap();
    let text = "[audio transcript: quoted]\nReviewed text";
    let reference = store.put_bytes(text.as_bytes().to_vec()).await.unwrap();
    for item in [
        InputItem::Text {
            origin: Some("user:test".into()),
            text: text.into(),
            provenance_ref: Some(source.to_string()),
        },
        InputItem::TextRef {
            origin: Some("user:test".into()),
            blob_ref: reference.to_string(),
            provenance_ref: Some(source.to_string()),
        },
    ] {
        let run = run_input_from_api(&store, std::slice::from_ref(&item))
            .await
            .unwrap();
        let append = context_entry_input_from_api(&store, &item).await.unwrap();
        assert_eq!(run, vec![append.clone()]);
        assert_eq!(append.content, harness::ContentRef::text(reference.clone()));
        assert_eq!(append.provenance_ref.as_ref(), Some(&source));
        assert_eq!(append.origin.as_deref(), Some("user:test"));
        let projected = api_projection::CoreAgentProjector::new(&store)
            .project_input_entries(&run)
            .await
            .unwrap();
        assert_eq!(
            projected,
            vec![InputItem::Text {
                origin: Some("user:test".into()),
                text: text.into(),
                provenance_ref: Some(source.to_string())
            }]
        );
        let result = context_append_result("note".into(), ContextAppendStatus::Applied, &append);
        assert_eq!(
            result.entry.unwrap().provenance_ref,
            Some(source.to_string())
        );
    }
    let plain = context_entry_input_from_api(
        &store,
        &InputItem::Text {
            origin: None,
            text: "edited dictation".into(),
            provenance_ref: None,
        },
    )
    .await
    .unwrap();
    assert!(plain.provenance_ref.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn text_input_provenance_requires_an_existing_source_blob() {
    let store = harness::storage::InMemoryBlobStore::new();
    let text_ref = store.put_bytes(b"text".to_vec()).await.unwrap();
    for reference in [
        "not-a-blob".into(),
        BlobRef::from_bytes(b"missing source").to_string(),
    ] {
        for item in [
            InputItem::Text {
                origin: None,
                text: "text".into(),
                provenance_ref: Some(reference.clone()),
            },
            InputItem::TextRef {
                origin: None,
                blob_ref: text_ref.to_string(),
                provenance_ref: Some(reference.clone()),
            },
        ] {
            let run_error = run_input_from_api(&store, std::slice::from_ref(&item))
                .await
                .unwrap_err();
            let append_error = context_entry_input_from_api(&store, &item)
                .await
                .unwrap_err();
            assert_eq!(run_error.kind, AgentApiErrorKind::InvalidRequest);
            assert_eq!(append_error.kind, run_error.kind);
        }
    }
}

#[test]
fn code_mode_api_grant_round_trips_and_keeps_binding_deadline_immutable() {
    let config = harness_session_config_from_api(
        api::SessionConfig {
            features: Some(api::FeaturesConfig {
                code_mode: Some(api::CodeModeFeature {
                    timeout_ms: 45,
                    allowed_tools: Some(vec![]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        openai_model(),
    )
    .unwrap();
    config.validate().unwrap();
    let feature = config.features.code_mode.as_ref().unwrap();
    assert_eq!(feature.allowed_tools, Some(vec![]));
    assert_eq!(feature.limits.timeout_ms, 45);
    assert_eq!(feature.limits.max_tool_calls, 128);
    let projected = api_projection::session_config_to_api(&config).unwrap();
    assert_eq!(
        projected.features.unwrap().code_mode.unwrap().timeout_ms,
        45
    );

    let mut state = harness::CoreAgentState::new();
    let tool_id = WorkflowToolId::new(tools::code::CODE_EXECUTE_WORKFLOW_TOOL_ID);
    let binding = harness::WorkflowToolBinding::admit(
        uuid::Uuid::from_u128(1),
        harness::WorkflowToolDefinition {
            workflow_tool_id: tool_id.clone(),
            schema_revision: 1,
            semantic_type: tools::code::CODE_EXECUTE_WORKFLOW_SEMANTIC_TYPE.into(),
            tool: tools::definitions::register(
                "code.execute",
                Default::default(),
                harness::ToolParallelism::ParallelSafe,
                Default::default(),
            ),
        },
        harness::WorkflowToolTarget::Bound {
            receiver: harness::WorkflowEndpointRef {
                workflow_id: "receiver".into(),
                workflow_kind: "code.execution".into(),
            },
            dispatch: harness::BoundWorkflowToolDispatch::Push,
        },
        harness::WorkflowToolCompletion::Joined {
            reply_schema_ref: None,
            deadline_after_ms: Some(tools::code::CODE_EXECUTION_OVERHEAD_MS + 44),
        },
    )
    .unwrap();
    state.workflow_tools.bindings.insert(tool_id, binding);
    assert!(validate_code_deadline_for_existing_bindings(&state, &config.features).is_err());
    let mut features = config.features;
    features.code_mode.as_mut().unwrap().limits.timeout_ms = 44;
    validate_code_deadline_for_existing_bindings(&state, &features).unwrap();
}

#[test]
fn prepared_toolsets_refuse_what_every_turn_would_refuse() {
    let config = |api_kind, tool_choice| harness::SessionConfig {
        model: harness::ModelSelection {
            api_kind,
            provider_id: "provider".to_owned(),
            model: "model".to_owned(),
        },
        generation: harness::GenerationConfig {
            tool_choice,
            ..Default::default()
        },
        limits: Default::default(),
        context: Default::default(),
        features: Default::default(),
    };
    let record = test_mcp_server_record("crm", mcp::McpServerStatus::Active);
    let tool = mcp_api::mcp_tool_from_config_attachment(&mcp_config_attachment(), &record, None)
        .expect("provider-executed tool");
    let tools = BTreeMap::from([(tool.id.clone(), tool.clone())]);

    let refused = session_preparation::validate_prepared_toolset(
        &config(harness::ProviderApiKind::OpenAiCompletions, None),
        &tools,
        false,
    )
    .expect_err("completions cannot run a provider-executed MCP tool");
    assert_eq!(refused.kind, AgentApiErrorKind::InvalidRequest);
    assert!(refused.message.contains("provider-executed MCP tool"));

    let dangling = Some(harness::ToolChoice::Specific {
        tool_id: harness::ToolId::new("typo"),
    });
    let refused = session_preparation::validate_prepared_toolset(
        &config(harness::ProviderApiKind::OpenAiResponses, dangling.clone()),
        &tools,
        false,
    )
    .expect_err("the choice names no tool");
    assert_eq!(refused.kind, AgentApiErrorKind::InvalidRequest);
    assert_eq!(
        refused.message,
        "tool_choice references missing active tool typo"
    );

    // With a server left out the choice may be naming its tool. Runs fail on it,
    // which is the funnel that reports it, rather than preparation refusing them.
    session_preparation::validate_prepared_toolset(
        &config(harness::ProviderApiKind::OpenAiResponses, dangling),
        &tools,
        true,
    )
    .expect("a left-out server does not refuse preparation");

    let chosen = Some(harness::ToolChoice::Specific {
        tool_id: tool.id.clone(),
    });
    session_preparation::validate_prepared_toolset(
        &config(harness::ProviderApiKind::OpenAiResponses, chosen),
        &tools,
        false,
    )
    .expect("a real choice on an API kind that can run the tool");
}

#[test]
fn only_the_timers_grant_installs_sleep() {
    let surface = |features: harness::FeaturesConfig| {
        let mut config = session_config_for_toolset();
        config.features = features;
        tools::toolset::register_toolset(&GatewayAgentApi::session_toolset_config(
            &config, false, false,
        ))
        .unwrap()
        .tools
    };
    let timers = || Some(harness::TimersFeature::default());
    let subagents = || Some(harness::SubagentsFeature::default());

    let spawn_only = surface(harness::FeaturesConfig {
        subagents: subagents(),
        ..Default::default()
    });
    for tool in [
        harness::AWAIT_TOOL_ID,
        harness::CANCEL_TOOL_ID,
        harness::DETACH_TOOL_ID,
    ] {
        assert!(spawn_only.contains_key(&ToolId::new(tool)), "{tool}");
    }
    assert!(!spawn_only.contains_key(&ToolId::new(harness::SLEEP_TOOL_ID)));

    for features in [
        harness::FeaturesConfig {
            timers: timers(),
            ..Default::default()
        },
        harness::FeaturesConfig {
            timers: timers(),
            subagents: subagents(),
            ..Default::default()
        },
    ] {
        assert!(surface(features).contains_key(&ToolId::new(harness::SLEEP_TOOL_ID)));
    }
}

fn session_config_for_toolset() -> harness::SessionConfig {
    harness_session_config_from_api(api::SessionConfig::default(), openai_model()).unwrap()
}

fn attachment_to(server_id: &str, tools: Option<&[&str]>) -> harness::McpServerAttachment {
    harness::McpServerAttachment {
        server_id: server_id.to_owned(),
        tools: tools.map(|tools| tools.iter().map(|tool| (*tool).to_owned()).collect()),
    }
}

fn features_attaching(servers: Vec<harness::McpServerAttachment>) -> harness::FeaturesConfig {
    harness::FeaturesConfig {
        mcp: Some(harness::McpFeature {
            servers,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn a_request_only_answers_for_the_attachments_it_adds() {
    let crm = attachment_to("crm", None);
    let held = features_attaching(vec![crm.clone()]);
    let docs = attachment_to("docs", None);
    let document = features_attaching(vec![crm.clone(), docs.clone()]);

    // Creation names everything, a put only what the session does not hold.
    assert_eq!(
        api_config::NewlyNamed::in_document(&document, None).mcp(),
        [crm.clone(), docs.clone()]
    );
    assert_eq!(
        api_config::NewlyNamed::in_document(&document, Some(&held)).mcp(),
        [docs]
    );
    assert!(
        api_config::NewlyNamed::in_document(&held, Some(&held))
            .mcp()
            .is_empty()
    );

    // Narrowing a held server's tools is a new request of that server.
    let narrowed = attachment_to("crm", Some(&["search"]));
    assert_eq!(
        api_config::NewlyNamed::in_document(
            &features_attaching(vec![narrowed.clone()]),
            Some(&held)
        )
        .mcp(),
        [narrowed]
    );
}

#[test]
fn a_catalog_action_makes_an_attachment_unavailable_and_an_outage_does_not() {
    for error in [
        mcp_api::mcp_tool_from_config_attachment(
            &mcp_config_attachment(),
            &test_mcp_server_record("crm", mcp::McpServerStatus::Disabled),
            None,
        )
        .unwrap_err(),
        mcp_api::mcp_tool_from_config_attachment(
            &mcp_config_attachment(),
            &test_mcp_server_record("crm", mcp::McpServerStatus::NeedsAuthConfig),
            None,
        )
        .unwrap_err(),
        mcp_api::map_mcp_error(mcp::McpRegistryError::NotFound {
            server_id: mcp::McpServerId::new("crm"),
        }),
        map_auth_error(auth::AuthRegistryError::GrantNotFound {
            grant_id: auth::AuthGrantId::new("grant"),
        }),
    ] {
        assert!(mcp_api::mcp_attachment_is_unavailable(&error), "{error:?}");
    }
    let outage = mcp_api::map_mcp_error(mcp::McpRegistryError::Store {
        message: "connection refused".to_owned(),
    });
    assert!(!mcp_api::mcp_attachment_is_unavailable(&outage));
}

#[tokio::test(flavor = "current_thread")]
async fn the_model_is_told_which_servers_it_cannot_use_and_why() {
    let blobs = harness::storage::InMemoryBlobStore::new();
    assert_eq!(mcp_api::mcp_catalog_entry(&blobs, &[]).await.unwrap(), None);

    let unavailable = |server: &str, error: AgentApiError| mcp_api::UnavailableMcpServer {
        attachment: attachment_to(server, None),
        error,
    };
    let entry = mcp_api::mcp_catalog_entry(
        &blobs,
        &[
            unavailable(
                "crm",
                AgentApiError::rejected("MCP server is disabled: crm"),
            ),
            unavailable(
                "docs",
                AgentApiError::not_found("MCP server not found: docs"),
            ),
        ],
    )
    .await
    .unwrap()
    .expect("a notice");
    assert!(matches!(
        entry.kind,
        harness::ContextEntryKind::Catalog { .. }
    ));
    let text =
        String::from_utf8(blobs.read_bytes(&entry.content.content_ref).await.unwrap()).unwrap();
    assert!(
        text.contains("- crm: MCP server is disabled: crm\n"),
        "{text}"
    );
    assert!(
        text.contains("- docs: MCP server not found: docs\n"),
        "{text}"
    );
}

#[test]
fn a_request_only_answers_for_a_default_it_changes() {
    fn attached(id: &str, default: bool) -> harness::EnvironmentAttachment {
        harness::EnvironmentAttachment {
            environment_id: id.to_owned(),
            default,
            access: harness::EnvironmentAccess::Read,
            working_directory: None,
        }
    }
    let features = |attachments: Vec<harness::EnvironmentAttachment>| harness::FeaturesConfig {
        environments: Some(harness::EnvironmentsFeature {
            environments: attachments,
            ..Default::default()
        }),
        ..Default::default()
    };
    let on_a = features(vec![attached("a", true), attached("b", false)]);
    let named = |document: &harness::FeaturesConfig, current| {
        api_config::NewlyNamed::in_document(document, current)
            .default_environment()
            .map(str::to_owned)
    };

    assert_eq!(named(&on_a, None).as_deref(), Some("a"));
    // Saving other settings beside a default that has since closed still works.
    assert_eq!(named(&on_a, Some(&on_a)), None);
    let on_b = features(vec![attached("a", false), attached("b", true)]);
    assert_eq!(named(&on_b, Some(&on_a)).as_deref(), Some("b"));
    let no_default = features(vec![attached("a", false)]);
    assert_eq!(named(&no_default, Some(&on_a)), None);
}
