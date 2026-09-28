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
        let listener = TcpListener::bind("127.0.0.1:0").expect("local HTTP fixture must bind");
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/rpc", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::spawn(move || {
            let mut universes = json!([
                {"universeId":"00000000-0000-0000-0000-000000000001","slug":"one","createdAtMs":0,"sessions":0,"profiles":0,"workspaces":0,"blobBytes":0},
                {"universeId":"00000000-0000-0000-0000-000000000002","createdAtMs":0,"sessions":0,"profiles":0,"workspaces":0,"blobBytes":0}
            ]);
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
                let result = match method {
                    "initialize" => {
                        json!({"protocolVersion":"test", "serverInfo":{"name":"fixture","version":"test","gitSha":"test","envd":{"version":"test","gitSha":"test","protocolVersion":1,"targets":[]}}, "capabilities":{"notifications":false,"historyRead":true,"eventLog":true,"localExecution":false}, "caller":{"scope":scope,"single":single,"keyPrefix":if single {Value::Null} else {json!("lsk_fixture")},"groups":groups}})
                    }
                    "deployment/universes/list" => {
                        json!({"universes":universes})
                    }
                    "deployment/universes/read" => {
                        json!({"universe":universes.as_array().unwrap().iter().find(|u| u["universeId"] == body["params"]["universeId"])})
                    }
                    "deployment/universes/slug/put" => {
                        let universe = universes
                            .as_array_mut()
                            .unwrap()
                            .iter_mut()
                            .find(|u| u["universeId"] == body["params"]["universeId"])
                            .unwrap();
                        universe["slug"] = body["params"]["slug"].clone();
                        json!({"universe":universe})
                    }
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
        .env_remove("LIGHTSPEED_API_KEY")
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
            "--api-key-stdin",
        ],
        Some("lsk_original"),
    ));
    success(cli(dir, &["connect", "use", "saved"], None));
    let overrides = [
        ("LIGHTSPEED_API_URL", other.endpoint.as_str()),
        ("LIGHTSPEED_API_KEY", "lsk_replacement"),
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
            "--api-key-stdin",
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
    assert!(universes.contains("00000000-0000-0000-0000-000000000002  (no slug)"));
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
    let unnamed = success(cli(
        dir,
        &[
            "--universe",
            "00000000-0000-0000-0000-000000000002",
            "universe",
            "status",
        ],
        None,
    ));
    assert!(unnamed.contains("Slug: (no slug)"));
    let renamed = success(cli(dir, &["universe", "set-slug", "one", "renamed"], None));
    assert!(renamed.contains("00000000-0000-0000-0000-000000000001  renamed"));
    let status = success(cli(dir, &["connect", "status", "--json"], None));
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap()["selectedUniverse"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap()["activeUniverse"]["slug"],
        "renamed"
    );
    let status = success(cli(dir, &["connect", "status"], None));
    assert!(status.contains("\n\nAPI key:\n"));
    assert!(status.ends_with(
        "\n\nSelected universe:\n  Slug: renamed\n  UUID: 00000000-0000-0000-0000-000000000001\n"
    ));
    let universe_status = success(cli(dir, &["universe", "status", "--json"], None));
    assert_eq!(
        serde_json::from_str::<Value>(&universe_status).unwrap(),
        json!({"universeId":"00000000-0000-0000-0000-000000000001", "slug":"renamed"})
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
fn universe_key_and_explicit_single_mode_send_no_selection_header() {
    for single in [false, true] {
        let runtime = Runtime::start(
            json!({"kind":"universe","universeId":"00000000-0000-0000-0000-000000000001"}),
            single,
        );
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path();
        let flag = if single {
            "--single"
        } else {
            "--api-key-stdin"
        };
        success(cli(
            dir,
            &["connect", "add", "bound", "--url", &runtime.endpoint, flag],
            (!single).then_some("lsk_fixture_secret"),
        ));
        success(cli(dir, &["connect", "use", "bound"], None));
        success(cli(dir, &["models", "list"], None));
        let status = success(cli(dir, &["universe", "status", "--json"], None));
        let status: Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["universeId"], "00000000-0000-0000-0000-000000000001");
        if single {
            assert_eq!(status["slug"], "one");
            let listed = success(cli(dir, &["universe", "list"], None));
            assert!(listed.contains("* 00000000-0000-0000-0000-000000000001  one"));
        } else {
            assert!(
                status["slugUnavailableReason"]
                    .as_str()
                    .unwrap()
                    .contains("requires a deployment key")
            );
            assert!(
                !runtime
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(method, _)| method.starts_with("deployment/"))
            );
        }
        assert!(
            !cli(
                dir,
                &[
                    "--universe",
                    "00000000-0000-0000-0000-000000000002",
                    "models",
                    "list"
                ],
                None
            )
            .status
            .success()
        );
        for (_, headers) in runtime.requests.lock().unwrap().iter() {
            assert!(!headers.contains("x-lightspeed-universe:"));
            assert_eq!(headers.contains("authorization:"), !single);
        }
    }
}

#[test]
fn universe_status_distinguishes_restricted_metadata_and_missing_universes() {
    for restricted in [true, false] {
        let runtime = Runtime::with_groups(
            json!({"kind":"deployment"}),
            false,
            if restricted {
                json!(["session"])
            } else {
                json!(["deployment/universes"])
            },
        );
        let directory = tempfile::tempdir().unwrap();
        let overrides = [
            ("LIGHTSPEED_API_URL", runtime.endpoint.as_str()),
            ("LIGHTSPEED_API_KEY", "lsk_fixture_secret"),
            (
                "LIGHTSPEED_UNIVERSE",
                "00000000-0000-0000-0000-000000000003",
            ),
        ];
        let output = success(cli_env(
            directory.path(),
            &["universe", "status", "--json"],
            None,
            &overrides,
        ));
        let status: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(status["universeId"], overrides[2].1);
        assert!(status["slug"].is_null());
        let reason = status["slugUnavailableReason"].as_str().unwrap();
        if restricted {
            assert!(reason.contains("requires a deployment key"));
        } else {
            assert!(reason.contains("unknown universe"));
        }
        assert_eq!(
            runtime
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _)| method == "deployment/universes/read")
                .count(),
            usize::from(!restricted)
        );
    }
}
