use async_trait::async_trait;
use serde_json::json;

use super::*;

#[test]
fn notification_serializes_as_json_rpc_lite_shape() {
    let notification = AgentNotification::RunCompleted {
        session_id: "session_1".to_owned(),
        run: RunView {
            id: "run_1".to_owned(),
            status: RunStatus::Completed,
            source: RunViewSource::Input {
                items: vec![InputItem::Text {
                    text: "hello".to_owned(),
                }],
            },
            items: Vec::new(),
            tool_batches: Vec::new(),
        },
    };

    let value = serde_json::to_value(notification).expect("serialize notification");

    assert_eq!(
        value,
        json!({
            "method": "run/completed",
            "params": {
                "sessionId": "session_1",
                "run": {
                    "id": "run_1",
                    "status": "completed",
                    "source": {
                        "type": "input",
                        "items": [{ "type": "text", "text": "hello" }]
                    },
                    "items": []
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

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["serverInfo"]["name"],
        json!("test-service")
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

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["session"]["status"],
        json!("closed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_update() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_UPDATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "expectedConfigRevision": 0,
                "patch": {
                    "instructions": {
                        "op": "set",
                        "value": {
                            "type": "text",
                            "text": "answer tersely"
                        }
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
async fn dispatch_json_rpc_routes_session_tools_update() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_TOOLS_UPDATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "expectedToolsRevision": 4,
                "update": {
                    "type": "patch",
                    "upsert": [],
                    "remove": []
                }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["session"]["activeTools"]["revision"],
        json!(5)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_context_compact() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_CONTEXT_COMPACT.to_owned(),
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
            method: METHOD_CONTEXT_REMOVE.to_owned(),
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
            method: METHOD_CONTEXT_APPEND.to_owned(),
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
async fn dispatch_json_rpc_routes_outbox_read_and_ack() {
    let read = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_OUTBOX_READ.to_owned(),
            params: Some(json!({ "after": 7, "waitMs": 100 })),
        },
    )
    .await;
    assert!(read.error.is_none());
    let read = read.result.expect("result");
    assert_eq!(read["result"]["nextAfter"], json!(8));
    assert_eq!(
        read["result"]["entries"][0]["payload"]["type"],
        json!("send")
    );

    let ack = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(2),
            method: METHOD_OUTBOX_ACK.to_owned(),
            params: Some(json!({
                "outboxId": "outbox_1",
                "result": { "type": "delivered", "channelMessageId": "42" }
            })),
        },
    )
    .await;
    assert!(ack.error.is_none());
    assert_eq!(
        ack.result.expect("result")["result"]["status"],
        json!("delivered")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_run_cancel() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_RUN_CANCEL.to_owned(),
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
async fn dispatch_json_rpc_routes_prompts_active() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_PROMPTS_ACTIVE.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["report"]["total_chars"],
        json!(42)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_skills_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SKILLS_LIST.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["skills"][0]["active"],
        json!(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_skills_active() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SKILLS_ACTIVE.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["activations"][0]["source"]["type"],
        json!("directContext")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_skills_activate() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SKILLS_ACTIVATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "skillId": "skill:one",
                "scope": "session"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["activation"]["scope"],
        json!("session")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_skills_deactivate() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SKILLS_DEACTIVATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "skillId": "skill:one"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["skillId"],
        json!("skill:one")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_LIST.to_owned(),
            params: Some(json!({ "sessionId": "session_1" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["environments"][0]["active"],
        json!(true)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_read() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_READ.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["environment"]["envId"],
        json!("test")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_create() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_CREATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test",
                "providerId": "sandbox-pool",
                "request": {
                    "type": "sandbox",
                    "spec": {
                        "image": "ubuntu:latest",
                        "cwd": "/workspace"
                    }
                },
                "activate": true
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["environment"]["envId"],
        json!("test")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_attach() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_ATTACH.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test",
                "providerId": "bridge-local",
                "request": {
                    "type": "target",
                    "targetId": "local-host"
                }
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["environment"]["kind"],
        json!("attachedHost")
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
                "envId": "test"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["activeEnvId"],
        json!("test")
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
    let result = response.result.expect("result");
    assert!(result["result"]["activeEnvId"].is_null());
    assert_eq!(result["result"]["environments"][0]["active"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environments_close() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENTS_CLOSE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test",
                "force": true
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["environment"]["status"],
        json!("detached")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_environment_credentials_bind() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENT_CREDENTIALS_BIND.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test",
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
async fn dispatch_json_rpc_routes_session_environment_credentials_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENT_CREDENTIALS_LIST.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test"
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
async fn dispatch_json_rpc_routes_session_environment_credentials_unbind() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_ENVIRONMENT_CREDENTIALS_UNBIND.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test",
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
async fn dispatch_json_rpc_routes_session_jobs_create() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_JOBS_CREATE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "envId": "test",
                "requestId": "request_1",
                "jobs": [{
                    "name": "build",
                    "argv": ["cargo", "test"]
                }]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["jobs"][0]["handle"]["jobId"],
        json!("job-1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_jobs_list() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_JOBS_LIST.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "limit": 10
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["jobs"][0]["namespace"],
        json!("session_1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_jobs_read() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_JOBS_READ.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "jobs": [{
                    "envId": "test",
                    "jobId": "job-1"
                }],
                "outputBytes": 1024
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["jobs"][0]["summary"]["status"],
        json!("succeeded")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_jobs_cancel() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_JOBS_CANCEL.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "jobs": [{
                    "envId": "test",
                    "jobId": "job-1"
                }],
                "force": true
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["jobs"][0]["summary"]["status"],
        json!("cancelled")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_provider_register() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENT_PROVIDERS_REGISTER.to_owned(),
            params: Some(json!({
                "providerId": "bridge-local",
                "providerKind": "bridge",
                "controllerConnection": {
                    "endpoint": "ws://127.0.0.1:9000/controller",
                    "transport": { "type": "webSocket" }
                },
                "capabilities": {
                    "listTargets": true,
                    "attachTarget": true,
                    "getTarget": true
                },
                "implementation": {
                    "name": "test-bridge",
                    "version": "1.0.0"
                },
                "leaseTtlMs": 30000
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["provider"]["providerId"],
        json!("bridge-local")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_provider_heartbeat() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT.to_owned(),
            params: Some(json!({
                "providerId": "bridge-local",
                "observedTargets": [{
                    "targetId": "local-host",
                    "status": "ready",
                    "scope": { "type": "default" },
                    "capabilities": {
                        "filesystemRead": true,
                        "filesystemWrite": true,
                        "processStart": true,
                        "processStdin": true
                    },
                    "defaultCwd": "/workspace"
                }]
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["targets"][0]["targetId"],
        json!("local-host")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_environment_provider_unregister() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER.to_owned(),
            params: Some(json!({ "providerId": "bridge-local" })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["provider"]["status"],
        json!("offline")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_mcp_server_create() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_MCP_SERVERS_CREATE.to_owned(),
            params: Some(json!({
                "serverId": "echo",
                "serverUrl": "https://echo.example.com/mcp",
                "defaultServerLabel": "echo"
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

#[test]
fn mcp_server_create_params_default_approval_is_never() {
    let params: McpServerCreateParams = serde_json::from_value(json!({
        "serverId": "echo",
        "serverUrl": "https://echo.example.com/mcp",
        "defaultServerLabel": "echo"
    }))
    .expect("params");

    assert_eq!(params.approval_default, RemoteMcpApprovalPolicy::Never);
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_mcp_link() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_MCP_LINK.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "serverId": "echo",
                "toolId": "mcp_echo"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["link"]["toolId"],
        json!("mcp_echo")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_session_mcp_unlink() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_SESSION_MCP_UNLINK.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "toolId": "mcp_echo"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["toolId"],
        json!("mcp_echo")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_run_start_with_config() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_RUN_START.to_owned(),
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
            method: METHOD_BLOB_PUT_MANY.to_owned(),
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
            method: METHOD_VFS_SNAPSHOT_COMMIT.to_owned(),
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
            method: METHOD_VFS_WORKSPACE_CREATE.to_owned(),
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
            method: METHOD_VFS_WORKSPACE_READ.to_owned(),
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
            method: METHOD_VFS_WORKSPACE_UPDATE.to_owned(),
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
            method: METHOD_VFS_WORKSPACE_UPDATE.to_owned(),
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
            method: METHOD_VFS_WORKSPACE_DELETE.to_owned(),
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

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_mount_put() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_MOUNT_PUT.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "mountPath": "/workspace",
                "source": { "type": "workspace", "workspaceId": "workspace_1" },
                "access": "readWrite"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["mount"]["source"]["workspaceId"],
        json!("workspace_1")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_json_rpc_routes_vfs_mount_delete() {
    let response = dispatch_json_rpc(
        &TestService,
        JsonRpcRequest {
            id: RequestId::Number(1),
            method: METHOD_VFS_MOUNT_DELETE.to_owned(),
            params: Some(json!({
                "sessionId": "session_1",
                "mountPath": "/workspace"
            })),
        },
    )
    .await;

    assert!(response.error.is_none());
    assert_eq!(
        response.result.expect("result")["result"]["mountPath"],
        json!("/workspace")
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
            output_ref: Some("sha256:abc".to_owned()),
        },
    };

    let value = serde_json::to_value(AgentNotification::SessionEvent { event })
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
                        "outputRef": "sha256:abc"
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
fn provider_context_item_serializes_debug_metadata() {
    let item = SessionItemView::ProviderContext {
        id: "item_42".to_owned(),
        content_ref: "sha256:compact".to_owned(),
        media_type: Some("application/json".to_owned()),
        preview: Some("OpenAI Responses compaction item".to_owned()),
        provider_kind: Some("openai.responses.compaction".to_owned()),
        provider_item_id: Some("item_compaction_1".to_owned()),
        token_estimate: Some(TokenEstimateView {
            tokens: 123,
            quality: TokenEstimateQualityView::ProviderCounted,
        }),
        display: None,
    };

    let value = serde_json::to_value(item).expect("serialize provider context item");

    assert_eq!(
        value,
        json!({
            "type": "providerContext",
            "id": "item_42",
            "contentRef": "sha256:compact",
            "mediaType": "application/json",
            "preview": "OpenAI Responses compaction item",
            "providerKind": "openai.responses.compaction",
            "providerItemId": "item_compaction_1",
            "tokenEstimate": {
                "tokens": 123,
                "quality": "providerCounted"
            }
        })
    );
}

#[test]
fn provider_context_item_serializes_mcp_display() {
    let item = SessionItemView::ProviderContext {
        id: "item_43".to_owned(),
        content_ref: "sha256:mcp".to_owned(),
        media_type: Some("application/json".to_owned()),
        preview: Some("OpenAI Responses MCP tool call: echo.echo".to_owned()),
        provider_kind: Some("openai.responses.mcp_call".to_owned()),
        provider_item_id: Some("mcp_1".to_owned()),
        token_estimate: None,
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
    };

    let value = serde_json::to_value(item).expect("serialize mcp provider context item");

    assert_eq!(
        value,
        json!({
            "type": "providerContext",
            "id": "item_43",
            "contentRef": "sha256:mcp",
            "mediaType": "application/json",
            "preview": "OpenAI Responses MCP tool call: echo.echo",
            "providerKind": "openai.responses.mcp_call",
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
        id: "run_1".to_owned(),
        status: RunStatus::Running,
        source: RunViewSource::Input { items: Vec::new() },
        items: Vec::new(),
        tool_batches: vec![ToolBatchView {
            id: "tool_batch_1".to_owned(),
            turn_id: "turn_1".to_owned(),
            status: ToolItemStatus::Succeeded,
            calls: vec![ToolCallView {
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
    };

    let value = serde_json::to_value(run).expect("serialize run");

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
    // `{universe_id}/{session_id}` (P90 multi-tenancy). Session ids rejecting
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
    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(InitializeResponse {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            server_info: ServerInfo {
                name: "test-service".to_owned(),
                version: "0".to_owned(),
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

    async fn update_profile(
        &self,
        params: ProfileUpdateParams,
    ) -> Result<AgentApiOutcome<ProfileUpdateResponse>, AgentApiError> {
        let mut profile = test_profile(params.profile_id);
        profile.revision = params.expected_revision.unwrap_or(profile.revision) + 1;
        Ok(AgentApiOutcome::new(ProfileUpdateResponse { profile }))
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

    async fn update_session(
        &self,
        params: SessionUpdateParams,
    ) -> Result<AgentApiOutcome<SessionUpdateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionUpdateResponse {
            session: test_session(params.session_id, SessionStatus::Idle),
        }))
    }

    async fn update_session_tools(
        &self,
        params: SessionToolsUpdateParams,
    ) -> Result<AgentApiOutcome<SessionToolsUpdateResponse>, AgentApiError> {
        let mut session = test_session(params.session_id, SessionStatus::Idle);
        session.active_tools.revision = params.expected_tools_revision.unwrap_or(0) + 1;
        Ok(AgentApiOutcome::new(SessionToolsUpdateResponse { session }))
    }

    async fn read_session(
        &self,
        _params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        Err(AgentApiError::internal("not implemented"))
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
            session: test_session(params.session_id, SessionStatus::Closed),
        }))
    }

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(ContextCompactResponse {
            session: test_session(params.session_id, SessionStatus::Idle),
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

    async fn read_outbox(
        &self,
        params: OutboxReadParams,
    ) -> Result<AgentApiOutcome<OutboxReadResponse>, AgentApiError> {
        let after = params.after.unwrap_or(0);
        Ok(AgentApiOutcome::new(OutboxReadResponse {
            entries: vec![OutboundMessageView {
                seq: after + 1,
                outbox_id: "outbox_1".to_owned(),
                session_id: "session_1".to_owned(),
                run_id: Some("run_1".to_owned()),
                origin: OutboundOriginView::ToolCall,
                payload: OutboundPayloadView::Send {
                    text: "hello".to_owned(),
                    reply_to: None,
                },
                attempts: 0,
                created_at_ms: 1,
            }],
            next_after: after + 1,
        }))
    }

    async fn ack_outbox(
        &self,
        params: OutboxAckParams,
    ) -> Result<AgentApiOutcome<OutboxAckResponse>, AgentApiError> {
        let status = match params.result {
            OutboundAckInput::Delivered { .. } => OutboundStatusView::Delivered,
            OutboundAckInput::Failed { .. } => OutboundStatusView::Failed,
        };
        Ok(AgentApiOutcome::new(OutboxAckResponse {
            outbox_id: params.outbox_id,
            status,
            attempts: 1,
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
        assert_eq!(generation.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(config.model.expect("model").model, "gpt-5.5");
        Ok(AgentApiOutcome::new(RunStartResponse {
            run: test_run("run_1".to_owned(), RunStatus::Running),
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

    async fn active_prompts(
        &self,
        _params: PromptsActiveParams,
    ) -> Result<AgentApiOutcome<PromptsActiveResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(PromptsActiveResponse {
            instructions: vec![PromptInstructionView {
                key: "instructions.100.prompts.0000.project".to_owned(),
                instructions_ref: format!("sha256:{}", "4".repeat(64)),
                media_type: Some("text/markdown".to_owned()),
                preview: Some("prompt instructions: instructions.md".to_owned()),
            }],
            report_ref: Some(format!("sha256:{}", "5".repeat(64))),
            report: Some(json!({
                "schema_version": "lightspeed.prompts.instructions.report.v1",
                "total_chars": 42
            })),
        }))
    }

    async fn list_skills(
        &self,
        _params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SkillListResponse {
            catalog_ref: Some(format!("sha256:{}", "5".repeat(64))),
            skills: vec![SkillListItem {
                skill_id: "skill:one".to_owned(),
                name: "one".to_owned(),
                description: "Use when testing skills.".to_owned(),
                short_description: Some("test skill".to_owned()),
                enabled: true,
                active: true,
            }],
        }))
    }

    async fn active_skills(
        &self,
        _params: SkillActiveParams,
    ) -> Result<AgentApiOutcome<SkillActiveResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SkillActiveResponse {
            catalog_ref: Some(format!("sha256:{}", "5".repeat(64))),
            activations: vec![test_skill_activation(SkillActivationScope::Run)],
        }))
    }

    async fn activate_skill(
        &self,
        params: SkillActivateParams,
    ) -> Result<AgentApiOutcome<SkillActivateResponse>, AgentApiError> {
        assert_eq!(params.skill_id, "skill:one");
        let activation = test_skill_activation(params.scope);
        Ok(AgentApiOutcome::new(SkillActivateResponse {
            activation: activation.clone(),
            active: vec![activation],
        }))
    }

    async fn deactivate_skill(
        &self,
        params: SkillDeactivateParams,
    ) -> Result<AgentApiOutcome<SkillDeactivateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SkillDeactivateResponse {
            skill_id: params.skill_id,
            active: Vec::new(),
        }))
    }

    async fn list_session_environments(
        &self,
        _params: SessionEnvironmentListParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentListResponse>, AgentApiError> {
        let environment = test_session_environment(true);
        Ok(AgentApiOutcome::new(SessionEnvironmentListResponse {
            active_env_id: Some(environment.env_id.clone()),
            environments: vec![environment],
        }))
    }

    async fn read_session_environment(
        &self,
        params: SessionEnvironmentReadParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentReadResponse>, AgentApiError> {
        assert_eq!(params.env_id, "test");
        Ok(AgentApiOutcome::new(SessionEnvironmentReadResponse {
            environment: test_session_environment(true),
        }))
    }

    async fn create_session_environment(
        &self,
        params: SessionEnvironmentCreateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCreateResponse>, AgentApiError> {
        let mut environment = test_session_environment(params.activate);
        environment.env_id = params.env_id.unwrap_or_else(|| "created".to_owned());
        Ok(AgentApiOutcome::new(SessionEnvironmentCreateResponse {
            active_env_id: params.activate.then(|| environment.env_id.clone()),
            environments: vec![environment.clone()],
            environment,
        }))
    }

    async fn attach_session_environment(
        &self,
        params: SessionEnvironmentAttachParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentAttachResponse>, AgentApiError> {
        let mut environment = test_session_environment(params.activate);
        environment.env_id = params.env_id.unwrap_or_else(|| "attached".to_owned());
        Ok(AgentApiOutcome::new(SessionEnvironmentAttachResponse {
            active_env_id: params.activate.then(|| environment.env_id.clone()),
            environments: vec![environment.clone()],
            environment,
        }))
    }

    async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError> {
        assert_eq!(params.env_id, "test");
        let environment = test_session_environment(true);
        Ok(AgentApiOutcome::new(SessionEnvironmentActivateResponse {
            active_env_id: Some(environment.env_id.clone()),
            environments: vec![environment.clone()],
            environment,
        }))
    }

    async fn deactivate_session_environment(
        &self,
        _params: SessionEnvironmentDeactivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionEnvironmentDeactivateResponse {
            active_env_id: None,
            environments: vec![test_session_environment(false)],
        }))
    }

    async fn close_session_environment(
        &self,
        _params: SessionEnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCloseResponse>, AgentApiError> {
        let mut environment = test_session_environment(false);
        environment.status = SessionEnvironmentStatusView::Detached;
        Ok(AgentApiOutcome::new(SessionEnvironmentCloseResponse {
            active_env_id: None,
            environments: vec![environment.clone()],
            environment,
        }))
    }

    async fn bind_session_environment_credential(
        &self,
        params: SessionEnvironmentCredentialBindParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCredentialBindResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            SessionEnvironmentCredentialBindResponse {
                credential: test_session_environment_credential(
                    params.session_id,
                    params.env_id,
                    params.env_name,
                    params.source,
                ),
            },
        ))
    }

    async fn list_session_environment_credentials(
        &self,
        params: SessionEnvironmentCredentialListParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCredentialListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            SessionEnvironmentCredentialListResponse {
                credentials: vec![test_session_environment_credential(
                    params.session_id,
                    params.env_id,
                    "GITHUB_TOKEN".to_owned(),
                    SessionEnvironmentCredentialSourceView::AuthGrant {
                        grant_id: "authgrant_repo".to_owned(),
                    },
                )],
            },
        ))
    }

    async fn unbind_session_environment_credential(
        &self,
        params: SessionEnvironmentCredentialUnbindParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCredentialUnbindResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            SessionEnvironmentCredentialUnbindResponse {
                credential: test_session_environment_credential(
                    params.session_id,
                    params.env_id,
                    params.env_name,
                    SessionEnvironmentCredentialSourceView::AuthGrant {
                        grant_id: "authgrant_repo".to_owned(),
                    },
                ),
            },
        ))
    }

    async fn create_session_jobs(
        &self,
        params: SessionJobCreateParams,
    ) -> Result<AgentApiOutcome<SessionJobCreateResponse>, AgentApiError> {
        assert_eq!(params.request_id, "request_1");
        Ok(AgentApiOutcome::new(SessionJobCreateResponse {
            env_id: params.env_id.unwrap_or_else(|| "test".to_owned()),
            jobs: vec![SessionJobStartedView {
                name: params.jobs.first().and_then(|job| job.name.clone()),
                job_id: "job-1".to_owned(),
                handle: test_session_job_handle(),
                status: SessionJobStatusView::Queued,
                dependencies: Vec::new(),
                queue_key: None,
            }],
        }))
    }

    async fn list_session_jobs(
        &self,
        params: SessionJobListParams,
    ) -> Result<AgentApiOutcome<SessionJobListResponse>, AgentApiError> {
        assert_eq!(params.session_id, "session_1");
        Ok(AgentApiOutcome::new(SessionJobListResponse {
            jobs: vec![test_session_job_record()],
        }))
    }

    async fn read_session_jobs(
        &self,
        params: SessionJobReadParams,
    ) -> Result<AgentApiOutcome<SessionJobReadResponse>, AgentApiError> {
        assert_eq!(params.jobs.len(), 1);
        Ok(AgentApiOutcome::new(SessionJobReadResponse {
            jobs: vec![SessionJobReadEntryView {
                handle: Some(test_session_job_handle()),
                summary: Some(test_session_job_summary(SessionJobStatusView::Succeeded)),
                output_chunks: vec![SessionJobOutputChunkView {
                    seq: 1,
                    stream: SessionJobOutputStreamView::Stdout,
                    data_base64: "b2sK".to_owned(),
                }],
                output_next_seq: 2,
                artifacts: Vec::new(),
                error: None,
            }],
        }))
    }

    async fn cancel_session_jobs(
        &self,
        params: SessionJobCancelParams,
    ) -> Result<AgentApiOutcome<SessionJobCancelResponse>, AgentApiError> {
        assert_eq!(params.jobs.len(), 1);
        Ok(AgentApiOutcome::new(SessionJobCancelResponse {
            jobs: vec![SessionJobCancelEntryView {
                handle: Some(test_session_job_handle()),
                summary: Some(test_session_job_summary(SessionJobStatusView::Cancelled)),
                error: None,
            }],
        }))
    }

    async fn register_environment_provider(
        &self,
        params: EnvironmentProviderRegisterParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderRegisterResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentProviderRegisterResponse {
            provider: test_environment_provider(
                params.provider_id,
                params.provider_kind,
                EnvironmentProviderStatusView::Online,
            ),
        }))
    }

    async fn heartbeat_environment_provider(
        &self,
        params: EnvironmentProviderHeartbeatParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderHeartbeatResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentProviderHeartbeatResponse {
            provider: test_environment_provider(
                params.provider_id,
                EnvironmentProviderKindView::Bridge,
                EnvironmentProviderStatusView::Online,
            ),
            targets: params.observed_targets,
        }))
    }

    async fn unregister_environment_provider(
        &self,
        params: EnvironmentProviderUnregisterParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderUnregisterResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            EnvironmentProviderUnregisterResponse {
                provider: test_environment_provider(
                    params.provider_id,
                    EnvironmentProviderKindView::Bridge,
                    EnvironmentProviderStatusView::Offline,
                ),
            },
        ))
    }

    async fn list_environment_providers(
        &self,
        _params: EnvironmentProviderListParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(EnvironmentProviderListResponse {
            providers: vec![test_environment_provider(
                "bridge-local".to_owned(),
                EnvironmentProviderKindView::Bridge,
                EnvironmentProviderStatusView::Online,
            )],
        }))
    }

    async fn list_environment_provider_targets(
        &self,
        params: EnvironmentProviderTargetListParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderTargetListResponse>, AgentApiError> {
        assert_eq!(params.provider_id, "bridge-local");
        Ok(AgentApiOutcome::new(
            EnvironmentProviderTargetListResponse {
                targets: vec![test_environment_target()],
            },
        ))
    }

    async fn put_blob(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobPutResponse {
            blob_ref: format!("sha256:{}", "1".repeat(64)),
            bytes: params.bytes_base64.len() as u64,
        }))
    }

    async fn put_blobs(
        &self,
        params: BlobPutManyParams,
    ) -> Result<AgentApiOutcome<BlobPutManyResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobPutManyResponse {
            blobs: params
                .blobs
                .into_iter()
                .enumerate()
                .map(|(index, blob)| BlobPutResponse {
                    blob_ref: format!("sha256:{index:064x}"),
                    bytes: blob.bytes_base64.len() as u64,
                })
                .collect(),
        }))
    }

    async fn get_blob(
        &self,
        params: BlobGetParams,
    ) -> Result<AgentApiOutcome<BlobGetResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobGetResponse {
            blob_ref: params.blob_ref,
            bytes_base64: "aGVsbG8=".to_owned(),
            bytes: 5,
        }))
    }

    async fn has_blobs(
        &self,
        params: BlobHasManyParams,
    ) -> Result<AgentApiOutcome<BlobHasManyResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(BlobHasManyResponse {
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

    async fn put_vfs_mount(
        &self,
        params: VfsMountPutParams,
    ) -> Result<AgentApiOutcome<VfsMountPutResponse>, AgentApiError> {
        let mount = VfsMountView {
            mount_path: params.mount_path,
            source: match params.source {
                VfsMountSourceInput::Snapshot { snapshot_ref } => {
                    VfsMountSourceView::Snapshot { snapshot_ref }
                }
                VfsMountSourceInput::Workspace { workspace_id } => VfsMountSourceView::Workspace {
                    workspace_id,
                    head_snapshot_ref: Some(format!("sha256:{}", "3".repeat(64))),
                    revision: Some(0),
                },
            },
            access: params.access,
        };
        Ok(AgentApiOutcome::new(VfsMountPutResponse {
            mount: mount.clone(),
            session: SessionView {
                vfs_mounts: vec![mount],
                ..test_session(params.session_id, SessionStatus::Idle)
            },
        }))
    }

    async fn delete_vfs_mount(
        &self,
        params: VfsMountDeleteParams,
    ) -> Result<AgentApiOutcome<VfsMountDeleteResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsMountDeleteResponse {
            mount_path: params.mount_path,
            session: test_session(params.session_id, SessionStatus::Idle),
        }))
    }

    async fn list_vfs_mounts(
        &self,
        params: VfsMountListParams,
    ) -> Result<AgentApiOutcome<VfsMountListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(VfsMountListResponse {
            mounts: vec![VfsMountView {
                mount_path: "/workspace".to_owned(),
                source: VfsMountSourceView::Workspace {
                    workspace_id: format!("workspace_{}", params.session_id),
                    head_snapshot_ref: Some(format!("sha256:{}", "3".repeat(64))),
                    revision: Some(0),
                },
                access: VfsMountAccess::ReadWrite,
            }],
        }))
    }

    async fn create_mcp_server(
        &self,
        params: McpServerCreateParams,
    ) -> Result<AgentApiOutcome<McpServerCreateResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(McpServerCreateResponse {
            server: test_mcp_server(params.server_id),
        }))
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

    async fn link_session_mcp(
        &self,
        params: SessionMcpLinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpLinkResponse>, AgentApiError> {
        let link = test_mcp_link(params.tool_id.unwrap_or_else(|| "mcp_echo".to_owned()));
        Ok(AgentApiOutcome::new(SessionMcpLinkResponse {
            link: link.clone(),
            links: vec![link],
            session: test_session(params.session_id, SessionStatus::Idle),
        }))
    }

    async fn unlink_session_mcp(
        &self,
        params: SessionMcpUnlinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpUnlinkResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionMcpUnlinkResponse {
            tool_id: params.tool_id,
            links: Vec::new(),
            session: test_session(params.session_id, SessionStatus::Idle),
        }))
    }

    async fn list_session_mcp(
        &self,
        _params: SessionMcpListParams,
    ) -> Result<AgentApiOutcome<SessionMcpListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(SessionMcpListResponse {
            links: vec![test_mcp_link("mcp_echo".to_owned())],
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
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}

fn test_auth_grant(grant_id: String, status: AuthGrantStatus) -> AuthGrantView {
    AuthGrantView {
        grant_id,
        provider_id: "static".to_owned(),
        provider_kind: AuthProviderKind::StaticBearer,
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
            config: Some(SessionConfigInput {
                tools: Some(ToolConfigInput {
                    fleet: Some(true),
                    ..ToolConfigInput::default()
                }),
                ..SessionConfigInput::default()
            }),
            instructions: Some(ProfileInstructions::Text {
                text: "Be concise.".to_owned(),
            }),
            mounts: Vec::new(),
            mcp: Vec::new(),
            environments: Vec::new(),
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
    SessionView {
        id,
        status,
        cwd: None,
        config_revision: 0,
        config: None,
        created_at_ms: 1,
        updated_at_ms: 2,
        runs: Vec::new(),
        active_context: ContextView::default(),
        active_tools: ActiveToolsView::default(),
        vfs_mounts: Vec::new(),
    }
}

fn test_session_environment(active: bool) -> SessionEnvironmentView {
    SessionEnvironmentView {
        env_id: "test".to_owned(),
        kind: SessionEnvironmentKindView::AttachedHost,
        status: SessionEnvironmentStatusView::Ready,
        capabilities: SessionEnvironmentCapabilitiesView {
            fs_read: true,
            fs_write: true,
            process_exec: true,
            process_stdin: true,
            network: false,
            persistent: false,
            ..SessionEnvironmentCapabilitiesView::default()
        },
        exec_target: Some(ToolExecutionTargetView {
            namespace: "env".to_owned(),
            id: "test".to_owned(),
        }),
        cwd: Some("/workspace".to_owned()),
        active,
    }
}

fn test_session_environment_credential(
    session_id: SessionId,
    env_id: EnvironmentId,
    env_name: String,
    source: SessionEnvironmentCredentialSourceView,
) -> SessionEnvironmentCredentialView {
    SessionEnvironmentCredentialView {
        session_id,
        env_id,
        env_name,
        source,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

fn test_session_job_handle() -> SessionJobHandleView {
    SessionJobHandleView {
        session_id: "session_1".to_owned(),
        env_id: "test".to_owned(),
        job_id: "job-1".to_owned(),
    }
}

fn test_session_job_record() -> SessionJobHandleRecordView {
    SessionJobHandleRecordView {
        handle: test_session_job_handle(),
        provider_id: "bridge-local".to_owned(),
        target_id: "local".to_owned(),
        namespace: "session_1".to_owned(),
        name: Some("build".to_owned()),
        queue_key: None,
        created_by_run_id: Some("run_1".to_owned()),
        created_by_turn_id: Some(1),
        created_by_tool_call_id: Some("call_1".to_owned()),
        created_at_ms: 123,
        start_request_hash: format!("sha256:{}", "1".repeat(64)),
    }
}

fn test_session_job_summary(status: SessionJobStatusView) -> SessionJobSummaryView {
    SessionJobSummaryView {
        namespace: "session_1".to_owned(),
        job_id: "job-1".to_owned(),
        name: Some("build".to_owned()),
        status,
        dependencies: Vec::new(),
        created_at_ms: 123,
        queued_at_ms: Some(124),
        started_at_ms: Some(125),
        finished_at_ms: Some(126),
        exit_code: Some(0),
        failure: None,
        queue_key: None,
    }
}

fn test_environment_provider(
    provider_id: EnvironmentProviderId,
    provider_kind: EnvironmentProviderKindView,
    status: EnvironmentProviderStatusView,
) -> EnvironmentProviderView {
    EnvironmentProviderView {
        provider_id,
        provider_kind,
        status,
        controller_connection: HostControllerConnectionView {
            endpoint: "ws://127.0.0.1:9000/controller".to_owned(),
            transport: HostTransportView::WebSocket,
        },
        capabilities: EnvironmentProviderCapabilitiesView {
            list_targets: true,
            attach_target: true,
            get_target: true,
            ..EnvironmentProviderCapabilitiesView::default()
        },
        implementation: EnvironmentProviderImplementationView {
            name: "test-bridge".to_owned(),
            version: Some("1.0.0".to_owned()),
        },
        last_seen_ms: 10,
        lease_expires_ms: 30_010,
        display_name: Some("Local bridge".to_owned()),
        metadata: BTreeMap::new(),
    }
}

fn test_environment_target() -> EnvironmentTargetSummaryView {
    EnvironmentTargetSummaryView {
        target_id: "local".to_owned(),
        status: EnvironmentTargetStatusView::Ready,
        scope: HostScopeView::Default,
        capabilities: HostCapabilitiesView {
            filesystem_read: true,
            filesystem_write: true,
            process_start: true,
            process_stdin: true,
            process_terminate: true,
            process_output_polling: true,
            process_output_notifications: false,
            process_pty: true,
            job_start: true,
            job_list: true,
            job_read: true,
            job_cancel: true,
            job_wait_hint: false,
            job_dependencies: true,
            job_queue_keys: true,
        },
        display_name: Some("Local".to_owned()),
        default_cwd: Some("/workspace".to_owned()),
        metadata: BTreeMap::new(),
    }
}

fn test_run(id: RunId, status: RunStatus) -> RunView {
    RunView {
        id,
        status,
        source: RunViewSource::Input { items: Vec::new() },
        items: Vec::new(),
        tool_batches: Vec::new(),
    }
}

fn test_skill_activation(scope: SkillActivationScope) -> SkillActivationView {
    SkillActivationView {
        skill_id: "skill:one".to_owned(),
        name: Some("one".to_owned()),
        description: Some("Use when testing skills.".to_owned()),
        short_description: Some("test skill".to_owned()),
        catalog_ref: format!("sha256:{}", "5".repeat(64)),
        scope,
        source: SkillActivationSource::DirectContext {
            context_ref: format!("sha256:{}", "6".repeat(64)),
        },
    }
}

fn test_mcp_server(server_id: String) -> McpServerView {
    McpServerView {
        default_server_label: server_id.clone(),
        server_url: format!("https://{server_id}.example.com/mcp"),
        server_id,
        display_name: None,
        transport: RemoteMcpTransport::Auto,
        description: None,
        allowed_tools: None,
        approval_default: RemoteMcpApprovalPolicy::ProviderDefault,
        defer_loading_default: None,
        auth_policy: McpServerAuthPolicy::None,
        status: McpServerStatus::Active,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

fn test_mcp_link(tool_id: String) -> SessionMcpLinkView {
    SessionMcpLinkView {
        tool_id,
        server_label: "echo".to_owned(),
        server_url: "https://echo.example.com/mcp".to_owned(),
        allowed_tools: None,
        approval: RemoteMcpApprovalPolicy::ProviderDefault,
        defer_loading: None,
        auth_ref: None,
    }
}
