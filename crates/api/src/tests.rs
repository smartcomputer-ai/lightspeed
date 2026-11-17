use serde_json::{Value, json};

use super::*;

#[test]
fn mcp_feature_preserves_an_empty_server_list() {
    for value in [json!({}), json!({ "servers": [] })] {
        let feature: McpFeature = serde_json::from_value(value).expect("empty MCP feature");
        assert!(feature.servers.is_empty());
        assert_eq!(serde_json::to_value(feature).unwrap()["servers"], json!([]));
    }
}

#[test]
fn session_retention_put_requires_an_explicit_nullable_policy() {
    assert!(
        serde_json::from_value::<SessionRetentionPutParams>(json!({
            "sessionId": "session_1"
        }))
        .is_err()
    );
    assert_eq!(
        serde_json::from_value::<SessionRetentionPutParams>(json!({
            "sessionId": "session_1",
            "deleteAfterCloseMs": null
        }))
        .expect("explicit null clears retention")
        .delete_after_close_ms,
        None
    );
}

#[test]
fn session_start_retention_distinguishes_inherit_clear_and_override() {
    let decode = |value| {
        serde_json::from_value::<SessionStartParams>(value)
            .expect("valid session start")
            .delete_after_close_ms
    };
    assert_eq!(decode(json!({})), None);
    assert_eq!(decode(json!({ "deleteAfterCloseMs": null })), Some(None));
    assert_eq!(
        decode(json!({ "deleteAfterCloseMs": 86_400_000 })),
        Some(Some(86_400_000))
    );
}

#[test]
fn credential_grant_import_params_redact_token_in_debug_output() {
    let params: CredentialGrantImportParams = serde_json::from_value(json!({
        "grantId": "authgrant_1",
        "token": "super-secret-token",
        "audience": "https://crm.example.com/mcp"
    }))
    .expect("deserialize import params");

    let debug = format!("{params:?}");

    assert!(!debug.contains("super-secret-token"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert_eq!(params.token, "super-secret-token");
    assert_eq!(params.exposure, CredentialGrantExposure::Brokered);
}

#[test]
fn credential_grant_lease_response_redacts_token_in_debug_output() {
    let response = CredentialGrantLeaseResponse {
        token: "super-secret-leased-token".to_owned(),
        expires_at_ms: Some(1234),
        grant_id: "authgrant_1".to_owned(),
        provider_kind: CredentialProviderKind::StaticToken,
    };

    let debug = format!("{response:?}");

    assert!(!debug.contains("super-secret-leased-token"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert!(debug.contains("authgrant_1"));
}

#[test]
fn credential_client_create_params_redact_client_secret_in_debug_output() {
    let params: CredentialClientCreateParams = serde_json::from_value(json!({
        "providerKind": "customOAuth",
        "authorizationEndpoint": "https://as.example.com/authorize",
        "tokenEndpoint": "https://as.example.com/token",
        "remoteClientId": "client-1",
        "clientSecret": "super-secret-client-secret"
    }))
    .expect("deserialize client create params");

    let debug = format!("{params:?}");

    assert!(!debug.contains("super-secret-client-secret"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert_eq!(
        params.client_secret.as_deref(),
        Some("super-secret-client-secret")
    );
}

#[test]
fn credential_provider_create_params_redact_credential_in_debug_output() {
    let params: CredentialProviderCreateParams = serde_json::from_value(json!({
        "providerId": "lightspeed-github",
        "config": {"type": "githubApp", "appId": "12345"},
        "credential": "-----BEGIN RSA PRIVATE KEY-----\nsuper-secret-key"
    }))
    .expect("deserialize provider create params");

    let debug = format!("{params:?}");

    assert!(!debug.contains("super-secret-key"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert!(
        params
            .credential
            .as_deref()
            .unwrap()
            .contains("super-secret-key")
    );
}

#[test]
fn model_endpoint_headers_are_tagged_values_on_the_wire() {
    let endpoint = json!({
        "baseUrl": "https://router.example/v1",
        "headers": {
            "x-api-key": { "type": "credential", "grantId": "authgrant_router" },
            "x-title": { "type": "literal", "value": "Lightspeed" },
        },
        "apiKinds": ["openai:completions"],
    });
    let decoded: ModelEndpointConfig =
        serde_json::from_value(endpoint.clone()).expect("deserialize endpoint");
    assert_eq!(
        decoded.headers.get("x-api-key"),
        Some(&RequestHeader::Credential {
            grant_id: "authgrant_router".to_owned(),
        })
    );
    assert_eq!(
        decoded.headers.get("x-title"),
        Some(&RequestHeader::Literal {
            value: "Lightspeed".to_owned(),
        })
    );
    assert_eq!(serde_json::to_value(&decoded).unwrap(), endpoint);

    // A header is always tagged: a bare string names no value source.
    let bare = json!({
        "baseUrl": "https://router.example/v1",
        "headers": { "x-title": "Lightspeed" },
        "apiKinds": ["openai:completions"],
    });
    assert!(serde_json::from_value::<ModelEndpointConfig>(bare).is_err());

    // An endpoint without headers leaves the field out.
    let plain = ModelEndpointConfig {
        base_url: "https://router.example/v1".to_owned(),
        headers: Default::default(),
        api_kinds: vec!["openai:completions".to_owned()],
    };
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("headers")
            .is_none()
    );
}

#[test]
fn mcp_servers_and_model_endpoints_share_one_header_shape() {
    let headers = json!({
        "x-tenant": { "type": "literal", "value": "acme" },
        "x-upstream-token": { "type": "credential", "grantId": "authgrant_upstream" },
    });
    let expected = BTreeMap::from([
        (
            "x-tenant".to_owned(),
            RequestHeader::Literal {
                value: "acme".to_owned(),
            },
        ),
        (
            "x-upstream-token".to_owned(),
            RequestHeader::Credential {
                grant_id: "authgrant_upstream".to_owned(),
            },
        ),
    ]);

    let server: McpServerInput = serde_json::from_value(json!({
        "serverId": "tracker",
        "serverUrl": "https://tracker.example/mcp",
        "defaultServerLabel": "tracker",
        "headers": headers,
    }))
    .expect("deserialize server");
    assert_eq!(server.headers, expected);
    assert_eq!(serde_json::to_value(&server).unwrap()["headers"], headers);

    let endpoint: ModelEndpointConfig = serde_json::from_value(json!({
        "baseUrl": "https://router.example/v1",
        "headers": headers,
        "apiKinds": ["openai:completions"],
    }))
    .expect("deserialize endpoint");
    assert_eq!(endpoint.headers, expected);
    assert_eq!(serde_json::to_value(&endpoint).unwrap()["headers"], headers);

    for name in ["api-key", "X-Api-Key", "Proxy-Authorization"] {
        assert!(is_credential_only_header(name), "{name}");
    }
    assert!(!is_credential_only_header("x-tenant"));
    assert!(
        CREDENTIAL_ONLY_HEADER_NAMES
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );

    // The value source is always named; nothing else is accepted.
    for invalid in [
        json!("acme"),
        json!({ "value": "acme" }),
        json!({ "type": "secret", "value": "acme" }),
        json!({ "type": "credential", "grant_id": "authgrant_upstream" }),
    ] {
        assert!(
            serde_json::from_value::<RequestHeader>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn environment_provider_put_params_redact_credential_in_debug_output() {
    let params: DeploymentEnvironmentProviderPutParams = serde_json::from_value(json!({
        "providerId": "incus-local",
        "controllerEndpoint": "wss://incus.example.com/control",
        "credential": "provider-bearer-secret"
    }))
    .expect("deserialize environment provider put params");

    let debug = format!("{params:?}");

    assert!(!debug.contains("provider-bearer-secret"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert_eq!(params.credential.as_deref(), Some("provider-bearer-secret"));
}

#[test]
fn public_session_config_rejects_integration_workflow_tool_bindings() {
    let config = serde_json::from_value::<SessionConfig>(json!({
        "integration": {
            "lifecycleController": {
                "workflowId": "work/other",
                "workflowKind": "agent_work"
            },
            "bindings": []
        }
    }));

    assert!(
        config.is_err(),
        "workflow-tool bindings must not enter through public session config"
    );
}

#[test]
fn public_workflow_tool_input_rejects_runtime_builtin_definitions() {
    let input = json!({
        "name": "subagent.run",
        "kind": { "type": "builtin", "settings": {} }
    });
    let error = serde_json::from_value::<WorkflowToolSpecInput>(input.clone())
        .expect_err("runtime-owned definitions are not public workflow declarations");
    assert_eq!(error.classify(), serde_json::error::Category::Data);
    let schema = serde_json::to_value(schemars::schema_for!(WorkflowToolSpecInput))
        .expect("workflow input schema");
    assert!(!jsonschema::is_valid(&schema, &input));
}

#[test]
fn integrated_session_creation_exposes_targets_and_completion_contracts() {
    let params: SessionStartParams = serde_json::from_value(json!({
        "sessionId": "session_1",
        "integration": {
            "version": 1,
            "lifecycleController": {
                "workflowId": "controller-1",
                "workflowKind": "order.workflow"
            },
            "tools": [{
                "definition": {
                    "workflowToolId": "accept-order",
                    "schemaRevision": 1,
                    "semanticType": "orders.accepted.v1",
                    "tool": {
                        "name": "accept_order",
                        "kind": {
                            "type": "function",
                            "inputSchemaRef": "sha256:input"
                        }
                    }
                },
                "target": {
                    "type": "bound",
                    "dispatch": "push",
                    "receiver": {
                        "workflowId": "receiver-1",
                        "workflowKind": "order.receiver"
                    }
                },
                "completion": {
                    "type": "promises",
                    "replySchemaRef": "sha256:reply",
                    "deadlineAfterMs": 30000,
                    "maxPromises": 4,
                    "keySource": {
                        "type": "arrayIndices",
                        "pointer": "/messages",
                        "prefix": "message-"
                    }
                }
            }]
        }
    }))
    .expect("integrated session params");

    let integration = params.integration.expect("integration");
    assert_eq!(integration.version, 1);
    assert_eq!(
        integration.tools[0].definition.tool.parallelism,
        ToolParallelismView::ParallelSafe
    );
    assert_eq!(
        serde_json::to_value(&integration).expect("serialize integration")["tools"][0]["definition"]
            ["tool"]["parallelism"],
        json!("parallelSafe")
    );
    assert!(matches!(
        integration.tools[0].target,
        WorkflowToolTargetInput::Bound {
            dispatch: BoundWorkflowToolDispatchInput::Push,
            ..
        }
    ));
    assert!(matches!(
        integration.tools[0].completion,
        WorkflowToolCompletionInput::Promises {
            key_source: WorkflowToolCompletionKeySourceInput::ArrayIndices { .. },
            ..
        }
    ));
}

#[test]
fn integrated_session_creation_rejects_receiver_shortcut_and_tool_targets() {
    for forbidden in ["receiver", "targetRequirement"] {
        let mut tool = json!({
            "definition": {
                "workflowToolId": "accept-order",
                "schemaRevision": 1,
                "semanticType": "orders.accepted.v1",
                "tool": {
                    "name": "accept_order",
                    "kind": {
                        "type": "function",
                        "inputSchemaRef": "sha256:input"
                    }
                }
            },
            "target": {
                "type": "bound",
                "dispatch": "pull",
                "receiver": {
                    "workflowId": "receiver-1",
                    "workflowKind": "order.receiver"
                }
            },
            "completion": {"type": "accepted"}
        });
        tool[forbidden] = json!({});
        let result = serde_json::from_value::<SessionStartParams>(json!({
            "integration": {"version": 1, "tools": [tool]}
        }));
        assert!(result.is_err(), "{forbidden} must not be accepted");
    }
}

#[test]
fn integrated_session_creation_rejects_bound_target_without_dispatch() {
    let result = serde_json::from_value::<SessionStartParams>(json!({
        "integration": {
            "version": 1,
            "tools": [{
                "definition": {
                    "workflowToolId": "accept-order",
                    "schemaRevision": 1,
                    "semanticType": "orders.accepted.v1",
                    "tool": {
                        "name": "accept_order",
                        "kind": {
                            "type": "function",
                            "inputSchemaRef": "sha256:input"
                        }
                    }
                },
                "target": {
                    "type": "bound",
                    "receiver": {
                        "workflowId": "receiver-1",
                        "workflowKind": "order.receiver"
                    }
                },
                "completion": {"type": "accepted"}
            }]
        }
    }));

    assert!(result.is_err(), "bound dispatch must be explicit");
}

#[test]
fn integrated_session_creation_decodes_joined_completion_with_or_without_a_deadline() {
    let params = serde_json::from_value::<SessionStartParams>(json!({
        "integration": {
            "version": 1,
            "tools": [{
                "definition": {
                    "workflowToolId": "send-message",
                    "schemaRevision": 1,
                    "semanticType": "channels.receipt.v1",
                    "tool": {
                        "name": "message_send",
                        "kind": {"type": "function", "inputSchemaRef": "sha256:input"}
                    }
                },
                "target": {
                    "type": "bound",
                    "dispatch": "push",
                    "receiver": {
                        "workflowId": "channels-1",
                        "workflowKind": "channels.session"
                    }
                },
                "completion": {
                    "type": "joined",
                    "deadlineAfterMs": 30000
                }
            }]
        }
    }))
    .expect("Joined integrated session");
    assert!(matches!(
        params.integration.as_ref().expect("integration").tools[0].completion,
        WorkflowToolCompletionInput::Joined {
            deadline_after_ms: Some(30_000),
            ..
        }
    ));

    // The wire leaves it optional; admission requires it of a bound receiver.
    assert_eq!(
        serde_json::from_value::<WorkflowToolCompletionInput>(json!({
            "type": "joined"
        }))
        .expect("Joined without a deadline decodes"),
        WorkflowToolCompletionInput::Joined {
            reply_schema_ref: None,
            deadline_after_ms: None,
        }
    );
}

#[test]
fn session_start_rejects_creation_environment_overrides() {
    for environment in [
        json!({"type": "existing", "environmentId": "workstation"}),
        json!({"type": "none"}),
        json!(null),
    ] {
        let mut request = json!({
            "profile": {"kind": "named", "profileId": "developer"},
            "environment": environment,
        });
        let error = serde_json::from_value::<SessionStartParams>(request.clone())
            .expect_err("ordinary start rejects the removed environment field");
        assert!(error.to_string().contains("unknown field `environment`"));
        request["integration"] = json!({"version": 1, "tools": []});
        let error = serde_json::from_value::<SessionStartParams>(request)
            .expect_err("an integration does not admit the removed environment field");
        assert!(error.to_string().contains("unknown field `environment`"));
    }
}

#[test]
fn run_terminal_notification_uses_token_only_wire_shape() {
    let params: RunStartParams = serde_json::from_value(json!({
        "sessionId": "session_1",
        "source": {"type": "input", "items": []},
        "notifyOnTerminal": {"token": "promise-1"}
    }))
    .expect("run terminal notification");
    assert_eq!(
        params.notify_on_terminal.expect("notification").token,
        "promise-1"
    );
    assert!(
        serde_json::from_value::<RunStartParams>(json!({
            "sessionId": "session_1",
            "source": {"type": "input", "items": []},
            "notifyOnTerminal": {
                "token": "promise-1",
                "holderWorkflowId": "caller-chosen-destination"
            }
        }))
        .is_err()
    );
}

#[test]
fn session_processing_tier_uses_lightspeed_owned_wire_vocabulary() {
    let config: SessionConfig = serde_json::from_value(json!({
        "generation": {"processingTier": "fast"}
    }))
    .expect("session processing tier");
    assert_eq!(
        config.generation.expect("generation").processing_tier,
        Some(ModelProcessingTier::Fast)
    );

    assert!(
        serde_json::from_value::<SessionConfig>(json!({
            "generation": {"processingTier": "priority"}
        }))
        .is_err()
    );

    let run: RunStartConfig = serde_json::from_value(json!({
        "generation": {"processingTier": "flex"}
    }))
    .expect("run processing tier override");
    assert_eq!(
        run.generation.expect("generation").processing_tier,
        Some(ModelProcessingTier::Flex)
    );
}

#[test]
fn model_list_params_default_to_the_unfiltered_provider_list() {
    let params: ModelListParams = serde_json::from_value(json!({})).expect("params");
    assert!(!params.selectable_only);
    assert_eq!(
        serde_json::to_value(ModelListParams {
            selectable_only: true,
        })
        .expect("serialize"),
        json!({ "selectableOnly": true })
    );
}

#[test]
fn mcp_server_put_params_default_approval_is_never_and_revision_optional() {
    let params: McpServerPutParams = serde_json::from_value(json!({
        "server": {
            "serverId": "echo",
            "serverUrl": "https://echo.example.com/mcp",
            "defaultServerLabel": "echo"
        }
    }))
    .expect("params");

    assert_eq!(params.server.approval, RemoteMcpApprovalPolicy::Never);
    assert_eq!(params.expected_revision, None);
    assert_eq!(params.server.credential, None);
}

#[test]
fn mcp_server_put_decodes_universe_credential_grant_credential() {
    let params: McpServerPutParams = serde_json::from_value(json!({
        "server": {
            "serverId": "echo",
            "serverUrl": "https://echo.example.com/mcp",
            "defaultServerLabel": "echo",
            "authPolicy": { "type": "requiredBearer" },
            "credential": { "type": "authGrant", "grantId": "authgrant_1" }
        }
    }))
    .expect("params");

    assert_eq!(
        params.server.credential,
        Some(McpServerCredential::CredentialGrant {
            grant_id: "authgrant_1".to_owned(),
        })
    );
}

#[test]
fn mcp_server_put_rejects_internal_transport_field() {
    let error = serde_json::from_value::<McpServerPutParams>(json!({
        "server": {
            "serverId": "echo",
            "serverUrl": "https://echo.example.com/mcp",
            "defaultServerLabel": "echo",
            "transport": "streamableHttp"
        }
    }))
    .expect_err("MCP transport is not public configuration");

    assert!(error.to_string().contains("unknown field `transport`"));
}

#[test]
fn mcp_session_attachments_reject_removed_connection_and_policy_fields() {
    for (field, value) in [
        ("authGrantId", json!("authgrant_1")),
        ("allowedTools", json!(["search"])),
        ("approval", json!("never")),
        ("deferLoading", json!(true)),
    ] {
        let mut attachment = serde_json::Map::from_iter([("serverId".to_owned(), json!("echo"))]);
        attachment.insert(field.to_owned(), value);
        let error = serde_json::from_value::<McpServerAttachment>(Value::Object(attachment))
            .expect_err("session MCP attachments must reject removed fields");
        assert!(
            error.to_string().contains("unknown field"),
            "{field}: {error}"
        );
    }
}

#[test]
fn session_event_serializes_with_cursor_and_kind() {
    let event = SessionEventView {
        cursor: EventCursor { seq: 3 },
        session_id: "session_1".to_owned(),
        observed_at_ms: 12,
        joins: EventJoinsView {
            run_id: Some("run_1".to_owned()),
            ..EventJoinsView::default()
        },
        kind: SessionEventKindView::RunCompleted {
            run_id: "run_1".to_owned(),
            output: Some(ContentRefView {
                blob_ref: "sha256:abc".to_owned(),
                mime: Some("text/plain".to_owned()),
                provider_kind: None,
                media_handle: None,
            }),
        },
    };

    let value = serde_json::to_value(event).expect("serialize session event");
    assert_eq!(
        value,
        json!({
            "cursor": {"seq":3}, "sessionId":"session_1", "observedAtMs":12,
            "joins":{"runId":"run_1"},
            "kind":{"type":"runCompleted","runId":"run_1","output":{"blobRef":"sha256:abc","mime":"text/plain","providerKind":null}}
        })
    );
}

#[test]
fn tool_batch_started_event_can_inline_tool_arguments() {
    let event = SessionEventView {
        cursor: EventCursor { seq: 4 },
        session_id: "session_1".to_owned(),
        observed_at_ms: 12,
        joins: EventJoinsView {
            run_id: Some("run_1".to_owned()),
            tool_batch_id: Some("tool_batch_1".to_owned()),
            ..EventJoinsView::default()
        },
        kind: SessionEventKindView::ToolBatchStarted {
            run_id: "run_1".to_owned(),
            turn_id: "turn_1".to_owned(),
            batch_id: "tool_batch_1".to_owned(),
            calls: vec![ToolCallEventView {
                tool_id: None,
                call_id: "call_1".to_owned(),
                tool_name: "read_file".to_owned(),
                arguments_ref: "sha256:args".to_owned(),
                arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                display: Some(ToolCallDisplayView {
                    group: ToolCallDisplayGroup::Explore,
                    verb: "Read".to_owned(),
                    target: Some("README.md".to_owned()),
                    detail: None,
                }),
            }],
        },
    };

    let value = serde_json::to_value(event).expect("serialize event");

    assert_eq!(
        value["kind"]["calls"][0],
        json!({
            "callId": "call_1",
            "toolName": "read_file",
            "argumentsRef": "sha256:args",
            "arguments": "{\"path\":\"README.md\"}",
            "display": {
                "group": "explore",
                "verb": "Read",
                "target": "README.md"
            }
        })
    );
}

#[test]
fn provider_context_entry_serializes_debug_metadata() {
    let entry = ContextEntryView {
        id: "item_42".to_owned(),
        key: None,
        kind: ContextEntryKindView::ProviderOpaque,
        content: crate::ContentRefView {
            blob_ref: "sha256:compact".to_owned(),
            mime: Some("application/json".to_owned()),
            provider_kind: Some("openai.responses.compaction".to_owned()),
            media_handle: None,
        },
        origin: None,
        provenance_ref: None,
        preview: Some("OpenAI Responses compaction item".to_owned()),
        provider_item_id: Some("item_compaction_1".to_owned()),
        token_estimate: Some(TokenEstimateView {
            tokens: 123,
            quality: TokenEstimateQualityView::ProviderCounted,
        }),
        text: None,
        text_truncated: false,
        display: None,
        citations: Vec::new(),
        source: None,
        supersedes: None,
        superseded_by: None,
    };

    let value = serde_json::to_value(entry).expect("serialize provider context entry");

    assert_eq!(
        value,
        json!({
            "id": "item_42",
            "kind": { "type": "providerOpaque" },
            "content": { "blobRef": "sha256:compact", "mime": "application/json", "providerKind": "openai.responses.compaction" },
            "preview": "OpenAI Responses compaction item",
            "providerItemId": "item_compaction_1",
            "tokenEstimate": {
                "tokens": 123,
                "quality": "providerCounted"
            }
        })
    );
}

#[test]
fn provider_context_entry_serializes_mcp_display() {
    let entry = ContextEntryView {
        id: "item_43".to_owned(),
        key: None,
        kind: ContextEntryKindView::ProviderOpaque,
        content: crate::ContentRefView {
            blob_ref: "sha256:mcp".to_owned(),
            mime: Some("application/json".to_owned()),
            provider_kind: Some("openai.responses.mcp_call".to_owned()),
            media_handle: None,
        },
        origin: None,
        provenance_ref: None,
        preview: Some("OpenAI Responses MCP tool call: echo.echo".to_owned()),
        provider_item_id: Some("mcp_1".to_owned()),
        token_estimate: None,
        text: None,
        text_truncated: false,
        display: Some(ProviderContextDisplayView {
            summary: ToolCallDisplayView {
                group: ToolCallDisplayGroup::Other,
                verb: "MCP".to_owned(),
                target: Some("echo.echo".to_owned()),
                detail: None,
            },
            tool_name: "echo.echo".to_owned(),
            status: ToolItemStatus::Succeeded,
            is_error: false,
            arguments: Some(r#"{"data":"simba"}"#.to_owned()),
            output: Some("Echoing your input: simba".to_owned()),
            error: None,
        }),
        citations: Vec::new(),
        source: None,
        supersedes: None,
        superseded_by: None,
    };

    let value = serde_json::to_value(entry).expect("serialize mcp provider context entry");

    assert_eq!(
        value,
        json!({
            "id": "item_43",
            "kind": { "type": "providerOpaque" },
            "content": { "blobRef": "sha256:mcp", "mime": "application/json", "providerKind": "openai.responses.mcp_call" },
            "preview": "OpenAI Responses MCP tool call: echo.echo",
            "providerItemId": "mcp_1",
            "display": {
                "summary": {
                    "group": "other",
                    "verb": "MCP",
                    "target": "echo.echo"
                },
                "toolName": "echo.echo",
                "status": "succeeded",
                "isError": false,
                "arguments": "{\"data\":\"simba\"}",
                "output": "Echoing your input: simba"
            }
        })
    );
}

#[test]
fn run_view_can_expose_tool_batches() {
    let run = RunView {
        output: None,
        output_text: None,
        id: "run_1".to_owned(),
        status: RunStatus::Running,
        started_at_ms: Some(10),
        finished_at_ms: None,
        source: RunViewSource::Input { items: Vec::new() },
        entries: Vec::new(),
        tool_batches: vec![ToolBatchView {
            id: "tool_batch_1".to_owned(),
            turn_id: "turn_1".to_owned(),
            status: ToolItemStatus::Succeeded,
            calls: vec![ToolCallView {
                attachments: Vec::new(),
                tool_id: None,
                started_at_ms: None,
                finished_at_ms: None,
                duration_ms: None,
                media: Vec::new(),
                call_id: "call_1".to_owned(),
                tool_name: "read_file".to_owned(),
                arguments_ref: "sha256:args".to_owned(),
                arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                arguments_preview_truncated: false,
                output: Some("ok".to_owned()),
                output_ref: None,
                output_preview_truncated: false,
                output_bytes: None,
                truncated: false,
                is_error: false,
                status: ToolItemStatus::Succeeded,
                effects: Vec::new(),
                display: Some(ToolCallDisplayView {
                    group: ToolCallDisplayGroup::Explore,
                    verb: "Read".to_owned(),
                    target: Some("README.md".to_owned()),
                    detail: None,
                }),
            }],
        }],
        usage: None,
        pending_approvals: Vec::new(),
        failure: None,
        earlier_events_omitted: false,
    };

    let value = serde_json::to_value(run).expect("serialize run");

    assert_eq!(value["startedAtMs"], 10);
    assert!(value.get("finishedAtMs").is_none());
    assert_eq!(
        value["toolBatches"][0],
        json!({
            "id": "tool_batch_1",
            "turnId": "turn_1",
            "status": "succeeded",
            "calls": [{
                "callId": "call_1",
                "toolName": "read_file",
                "argumentsRef": "sha256:args",
                "arguments": "{\"path\":\"README.md\"}",
                "output": "ok",
                "isError": false,
                "status": "succeeded",
                "display": {
                    "group": "explore",
                    "verb": "Read",
                    "target": "README.md"
                }
            }]
        })
    );
}

#[test]
fn a_workflow_tool_definition_keeps_its_namespace_apart_from_the_registry_id() {
    let definition = json!({
        "workflowToolId": "acme.review",
        "schemaRevision": 1,
        "semanticType": "acme.review.v1",
        "tool": {
            "name": "request_review",
            "kind": {"type": "function", "inputSchemaRef": "sha256:schema"},
        },
    });
    let input: WorkflowToolDefinitionInput =
        serde_json::from_value(definition.clone()).expect("definition");
    assert_eq!(input.workflow_tool_id, "acme.review");
    assert_eq!(input.tool.name, "request_review");

    let mut legacy = definition;
    legacy["toolId"] = legacy["workflowToolId"].take();
    legacy.as_object_mut().unwrap().remove("workflowToolId");
    assert!(serde_json::from_value::<WorkflowToolDefinitionInput>(legacy).is_err());
}

#[test]
fn session_lifecycle_status_serializes_as_string_enum() {
    assert_eq!(
        serde_json::to_value(SessionLifecycleStatus::New).expect("serialize status"),
        json!("new")
    );
}

#[test]
fn a_new_mcp_server_defaults_to_runtime_execution() {
    let input: McpServerInput = serde_json::from_value(json!({
        "serverId": "docs",
        "serverUrl": "https://docs.example.com/mcp",
        "defaultServerLabel": "docs",
    }))
    .expect("minimal server document");

    assert_eq!(input.execution, RemoteMcpExecution::Runtime);
    assert_eq!(
        serde_json::to_value(RemoteMcpExecution::Runtime).unwrap(),
        json!("runtime")
    );
}

#[test]
fn session_execution_failure_serializes_camel_case_and_is_omitted_when_absent() {
    for (failure, wire) in [
        (SessionExecutionFailureView::Failed, "failed"),
        (SessionExecutionFailureView::Terminated, "terminated"),
        (SessionExecutionFailureView::TimedOut, "timedOut"),
        (SessionExecutionFailureView::Missing, "missing"),
    ] {
        assert_eq!(
            serde_json::to_value(failure).expect("serialize"),
            json!(wire)
        );
    }
    let mut session = test_session("session_1".to_owned(), SessionLifecycleStatus::Open);
    let healthy = serde_json::to_value(&session).expect("serialize session");
    assert!(healthy.get("executionFailure").is_none());
    session.execution_failure = Some(SessionExecutionFailureView::TimedOut);
    let dead = serde_json::to_value(&session).expect("serialize session");
    assert_eq!(dead["lifecycleStatus"], json!("open"));
    assert_eq!(dead["activity"], json!("idle"));
    assert_eq!(dead["executionFailure"], json!("timedOut"));
}

#[test]
fn run_lifecycle_statuses_keep_cancelling_distinct() {
    assert_eq!(
        serde_json::to_value(RunStatus::Cancelling).expect("serialize status"),
        json!("cancelling")
    );
}

#[test]
fn tool_call_status_can_represent_requested_calls() {
    assert_eq!(
        serde_json::to_value(ToolItemStatus::Requested).expect("serialize status"),
        json!("requested")
    );
    assert_eq!(
        serde_json::to_value(ToolItemStatus::Cancelled).expect("serialize status"),
        json!("cancelled")
    );
}

#[test]
fn session_id_validation_matches_public_api_shape() {
    assert_eq!(validate_session_id("session-1"), Ok(()));
    assert_eq!(validate_session_id("session_1.test:dev"), Ok(()));
    assert_eq!(validate_session_id(""), Err(SessionIdError::Empty));
    assert_eq!(
        validate_session_id("-session"),
        Err(SessionIdError::InvalidStart)
    );
    assert_eq!(
        validate_session_id("session/name"),
        Err(SessionIdError::InvalidCharacter { index: 7, ch: '/' })
    );
    assert_eq!(
        validate_session_id("session name"),
        Err(SessionIdError::InvalidCharacter { index: 7, ch: ' ' })
    );
}

#[test]
fn session_id_reserves_slash_as_the_workflow_id_separator() {
    // The hosted runtime composes Temporal workflow ids as
    // `{universe_id}/{session_id}`. Session ids rejecting
    // `/` is what makes that composition unambiguously splittable, so this is
    // a load-bearing invariant, not an incidental charset choice.
    assert_eq!(
        validate_session_id("universe/session"),
        Err(SessionIdError::InvalidCharacter { index: 8, ch: '/' })
    );
}

fn test_session(id: SessionId, lifecycle_status: SessionLifecycleStatus) -> SessionView {
    let retention = test_session_retention(&id);
    SessionView {
        execution_failure: None,
        access: test_access_summary(),
        activity: SessionActivity::Idle,
        metadata: Default::default(),
        id,
        display_name: Some("Test session".to_owned()),
        lifecycle_status,
        closed_at_ms: None,
        retention,
        active_run: None,
        lifecycle_controlled: false,
        config_revision: 0,
        config: None,
        active_environment_id: None,
        created_at_ms: 1,
        updated_at_ms: 2,
        runs: Vec::new(),
        active_context: ContextView::default(),
        active_tools: ActiveToolsView::default(),
        integration: None,
        origin: None,
    }
}

fn test_session_retention(root_session_id: &str) -> SessionRetentionView {
    SessionRetentionView {
        root_session_id: root_session_id.to_owned(),
        delete_after_close_ms: None,
        delete_at_ms: None,
    }
}

fn test_registration_key() -> EnvironmentRegistrationKeyView {
    EnvironmentRegistrationKeyView {
        registration_key_id: "rk_harbor".to_owned(),
        display_name: "harbor".to_owned(),
        key_prefix: "lsrk_test-se".to_owned(),
        identity_mode: EnvironmentIdentityModeView::Ephemeral,
        max_active_environments: Some(8),
        ephemeral_disconnect_grace_ms: 300_000,
        offline_close_after_ms: None,
        declared_only: false,
        expires_at_ms: None,
        status: EnvironmentRegistrationKeyStatusView::Active,
        registered_environment_count: 1,
        active_environment_count: 1,
        last_registered_at_ms: Some(10),
        created_at_ms: 5,
        updated_at_ms: 5,
        revoked_at_ms: None,
    }
}

#[test]
fn registration_secret_views_redact_debug_output() {
    let response = EnvironmentRegistrationKeyCreateResponse {
        registration_key: test_registration_key(),
        secret: EnvironmentRegistrationSecretView("lsrk_plaintext".to_owned()),
    };
    let debug = format!("{response:?}");
    assert!(!debug.contains("lsrk_plaintext"));
    assert!(debug.contains("<redacted>"));
    let json = serde_json::to_value(&response).expect("serialize");
    assert_eq!(json["secret"], "lsrk_plaintext");
}

fn test_deployment_environment_provider(provider_id: &str) -> DeploymentEnvironmentProviderView {
    DeploymentEnvironmentProviderView {
        provider_id: provider_id.to_owned(),
        display_name: Some("Local Incus".to_owned()),
        controller_endpoint: "ws://127.0.0.1:19090/control".to_owned(),
        data_route_base: None,
        effective_data_route_base: Some("ws://127.0.0.1:19090/routes".to_owned()),
        has_credential: false,
        available_to_all_universes: true,
        created_at_ms: 10,
        updated_at_ms: 20,
    }
}

#[test]
fn environment_provider_put_params_redact_credential_and_view_omits_it() {
    let params: DeploymentEnvironmentProviderPutParams = serde_json::from_value(json!({
        "providerId": "incus-local",
        "controllerEndpoint": "wss://provider.example/control",
        "credential": "provider-bearer-secret"
    }))
    .expect("put params");
    assert!(!params.clear_credential);
    assert!(params.data_route_base.is_none());
    assert!(!format!("{params:?}").contains("provider-bearer-secret"));
    let view = serde_json::to_value(test_deployment_environment_provider("incus-local"))
        .expect("serialize view");
    assert_eq!(view["hasCredential"], json!(false));
    assert!(view.get("credential").is_none());
    assert!(view.get("dataRouteBase").is_none());
    assert_eq!(
        view["effectiveDataRouteBase"],
        json!("ws://127.0.0.1:19090/routes")
    );
}

fn test_access_summary() -> ResourceAccessSummary {
    ResourceAccessSummary {
        visibility: Visibility::Universe,
        created_by: Some(Attribution::Local),
    }
}

#[test]
fn code_mode_feature_defaults_and_public_field_names_are_stable() {
    let defaults: CodeModeFeature = serde_json::from_value(json!({})).unwrap();
    assert_eq!(defaults.timeout_ms, 60_000);
    let feature: CodeModeFeature =
        serde_json::from_value(json!({"timeoutMs":1000,"allowedTools":[]})).unwrap();
    assert_eq!(feature.timeout_ms, 1000);
    assert_eq!(feature.max_tool_calls, 128);
    assert_eq!(feature.allowed_tools, Some(vec![]));
    let value = serde_json::to_value(feature).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["maxOutstandingToolCalls"], 16);
    assert!(serde_json::from_value::<CodeModeFeature>(json!({"maxToolCallz":4})).is_err());
}

#[test]
fn an_environment_credential_source_is_a_grant_or_a_provider_credential() {
    let bind = |source: Value| {
        serde_json::from_value::<EnvironmentCredentialBindParams>(json!({
            "environmentId": "build-box",
            "envName": "TOKEN",
            "source": source,
        }))
    };
    for (wire, source) in [
        (
            json!({ "type": "authGrant", "grantId": "authgrant_repo" }),
            EnvironmentCredentialSourceView::CredentialGrant {
                grant_id: "authgrant_repo".to_owned(),
            },
        ),
        (
            json!({ "type": "authProviderCredential", "providerId": "model:openai" }),
            EnvironmentCredentialSourceView::CredentialProviderCredential {
                provider_id: "model:openai".to_owned(),
            },
        ),
    ] {
        assert_eq!(
            bind(wire.clone()).expect("a bindable source").source,
            source
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), wire);
    }

    // A stored secret cannot be named: a bare secret id has no exposure to
    // check, so a request that carries one is not a binding request.
    assert!(bind(json!({ "type": "directSecret", "secretId": "authsec_1" })).is_err());

    // The published schema offers exactly the two sources.
    let mut generator = schemars::generate::SchemaSettings::draft07().into_generator();
    let schema = generator
        .root_schema_for::<EnvironmentCredentialSourceView>()
        .to_value();
    let mut types: Vec<&str> = schema["oneOf"]
        .as_array()
        .expect("one schema per source")
        .iter()
        .filter_map(|variant| variant["properties"]["type"]["const"].as_str())
        .collect();
    types.sort_unstable();
    assert_eq!(types, ["authGrant", "authProviderCredential"]);
}

#[test]
fn gone_error_carries_the_permanent_universe_record() {
    let record = DeletedUniverseView {
        metadata: Default::default(),
        universe_id: "universe".into(),
        handle: "old".into(),
        display_name: None,
        created_at_ms: 1,
        deleting_at_ms: 2,
        deleted_at_ms: 3,
        requested_by: Attribution::Local,
        summary: json!({"contained":{"sessions":2},"removed":{"workflowsTerminated":1}}),
    };
    let error = AgentApiError::gone(record.clone());
    assert_eq!(error.kind, AgentApiErrorKind::Gone);
    let value = serde_json::to_value(&error).unwrap();
    assert_eq!(value["kind"], "gone");
    assert_eq!(value["deletedUniverse"]["handle"], "old");
    let decoded: AgentApiError = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.deleted_universe.as_deref(), Some(&record));
}
