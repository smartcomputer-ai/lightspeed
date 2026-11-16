//! A local HTTP fixture exercises the real binary without a browser or credentials.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const DEVICE: &str = "private-device-credential";
const SESSION: &str = "private-cli-session";
const RENEWED: &str = "renewed-cli-session";
#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Value,
    at: Instant,
}
struct Platform {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    directory: tempfile::TempDir,
}
impl Platform {
    fn start(replies: Vec<(u16, Value)>, foreign_approval: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/nested");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let worker = thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            while !stop.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(value) => value,
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
                let request = loop {
                    let read = socket.read(&mut buffer).unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let header = |name: &str| {
                            headers.lines().find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case(name)
                                    .then(|| value.trim().to_owned())
                            })
                        };
                        let size: usize = header("content-length")
                            .unwrap_or_else(|| "0".into())
                            .parse()
                            .unwrap();
                        if bytes.len() < end + 4 + size {
                            continue;
                        }
                        let mut first = headers.lines().next().unwrap().split_whitespace();
                        break Request {
                            method: first.next().unwrap().into(),
                            path: first.next().unwrap().into(),
                            authorization: header("authorization"),
                            body: if size == 0 {
                                json!({})
                            } else {
                                serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap()
                            },
                            at: Instant::now(),
                        };
                    }
                };
                recorded.lock().unwrap().push(request.clone());
                let (status, body, extra) = match (
                    request.method.as_str(),
                    request.path.split('?').next().unwrap(),
                ) {
                    ("POST", "/nested/api/auth/device/code") => (
                        200,
                        json!({"device_code": DEVICE,"user_code":"ABCD-EFGH","verification_uri":if foreign_approval { "https://outside.invalid/device".to_owned() } else { format!("{origin}/nested/device") },"expires_in":60,"interval":1}),
                        "",
                    ),
                    ("POST", "/nested/api/auth/device/token") => {
                        assert_eq!(request.body["device_code"], DEVICE);
                        assert_eq!(request.body["client_id"], "lightspeed-cli");
                        assert_eq!(
                            request.body["grant_type"],
                            "urn:ietf:params:oauth:grant-type:device_code"
                        );
                        let (status, body) = replies.pop_front().expect("unexpected extra poll");
                        (status, body, "")
                    }
                    ("GET", "/nested/api/v1/connection") => (
                        200,
                        json!({"runtime":"platform","contractRevision":api::HTTP_CONTRACT_REVISION,"caller":{"single":false},"platform":{"principal":{"kind":"user","id":"person","name":"Ada","platformAdmin":false},"credential":{"kind":"cliSession","id":"session"}}}),
                        "",
                    ),
                    // Renewal may accompany a denied operation; the following status/logout
                    // must use the replacement credential despite that operation failing.
                    ("GET", "/nested/api/v1/universes") => (
                        403,
                        json!({"type":"about:blank","title":"Forbidden","status":403,"kind":"forbidden","detail":"outside credential scope"}),
                        "set-auth-token: renewed-cli-session\r\n",
                    ),
                    ("DELETE", "/nested/api/v1/platform/session") => {
                        (200, json!({"revoked":true}), "")
                    }
                    _ => panic!("unexpected request {} {}", request.method, request.path),
                };
                let body = serde_json::to_vec(&body).unwrap();
                write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            }
        });
        Self {
            url,
            requests,
            stopping,
            worker: Some(worker),
            directory: tempfile::tempdir().unwrap(),
        }
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lightspeed"))
            .current_dir(self.directory.path())
            .args(args)
            .env(
                "LIGHTSPEED_CONFIG_DIR",
                self.directory.path().join("config"),
            )
            .env_remove("LIGHTSPEED_API_URL")
            .env_remove("LIGHTSPEED_API_TOKEN")
            .env_remove("LIGHTSPEED_UNIVERSE")
            .output()
            .unwrap()
    }
    fn login(&self) -> Output {
        self.cli(&[
            "connect",
            "login",
            "--url",
            &self.url,
            "--name",
            "team",
            "--no-browser",
        ])
    }
    fn config(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.directory.path().join("config/connections.json")).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Platform {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn token() -> (u16, Value) {
    (
        200,
        json!({"access_token":SESSION,"token_type":"Bearer","expires_in":3600,"scope":""}),
    )
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
fn headless_device_login_obeys_polling_persists_privately_renews_and_revokes_only_its_session() {
    let platform = Platform::start(
        vec![
            (400, json!({"error":"authorization_pending"})),
            (400, json!({"error":"slow_down"})),
            token(),
        ],
        false,
    );
    let output = success(platform.login());
    assert!(output.contains("ABCD-EFGH"));
    assert!(!output.contains(DEVICE));
    assert!(!output.contains(SESSION));
    let requests = platform.requests.lock().unwrap();
    let polls: Vec<_> = requests
        .iter()
        .filter(|r| r.path.ends_with("/device/token"))
        .collect();
    assert_eq!(polls.len(), 3);
    assert!(polls[2].at.duration_since(polls[1].at) >= Duration::from_secs(6));
    assert!(
        requests
            .iter()
            .all(|r| !r.path.contains(DEVICE) && !r.path.contains(SESSION))
    );
    drop(requests);
    let config = platform.config();
    let saved = &config["connections"]["team"];
    assert_eq!(config["selected"], "team");
    assert_eq!(saved["credentialKind"], "platformSession");
    assert_eq!(saved["endpoint"], format!("{}/api/v1", platform.url));
    assert!(!config.to_string().contains(SESSION));
    let credential = std::path::PathBuf::from(saved["credentialFile"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(&credential).unwrap(), SESSION);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let denied = platform.cli(&["universe", "list", "--json"]);
    assert!(!denied.status.success());
    assert_eq!(std::fs::read_to_string(&credential).unwrap(), RENEWED);
    let status = success(platform.cli(&["connect", "status", "--json"]));
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap()["principal"]["name"],
        "Ada"
    );
    success(platform.cli(&["connect", "logout"]));
    assert!(!credential.exists());
    assert!(platform.config()["connections"]["team"]["credentialFile"].is_null());
    let requests = platform.requests.lock().unwrap();
    let revoked: Vec<_> = requests.iter().filter(|r| r.method == "DELETE").collect();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].path, "/nested/api/v1/platform/session");
    assert_eq!(
        revoked[0].authorization.as_deref(),
        Some("Bearer renewed-cli-session")
    );
}

#[test]
fn denied_authorization_never_saves_or_prints_device_credentials() {
    let platform = Platform::start(
        vec![(
            400,
            json!({"error":"access_denied","error_description":DEVICE}),
        )],
        false,
    );
    let output = platform.login();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("denied"));
    assert!(!stderr.contains(DEVICE));
    assert!(
        !platform
            .directory
            .path()
            .join("config/connections.json")
            .exists()
    );
}

#[test]
fn foreign_approval_is_refused_before_polling_or_saving_any_session() {
    let platform = Platform::start(vec![token()], true);
    let output = platform.login();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("deployment's origin")
    );
    assert_eq!(platform.requests.lock().unwrap().len(), 1);
    assert!(
        !platform
            .directory
            .path()
            .join("config/connections.json")
            .exists()
    );
}
