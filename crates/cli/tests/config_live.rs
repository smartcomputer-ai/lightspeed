//! Live coverage for `config export`, `config diff` and `config apply`
//! against a real runtime.
//!
//! The suite needs an authenticated runtime and a deployment key that may
//! create universes and write every resource a document holds:
//!
//! ```bash
//! LIGHTSPEED_CONFIG_LIVE_API_URL=http://127.0.0.1:18080/rpc \
//! LIGHTSPEED_CONFIG_LIVE_API_KEY_FILE=/path/to/deployment-key \
//!   cargo test -p cli --test config_live -- --ignored --test-threads=1
//! ```
//!
//! It creates two universes with unique handles and deletes them again.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};

struct Live {
    api_url: String,
    api_key: String,
    directory: tempfile::TempDir,
}

impl Live {
    fn from_env() -> Self {
        let api_url = std::env::var("LIGHTSPEED_CONFIG_LIVE_API_URL").expect(
            "LIGHTSPEED_CONFIG_LIVE_API_URL must name the JSON-RPC endpoint of a running, authenticated runtime",
        );
        let key_file = std::env::var("LIGHTSPEED_CONFIG_LIVE_API_KEY_FILE").expect(
            "LIGHTSPEED_CONFIG_LIVE_API_KEY_FILE must name a file holding a deployment API key",
        );
        let api_key = std::fs::read_to_string(&key_file)
            .unwrap_or_else(|error| panic!("read {key_file}: {error}"))
            .trim()
            .to_owned();
        Self {
            api_url,
            api_key,
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn run(&self, args: &[&str], secrets: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lightspeed"));
        command
            .current_dir(self.directory.path())
            .args(args)
            .env("LIGHTSPEED_CONFIG_DIR", self.path("cli-config"))
            .env("LIGHTSPEED_API_URL", &self.api_url)
            .env("LIGHTSPEED_API_KEY", &self.api_key)
            .env_remove("LIGHTSPEED_UNIVERSE");
        for (name, value) in secrets {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    /// Run a command that must succeed and return what it printed.
    fn ok(&self, args: &[&str], secrets: &[(&str, &str)]) -> String {
        let output = self.run(args, secrets);
        assert!(
            output.status.success(),
            "{args:?} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn diff_status(&self, file: &Path, secrets: &[(&str, &str)]) -> (Option<i32>, String) {
        let output = self.run(&["config", "diff", "-f", file.to_str().unwrap()], secrets);
        (
            output.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    fn export(&self, handle: &str) -> Value {
        serde_json::from_str(&self.ok(
            &["--universe", handle, "config", "export", "--format", "json"],
            &[],
        ))
        .expect("export prints one JSON document")
    }

    async fn call(&self, method: &str, params: Value) -> Value {
        let response: Value = reqwest::Client::new()
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .json(&json!({ "id": 1, "method": method, "params": params }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"]["result"].clone()
    }

    async fn delete_universe(&self, handle: &str) {
        self.call("deployment/universes/delete", json!({ "universe": handle }))
            .await;
    }
}

fn document(handle: &str) -> String {
    format!(
        r#"schemaVersion: 1
kind: universe
handle: {handle}
credentials:
  - grantId: tracker-token
    displayName: Issue tracker
    tokenFrom: {{ env: LIVE_TRACKER_TOKEN }}
providers:
  - providerId: model:primary
    displayName: Primary models
    config:
      type: modelApiKey
      endpoint:
        baseUrl: http://127.0.0.1:11434/v1
        apiKinds: ["openai:completions"]
    credentialFrom: {{ file: model-key }}
modelDefaults:
  agentRun: {{ providerId: primary, apiKind: "openai:completions", model: large-1 }}
mcpServers:
  - serverId: tracker
    displayName: Issue tracker
    serverUrl: https://tracker.example.com/mcp
    defaultServerLabel: tracker
    headers:
      x-upstream-token: {{ type: credential, grantId: tracker-token }}
registrationKeys:
  - registrationKeyId: runners
    displayName: Runners
    identityMode: persistent
    declaredOnly: true
    maxActiveEnvironments: 2
    secretFrom: {{ env: LIVE_RUNNERS_KEY }}
environments:
  - environmentId: build-box
    displayName: Build box
    metadata: {{ team: build }}
    source: {{ type: registered, registrationKeyId: runners, name: lightspeed-runner-0 }}
    credentials:
      - envName: TRACKER_TOKEN
        source: {{ type: authGrant, grantId: tracker-token }}
  - environmentId: lab
    source: {{ type: external, endpoint: "wss://lab.example.com/envd" }}
profiles:
  - profileId: reviewer
    displayName: Reviewer
    instructions: {{ type: text, text: "Review the change and report risks." }}
    config:
      features:
        environments:
          version: 1
          environments:
            - {{ environmentId: build-box, default: true, access: read }}
  - file: triage.json
apiKeys:
  - keyId: release-bot
    displayName: Release bot
    groups: [session, profiles]
    secretFrom: {{ env: LIVE_RELEASE_BOT_KEY }}
"#
    )
}

fn generate(live: &Live, kind: &str) -> String {
    let command: &[&str] = match kind {
        "api-key" => &["api-key", "generate-secret"],
        _ => &["environment", "registration-key", "generate-secret"],
    };
    live.ok(command, &[]).trim().to_owned()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a running authenticated runtime; see the module documentation"]
async fn a_universe_is_declared_converged_and_promoted_to_another_universe() {
    let live = Live::from_env();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let source = format!("config-live-a-{}", &suffix[..10]);
    let promoted = format!("config-live-b-{}", &suffix[..10]);
    let file = live.path("universe.yaml");
    std::fs::write(&file, document(&source)).unwrap();
    std::fs::write(
        live.path("triage.json"),
        r#"{ "profileId": "triage", "displayName": "Triage", "description": "Sorts incoming reports" }"#,
    )
    .unwrap();
    std::fs::write(live.path("model-key"), "model-secret-1\n").unwrap();
    let runners_key = generate(&live, "registration-key");
    let bot_key = generate(&live, "api-key");
    let secrets = [
        ("LIVE_TRACKER_TOKEN", "tracker-secret-1"),
        ("LIVE_RUNNERS_KEY", runners_key.as_str()),
        ("LIVE_RELEASE_BOT_KEY", bot_key.as_str()),
    ];
    let file_arg = file.to_str().unwrap();

    // A new universe: everything is created, and nothing was written by
    // looking.
    let (status, plan) = live.diff_status(&file, &secrets);
    assert_eq!(status, Some(2), "{plan}");
    assert!(
        plan.contains(&format!("create      universe/{source}")),
        "{plan}"
    );
    let applied = live.ok(&["config", "apply", "-f", file_arg], &secrets);
    for resource in [
        "credentials/tracker-token",
        "providers/model:primary",
        "modelDefaults/agentRun",
        "mcpServers/tracker",
        "registrationKeys/runners",
        "environments/build-box",
        "environments/lab",
        "environments.credentials/build-box/TRACKER_TOKEN",
        "profiles/reviewer",
        "profiles/triage",
        "apiKeys/release-bot",
    ] {
        assert!(
            applied.contains(&format!("created     {resource}")),
            "{resource} was not created:\n{applied}"
        );
    }
    for secret in ["tracker-secret-1", "model-secret-1", &runners_key, &bot_key] {
        assert!(!applied.contains(secret), "a secret reached the output");
    }

    // The same document again: no change, by diff and by apply.
    let (status, plan) = live.diff_status(&file, &secrets);
    assert_eq!(status, Some(0), "{plan}");
    let again = live.ok(&["config", "apply", "-f", file_arg], &secrets);
    assert!(again.contains("11 unchanged"), "{again}");

    // The declared environment waits for its machine under its chosen id,
    // and the profile already points at it.
    let environments = live.call_as(&source, "environments/list", json!({})).await;
    let build_box = environments["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|environment| environment["environmentId"] == "build-box")
        .expect("declared environment")
        .clone();
    assert_eq!(build_box["status"], "away");
    assert_eq!(build_box["source"]["name"], "lightspeed-runner-0");

    // An edit made outside the document is reverted by the next apply, and
    // a changed secret is replaced.
    live.call_as(
        &source,
        "mcp/servers/put",
        json!({ "server": {
            "serverId": "tracker",
            "displayName": "Edited by hand",
            "serverUrl": "https://tracker.example.com/mcp",
            "defaultServerLabel": "tracker",
        }}),
    )
    .await;
    let rotated = [
        ("LIVE_TRACKER_TOKEN", "tracker-secret-2"),
        ("LIVE_RUNNERS_KEY", runners_key.as_str()),
        ("LIVE_RELEASE_BOT_KEY", bot_key.as_str()),
    ];
    let (status, drift) = live.diff_status(&file, &rotated);
    assert_eq!(status, Some(2), "{drift}");
    assert!(
        drift.contains("update      credentials/tracker-token (secret)"),
        "{drift}"
    );
    assert!(
        drift.contains("update      mcpServers/tracker (displayName, headers)"),
        "{drift}"
    );
    live.ok(&["config", "apply", "-f", file_arg], &rotated);
    let (status, plan) = live.diff_status(&file, &rotated);
    assert_eq!(status, Some(0), "{plan}");

    // A setting that is fixed once the resource exists is a conflict, and a
    // document with a conflict writes nothing at all.
    let conflicting = live.path("conflict.yaml");
    std::fs::write(
        &conflicting,
        document(&source)
            .replace("name: lightspeed-runner-0", "name: lightspeed-runner-9")
            .replace("displayName: Reviewer", "displayName: Must not be written"),
    )
    .unwrap();
    let refused = live.run(
        &["config", "apply", "-f", conflicting.to_str().unwrap()],
        &rotated,
    );
    assert!(!refused.status.success());
    let refusal = String::from_utf8_lossy(&refused.stdout).into_owned();
    assert!(
        refusal.contains("conflict    environments/build-box"),
        "{refusal}"
    );
    let (status, plan) = live.diff_status(&file, &rotated);
    assert_eq!(
        status,
        Some(0),
        "the conflicting document wrote something:\n{plan}"
    );

    // Promotion: the exported document, with its secrets bound, builds a
    // second universe that exports to the same configuration.
    let exported = live.export(&source);
    assert_eq!(exported["handle"], source.as_str());
    assert_eq!(
        exported["credentials"][0]["tokenFrom"],
        json!({ "env": "TRACKER_TOKEN" })
    );
    assert!(
        !exported.to_string().contains("tracker-secret-2"),
        "an export never holds a secret"
    );
    let mut promoted_document = exported.clone();
    promoted_document["handle"] = json!(promoted);
    let promoted_file = live.path("promoted.json");
    std::fs::write(
        &promoted_file,
        serde_json::to_string_pretty(&promoted_document).unwrap(),
    )
    .unwrap();
    let promoted_runners_key = generate(&live, "registration-key");
    let promoted_bot_key = generate(&live, "api-key");
    let bound = [
        ("TRACKER_TOKEN", "tracker-secret-2"),
        ("MODEL_PRIMARY_CREDENTIAL", "model-secret-1"),
        ("RUNNERS_REGISTRATION_KEY", promoted_runners_key.as_str()),
        ("RELEASE_BOT_API_KEY", promoted_bot_key.as_str()),
    ];
    live.ok(
        &["config", "apply", "-f", promoted_file.to_str().unwrap()],
        &bound,
    );
    let (status, plan) = live.diff_status(&promoted_file, &bound);
    assert_eq!(status, Some(0), "{plan}");
    let mut promoted_export = live.export(&promoted);
    promoted_export["handle"] = json!(source);
    assert_eq!(promoted_export, exported);

    live.delete_universe(&source).await;
    live.delete_universe(&promoted).await;
}

impl Live {
    /// Call a universe method directly, as an operator editing by hand would.
    async fn call_as(&self, universe: &str, method: &str, params: Value) -> Value {
        let response: Value = reqwest::Client::new()
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .header("x-lightspeed-universe", universe)
            .json(&json!({ "id": 1, "method": method, "params": params }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"]["result"].clone()
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a running authenticated runtime; see the module documentation"]
async fn a_deployment_key_is_declared_and_its_fixed_settings_are_a_conflict() {
    let live = Live::from_env();
    let key_id = format!(
        "config-live-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    );
    let file = live.path("deployment.yaml");
    let document = |assert_actor: bool| {
        format!(
            "schemaVersion: 1\nkind: deployment\napiKeys:\n  - keyId: {key_id}\n    displayName: Config live\n    groups: [session]\n    assertActor: {assert_actor}\n    secretFrom: {{ file: key }}\n"
        )
    };
    std::fs::write(&file, document(false)).unwrap();
    std::fs::write(live.path("key"), generate(&live, "api-key")).unwrap();
    let file_arg = file.to_str().unwrap();

    let created = live.ok(&["config", "apply", "-f", file_arg], &[]);
    assert!(
        created.contains(&format!("created     apiKeys/{key_id}")),
        "{created}"
    );
    let (status, plan) = live.diff_status(&file, &[]);
    assert_eq!(status, Some(0), "{plan}");

    // Another secret under the same id replaces the stored one.
    std::fs::write(live.path("key"), generate(&live, "api-key")).unwrap();
    let (status, plan) = live.diff_status(&file, &[]);
    assert_eq!(status, Some(2), "{plan}");
    assert!(
        plan.contains(&format!("update      apiKeys/{key_id} (secret)")),
        "{plan}"
    );
    live.ok(&["config", "apply", "-f", file_arg], &[]);

    // Whether a key may assert an actor never changes.
    std::fs::write(&file, document(true)).unwrap();
    let refused = live.run(&["config", "apply", "-f", file_arg], &[]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("`assertActor` cannot be changed"),
        "{}",
        String::from_utf8_lossy(&refused.stdout)
    );

    // The deployment's export lists the key, and a revoked key is a
    // conflict from then on.
    let exported = live.ok(
        &["config", "export", "--deployment", "--format", "json"],
        &[],
    );
    assert!(exported.contains(&key_id), "{exported}");
    live.call(
        "deployment/api-keys/revoke",
        json!({ "scope": { "kind": "deployment" }, "keyId": key_id }),
    )
    .await;
    std::fs::write(&file, document(false)).unwrap();
    let (status, plan) = live.diff_status(&file, &[]);
    assert_eq!(status, Some(1), "{plan}");
    assert!(plan.contains("is revoked"), "{plan}");
    let exported = live.ok(
        &["config", "export", "--deployment", "--format", "json"],
        &[],
    );
    assert!(
        !exported.contains(&key_id),
        "a revoked key is not configuration"
    );
}
