//! Authenticated HTTP and direct-service authorization over real PostgreSQL and
//! Temporal, with a fake model. Requires an explicitly selected disposable DB.
mod support;

use access::*;
use api::{AgentApiErrorKind, AgentApiService as _};
use auth::{ApiKeyStore as _, CreateApiKey, MintedApiKey};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    config::GatewayAuthMode,
    gateway::{GatewayRoutes, GatewayState, gateway_router, principal::with_request_context},
};
use uuid::Uuid;

async fn rpc(url: &str, key: &MintedApiKey, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(url)
        .bearer_auth(key.secret.expose())
        .json(&json!({"id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn success(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    value["result"]["result"].clone()
}
fn forbidden(value: Value) {
    assert_eq!(value["error"]["data"]["kind"], "forbidden", "{value}");
}
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; fake model; serialized"]
async fn authenticated_roles_ownership_and_direct_service_boundaries() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
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
    let access = PgAccessStore::new(pool.clone());
    // The host-only development fixture is explicit; HTTP still uses scoped keys.
    let admin = access.initialize_local_development(universe, 1).await?;
    let keys = PgApiKeyStore::new(pool.clone());
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let mut callers = Vec::new();
    for role in [
        Role::Viewer,
        Role::Contributor,
        Role::Contributor,
        Role::Operator,
        Role::Admin,
    ] {
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
                        role,
                    },
                },
                3,
            )
            .await?;
        let key = auth::mint_api_key(scope, id, admin.id, None, 4);
        keys.create_api_key(CreateApiKey {
            authority_scope: AccessScope::Deployment,
            key_hash: key.key_hash.clone(),
            record: key.record.clone(),
        })
        .await?;
        callers.push(key);
    }
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, session_id| async move {
        let runtime = Arc::new(UniverseRuntime::new(client, queue, None, DeploymentStores::from_env().await?)?);
        let api = runtime.state_for(universe, false).await?.api.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/rpc", listener.local_addr()?);
        let router = gateway_router(Arc::new(GatewayState::multi(GatewayAuthMode::Authenticated,
            runtime, endpoint.clone())), 1024 * 1024, GatewayRoutes { api: true, environment: false });
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
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
            for caller in [bob, operator, universe_admin] {
                forbidden(rpc(&endpoint, caller, "session/start", start.clone()).await);
                forbidden(rpc(&endpoint, caller, "session/rename", json!({"sessionId":session,"displayName":"takeover"})).await);
                forbidden(rpc(&endpoint, caller, "session/runs/start", json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"takeover"}]}})).await);
                forbidden(rpc(&endpoint, caller, "session/context/append", json!({"sessionId":session,"entries":[]})).await);
            }
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
            assert!(!success(rpc(&endpoint, operator, "bots/list", json!({})).await)["bots"].as_array().unwrap().iter().any(|b| b["bot"]["botId"] == private_bot));
            success(rpc(&endpoint, alice, "bots/read", json!({"botId":private_bot})).await);
            assert_eq!(rpc(&endpoint, alice, "session/start", json!({"sessionId":session,"access":{"visibility":"restricted"}})).await["error"]["data"]["kind"], "invalid_request");
            // Collections. A restricted collection is one audience for the bots and
            // sessions created in it; members carry no policy of their own.
            let collection = success(rpc(&endpoint, alice, "collection/create", json!({"displayName":"Investigation","access":{"visibility":"restricted"}})).await)["collection"].clone();
            let collection_id = collection["collectionId"].as_str().unwrap().to_owned();
            let collection_ref = json!({"kind":"collection","id":collection_id});
            let team_bot = format!("bot-{}", Uuid::new_v4().simple());
            success(rpc(&endpoint, alice, "bots/create", json!({"bot":{"botId":team_bot,"profileId":profile_for_bot},"access":{"root":collection_ref}})).await);
            let team_session = success(rpc(&endpoint, alice, "session/start", json!({"access":{"root":collection_ref}})).await)["session"]["id"].as_str().unwrap().to_owned();
            assert_eq!(rpc(&endpoint, alice, "session/start", json!({"access":{"root":collection_ref,"visibility":"restricted"}})).await["error"]["data"]["kind"], "invalid_request");
            forbidden(rpc(&endpoint, bob, "session/start", json!({"access":{"root":collection_ref}})).await);
            for caller in [viewer, bob, operator, universe_admin] {
                not_found(rpc(&endpoint, caller, "collection/read", json!({"collectionId":collection_id})).await);
                not_found(rpc(&endpoint, caller, "bots/read", json!({"botId":team_bot})).await);
                not_found(rpc(&endpoint, caller, "session/read", json!({"sessionId":team_session})).await);
                assert!(success(rpc(&endpoint, caller, "collection/list", json!({})).await)["collections"].as_array().unwrap().is_empty() || caller.record.principal_id == alice.record.principal_id);
            }
            let member_policy = success(rpc(&endpoint, alice, "access/policy/read", json!({"resource":{"kind":"session","id":team_session}})).await)["policy"].clone();
            assert_eq!(member_policy["root"], collection_ref);
            assert_eq!(member_policy["visibility"], "restricted");
            // Members: the bot, its own session, and the session created in the collection.
            let members = success(rpc(&endpoint, alice, "collection/read", json!({"collectionId":collection_id})).await)["members"].clone();
            let members = members.as_array().unwrap();
            assert!(members.contains(&json!({"kind":"bot","id":team_bot})), "{members:?}");
            assert!(members.contains(&json!({"kind":"session","id":team_session})), "{members:?}");
            assert!(members.iter().all(|m| m["kind"] == "bot" || m["kind"] == "session"), "{members:?}");
            // Sharing the collection shares everything in it; write lets a member create in it.
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":collection_ref,"visibility":"restricted","grants":[
                {"subject":{"kind":"principal","id":bob.record.principal_id},"permission":"write"},
                {"subject":{"kind":"principal","id":viewer.record.principal_id},"permission":"read"}]})).await);
            for caller in [viewer, bob] {
                success(rpc(&endpoint, caller, "collection/read", json!({"collectionId":collection_id})).await);
                success(rpc(&endpoint, caller, "bots/read", json!({"botId":team_bot})).await);
                success(rpc(&endpoint, caller, "session/read", json!({"sessionId":team_session})).await);
                assert!(lists(&success(rpc(&endpoint, caller, "session/list", json!({})).await), &team_session));
            }
            let bobs_session = success(rpc(&endpoint, bob, "session/start", json!({"access":{"root":collection_ref}})).await)["session"]["id"].as_str().unwrap().to_owned();
            success(rpc(&endpoint, viewer, "session/read", json!({"sessionId":bobs_session})).await);
            // Governance needs no content: an Operator stops and an Admin deletes a
            // restricted session they cannot read; neither reads it afterwards.
            success(rpc(&endpoint, operator, "session/close", json!({"sessionId":bobs_session,"force":true})).await);
            not_found(rpc(&endpoint, operator, "session/read", json!({"sessionId":bobs_session})).await);
            success(rpc(&endpoint, universe_admin, "session/delete", json!({"sessionId":bobs_session})).await);
            // The collection cannot go while it holds members; renaming follows ownership.
            assert_eq!(rpc(&endpoint, alice, "collection/delete", json!({"collectionId":collection_id})).await["error"]["data"]["kind"], "conflict");
            forbidden(rpc(&endpoint, bob, "collection/update", json!({"collectionId":collection_id,"displayName":"Theirs"})).await);
            let renamed = success(rpc(&endpoint, alice, "collection/update", json!({"collectionId":collection_id,"displayName":"Ours","expectedRevision":1})).await)["collection"].clone();
            assert_eq!(renamed["revision"], 2);
            // Hand-off moves the whole tree in one step; the previous owner keeps nothing.
            forbidden(rpc(&endpoint, bob, "access/policy/put", json!({"resource":collection_ref,"visibility":"restricted","owner":bob.record.principal_id,"grants":[]})).await);
            success(rpc(&endpoint, alice, "access/policy/put", json!({"resource":collection_ref,"visibility":"restricted","owner":bob.record.principal_id,"grants":[]})).await);
            not_found(rpc(&endpoint, alice, "session/read", json!({"sessionId":team_session})).await);
            not_found(rpc(&endpoint, alice, "collection/read", json!({"collectionId":collection_id})).await);
            assert_eq!(success(rpc(&endpoint, bob, "access/policy/read", json!({"resource":{"kind":"bot","id":team_bot}})).await)["policy"]["owner"], json!(bob.record.principal_id));
            success(rpc(&endpoint, bob, "collection/update", json!({"collectionId":collection_id,"displayName":"Bob's"})).await);
            // A bot created without a root gets a collection of its own, which goes with the bot.
            let own = success(rpc(&endpoint, alice, "access/policy/read", json!({"resource":{"kind":"bot","id":private_bot}})).await)["policy"].clone();
            assert_eq!(own["root"]["kind"], "collection");
            let own_collection = own["root"]["id"].as_str().unwrap().to_owned();
            assert_eq!(own["visibility"], "restricted");
            success(rpc(&endpoint, alice, "collection/read", json!({"collectionId":own_collection})).await);
            not_found(rpc(&endpoint, operator, "collection/read", json!({"collectionId":own_collection})).await);
            // Removal of that collection with the bot needs the bots worker; the bots suite covers it.
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
            // Elevated roles can stop; only the owner or an Admin deletes someone else's session.
            forbidden(rpc(&endpoint, bob, "session/close", json!({"sessionId":session,"force":true})).await);
            success(rpc(&endpoint, operator, "session/close", json!({"sessionId":session,"force":true})).await);
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
            assert_eq!(admissions, 1, "one record per run, despite nested service calls");
            anyhow::Ok(())
        };
        let result = tokio::time::timeout(Duration::from_secs(150), outcome).await;
        server.abort();
        result?
    }).await
}
