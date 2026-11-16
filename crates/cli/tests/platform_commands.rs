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
                        json!({"runtime":"platform","contractRevision":"1","caller":{"single":false},"serverInfo":{"name":"Platform","version":"test"},"platform":{"principal":{"kind":"user","id":"creator","name":"Creator"},"credential":{"kind":"personalKey","id":"key-id"},"accounts":[],"restrictions":{}}}),
                    ),
                    ("GET", "/api/v1/platform/accounts/account-a") => {
                        (200, json!({"account":{"id":"account-a"}}))
                    }
                    ("GET", "/api/v1/platform/accounts") => {
                        (200, json!({"accounts":[],"canCreate":true}))
                    }
                    ("PUT", "/api/v1/platform/accounts/account-a/members/user-b") => (
                        200,
                        json!({"member":{"userId":"user-b","role":body["role"]}}),
                    ),
                    ("POST", "/api/v1/platform/accounts/account-a/universes") => (
                        200,
                        json!({"universe":{"lightspeedUniverseId":body["universeId"],"handle":body["handle"],"accountId":"account-a"}}),
                    ),
                    ("GET", "/api/v1/universes/new-work") => (
                        404,
                        json!({"type":"about:blank","title":"Not found","detail":"not found","status":404,"kind":"notFound"}),
                    ),
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
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn account_members_use_native_routes_and_path_owned_identifiers() {
    let platform = Platform::start();
    platform.success(&[
        "--account",
        "account-a",
        "platform",
        "account",
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
        "/api/v1/platform/accounts/account-a/members/user-b"
    );
    assert_eq!(request.2, json!({"role":"creator"}));
}
#[test]
fn platform_creation_uses_selected_account_and_explicit_owner() {
    let platform = Platform::start();
    platform.success(&[
        "--account",
        "account-a",
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
    assert_eq!(request.1, "/api/v1/platform/accounts/account-a/universes");
    assert_eq!(
        request.2["initialOwner"],
        json!({"kind":"user","id":"creator"})
    );
    assert_eq!(request.2["metadata"], json!({"team":"blue"}));
    assert!(
        request.2.get("protected").is_none(),
        "account default remains authoritative"
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
        "--account",
        "account-a",
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
    assert_eq!(request.1, "/api/v1/platform/accounts/account-a/universes");
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
        "--account",
        "account-a",
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
