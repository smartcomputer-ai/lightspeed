//! Exercise the actual CLI process, persisted connections and wire headers.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

struct Runtime {
    endpoint: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Runtime {
    fn start(scope: Value, single: bool) -> Self {
        Self::with_groups(
            scope,
            single,
            json!(["session", "models", "deployment/universes"]),
        )
    }

    fn with_groups(scope: Value, single: bool, groups: Value) -> Self {
        Self::with_startup_statuses(scope, single, groups, vec![])
    }

    fn with_startup_statuses(
        _scope: Value,
        single: bool,
        _groups: Value,
        statuses: Vec<u16>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("local HTTP fixture must bind");
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/rpc", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::spawn(move || {
            let mut universes = json!([
                {"universeId":"00000000-0000-0000-0000-000000000001","handle":"one","createdAtMs":0,"sessions":0,"profiles":0,"workspaces":0,"blobBytes":0},
                {"universeId":"00000000-0000-0000-0000-000000000002","handle":"u-000000000002","createdAtMs":0,"sessions":0,"profiles":0,"workspaces":0,"blobBytes":0}
            ]);
            let mut statuses = statuses.into_iter();
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                // Accepted sockets can inherit the listener's nonblocking mode.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = vec![];
                let mut buffer = [0; 4096];
                let (headers, body) = loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8(bytes[..end].to_vec())
                            .unwrap()
                            .to_lowercase();
                        let size: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break (
                                headers,
                                serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + size])
                                    .unwrap(),
                            );
                        }
                    }
                };
                let method = body["method"].as_str().unwrap();
                recorded.lock().unwrap().push((method.into(), headers));
                if method == "initialize"
                    && let Some(status) = statuses.next()
                {
                    write!(stream, "HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    continue;
                }
                let result = match method {
                    "initialize" => {
                        json!({"protocolVersion":"test", "serverInfo":{"name":"fixture","version":"test","gitSha":"test","envd":{"version":"test","gitSha":"test","protocolVersion":1,"targets":[]}}, "capabilities":{"notifications":false,"historyRead":true,"eventLog":true,"localExecution":false}, "caller":{"single":single,"tokenName":if single {Value::Null} else {json!("fixture")}}})
                    }
                    "deployment/universes/list" => {
                        json!({"universes":universes})
                    }
                    // As the runtime does, address a universe by UUID or handle.
                    "deployment/universes/read" => {
                        let named = &body["params"]["universe"];
                        json!({"universe":universes.as_array().unwrap().iter().find(|u| u["universeId"] == *named || u["handle"] == *named)})
                    }
                    "deployment/universes/handle/put" => {
                        let named = &body["params"]["universe"];
                        let universe = universes
                            .as_array_mut()
                            .unwrap()
                            .iter_mut()
                            .find(|u| u["universeId"] == *named || u["handle"] == *named)
                            .unwrap();
                        universe["handle"] = body["params"]["handle"].clone();
                        json!({"universe":universe})
                    }
                    "deployment/universes/configuration/put" => {
                        let named = &body["params"]["universe"];
                        let universe = universes
                            .as_array_mut()
                            .unwrap()
                            .iter_mut()
                            .find(|u| u["universeId"] == *named || u["handle"] == *named)
                            .unwrap();
                        for field in ["displayName", "members", "directoryGroupMappings"] {
                            if let Some(value) = body["params"].get(field) {
                                universe[field] = value.clone();
                            }
                        }
                        json!({"universe": universe})
                    }
                    "auth/grants/list" => json!({"grants":[]}),
                    "auth/providers/list" => json!({"providers":[]}),
                    "models/defaults/read" => json!({"defaults":{}}),
                    "mcp/servers/list" => json!({"servers":[]}),
                    "environments/provider-bindings/list" => json!({"bindings":[]}),
                    "environments/registration-keys/list" => json!({"registrationKeys":[]}),
                    "environments/list" => json!({"environments":[]}),
                    "profiles/list" => json!({"profiles":[]}),
                    "models/list" => json!({"models":[],"providers":[]}),
                    _ => panic!("unexpected method {method}"),
                };
                let response = if method == "deployment/universes/read"
                    && result["universe"].is_null()
                {
                    json!({"id":body["id"],"error":{"code":-32004,"message":"unknown universe"}})
                } else {
                    json!({"id":body["id"],"result":{"result":result,"notifications":[]}})
                }
                .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn cli(dir: &std::path::Path, args: &[&str], secret: Option<&str>) -> Output {
    cli_env(dir, args, secret, &[])
}
fn cli_env(
    dir: &std::path::Path,
    args: &[&str],
    secret: Option<&str>,
    env: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightspeed"));
    command
        .current_dir(dir)
        .args(args)
        .env("LIGHTSPEED_CONFIG_DIR", dir.join("config"))
        .env_remove("LIGHTSPEED_API_URL")
        .env_remove("LIGHTSPEED_RPC_TOKEN")
        .env_remove("LIGHTSPEED_UNIVERSE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    if let Some(secret) = secret {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(secret.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn environment_precedence_and_endpoint_changes_do_not_reuse_saved_keys() {
    let runtime = Runtime::start(json!({"kind":"deployment"}), false);
    let other = Runtime::start(json!({"kind":"deployment"}), false);
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    success(cli(
        dir,
        &[
            "connect",
            "add",
            "saved",
            "--url",
            &runtime.endpoint,
            "--token-stdin",
        ],
        Some("lsk_original"),
    ));
    success(cli(dir, &["connect", "use", "saved"], None));
    let overrides = [
        ("LIGHTSPEED_API_URL", other.endpoint.as_str()),
        ("LIGHTSPEED_RPC_TOKEN", "lsk_replacement"),
    ];
    success(cli_env(
        dir,
        &["--connection", "saved", "connect", "status"],
        None,
        &overrides,
    ));
    assert!(other.requests.lock().unwrap().is_empty());
    assert!(
        runtime
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("authorization: bearer lsk_original")
    );
    success(cli_env(dir, &["connect", "status"], None, &overrides));
    assert!(
        other
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("authorization: bearer lsk_replacement")
    );
    // An endpoint override alone must never forward the stored secret or downgrade authentication.
    let refused = cli_env(dir, &["connect", "status"], None, &overrides[..1]);
    assert!(!refused.status.success());
    assert!(
        !other
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("authorization:")
    );
    // A deliberately supplied replacement remains usable if the old local key was lost.
    for file in std::fs::read_dir(dir.join("config/credentials")).unwrap() {
        std::fs::remove_file(file.unwrap().path()).unwrap();
    }
    success(cli_env(dir, &["connect", "status"], None, &overrides[1..]));
    assert!(
        runtime
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("authorization: bearer lsk_replacement")
    );
    assert!(
        !cli(dir, &["--connection", "saved", "connect", "status"], None)
            .status
            .success()
    );
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
#[test]
fn named_connections_universe_selection_and_per_command_override() {
    let runtime = Runtime::start(json!({"kind":"deployment"}), false);
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    success(cli(
        dir,
        &[
            "connect",
            "add",
            "test",
            "--url",
            &runtime.endpoint,
            "--token-stdin",
        ],
        Some("lsk_fixture_secret"),
    ));
    let list = success(cli(dir, &["connect", "list", "--json"], None));
    assert!(!list.contains("lsk_fixture_secret"));
    assert_eq!(
        serde_json::from_str::<Value>(&list).unwrap()[0]["selected"],
        false
    );
    success(cli(dir, &["connect", "use", "test"], None));
    let none = success(cli(dir, &["universe", "status", "--json"], None));
    assert!(serde_json::from_str::<Value>(&none).unwrap()["universeId"].is_null());
    let none_list = success(cli(dir, &["universe", "list"], None));
    assert!(!none_list.contains('*'));
    success(cli(dir, &["universe", "use", "one"], None));
    success(cli(dir, &["models", "list"], None));
    success(cli(
        dir,
        &[
            "--connection",
            "test",
            "--universe",
            "00000000-0000-0000-0000-000000000002",
            "models",
            "list",
        ],
        None,
    ));
    let universes = success(cli(dir, &["universe", "list"], None));
    assert!(universes.contains("* 00000000-0000-0000-0000-000000000001  one"));
    assert!(universes.contains("00000000-0000-0000-0000-000000000002  u-000000000002"));
    let overridden = success(cli(
        dir,
        &[
            "--universe",
            "00000000-0000-0000-0000-000000000002",
            "universe",
            "list",
            "--json",
        ],
        None,
    ));
    let overridden: Value = serde_json::from_str(&overridden).unwrap();
    assert_eq!(overridden["universes"][0]["active"], false);
    assert_eq!(overridden["universes"][1]["active"], true);
    assert_eq!(
        overridden["activeUniverseId"],
        "00000000-0000-0000-0000-000000000002"
    );
    let derived = success(cli(
        dir,
        &[
            "--universe",
            "00000000-0000-0000-0000-000000000002",
            "universe",
            "status",
        ],
        None,
    ));
    assert!(derived.contains("Handle: u-000000000002"));
    let renamed = success(cli(
        dir,
        &["universe", "set-handle", "one", "renamed"],
        None,
    ));
    assert!(renamed.contains("00000000-0000-0000-0000-000000000001  renamed"));
    let status = success(cli(dir, &["connect", "status", "--json"], None));
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap()["selectedUniverse"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap()["activeUniverse"]["handle"],
        "renamed"
    );
    let status = success(cli(dir, &["connect", "status"], None));
    assert!(status.contains("  Token: fixture\n  Authority: full runtime access"));
    assert!(status.ends_with(
        "\n\nSelected universe:\n  Handle: renamed\n  UUID: 00000000-0000-0000-0000-000000000001\n"
    ));
    let universe_status = success(cli(dir, &["universe", "status", "--json"], None));
    assert_eq!(
        serde_json::from_str::<Value>(&universe_status).unwrap(),
        json!({"universeId":"00000000-0000-0000-0000-000000000001", "handle":"renamed"})
    );
    let requests = runtime.requests.lock().unwrap();
    let models: Vec<_> = requests
        .iter()
        .filter(|(m, _)| m == "models/list")
        .collect();
    assert!(
        models[0]
            .1
            .contains("x-lightspeed-universe: 00000000-0000-0000-0000-000000000001")
    );
    assert!(
        models[1]
            .1
            .contains("x-lightspeed-universe: 00000000-0000-0000-0000-000000000002")
    );
    for (method, headers) in requests.iter() {
        assert!(headers.contains("authorization: bearer lsk_fixture_secret"));
        if method != "models/list" {
            assert!(!headers.contains("x-lightspeed-universe:"), "{method}");
        }
    }
    drop(requests);
    success(cli(dir, &["connect", "remove", "test"], None));
    assert!(!cli(dir, &["models", "list"], None).status.success());
    assert_eq!(
        std::fs::read_dir(dir.join("config/credentials"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_named_handle_is_sent_as_given_and_only_a_saved_selection_becomes_a_uuid() {
    const ONE: &str = "00000000-0000-0000-0000-000000000001";
    let universe_headers = |runtime: &Runtime, method: &str| -> Vec<String> {
        runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, headers)| {
                headers
                    .lines()
                    .find_map(|line| line.strip_prefix("x-lightspeed-universe: "))
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    };

    let runtime = Runtime::start(json!({"kind":"deployment"}), false);
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    success(cli(
        dir,
        &[
            "--universe",
            "one",
            "connect",
            "add",
            "test",
            "--url",
            &runtime.endpoint,
            "--token-stdin",
        ],
        Some("lsk_fixture_secret"),
    ));
    let saved = success(cli(dir, &["connect", "list", "--json"], None));
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap()[0]["universe"],
        ONE
    );
    success(cli(dir, &["connect", "use", "test"], None));
    success(cli(dir, &["models", "list"], None));
    success(cli(dir, &["--universe", "one", "models", "list"], None));
    assert_eq!(universe_headers(&runtime, "models/list"), [ONE, "one"]);
    let status = success(cli(
        dir,
        &["--universe", "one", "universe", "status", "--json"],
        None,
    ));
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap(),
        json!({"universeId": ONE, "handle": "one"})
    );
    let listed = success(cli(
        dir,
        &["--universe", "u-000000000002", "universe", "list", "--json"],
        None,
    ));
    let listed: Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed["universes"][0]["active"], false);
    assert_eq!(listed["universes"][1]["active"], true);
    assert_eq!(
        listed["activeUniverseId"],
        "00000000-0000-0000-0000-000000000002"
    );
}

#[test]
fn config_wait_retries_startup_and_applies_declarations_idempotently() {
    let runtime = Runtime::with_startup_statuses(
        json!({"kind":"deployment"}),
        false,
        json!(["deployment/universes"]),
        vec![503],
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("universe.yaml"), "schemaVersion: 1\nkind: universe\nhandle: one\ndisplayName: Research\nmembers: []\ndirectoryGroupMappings:\n  - issuer: https://id.example\n    group: readers\n    role: viewer\n").unwrap();
    let env = [
        ("LIGHTSPEED_API_URL", runtime.endpoint.as_str()),
        ("LIGHTSPEED_RPC_TOKEN", "lsk_fixture"),
    ];
    let output = cli_env(
        dir.path(),
        &["config", "apply", "--wait", "5", "-f", "universe.yaml"],
        None,
        &env,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = runtime.requests.lock().unwrap().clone();
    assert_eq!(calls.iter().filter(|(m, _)| m == "initialize").count(), 2);
    assert_eq!(
        calls
            .iter()
            .filter(|(m, _)| m == "deployment/universes/configuration/put")
            .count(),
        1
    );
    let output = cli_env(
        dir.path(),
        &["config", "diff", "-f", "universe.yaml"],
        None,
        &env,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("unchanged"));
    let exported = cli_env(
        dir.path(),
        &["--universe", "one", "config", "export", "--format", "json"],
        None,
        &env,
    );
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let exported: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(exported["displayName"], "Research");
    assert_eq!(exported["members"], json!([]));
    assert_eq!(exported["directoryGroupMappings"][0]["group"], "readers");
    std::fs::write(
        dir.path().join("exported.json"),
        serde_json::to_vec(&exported).unwrap(),
    )
    .unwrap();
    assert!(
        cli_env(
            dir.path(),
            &["config", "diff", "-f", "exported.json"],
            None,
            &env
        )
        .status
        .success()
    );

    std::fs::write(
        dir.path().join("universe.yaml"),
        "schemaVersion: 1\nkind: universe\nhandle: one\n",
    )
    .unwrap();
    assert!(
        cli_env(
            dir.path(),
            &["config", "apply", "-f", "universe.yaml"],
            None,
            &env
        )
        .status
        .success()
    );
    assert_eq!(
        runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == "deployment/universes/configuration/put")
            .count(),
        1
    );
}

#[test]
fn config_wait_does_not_retry_authentication_or_wait_without_the_flag() {
    for (status, wait) in [(401, true), (403, true), (503, false)] {
        let runtime = Runtime::with_startup_statuses(
            json!({"kind":"deployment"}),
            false,
            json!(["deployment/universes"]),
            vec![status],
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("universe.yaml"),
            "schemaVersion: 1\nkind: universe\nhandle: one\n",
        )
        .unwrap();
        let args = if wait {
            vec!["config", "apply", "--wait", "5", "-f", "universe.yaml"]
        } else {
            vec!["config", "apply", "-f", "universe.yaml"]
        };
        let output = cli_env(
            dir.path(),
            &args,
            None,
            &[
                ("LIGHTSPEED_API_URL", &runtime.endpoint),
                ("LIGHTSPEED_RPC_TOKEN", "lsk_fixture"),
            ],
        );
        assert!(!output.status.success());
        assert_eq!(runtime.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn config_wait_has_a_bounded_deadline() {
    let runtime = Runtime::with_startup_statuses(
        json!({"kind":"deployment"}),
        false,
        json!(["deployment/universes"]),
        vec![503; 20],
    );
    let dir = tempfile::tempdir().unwrap();
    let start = std::time::Instant::now();
    let output = cli_env(
        dir.path(),
        &["config", "apply", "--wait=1", "-f", "unused.yaml"],
        None,
        &[
            ("LIGHTSPEED_API_URL", &runtime.endpoint),
            ("LIGHTSPEED_RPC_TOKEN", "lsk_fixture"),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("within 1 seconds"));
    assert!(start.elapsed() < Duration::from_secs(4));
}
