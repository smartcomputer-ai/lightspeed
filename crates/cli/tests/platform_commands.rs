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

/// What declarative documents create, kept across the commands of a test.
#[derive(Default)]
struct State {
    organizations: Vec<Value>,
    service_users: Vec<Value>,
    universes: Vec<Value>,
    policies: std::collections::BTreeMap<String, Value>,
    grants: Vec<Value>,
    /// Declared keys: (service user ID, key name) to the last put.
    keys: std::collections::BTreeMap<(String, String), Value>,
}

/// Organizations, service users, and the universes, policies and grants that
/// documents declare. `None` leaves the request to the fixed routes.
fn stateful(state: &mut State, method: &str, path: &str, body: &Value) -> Option<(u16, Value)> {
    let not_found = || {
        (
            404,
            json!({"type":"about:blank","title":"Not found","detail":"not found","status":404,"kind":"notFound"}),
        )
    };
    let parts: Vec<&str> = path.trim_start_matches("/api/v1/").split('/').collect();
    match (method, parts.as_slice()) {
        ("POST", ["organizations", "enroll"]) => {
            let id = format!("org-{}", body["handle"].as_str()?);
            if state.organizations.iter().any(|org| org["id"] == id) {
                return Some((200, json!({"organizationId":id, "enrolled":false})));
            }
            stateful(state, "POST", "/api/v1/platform/organizations", body)?;
            Some((201, json!({"organizationId":id, "enrolled":true})))
        }
        (
            "PUT",
            [
                "platform",
                "organizations",
                org,
                "service-users",
                user,
                "federated-credentials",
            ],
        ) => {
            let service = state
                .service_users
                .iter_mut()
                .find(|s| s["id"] == *user && s["organizationId"] == *org)?;
            let entries = service
                .as_object_mut()?
                .entry("federatedCredentials")
                .or_insert(json!([]))
                .as_array_mut()?;
            entries.retain(|e| e["issuer"] != body["issuer"] || e["subject"] != body["subject"]);
            entries.push(body.clone());
            Some((200, json!({"credential":body})))
        }
        ("GET", ["platform", "organizations"]) => Some((
            200,
            json!({"organizations": state.organizations, "canCreate": true}),
        )),
        ("POST", ["platform", "organizations"]) => {
            let organization = json!({
                "id": format!("org-{}", body["handle"].as_str().unwrap()), "handle": body["handle"], "name": body["name"], "personal": false,
                "baseUniverseRole": body.get("baseUniverseRole").cloned().unwrap_or(json!("viewer")),
                "defaultProtected": body.get("defaultProtected").cloned().unwrap_or(json!(true)),
                "requireProtected": body.get("requireProtected").cloned().unwrap_or(json!(false)),
                "universeQuota": 100, "universeQuotaSource": "deployment", "membershipPolicy": {},
            });
            let mut organization = organization;
            if let Some(quota) = body.get("universeQuota").filter(|quota| !quota.is_null()) {
                organization["universeQuota"] = quota.clone();
                organization["universeQuotaSource"] = json!("organization");
            }
            state.organizations.push(organization.clone());
            Some((200, json!({"organization": organization})))
        }
        ("PATCH", ["platform", "organizations", id]) => {
            let organization = state
                .organizations
                .iter_mut()
                .find(|candidate| candidate["id"] == *id)?;
            for (field, value) in body.as_object().unwrap() {
                organization[field] = value.clone();
            }
            Some((200, json!({"organization": organization})))
        }
        ("GET", ["platform", "organizations", id, "service-users"]) => {
            let listed: Vec<&Value> = state
                .service_users
                .iter()
                .filter(|candidate| candidate["organizationId"] == *id)
                .collect();
            Some((200, json!({"serviceUsers": listed})))
        }
        ("POST", ["platform", "organizations", id, "service-users"]) if id.starts_with("org-") => {
            let service_user = json!({"id": format!("su-{}", body["name"].as_str().unwrap()), "organizationId": id, "name": body["name"], "role": body["role"], "enabled": true});
            state.service_users.push(service_user.clone());
            Some((200, json!({"serviceUser": service_user})))
        }
        ("PATCH", ["platform", "organizations", _, "service-users", id]) => {
            let service_user = state
                .service_users
                .iter_mut()
                .find(|candidate| candidate["id"] == *id)?;
            for (field, value) in body.as_object().unwrap() {
                service_user[field] = value.clone();
            }
            Some((200, json!({"serviceUser": service_user})))
        }
        ("POST", ["platform", "organizations", id, "universes"]) if id.starts_with("org-") => {
            let universe = json!({"lightspeedUniverseId": body["universeId"], "handle": body["handle"], "organizationId": id});
            state.universes.push(universe.clone());
            Some((200, json!({"universe": universe})))
        }
        ("GET", ["universes", reference])
            if reference.starts_with("declared-")
                || state
                    .universes
                    .iter()
                    .any(|universe| universe["lightspeedUniverseId"] == *reference) =>
        {
            match state.universes.iter().find(|universe| {
                universe["handle"] == *reference || universe["lightspeedUniverseId"] == *reference
            }) {
                Some(universe) => Some((
                    200,
                    json!({"universe": {"universeId": universe["lightspeedUniverseId"], "handle": universe["handle"], "protected": true}}),
                )),
                None => Some(not_found()),
            }
        }
        (
            "PUT",
            [
                "platform",
                "organizations",
                _,
                "service-users",
                _,
                "credentials",
            ],
        ) if body["name"] == "forbidden" => Some((
            403,
            json!({"type":"about:blank","title":"Forbidden","detail":"Declare service-user keys from a human session","status":403,"kind":"forbidden"}),
        )),
        (
            "PUT",
            [
                "platform",
                "organizations",
                _,
                "service-users",
                service_user,
                "credentials",
            ],
        ) => {
            let index = (
                (*service_user).to_owned(),
                body["name"].as_str().unwrap().to_owned(),
            );
            let current = state.keys.get(&index).cloned();
            let mut updated = Vec::new();
            if let Some(current) = &current {
                if current.get("restrictions") != body.get("restrictions") {
                    updated.push("restrictions");
                }
                if current.get("expiresAt") != body.get("expiresAt") {
                    updated.push("expiresAt");
                }
            }
            let outcome = json!({
                "credential": current.as_ref().map(|_| json!({"id": "declared-key"})),
                "created": current.is_none(),
                "secretReplaced": current.as_ref().is_some_and(|current| current["secret"] != body["secret"]),
                "updated": updated,
            });
            if body["dryRun"] != true {
                let mut stored = body.clone();
                stored.as_object_mut().unwrap().remove("dryRun");
                state.keys.insert(index, stored);
            }
            Some((200, outcome))
        }
        ("POST", ["platform", "credentials", key, "replace"]) => Some((
            200,
            json!({"credential": {"id": key}, "secret": "lsp_replaced"}),
        )),
        ("GET", ["platform", "universes", id]) => {
            let universe = state
                .universes
                .iter()
                .find(|universe| universe["lightspeedUniverseId"] == *id)?;
            Some((200, json!({"universe": universe})))
        }
        ("GET", ["platform", "universes", id, "access-policy"]) => Some((
            200,
            json!({"policy": state.policies.get(*id).cloned().unwrap_or(json!({}))}),
        )),
        ("PUT", ["platform", "universes", id, "access-policy"]) => {
            state
                .policies
                .insert((*id).to_owned(), body["policy"].clone());
            Some((200, json!({"policy": body["policy"]})))
        }
        ("GET", ["platform", "universes", id, "grants"]) => {
            let listed: Vec<&Value> = state
                .grants
                .iter()
                .filter(|grant| grant["universeId"] == *id)
                .collect();
            Some((200, json!({"grants": listed})))
        }
        ("PUT", ["platform", "universes", id, "grants"]) => {
            state.grants.retain(|grant| {
                !(grant["universeId"] == *id && grant["principal"] == body["principal"])
            });
            let grant = json!({"id": "grant", "universeId": id, "principal": body["principal"], "role": body["role"]});
            state.grants.push(grant.clone());
            Some((200, json!({"grant": grant})))
        }
        _ => None,
    }
}

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
            let mut state = State::default();
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
                let fixed = stateful(&mut state, &method, &path, &body);
                let (status, response) = if let Some(response) = fixed {
                    response
                } else {
                    match (method.as_str(), path.as_str()) {
                        (_, "/api/v1/connection") => (
                            200,
                            json!({"runtime":"platform","contractRevision":"1","caller":{"single":false},"serverInfo":{"name":"Platform","version":"test"},"platform":{"principal":{"kind":"user","id":"creator","name":"Creator","platformAdmin":platform_admin},"credential":{"kind":"personalKey","id":"key-id"},"organizations":[],"restrictions":{}}}),
                        ),
                        ("GET", "/api/v1/platform/organizations/organization-a") => {
                            (200, json!({"organization":{"id":"organization-a"}}))
                        }
                        ("PUT", "/api/v1/platform/organizations/organization-a/members/user-b") => {
                            (
                                200,
                                json!({"member":{"userId":"user-b","role":body["role"]}}),
                            )
                        }
                        ("POST", "/api/v1/platform/organizations/organization-a/service-users") => {
                            (
                                200,
                                json!({"serviceUser":{"id":"worker","name":body["name"],"role":body["role"]}}),
                            )
                        }
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
                            "/api/v1/universes/00000000-0000-0000-0000-000000000002/environment-providers",
                        ) => (
                            200,
                            json!({"providers":[{"providerId":"incus","availableToAllUniverses":false}]}),
                        ),
                        (
                            "PUT",
                            "/api/v1/universes/00000000-0000-0000-0000-000000000002/deployment-provider-bindings/gpu",
                        ) => (
                            200,
                            json!({"binding":{"providerId":"gpu","createdAtMs":1,"updatedAtMs":1}}),
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
                    }
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
            json!({"schemaVersion":1,"kind":"universe","handle":"existing-work","environmentProviders":[{"providerId":"gpu"}]}),
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
        json!({"schemaVersion":1,"kind":"universe","handle":"new-work","environmentProviders":[{"providerId":"gpu"}]}).to_string(),
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
    for bindings in [None, Some(json!([])), Some(json!([{"providerId":"incus"}]))] {
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
        json!({"schemaVersion":1,"kind":"universe","handle":"existing-work","environmentProviders":[{"providerId":"gpu"}]}).to_string(),
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
        "/api/v1/universes/00000000-0000-0000-0000-000000000002/deployment-provider-bindings/gpu"
    );
    assert_eq!(writes[0].2, json!({}));
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

fn write_documents(platform: &Platform, files: &[(&str, &str)]) -> String {
    let directory = platform.directory.path().join("documents");
    std::fs::create_dir_all(&directory).unwrap();
    for (name, text) in files {
        std::fs::write(directory.join(name), text).unwrap();
    }
    directory.to_string_lossy().into_owned()
}

const ORGANIZATION_DOCUMENT: &str = "schemaVersion: 1
kind: organization
handle: research
name: Research
baseUniverseRole: null
universeQuota: 20
membershipPolicy:
  directoryGroupMappings:
    - { issuer: \"https://id.example\", group: research, role: member }
serviceUsers:
  - { name: deployer, role: creator }
";

#[test]
fn documents_declare_an_organization_and_a_universe_with_its_access_in_one_run() {
    let platform = Platform::with_platform_admin(true);
    let documents = write_documents(
        &platform,
        &[
            ("research.yaml", ORGANIZATION_DOCUMENT),
            (
                "work.yaml",
                "schemaVersion: 1
kind: universe
handle: declared-work
organization: research
access:
  directoryGroupMappings:
    - { issuer: \"https://id.example\", group: research-ops, role: operator }
  serviceUsers:
    - { name: deployer, role: owner }
",
            ),
        ],
    );
    platform.success(&["config", "apply", "-f", &documents]);
    let writes: Vec<(String, String, Value)> = platform
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .cloned()
        .collect();
    let paths: Vec<(&str, &str)> = writes
        .iter()
        .map(|(method, path, _)| (method.as_str(), path.as_str()))
        .collect();
    // The organization and its service user come first, so the universe and
    // its grant find them.
    assert_eq!(
        paths,
        [
            ("POST", "/api/v1/platform/organizations"),
            ("PATCH", "/api/v1/platform/organizations/org-research"),
            (
                "POST",
                "/api/v1/platform/organizations/org-research/service-users"
            ),
            (
                "POST",
                "/api/v1/platform/organizations/org-research/universes"
            ),
            (
                "PUT",
                &*format!(
                    "/api/v1/platform/universes/{}/access-policy",
                    writes[3].2["universeId"].as_str().unwrap()
                )
            ),
            (
                "PUT",
                &*format!(
                    "/api/v1/platform/universes/{}/grants",
                    writes[3].2["universeId"].as_str().unwrap()
                )
            ),
        ]
    );
    assert_eq!(
        writes[0].2,
        json!({"name": "Research", "handle": "research", "baseUniverseRole": null, "universeQuota": 20})
    );
    assert_eq!(
        writes[1].2["membershipPolicy"]["directoryGroupMappings"][0]["group"],
        "research"
    );
    assert_eq!(writes[2].2, json!({"name": "deployer", "role": "creator"}));
    assert_eq!(
        writes[5].2,
        json!({"principal": {"kind": "serviceUser", "id": "su-deployer"}, "role": "owner"})
    );
    drop(writes);
    // Applied, the documents describe what is there.
    let diff = platform.run(&["config", "diff", "-f", &documents]);
    assert!(
        diff.status.success(),
        "{}{}",
        String::from_utf8_lossy(&diff.stdout),
        String::from_utf8_lossy(&diff.stderr)
    );
}

#[test]
fn a_grant_to_an_undeclared_service_user_is_refused_before_anything_is_written() {
    let platform = Platform::with_platform_admin(true);
    let documents = write_documents(
        &platform,
        &[
            ("research.yaml", ORGANIZATION_DOCUMENT),
            (
                "work.yaml",
                "schemaVersion: 1
kind: universe
handle: declared-work
organization: research
access:
  serviceUsers:
    - { name: ghost, role: viewer }
",
            ),
        ],
    );
    let result = platform.run(&["config", "apply", "-f", &documents]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("no service user named ghost"));
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
fn only_a_platform_admin_sets_an_organization_universe_limit() {
    let platform = Platform::start();
    let documents = write_documents(&platform, &[("research.yaml", ORGANIZATION_DOCUMENT)]);
    let result = platform.run(&["config", "apply", "-f", &documents]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains("only a Platform Admin sets a universe limit")
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
fn an_exported_organization_applies_without_changes() {
    let platform = Platform::with_platform_admin(true);
    let documents = write_documents(&platform, &[("research.yaml", ORGANIZATION_DOCUMENT)]);
    platform.success(&["config", "apply", "-f", &documents]);
    let exported = platform.success(&[
        "--organization",
        "org-research",
        "config",
        "export",
        "--kind",
        "organization",
    ]);
    let document: Value = serde_yaml::from_str(&exported).unwrap();
    assert_eq!(document["kind"], "organization");
    assert_eq!(document["handle"], "research");
    assert_eq!(document["universeQuota"], 20);
    assert_eq!(
        document["serviceUsers"],
        json!([{"name": "deployer", "role": "creator"}])
    );
    let reexported = write_documents(&platform, &[("research.yaml", &exported)]);
    let diff = platform.run(&["config", "diff", "-f", &reexported]);
    assert!(
        diff.status.success(),
        "{}",
        String::from_utf8_lossy(&diff.stdout)
    );
}

#[test]
fn a_declared_service_user_key_takes_its_secret_from_outside_and_a_changed_secret_replaces_it() {
    let platform = Platform::with_platform_admin(true);
    let documents = write_documents(
        &platform,
        &[(
            "research.yaml",
            "schemaVersion: 1
kind: organization
handle: research
name: Research
serviceUsers:
  - name: deployer
    role: creator
    keys:
      - { name: ci, secretFrom: { env: DEPLOYER_KEY }, restrictions: { readOnly: true } }
",
        )],
    );
    let first = format!("lsa_{}", "a".repeat(64));
    let run = |args: &[&str], secret: &str| {
        Command::new(env!("CARGO_BIN_EXE_lightspeed"))
            .current_dir(platform.directory.path())
            .args(args)
            .env(
                "LIGHTSPEED_CONFIG_DIR",
                platform.directory.path().join("config"),
            )
            .env("LIGHTSPEED_API_URL", &platform.endpoint)
            .env("LIGHTSPEED_API_TOKEN", "lsk_fixture")
            .env("DEPLOYER_KEY", secret)
            .output()
            .unwrap()
    };
    let applied = run(&["config", "apply", "-f", &documents], &first);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let put = platform
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|(method, path, _)| {
            method == "PUT" && path.ends_with("/service-users/su-deployer/credentials")
        })
        .cloned()
        .expect("the key was put");
    assert_eq!(
        put.2,
        json!({"name": "ci", "secret": first, "restrictions": {"readOnly": true}})
    );
    // The same secret again: nothing to do.
    assert!(
        run(&["config", "diff", "-f", &documents], &first)
            .status
            .success()
    );
    // A rotated secret in the secret store replaces the key's secret in place.
    let rotated = run(
        &["config", "diff", "-f", &documents],
        &format!("lsa_{}", "b".repeat(64)),
    );
    assert_eq!(rotated.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rotated.stdout).contains("keys/deployer/ci (secret)"));
}

#[test]
fn a_key_gets_a_new_secret_in_place_only_to_an_explicit_destination() {
    let platform = Platform::start();
    let refused = platform.run(&["platform", "api-key", "replace", "key-id"]);
    assert!(!refused.status.success());
    let shown = platform.success(&["platform", "api-key", "replace", "key-id", "--show-secret"]);
    assert!(shown.contains("lsp_replaced"));
    assert!(
        platform
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path, _)| method == "POST"
                && path == "/api/v1/platform/credentials/key-id/replace")
    );
}

#[test]
fn a_service_user_key_secret_is_generated_without_a_connection() {
    let output = Command::new(env!("CARGO_BIN_EXE_lightspeed"))
        .args(["platform", "api-key", "generate-secret"])
        .env_remove("LIGHTSPEED_API_URL")
        .env("LIGHTSPEED_CONFIG_DIR", tempfile::tempdir().unwrap().path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let secret = String::from_utf8(output.stdout).unwrap();
    assert!(secret.trim().starts_with("lsa_") && secret.trim().len() >= 68);
}

#[test]
fn a_connection_that_may_not_declare_keys_is_told_how_before_anything_is_written() {
    let platform = Platform::with_platform_admin(true);
    let documents = write_documents(
        &platform,
        &[(
            "research.yaml",
            "schemaVersion: 1
kind: organization
handle: research
name: Research
serviceUsers:
  - name: deployer
    role: creator
",
        )],
    );
    platform.success(&["config", "apply", "-f", &documents]);
    let documents = write_documents(
        &platform,
        &[(
            "research.yaml",
            "schemaVersion: 1
kind: organization
handle: research
name: Research
serviceUsers:
  - name: deployer
    role: creator
    keys:
      - { name: forbidden, secretFrom: { env: DEPLOYER_KEY } }
",
        )],
    );
    let writes_before = platform
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .count();
    let result = Command::new(env!("CARGO_BIN_EXE_lightspeed"))
        .current_dir(platform.directory.path())
        .args(["config", "apply", "-f", &documents])
        .env(
            "LIGHTSPEED_CONFIG_DIR",
            platform.directory.path().join("config"),
        )
        .env("LIGHTSPEED_API_URL", &platform.endpoint)
        .env("LIGHTSPEED_API_TOKEN", "lsk_fixture")
        .env("DEPLOYER_KEY", format!("lsa_{}", "a".repeat(64)))
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report = String::from_utf8_lossy(&result.stdout);
    assert!(report.contains("--allow-service-user-keys"), "{report}");
    let requests = platform.requests.lock().unwrap();
    // Only the dry run was sent: nothing was written.
    let writes: Vec<_> = requests
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .skip(writes_before)
        .collect();
    assert!(
        writes.iter().all(|(_, _, body)| body["dryRun"] == true),
        "{writes:?}"
    );
}

#[test]
fn a_pipeline_key_is_created_to_declare_service_user_keys_only_when_asked() {
    let platform = Platform::start();
    platform.success(&[
        "platform",
        "api-key",
        "create",
        "Pipeline",
        "--allow-service-user-keys",
        "--show-secret",
    ]);
    let requests = platform.requests.lock().unwrap();
    let (_, _, body) = requests
        .iter()
        .find(|(method, path, _)| method == "POST" && path == "/api/v1/platform/credentials")
        .unwrap();
    assert_eq!(body["restrictions"]["allowServiceUserKeys"], true);
}

#[test]
fn enroll_precedes_discovery_and_federated_declarations_round_trip() {
    let platform = Platform::with_platform_admin(true);
    let file = write_documents(
        &platform,
        &[(
            "research.yaml",
            "schemaVersion: 1
kind: organization
handle: research
name: Research
serviceUsers:
  - name: reader
    role: member
    federatedCredentials:
      - issuer: cluster
        subject: research-reader
        restrictions: { readOnly: true }
",
        )],
    );
    let apply = ["config", "apply", "--enroll", "-f", &file, "--json"];
    platform.success(&apply);
    let requests = platform.requests.lock().unwrap().clone();
    assert_eq!(requests[0].1, "/api/v1/organizations/enroll");
    assert_eq!(requests[1].1, "/api/v1/connection");
    let steps: Vec<Value> = serde_json::from_str(&platform.success(&apply)).unwrap();
    assert!(steps.iter().all(|s| s["action"] == "unchanged"));
    let exported: Value = serde_yaml::from_str(&platform.success(&[
        "--organization",
        "org-research",
        "config",
        "export",
        "--kind",
        "organization",
    ]))
    .unwrap();
    assert_eq!(
        exported["serviceUsers"][0]["federatedCredentials"][0],
        json!({"issuer":"cluster","subject":"research-reader","restrictions":{"readOnly":true}})
    );
}

#[test]
fn enrollment_requires_exactly_one_organization_before_contacting_platform() {
    let platform = Platform::with_platform_admin(true);
    let doc = write_documents(
        &platform,
        &[(
            "a.yaml",
            "schemaVersion: 1\nkind: universe\nhandle: declared-work\n",
        )],
    );
    assert!(
        !platform
            .run(&["config", "apply", "--enroll", "-f", &doc])
            .status
            .success()
    );
    assert!(platform.requests.lock().unwrap().is_empty());
}
