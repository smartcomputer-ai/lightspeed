//! Authenticated HTTP and direct-service authorization over real PostgreSQL and
//! Temporal, with a fake model. Requires an explicitly selected disposable DB.
mod support;

use access::*;
use api::{AgentApiErrorKind, AgentApiService as _};
use auth::{ApiKeyStore as _, CreateApiKey, MintedApiKey};
use base64::Engine as _;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    config::GatewayAuthMode,
    gateway::{GatewayRoutes, GatewayState, gateway_router, principal::with_request_context},
};
use uuid::Uuid;

async fn rpc_response(
    url: &str,
    key: &MintedApiKey,
    method: &str,
    params: Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(url)
        .bearer_auth(key.secret.expose())
        .json(&json!({"id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
}
async fn rpc(url: &str, key: &MintedApiKey, method: &str, params: Value) -> Value {
    rpc_response(url, key, method, params)
        .await
        .json()
        .await
        .unwrap()
}
#[track_caller]
fn success(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    value["result"]["result"].clone()
}
#[track_caller]
fn forbidden(value: Value) {
    assert_eq!(value["error"]["data"]["kind"], "forbidden", "{value}");
}
#[track_caller]
fn not_found(value: Value) {
    assert_eq!(value["error"]["data"]["kind"], "not_found", "{value}");
}
fn lists(value: &Value, session: &str) -> bool {
    value["sessions"]
        .as_array()
        .expect("session list")
        .iter()
        .any(|s| s["id"] == session)
}
/// The error kind of a response, or `success`.
fn kind(value: &Value) -> &str {
    value["error"]["data"]["kind"].as_str().unwrap_or("success")
}
/// The refusal message of a failed call.
#[track_caller]
fn refusal(value: &Value) -> &str {
    value["error"]["data"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a refusal: {value}"))
}
/// Whether a list response names `id` in `field` of an item under `items`.
fn listed(value: &Value, items: &str, field: &str, id: &str) -> bool {
    value[items]
        .as_array()
        .unwrap_or_else(|| panic!("{items} list: {value}"))
        .iter()
        .any(|item| item[field] == id)
}
fn text_input(session: &str) -> Value {
    json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"hello"}]}})
}
async fn upload(url: &str, key: &MintedApiKey, bytes: &[u8]) -> Value {
    let body =
        json!({"blobs":[{"bytesBase64":base64::engine::general_purpose::STANDARD.encode(bytes)}]});
    success(rpc(url, key, "blobs/put", body).await)["blobs"][0]["blobRef"].clone()
}
/// A snapshot manifest of top-level files `(path, blob, size)`.
fn manifest(files: &[(&str, &Value, usize)]) -> Value {
    let entries: serde_json::Map<String, Value> = files
        .iter()
        .map(|(path, blob, size)| {
            (
                (*path).to_owned(),
                json!({"kind":"file","blob_ref":blob,"size_bytes":size,"executable":false}),
            )
        })
        .collect();
    let bytes: usize = files.iter().map(|(_, _, size)| size).sum();
    json!({"manifest":{"schema_version":"lightspeed.vfs.snapshot.v1","root":{"entries":entries},
        "totals":{"files":files.len(),"bytes":bytes}}})
}
/// Poll a run until it reaches a terminal status and return its view.
async fn terminal_run(url: &str, key: &MintedApiKey, session: &str, run: &str) -> Value {
    let read = json!({"sessionId":session,"runId":run});
    for _ in 0..300 {
        let view = success(rpc(url, key, "session/runs/read", read.clone()).await)["run"].clone();
        if matches!(
            view["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("run {run} of session {session} did not finish");
}

/// The shared universe's local development administrator, and one member per
/// requested role, each holding a universe-scoped key of its own.
async fn members(
    pool: &sqlx::PgPool,
    universe: Uuid,
    roles: &[Role],
) -> anyhow::Result<(Principal, Vec<MintedApiKey>)> {
    let access = PgAccessStore::new(pool.clone());
    // The host-only development fixture is explicit; HTTP still uses scoped keys.
    let admin = access.initialize_local_development(universe, 1).await?;
    let keys = PgApiKeyStore::new(pool.clone());
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let mut callers = Vec::new();
    for role in roles {
        let id = Uuid::new_v4();
        access
            .apply(
                admin.id,
                AccessChange::CreatePrincipal {
                    id,
                    kind: PrincipalKind::User,
                    management_scope: AccessScope::Deployment,
                    display_name: format!("Test {role:?}"),
                },
                2,
            )
            .await?;
        access
            .apply(
                admin.id,
                AccessChange::AssignRole {
                    assignment: RoleAssignment {
                        scope,
                        subject: Subject::Principal(id),
                        role: *role,
                    },
                },
                3,
            )
            .await?;
        callers.push(issue_key(&keys, scope, id, admin.id).await?);
    }
    Ok((admin, callers))
}
async fn issue_key(
    keys: &PgApiKeyStore,
    scope: AccessScope,
    principal: Uuid,
    created_by: Uuid,
) -> anyhow::Result<MintedApiKey> {
    let key = auth::mint_api_key(scope, principal, created_by, None, 4);
    keys.create_api_key(CreateApiKey {
        authority_scope: AccessScope::Deployment,
        key_hash: key.key_hash.clone(),
        record: key.record.clone(),
    })
    .await?;
    Ok(key)
}
/// The authenticated gateway over `runtime`, served on a free local port.
async fn serve(
    runtime: Arc<UniverseRuntime>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<std::io::Result<()>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/rpc", listener.local_addr()?);
    let router = gateway_router(
        Arc::new(GatewayState::multi(
            GatewayAuthMode::Authenticated,
            runtime,
            endpoint.clone(),
        )),
        1024 * 1024,
        GatewayRoutes {
            api: true,
            environment: false,
        },
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    Ok((endpoint, server))
}
async fn disposable_pool() -> anyhow::Result<(sqlx::PgPool, Uuid)> {
    let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")?;
    assert_eq!(
        std::env::var("LIGHTSPEED_POSTGRES_URL")?,
        url,
        "select the same disposable database for runtime and test"
    );
    let pool = sqlx::PgPool::connect(&url).await?;
    PgStore::migrate(&pool).await?;
    let universe = support::live::live_universe_id()?;
    PgStore::new(pool.clone(), store_pg::PgStoreConfig::new(universe))
        .ensure_universe()
        .await?;
    Ok((pool, universe))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; fake model; serialized"]
async fn authenticated_roles_ownership_and_direct_service_boundaries() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let (pool, universe) = disposable_pool().await?;
    let access = PgAccessStore::new(pool.clone());
    let keys = PgApiKeyStore::new(pool.clone());
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let (admin, callers) = members(
        &pool,
        universe,
        &[
            Role::Viewer,
            Role::Contributor,
            Role::Contributor,
            Role::Operator,
            Role::Admin,
        ],
    )
    .await?;
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, session_id| async move {
        let runtime = Arc::new(UniverseRuntime::new(client, queue, None, DeploymentStores::from_env().await?)?);
        let api = runtime.state_for(universe, false).await?.api.clone();
        let (endpoint, server) = serve(runtime).await?;
        let outcome = async {
            let [viewer, alice, bob, operator, universe_admin] = callers.as_slice() else { unreachable!() };
            // Every declared mutating/service route rejects a Viewer before decoding its body.
            for method in api::method_manifest() {
                if !matches!(method.access, api::MethodAccess::Universe(UniverseAction::Read)) {
                    forbidden(rpc(&endpoint, viewer, method.method, json!({})).await);
                }
            }
            let session = session_id.as_str();
            let start = json!({"sessionId":session});
            forbidden(rpc(&endpoint, viewer, "session/start", start.clone()).await);
            success(rpc(&endpoint, alice, "session/start", start.clone()).await);
            success(rpc(&endpoint, alice, "session/start", start.clone()).await);
            // A universe-visible session is shared work: every Contributor and
            // above works in it, not only its owner.
            for caller in [bob, operator, universe_admin] {
                success(rpc(&endpoint, caller, "session/rename", json!({"sessionId":session,"displayName":"shared"})).await);
                success(rpc(&endpoint, caller, "session/context/append", json!({"sessionId":session,"entries":[{"key":"team-note","item":{"type":"text","text":"shared context"}}]})).await);
            }
            success(rpc(&endpoint, bob, "session/runs/start", json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"joining in"}]}})).await);
            forbidden(rpc(&endpoint, viewer, "session/rename", json!({"sessionId":session,"displayName":"viewer"})).await);
            success(rpc(&endpoint, viewer, "session/read", json!({"sessionId":session})).await);
            success(rpc(&endpoint, viewer, "session/list", json!({})).await);
            forbidden(rpc(&endpoint, viewer, "blobs/put", json!({"blobs":[]})).await);

            // Sharing. A restricted session is absent to everyone but its owner,
            // Admin included, in reads and lists alike.
            let private = success(rpc(&endpoint, alice, "session/start", json!({"access":{"visibility":"restricted"}})).await)["session"]["id"].as_str().unwrap().to_owned();
            let resource = json!({"kind":"session","id":private});
            let read = json!({"sessionId":private});
            for caller in [viewer, bob, operator, universe_admin] {
                not_found(rpc(&endpoint, caller, "session/read", read.clone()).await);
                not_found(rpc(&endpoint, caller, "session/events/read", read.clone()).await);
                not_found(rpc(&endpoint, caller, "access/policy/read", json!({"resource":resource})).await);
                assert!(!lists(&success(rpc(&endpoint, caller, "session/list", json!({})).await), &private));
            }
            // Those whose role could delete learn nothing either; the Viewer's role
            // refuses first. Admin governs without reading: the delete is admitted and
            // then refused by state, since the session is still open.
            for caller in [bob, operator] {
                not_found(rpc(&endpoint, caller, "session/delete", read.clone()).await);
            }
            assert_eq!(rpc(&endpoint, universe_admin, "session/delete", read.clone()).await["error"]["data"]["kind"], "rejected");
            // Privileged reading. The universe Admin grants `read_private_content`
            // to the Viewer, a person; the Viewer then reads the restricted session,
            // is audited for exactly those reads, and loses them with the capability.
            let privileged_rows = |method: &'static str| {
                let pool = pool.clone();
                let viewer = viewer.record.principal_id;
                async move {
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM access_audit_events WHERE universe_id=$1 AND acting_principal_id=$2 AND ($3 = '' OR method=$3) AND privileged AND outcome='succeeded'")
                        .bind(universe).bind(viewer).bind(method).fetch_one(&pool).await
                }
            };
            let assignment = json!({"scope":{"kind":"universe","universeId":universe},"principalId":viewer.record.principal_id,"capability":"read_private_content"});
            forbidden(rpc(&endpoint, alice, "deployment/identity/apply", json!({"operation":"assign_capability","assignment":assignment})).await);
            success(rpc(&endpoint, universe_admin, "deployment/identity/apply", json!({"operation":"assign_capability","assignment":assignment})).await);
            // Never a service: privileged reading is a person's accountable act.
            let service = Uuid::new_v4();
            access.apply(admin.id, AccessChange::CreatePrincipal { id: service, kind: PrincipalKind::Service, management_scope: scope, display_name: "Exporter".into() }, 24).await?;
            assert!(matches!(
                access.apply(admin.id, AccessChange::AssignCapability { assignment: CapabilityAssignment { scope, principal_id: service, capability: Capability::ReadPrivateContent } }, 25).await,
                Err(AccessError::Invalid(_))
            ));
            let privileged_read = rpc_response(&endpoint, viewer, "session/read", read.clone()).await;
            assert_eq!(privileged_read.headers()["x-lightspeed-privileged-read"], "true");
            success(privileged_read.json().await?);
            success(rpc(&endpoint, viewer, "session/events/read", read.clone()).await);
            success(rpc(&endpoint, viewer, "access/policy/read", json!({"resource":resource})).await);
            assert!(lists(&success(rpc(&endpoint, viewer, "session/list", json!({})).await), &private));
            for method in ["session/read", "session/events/read", "access/policy/read", "session/list"] {
                assert_eq!(privileged_rows(method).await?, 1, "{method}");
            }
            // Reads the Viewer could make anyway leave no privileged row or UI marker.
            let ordinary_read = rpc_response(&endpoint, viewer, "session/read", json!({"sessionId":session})).await;
            assert!(!ordinary_read.headers().contains_key("x-lightspeed-privileged-read"));
            success(ordinary_read.json().await?);
            success(rpc(&endpoint, viewer, "session/list", json!({"metadata":{"absent":"yes"}})).await);
            assert_eq!(privileged_rows("").await?, 4);
            success(rpc(&endpoint, universe_admin, "deployment/identity/apply", json!({"operation":"revoke_capability","assignment":assignment})).await);
            not_found(rpc(&endpoint, viewer, "session/read", read.clone()).await);
            assert!(!lists(&success(rpc(&endpoint, viewer, "session/list", json!({})).await), &private));
            assert_eq!(privileged_rows("").await?, 4);
            assert!(lists(&success(rpc(&endpoint, alice, "session/list", json!({})).await), &private));
            let policy = success(rpc(&endpoint, alice, "access/policy/read", json!({"resource":resource})).await)["policy"].clone();
            assert_eq!(policy["visibility"], "restricted");
            assert_eq!(policy["owner"], json!(alice.record.principal_id));
            let revision = policy["revision"].as_u64().unwrap();
            // The owner grants read to Bob and write to a group holding the Viewer.
            let readers = Uuid::new_v4();
            access.apply(admin.id, AccessChange::CreateGroup { id: readers, display_name: "Writers".into() }, 20).await?;
            access.apply(admin.id, AccessChange::PutMembership { membership: access::Membership { group_id: readers, principal_id: viewer.record.principal_id } }, 21).await?;
            // A subject must hold a role in the universe before it can be granted anything.
            let put_without_role = rpc(&endpoint, alice, "access/policy/put", json!({"resource":resource,"visibility":"restricted",
                "grants":[{"subject":{"kind":"group","id":readers},"permission":"read"}]})).await;
            assert_eq!(put_without_role["error"]["data"]["kind"], "invalid_request", "{put_without_role}");
            access.apply(admin.id, AccessChange::AssignRole { assignment: RoleAssignment { scope, subject: Subject::Group(readers), role: Role::Viewer } }, 23).await?;
            let grants = json!([
                {"subject":{"kind":"principal","id":bob.record.principal_id},"permission":"read"},
                {"subject":{"kind":"group","id":readers},"permission":"write"},
            ]);
            let put = json!({"resource":resource,"visibility":"restricted","grants":grants,"expectedRevision":revision});
            let put_result = rpc(&endpoint, alice, "access/policy/put", put.clone()).await;
            assert_eq!(put_result["result"]["result"]["policy"]["revision"], json!(revision + 1), "{put_result}");
            // A stale revision is a conflict; a subject outside the universe is invalid.
            assert_eq!(rpc(&endpoint, alice, "access/policy/put", put).await["error"]["data"]["kind"], "conflict");
            assert_eq!(rpc(&endpoint, alice, "access/policy/put", json!({"resource":resource,"visibility":"restricted",
                "grants":[{"subject":{"kind":"principal","id":Uuid::new_v4()},"permission":"read"}]})).await["error"]["data"]["kind"], "invalid_request");
            // A reader reads and is listed to, but neither controls nor shares.
            success(rpc(&endpoint, bob, "session/read", read.clone()).await);
            assert!(lists(&success(rpc(&endpoint, bob, "session/list", json!({})).await), &private));
            forbidden(rpc(&endpoint, bob, "session/rename", json!({"sessionId":private,"displayName":"mine"})).await);
            forbidden(rpc(&endpoint, bob, "access/policy/put", json!({"resource":resource,"visibility":"universe"})).await);
            // A writer through a group controls, but the role still bounds it: the
            // Viewer cannot steer. A writer shares read and visibility, never write.
            success(rpc(&endpoint, viewer, "session/read", read.clone()).await);
            forbidden(rpc(&endpoint, viewer, "session/rename", json!({"sessionId":private,"displayName":"mine"})).await);
            forbidden(rpc(&endpoint, viewer, "access/policy/put", json!({"resource":resource,"visibility":"restricted","grants":[
                {"subject":{"kind":"principal","id":bob.record.principal_id},"permission":"write"},
                {"subject":{"kind":"group","id":readers},"permission":"write"}]})).await);
            forbidden(rpc(&endpoint, viewer, "access/policy/put", json!({"resource":resource,"visibility":"restricted","grants":[
                {"subject":{"kind":"principal","id":bob.record.principal_id},"permission":"read"}]})).await);
            access.apply(admin.id, AccessChange::PutMembership { membership: access::Membership { group_id: readers, principal_id: bob.record.principal_id } }, 22).await?;
            success(rpc(&endpoint, bob, "session/rename", json!({"sessionId":private,"displayName":"ours"})).await);
            success(rpc(&endpoint, bob, "access/policy/put", json!({"resource":resource,"visibility":"restricted","grants":[
                {"subject":{"kind":"principal","id":operator.record.principal_id},"permission":"read"},
                {"subject":{"kind":"group","id":readers},"permission":"write"}]})).await);
            // Elevated roles act only on what they can see: a reading Operator stops, an Admin without a grant still sees nothing.
            success(rpc(&endpoint, operator, "session/read", read.clone()).await);
            not_found(rpc(&endpoint, universe_admin, "session/read", read.clone()).await);
            // Revocation by replacement: the whole set is what the owner says.
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":resource,"visibility":"restricted"})).await);
            for caller in [viewer, bob, operator] {
                not_found(rpc(&endpoint, caller, "session/read", read.clone()).await);
            }
            // Universe visibility restores role rules; a bot's audience is set the same way.
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":resource,"visibility":"universe"})).await);
            success(rpc(&endpoint, viewer, "session/read", read.clone()).await);
            let private_bot = format!("bot-{}", Uuid::new_v4().simple());
            let profile_for_bot = format!("profile-{}", Uuid::new_v4().simple());
            success(rpc(&endpoint, alice, "profiles/put", json!({"profile":{"profileId":profile_for_bot}})).await);
            success(rpc(&endpoint, alice, "bots/create", json!({"bot":{"botId":private_bot,"profileId":profile_for_bot},"access":{"visibility":"restricted"}})).await);
            not_found(rpc(&endpoint, operator, "bots/read", json!({"botId":private_bot})).await);
            assert!(!success(rpc(&endpoint, operator, "bots/list", json!({})).await)["bots"].as_array().unwrap().iter().any(|b| b["botId"] == private_bot));
            success(rpc(&endpoint, alice, "bots/read", json!({"botId":private_bot})).await);
            assert_eq!(rpc(&endpoint, alice, "session/start", json!({"sessionId":session,"access":{"visibility":"restricted"}})).await["error"]["data"]["kind"], "invalid_request");
            // A bot is its own root: its policy names itself, and every session it
            // creates carries its audience. Sharing the bot shares that whole tree.
            let bot_ref = json!({"kind":"bot","id":private_bot});
            let own = success(rpc(&endpoint, alice, "access/policy/read", json!({"resource":bot_ref})).await)["policy"].clone();
            assert_eq!(own["root"], bot_ref);
            assert_eq!(own["visibility"], "restricted");
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":bot_ref,"visibility":"restricted","grants":[
                {"subject":{"kind":"principal","id":bob.record.principal_id},"permission":"write"},
                {"subject":{"kind":"principal","id":viewer.record.principal_id},"permission":"read"}]})).await);
            for caller in [viewer, bob] {
                success(rpc(&endpoint, caller, "bots/read", json!({"botId":private_bot})).await);
                assert!(success(rpc(&endpoint, caller, "bots/list", json!({})).await)["bots"].as_array().unwrap().iter().any(|b| b["botId"] == private_bot));
            }
            not_found(rpc(&endpoint, operator, "bots/read", json!({"botId":private_bot})).await);
            // Governance needs no content: an Operator stops and an Admin deletes a
            // restricted session they cannot read; neither reads it afterwards.
            let bobs_private = success(rpc(&endpoint, bob, "session/start", json!({"access":{"visibility":"restricted"}})).await)["session"]["id"].as_str().unwrap().to_owned();
            not_found(rpc(&endpoint, operator, "session/read", json!({"sessionId":bobs_private})).await);
            success(rpc(&endpoint, operator, "session/close", json!({"sessionId":bobs_private,"force":true})).await);
            not_found(rpc(&endpoint, operator, "session/read", json!({"sessionId":bobs_private})).await);
            not_found(rpc(&endpoint, universe_admin, "session/read", json!({"sessionId":bobs_private})).await);
            success(rpc(&endpoint, universe_admin, "session/delete", json!({"sessionId":bobs_private})).await);
            // Hand-off moves the whole tree in one step; the previous owner keeps nothing.
            forbidden(rpc(&endpoint, bob, "access/policy/put", json!({"resource":bot_ref,"visibility":"restricted","owner":bob.record.principal_id,"grants":[]})).await);
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":bot_ref,"visibility":"restricted","owner":bob.record.principal_id,"grants":[]})).await);
            not_found(rpc(&endpoint, alice, "bots/read", json!({"botId":private_bot})).await);
            not_found(rpc(&endpoint, viewer, "bots/read", json!({"botId":private_bot})).await);
            assert_eq!(success(rpc(&endpoint, bob, "access/policy/read", json!({"resource":bot_ref})).await)["policy"]["owner"], json!(bob.record.principal_id));
            let team_session = success(rpc(&endpoint, alice, "session/start", json!({})).await)["session"]["id"].as_str().unwrap().to_owned();
            // Execution identity. Everything so far runs as the universe's execution
            // service; personal execution needs an Admin to enable it and binds the
            // owner, restricts the root by default, and refuses hand-off.
            let execution = success(rpc(&endpoint, universe_admin, "access/execution/read", json!({})).await)["policy"].clone();
            // Creation options and sharing names are available to ordinary
            // members without exposing the administrative directory.
            assert_eq!(success(rpc(&endpoint, alice, "access/execution/read", json!({})).await)["policy"], execution);
            forbidden(rpc(&endpoint, alice, "access/execution/update", json!({"personalExecutionEnabled":true})).await);
            let subjects = success(rpc(&endpoint, alice, "access/subjects", json!({"query":bob.record.principal_id.to_string()})).await);
            assert_eq!(subjects["principalId"], alice.record.principal_id.to_string());
            assert_eq!(subjects["subjects"][0]["subject"]["id"], bob.record.principal_id.to_string());
            assert_eq!(subjects["subjects"][0].as_object().unwrap().len(), 2);

            let service_principal = execution["executionPrincipalId"].as_str().unwrap().to_owned();
            assert_eq!(execution["personalExecutionEnabled"], false);
            let read_session = success(rpc(&endpoint, alice, "session/read", json!({"sessionId":session})).await)["session"].clone();
            assert_eq!(read_session["access"]["execution"]["runAs"], json!(service_principal), "{read_session}");
            assert_eq!(read_session["access"]["execution"]["kind"], "service");
            assert_eq!(read_session["access"]["root"], json!({"kind":"session","id":session}));
            forbidden(rpc(&endpoint, alice, "session/start", json!({"execution":{"kind":"personal"}})).await);
            success(rpc(&endpoint, universe_admin, "access/execution/update", json!({"personalExecutionEnabled":true})).await);
            let mine_id = success(rpc(&endpoint, alice, "session/start", json!({"execution":{"kind":"personal"}})).await)["session"]["id"].as_str().unwrap().to_owned();
            let mine = success(rpc(&endpoint, alice, "session/read", json!({"sessionId":mine_id})).await)["session"].clone();
            assert_eq!(mine["access"]["visibility"], "restricted", "{mine}");
            assert_eq!(mine["access"]["execution"], json!({"runAs":alice.record.principal_id,"kind":"personal"}));
            not_found(rpc(&endpoint, bob, "session/read", json!({"sessionId":mine_id})).await);
            assert_eq!(rpc(&endpoint, alice, "access/policy/put", json!({"resource":{"kind":"session","id":mine_id},"visibility":"restricted","owner":bob.record.principal_id})).await["error"]["data"]["kind"], "invalid_request");
            // Personal work runs as its owner: made universe-visible, others read
            // it but only the owner works in it; Operators may still stop it.
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":{"kind":"session","id":mine_id},"visibility":"universe"})).await);
            success(rpc(&endpoint, bob, "session/read", json!({"sessionId":mine_id})).await);
            for caller in [bob, operator, universe_admin] {
                forbidden(rpc(&endpoint, caller, "session/rename", json!({"sessionId":mine_id,"displayName":"not yours"})).await);
                forbidden(rpc(&endpoint, caller, "session/runs/start", json!({"sessionId":mine_id,"source":{"type":"input","items":[{"type":"text","text":"not yours"}]}})).await);
            }
            success(rpc(&endpoint, alice, "session/rename", json!({"sessionId":mine_id,"displayName":"still mine"})).await);
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":{"kind":"session","id":mine_id},"visibility":"restricted"})).await);
            // A root created without a choice runs as the execution service.
            let member = success(rpc(&endpoint, bob, "session/read", json!({"sessionId":team_session})).await)["session"].clone();
            assert_eq!(member["access"]["execution"]["runAs"], json!(service_principal));
            let roster = success(rpc(&endpoint, alice, "bots/list", json!({})).await);
            assert!(roster["bots"].as_array().unwrap().iter().all(|b| b["access"]["execution"]["runAs"] == json!(service_principal)), "{roster}");
            success(rpc(&endpoint, universe_admin, "access/execution/update", json!({"personalExecutionEnabled":false})).await);
            forbidden(rpc(&endpoint, alice, "session/start", json!({"execution":{"kind":"personal"}})).await);
            // Run admission: the execution service must hold authority. Disabling
            // it refuses every new run in the universe; a run records who it ran as.
            let service_uuid = Uuid::parse_str(&service_principal)?;
            let run_input = json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"hello"}]}});
            access.apply(admin.id, AccessChange::SetPrincipalStatus { id: service_uuid, status: PrincipalStatus::Disabled }, 30).await?;
            forbidden(rpc(&endpoint, alice, "session/runs/start", run_input.clone()).await);
            access.apply(admin.id, AccessChange::SetPrincipalStatus { id: service_uuid, status: PrincipalStatus::Active }, 31).await?;
            success(rpc(&endpoint, alice, "session/runs/start", run_input).await);
            let bobs_root = success(rpc(&endpoint, bob, "session/start", json!({})).await)["session"]["id"].as_str().unwrap().to_owned();
            // Content. A hash is never access: a blob is read through a resource the
            // caller may read and that admitted it, or, without one, only by its uploader.
            let secret = json!({"blobs":[{"bytesBase64": base64::engine::general_purpose::STANDARD.encode(b"alice's private notes")}]});
            let uploaded = success(rpc(&endpoint, alice, "blobs/put", secret.clone()).await)["blobs"][0]["blobRef"].as_str().unwrap().to_owned();
            success(rpc(&endpoint, alice, "blobs/read", json!({"blobRef":uploaded})).await);
            forbidden(rpc(&endpoint, bob, "blobs/read", json!({"blobRef":uploaded})).await);
            assert_eq!(success(rpc(&endpoint, bob, "blobs/has", json!({"blobRefs":[uploaded]})).await)["blobs"][0]["exists"], false);
            // Attaching makes it content of the session, readable through it by its readers.
            let attach = |session: &str, blob_ref: &str| json!({"sessionId":session,"source":{"type":"input","items":[{"type":"textRef","blobRef":blob_ref}]}});
            let through = |session: &str| json!({"blobRef":uploaded,"resource":{"kind":"session","id":session}});
            success(rpc(&endpoint, alice, "session/runs/start", attach(&mine_id, &uploaded)).await);
            success(rpc(&endpoint, alice, "blobs/read", through(&mine_id)).await);
            not_found(rpc(&endpoint, bob, "blobs/read", through(&mine_id)).await);
            // Neither attaching the reference nor writing the digest into text makes it
            // content of Bob's session: the first is refused, the second is retained
            // for the sweeper only.
            forbidden(rpc(&endpoint, bob, "session/runs/start", attach(&bobs_root, &uploaded)).await);
            success(rpc(&endpoint, bob, "session/runs/start", json!({"sessionId":bobs_root,"source":{"type":"input","items":[{"type":"text","text":uploaded}]}})).await);
            forbidden(rpc(&endpoint, bob, "blobs/read", through(&bobs_root)).await);
            assert_eq!(success(rpc(&endpoint, bob, "blobs/has", json!({"blobRefs":[uploaded],"resource":{"kind":"session","id":bobs_root}})).await)["blobs"][0]["exists"], false);
            // A reader of Alice's session reads its content through it, and nowhere else.
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":{"kind":"session","id":mine_id},"visibility":"restricted","grants":[{"subject":{"kind":"principal","id":bob.record.principal_id},"permission":"read"}]})).await);
            success(rpc(&endpoint, bob, "blobs/read", through(&mine_id)).await);
            forbidden(rpc(&endpoint, bob, "blobs/read", through(&bobs_root)).await);
            forbidden(rpc(&endpoint, bob, "blobs/read", json!({"blobRef":uploaded})).await);
            // Uploading the same bytes grants exactly those bytes; content addressing makes it free.
            success(rpc(&endpoint, bob, "blobs/put", secret).await);
            success(rpc(&endpoint, bob, "blobs/read", json!({"blobRef":uploaded})).await);
            success(rpc(&endpoint, bob, "session/runs/start", attach(&bobs_root, &uploaded)).await);
            success(rpc(&endpoint, bob, "blobs/read", through(&bobs_root)).await);
            // A snapshot admits only children the committer may supply; the committed
            // manifest is the committer's own upload and usable by nobody else.
            let manifest = |blob_ref: &str| json!({"manifest":{"schema_version":"lightspeed.vfs.snapshot.v1",
                "root":{"entries":{"notes.txt":{"kind":"file","blob_ref":blob_ref,"size_bytes":21,"executable":false}}},
                "totals":{"files":1,"bytes":21}}});
            forbidden(rpc(&endpoint, operator, "vfs/snapshots/commit", manifest(&uploaded)).await);
            let snapshot = success(rpc(&endpoint, alice, "vfs/snapshots/commit", manifest(&uploaded)).await)["snapshotRef"].as_str().unwrap().to_owned();
            forbidden(rpc(&endpoint, viewer, "vfs/workspaces/create", json!({"snapshotRef":snapshot})).await);
            // An operator explicitly uploads the file and commits its manifest
            // before placing it in a universe-visible workspace.
            // Upload exactly the fixture's bytes, preserving the content address.
            let bytes = success(rpc(&endpoint, alice, "blobs/read", json!({"blobRef":uploaded})).await)["bytesBase64"].clone();
            success(rpc(&endpoint, operator, "blobs/put", json!({"blobs":[{"bytesBase64":bytes}]})).await);
            success(rpc(&endpoint, operator, "vfs/snapshots/commit", manifest(&uploaded)).await);
            let created_workspace = success(rpc(&endpoint, operator, "vfs/workspaces/create", json!({"snapshotRef":snapshot})).await)["workspace"].clone();
            let workspace = created_workspace["workspaceId"].as_str().unwrap().to_owned();
            let workspace_revision = created_workspace["revision"].clone();
            forbidden(rpc(&endpoint, viewer, "blobs/read", json!({"blobRef":uploaded})).await);
            let mut from_workspace = manifest(&uploaded);
            from_workspace["sourceWorkspaceId"] = json!(workspace);
            success(rpc(&endpoint, universe_admin, "vfs/snapshots/commit", from_workspace.clone()).await);
            let foreign_blob = success(rpc(&endpoint, alice, "blobs/put", json!({"blobs":[{"bytesBase64":base64::engine::general_purpose::STANDARD.encode(b"still private") }]})).await)["blobs"][0]["blobRef"].as_str().unwrap().to_owned();
            from_workspace["manifest"]["root"]["entries"]["notes.txt"]["blob_ref"] = json!(foreign_blob);
            forbidden(rpc(&endpoint, universe_admin, "vfs/snapshots/commit", from_workspace.clone()).await);
            // Raw manifest uploads cannot smuggle somebody else's bytes into a workspace.
            let raw_manifest = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&from_workspace["manifest"])?);
            let poisoned = success(rpc(&endpoint, operator, "blobs/put", json!({"blobs":[{"bytesBase64":raw_manifest}]})).await)["blobs"][0]["blobRef"].clone();
            forbidden(rpc(&endpoint, operator, "vfs/workspaces/create", json!({"snapshotRef":poisoned})).await);
            forbidden(rpc(&endpoint, operator, "vfs/workspaces/update", json!({"workspaceId":workspace,"snapshotRef":poisoned,"expectedRevision":workspace_revision})).await);
            let file = success(rpc(&endpoint, viewer, "vfs/workspaces/files/read", json!({"workspaceId":workspace,"path":"notes.txt"})).await);
            assert_eq!(file["blobRef"], uploaded);
            not_found(rpc(&endpoint, viewer, "vfs/workspaces/files/read", json!({"workspaceId":workspace,"path":"missing.txt"})).await);
            assert_eq!(rpc(&endpoint, viewer, "vfs/workspaces/files/read", json!({"workspaceId":workspace,"path":"../notes.txt"})).await["error"]["data"]["kind"], "invalid_request");
            // A different editor retains existing files, uploads one addition,
            // commits through the named workspace, then advances its revision.
            let added = success(rpc(&endpoint, universe_admin, "blobs/put", json!({"blobs":[{"bytesBase64":base64::engine::general_purpose::STANDARD.encode(b"workspace shared edit")}]})).await)["blobs"][0]["blobRef"].clone();
            let mut edit = manifest(&uploaded);
            edit["sourceWorkspaceId"] = json!(workspace);
            edit["manifest"]["root"]["entries"]["added.txt"] = json!({"kind":"file","blob_ref":added,"size_bytes":21,"executable":false});
            edit["manifest"]["totals"] = json!({"files":2,"bytes":42});
            let edited = success(rpc(&endpoint, universe_admin, "vfs/snapshots/commit", edit).await)["snapshotRef"].clone();
            success(rpc(&endpoint, universe_admin, "vfs/workspaces/update", json!({"workspaceId":workspace,"snapshotRef":edited,"expectedRevision":workspace_revision})).await);
            assert_eq!(success(rpc(&endpoint, viewer, "vfs/workspaces/files/read", json!({"workspaceId":workspace,"path":"added.txt"})).await)["blobRef"], added);

            // The group's role served the sharing scenario only; the revocation checks below assume the Viewer's own role.
            access.apply(admin.id, AccessChange::RevokeRole { assignment: RoleAssignment { scope, subject: Subject::Group(readers), role: Role::Viewer } }, 24).await?;
            // A contributor can author templates but cannot edit another author's template.
            let profile = format!("profile-{}", Uuid::new_v4().simple());
            let document = json!({"profile":{"profileId":profile}});
            success(rpc(&endpoint, alice, "profiles/put", document.clone()).await);
            forbidden(rpc(&endpoint, bob, "profiles/put", document.clone()).await);
            forbidden(rpc(&endpoint, viewer, "profiles/put", document.clone()).await);
            success(rpc(&endpoint, operator, "profiles/put", document.clone()).await);
            let owned = access.anchor(universe, &ResourceRef::Profile(profile.clone())).await?.unwrap();
            assert_eq!(owned.created_by, ActionActor::Principal { id: alice.record.principal_id });
            assert_eq!(owned.controller, ResourceController::Principal(alice.record.principal_id));
            let bot = format!("bot-{}", Uuid::new_v4().simple());
            let create_bot = json!({"bot":{"botId":bot,"profileId":profile},
                "triggers":[{"triggerId":"hook","kind":"webhook"}]});
            success(rpc(&endpoint, alice, "bots/create", create_bot).await);
            let bot_ownership = access.anchor(universe, &ResourceRef::Bot(bot.clone())).await?.unwrap();
            assert_eq!(bot_ownership.created_by, ActionActor::Principal { id: alice.record.principal_id });
            let trigger = json!({"botId":bot,"triggerId":"hook"});
            for caller in [alice, operator] {
                assert!(success(rpc(&endpoint, caller, "bots/triggers/read", trigger.clone()).await)["trigger"]["ingestPath"].is_string());
            }
            for caller in [bob, viewer] {
                let view = success(rpc(&endpoint, caller, "bots/triggers/read", trigger.clone()).await);
                assert!(view["trigger"]["ingestPath"].is_null());
                assert!(view["trigger"]["pairingCode"].is_null());
                let list = success(rpc(&endpoint, caller, "bots/triggers/list", json!({"botId":bot})).await);
                assert!(list["triggers"][0]["ingestPath"].is_null());
            }
            forbidden(rpc(&endpoint, bob, "bots/triggers/delete", trigger).await);
            // Direct callers receive the same enforcement; without a context there is no caller.
            let unscoped_api = api.clone();
            assert_eq!(tokio::spawn(async move { unscoped_api.list_profiles(api::ProfileListParams {}).await }).await?.unwrap_err().kind, AgentApiErrorKind::Unauthenticated);
            let headers = { let mut h = axum::http::HeaderMap::new(); h.insert("authorization", format!("Bearer {}", viewer.secret.expose()).parse()?); h };
            let context = temporal_server::gateway::authentication::authenticate(&keys, &access, &headers,
                api::METHOD_SESSION_READ, 10).await?;
            let denied = with_request_context(context.clone(), api.rename_session(api::SessionRenameParams {
                session_id: session.into(), display_name: None })).await.unwrap_err();
            assert_eq!(denied.kind, AgentApiErrorKind::Forbidden);
            access.apply(admin.id, AccessChange::RevokeRole { assignment: RoleAssignment {
                scope, subject: Subject::Principal(viewer.record.principal_id), role: Role::Viewer } }, 11).await?;
            // A context is one request's resolved snapshot: handlers decide from it, the
            // next request is resolved afresh, and only parked requests revalidate.
            assert!(with_request_context(context, api.list_profiles(api::ProfileListParams {})).await.is_ok());
            forbidden(rpc(&endpoint, viewer, "profiles/list", json!({})).await);
            // A real run is accepted and completes through the worker's fake model.
            let run = success(rpc(&endpoint, alice, "session/runs/start", json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"hello"}]}})).await);
            assert!(run["run"]["id"].is_string(), "{run}");
            // Anyone working in shared work may stop it; only the owner or an
            // Admin deletes someone else's session.
            success(rpc(&endpoint, bob, "session/close", json!({"sessionId":session,"force":true})).await);
            forbidden(rpc(&endpoint, bob, "session/delete", json!({"sessionId":session})).await);
            forbidden(rpc(&endpoint, operator, "session/delete", json!({"sessionId":session})).await);
            success(rpc(&endpoint, universe_admin, "session/delete", json!({"sessionId":session})).await);
            // Deletion releases the id: the next creator owns it under a fresh anchor.
            assert!(access.anchor(universe, &ResourceRef::Session(session.into())).await?.is_none());
            success(rpc(&endpoint, bob, "session/start", start).await);
            let reused = access.anchor(universe, &ResourceRef::Session(session.into())).await?.unwrap();
            assert_eq!(reused.created_by, ActionActor::Principal { id: bob.record.principal_id });
            let admissions: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_events WHERE universe_id=$1 AND acting_principal_id=$2 AND method='session/runs/start' AND outcome='succeeded'")
                .bind(universe).bind(alice.record.principal_id).fetch_one(&pool).await?;
            assert_eq!(admissions, 3, "one record per admitted run (three by Alice), despite nested service calls");
            anyhow::Ok(())
        };
        let result = tokio::time::timeout(Duration::from_secs(150), outcome).await;
        server.abort();
        result?
    }).await
}

/// Workspaces, environments and MCP servers under resource policies: who
/// sees and configures them, what a session's execution identity may attach
/// and run with, and how losing `use` stops a run at its next model call.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; fake model; serialized"]
async fn resource_policies_bound_what_sessions_attach_and_run_with() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let (pool, universe) = disposable_pool().await?;
    let access = PgAccessStore::new(pool.clone());
    let keys = PgApiKeyStore::new(pool.clone());
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let (admin, callers) = members(
        &pool,
        universe,
        &[
            Role::Viewer,
            Role::Contributor,
            Role::Contributor,
            Role::Operator,
            Role::Admin,
        ],
    )
    .await?;
    // The host administrator's own key, for deployment identity methods.
    let host = issue_key(&keys, AccessScope::Deployment, admin.id, admin.id).await?;
    // A file tool that runs long enough for a revocation to land during it.
    let (activities, counters) = support::live::fake_worker_activities_for_run_control(
        Duration::ZERO,
        Duration::from_secs(4),
        1,
    )
    .await?;
    support::live::run_with_live_worker(activities, move |client, queue, _| async move {
        let runtime = Arc::new(UniverseRuntime::new(client, queue, None, DeploymentStores::from_env().await?)?);
        let (endpoint, server) = serve(runtime).await?;
        let outcome = async {
            let [viewer, alice, bob, operator, universe_admin] = callers.as_slice() else { unreachable!() };
            let url = endpoint.as_str();
            let person = |key: &MintedApiKey| json!({"kind":"principal","id":key.record.principal_id});
            let may_use = |subject: Value| json!({"subject":subject,"permission":"use"});
            let restrict = |resource: &Value, grants: Vec<Value>| json!({"resource":resource,"visibility":"restricted","grants":grants});
            let agent_id = success(rpc(url, alice, "access/execution/read", json!({})).await)["policy"]["executionPrincipalId"]
                .as_str().unwrap().to_owned();
            let agent_uuid = Uuid::parse_str(&agent_id)?;
            let agent = json!({"kind":"principal","id":agent_id});

            // Workspaces. A Contributor creates and owns a restricted workspace;
            // to everyone else, an Operator included, it does not exist.
            let notes = upload(url, alice, b"restricted notes").await;
            let snapshot = success(rpc(url, alice, "vfs/snapshots/commit", manifest(&[("notes.txt", &notes, 16)])).await)["snapshotRef"].clone();
            let created = success(rpc(url, alice, "vfs/workspaces/create", json!({"snapshotRef":snapshot,"access":{"visibility":"restricted"}})).await)["workspace"].clone();
            assert_eq!(created["access"]["visibility"], "restricted", "{created}");
            assert_eq!(created["access"]["owner"], json!(alice.record.principal_id));
            assert!(created["access"]["execution"].is_null());
            let workspace = created["workspaceId"].as_str().unwrap().to_owned();
            let workspace_ref = json!({"kind":"workspace","id":workspace});
            let read_workspace = json!({"workspaceId":workspace});
            let read_file = |path: &str| json!({"workspaceId":workspace,"path":path});
            let through_workspace = json!({"snapshotRef":snapshot,"workspaceId":workspace});
            for caller in [viewer, bob, operator] {
                not_found(rpc(url, caller, "vfs/workspaces/read", read_workspace.clone()).await);
                not_found(rpc(url, caller, "vfs/workspaces/files/read", read_file("notes.txt")).await);
                not_found(rpc(url, caller, "vfs/snapshots/read", through_workspace.clone()).await);
                not_found(rpc(url, caller, "access/policy/read", json!({"resource":workspace_ref})).await);
                assert!(!listed(&success(rpc(url, caller, "vfs/workspaces/list", json!({})).await), "workspaces", "workspaceId", &workspace));
            }
            // A snapshot reference alone confers nothing.
            forbidden(rpc(url, bob, "vfs/snapshots/read", json!({"snapshotRef":snapshot})).await);
            // `use` is the only permission these kinds take.
            assert_eq!(kind(&rpc(url, alice, "access/policy/put", restrict(&workspace_ref, vec![json!({"subject":person(bob),"permission":"read"})])).await), "invalid_request");
            success(rpc(url, alice, "access/policy/put", restrict(&workspace_ref, vec![may_use(person(bob))])).await);
            // The grantee sees the workspace everywhere and advances its head...
            success(rpc(url, bob, "vfs/workspaces/read", read_workspace.clone()).await);
            assert_eq!(success(rpc(url, bob, "vfs/workspaces/files/read", read_file("notes.txt")).await)["blobRef"], notes);
            success(rpc(url, bob, "vfs/snapshots/read", through_workspace.clone()).await);
            assert!(listed(&success(rpc(url, bob, "vfs/workspaces/list", json!({})).await), "workspaces", "workspaceId", &workspace));
            let added = upload(url, bob, b"bob's addition").await;
            let mut edit = manifest(&[("notes.txt", &notes, 16), ("added.txt", &added, 14)]);
            edit["sourceWorkspaceId"] = json!(workspace);
            let edited = success(rpc(url, bob, "vfs/snapshots/commit", edit).await)["snapshotRef"].clone();
            let advanced = success(rpc(url, bob, "vfs/workspaces/update", json!({"workspaceId":workspace,"snapshotRef":edited,"expectedRevision":created["revision"]})).await)["workspace"].clone();
            assert_eq!(advanced["headSnapshotRef"], edited);
            // ...but configures nothing: no rename, no delete, no sharing.
            forbidden(rpc(url, bob, "vfs/workspaces/update", json!({"workspaceId":workspace,"snapshotRef":edited,"displayName":"Bob's"})).await);
            forbidden(rpc(url, bob, "vfs/workspaces/delete", read_workspace.clone()).await);
            forbidden(rpc(url, bob, "access/policy/put", restrict(&workspace_ref, vec![may_use(person(bob)), may_use(person(viewer))])).await);
            not_found(rpc(url, viewer, "vfs/workspaces/read", read_workspace.clone()).await);
            // Admin governs it without a grant: sees it and changes who may use it.
            success(rpc(url, universe_admin, "vfs/workspaces/read", read_workspace.clone()).await);
            assert!(listed(&success(rpc(url, universe_admin, "vfs/workspaces/list", json!({})).await), "workspaces", "workspaceId", &workspace));
            success(rpc(url, universe_admin, "access/policy/put", restrict(&workspace_ref, vec![may_use(person(bob)), may_use(person(viewer))])).await);
            // A grant lets the Viewer see it; the role still bounds use.
            assert_eq!(success(rpc(url, viewer, "vfs/workspaces/files/read", read_file("added.txt")).await)["blobRef"], added);
            forbidden(rpc(url, viewer, "vfs/workspaces/update", json!({"workspaceId":workspace,"snapshotRef":edited})).await);
            // Repeating the create names a taken id: it fails without touching
            // the live workspace's access, even for its owner.
            let repeat = json!({"workspaceId":workspace,"snapshotRef":snapshot,"access":{"visibility":"universe","grants":[may_use(person(bob))]}});
            assert_eq!(kind(&rpc(url, alice, "vfs/workspaces/create", repeat).await), "conflict");
            let unchanged = success(rpc(url, alice, "access/policy/read", json!({"resource":workspace_ref})).await)["policy"].clone();
            assert_eq!(unchanged["visibility"], "restricted", "{unchanged}");
            assert_eq!(unchanged["grants"].as_array().unwrap().len(), 2, "{unchanged}");
            // On a universe-visible workspace an Operator configures by role, yet
            // only its owner or Admin decides who may use it.
            let open = success(rpc(url, alice, "vfs/workspaces/create", json!({"displayName":"Open"})).await)["workspace"].clone();
            let open_id = open["workspaceId"].as_str().unwrap().to_owned();
            let rename = |name: &str| json!({"workspaceId":open_id,"snapshotRef":open["headSnapshotRef"],"displayName":name});
            forbidden(rpc(url, bob, "vfs/workspaces/update", rename("Bob's")).await);
            success(rpc(url, operator, "vfs/workspaces/update", rename("Renamed by an Operator")).await);
            forbidden(rpc(url, operator, "access/policy/put", json!({"resource":{"kind":"workspace","id":open_id},"visibility":"restricted"})).await);

            // MCP servers. An Operator registers a restricted server; a
            // Contributor may not register one, nor see this one.
            let server = format!("restricted-{}", Uuid::new_v4().simple());
            let server_ref = json!({"kind":"mcp_server","id":server});
            let record = json!({"serverId":server,"serverUrl":"https://mcp.example.invalid/mcp","defaultServerLabel":"restricted"});
            forbidden(rpc(url, bob, "mcp/servers/put", json!({"server":record})).await);
            let registered = success(rpc(url, operator, "mcp/servers/put", json!({"server":record,"access":{"visibility":"restricted"}})).await)["server"].clone();
            assert_eq!(registered["access"]["owner"], json!(operator.record.principal_id), "{registered}");
            // Access is chosen at creation and changed afterwards only as a policy.
            assert_eq!(kind(&rpc(url, operator, "mcp/servers/put", json!({"server":record,"access":{"visibility":"universe"}})).await), "invalid_request");
            not_found(rpc(url, bob, "mcp/servers/read", json!({"serverId":server})).await);
            assert!(!listed(&success(rpc(url, bob, "mcp/servers/list", json!({})).await), "servers", "serverId", &server));
            // Its login client belongs to its configuration: hidden with the
            // server, and changed only by whoever may configure it.
            let login_client = format!("mcp:{server}");
            let login = json!({"clientId":login_client,"providerKind":"mcpOAuth","authorizationEndpoint":"https://auth.example.invalid/authorize","tokenEndpoint":"https://auth.example.invalid/token","remoteClientId":"restricted","audience":"https://mcp.example.invalid/mcp"});
            not_found(rpc(url, bob, "auth/clients/create", login.clone()).await);
            success(rpc(url, operator, "auth/clients/create", login).await);
            assert!(listed(&success(rpc(url, operator, "auth/clients/list", json!({})).await), "clients", "clientId", &login_client));
            assert!(!listed(&success(rpc(url, bob, "auth/clients/list", json!({})).await), "clients", "clientId", &login_client));
            not_found(rpc(url, bob, "auth/clients/read", json!({"clientId":login_client})).await);
            not_found(rpc(url, bob, "auth/clients/delete", json!({"clientId":login_client})).await);
            success(rpc(url, operator, "auth/clients/read", json!({"clientId":login_client})).await);
            // Moving a bound credential to another URL is a credential change:
            // the server's owner, a Contributor, edits it but cannot redirect
            // the secret without configuring the universe.
            let grant = success(rpc(url, operator, "auth/grants/import", json!({"token":"live-bound-token"})).await)["grant"]["grantId"].clone();
            let handed = format!("handed-{}", Uuid::new_v4().simple());
            let bound = |server_url: &str, description: &str| json!({"server":{"serverId":handed,"serverUrl":server_url,"defaultServerLabel":"handed","description":description,"authPolicy":{"type":"requiredBearer"},"credential":{"type":"authGrant","grantId":grant}}});
            success(rpc(url, operator, "mcp/servers/put", bound("https://mcp.example.invalid/bound", "registered")).await);
            success(rpc(url, operator, "access/policy/put", json!({"resource":{"kind":"mcp_server","id":handed},"visibility":"universe","owner":bob.record.principal_id})).await);
            success(rpc(url, bob, "mcp/servers/put", bound("https://mcp.example.invalid/bound", "edited by its owner")).await);
            forbidden(rpc(url, bob, "mcp/servers/put", bound("https://elsewhere.example.invalid/mcp", "edited by its owner")).await);
            // Its login client decides where every grant naming it sends its
            // refresh token, so the owner may not replace it either.
            let handed_login = json!({"clientId":format!("mcp:{handed}"),"providerKind":"mcpOAuth","authorizationEndpoint":"https://auth.example.invalid/authorize","tokenEndpoint":"https://auth.example.invalid/token","remoteClientId":"handed","audience":"https://mcp.example.invalid/bound"});
            forbidden(rpc(url, bob, "auth/clients/create", handed_login.clone()).await);
            success(rpc(url, operator, "auth/clients/create", handed_login).await);
            forbidden(rpc(url, bob, "auth/clients/delete", json!({"clientId":format!("mcp:{handed}")})).await);
            success(rpc(url, bob, "auth/clients/read", json!({"clientId":format!("mcp:{handed}")})).await);
            // The Default agent identity may not use it, and a Contributor who
            // may not see it learns only that it is missing, wherever it is attached.
            let with_server = json!({"features":{"mcp":{"servers":[{"serverId":server}]}}});
            let refused = rpc(url, bob, "session/start", json!({"config":with_server})).await;
            not_found(refused.clone());
            assert!(!refusal(&refused).contains("Default agent identity"), "{refused}");
            let session = success(rpc(url, bob, "session/start", json!({})).await)["session"]["id"].as_str().unwrap().to_owned();
            let attach_server = json!({"sessionId":session,"config":with_server});
            not_found(rpc(url, bob, "session/config/put", attach_server.clone()).await);
            // Granting `use` to the Default agent identity opens the server to
            // every session running as it, whoever controls the session.
            success(rpc(url, operator, "access/policy/put", restrict(&server_ref, vec![may_use(agent.clone())])).await);
            // The preview decides for that identity, and only on what the caller sees.
            let preview = json!({"as":"execution_service","resources":[server_ref, workspace_ref]});
            let for_owner = success(rpc(url, operator, "access/read", preview.clone()).await)["resources"].clone();
            let server_actions = for_owner[0]["actions"].as_array().unwrap();
            assert!(server_actions.contains(&json!("use_resource")), "{for_owner}");
            assert!(!server_actions.contains(&json!("configure_resource")), "{for_owner}");
            assert_eq!(for_owner[1]["actions"], json!([]), "the owner of the server cannot see the workspace: {for_owner}");
            let for_contributor = success(rpc(url, bob, "access/read", preview).await)["resources"].clone();
            assert_eq!(for_contributor[0]["actions"], json!([]), "the Contributor cannot see the server: {for_contributor}");
            assert_eq!(for_contributor[1]["actions"], json!([]), "the identity cannot use the workspace: {for_contributor}");
            let own = success(rpc(url, bob, "access/read", json!({"resources":[workspace_ref]})).await)["resources"][0]["actions"].clone();
            assert!(own.as_array().unwrap().contains(&json!("use_resource")), "{own}");
            // The same attachments are admitted now, and a run starts and completes.
            success(rpc(url, bob, "session/start", json!({"config":with_server})).await);
            success(rpc(url, bob, "session/config/put", attach_server.clone()).await);
            let run = success(rpc(url, bob, "session/runs/start", text_input(&session)).await)["run"]["id"].as_str().unwrap().to_owned();
            assert_eq!(terminal_run(url, bob, &session, &run).await["status"], "completed");
            // Revoked, the next run is refused at admission: as missing to whoever
            // may not see the server, and naming it and the identity to a grantee.
            success(rpc(url, operator, "access/policy/put", restrict(&server_ref, vec![])).await);
            not_found(rpc(url, bob, "session/runs/start", text_input(&session)).await);
            success(rpc(url, operator, "access/policy/put", restrict(&server_ref, vec![may_use(person(bob))])).await);
            let refused = rpc(url, bob, "session/runs/start", text_input(&session)).await;
            forbidden(refused.clone());
            assert!(refusal(&refused).contains(&server) && refusal(&refused).contains("Default agent identity"), "{refused}");
            forbidden(rpc(url, bob, "session/start", json!({"config":with_server})).await);
            // Removing the attachment is always admitted, and runs are admissible again.
            success(rpc(url, bob, "session/config/put", json!({"sessionId":session,"config":{}})).await);
            let run = success(rpc(url, bob, "session/runs/start", text_input(&session)).await)["run"]["id"].as_str().unwrap().to_owned();
            assert_eq!(terminal_run(url, bob, &session, &run).await["status"], "completed");

            // Personal execution: a grantee's own session runs as the grantee and
            // may attach what the grantee may use; another Contributor may not.
            success(rpc(url, universe_admin, "access/execution/update", json!({"personalExecutionEnabled":true})).await);
            let personal = json!({"execution":{"kind":"personal"},"config":with_server});
            let mine = success(rpc(url, bob, "session/start", personal.clone()).await)["session"]["id"].as_str().unwrap().to_owned();
            let run = success(rpc(url, bob, "session/runs/start", text_input(&mine)).await)["run"]["id"].as_str().unwrap().to_owned();
            assert_eq!(terminal_run(url, bob, &mine, &run).await["status"], "completed");
            not_found(rpc(url, alice, "session/start", personal).await);
            success(rpc(url, universe_admin, "access/execution/update", json!({"personalExecutionEnabled":false})).await);

            // Environments. An Operator registers a restricted external machine.
            let machine = |request: &str| json!({"requestId":request,"connection":{"endpoint":"ws://127.0.0.1:1/restricted","transport":"webSocket"},"access":{"visibility":"restricted"}});
            forbidden(rpc(url, bob, "environments/external/create", machine(&format!("denied-{}", Uuid::new_v4().simple()))).await);
            let environment = success(rpc(url, operator, "environments/external/create", machine(&format!("restricted-{}", Uuid::new_v4().simple()))).await)["environment"].clone();
            assert_eq!(environment["access"]["visibility"], "restricted", "{environment}");
            let environment_id = environment["environmentId"].as_str().unwrap().to_owned();
            let environment_ref = json!({"kind":"environment","id":environment_id});
            let read_environment = json!({"environmentId":environment_id});
            for caller in [viewer, bob] {
                not_found(rpc(url, caller, "environments/read", read_environment.clone()).await);
                assert!(!listed(&success(rpc(url, caller, "environments/list", json!({})).await), "environments", "environmentId", &environment_id));
            }
            not_found(rpc(url, bob, "environments/jobs/create", json!({"environmentId":environment_id,"requestId":"hidden","jobs":[]})).await);
            let with_environment = json!({"features":{"environments":{"environments":[{"environmentId":environment_id,"access":"read"}]}}});
            not_found(rpc(url, bob, "session/start", json!({"config":with_environment})).await);
            let activate = json!({"sessionId":session,"environmentId":environment_id});
            not_found(rpc(url, bob, "session/environments/activate", activate.clone()).await);
            // A grant shows it to the Contributor, who then learns that the
            // identity their session runs as may not use it.
            success(rpc(url, operator, "access/policy/put", restrict(&environment_ref, vec![may_use(person(bob))])).await);
            success(rpc(url, bob, "environments/read", read_environment.clone()).await);
            let refused = rpc(url, bob, "session/environments/activate", activate).await;
            forbidden(refused.clone());
            assert!(refusal(&refused).contains(&environment_id) && refusal(&refused).contains("Default agent identity"), "{refused}");
            forbidden(rpc(url, bob, "session/start", json!({"config":with_environment})).await);
            // A grantee uses the machine and never configures it.
            forbidden(rpc(url, bob, "environments/close", read_environment.clone()).await);
            success(rpc(url, operator, "environments/close", read_environment).await);

            // The Default agent identity is an Executor, assigned by the system
            // alone: it holds nothing else, gets no key and cannot be re-assigned.
            let rights = access.effective_access(agent_uuid, scope).await?;
            assert_eq!(rights.roles, BTreeSet::from([Role::Executor]));
            assert!(rights.capabilities.is_empty());
            assert_eq!(rights.universe_action(UniverseAction::UseResource), RoleDecision::Allowed);
            for action in [UniverseAction::CreateSession, UniverseAction::CreateWorkspace, UniverseAction::ConfigureResource, UniverseAction::ManageAccess] {
                assert_eq!(rights.universe_action(action), RoleDecision::Denied, "{action:?}");
            }
            let key_for = |principal: Uuid| json!({"scope":scope,"principalId":principal,"displayName":"agent key"});
            forbidden(rpc(url, &host, "deployment/api-keys/create", key_for(agent_uuid)).await);
            forbidden(rpc(url, universe_admin, "deployment/api-keys/create", key_for(agent_uuid)).await);
            // An ordinary service principal gets one from the same administrator.
            let service = Uuid::new_v4();
            access.apply(admin.id, AccessChange::CreatePrincipal { id: service, kind: PrincipalKind::Service, management_scope: scope, display_name: "Exporter".into() }, 40).await?;
            access.apply(admin.id, AccessChange::AssignRole { assignment: RoleAssignment { scope, subject: Subject::Principal(service), role: Role::Viewer } }, 41).await?;
            success(rpc(url, &host, "deployment/api-keys/create", key_for(service)).await);
            for change in [
                AccessChange::AssignRole { assignment: RoleAssignment { scope, subject: Subject::Principal(bob.record.principal_id), role: Role::Executor } },
                AccessChange::RevokeRole { assignment: RoleAssignment { scope, subject: Subject::Principal(agent_uuid), role: Role::Executor } },
            ] {
                // Identity changes report an invalid change as rejected.
                for caller in [&host, universe_admin] {
                    let refused = rpc(url, caller, "deployment/identity/apply", json!(change)).await;
                    assert_eq!(kind(&refused), "rejected", "{change:?}");
                    assert!(refusal(&refused).contains("executor is system-assigned"), "{refused}");
                }
            }
            assert_eq!(access.effective_access(agent_uuid, scope).await?.roles, BTreeSet::from([Role::Executor]));
            assert!(!access.effective_access(bob.record.principal_id, scope).await?.roles.contains(&Role::Executor));

            // Revocation stops work at the next model call. The Default agent
            // identity may use Alice's restricted workspace from its creation; a
            // run calls a file tool, and Alice revokes the grant while it runs.
            let shared = success(rpc(url, alice, "vfs/workspaces/create", json!({"access":{"visibility":"restricted","grants":[may_use(agent.clone())]}})).await)["workspace"]["workspaceId"]
                .as_str().unwrap().to_owned();
            let shared_ref = json!({"kind":"workspace","id":shared});
            let attached = json!({"features":{"vfs":{"workspaces":[{"path":"/workspace","workspaceId":shared,"access":"read"}]}}});
            let session = success(rpc(url, alice, "session/start", json!({"config":attached})).await)["session"]["id"].as_str().unwrap().to_owned();
            let tools_started = counters.tool_calls_started();
            let run = success(rpc(url, alice, "session/runs/start", text_input(&session)).await)["run"]["id"].as_str().unwrap().to_owned();
            support::live::wait_until("the file tool to start", Duration::from_secs(20), async || Ok(counters.tool_calls_started() > tools_started)).await?;
            let generations = counters.generations_started();
            success(rpc(url, alice, "access/policy/put", restrict(&shared_ref, vec![])).await);
            let failed = terminal_run(url, alice, &session, &run).await;
            assert_eq!(failed["status"], "failed", "{failed}");
            // The authorized turn completed its tool call; the next model call never reached the model.
            assert!(counters.tool_calls_completed() > 0);
            assert_eq!(counters.generations_started(), generations);
            let events: api::SessionEventsReadResponse = serde_json::from_value(success(rpc(url, alice, "session/events/read", json!({"sessionId":session,"limit":500})).await))?;
            let failure = events.events.iter().find_map(|event| match &event.kind {
                api::SessionEventKindView::RunFailed { run_id, kind, message } if run_id.as_str() == run => Some((*kind, message.clone())),
                _ => None,
            });
            let (failure_kind, message) = failure.expect("the run records its failure");
            assert_eq!(failure_kind, api::RunFailureKindView::AuthorityRevoked);
            assert!(message.contains(&shared), "the failure names the resource: {message}");
            // The session stays open; the next run is refused at admission,
            // naming the resource and the identity to the owner.
            assert_eq!(success(rpc(url, alice, "session/read", json!({"sessionId":session})).await)["session"]["status"], "idle");
            let refused = rpc(url, alice, "session/runs/start", text_input(&session)).await;
            forbidden(refused.clone());
            assert!(refusal(&refused).contains(&shared) && refusal(&refused).contains("Default agent identity"), "{refused}");
            // Removing the attachment repairs the session.
            success(rpc(url, alice, "session/config/put", json!({"sessionId":session,"config":{}})).await);
            let run = success(rpc(url, alice, "session/runs/start", text_input(&session)).await)["run"]["id"].as_str().unwrap().to_owned();
            assert_eq!(terminal_run(url, alice, &session, &run).await["status"], "completed");
            anyhow::Ok(())
        };
        let result = tokio::time::timeout(Duration::from_secs(170), outcome).await;
        server.abort();
        result?
    }).await
}
