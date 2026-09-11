use async_trait::async_trait;
use serde_json::{Value, json};

use super::*;

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
fn notification_serializes_as_json_rpc_lite_shape() {
    let notification = AgentNotification::RunCompleted {
        session_id: "session_1".to_owned(),
        run: RunView {
            output: None,
            output_text: None,
            id: "run_1".to_owned(),
            status: RunStatus::Completed,
            started_at_ms: Some(10),
            completed_at_ms: Some(20),
            source: RunViewSource::Input {
                items: vec![InputItem::Text {
                    origin: None,
                    text: "hello".to_owned(),
                }],
            },
            entries: Vec::new(),
            tool_batches: Vec::new(),
            usage: None,
            pending_approvals: Vec::new(),
        },
    };

    let value = serde_json::to_value(notification).expect("serialize notification");

    assert_eq!(
        value,
        json!({
            "method": "session/runs/completed",
            "params": {
                "sessionId": "session_1",
                "run": {
                    "id": "run_1",
                    "status": "completed",
                    "startedAtMs": 10,
                    "completedAtMs": 20,
                    "source": {
                        "type": "input",
                        "items": [{ "type": "text", "text": "hello" }]
                    },
                    "entries": []
                }
            }
        })
    );
}

#[test]
fn auth_grant_import_params_redact_token_in_debug_output() {
    let params: AuthGrantImportParams = serde_json::from_value(json!({
        "grantId": "authgrant_1",
        "token": "super-secret-token",
        "audience": "https://crm.example.com/mcp"
    }))
    .expect("deserialize import params");

    let debug = format!("{params:?}");

    assert!(!debug.contains("super-secret-token"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert_eq!(params.token, "super-secret-token");
    assert_eq!(params.exposure, AuthGrantExposure::Brokered);
}

#[test]
fn auth_grant_lease_response_redacts_token_in_debug_output() {
    let response = AuthGrantLeaseResponse {
        token: "super-secret-leased-token".to_owned(),
        expires_at_ms: Some(1234),
        grant_id: "authgrant_1".to_owned(),
        provider_kind: AuthProviderKind::StaticBearer,
    };

    let debug = format!("{response:?}");

    assert!(!debug.contains("super-secret-leased-token"), "{debug}");
    assert!(debug.contains("<redacted>"));
    assert!(debug.contains("authgrant_1"));
}

#[test]
fn auth_client_create_params_redact_client_secret_in_debug_output() {
    let params: AuthClientCreateParams = serde_json::from_value(json!({
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
fn auth_provider_create_params_redact_credential_in_debug_output() {
    let params: AuthProviderCreateParams = serde_json::from_value(json!({
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
fn request_ids_accept_number_or_string() {
    let numeric: JsonRpcRequest = serde_json::from_value(json!({
        "id": 7,
        "method": "session/start"
    }))
    .expect("numeric id");
    let string: JsonRpcRequest = serde_json::from_value(json!({
        "id": "req_7",
        "method": "session/start"
    }))
    .expect("string id");

    assert_eq!(numeric.id, RequestId::Number(7));
    assert_eq!(string.id, RequestId::String("req_7".to_owned()));
}

#[test]
fn public_session_config_rejects_managed_workflow_tool_bindings() {
    let config = serde_json::from_value::<SessionConfig>(json!({
        "workflowTools": {
            "lifecycleController": {
                "workflowId": "work/other",
                "workflowKind": "agent_work"
            },
            "bindings": []
        }
    }));

    assert!(
        config.is_err(),
        "managed workflow-tool bindings must not enter through public session config"
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
fn managed_session_creation_exposes_targets_and_completion_contracts() {
    let params: ManagedSessionStartParams = serde_json::from_value(json!({
        "sessionId": "session_1",
        "workflowTools": {
            "version": 1,
            "lifecycleController": {
                "workflowId": "controller-1",
                "workflowKind": "order.workflow"
            },
            "tools": [{
                "definition": {
                    "toolId": "accept-order",
                    "revision": 1,
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
    .expect("managed session params");

    let workflow_tools = params.workflow_tools;
    assert_eq!(workflow_tools.version, 1);
    assert_eq!(
        workflow_tools.tools[0].definition.tool.parallelism,
        ToolParallelismView::ParallelSafe
    );
    assert_eq!(
        serde_json::to_value(&workflow_tools).expect("serialize workflow tools")["tools"][0]["definition"]
            ["tool"]["parallelism"],
        json!("parallelSafe")
    );
    assert!(matches!(
        workflow_tools.tools[0].target,
        WorkflowToolTargetInput::Bound {
            dispatch: BoundWorkflowToolDispatchInput::Push,
            ..
        }
    ));
    assert!(matches!(
        workflow_tools.tools[0].completion,
        WorkflowToolCompletionInput::Promises {
            key_source: WorkflowToolCompletionKeySourceInput::ArrayIndices { .. },
            ..
        }
    ));
}

#[test]
fn managed_session_creation_rejects_legacy_receiver_shortcut_and_tool_targets() {
    for forbidden in ["receiver", "targetRequirement"] {
        let mut tool = json!({
            "definition": {
                "toolId": "accept-order",
                "revision": 1,
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
        let result = serde_json::from_value::<ManagedSessionStartParams>(json!({
            "workflowTools": {"version": 1, "tools": [tool]}
        }));
        assert!(result.is_err(), "{forbidden} must not be accepted");
    }
}

#[test]
fn managed_session_creation_rejects_bound_target_without_dispatch() {
    let result = serde_json::from_value::<ManagedSessionStartParams>(json!({
        "workflowTools": {
            "version": 1,
            "tools": [{
                "definition": {
                    "toolId": "accept-order",
                    "revision": 1,
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
fn managed_session_creation_decodes_joined_completion_with_required_deadline() {
    let params = serde_json::from_value::<ManagedSessionStartParams>(json!({
        "workflowTools": {
            "version": 1,
            "tools": [{
                "definition": {
                    "toolId": "send-message",
                    "revision": 1,
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
    .expect("Joined managed session declaration");
    assert!(matches!(
        params.workflow_tools.tools[0].completion,
        WorkflowToolCompletionInput::Joined {
            deadline_after_ms: 30_000,
            ..
        }
    ));

    assert!(
        serde_json::from_value::<WorkflowToolCompletionInput>(json!({
            "type": "joined"
        }))
        .is_err()
    );
}

#[test]
fn ordinary_session_start_rejects_managed_creation_fields() {
    assert!(
        serde_json::from_value::<SessionStartParams>(json!({
            "workflowTools": {"version": 1, "tools": []}
        }))
        .is_err()
    );
}

#[test]
fn session_start_decodes_creation_environment_overrides() {
    let existing: SessionStartParams = serde_json::from_value(json!({
        "profile": {"kind": "named", "profileId": "developer"},
        "environment": {"type": "existing", "environmentId": "workstation"}
    }))
    .expect("existing environment override");
    assert!(matches!(
        existing.environment,
        Some(SessionEnvironmentOverride::Existing { environment_id })
            if environment_id == "workstation"
    ));

    let none: SessionStartParams = serde_json::from_value(json!({
        "environment": {"type": "none"}
    }))
    .expect("none environment override");
    assert!(matches!(
        none.environment,
        Some(SessionEnvironmentOverride::None {})
    ));
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

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_managed_session_start() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_MANAGED_START.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "workflowTools": {
                    "version": 1,
                    "lifecycleController": {
                        "workflowId": "controller-1",
                        "workflowKind": "order.workflow"
                    },
                    "tools": []
                }
            })),
        },
    )
    .await;

    let error = response
        .error
        .expect("test service returns an internal error");
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.data.expect("typed error").kind,
        AgentApiErrorKind::Internal
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_calls_api_service() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_INITIALIZE.to_owned(),
            params: Some(json!({})),
        },
    )
    .await;

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(
        response.result.expect("result")["result"]["serverInfo"]["name"],
        json!("test-service")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_models_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_MODELS_LIST.to_owned(),
            params: Some(json!({})),
        },
    )
    .await;

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.expect("result");
    assert_eq!(
        result["result"]["models"][0]["apiKind"],
        json!("openai:responses")
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

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_rejects_unknown_methods() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::String("req_1".to_owned()),
            method: "missing/method".to_owned(),
            params: None,
        },
    )
    .await;

    assert_eq!(response.error.expect("error").code, -32601);
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_close() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_CLOSE.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(
        response.result.expect("result")["result"]["session"]["status"],
        json!("closed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_delete() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_DELETE.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(
        response.result.expect("result")["result"]["session"]["lifecycleStatus"],
        json!("closed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_LIST.to_owned(),
            params: Some(json!({ "cursor": "40:session_prev", "limit": 10 })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(
        result["result"]["sessions"][0]["displayName"],
        json!("Test session")
    );
    assert_eq!(
        result["result"]["sessions"][0]["lifecycleStatus"],
        json!("open")
    );
    assert_eq!(result["result"]["nextCursor"], json!("40:session_prev"));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_rename() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_RENAME.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "displayName": "Family chat"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(result["result"]["session"]["id"], json!("session_1"));
    assert_eq!(
        result["result"]["session"]["displayName"],
        json!("Family chat")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_config_put() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_CONFIG_PUT.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "expectedConfigRevision": 0,
                "config": {
                    "generation": { "reasoningEffort": "high" },
                    "features": {
                        "timers": {},
                        "vfs": { "tools": "edit" }
                    }
                }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["session"]["id"],
        json!("session_1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_context_compact() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_CONTEXT_COMPACT.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["session"]["id"],
        json!("session_1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_context_remove() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_CONTEXT_REMOVE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "keys": ["channel.room.batch-1"]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["results"],
        json!([
            {
                "key": "channel.room.batch-1",
                "status": "removed"
            }
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_context_append() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_CONTEXT_APPEND.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "entries": [
                    {
                        "key": "channel.room.batch-1",
                        "item": { "type": "text", "text": "Alice: hello" }
                    }
                ]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["results"],
        json!([
            {
                "key": "channel.room.batch-1",
                "status": "applied"
            }
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_run_cancel() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_RUNS_CANCEL.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "runId": "run_1"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["run"]["status"],
        json!("cancelled")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_run_approvals_decide() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_RUNS_APPROVALS_DECIDE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "runId": "run_1",
                "decisions": [{
                    "approvalId": "approval_1",
                    "decision": "approve"
                }]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(result["result"]["results"][0]["status"], json!("decided"));
    assert_eq!(result["result"]["run"]["status"], json!("running"));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_run_steer() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_RUNS_STEER.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "runId": "run_1",
                "items": [{ "type": "text", "text": "focus on the tests" }]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(result["result"]["steeringId"], json!("steering_1"));
    assert_eq!(result["result"]["run"]["status"], json!("running"));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_skills_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_SKILLS_LIST.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result")["result"].clone();
    assert_eq!(
        result.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["catalogs"]
    );
    let catalog = &result["catalogs"][0];
    assert_eq!(catalog["source"], json!({"type": "vfs"}));
    assert_eq!(catalog["availability"], "available");
    assert_eq!(catalog["warnings"], json!([]));
    assert!(catalog.get("contextKey").is_none());
    assert_eq!(
        catalog["skills"][0]["location"],
        json!({"skillDirPath":"/skills/one", "skillDocPath":"/skills/one/SKILL.md"})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environments_create() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENTS_CREATE.to_owned(),
            params: Some(json!({
                "requestId": "request-1",
                "bindingId": "primary",
                "templateId": "rust-v1",
                "resources": { "cpu": 2, "memoryBytes": 2147483648_u64, "diskBytes": 10737418240_u64 }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["environment"]["environmentId"],
        json!("evi_test")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_power_and_idle_policy_put() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENTS_POWER_PUT.to_owned(),
            params: Some(json!({
                "environmentId": "evi_test",
                "power": "paused"
            })),
        },
    )
    .await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let environment = &response.result.expect("result")["result"]["environment"];
    assert_eq!(environment["desiredPower"], json!("paused"));
    assert_eq!(
        environment["incarnation"]["powerStates"],
        json!(["running", "paused"])
    );

    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(2),
            method: METHOD_ENVIRONMENTS_IDLE_POLICY_PUT.to_owned(),
            params: Some(json!({
                "environmentId": "evi_test",
                "idlePolicy": {"pauseAfterMs": 60000, "closeAfterMs": 3600000}
            })),
        },
    )
    .await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let environment = &response.result.expect("result")["result"]["environment"];
    assert_eq!(
        environment["idlePolicy"],
        json!({"pauseAfterMs": 60000, "closeAfterMs": 3600000})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_ingress_put() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENTS_INGRESS_PUT.to_owned(),
            params: Some(json!({
                "environmentId": "evi_test",
                "enabled": true
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let environment = &response.result.expect("result")["result"]["environment"];
    assert_eq!(environment["publicIngressEnabled"], json!(true));
    assert_eq!(
        environment["publicEndpoint"],
        json!("https://opaque.env.example")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_activate() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_ACTIVATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "environmentId": "evi_test"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["session"]["activeEnvironmentId"],
        json!("evi_test")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_deactivate() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_DEACTIVATE.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert!(response.result.expect("result")["result"]["session"]["activeEnvironmentId"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_credentials_bind() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENTS_CREDENTIALS_BIND.to_owned(),
            params: Some(json!({
                "environmentId": "evi_test",
                "envName": "GITHUB_TOKEN",
                "source": {
                    "type": "authGrant",
                    "grantId": "authgrant_repo"
                }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(
        result["result"]["credential"]["envName"],
        json!("GITHUB_TOKEN")
    );
    assert_eq!(
        result["result"]["credential"]["source"]["grantId"],
        json!("authgrant_repo")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_credentials_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENTS_CREDENTIALS_LIST.to_owned(),
            params: Some(json!({
                "environmentId": "evi_test"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(
        result["result"]["credentials"][0]["envName"],
        json!("GITHUB_TOKEN")
    );
    assert_eq!(
        result["result"]["credentials"][0]["source"]["type"],
        json!("authGrant")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_credentials_unbind() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND.to_owned(),
            params: Some(json!({
                "environmentId": "evi_test",
                "envName": "GITHUB_TOKEN"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["credential"]["envName"],
        json!("GITHUB_TOKEN")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_mcp_server_put() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_MCP_SERVERS_PUT.to_owned(),
            params: Some(json!({
                "server": {
                    "serverId": "echo",
                    "serverUrl": "https://echo.example.com/mcp",
                    "defaultServerLabel": "echo"
                },
                "expectedRevision": 1
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["server"]["serverId"],
        json!("echo")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_mcp_server_auth_discovery() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_MCP_SERVERS_AUTH_DISCOVER.to_owned(),
            params: Some(json!({
                "serverUrl": "https://mcp.example.com/mcp"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["oauth"]["resource"],
        json!("https://mcp.example.com/mcp")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_live_mcp_tool_discovery_without_revision() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_MCP_SERVERS_TOOLS_DISCOVER.to_owned(),
            params: Some(json!({ "serverId": "echo" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(result["result"]["status"], json!("success"));
    assert_eq!(result["result"]["tools"][0]["name"], json!("echo_search"));
    assert_eq!(
        result["result"]["tools"][0]["annotations"]["readOnlyHint"],
        json!(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_auth_grant_lease() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_AUTH_GRANTS_LEASE.to_owned(),
            params: Some(json!({
                "grantId": "authgrant_1",
                "audience": "https://api.example.com"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.expect("result");
    assert_eq!(result["result"]["grantId"], json!("authgrant_1"));
    assert_eq!(result["result"]["token"], json!("leased-token"));
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

    assert_eq!(
        params.server.approval_default,
        RemoteMcpApprovalPolicy::Never
    );
    assert_eq!(params.expected_revision, None);
    assert_eq!(params.server.credential, None);
}

#[test]
fn mcp_server_put_decodes_universe_auth_grant_credential() {
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
        Some(McpServerCredential::AuthGrant {
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
fn mcp_session_links_reject_removed_connection_and_policy_fields() {
    for (field, value) in [
        ("authGrantId", json!("authgrant_1")),
        ("allowedTools", json!(["search"])),
        ("approval", json!("never")),
        ("deferLoading", json!(true)),
    ] {
        let mut link = serde_json::Map::from_iter([("serverId".to_owned(), json!("echo"))]);
        link.insert(field.to_owned(), value);
        let error = serde_json::from_value::<McpServerLink>(Value::Object(link))
            .expect_err("session MCP links must reject removed fields");
        assert!(
            error.to_string().contains("unknown field"),
            "{field}: {error}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_run_start_with_config() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_RUNS_START.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "source": {
                    "type": "input",
                    "items": [{ "type": "text", "text": "hello" }]
                },
                "config": {
                    "model": {
                        "providerId": "openai",
                        "apiKind": "openai:responses",
                        "model": "gpt-5.5"
                    },
                    "generation": {
                        "maxOutputTokens": 1024,
                        "reasoningEffort": "high"
                    }
                }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["run"]["status"],
        json!("running")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_blob_put_many() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_BLOBS_PUT.to_owned(),
            params: Some(json!({
                "blobs": [
                    { "bytesBase64": "aGVsbG8=" },
                    { "bytesBase64": "d29ybGQ=" }
                ]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["blobs"][1]["bytes"],
        json!(8)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_snapshot_commit() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_SNAPSHOTS_COMMIT.to_owned(),
            params: Some(json!({
                "manifest": {
                    "schema_version": "lightspeed.vfs.snapshot.v1",
                    "root": { "entries": {} },
                    "totals": { "files": 0, "bytes": 0 }
                }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["snapshotRef"],
        json!(format!("sha256:{}", "2".repeat(64)))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_workspace_create() {
    let snapshot_ref = format!("sha256:{}", "2".repeat(64));
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_WORKSPACES_CREATE.to_owned(),
            params: Some(json!({
                "workspaceId": "workspace_1",
                "snapshotRef": snapshot_ref
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["workspace"]["workspaceId"],
        json!("workspace_1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_workspace_read() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_WORKSPACES_READ.to_owned(),
            params: Some(json!({ "workspaceId": "workspace_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["workspace"]["revision"],
        json!(4)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_workspace_update() {
    let snapshot_ref = format!("sha256:{}", "4".repeat(64));
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_WORKSPACES_UPDATE.to_owned(),
            params: Some(json!({
                "workspaceId": "workspace_1",
                "expectedRevision": 4,
                "snapshotRef": snapshot_ref
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["workspace"]["revision"],
        json!(5)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_workspace_update_without_expected_revision() {
    let snapshot_ref = format!("sha256:{}", "4".repeat(64));
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_WORKSPACES_UPDATE.to_owned(),
            params: Some(json!({
                "workspaceId": "workspace_1",
                "snapshotRef": snapshot_ref
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["workspace"]["revision"],
        json!(5)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_workspace_delete() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_WORKSPACES_DELETE.to_owned(),
            params: Some(json!({ "workspaceId": "workspace_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["workspace"]["workspaceId"],
        json!("workspace_1")
    );
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
                content_ref: "sha256:abc".to_owned(),
                media_type: Some("text/plain".to_owned()),
                provider_kind: None,
                media_handle: None,
            }),
        },
    };

    let value = serde_json::to_value(AgentNotification::SessionEvent {
        event: Box::new(event),
    })
    .expect("serialize event notification");

    assert_eq!(
        value,
        json!({
            "method": "session/event",
            "params": {
                "event": {
                    "cursor": { "seq": 3 },
                    "sessionId": "session_1",
                    "observedAtMs": 12,
                    "joins": { "runId": "run_1" },
                    "kind": {
                        "type": "runCompleted",
                        "runId": "run_1",
                        "output": { "contentRef": "sha256:abc", "mediaType": "text/plain", "providerKind": null }
                    }
                }
            }
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
            content_ref: "sha256:compact".to_owned(),
            media_type: Some("application/json".to_owned()),
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
            "content": { "contentRef": "sha256:compact", "mediaType": "application/json", "providerKind": "openai.responses.compaction" },
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
            content_ref: "sha256:mcp".to_owned(),
            media_type: Some("application/json".to_owned()),
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
            "content": { "contentRef": "sha256:mcp", "mediaType": "application/json", "providerKind": "openai.responses.mcp_call" },
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
        completed_at_ms: None,
        source: RunViewSource::Input { items: Vec::new() },
        entries: Vec::new(),
        tool_batches: vec![ToolBatchView {
            id: "tool_batch_1".to_owned(),
            turn_id: "turn_1".to_owned(),
            status: ToolItemStatus::Succeeded,
            calls: vec![ToolCallView {
                tool_id: None,
                started_at_ms: None,
                completed_at_ms: None,
                duration_ms: None,
                media: Vec::new(),
                call_id: "call_1".to_owned(),
                tool_name: "read_file".to_owned(),
                arguments_ref: "sha256:args".to_owned(),
                arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                output: Some("ok".to_owned()),
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
    };

    let value = serde_json::to_value(run).expect("serialize run");

    assert_eq!(value["startedAtMs"], 10);
    assert!(value.get("completedAtMs").is_none());
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
fn session_status_serializes_as_string_enum() {
    assert_eq!(
        serde_json::to_value(SessionStatus::Idle).expect("serialize status"),
        json!("idle")
    );
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

struct TestService;

#[async_trait]
impl AgentApiService for TestService {
    async fn list_models(
        &self,
        _params: ModelListParams,
    ) -> Result<AgentApiOutcome<ModelListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ModelListResponse {
            models: vec![ModelView {
                provider_id: "openai".to_owned(),
                api_kind: "openai:responses".to_owned(),
                model: "gpt-test".to_owned(),
                display_name: "gpt-test".to_owned(),
                capabilities: ModelCapabilitiesView::default(),
                created_at_ms: Some(1_700_000_000_000),
                source: ModelSource::Provider,
                fetched_at_ms: 1,
            }],
            providers: vec![ModelProviderDiscoveryView {
                provider_id: "openai".to_owned(),
                api_kinds: vec!["openai:responses".to_owned()],
                fetched_at_ms: Some(1),
                error: None,
                credential: ModelProviderCredentialStatus::Configured,
                credential_source: ModelProviderCredentialSource::Deployment,
            }],
        }))
    }

    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(InitializeResponse {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            server_info: ServerInfo {
                name: "test-service".to_owned(),
                version: "0+0000000".to_owned(),
                git_sha: "0000000".to_owned(),
                envd: EnvironmentDaemonInfo {
                    version: "0".to_owned(),
                    git_sha: "0000000".to_owned(),
                    protocol_version: 0,
                    targets: Vec::new(),
                },
            },
            capabilities: ServerCapabilities {
                notifications: false,
                history_read: true,
                event_log: true,
                local_execution: false,
            },
        }))
    }

    async fn start_session(
        &self,
        _params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        Err(AgentApiError::internal("not implemented"))
    }

    async fn start_managed_session(
        &self,
        _params: ManagedSessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        Err(AgentApiError::internal("not implemented"))
    }

    async fn create_profile(
        &self,
        params: ProfileCreateParams,
    ) -> Result<AgentApiOutcome<ProfileCreateResponse>, AgentApiError> {
        let input = params.profile;
        Ok(AgentApiOutcome::new(ProfileCreateResponse {
            profile: AgentProfile {
                profile_id: input.profile_id,
                display_name: input.display_name,
                description: input.description,
                revision: 1,
                document: input.document,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        }))
    }

    async fn read_profile(
        &self,
        params: ProfileReadParams,
    ) -> Result<AgentApiOutcome<ProfileReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ProfileReadResponse {
            profile: test_profile(params.profile_id),
        }))
    }

    async fn list_profiles(
        &self,
        _params: ProfileListParams,
    ) -> Result<AgentApiOutcome<ProfileListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ProfileListResponse {
            profiles: vec![test_profile(ProfileId::new("support")).summary()],
        }))
    }

    async fn put_profile(
        &self,
        params: ProfilePutParams,
    ) -> Result<AgentApiOutcome<ProfilePutResponse>, AgentApiError> {
        let mut profile = test_profile(params.profile.profile_id);
        profile.display_name = params.profile.display_name;
        profile.description = params.profile.description;
        profile.document = params.profile.document;
        profile.revision = params.expected_revision.unwrap_or(profile.revision) + 1;
        Ok(AgentApiOutcome::new(ProfilePutResponse { profile }))
    }

    async fn delete_profile(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<AgentApiOutcome<ProfileDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ProfileDeleteResponse {
            profile: test_profile(params.profile_id),
        }))
    }

    async fn apply_profile(
        &self,
        params: ProfileApplyParams,
    ) -> Result<AgentApiOutcome<ProfileApplyResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ProfileApplyResponse {
            session: test_session(params.session_id, SessionStatus::Idle),
            applied: ProfileApplySummary::default(),
        }))
    }

    async fn put_session_config(
        &self,
        params: SessionConfigPutParams,
    ) -> Result<AgentApiOutcome<SessionConfigPutResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionConfigPutResponse {
            session: test_session_mutation(params.session_id, SessionStatus::Idle),
        }))
    }

    async fn read_session(
        &self,
        _params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        Err(AgentApiError::internal("not implemented"))
    }

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<AgentApiOutcome<SessionListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionListResponse {
            sessions: vec![SessionSummaryView {
                metadata: Default::default(),
                id: "session_test".to_owned(),
                display_name: Some("Test session".to_owned()),
                lifecycle_status: SessionLifecycleStatus::Open,
                closed_at_ms: None,
                retention: test_session_retention("session_test"),
                managed: false,
                origin: None,
                created_at_ms: 1,
                updated_at_ms: 2,
            }],
            next_cursor: params.cursor,
        }))
    }

    async fn rename_session(
        &self,
        params: SessionRenameParams,
    ) -> Result<AgentApiOutcome<SessionRenameResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionRenameResponse {
            session: SessionSummaryView {
                metadata: Default::default(),
                retention: test_session_retention(&params.session_id),
                id: params.session_id,
                display_name: params.display_name,
                lifecycle_status: SessionLifecycleStatus::Open,
                closed_at_ms: None,
                managed: false,
                origin: None,
                created_at_ms: 1,
                updated_at_ms: 2,
            },
        }))
    }

    async fn put_session_metadata(
        &self,
        params: SessionMetadataPutParams,
    ) -> Result<AgentApiOutcome<SessionMetadataPutResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionMetadataPutResponse {
            session: SessionSummaryView {
                retention: test_session_retention(&params.session_id),
                id: params.session_id,
                display_name: None,
                metadata: params.metadata,
                lifecycle_status: SessionLifecycleStatus::Open,
                closed_at_ms: None,
                managed: false,
                origin: None,
                created_at_ms: 1,
                updated_at_ms: 2,
            },
        }))
    }

    async fn read_session_events(
        &self,
        _params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
        Err(AgentApiError::internal("not implemented"))
    }

    async fn close_session(
        &self,
        params: SessionCloseParams,
    ) -> Result<AgentApiOutcome<SessionCloseResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionCloseResponse {
            session: test_session_mutation(params.session_id, SessionStatus::Closed),
        }))
    }

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<AgentApiOutcome<SessionDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionDeleteResponse {
            session: SessionSummaryView {
                metadata: Default::default(),
                retention: test_session_retention(&params.session_id),
                id: params.session_id,
                display_name: None,
                lifecycle_status: SessionLifecycleStatus::Closed,
                closed_at_ms: Some(2),
                managed: false,
                origin: None,
                created_at_ms: 1,
                updated_at_ms: 2,
            },
            deleted_session_count: 1,
        }))
    }

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ContextCompactResponse {
            session: test_session_mutation(params.session_id, SessionStatus::Idle),
        }))
    }

    async fn append_context(
        &self,
        params: ContextAppendParams,
    ) -> Result<AgentApiOutcome<ContextAppendResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ContextAppendResponse {
            context_revision: 1,
            results: params
                .entries
                .iter()
                .map(|entry| ContextAppendResult {
                    key: entry.key.clone(),
                    status: ContextAppendStatus::Applied,
                    entry: None,
                    failure: None,
                    activation_text: None,
                    activation_text_truncated: false,
                })
                .collect(),
        }))
    }

    async fn remove_context(
        &self,
        params: ContextRemoveParams,
    ) -> Result<AgentApiOutcome<ContextRemoveResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ContextRemoveResponse {
            context_revision: 1,
            results: params
                .keys
                .iter()
                .map(|key| ContextRemoveResult {
                    key: key.clone(),
                    status: ContextRemoveStatus::Removed,
                    failure: None,
                })
                .collect(),
        }))
    }

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        let config = params.config.expect("run config");
        assert_eq!(params.session_id, "session_1");
        let generation = config.generation.expect("generation");
        assert_eq!(generation.max_output_tokens, Some(1024));
        assert_eq!(generation.reasoning_effort, Some("high".to_owned()));
        assert_eq!(config.model.expect("model").model, "gpt-5.5");
        Ok(AgentApiOutcome::new(RunStartResponse {
            run: test_run("run_1".to_owned(), RunStatus::Running),
        }))
    }

    async fn list_runs(
        &self,
        _params: RunListParams,
    ) -> Result<AgentApiOutcome<RunListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(RunListResponse {
            runs: Vec::new(),
            next_cursor: None,
            has_older_runs: false,
        }))
    }

    async fn read_run(
        &self,
        params: RunReadParams,
    ) -> Result<AgentApiOutcome<RunReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(RunReadResponse {
            run: test_run(params.run_id, RunStatus::Completed),
        }))
    }

    async fn cancel_run(
        &self,
        params: RunCancelParams,
    ) -> Result<AgentApiOutcome<RunCancelResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(RunCancelResponse {
            run: test_run(params.run_id, RunStatus::Cancelled),
        }))
    }

    async fn decide_run_approvals(
        &self,
        params: RunApprovalsDecideParams,
    ) -> Result<AgentApiOutcome<RunApprovalsDecideResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(RunApprovalsDecideResponse {
            results: params
                .decisions
                .into_iter()
                .map(|decision| ApprovalDecisionResult {
                    approval_id: decision.approval_id,
                    status: ApprovalDecisionStatus::Decided,
                    failure: None,
                })
                .collect(),
            run: test_run(params.run_id, RunStatus::Running),
        }))
    }

    async fn steer_run(
        &self,
        params: RunSteerParams,
    ) -> Result<AgentApiOutcome<RunSteerResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(RunSteerResponse {
            steering_id: "steering_1".to_owned(),
            run: test_run(params.run_id, RunStatus::Running),
        }))
    }

    async fn list_skills(
        &self,
        _params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SkillListResponse {
            catalogs: vec![SkillCatalogView {
                source: SkillCatalogSource::Vfs,
                availability: SkillCatalogAvailability::Available,
                warnings: vec![],
                catalog_ref: Some(format!("sha256:{}", "5".repeat(64))),
                skills: vec![SkillListItem {
                    skill_id: "skill:one".to_owned(),
                    name: "one".to_owned(),
                    description: "Use when testing skills.".to_owned(),
                    short_description: Some("test skill".to_owned()),
                    enabled: true,
                    location: SkillLocationView {
                        skill_dir_path: "/skills/one".into(),
                        skill_doc_path: "/skills/one/SKILL.md".into(),
                    },
                }],
            }],
        }))
    }

    async fn create_environment(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentCreateResponse>, AgentApiError> {
        assert_eq!(params.binding_id, "primary");
        Ok(AgentApiOutcome::new(EnvironmentCreateResponse {
            environment: test_environment_instance(),
        }))
    }

    async fn read_environment(
        &self,
        _params: EnvironmentReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentReadResponse {
            environment: test_environment_instance(),
        }))
    }

    async fn list_environments(
        &self,
        _params: EnvironmentListParams,
    ) -> Result<AgentApiOutcome<EnvironmentListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentListResponse {
            environments: vec![test_environment_instance()],
        }))
    }

    async fn close_environment(
        &self,
        _params: EnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<EnvironmentCloseResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentCloseResponse {
            environment: test_environment_instance(),
        }))
    }

    async fn create_external_environment(
        &self,
        _params: EnvironmentExternalCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentExternalCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentExternalCreateResponse {
            environment: test_external_environment(),
        }))
    }

    async fn put_environment_ingress(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIngressPutResponse>, AgentApiError> {
        let mut environment = test_environment_instance();
        environment.public_ingress_enabled = params.enabled;
        environment.public_endpoint = params
            .enabled
            .then(|| "https://opaque.env.example".to_owned());
        Ok(AgentApiOutcome::new(EnvironmentIngressPutResponse {
            environment,
        }))
    }

    async fn put_environment_power(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentPowerPutResponse>, AgentApiError> {
        let mut environment = test_environment_instance();
        environment.desired_power = params.power;
        Ok(AgentApiOutcome::new(EnvironmentPowerPutResponse {
            environment,
        }))
    }

    async fn put_environment_idle_policy(
        &self,
        params: EnvironmentIdlePolicyPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIdlePolicyPutResponse>, AgentApiError> {
        let mut environment = test_environment_instance();
        environment.idle_policy = params.idle_policy;
        Ok(AgentApiOutcome::new(EnvironmentIdlePolicyPutResponse {
            environment,
        }))
    }

    async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError> {
        assert_eq!(params.environment_id, "evi_test");
        let mut session = test_session("session_1".to_owned(), SessionStatus::Idle);
        session.active_environment_id = Some(params.environment_id);
        Ok(AgentApiOutcome::new(SessionEnvironmentActivateResponse {
            session,
        }))
    }

    async fn deactivate_session_environment(
        &self,
        _params: SessionEnvironmentDeactivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionEnvironmentDeactivateResponse {
            session: test_session("session_1".to_owned(), SessionStatus::Idle),
        }))
    }

    async fn bind_environment_credential(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialBindResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentCredentialBindResponse {
            credential: test_environment_credential(
                params.environment_id,
                params.env_name,
                params.source,
            ),
        }))
    }

    async fn list_environment_credentials(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentCredentialListResponse {
            credentials: vec![test_environment_credential(
                params.environment_id,
                "GITHUB_TOKEN".to_owned(),
                EnvironmentCredentialSourceView::AuthGrant {
                    grant_id: "authgrant_repo".to_owned(),
                },
            )],
        }))
    }

    async fn unbind_environment_credential(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialUnbindResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentCredentialUnbindResponse {
            credential: test_environment_credential(
                params.environment_id,
                params.env_name,
                EnvironmentCredentialSourceView::AuthGrant {
                    grant_id: "authgrant_repo".to_owned(),
                },
            ),
        }))
    }

    async fn create_environment_jobs(
        &self,
        _params: EnvironmentJobCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentJobCreateResponse {
            environment_id: "evi_test".to_owned(),
            job_group_id: "ejg_test".to_owned(),
            jobs: Vec::new(),
        }))
    }

    async fn read_environment_jobs(
        &self,
        _params: EnvironmentJobReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentJobReadResponse {
            jobs: Vec::new(),
        }))
    }

    async fn cancel_environment_jobs(
        &self,
        _params: EnvironmentJobCancelParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCancelResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentJobCancelResponse {
            jobs: Vec::new(),
        }))
    }

    async fn create_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyCreateResponse>, AgentApiError> {
        assert_eq!(params.display_name, "harbor");
        Ok(AgentApiOutcome::new(
            EnvironmentRegistrationKeyCreateResponse {
                registration_key: test_registration_key(),
                secret: EnvironmentRegistrationSecretView("lsrk_test-secret".to_owned()),
            },
        ))
    }

    async fn read_environment_registration_key(
        &self,
        _params: EnvironmentRegistrationKeyReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            EnvironmentRegistrationKeyReadResponse {
                registration_key: test_registration_key(),
            },
        ))
    }

    async fn list_environment_registration_keys(
        &self,
        _params: EnvironmentRegistrationKeyListParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            EnvironmentRegistrationKeyListResponse {
                registration_keys: vec![test_registration_key()],
            },
        ))
    }

    async fn revoke_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyRevokeResponse>, AgentApiError> {
        let mut key = test_registration_key();
        key.status = EnvironmentRegistrationKeyStatusView::Revoked;
        key.revoked_at_ms = Some(20);
        Ok(AgentApiOutcome::new(
            EnvironmentRegistrationKeyRevokeResponse {
                registration_key: key,
                closed_environment_ids: if params.close_environments {
                    vec!["evi_registered".to_owned()]
                } else {
                    Vec::new()
                },
            },
        ))
    }

    async fn put_blobs(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobPutResponse {
            blobs: params
                .blobs
                .into_iter()
                .enumerate()
                .map(|(index, blob)| BlobPutResult {
                    blob_ref: format!("sha256:{index:064x}"),
                    bytes: blob.bytes_base64.len() as u64,
                })
                .collect(),
        }))
    }

    async fn read_blob(
        &self,
        params: BlobReadParams,
    ) -> Result<AgentApiOutcome<BlobReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobReadResponse {
            blob_ref: params.blob_ref,
            bytes_base64: "aGVsbG8=".to_owned(),
            bytes: 5,
        }))
    }

    async fn has_blobs(
        &self,
        params: BlobHasParams,
    ) -> Result<AgentApiOutcome<BlobHasResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobHasResponse {
            blobs: params
                .blob_refs
                .into_iter()
                .map(|blob_ref| BlobHasItem {
                    blob_ref,
                    exists: true,
                })
                .collect(),
        }))
    }

    async fn commit_vfs_snapshot(
        &self,
        _params: VfsSnapshotCommitParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsSnapshotCommitResponse {
            snapshot_ref: format!("sha256:{}", "2".repeat(64)),
            files: 1,
            bytes: 5,
        }))
    }

    async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsSnapshotReadResponse {
            snapshot_ref: params.snapshot_ref,
            manifest: json!({
                "schema_version": "lightspeed.vfs.snapshot.v1",
                "root": { "entries": {} },
                "totals": { "files": 0, "bytes": 0 }
            }),
            files: 0,
            bytes: 0,
        }))
    }

    async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceCreateResponse>, AgentApiError> {
        let snapshot_ref = params
            .snapshot_ref
            .unwrap_or_else(|| format!("sha256:{}", "0".repeat(64)));
        Ok(AgentApiOutcome::new(VfsWorkspaceCreateResponse {
            workspace: VfsWorkspaceView {
                workspace_id: params
                    .workspace_id
                    .unwrap_or_else(|| "workspace_test".to_owned()),
                display_name: params.display_name,
                base_snapshot_ref: Some(snapshot_ref.clone()),
                head_snapshot_ref: snapshot_ref,
                files: 0,
                bytes: 0,
                revision: 0,
                created_at_ms: 10,
                updated_at_ms: 10,
            },
        }))
    }

    async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsWorkspaceReadResponse {
            workspace: test_workspace(params.workspace_id, 4),
        }))
    }

    async fn list_vfs_workspaces(
        &self,
        _params: VfsWorkspaceListParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsWorkspaceListResponse {
            workspaces: vec![test_workspace("workspace_test".to_owned(), 4)],
        }))
    }

    async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceUpdateResponse>, AgentApiError> {
        let mut workspace = test_workspace(
            params.workspace_id,
            params.expected_revision.unwrap_or(4) + 1,
        );
        workspace.head_snapshot_ref = params.snapshot_ref;
        workspace.display_name = params.display_name;
        Ok(AgentApiOutcome::new(VfsWorkspaceUpdateResponse {
            workspace,
        }))
    }

    async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsWorkspaceDeleteResponse {
            workspace: test_workspace(params.workspace_id, 4),
        }))
    }

    async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<AgentApiOutcome<McpServerPutResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(McpServerPutResponse {
            server: test_mcp_server(params.server.server_id),
        }))
    }

    async fn discover_mcp_server_auth(
        &self,
        params: McpServerAuthDiscoverParams,
    ) -> Result<AgentApiOutcome<McpServerAuthDiscoverResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(McpServerAuthDiscoverResponse {
            oauth: Some(McpOAuthDiscoveryView {
                resource: params.server_url,
                authorization_servers: vec!["https://auth.example.com".to_owned()],
                scopes_supported: vec!["mcp:tools".to_owned()],
            }),
        }))
    }

    async fn discover_mcp_server_tools(
        &self,
        params: McpServerToolsDiscoverParams,
    ) -> Result<AgentApiOutcome<McpServerToolsDiscoverResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            McpServerToolsDiscoverResponse::Success {
                tools: vec![McpAdvertisedToolView {
                    name: format!("{}_search", params.server_id),
                    title: Some("Search".to_owned()),
                    description: Some("Search the configured service".to_owned()),
                    annotations: Some(McpToolAnnotationsView {
                        read_only_hint: Some(true),
                        ..McpToolAnnotationsView::default()
                    }),
                }],
            },
        ))
    }

    async fn list_mcp_servers(
        &self,
        _params: McpServerListParams,
    ) -> Result<AgentApiOutcome<McpServerListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(McpServerListResponse {
            servers: vec![test_mcp_server("echo".to_owned())],
        }))
    }

    async fn read_mcp_server(
        &self,
        params: McpServerReadParams,
    ) -> Result<AgentApiOutcome<McpServerReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(McpServerReadResponse {
            server: test_mcp_server(params.server_id),
        }))
    }

    async fn delete_mcp_server(
        &self,
        params: McpServerDeleteParams,
    ) -> Result<AgentApiOutcome<McpServerDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(McpServerDeleteResponse {
            server: test_mcp_server(params.server_id),
        }))
    }

    async fn import_auth_grant(
        &self,
        params: AuthGrantImportParams,
    ) -> Result<AgentApiOutcome<AuthGrantImportResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGrantImportResponse {
            grant: test_auth_grant(
                params.grant_id.unwrap_or_else(|| "authgrant_1".to_owned()),
                AuthGrantStatus::Active,
            ),
        }))
    }

    async fn lease_auth_grant(
        &self,
        params: AuthGrantLeaseParams,
    ) -> Result<AgentApiOutcome<AuthGrantLeaseResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGrantLeaseResponse {
            token: "leased-token".to_owned(),
            expires_at_ms: None,
            grant_id: params.grant_id,
            provider_kind: AuthProviderKind::StaticBearer,
        }))
    }

    async fn list_auth_grants(
        &self,
        _params: AuthGrantListParams,
    ) -> Result<AgentApiOutcome<AuthGrantListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGrantListResponse {
            grants: vec![test_auth_grant(
                "authgrant_1".to_owned(),
                AuthGrantStatus::Active,
            )],
        }))
    }

    async fn read_auth_grant(
        &self,
        params: AuthGrantReadParams,
    ) -> Result<AgentApiOutcome<AuthGrantReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGrantReadResponse {
            grant: test_auth_grant(params.grant_id, AuthGrantStatus::Active),
        }))
    }

    async fn revoke_auth_grant(
        &self,
        params: AuthGrantRevokeParams,
    ) -> Result<AgentApiOutcome<AuthGrantRevokeResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGrantRevokeResponse {
            grant: test_auth_grant(params.grant_id, AuthGrantStatus::Revoked),
        }))
    }

    async fn create_auth_client(
        &self,
        params: AuthClientCreateParams,
    ) -> Result<AgentApiOutcome<AuthClientCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthClientCreateResponse {
            client: test_auth_client(params.client_id.unwrap_or_else(|| "crm".to_owned())),
        }))
    }

    async fn list_auth_clients(
        &self,
        _params: AuthClientListParams,
    ) -> Result<AgentApiOutcome<AuthClientListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthClientListResponse {
            clients: vec![test_auth_client("crm".to_owned())],
        }))
    }

    async fn read_auth_client(
        &self,
        params: AuthClientReadParams,
    ) -> Result<AgentApiOutcome<AuthClientReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthClientReadResponse {
            client: test_auth_client(params.client_id),
        }))
    }

    async fn delete_auth_client(
        &self,
        params: AuthClientDeleteParams,
    ) -> Result<AgentApiOutcome<AuthClientDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthClientDeleteResponse {
            client: test_auth_client(params.client_id),
        }))
    }

    async fn start_auth_flow(
        &self,
        params: AuthFlowStartParams,
    ) -> Result<AgentApiOutcome<AuthFlowStartResponse>, AgentApiError> {
        let _ = params;
        Ok(AgentApiOutcome::new(AuthFlowStartResponse {
            flow_id: "authflow_1".to_owned(),
            authorize_url: "https://as.example.com/authorize?state=test".to_owned(),
            expires_at_ms: 600_000,
        }))
    }

    async fn read_auth_flow_status(
        &self,
        params: AuthFlowStatusParams,
    ) -> Result<AgentApiOutcome<AuthFlowStatusResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthFlowStatusResponse {
            flow: AuthFlowView {
                flow_id: params.flow_id,
                client_id: "crm".to_owned(),
                provider_id: "crm".to_owned(),
                status: AuthFlowStatus::Pending,
                grant_id: None,
                error: None,
                expires_at_ms: 600_000,
                created_at_ms: 1,
                updated_at_ms: 2,
            },
        }))
    }

    async fn create_auth_provider(
        &self,
        params: AuthProviderCreateParams,
    ) -> Result<AgentApiOutcome<AuthProviderCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthProviderCreateResponse {
            provider: test_auth_provider(
                params
                    .provider_id
                    .unwrap_or_else(|| "lightspeed-github".to_owned()),
            ),
        }))
    }

    async fn list_auth_providers(
        &self,
        _params: AuthProviderListParams,
    ) -> Result<AgentApiOutcome<AuthProviderListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthProviderListResponse {
            providers: vec![test_auth_provider("lightspeed-github".to_owned())],
        }))
    }

    async fn read_auth_provider(
        &self,
        params: AuthProviderReadParams,
    ) -> Result<AgentApiOutcome<AuthProviderReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthProviderReadResponse {
            provider: test_auth_provider(params.provider_id),
        }))
    }

    async fn delete_auth_provider(
        &self,
        params: AuthProviderDeleteParams,
    ) -> Result<AgentApiOutcome<AuthProviderDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthProviderDeleteResponse {
            provider: test_auth_provider(params.provider_id),
        }))
    }

    async fn list_github_installations(
        &self,
        _params: AuthGitHubInstallationListParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGitHubInstallationListResponse {
            installations: vec![GitHubInstallationView {
                installation_id: 678,
                account_login: Some("acme".to_owned()),
                repository_selection: Some("selected".to_owned()),
                permissions: serde_json::json!({"contents": "read"}),
            }],
        }))
    }

    async fn grant_github_installation(
        &self,
        _params: AuthGitHubInstallationGrantParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationGrantResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(AuthGitHubInstallationGrantResponse {
            grant: test_auth_grant("authgrant_install".to_owned(), AuthGrantStatus::Active),
        }))
    }

    async fn create_bot(
        &self,
        _params: BotCreateParams,
    ) -> Result<AgentApiOutcome<BotCreateResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "create_bot is not part of the api test double",
        ))
    }

    async fn put_bot(
        &self,
        _params: BotPutParams,
    ) -> Result<AgentApiOutcome<BotPutResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "put_bot is not part of the api test double",
        ))
    }

    async fn read_bot(
        &self,
        _params: BotReadParams,
    ) -> Result<AgentApiOutcome<BotReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "read_bot is not part of the api test double",
        ))
    }

    async fn list_bots(
        &self,
        _params: BotListParams,
    ) -> Result<AgentApiOutcome<BotListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "list_bots is not part of the api test double",
        ))
    }

    async fn close_bot(
        &self,
        _params: BotCloseParams,
    ) -> Result<AgentApiOutcome<BotCloseResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "close_bot is not part of the api test double",
        ))
    }

    async fn delete_bot(
        &self,
        _params: BotDeleteParams,
    ) -> Result<AgentApiOutcome<BotDeleteResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "delete_bot is not part of the api test double",
        ))
    }

    async fn read_bot_state(
        &self,
        _params: BotStateReadParams,
    ) -> Result<AgentApiOutcome<BotStateReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "read_bot_state is not part of the api test double",
        ))
    }

    async fn rotate_bot_session(
        &self,
        _params: BotSessionRotateParams,
    ) -> Result<AgentApiOutcome<BotSessionRotateResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "rotate_bot_session is not part of the api test double",
        ))
    }

    async fn put_bot_trigger(
        &self,
        _params: BotTriggerPutParams,
    ) -> Result<AgentApiOutcome<BotTriggerPutResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "put_bot_trigger is not part of the api test double",
        ))
    }

    async fn read_bot_trigger(
        &self,
        _params: BotTriggerReadParams,
    ) -> Result<AgentApiOutcome<BotTriggerReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "read_bot_trigger is not part of the api test double",
        ))
    }

    async fn list_bot_triggers(
        &self,
        _params: BotTriggerListParams,
    ) -> Result<AgentApiOutcome<BotTriggerListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "list_bot_triggers is not part of the api test double",
        ))
    }

    async fn delete_bot_trigger(
        &self,
        _params: BotTriggerDeleteParams,
    ) -> Result<AgentApiOutcome<BotTriggerDeleteResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "delete_bot_trigger is not part of the api test double",
        ))
    }

    async fn admit_bot_event(
        &self,
        _params: BotEventAdmitParams,
    ) -> Result<AgentApiOutcome<BotEventAdmitResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "admit_bot_event is not part of the api test double",
        ))
    }

    async fn replay_bot_event(
        &self,
        _params: BotEventReplayParams,
    ) -> Result<AgentApiOutcome<BotEventReplayResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "replay_bot_event is not part of the api test double",
        ))
    }

    async fn list_bot_events(
        &self,
        _params: BotEventListParams,
    ) -> Result<AgentApiOutcome<BotEventListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "list_bot_events is not part of the api test double",
        ))
    }

    async fn read_bot_event(
        &self,
        _params: BotEventReadParams,
    ) -> Result<AgentApiOutcome<BotEventReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "read_bot_event is not part of the api test double",
        ))
    }

    async fn test_bot_filter(
        &self,
        _params: BotFilterTestParams,
    ) -> Result<AgentApiOutcome<BotFilterTestResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "test_bot_filter is not part of the api test double",
        ))
    }

    async fn create_channel_account(
        &self,
        _params: ChannelAccountCreateParams,
    ) -> Result<AgentApiOutcome<ChannelAccountCreateResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "create_channel_account is not part of the api test double",
        ))
    }

    async fn put_channel_account(
        &self,
        _params: ChannelAccountPutParams,
    ) -> Result<AgentApiOutcome<ChannelAccountPutResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "put_channel_account is not part of the api test double",
        ))
    }

    async fn read_channel_account(
        &self,
        _params: ChannelAccountReadParams,
    ) -> Result<AgentApiOutcome<ChannelAccountReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "read_channel_account is not part of the api test double",
        ))
    }

    async fn list_channel_accounts(
        &self,
        _params: ChannelAccountListParams,
    ) -> Result<AgentApiOutcome<ChannelAccountListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "list_channel_accounts is not part of the api test double",
        ))
    }

    async fn delete_channel_account(
        &self,
        _params: ChannelAccountDeleteParams,
    ) -> Result<AgentApiOutcome<ChannelAccountDeleteResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "delete_channel_account is not part of the api test double",
        ))
    }

    async fn admit_channel_inbound(
        &self,
        _params: ChannelInboundAdmitParams,
    ) -> Result<AgentApiOutcome<ChannelInboundAdmitResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "admit_channel_inbound is not part of the api test double",
        ))
    }

    async fn list_channel_pairings(
        &self,
        _params: ChannelPairingListParams,
    ) -> Result<AgentApiOutcome<ChannelPairingListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "list_channel_pairings is not part of the api test double",
        ))
    }

    async fn delete_channel_pairing(
        &self,
        _params: ChannelPairingDeleteParams,
    ) -> Result<AgentApiOutcome<ChannelPairingDeleteResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "delete_channel_pairing is not part of the api test double",
        ))
    }

    async fn read_channel_conversation(
        &self,
        _params: ChannelConversationReadParams,
    ) -> Result<AgentApiOutcome<ChannelConversationReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "read_channel_conversation is not part of the api test double",
        ))
    }
}

fn test_auth_provider(provider_id: String) -> AuthProviderView {
    AuthProviderView {
        provider_id,
        provider_kind: AuthProviderKind::GitHubApp,
        display_name: None,
        config: AuthProviderConfigView::GitHubApp {
            app_id: "12345".to_owned(),
            api_base_url: "https://api.github.com".to_owned(),
        },
        has_credential: true,
        status: AuthProviderStatus::Active,
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}

fn test_auth_client(client_id: String) -> OAuthClientView {
    OAuthClientView {
        client_id,
        provider_id: "crm".to_owned(),
        provider_kind: AuthProviderKind::McpOAuth,
        display_name: None,
        authorization_endpoint: "https://as.example.com/authorize".to_owned(),
        token_endpoint: "https://as.example.com/token".to_owned(),
        remote_client_id: "client-1".to_owned(),
        has_client_secret: false,
        token_endpoint_auth_method: TokenEndpointAuthMethod::None,
        scopes_default: Vec::new(),
        audience: Some("https://crm.example.com/mcp".to_owned()),
        authorization_server_issuer: Some("https://as.example.com".to_owned()),
        authorization_response_iss_parameter_supported: true,
        authorization_server_scopes_supported: Vec::new(),
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}

fn test_auth_grant(grant_id: String, status: AuthGrantStatus) -> AuthGrantView {
    AuthGrantView {
        grant_id,
        provider_id: "static".to_owned(),
        provider_kind: AuthProviderKind::StaticBearer,
        exposure: AuthGrantExposure::Brokered,
        principal: PrincipalRefView::default(),
        display_name: None,
        subject_hint: None,
        scopes: Vec::new(),
        audience: None,
        has_access_token: true,
        has_refresh_token: false,
        expires_at_ms: None,
        status,
        metadata: serde_json::Value::Object(Default::default()),
        last_leased_at_ms: None,
        lease_count: 0,
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}

fn test_profile(profile_id: ProfileId) -> AgentProfile {
    AgentProfile {
        profile_id,
        display_name: Some("Support".to_owned()),
        description: Some("Ticket support profile".to_owned()),
        revision: 1,
        document: ProfileDocument {
            metadata: Default::default(),
            retention: None,
            config: Some(SessionConfig {
                features: Some(FeaturesConfig {
                    subagents: Some(SubagentsFeature {
                        version: CURRENT_FEATURE_VERSION,
                        agents: vec![SubagentAgentRef {
                            profile_id: ProfileId::new("reviewer"),
                        }],
                        max_depth: 2,
                        max_descendants: 16,
                        max_concurrent: 4,
                        deadline_ms: 3_600_000,
                    }),
                    ..FeaturesConfig::default()
                }),
                ..SessionConfig::default()
            }),
            instructions: Some(ProfileInstructions::Text {
                text: "Be concise.".to_owned(),
            }),
            environment: None,
        },
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}

fn test_workspace(workspace_id: String, revision: u64) -> VfsWorkspaceView {
    VfsWorkspaceView {
        workspace_id,
        display_name: Some("Test workspace".to_owned()),
        base_snapshot_ref: Some(format!("sha256:{}", "2".repeat(64))),
        head_snapshot_ref: format!("sha256:{}", "3".repeat(64)),
        files: 2,
        bytes: 64,
        revision,
        created_at_ms: 10,
        updated_at_ms: 20,
    }
}

fn test_session(id: SessionId, status: SessionStatus) -> SessionView {
    let retention = test_session_retention(&id);
    SessionView {
        metadata: Default::default(),
        id,
        display_name: Some("Test session".to_owned()),
        status,
        closed_at_ms: None,
        retention,
        active_run: None,
        managed: false,
        config_revision: 0,
        config: None,
        active_environment_id: None,
        created_at_ms: 1,
        updated_at_ms: 2,
        runs: Vec::new(),
        active_context: ContextView::default(),
        active_tools: ActiveToolsView::default(),
        management: None,
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

fn test_session_mutation(id: SessionId, status: SessionStatus) -> SessionMutationView {
    SessionMutationView {
        id,
        status,
        head_cursor: None,
        config_revision: 0,
        context_revision: 0,
    }
}

fn test_environment_credential(
    environment_id: EnvironmentId,
    env_name: String,
    source: EnvironmentCredentialSourceView,
) -> EnvironmentCredentialView {
    EnvironmentCredentialView {
        environment_id,
        env_name,
        source,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

fn test_environment_instance() -> EnvironmentView {
    EnvironmentView {
        environment_id: "evi_test".to_owned(),
        request_id: "request-1".to_owned(),
        source: EnvironmentSourceView::Provisioned {
            provider_id: "bridge-local".to_owned(),
            binding_id: "primary".to_owned(),
        },
        display_name: Some("Local".to_owned()),
        status: EnvironmentLifecycleStatusView::Ready,
        desired_power: EnvironmentPowerStateView::Running,
        idle_policy: None,
        incarnation: EnvironmentIncarnationView {
            incarnation_id: "incarnation-1".to_owned(),
            provision_request_id: Some("request-1".to_owned()),
            provider_target_id: Some("local".to_owned()),
            template_id: Some("rust-v1".to_owned()),
            power_states: vec![
                EnvironmentPowerStateView::Running,
                EnvironmentPowerStateView::Paused,
            ],
            created_at_ms: 10,
            updated_at_ms: 10,
        },
        public_ingress_enabled: false,
        public_endpoint: None,
        origin_session: None,
        metadata: BTreeMap::new(),
        last_seen_at_ms: None,
        created_at_ms: 10,
        updated_at_ms: 10,
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
        expires_at_ms: None,
        status: EnvironmentRegistrationKeyStatusView::Active,
        registered_environment_count: 1,
        active_environment_count: 1,
        last_registered_at_ms: Some(10),
        created_at_ms: 5,
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

fn test_external_environment() -> EnvironmentView {
    EnvironmentView {
        environment_id: "evi_enrolled".to_owned(),
        request_id: "request-enrolled".to_owned(),
        source: EnvironmentSourceView::External {
            connection: EnvironmentConnectionView {
                endpoint: "ws://envd.test:19091".to_owned(),
                transport: EnvironmentConnectionTransportView::WebSocket,
            },
        },
        display_name: Some("External".to_owned()),
        status: EnvironmentLifecycleStatusView::Ready,
        desired_power: EnvironmentPowerStateView::Running,
        idle_policy: None,
        incarnation: EnvironmentIncarnationView {
            incarnation_id: "incarnation-enrolled".to_owned(),
            provision_request_id: None,
            provider_target_id: None,
            template_id: None,
            power_states: Vec::new(),
            created_at_ms: 10,
            updated_at_ms: 10,
        },
        public_ingress_enabled: false,
        public_endpoint: None,
        origin_session: None,
        metadata: BTreeMap::new(),
        last_seen_at_ms: None,
        created_at_ms: 10,
        updated_at_ms: 10,
    }
}

fn test_run(id: RunId, status: RunStatus) -> RunView {
    RunView {
        output: None,
        output_text: None,
        id,
        status,
        started_at_ms: None,
        completed_at_ms: None,
        source: RunViewSource::Input { items: Vec::new() },
        entries: Vec::new(),
        tool_batches: Vec::new(),
        usage: None,
        pending_approvals: Vec::new(),
    }
}

fn test_mcp_server(server_id: String) -> McpServerView {
    McpServerView {
        default_server_label: server_id.clone(),
        server_url: format!("https://{server_id}.example.com/mcp"),
        server_id,
        display_name: None,
        description: None,
        allowed_tools: None,
        execution: RemoteMcpExecution::Provider,
        exposure: RemoteMcpExposure::Inject,
        approval_default: RemoteMcpApprovalPolicy::Never,
        defer_loading_default: None,
        allow_private_network: false,
        auth_policy: McpServerAuthPolicy::None,
        credential: None,
        status: McpServerStatus::Active,
        revision: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

struct TestOperatorService;

fn test_operator_universe(universe_id: &str) -> OperatorUniverseView {
    OperatorUniverseView {
        universe_id: universe_id.to_owned(),
        slug: Some("acme".to_owned()),
        created_at_ms: 10,
        last_activity_at_ms: Some(20),
        sessions: 3,
        workspaces: 2,
        profiles: 1,
        blob_bytes: 4096,
    }
}

fn test_operator_api_key(key_prefix: &str) -> OperatorApiKeyView {
    OperatorApiKeyView {
        key_prefix: key_prefix.to_owned(),
        display_name: Some("coding agent".to_owned()),
        created_at_ms: 10,
        revoked_at_ms: None,
        last_used_at_ms: Some(20),
    }
}

fn test_operator_environment_provider(provider_id: &str) -> OperatorEnvironmentProviderView {
    OperatorEnvironmentProviderView {
        provider_id: provider_id.to_owned(),
        display_name: Some("Local Incus".to_owned()),
        controller_connection: OperatorEnvironmentProviderConnection {
            endpoint: "ws://127.0.0.1:19090/control".to_owned(),
            transport: OperatorEnvironmentProviderTransport::WebSocket,
        },
        metadata: BTreeMap::new(),
        created_at_ms: 10,
        updated_at_ms: 20,
    }
}

#[async_trait]
impl OperatorApiService for TestOperatorService {
    async fn create_universe(
        &self,
        params: OperatorUniverseCreateParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(OperatorUniverseCreateResponse {
            universe: test_operator_universe(&params.universe_id),
            created: true,
        }))
    }

    async fn list_universes(
        &self,
        _params: OperatorUniverseListParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(OperatorUniverseListResponse {
            universes: vec![test_operator_universe("universe_test")],
        }))
    }

    async fn read_universe(
        &self,
        params: OperatorUniverseReadParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(OperatorUniverseReadResponse {
            universe: test_operator_universe(&params.universe_id),
        }))
    }

    async fn delete_universe(
        &self,
        params: OperatorUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(OperatorUniverseDeleteResponse {
            universe_id: params.universe_id,
            workflows_terminated: 2,
            blob_objects_deleted: 5,
        }))
    }

    async fn create_api_key(
        &self,
        _params: OperatorApiKeyCreateParams,
    ) -> Result<AgentApiOutcome<OperatorApiKeyCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(OperatorApiKeyCreateResponse {
            api_key: test_operator_api_key("lsk_ab12cd34"),
            secret: "lsk_one_time_secret".to_owned(),
        }))
    }

    async fn list_api_keys(
        &self,
        _params: OperatorApiKeyListParams,
    ) -> Result<AgentApiOutcome<OperatorApiKeyListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(OperatorApiKeyListResponse {
            api_keys: vec![test_operator_api_key("lsk_ab12cd34")],
        }))
    }

    async fn revoke_api_key(
        &self,
        _params: OperatorApiKeyRevokeParams,
    ) -> Result<AgentApiOutcome<OperatorApiKeyRevokeResponse>, AgentApiError> {
        let mut api_key = test_operator_api_key("lsk_ab12cd34");
        api_key.revoked_at_ms = Some(30);
        Ok(AgentApiOutcome::new(OperatorApiKeyRevokeResponse {
            api_key,
        }))
    }

    async fn put_environment_provider(
        &self,
        params: OperatorEnvironmentProviderPutParams,
    ) -> Result<AgentApiOutcome<OperatorEnvironmentProviderPutResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            OperatorEnvironmentProviderPutResponse {
                provider: test_operator_environment_provider(&params.provider_id),
            },
        ))
    }

    async fn list_environment_providers(
        &self,
        _params: OperatorEnvironmentProviderListParams,
    ) -> Result<AgentApiOutcome<OperatorEnvironmentProviderListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            OperatorEnvironmentProviderListResponse {
                providers: vec![test_operator_environment_provider("incus-local")],
            },
        ))
    }

    async fn read_environment_provider(
        &self,
        params: OperatorEnvironmentProviderReadParams,
    ) -> Result<AgentApiOutcome<OperatorEnvironmentProviderReadResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            OperatorEnvironmentProviderReadResponse {
                provider: test_operator_environment_provider(&params.provider_id),
            },
        ))
    }

    async fn delete_environment_provider(
        &self,
        params: OperatorEnvironmentProviderDeleteParams,
    ) -> Result<AgentApiOutcome<OperatorEnvironmentProviderDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            OperatorEnvironmentProviderDeleteResponse {
                provider: test_operator_environment_provider(&params.provider_id),
            },
        ))
    }

    async fn adopt_environment(
        &self,
        params: OperatorEnvironmentAdoptParams,
    ) -> Result<AgentApiOutcome<OperatorEnvironmentAdoptResponse>, AgentApiError> {
        let mut environment = test_environment_instance();
        environment.request_id = params.request_id;
        environment.display_name = params.display_name;
        environment.incarnation.template_id = None;
        Ok(AgentApiOutcome::new(OperatorEnvironmentAdoptResponse {
            environment,
        }))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn operator_environment_provider_methods_dispatch() {
    let requests = [
        (
            METHOD_OPERATOR_ENVIRONMENT_PROVIDERS_PUT,
            json!({
                "providerId": "incus-local",
                "displayName": "Local Incus",
                "controllerConnection": {
                    "endpoint": "ws://127.0.0.1:19090/control",
                    "transport": { "type": "webSocket" }
                }
            }),
        ),
        (METHOD_OPERATOR_ENVIRONMENT_PROVIDERS_LIST, json!({})),
        (
            METHOD_OPERATOR_ENVIRONMENT_PROVIDERS_READ,
            json!({ "providerId": "incus-local" }),
        ),
        (
            METHOD_OPERATOR_ENVIRONMENT_PROVIDERS_DELETE,
            json!({ "providerId": "incus-local" }),
        ),
    ];
    for (index, (method, params)) in requests.into_iter().enumerate() {
        let response = dispatch_operator_json_rpc(
            &TestOperatorService,
            JsonRpcRequest {
                id: RequestId::Number(index as u64),
                method: method.to_owned(),
                params: Some(params),
            },
        )
        .await;
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        assert!(
            response.result.expect("result")["result"]
                .get(if method.ends_with("/list") {
                    "providers"
                } else {
                    "provider"
                })
                .is_some(),
            "{method}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn operator_environment_adoption_dispatches_explicit_ownership_transfer() {
    let response = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_OPERATOR_ENVIRONMENTS_ADOPT.to_owned(),
            params: Some(json!({
                "universeId": "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f",
                "requestId": "adopt-1",
                "bindingId": "primary",
                "sourceTarget": "legacy/hand-built-vm",
                "takeOwnership": true,
                "displayName": "Imported VM"
            })),
        },
    )
    .await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let environment = &response.result.expect("result")["result"]["environment"];
    assert_eq!(environment["requestId"], "adopt-1");
    assert_eq!(environment["displayName"], "Imported VM");
    assert!(environment["incarnation"].get("templateId").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_operator_json_rpc_routes_universe_lifecycle() {
    let create = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_OPERATOR_UNIVERSES_CREATE.to_owned(),
            params: Some(json!({ "universeId": "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f" })),
        },
    )
    .await;
    assert!(create.error.is_none());
    let result = create.result.expect("result");
    assert_eq!(result["result"]["created"], json!(true));
    assert_eq!(
        result["result"]["universe"]["universeId"],
        json!("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f")
    );

    let list = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(2),
            method: METHOD_OPERATOR_UNIVERSES_LIST.to_owned(),
            params: Some(json!({})),
        },
    )
    .await;
    let result = list.result.expect("result");
    assert_eq!(result["result"]["universes"][0]["sessions"], json!(3));
    assert_eq!(result["result"]["universes"][0]["blobBytes"], json!(4096));

    let delete = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(3),
            method: METHOD_OPERATOR_UNIVERSES_DELETE.to_owned(),
            params: Some(json!({ "universeId": "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f" })),
        },
    )
    .await;
    let result = delete.result.expect("result");
    assert_eq!(result["result"]["workflowsTerminated"], json!(2));
    assert_eq!(result["result"]["blobObjectsDeleted"], json!(5));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_operator_json_rpc_routes_scoped_api_key_management() {
    let universe_id = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
    let create = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_OPERATOR_API_KEYS_CREATE.to_owned(),
            params: Some(json!({
                "universeId": universe_id,
                "displayName": "coding agent",
                "principal": { "kind": "serviceAccount", "id": "agent-1" }
            })),
        },
    )
    .await;
    let result = create.result.expect("create result");
    assert_eq!(result["result"]["apiKey"]["keyPrefix"], "lsk_ab12cd34");
    assert_eq!(result["result"]["secret"], "lsk_one_time_secret");

    let list = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(2),
            method: METHOD_OPERATOR_API_KEYS_LIST.to_owned(),
            params: Some(json!({ "universeId": universe_id })),
        },
    )
    .await;
    let result = list.result.expect("list result");
    assert_eq!(result["result"]["apiKeys"].as_array().unwrap().len(), 1);
    assert!(result["result"]["apiKeys"][0].get("secret").is_none());

    let revoke = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(3),
            method: METHOD_OPERATOR_API_KEYS_REVOKE.to_owned(),
            params: Some(json!({
                "universeId": universe_id,
                "keyPrefix": "lsk_ab12cd34"
            })),
        },
    )
    .await;
    let result = revoke.result.expect("revoke result");
    assert_eq!(result["result"]["apiKey"]["revokedAtMs"], 30);
}

#[test]
fn operator_api_key_create_response_redacts_secret_in_debug_output() {
    let response = OperatorApiKeyCreateResponse {
        api_key: test_operator_api_key("lsk_ab12cd34"),
        secret: "lsk_one_time_secret".to_owned(),
    };
    let debug = format!("{response:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("lsk_one_time_secret"));
}

#[tokio::test(flavor = "current_thread")]
async fn operator_dispatch_rejects_universe_scoped_methods_and_vice_versa() {
    // The operator dispatcher only knows operator methods…
    let response = dispatch_operator_json_rpc(
        &TestOperatorService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_LIST.to_owned(),
            params: Some(json!({})),
        },
    )
    .await;
    assert_eq!(response.error.expect("error").code, -32601);

    // …and the universe dispatcher does not know operator methods.
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(2),
            method: METHOD_OPERATOR_UNIVERSES_LIST.to_owned(),
            params: Some(json!({})),
        },
    )
    .await;
    assert_eq!(response.error.expect("error").code, -32601);
}
