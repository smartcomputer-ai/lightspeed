//! Drive the real command tree against a stateful, isolated runtime fixture.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

struct Runtime {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    directory: tempfile::TempDir,
}
impl Runtime {
    fn start(mut handler: impl FnMut(&str, &Value) -> Value + Send + 'static) -> Self {
        Self::start_api(move |method, params| Ok(handler(method, params)))
    }
    fn start_api(
        mut handler: impl FnMut(&str, &Value) -> Result<Value, Value> + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local fixture must bind");
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/rpc", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 8192];
                let request: Value = loop {
                    let count = socket.read(&mut buf).unwrap();
                    assert!(count > 0, "request ended before its body");
                    bytes.extend_from_slice(&buf[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let size: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap();
                        }
                    }
                };
                recorded.lock().unwrap().push(request.clone());
                let method = request["method"].as_str().unwrap();
                if method == api::METHOD_SESSION_READ {
                    let params: api::SessionReadParams =
                        serde_json::from_value(request["params"].clone()).unwrap();
                    assert_ne!(params.run_limit, Some(0), "runLimit must be positive");
                }
                let result = if method == "initialize" {
                    json!({"protocolVersion":"test", "serverInfo":{"name":"fixture","version":"test","gitSha":"test","envd":{"version":"test","gitSha":"test","protocolVersion":1,"targets":[]}}, "capabilities":{"notifications":false,"historyRead":true,"eventLog":true,"localExecution":false}, "caller":{"scope":{"kind":"deployment"},"single":false,"keyPrefix":"lsk_fixture","groups":api::MethodGroup::ALL}})
                } else {
                    Value::Null
                };
                let outcome = if method == "initialize" {
                    Ok(result)
                } else {
                    handler(method, &request["params"])
                };
                let response = match outcome {
                    Ok(result) => {
                        json!({"id":request["id"],"result":{"result":result,"notifications":[]}})
                    }
                    Err(error) => json!({"id":request["id"],"error":error}),
                }
                .to_string();
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
            directory: tempfile::tempdir().unwrap(),
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lightspeed"))
            .current_dir(self.directory.path())
            .args(args)
            .env(
                "LIGHTSPEED_CONFIG_DIR",
                self.directory.path().join("config"),
            )
            .env("LIGHTSPEED_API_URL", &self.endpoint)
            .env("LIGHTSPEED_API_KEY", "lsk_fixture")
            .env(
                "LIGHTSPEED_UNIVERSE",
                "00000000-0000-0000-0000-000000000001",
            )
            .output()
            .unwrap()
    }
    fn success(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.success(args)).expect("stdout must be one JSON document")
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
fn session(config: Value) -> Value {
    json!({"id":"s1","status":"idle","activity":"idle","retention":{"rootSessionId":"s1"},"managed":false,"configRevision":7,"contextRevision":0,"createdAtMs":1,"updatedAtMs":1,"access":{"visibility":"restricted"},"activeContext":{"revision":0},"config":config})
}

fn session_summary(id: &str) -> Value {
    json!({"id":id,"displayName":"Review changes","lifecycleStatus":"open","activity":"idle","retention":{"rootSessionId":id},"managed":false,"access":{"visibility":"restricted"},"createdAtMs":1,"updatedAtMs":1})
}

#[test]
fn chat_list_requests_only_ten_unmanaged_roots_and_shows_status_without_starting_chat() {
    let runtime = Runtime::start(|method, params| {
        assert_eq!(method, "session/list");
        assert_eq!(params["limit"], 10);
        assert_eq!(params["managed"], false);
        assert_eq!(params["subagent"], false);
        assert!(params.get("closed").is_none());
        assert!(params.get("cursor").is_none());
        let mut sessions: Vec<_> = (0..10)
            .map(|i| session_summary(&format!("chat-{i}")))
            .collect();
        sessions[0]["lifecycleStatus"] = json!("closed");
        sessions[1]["activity"] = json!("working");
        sessions[2]["activity"] = json!("waiting");
        json!({"sessions":sessions,"nextCursor":"more-sessions"})
    });
    let text = runtime.success(&["chat", "--list"]);
    for expected in [
        "SESSION",
        "STATUS",
        "ACTIVITY",
        "UPDATED",
        "closed",
        "working",
        "waiting",
        "Review changes",
        "chat-9",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let rows = runtime.json(&["chat", "--list", "--json"]);
    assert_eq!(rows.as_array().unwrap().len(), 10);
    assert_eq!(rows[0]["id"], "chat-0");
    assert_eq!(rows[9]["id"], "chat-9");
}

#[test]
fn chat_resume_aliases_select_the_latest_open_unmanaged_root_without_starting_a_session() {
    let runtime = Runtime::start(|method, params| match method {
        "session/list" => {
            assert_eq!(params["limit"], 1);
            assert_eq!(params["managed"], false);
            assert_eq!(params["subagent"], false);
            assert_eq!(params["closed"], false);
            json!({"sessions":[session_summary("s1")]})
        }
        "session/read" => {
            assert_eq!(params["sessionId"], "s1");
            json!({"session":session(json!({"model":{"providerId":"fixture","apiKind":"openai:responses","model":"fixture"}})),"hasOlderRuns":false})
        }
        "session/events/read" => {
            assert_eq!(params["sessionId"], "s1");
            json!({"events":[],"complete":true})
        }
        _ => panic!("resume must not create or mutate sessions: {method}"),
    });
    for flag in ["--resume", "--continue"] {
        assert_eq!(runtime.json(&["chat", flag, "--json"]), json!([]));
    }
}

#[test]
fn chat_resume_reports_empty_closed_or_deleted_sessions_without_creating_replacements() {
    for state in ["empty", "closed", "deleted"] {
        let runtime = Runtime::start_api(move |method, _| match method {
            "session/list" => Ok(
                json!({"sessions":if state == "empty" {vec![]} else {vec![session_summary("s1")]}}),
            ),
            "session/read" if state == "closed" => {
                let mut session = session(json!({}));
                session["status"] = json!("closed");
                Ok(json!({"session":session,"hasOlderRuns":false}))
            }
            "session/read" if state == "deleted" => Err(
                json!({"code":-32000,"message":"session deleted","data":{"kind":"not_found","message":"session deleted"}}),
            ),
            _ => panic!("resume must not create a replacement: {method}"),
        });
        let output = runtime.run(&["chat", "--resume", "--json"]);
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(match state {
                "empty" => "No resumable",
                "closed" => "closed before",
                _ => "session deleted",
            }),
            "{error}"
        );
        if state == "empty" {
            assert!(
                runtime
                    .success(&["chat", "--list"])
                    .contains("No unmanaged chat sessions")
            );
            assert_eq!(runtime.json(&["chat", "--list", "--json"]), json!([]));
        }
    }
}

#[test]
fn session_read_accepts_a_name_and_renders_human_and_json_output() {
    let runtime = Runtime::start(|method, params| {
        assert_eq!(method, api::METHOD_SESSION_READ);
        assert_eq!(params["sessionId"], "test1");
        json!({"session":session(json!({})),"hasOlderRuns":false})
    });
    let output = runtime.success(&["session", "read", "test1"]);
    assert!(output.contains("s1"));
    assert!(output.contains("idle"));
    let output = runtime.json(&["session", "read", "test1", "--json"]);
    assert_eq!(output["id"], "s1");
    assert_eq!(output["status"], "idle");
}

#[test]
fn chat_upload_and_workspace_flags_preserve_other_attachments_and_plain_resume() {
    let original = json!({"path":"/existing","workspaceId":"existing","access":"read"});
    let state = Arc::new(Mutex::new(session(json!({
        "model":{"providerId":"fixture","apiKind":"openai:responses","model":"fixture"},
        "limits":{"maxTurns":7},
        "features":{"vfs":{"version":api::CURRENT_FEATURE_VERSION,"workspaces":[original]}}
    }))));
    let stored = state.clone();
    let runtime = Runtime::start_api(move |method, params| {
        Ok(match method {
            "session/start" => {
                return Err(
                    json!({"code":-32000,"message":"exists","data":{"kind":"conflict","message":"exists"}}),
                );
            }
            "session/read" => {
                json!({"session":stored.lock().unwrap().clone(),"hasOlderRuns":false})
            }
            "session/events/read" => json!({"events":[],"complete":true}),
            "session/config/put" => {
                assert_eq!(params["expectedConfigRevision"], 7);
                let mut current = stored.lock().unwrap();
                current["config"] = params["config"].clone();
                json!({"session":current.clone()})
            }
            "vfs/snapshots/commit" => json!({"snapshotRef":"sha256:fixture","files":0,"bytes":0}),
            "vfs/workspaces/create" => {
                assert_eq!(params["snapshotRef"], "sha256:fixture");
                json!({"workspace":{"workspaceId":"uploaded","headSnapshotRef":"sha256:fixture","files":0,"bytes":0,"revision":0,"createdAtMs":1,"updatedAtMs":1}})
            }
            _ => panic!("unexpected method: {method}"),
        })
    });
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        runtime.json(&[
            "chat",
            "-s",
            "s1",
            "--json",
            "--upload",
            directory.path().to_str().unwrap()
        ]),
        json!([])
    );
    {
        let current = state.lock().unwrap();
        assert_eq!(current["config"]["limits"]["maxTurns"], 7);
        assert_eq!(
            current["config"]["features"]["vfs"]["workspaces"],
            json!([
                original,
                {"path":"/workspace","workspaceId":"uploaded","access":"edit"}
            ])
        );
    }
    runtime.json(&[
        "chat",
        "-s",
        "s1",
        "--json",
        "--workspace",
        "reused",
        "--workspace-path",
        "/workspace",
        "--workspace-access",
        "read",
    ]);
    let before_resume = state.lock().unwrap().clone();
    assert_eq!(
        before_resume["config"]["features"]["vfs"]["workspaces"],
        json!([
            original,
            {"path":"/workspace","workspaceId":"reused","access":"read"}
        ])
    );
    runtime.requests.lock().unwrap().clear();
    runtime.json(&["chat", "-s", "s1", "--json"]);
    assert_eq!(*state.lock().unwrap(), before_resume);
    assert!(
        !runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(
                request["method"].as_str(),
                Some("session/config/put" | "vfs/workspaces/create" | "vfs/snapshots/commit")
            ))
    );
}

#[test]
fn attachments_preserve_configuration_and_list_declarations_without_materialized_tools() {
    let state = Arc::new(Mutex::new(session(json!({"limits":{"maxTurns":7}}))));
    let stored = state.clone();
    let runtime = Runtime::start(move |method, params| match method {
        "session/read" => json!({"session":stored.lock().unwrap().clone(),"hasOlderRuns":false}),
        "session/config/put" => {
            assert_eq!(params["expectedConfigRevision"], 7);
            let mut session = stored.lock().unwrap();
            session["config"] = params["config"].clone();
            json!({"session":session.clone()})
        }
        _ => panic!("unexpected method: {method}"),
    });
    runtime.success(&[
        "session",
        "workspace",
        "attach",
        "--session",
        "s1",
        "--workspace",
        "w1",
        "--path",
        "/repo",
        "--access",
        "read",
    ]);
    runtime.success(&[
        "session",
        "mcp",
        "attach",
        "m1",
        "--session",
        "s1",
        "--tools",
        "search",
    ]);
    let declared = runtime.json(&["sessions", "mcps", "list", "--session", "s1", "--json"]);
    assert_eq!(declared, json!([{"serverId":"m1","tools":["search"]}]));
    runtime.success(&[
        "session",
        "environment",
        "attach",
        "e1",
        "--session",
        "s1",
        "--access",
        "jobs",
        "--working-directory",
        "/repo",
    ]);
    let config = state.lock().unwrap()["config"].clone();
    assert_eq!(config["limits"]["maxTurns"], 7);
    assert_eq!(config["features"]["vfs"]["workspaces"][0]["access"], "read");
    assert_eq!(
        config["features"]["environments"]["environments"][0]["access"],
        "jobs"
    );
    assert_eq!(
        runtime.json(&["session", "config", "read", "s1", "--json"]),
        config
    );
    runtime.success(&["session", "mcp", "detach", "m1", "--session", "s1"]);
    assert_eq!(
        runtime.json(&["session", "mcp", "list", "--session", "s1", "--json"]),
        json!([])
    );
    assert!(
        !runtime
            .run(&["session", "mcp", "detach", "m1", "--session", "s1"])
            .status
            .success()
    );
    runtime.success(&["session", "environment", "detach", "e1", "--session", "s1"]);
    assert_eq!(
        runtime.json(&[
            "session",
            "environment",
            "list",
            "--session",
            "s1",
            "--json"
        ]),
        json!([])
    );
    assert_eq!(state.lock().unwrap()["config"]["limits"]["maxTurns"], 7);
}

fn provider(id: &str, kind: &str, config: Value) -> Value {
    json!({"providerId":id,"providerKind":kind,"config":config,"hasCredential":true,"status":"active","createdAtMs":0,"updatedAtMs":0})
}
#[test]
fn github_commands_filter_providers_and_refuse_to_delete_model_connections() {
    let github = provider(
        "github",
        "gitHubApp",
        json!({"type":"githubApp","appId":"1","apiBaseUrl":"https://api.github.com"}),
    );
    let model = provider("model:openai", "modelApiKey", json!({"type":"modelApiKey"}));
    let runtime = Runtime::start(move |method, _| match method {
        "auth/providers/list" => json!({"providers":[github,model]}),
        "auth/providers/read" => json!({"provider":model}),
        _ => panic!("unexpected request: {method}"),
    });
    let listed = runtime.json(&["credentials", "github", "app", "list", "--json"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["providerId"], "github");
    assert!(
        !runtime
            .run(&["credential", "github", "app", "delete", "model:openai"])
            .status
            .success()
    );
    assert!(
        !runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "auth/providers/delete")
    );
    assert_eq!(
        runtime.json(&["models", "providers", "list", "--json"])[0]["providerId"],
        "model:openai"
    );
}

#[test]
fn oauth_no_wait_and_empty_lists_have_useful_output_and_clean_json() {
    let runtime = Runtime::start(|method, _| match method {
        "auth/flows/start" => {
            json!({"flowId":"flow1","authorizeUrl":"https://auth.example/authorize","expiresAtMs":9999999999999_i64})
        }
        "vfs/workspaces/list" => json!({"workspaces":[]}),
        "mcp/servers/list" => json!({"servers":[]}),
        "profiles/list" => json!({"profiles":[]}),
        _ => panic!("unexpected request: {method}"),
    });
    assert_eq!(
        runtime.json(&["credential", "login", "client1", "--no-wait", "--json"])["flowId"],
        "flow1"
    );
    assert!(
        runtime
            .success(&["workspaces", "list"])
            .contains("No workspaces")
    );
    assert!(
        runtime
            .success(&["mcps", "list"])
            .contains("No MCP servers")
    );
    assert!(
        runtime
            .success(&["profiles", "list"])
            .contains("No profiles")
    );
}

#[test]
fn provisioning_and_template_commands_send_current_typed_requests() {
    let runtime = Runtime::start(|method, params| match method {
        "environments/templates/list" => {
            assert_eq!(params["bindingId"], "local");
            json!({"templates":[{"templateId":"linux-v1","providerId":"incus","bindingId":"local","displayName":"Linux","publicIngress":false,"deprecated":false}]})
        }
        "environments/create" | "environments/external/create" => {
            let source = if method == "environments/create" {
                assert_eq!(params["templateId"], "linux-v1");
                assert_eq!(params["bindingId"], "local");
                assert_eq!(params["metadata"], json!({"project":"audit"}));
                json!({"type":"provisioned","providerId":"incus","bindingId":"local"})
            } else {
                assert_eq!(
                    params["connection"],
                    json!({"endpoint":"wss://env.example/ws","transport":"webSocket"})
                );
                json!({"type":"external","connection":params["connection"]})
            };
            assert_eq!(params["requestId"], "retry-1");
            json!({"environment":{"environmentId":"env1","requestId":params["requestId"],"source":source,"status":"provisioning","desiredPower":"running","incarnation":{"incarnationId":"i1","createdAtMs":0,"updatedAtMs":0},"publicIngressEnabled":false,"createdAtMs":0,"updatedAtMs":0}})
        }
        "deployment/environment-providers/list" => json!({"providers":[]}),
        "environments/provider-bindings/list" => json!({"bindings":[]}),
        _ => panic!("unexpected request {method}"),
    });
    let templates = runtime.json(&[
        "environments",
        "templates",
        "list",
        "--binding",
        "local",
        "--json",
    ]);
    assert_eq!(templates["templates"][0]["templateId"], "linux-v1");
    let created = runtime.json(&[
        "environment",
        "create",
        "--binding",
        "local",
        "--template",
        "linux-v1",
        "--request-id",
        "retry-1",
        "--metadata",
        "project=audit",
        "--json",
    ]);
    assert_eq!(created["status"], "provisioning");
    assert_eq!(
        runtime.json(&[
            "environment",
            "register",
            "wss://env.example/ws",
            "--request-id",
            "retry-1",
            "--json"
        ])["environmentId"],
        "env1"
    );
    assert!(
        runtime
            .success(&["environment", "provider", "list"])
            .contains("No environment provider controllers")
    );
    assert!(
        runtime
            .success(&["environment", "binding", "list"])
            .contains("No environment provider bindings")
    );
}

#[test]
fn config_replacement_checks_the_requested_revision_and_outputs_one_document() {
    let runtime = Runtime::start(|method, params| match method {
        "session/read" => {
            json!({"session":session(json!({"limits":{"maxTurns":3}})),"hasOlderRuns":false})
        }
        "session/config/put" => {
            assert_eq!(params["config"], json!({"limits":{"maxTurns":11}}));
            assert_eq!(params["expectedConfigRevision"], 42);
            json!({"session":{"id":"s1","status":"idle","configRevision":43,"contextRevision":9}})
        }
        _ => panic!("unexpected request {method}"),
    });
    let file = runtime.directory.path().join("config.json");
    std::fs::write(&file, r#"{"limits":{"maxTurns":11}}"#).unwrap();
    let output = runtime.json(&[
        "session",
        "config",
        "put",
        "s1",
        "--file",
        file.to_str().unwrap(),
        "--expected-revision",
        "42",
        "--json",
    ]);
    assert_eq!(output["session"]["configRevision"], 43);
}

#[test]
fn model_defaults_commands_round_trip_routes_clear_explicitly_and_guard_revisions() {
    let mut defaults = json!({"revision":0, "agentRun":null, "speechToText":null});
    let runtime = Runtime::start(move |method, params| {
        match method {
            "models/defaults/read" => {}
            "models/defaults/put" => {
                assert_eq!(params["expectedRevision"], defaults["revision"]);
                let slot = params["slot"].as_str().unwrap();
                assert!(
                    params.get("model").is_some(),
                    "clearing must send explicit null"
                );
                defaults[slot] = params["model"].clone();
                defaults["revision"] = json!(defaults["revision"].as_u64().unwrap() + 1);
            }
            other => panic!("unexpected method {other}"),
        }
        json!({"defaults":defaults})
    });
    let agent = runtime.json(&[
        "model",
        "defaults",
        "set",
        "agent-run",
        "--provider",
        "anthropic",
        "--api-kind",
        "anthropic:messages",
        "--model",
        "chosen",
        "--json",
    ]);
    assert_eq!(agent["agentRun"]["apiKind"], "anthropic:messages");
    assert_eq!(agent["revision"], 1);
    let speech = runtime.json(&[
        "model",
        "defaults",
        "set",
        "speech-to-text",
        "--provider",
        "custom-speech",
        "--api-kind",
        "openai:audio-transcriptions",
        "--model",
        "transcriber",
        "--expected-revision",
        "1",
        "--json",
    ]);
    assert_eq!(speech["agentRun"], agent["agentRun"]);
    let cleared = runtime.json(&["model", "defaults", "clear", "agent-run", "--json"]);
    assert_eq!(cleared["agentRun"], Value::Null);
    assert_eq!(cleared["speechToText"], speech["speechToText"]);
    assert_eq!(
        runtime.json(&["model", "defaults", "read", "--json"]),
        cleared
    );
    let requests = runtime.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "models/defaults/read")
            .count(),
        3
    );
}

#[test]
fn model_defaults_conflict_is_not_retried_with_a_new_revision() {
    let runtime = Runtime::start_api(|method, _params| {
        assert_eq!(method, "models/defaults/put");
        Err(
            json!({"code":-32009, "message":"defaults changed", "data":{"kind":"conflict","message":"defaults changed"}}),
        )
    });
    let output = runtime.run(&[
        "model",
        "defaults",
        "clear",
        "agent-run",
        "--expected-revision",
        "4",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("defaults changed"));
    assert_eq!(
        runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "models/defaults/put")
            .count(),
        1
    );
}
