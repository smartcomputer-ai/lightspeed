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
    json!({"id":"s1","lifecycleStatus":"open","activity":"idle","retention":{"rootSessionId":"s1"},"lifecycleControlled":false,"configRevision":7,"contextRevision":0,"createdAtMs":1,"updatedAtMs":1,"access":{"visibility":"restricted"},"activeContext":{"revision":0},"config":config})
}

fn session_summary(id: &str) -> Value {
    json!({"id":id,"displayName":"Review changes","lifecycleStatus":"open","activity":"idle","retention":{"rootSessionId":id},"lifecycleControlled":false,"access":{"visibility":"restricted"},"createdAtMs":1,"updatedAtMs":1})
}

#[test]
fn session_delete_is_permanent_unless_soft_delete_is_requested() {
    for soft_delete in [false, true] {
        let runtime = Runtime::start(move |method, params| {
            assert_eq!(method, "session/delete");
            assert_eq!(params["sessionId"], "s1");
            assert_eq!(params["cascade"], true);
            assert_eq!(
                params
                    .get("softDelete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                soft_delete
            );
            let mut summary = session_summary("s1");
            summary["lifecycleStatus"] = json!("closed");
            json!({"session": summary, "deletedSessionCount": 1})
        });
        let mut args = vec!["session", "delete", "s1", "--cascade"];
        if soft_delete {
            args.push("--soft-delete");
        }
        runtime.success(&args);
    }
}

#[test]
fn chat_list_requests_only_ten_uncontrolled_roots_and_shows_status_without_starting_chat() {
    let runtime = Runtime::start(|method, params| {
        assert_eq!(method, "session/list");
        assert_eq!(params["limit"], 10);
        assert_eq!(params["lifecycleControlled"], false);
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
fn chat_resume_aliases_select_the_latest_open_uncontrolled_root_without_starting_a_session() {
    let runtime = Runtime::start(|method, params| match method {
        "session/list" => {
            assert_eq!(params["limit"], 1);
            assert_eq!(params["lifecycleControlled"], false);
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
                session["lifecycleStatus"] = json!("closed");
                Ok(json!({"session":session,"hasOlderRuns":false}))
            }
            "session/read" if state == "deleted" => Err(
                json!({"code":-32000,"message":"session deleted","data":{"kind":"notFound","message":"session deleted"}}),
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
                    .contains("No chat sessions")
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
    assert_eq!(output["lifecycleStatus"], "open");
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
            assert_eq!(params["providerId"], "incus");
            json!({"templates":[{"templateId":"linux-v1","providerId":"incus","displayName":"Linux","powerStates":["running","paused"],"publicIngress":false,"deprecated":false}]})
        }
        "environments/create" | "environments/external/create" => {
            let (source, state, status) = if method == "environments/create" {
                assert_eq!(params["templateId"], "linux-v1");
                assert_eq!(params["providerId"], "incus");
                assert_eq!(params["metadata"], json!({"project":"audit"}));
                (
                    json!({"type":"provisioned","providerId":"incus","templateId":"linux-v1"}),
                    "provisioning",
                    "provisioning",
                )
            } else {
                assert_eq!(params["endpoint"], "wss://env.example/ws");
                assert!(params.get("connection").is_none(), "{params}");
                (
                    json!({"type":"external","endpoint":params["endpoint"]}),
                    "open",
                    "ready",
                )
            };
            assert_eq!(params["requestId"], "retry-1");
            json!({"environment":{"environmentId":"env1","requestId":params["requestId"],"source":source,"state":state,"status":status,"stateChangedAtMs":0,"createdAtMs":0,"updatedAtMs":0}})
        }
        "environments/list" => {
            assert_eq!(params["includeClosed"], true);
            assert_eq!(params["after"], "env0");
            json!({"environments":[{"environmentId":"env1","source":{"type":"external","endpoint":"wss://env.example/ws"},"state":"closed","status":"closed","closeReason":"requested","stateChangedAtMs":0,"closedAtMs":0,"createdAtMs":0,"updatedAtMs":0}],"nextAfter":"env1"})
        }
        "environments/update" => {
            assert_eq!(params["environmentId"], "env1");
            assert_eq!(params["displayName"], "Build box");
            assert_eq!(params["metadata"], json!({"team":"platform"}));
            json!({"environment":{"environmentId":"env1","displayName":"Build box","metadata":{"team":"platform"},"source":{"type":"external","endpoint":"wss://env.example/ws"},"state":"open","status":"ready","stateChangedAtMs":0,"createdAtMs":0,"updatedAtMs":1}})
        }
        "deployment/environment-providers/list" => json!({"providers":[]}),
        "environments/provider-bindings/list" => json!({"bindings":[]}),
        _ => panic!("unexpected request {method}"),
    });
    let templates = runtime.json(&[
        "environments",
        "templates",
        "list",
        "--provider",
        "incus",
        "--json",
    ]);
    assert_eq!(templates["templates"][0]["templateId"], "linux-v1");
    let created = runtime.json(&[
        "environment",
        "create",
        "--provider",
        "incus",
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
    let page = runtime.success(&["environment", "list", "--include-closed", "--after", "env0"]);
    assert!(page.contains("STATE"), "{page}");
    assert!(page.contains("--after env1"), "{page}");
    let updated = runtime.json(&[
        "environment",
        "update",
        "env1",
        "--display-name",
        "Build box",
        "--metadata",
        "team=platform",
        "--json",
    ]);
    assert_eq!(updated["displayName"], "Build box");
}

#[test]
fn a_fixed_machine_is_declared_released_and_given_a_chosen_id() {
    let runtime = Runtime::start(|method, params| {
        let waiting = |daemon: Option<&str>| {
            let mut source = json!({"type":"registered","registrationKeyId":"runners","name":"lightspeed-runner-0","identityMode":"persistent","connected":false,"connectionChangedAtMs":1,"lastSeenAtMs":1});
            if let Some(daemon) = daemon {
                source["daemonId"] = json!(daemon);
            }
            json!({"environmentId":"build-box","displayName":"Build box","source":source,"state":"open","status":"away","stateChangedAtMs":1,"createdAtMs":1,"updatedAtMs":1})
        };
        match method {
            "environments/registration-keys/create" => {
                assert_eq!(
                    *params,
                    json!({
                        "registrationKeyId": "runners",
                        "displayName": "Runners",
                        "identityMode": "persistent",
                        "declaredOnly": true,
                    })
                );
                json!({"registrationKey":{"registrationKeyId":"runners","displayName":"Runners","keyPrefix":"lsrk_fixture","identityMode":"persistent","ephemeralDisconnectGraceMs":300000,"declaredOnly":true,"status":"active","registeredEnvironmentCount":0,"activeEnvironmentCount":0,"createdAtMs":1,"updatedAtMs":1},"secret":"lsrk_fixture-secret"})
            }
            "environments/registered/put" => {
                assert_eq!(
                    *params,
                    json!({
                        "environmentId": "build-box",
                        "registrationKeyId": "runners",
                        "name": "lightspeed-runner-0",
                        "displayName": "Build box",
                        "metadata": {"team": "build"},
                    })
                );
                json!({"environment": waiting(None), "created": true})
            }
            "environments/registered/release" => {
                assert_eq!(*params, json!({"environmentId": "build-box"}));
                json!({"environment": waiting(None), "released": true})
            }
            "environments/external/create" => {
                assert_eq!(params["environmentId"], "ext-box");
                json!({"environment":{"environmentId":"ext-box","requestId":params["requestId"],"source":{"type":"external","endpoint":params["endpoint"]},"state":"open","status":"ready","stateChangedAtMs":0,"createdAtMs":0,"updatedAtMs":0}})
            }
            "environments/create" => {
                assert_eq!(params["environmentId"], "sandbox-1");
                json!({"environment":{"environmentId":"sandbox-1","requestId":params["requestId"],"source":{"type":"provisioned","providerId":"incus","templateId":"linux-v1"},"state":"provisioning","status":"provisioning","stateChangedAtMs":0,"createdAtMs":0,"updatedAtMs":0}})
            }
            _ => panic!("unexpected request {method}"),
        }
    });

    let key = runtime.json(&[
        "environment",
        "registration-key",
        "create",
        "--id",
        "runners",
        "--display-name",
        "Runners",
        "--declared-only",
        "--json",
    ]);
    assert_eq!(key["registrationKey"]["declaredOnly"], true);

    let declared = runtime.success(&[
        "environment",
        "declare",
        "build-box",
        "--key",
        "runners",
        "--registration-name",
        "lightspeed-runner-0",
        "--display-name",
        "Build box",
        "--metadata",
        "team=build",
    ]);
    assert!(declared.contains("build-box"), "{declared}");
    assert!(
        declared.contains("It is away until its daemon registers."),
        "{declared}"
    );
    // A declaration names its key and the daemon's registration name.
    assert!(
        !runtime
            .run(&["environment", "declare", "build-box"])
            .status
            .success()
    );

    let released = runtime.json(&["environment", "release", "build-box", "--json"]);
    assert_eq!(released["released"], true);
    assert!(released["environment"]["source"].get("daemonId").is_none());

    assert_eq!(
        runtime.json(&[
            "environment",
            "register",
            "wss://env.example/ws",
            "--id",
            "ext-box",
            "--request-id",
            "retry-1",
            "--json"
        ])["environmentId"],
        "ext-box"
    );
    assert_eq!(
        runtime.json(&[
            "environment",
            "create",
            "--id",
            "sandbox-1",
            "--provider",
            "incus",
            "--template",
            "linux-v1",
            "--request-id",
            "retry-1",
            "--json",
        ])["environmentId"],
        "sandbox-1"
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
            json!({"session":{"id":"s1","lifecycleStatus":"open","activity":"idle","configRevision":43,"contextRevision":9}})
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
                // A revision is sent only when the caller gave one; otherwise
                // the write is unconditional.
                if params["slot"] == "speechToText" {
                    assert_eq!(params["expectedRevision"], defaults["revision"]);
                } else {
                    assert!(params.get("expectedRevision").is_none());
                }
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
    // Writing never reads first: only the explicit read did.
    let requests = runtime.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "models/defaults/read")
            .count(),
        1
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

#[test]
fn a_generated_secret_declares_an_api_key_and_a_registration_key() {
    // Generation contacts nothing: it works with no connection configured.
    let offline = tempfile::tempdir().unwrap();
    let generate = |kind: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_lightspeed"))
            .current_dir(offline.path())
            .args(kind)
            .env("LIGHTSPEED_CONFIG_DIR", offline.path().join("config"))
            .env_remove("LIGHTSPEED_API_URL")
            .env_remove("LIGHTSPEED_API_KEY")
            .env_remove("LIGHTSPEED_UNIVERSE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let api_secret = generate(&["api-key", "generate-secret"]);
    let registration_secret = generate(&["environment", "registration-key", "generate-secret"]);
    assert!(api_secret.starts_with("lsk_") && api_secret.len() == 4 + 43);
    assert!(registration_secret.starts_with("lsrk_") && registration_secret.len() == 5 + 43);

    let (expected_api, expected_registration) = (api_secret.clone(), registration_secret.clone());
    let runtime = Runtime::start(move |method, params| match method {
        "deployment/api-keys/put" => {
            assert_eq!(
                *params,
                json!({
                    "scope": {"kind":"universe","universeId":"00000000-0000-0000-0000-000000000001"},
                    "keyId": "release-bot",
                    "groups": ["session", "profiles"],
                    "assertActor": false,
                    "displayName": "Release bot",
                    "secret": expected_api,
                })
            );
            json!({"apiKey":{"keyId":"release-bot","keyPrefix":&expected_api[..12],"scope":params["scope"],"groups":["session","profiles"],"assertActor":false,"createdBy":{"kind":"local"},"displayName":"Release bot","createdAtMs":1},"created":true,"secretReplaced":false})
        }
        "deployment/api-keys/rotate" | "deployment/api-keys/revoke" => {
            assert_eq!(
                *params,
                json!({"scope":{"kind":"deployment"},"keyId":"platform"})
            );
            let api_key = json!({"keyId":"platform","keyPrefix":"lsk_rotated1","scope":{"kind":"deployment"},"groups":["session"],"assertActor":true,"createdBy":{"kind":"local"},"createdAtMs":1});
            if method.ends_with("rotate") {
                json!({"apiKey":api_key,"secret":"lsk_rotated_secret"})
            } else {
                json!({"apiKey":api_key})
            }
        }
        "environments/registration-keys/put" => {
            assert_eq!(
                *params,
                json!({
                    "registrationKeyId": "build-pool",
                    "displayName": "Build pool",
                    "identityMode": "ephemeral",
                    "maxActiveEnvironments": 4,
                    "secret": expected_registration,
                })
            );
            json!({"registrationKey":{"registrationKeyId":"build-pool","displayName":"Build pool","keyPrefix":&expected_registration[..12],"identityMode":"ephemeral","maxActiveEnvironments":4,"ephemeralDisconnectGraceMs":300000,"status":"active","registeredEnvironmentCount":0,"activeEnvironmentCount":0,"createdAtMs":1,"updatedAtMs":1},"created":false,"secretReplaced":true})
        }
        _ => panic!("unexpected request {method}"),
    });

    // The secret is read from a file or the environment, never an argument.
    let file = runtime.directory.path().join("api-key");
    std::fs::write(&file, format!("{api_secret}\n")).unwrap();
    let output = runtime.json(&[
        "api-key",
        "put",
        "release-bot",
        "--display-name",
        "Release bot",
        "--group",
        "session",
        "--group",
        "profiles",
        "--secret-file",
        file.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(output["created"], true);
    assert!(!output.to_string().contains(&api_secret));

    let declared = Command::new(env!("CARGO_BIN_EXE_lightspeed"))
        .current_dir(runtime.directory.path())
        .args([
            "environment",
            "registration-key",
            "put",
            "build-pool",
            "--display-name",
            "Build pool",
            "--mode",
            "ephemeral",
            "--max-active",
            "4",
            "--secret-env",
            "POOL_SECRET",
        ])
        .env(
            "LIGHTSPEED_CONFIG_DIR",
            runtime.directory.path().join("config"),
        )
        .env("LIGHTSPEED_API_URL", &runtime.endpoint)
        .env("LIGHTSPEED_API_KEY", "lsk_fixture")
        .env(
            "LIGHTSPEED_UNIVERSE",
            "00000000-0000-0000-0000-000000000001",
        )
        .env("POOL_SECRET", &registration_secret)
        .output()
        .unwrap();
    assert!(
        declared.status.success(),
        "{}",
        String::from_utf8_lossy(&declared.stderr)
    );
    let stdout = String::from_utf8(declared.stdout).unwrap();
    assert!(stdout.contains("The previous secret is retired."));
    assert!(!stdout.contains(&registration_secret));

    // A declared key needs a secret source, and exactly one.
    for sources in [
        &[][..],
        &["--secret-stdin", "--secret-env", "POOL_SECRET"][..],
    ] {
        let mut args = vec!["api-key", "put", "x", "--display-name", "x", "--deployment"];
        args.extend_from_slice(sources);
        assert!(!runtime.run(&args).status.success());
    }

    // Rotation and revocation address a key by its id and scope: the
    // selected universe unless --deployment is given.
    let rotated = runtime.json(&["api-key", "rotate", "platform", "--deployment", "--json"]);
    assert_eq!(rotated["secret"], "lsk_rotated_secret");
    runtime.success(&["api-key", "revoke", "platform", "--deployment"]);
}

#[test]
fn a_credential_is_declared_and_its_value_replaced_with_its_settings_kept() {
    let grant = |token_marker: &str| json!({"grantId":"tracker-token","providerId":"static","providerKind":"staticToken","exposure":"brokered","createdBy":{"kind":"local"},"displayName":"Tracker","subjectHint":"ci","scopes":["read"],"audience":"https://tracker.example.com/mcp","hasAccessToken":true,"hasRefreshToken":false,"expiresAtMs":99,"status":"active","metadata":{"owner":token_marker},"leaseCount":0,"createdAtMs":1,"updatedAtMs":2});
    let runtime = Runtime::start(move |method, params| match method {
        "auth/grants/read" => {
            assert_eq!(*params, json!({"grantId":"tracker-token"}));
            json!({"grant": grant("ops")})
        }
        "auth/grants/put" => {
            if params["dryRun"] == true {
                // A declaration: exactly the settings given, nothing carried over.
                assert_eq!(
                    *params,
                    json!({
                        "grantId": "tracker-token",
                        "exposure": "brokered",
                        "token": "declared-token",
                        "displayName": "Tracker",
                        "audience": "https://tracker.example.com/mcp",
                        "dryRun": true,
                    })
                );
                json!({"grant": grant("ops"), "created": false, "tokenReplaced": true})
            } else {
                // A replacement: the settings the credential has, the new
                // token, and not the old token's expiry.
                assert_eq!(
                    *params,
                    json!({
                        "grantId": "tracker-token",
                        "providerId": "static",
                        "exposure": "brokered",
                        "token": "replaced-token",
                        "displayName": "Tracker",
                        "subjectHint": "ci",
                        "scopes": ["read"],
                        "audience": "https://tracker.example.com/mcp",
                        "metadata": {"owner": "ops"},
                    })
                );
                json!({"grant": grant("ops"), "created": false, "tokenReplaced": true})
            }
        }
        _ => panic!("unexpected request {method}"),
    });
    let with_token = |args: &[&str], token: &str| {
        Command::new(env!("CARGO_BIN_EXE_lightspeed"))
            .current_dir(runtime.directory.path())
            .args(args)
            .env(
                "LIGHTSPEED_CONFIG_DIR",
                runtime.directory.path().join("config"),
            )
            .env("LIGHTSPEED_API_URL", &runtime.endpoint)
            .env("LIGHTSPEED_API_KEY", "lsk_fixture")
            .env(
                "LIGHTSPEED_UNIVERSE",
                "00000000-0000-0000-0000-000000000001",
            )
            .env("TRACKER_TOKEN", token)
            .output()
            .unwrap()
    };

    let declared = with_token(
        &[
            "credential",
            "put",
            "tracker-token",
            "--token-env",
            "TRACKER_TOKEN",
            "--display-name",
            "Tracker",
            "--audience",
            "https://tracker.example.com/mcp",
            "--dry-run",
        ],
        "declared-token",
    );
    assert!(
        declared.status.success(),
        "{}",
        String::from_utf8_lossy(&declared.stderr)
    );
    let stdout = String::from_utf8(declared.stdout).unwrap();
    assert!(stdout.contains("Would replace the credential's token."));
    assert!(!stdout.contains("declared-token"));

    let replaced = with_token(
        &[
            "credential",
            "replace",
            "tracker-token",
            "--token-env",
            "TRACKER_TOKEN",
        ],
        "replaced-token",
    );
    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    assert!(
        !String::from_utf8(replaced.stdout)
            .unwrap()
            .contains("replaced-token")
    );
    let methods: Vec<String> = runtime
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["method"].as_str().unwrap().to_owned())
        .filter(|method| method.starts_with("auth/"))
        .collect();
    assert_eq!(
        methods,
        ["auth/grants/put", "auth/grants/read", "auth/grants/put"]
    );
}

/// A runtime that keeps what is put into it, for the document commands: a
/// universe that exists once created, token credentials whose token it
/// compares itself, MCP servers with revisions, and registration keys that
/// list the prefix of their secret.
fn document_runtime() -> Runtime {
    let mut universe: Option<Value> = None;
    let mut tokens = std::collections::BTreeMap::<String, (Value, String)>::new();
    let mut servers = std::collections::BTreeMap::<String, Value>::new();
    let mut keys = std::collections::BTreeMap::<String, (Value, String)>::new();
    Runtime::start_api(move |method, params| {
        Ok(match method {
            "deployment/universes/read" => match &universe {
                Some(universe) => json!({ "universe": universe }),
                None => {
                    return Err(
                        json!({"code":-32004,"message":"unknown universe","data":{"kind":"notFound","message":"unknown universe"}}),
                    );
                }
            },
            "deployment/universes/create" => {
                assert_eq!(params["handle"], "research");
                universe = Some(
                    json!({"universeId":params["universeId"],"handle":"research","createdAtMs":1,"sessions":0,"workspaces":0,"profiles":0,"blobBytes":0}),
                );
                json!({ "universe": universe, "created": true })
            }
            "auth/grants/list" => json!({
                "grants": tokens.values().map(|(view, _)| view.clone()).collect::<Vec<_>>()
            }),
            "auth/grants/put" => {
                let id = params["grantId"].as_str().unwrap().to_owned();
                let token = params["token"].as_str().unwrap().to_owned();
                assert!(!token.is_empty(), "a put carries the resolved token");
                let existing = tokens.get(&id).map(|(_, stored)| stored.clone());
                let view = json!({
                    "grantId": id, "providerId": "static", "providerKind": "staticToken",
                    "exposure": "brokered", "createdBy": {"kind": "local"},
                    "displayName": params["displayName"],
                    "hasAccessToken": true, "hasRefreshToken": false, "status": "active",
                    "leaseCount": 0, "createdAtMs": 1, "updatedAtMs": 1,
                });
                if params["dryRun"] != json!(true) {
                    tokens.insert(id, (view.clone(), token.clone()));
                }
                json!({
                    "grant": view,
                    "created": existing.is_none(),
                    "tokenReplaced": existing.is_some_and(|stored| stored != token),
                })
            }
            "mcp/servers/list" => json!({ "servers": servers.values().collect::<Vec<_>>() }),
            "mcp/servers/put" => {
                let id = params["server"]["serverId"].as_str().unwrap().to_owned();
                let revision = servers
                    .get(&id)
                    .map_or(0, |server| server["revision"].as_u64().unwrap());
                // A replacement is guarded by the revision that was read.
                assert_eq!(
                    params.get("expectedRevision").and_then(Value::as_u64),
                    (revision > 0).then_some(revision)
                );
                let mut view = json!({
                    "execution": "hosted", "exposure": "upfront", "approval": "never",
                    "allowPrivateNetwork": false, "authPolicy": "none", "status": "active",
                    "headers": {},
                });
                for (field, value) in params["server"].as_object().unwrap() {
                    view[field] = value.clone();
                }
                view["revision"] = json!(revision + 1);
                view["createdAtMs"] = json!(1);
                view["updatedAtMs"] = json!(1);
                servers.insert(id, view.clone());
                json!({ "server": view })
            }
            "environments/registration-keys/list" => json!({
                "registrationKeys": keys.values().map(|(view, _)| view.clone()).collect::<Vec<_>>()
            }),
            "environments/registration-keys/put" => {
                let id = params["registrationKeyId"].as_str().unwrap().to_owned();
                let secret = params["secret"].as_str().unwrap().to_owned();
                let existing = keys.get(&id).map(|(_, stored)| stored.clone());
                let view = json!({
                    "registrationKeyId": id, "displayName": params["displayName"],
                    "keyPrefix": &secret[..12], "identityMode": params["identityMode"],
                    "maxActiveEnvironments": params["maxActiveEnvironments"],
                    "ephemeralDisconnectGraceMs": 300000, "status": "active",
                    "registeredEnvironmentCount": 0, "activeEnvironmentCount": 0,
                    "createdAtMs": 1, "updatedAtMs": 1,
                });
                keys.insert(id, (view.clone(), secret.clone()));
                json!({
                    "registrationKey": view,
                    "created": existing.is_none(),
                    "secretReplaced": existing.is_some_and(|stored| stored != secret),
                })
            }
            _ => panic!("unexpected request {method}"),
        })
    })
}

fn run_documents(runtime: &Runtime, args: &[&str], secrets: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightspeed"));
    command
        .current_dir(runtime.directory.path())
        .args(args)
        .env(
            "LIGHTSPEED_CONFIG_DIR",
            runtime.directory.path().join("config"),
        )
        .env("LIGHTSPEED_API_URL", &runtime.endpoint)
        .env("LIGHTSPEED_API_KEY", "lsk_fixture")
        .env_remove("LIGHTSPEED_UNIVERSE")
        .env_remove("TRACKER_TOKEN")
        .env_remove("RUNNERS_KEY");
    for (name, value) in secrets {
        command.env(name, value);
    }
    command.output().unwrap()
}

#[test]
fn a_document_is_diffed_applied_and_applied_again_without_a_write() {
    let runtime = document_runtime();
    let directory = runtime.directory.path();
    std::fs::write(
        directory.join("research.yaml"),
        r#"
schemaVersion: 1
kind: universe
handle: research
credentials:
  - grantId: tracker-token
    displayName: Issue tracker
    tokenFrom: { env: TRACKER_TOKEN }
mcpServers:
  - serverId: tracker
    serverUrl: https://tracker.example.com/mcp
    defaultServerLabel: tracker
    headers:
      x-upstream-token: { type: credential, grantId: tracker-token }
registrationKeys:
  - registrationKeyId: runners
    displayName: Runners
    identityMode: persistent
    secretFrom: { env: RUNNERS_KEY }
"#,
    )
    .unwrap();
    let first_secrets = [
        ("TRACKER_TOKEN", "tracker-secret-1"),
        ("RUNNERS_KEY", "lsrk_first-secret-of-the-runners-key"),
    ];
    let text = |output: &Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    let writes = |runtime: &Runtime| {
        runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                let method = request["method"].as_str().unwrap();
                (method.ends_with("/put") || method.ends_with("/create"))
                    && request["params"]["dryRun"] != json!(true)
            })
            .count()
    };

    // Requests other than the connection's own handshake.
    let calls = |runtime: &Runtime| {
        runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] != "initialize")
            .count()
    };

    // A missing secret fails before anything is read or written.
    let missing = run_documents(&runtime, &["config", "apply", "-f", "research.yaml"], &[]);
    assert!(!missing.status.success());
    let message = text(&missing);
    assert!(message.contains("TRACKER_TOKEN is not set"), "{message}");
    assert!(message.contains("RUNNERS_KEY is not set"), "{message}");
    assert_eq!(calls(&runtime), 0);

    // diff reports the plan with status 2 and writes nothing.
    let planned = run_documents(
        &runtime,
        &["config", "diff", "-f", "research.yaml"],
        &first_secrets,
    );
    assert_eq!(planned.status.code(), Some(2), "{}", text(&planned));
    let plan = text(&planned);
    for line in [
        "create      universe/research",
        "create      credentials/tracker-token",
        "create      mcpServers/tracker",
        "create      registrationKeys/runners",
        "4 to change, 0 unchanged",
    ] {
        assert!(plan.contains(line), "missing {line:?} in:\n{plan}");
    }
    assert_eq!(writes(&runtime), 0);

    // apply writes each resource once, and no secret reaches the output.
    let applied = run_documents(
        &runtime,
        &["config", "apply", "-f", "research.yaml"],
        &first_secrets,
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let report = text(&applied);
    assert!(
        report.contains("created     mcpServers/tracker"),
        "{report}"
    );
    assert!(report.contains("4 changed, 0 unchanged"), "{report}");
    assert!(!report.contains("tracker-secret-1") && !report.contains("lsrk_first"));
    assert_eq!(writes(&runtime), 4);

    // Applying the same document again changes nothing.
    let again = run_documents(
        &runtime,
        &["config", "apply", "-f", "research.yaml"],
        &first_secrets,
    );
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("3 unchanged"), "{}", text(&again));
    assert_eq!(writes(&runtime), 4, "an unchanged document causes no write");
    let quiet = run_documents(
        &runtime,
        &["config", "diff", "-f", "research.yaml"],
        &first_secrets,
    );
    assert_eq!(quiet.status.code(), Some(0), "{}", text(&quiet));

    // A changed field and two changed secrets are found and written; the
    // server compares the token, the key is recognised by its prefix.
    let edited = std::fs::read_to_string(directory.join("research.yaml"))
        .unwrap()
        .replace("tracker.example.com/mcp", "tracker.example.com/v2/mcp");
    std::fs::write(directory.join("research.yaml"), edited).unwrap();
    let second_secrets = [
        ("TRACKER_TOKEN", "tracker-secret-2"),
        ("RUNNERS_KEY", "lsrk_second-secret-of-the-runners-key"),
    ];
    let changed = run_documents(
        &runtime,
        &["config", "diff", "-f", "research.yaml", "--json"],
        &second_secrets,
    );
    assert_eq!(changed.status.code(), Some(2));
    let steps: Value = serde_json::from_slice(&changed.stdout).unwrap();
    let step = |section: &str| {
        steps
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["section"] == section)
            .unwrap_or_else(|| panic!("no step for {section}"))
            .clone()
    };
    assert_eq!(step("credentials")["action"], "update");
    assert_eq!(step("credentials")["secretReplaced"], true);
    assert_eq!(step("mcpServers")["fields"], json!(["serverUrl"]));
    assert_eq!(step("registrationKeys")["secretReplaced"], true);
    let updated = run_documents(
        &runtime,
        &["config", "apply", "-f", "research.yaml"],
        &second_secrets,
    );
    assert!(updated.status.success(), "{}", text(&updated));
    assert_eq!(writes(&runtime), 7);

    // An unknown field is refused before any call.
    std::fs::write(
        directory.join("typo.yaml"),
        "schemaVersion: 1\nkind: universe\nhandle: other\nmcpServers:\n  - serverId: x\n    serverUrl: https://x.example.com/mcp\n    defaultServerLabel: x\n    serverURL: typo\n",
    )
    .unwrap();
    let before = calls(&runtime);
    let typo = run_documents(&runtime, &["config", "apply", "-f", "typo.yaml"], &[]);
    assert!(!typo.status.success());
    assert!(
        text(&typo).contains("unknown field `serverURL`"),
        "{}",
        text(&typo)
    );
    assert_eq!(calls(&runtime), before);
}

#[test]
fn the_document_schema_is_printed_without_a_connection() {
    let offline = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lightspeed"))
        .current_dir(offline.path())
        .args(["config", "schema"])
        .env("LIGHTSPEED_CONFIG_DIR", offline.path().join("config"))
        .env_remove("LIGHTSPEED_API_URL")
        .env_remove("LIGHTSPEED_API_KEY")
        .env_remove("LIGHTSPEED_UNIVERSE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["title"], "Lightspeed configuration document");
    assert!(schema["definitions"]["UniverseDocument"].is_object());
}
