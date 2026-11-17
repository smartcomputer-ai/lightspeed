//! Native Platform commands use the shared route contract and explicit secret outputs.
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

struct Platform {
    endpoint: String,
    requests: Arc<Mutex<Vec<(String, String, Value)>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    directory: tempfile::TempDir,
}
impl Platform {
    fn start() -> Self {
        Self::with_platform_admin(false)
    }
    fn with_platform_admin(platform_admin: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture bind");
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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
                let mut buffer = [0; 8192];
                let (method, path, body) = loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let size: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .unwrap_or_else(|| "0".into())
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            let mut request = headers.lines().next().unwrap().split_whitespace();
                            break (
                                request.next().unwrap().to_owned(),
                                request.next().unwrap().to_owned(),
                                if size == 0 {
                                    json!({})
                                } else {
                                    serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap()
                                },
                            );
                        }
                    }
                };
                recorded
                    .lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone()));
                let (status, response) = match (method.as_str(), path.as_str()) {
                    (_, "/api/v1/connection") => (
                        200,
                        json!({"runtime":"platform","contractRevision":"1","caller":{"single":false},"serverInfo":{"name":"Platform","version":"test"},"platform":{"principal":{"kind":"user","id":"creator","name":"Creator","platformAdmin":platform_admin},"credential":{"kind":"personalKey","id":"key-id"},"organizations":[],"restrictions":{}}}),
                    ),
                    ("GET", "/api/v1/platform/organizations/organization-a") => {
                        (200, json!({"organization":{"id":"organization-a"}}))
                    }
                    ("GET", "/api/v1/platform/organizations") => {
                        (200, json!({"organizations":[],"canCreate":true}))
                    }
                    ("PUT", "/api/v1/platform/organizations/organization-a/members/user-b") => (
                        200,
                        json!({"member":{"userId":"user-b","role":body["role"]}}),
                    ),
                    ("POST", "/api/v1/platform/organizations/organization-a/service-users") => (
                        200,
                        json!({"serviceUser":{"id":"worker","name":body["name"],"role":body["role"]}}),
                    ),
                    ("POST", "/api/v1/platform/organizations/organization-a/universes") => (
                        200,
                        json!({"universe":{"lightspeedUniverseId":body["universeId"],"handle":body["handle"],"organizationId":"organization-a"}}),
                    ),
                    ("GET", "/api/v1/universes/new-work") => (
                        404,
                        json!({"type":"about:blank","title":"Not found","detail":"not found","status":404,"kind":"notFound"}),
                    ),
                    (
                        "GET",
                        "/api/v1/universes/existing-work"
                        | "/api/v1/universes/00000000-0000-0000-0000-000000000002",
                    ) => (
                        200,
                        json!({"universe":{"universeId":"00000000-0000-0000-0000-000000000002","handle":"existing-work","protected":false}}),
                    ),
                    (
                        "GET",
                        "/api/v1/universes/00000000-0000-0000-0000-000000000002/environment-provider-bindings",
                    ) => (
                        200,
                        json!({"bindings":[{"providerId":"incus","enabled":true}]}),
                    ),
                    (
                        "PUT",
                        "/api/v1/universes/00000000-0000-0000-0000-000000000002/deployment-provider-bindings/incus",
                    ) => (
                        200,
                        json!({"binding":{"providerId":"incus","enabled":body["enabled"]}}),
                    ),
                    (
                        "PUT",
                        "/api/v1/universes/00000000-0000-0000-0000-000000000002/configuration",
                    ) => (200, json!({"universe":body})),
                    ("POST", "/api/v1/platform/credentials") => (
                        200,
                        json!({"credential":{"id":"key-id"},"secret":"lsk_supersecret"}),
                    ),
                    _ => (
                        500,
                        json!({"type":"about:blank","title":"Unexpected request","detail":format!("{method} {path}"),"status":500,"kind":"internal"}),
                    ),
                };
                let response = response.to_string();
                write!(socket, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
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
            .env("LIGHTSPEED_API_TOKEN", "lsk_fixture")
            .env_remove("LIGHTSPEED_UNIVERSE")
            .output()
            .unwrap()
    }
    fn success(&self, args: &[&str]) -> String {
        let result = self.run(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }
}
impl Drop for Platform {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.expect("HTTP fixture worker failed");
        }
    }
}

#[test]
fn organization_members_use_native_routes_and_path_owned_identifiers() {
    let platform = Platform::start();
    platform.success(&[
        "--organization",
        "organization-a",
        "platform",
        "organization",
        "member",
        "set-role",
        "user-b",
        "--role",
        "creator",
    ]);
    let requests = platform.requests.lock().unwrap();
    let request = requests
        .iter()
        .find(|(method, _, _)| method == "PUT")
        .unwrap();
    assert_eq!(
        request.1,
        "/api/v1/platform/organizations/organization-a/members/user-b"
    );
    assert_eq!(request.2, json!({"role":"creator"}));
}
#[test]
fn service_user_creation_uses_the_selected_organization() {
    let platform = Platform::start();
    platform.success(&[
        "--organization",
        "organization-a",
        "platform",
        "service-user",
        "create",
        "Automation",
        "--role",
        "creator",
    ]);
    let requests = platform.requests.lock().unwrap();
    let request = requests
        .iter()
        .find(|(method, _, _)| method == "POST")
        .unwrap();
    assert_eq!(
        request.1,
        "/api/v1/platform/organizations/organization-a/service-users"
    );
    assert_eq!(request.2, json!({"name":"Automation","role":"creator"}));
}
#[test]
fn platform_creation_uses_selected_organization_and_explicit_owner() {
    let platform = Platform::start();
    platform.success(&[
        "--organization",
        "organization-a",
        "universe",
        "create",
        "--handle",
        "new-work",
        "--name",
        "Work",
        "--owner-user",
        "creator",
        "--metadata",
        "team=blue",
    ]);
    let requests = platform.requests.lock().unwrap();
    let request = requests
        .iter()
        .find(|(method, _, _)| method == "POST")
        .unwrap();
    assert_eq!(
        request.1,
        "/api/v1/platform/organizations/organization-a/universes"
    );
    assert_eq!(
        request.2["initialOwner"],
        json!({"kind":"user","id":"creator"})
    );
    assert_eq!(request.2["metadata"], json!({"team":"blue"}));
    assert!(
        request.2.get("protected").is_none(),
        "organization default remains authoritative"
    );
}
#[test]
fn configuration_provisions_through_platform_with_default_protection() {
    let platform = Platform::start();
    let path = platform.directory.path().join("universe.json");
    std::fs::write(
        &path,
        r#"{"schemaVersion":1,"kind":"universe","handle":"new-work"}"#,
    )
    .unwrap();
    platform.success(&[
        "--organization",
        "organization-a",
        "config",
        "apply",
        "--file",
        path.to_str().unwrap(),
    ]);
    let requests = platform.requests.lock().unwrap();
    let request = requests
        .iter()
        .find(|(method, _, _)| method == "POST")
        .unwrap();
    assert_eq!(
        request.1,
        "/api/v1/platform/organizations/organization-a/universes"
    );
    assert_eq!(request.2["protected"], true);
}
#[test]
fn api_key_secret_requires_explicit_destination_and_is_excluded_from_regular_output() {
    let platform = Platform::start();
    assert!(
        !platform
            .run(&["platform", "api-key", "create", "Automation"])
            .status
            .success()
    );
    assert!(
        !platform
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _, _)| method == "POST")
    );
    let path = platform.directory.path().join("secret");
    let output = platform.success(&[
        "platform",
        "api-key",
        "create",
        "Automation",
        "--read-only",
        "--output-secret",
        path.to_str().unwrap(),
    ]);
    assert!(!output.contains("lsk_supersecret"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "lsk_supersecret\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn configuration_retry_keeps_an_explicit_provisioning_uuid() {
    let platform = Platform::start();
    let path = platform.directory.path().join("retry.json");
    std::fs::write(
        &path,
        r#"{"schemaVersion":1,"kind":"universe","handle":"new-work"}"#,
    )
    .unwrap();
    let id = "22222222-2222-4222-8222-222222222222";
    let args = [
        "--organization",
        "organization-a",
        "config",
        "apply",
        "--file",
        path.to_str().unwrap(),
        "--universe-id",
        id,
    ];
    platform.success(&args);
    platform.success(&args);
    let requests = platform.requests.lock().unwrap();
    let creations: Vec<_> = requests
        .iter()
        .filter(|(method, _, _)| method == "POST")
        .collect();
    assert_eq!(creations.len(), 2);
    assert!(
        creations
            .iter()
            .all(|(_, _, body)| body["universeId"] == id)
    );
}

#[test]
fn configuration_binding_changes_fail_before_any_document_is_written() {
    let platform = Platform::start();
    for (file, document) in [
        (
            "first.json",
            json!({"schemaVersion":1,"kind":"universe","handle":"new-work"}),
        ),
        (
            "second.json",
            json!({"schemaVersion":1,"kind":"universe","handle":"existing-work","environmentProviders":[{"providerId":"incus","enabled":false}]}),
        ),
    ] {
        std::fs::write(platform.directory.path().join(file), document.to_string()).unwrap();
    }
    let result = platform.run(&[
        "--organization",
        "organization-a",
        "config",
        "apply",
        "-f",
        "first.json",
        "second.json",
    ]);
    assert!(!result.status.success());
    let report = String::from_utf8_lossy(&result.stdout);
    assert!(
        report.contains("changing deployment provider admission requires Platform Admin"),
        "{report}"
    );
    assert!(
        platform
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[test]
fn configuration_creation_with_bindings_is_refused_before_provisioning() {
    let platform = Platform::start();
    std::fs::write(
        platform.directory.path().join("universe.json"),
        json!({"schemaVersion":1,"kind":"universe","handle":"new-work","environmentProviders":[{"providerId":"incus","enabled":true}]}).to_string(),
    ).unwrap();
    let result = platform.run(&[
        "--organization",
        "organization-a",
        "config",
        "apply",
        "-f",
        "universe.json",
    ]);
    assert!(!result.status.success());
    assert!(
        platform
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[test]
fn unchanged_or_unmanaged_bindings_do_not_require_platform_admin() {
    for bindings in [
        None,
        Some(json!([])),
        Some(json!([{"providerId":"incus","enabled":true}])),
    ] {
        let platform = Platform::start();
        let mut document = json!({"schemaVersion":1,"kind":"universe","handle":"existing-work"});
        if let Some(bindings) = bindings {
            document["environmentProviders"] = bindings;
        }
        std::fs::write(
            platform.directory.path().join("universe.json"),
            document.to_string(),
        )
        .unwrap();
        platform.success(&["config", "apply", "-f", "universe.json"]);
        assert!(
            platform
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
    }
}

#[test]
fn platform_admin_can_reconcile_binding_on_the_resolved_universe_uuid() {
    let platform = Platform::with_platform_admin(true);
    std::fs::write(
        platform.directory.path().join("universe.json"),
        json!({"schemaVersion":1,"kind":"universe","handle":"existing-work","environmentProviders":[{"providerId":"incus","enabled":false}]}).to_string(),
    ).unwrap();
    platform.success(&["config", "apply", "-f", "universe.json"]);
    let requests = platform.requests.lock().unwrap();
    let writes: Vec<_> = requests
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].1,
        "/api/v1/universes/00000000-0000-0000-0000-000000000002/deployment-provider-bindings/incus"
    );
    assert_eq!(writes[0].2, json!({"enabled":false}));
}

#[test]
fn creator_configuration_is_applied_by_creation_without_a_second_lifecycle_write() {
    let platform = Platform::start();
    std::fs::write(
        platform.directory.path().join("universe.json"),
        json!({"schemaVersion":1,"kind":"universe","handle":"new-work","displayName":"New work","protected":false,"metadata":{"purpose":"automation"}}).to_string(),
    ).unwrap();
    platform.success(&[
        "--organization",
        "organization-a",
        "config",
        "apply",
        "-f",
        "universe.json",
    ]);
    let requests = platform.requests.lock().unwrap();
    let writes: Vec<_> = requests
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "POST");
    assert_eq!(
        writes[0].1,
        "/api/v1/platform/organizations/organization-a/universes"
    );
    assert_eq!(writes[0].2["name"], "New work");
    assert_eq!(writes[0].2["protected"], false);
    assert_eq!(writes[0].2["metadata"], json!({"purpose":"automation"}));
}

#[test]
fn owner_configuration_updates_do_not_resubmit_unchanged_protection() {
    let platform = Platform::start();
    std::fs::write(
        platform.directory.path().join("universe.json"),
        json!({"schemaVersion":1,"kind":"universe","handle":"existing-work","protected":false,"metadata":{"purpose":"automation"}}).to_string(),
    ).unwrap();
    platform.success(&["config", "apply", "-f", "universe.json"]);
    let requests = platform.requests.lock().unwrap();
    let writes: Vec<_> = requests
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].1,
        "/api/v1/universes/00000000-0000-0000-0000-000000000002/configuration"
    );
    assert_eq!(writes[0].2, json!({"metadata":{"purpose":"automation"}}));
}
