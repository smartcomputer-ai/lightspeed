//! In-flight content revocation and durable administrative audit over disposable
//! PostgreSQL and Temporal. No workflow, provider, or external credential is used.
use access::*;
use api::{AgentApiErrorKind, AgentApiService as _, DeploymentApiService as _};
use auth::{ApiKeyStore as _, CreateApiKey, MintedApiKey};
use axum::http::HeaderMap;
use engine::{
    CoreAgentCodec, CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent, SessionId,
    UncommittedCoreAgentEvent,
    storage::{AppendSessionEvents, CreateSession, SessionStore as _},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    config::GatewayAuthMode,
    gateway::{
        GatewayAgentApi, GatewayDeploymentApi, GatewayRoutes, GatewayState,
        authentication::{authenticate, local_context},
        connect_temporal, gateway_router,
        principal::with_request_context,
    },
};
use uuid::Uuid;

struct Fixture {
    pool: sqlx::PgPool,
    access: PgAccessStore,
    keys: PgApiKeyStore,
    universe: Uuid,
    admin: Principal,
    api: Arc<GatewayAgentApi>,
    deployment: GatewayDeploymentApi,
    endpoint: String,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")?;
        anyhow::ensure!(url == std::env::var("LIGHTSPEED_POSTGRES_URL")?);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&url)
            .await?;
        PgStore::migrate(&pool).await?;
        let universe = Uuid::new_v4();
        store_pg::create_universe(&pool, universe).await?;
        let access = PgAccessStore::new(pool.clone());
        let host = access.initialize_local_development(universe, 1).await?;
        let admin_id = Uuid::new_v4();
        access
            .apply(
                host.id,
                AccessChange::CreatePrincipal {
                    id: admin_id,
                    kind: PrincipalKind::User,
                    management_scope: AccessScope::Deployment,
                    display_name: "Audit administrator".into(),
                },
                2,
            )
            .await?;
        for (scope, role) in [
            (AccessScope::Deployment, Role::DeploymentAdmin),
            (
                AccessScope::Universe {
                    universe_id: universe,
                },
                Role::Admin,
            ),
        ] {
            access
                .apply(
                    host.id,
                    AccessChange::AssignRole {
                        assignment: RoleAssignment {
                            scope,
                            subject: Subject::Principal(admin_id),
                            role,
                        },
                    },
                    3,
                )
                .await?;
        }
        let admin = access.principal(admin_id).await?.unwrap();
        let client = connect_temporal(&std::env::var("TEMPORAL_ADDRESS")?, "default").await?;
        let runtime = Arc::new(UniverseRuntime::new(
            client,
            format!("audit-{}", Uuid::new_v4()),
            Some("http://127.0.0.1:1".into()),
            DeploymentStores::from_env().await?,
        )?);
        let api = runtime.state_for(universe, false).await?.api.clone();
        let deployment = GatewayDeploymentApi::new(runtime.clone());
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
        Ok(Self {
            keys: PgApiKeyStore::new(pool.clone()),
            pool,
            access,
            universe,
            admin,
            api,
            deployment,
            endpoint,
            server,
        })
    }
    fn scope(&self) -> AccessScope {
        AccessScope::Universe {
            universe_id: self.universe,
        }
    }
    async fn change(&self, change: AccessChange) {
        self.access.apply(self.admin.id, change, 2).await.unwrap();
    }
    async fn principal(&self, kind: PrincipalKind) -> Uuid {
        let id = Uuid::new_v4();
        self.change(AccessChange::CreatePrincipal {
            id,
            kind,
            management_scope: AccessScope::Deployment,
            display_name: "Audit fixture".into(),
        })
        .await;
        id
    }
    async fn key(&self, principal_id: Uuid, scope: AccessScope) -> MintedApiKey {
        let key = auth::mint_api_key(scope, principal_id, self.admin.id, None, 3);
        self.keys
            .create_api_key(CreateApiKey {
                authority_scope: AccessScope::Deployment,
                key_hash: key.key_hash.clone(),
                record: key.record.clone(),
            })
            .await
            .unwrap();
        key
    }
    async fn session(&self) -> String {
        let id = SessionId::new(format!("audit-session-{}", Uuid::new_v4()));
        self.access
            .reserve_ownership(
                self.universe,
                &ResourceOwnership {
                    resource: ResourceRef::Session(id.to_string()),
                    created_by: ActionActor::Principal { id: self.admin.id },
                    controller: ResourceController::Principal(self.admin.id),
                    created_at_ms: 1,
                },
            )
            .await
            .unwrap();
        let store = PgStore::new(
            self.pool.clone(),
            store_pg::PgStoreConfig::new(self.universe),
        );
        store
            .create_session(CreateSession {
                session_id: id.clone(),
                metadata: Default::default(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let event = CoreAgentCodec
            .encode_uncommitted(&UncommittedCoreAgentEvent {
                observed_at_ms: 2,
                joins: CoreAgentJoins::default(),
                event: CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed),
            })
            .unwrap();
        store
            .append(AppendSessionEvents {
                session_id: id.clone(),
                expected_head: None,
                events: vec![event],
            })
            .await
            .unwrap();
        id.to_string()
    }
}
fn headers(key: &MintedApiKey, scope: Option<Uuid>, user: Option<Uuid>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", key.secret.expose()).parse().unwrap(),
    );
    if let Some(id) = scope {
        headers.insert("x-lightspeed-universe", id.to_string().parse().unwrap());
    }
    if let Some(id) = user {
        headers.insert(
            "x-lightspeed-principal",
            format!("user:{id}").parse().unwrap(),
        );
    }
    headers
}
async fn rpc(
    endpoint: &str,
    headers: HeaderMap,
    method: &str,
    params: Value,
) -> Result<Value, AgentApiErrorKind> {
    let response: Value = reqwest::Client::new()
        .post(endpoint)
        .headers(headers)
        .json(&json!({"id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    if let Some(error) = response.get("error") {
        return Err(
            serde_json::from_value::<api::AgentApiError>(error["data"].clone())
                .unwrap()
                .kind,
        );
    }
    Ok(response["result"]["result"].clone())
}
async fn blocked_reader(pool: &sqlx::PgPool, locker: i32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(locker)
            .fetch_one(pool)
            .await
            .unwrap();
            if blocked {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("reader must pass admission and reach event storage before revocation");
}

#[derive(Clone, Copy, Debug)]
enum Revocation {
    Role,
    GroupMembership,
    GroupRole,
    UserDisabled,
    ServiceDisabled,
    Assertion,
    UserKey,
    ServiceKey,
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly selected disposable PostgreSQL/Temporal; serialize; no providers"]
async fn admitted_long_polls_and_buffered_pages_recheck_before_delivery() -> anyhow::Result<()> {
    let f = Fixture::new().await?;
    let session = f.session().await;
    let variants = [
        Revocation::Role,
        Revocation::GroupMembership,
        Revocation::GroupRole,
        Revocation::UserDisabled,
        Revocation::ServiceDisabled,
        Revocation::Assertion,
        Revocation::UserKey,
        Revocation::ServiceKey,
    ];
    // Every revocation is exercised through HTTP and a captured direct-service context.
    for direct in [false, true] {
        for revocation in variants {
            exercise_revocation(&f, &session, revocation, direct, false, false).await;
        }
    }
    // Also block a populated forward/backward page after admission. The final
    // projection check must discard data, even when there is no waiting loop.
    for direct in [false, true] {
        for backward in [false, true] {
            exercise_revocation(&f, &session, Revocation::Role, direct, true, backward).await;
        }
    }
    println!(
        "20 deterministic in-flight revocations passed (HTTP and direct, quiet and populated pages)"
    );
    Ok(())
}

async fn exercise_revocation(
    f: &Fixture,
    session: &str,
    revocation: Revocation,
    direct: bool,
    populated: bool,
    backward: bool,
) {
    let user = f.principal(PrincipalKind::User).await;
    let service = f.principal(PrincipalKind::Service).await;
    let group = Uuid::new_v4();
    f.change(AccessChange::CreateGroup {
        id: group,
        display_name: "Readers".into(),
    })
    .await;
    let membership = Membership {
        group_id: group,
        principal_id: user,
    };
    let grouped = matches!(
        revocation,
        Revocation::GroupMembership | Revocation::GroupRole
    );
    let role = RoleAssignment {
        scope: f.scope(),
        subject: if grouped {
            Subject::Group(group)
        } else {
            Subject::Principal(user)
        },
        role: Role::Viewer,
    };
    f.change(AccessChange::AssignRole { assignment: role })
        .await;
    if grouped {
        f.change(AccessChange::PutMembership { membership }).await;
    }
    let assertion = CapabilityAssignment {
        scope: f.scope(),
        principal_id: service,
        capability: ServiceCapability::AssertUser,
    };
    f.change(AccessChange::AssignCapability {
        assignment: assertion,
    })
    .await;
    let key = f.key(user, f.scope()).await;
    let service_key = f.key(service, AccessScope::Deployment).await;
    let asserted = !matches!(revocation, Revocation::UserKey);
    let h = if asserted {
        headers(&service_key, Some(f.universe), Some(user))
    } else {
        headers(&key, None, None)
    };
    let context = authenticate(&f.keys, &f.access, &h, api::METHOD_SESSION_EVENTS_READ, 4)
        .await
        .unwrap();
    // Authorized populated reads still work in both directions.
    let params = api::SessionEventsReadParams {
        session_id: session.into(),
        direction: if backward {
            api::SessionEventDirection::Backward
        } else {
            api::SessionEventDirection::Forward
        },
        after: if backward {
            None
        } else {
            Some(api::EventCursor {
                seq: if populated { 0 } else { 1 },
            })
        },
        before: None,
        limit: Some(10),
        wait_ms: Some(if populated { 0 } else { 30_000 }),
    };
    let positive = with_request_context(
        context.clone(),
        f.api.read_session_events(api::SessionEventsReadParams {
            after: None,
            wait_ms: None,
            ..params.clone()
        }),
    )
    .await
    .unwrap();
    assert_eq!(positive.result.events.len(), 1);
    let mut lock = f.pool.begin().await.unwrap();
    // Empty tails can avoid querying session_events entirely. Gate their session
    // lookup instead; populated pages are gated during the actual event fetch.
    sqlx::query(if populated {
        "LOCK TABLE session_events IN ACCESS EXCLUSIVE MODE"
    } else {
        "LOCK TABLE sessions IN ACCESS EXCLUSIVE MODE"
    })
    .execute(&mut *lock)
    .await
    .unwrap();
    let locker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let endpoint = f.endpoint.clone();
    let api = f.api.clone();
    let reader = tokio::spawn(async move {
        if direct {
            with_request_context(context, api.read_session_events(params))
                .await
                .map(|v| json!(v.result))
                .map_err(|e| e.kind)
        } else {
            rpc(&endpoint, h, api::METHOD_SESSION_EVENTS_READ, json!(params)).await
        }
    });
    blocked_reader(&f.pool, locker).await;
    match revocation {
        Revocation::Role | Revocation::GroupRole => {
            f.change(AccessChange::RevokeRole { assignment: role })
                .await
        }
        Revocation::GroupMembership => {
            f.change(AccessChange::RemoveMembership { membership })
                .await
        }
        Revocation::UserDisabled => {
            f.change(AccessChange::SetPrincipalStatus {
                id: user,
                status: PrincipalStatus::Disabled,
            })
            .await
        }
        Revocation::ServiceDisabled => {
            f.change(AccessChange::SetPrincipalStatus {
                id: service,
                status: PrincipalStatus::Disabled,
            })
            .await
        }
        Revocation::Assertion => {
            f.change(AccessChange::RevokeCapability {
                assignment: assertion,
            })
            .await
        }
        Revocation::UserKey => {
            f.keys
                .revoke_managed_key(
                    f.admin.id,
                    AccessScope::Deployment,
                    f.scope(),
                    &key.record.key_prefix,
                    5,
                )
                .await
                .unwrap();
        }
        Revocation::ServiceKey => {
            f.keys
                .revoke_managed_key(
                    f.admin.id,
                    AccessScope::Deployment,
                    AccessScope::Deployment,
                    &service_key.record.key_prefix,
                    5,
                )
                .await
                .unwrap();
        }
    }
    lock.commit().await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), reader)
        .await
        .expect("revocation must stop a quiet long poll before its 30-second timeout")
        .unwrap();
    assert_eq!(
        response,
        Err(AgentApiErrorKind::Rejected),
        "{revocation:?}, direct={direct}, populated={populated}, backward={backward}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_events WHERE outcome='denied' AND identity->>'actingPrincipal'=$1 AND method='session/events/read'")
        .bind(user.to_string()).fetch_one(&f.pool).await.unwrap();
    assert_eq!(
        rows, 1,
        "one attributed revocation denial, including HTTP propagation"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly selected disposable PostgreSQL/Temporal; serialize; no providers"]
async fn deployment_operations_and_denials_keep_safe_durable_attribution() -> anyhow::Result<()> {
    let f = Fixture::new().await?;
    let service = f.principal(PrincipalKind::Service).await;
    f.change(AccessChange::AssignCapability {
        assignment: CapabilityAssignment {
            scope: AccessScope::Deployment,
            principal_id: service,
            capability: ServiceCapability::AssertUser,
        },
    })
    .await;
    let service_key = f.key(service, AccessScope::Deployment).await;
    let h = headers(&service_key, None, Some(f.admin.id));
    let captured = authenticate(
        &f.keys,
        &f.access,
        &h,
        api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
        10,
    )
    .await?;
    // No gateway-only privilege checks: a missing context fails closed on direct calls.
    assert_eq!(
        f.deployment
            .list_universes(api::DeploymentUniverseListParams {})
            .await
            .unwrap_err()
            .kind,
        AgentApiErrorKind::Rejected
    );
    // Unknown/bad credentials and header-only assertions never become attributed users.
    let marker = format!("sensitive-{}", Uuid::new_v4());
    let mut bad = HeaderMap::new();
    bad.insert("authorization", format!("Bearer lsk_{marker}").parse()?);
    bad.insert(
        "x-lightspeed-principal",
        format!("user:{}", f.admin.id).parse()?,
    );
    assert_eq!(
        rpc(
            &f.endpoint,
            bad.clone(),
            api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
            json!({"secret": marker})
        )
        .await,
        Err(AgentApiErrorKind::Rejected)
    );
    assert_eq!(
        rpc(&f.endpoint, bad, &marker, json!({})).await,
        Err(AgentApiErrorKind::Rejected)
    );
    let user = f.principal(PrincipalKind::User).await;
    f.change(AccessChange::AssignRole {
        assignment: RoleAssignment {
            scope: f.scope(),
            subject: Subject::Principal(user),
            role: Role::Viewer,
        },
    })
    .await;
    let user_key = f.key(user, f.scope()).await;
    let spoof = headers(&user_key, None, Some(f.admin.id));
    assert_eq!(
        rpc(&f.endpoint, spoof, api::METHOD_SESSION_LIST, json!({})).await,
        Err(AgentApiErrorKind::Rejected)
    );
    let denied = rpc(
        &f.endpoint,
        headers(&user_key, None, None),
        api::METHOD_SESSION_RENAME,
        json!({"sessionId":marker,"displayName":marker}),
    )
    .await;
    assert_eq!(denied, Err(AgentApiErrorKind::Rejected));
    // Store-level directory and key decisions also produce an attributed denial.
    let ordinary = headers(&service_key, None, Some(user));
    assert_eq!(
        rpc(
            &f.endpoint,
            ordinary.clone(),
            api::METHOD_DEPLOYMENT_IDENTITY_DIRECTORY,
            json!({"scope":AccessScope::Deployment})
        )
        .await,
        Err(AgentApiErrorKind::Rejected)
    );
    assert_eq!(
        rpc(
            &f.endpoint,
            ordinary,
            api::METHOD_DEPLOYMENT_API_KEYS_CREATE,
            json!({"scope":AccessScope::Deployment,"principalId":user,"displayName":marker})
        )
        .await,
        Err(AgentApiErrorKind::Rejected)
    );

    let universe = Uuid::new_v4();
    rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_UNIVERSES_CREATE,
        json!({"universeId":universe}),
    )
    .await
    .unwrap();
    // A deployment credential with only a universe assertion cannot borrow
    // this administrator's authority elsewhere, even through identity methods.
    let scoped_service = f.principal(PrincipalKind::Service).await;
    f.change(AccessChange::AssignCapability {
        assignment: CapabilityAssignment {
            scope: f.scope(),
            principal_id: scoped_service,
            capability: ServiceCapability::AssertUser,
        },
    })
    .await;
    let scoped_key = f.key(scoped_service, AccessScope::Deployment).await;
    let scoped_headers = headers(&scoped_key, Some(f.universe), Some(f.admin.id));
    for method in [
        api::METHOD_DEPLOYMENT_IDENTITY_SELF,
        api::METHOD_DEPLOYMENT_IDENTITY_DIRECTORY,
    ] {
        assert_eq!(
            rpc(
                &f.endpoint,
                scoped_headers.clone(),
                method,
                json!({"scope":AccessScope::Deployment})
            )
            .await,
            Err(AgentApiErrorKind::Rejected)
        );
    }
    let self_view = rpc(
        &f.endpoint,
        scoped_headers.clone(),
        api::METHOD_DEPLOYMENT_IDENTITY_SELF,
        json!({"scope":f.scope()}),
    )
    .await
    .unwrap();
    assert_eq!(self_view["universes"].as_array().unwrap().len(), 1);
    assert_eq!(self_view["universes"][0]["scope"], json!(f.scope()));
    assert_eq!(
        rpc(
            &f.endpoint,
            scoped_headers,
            api::METHOD_DEPLOYMENT_IDENTITY_APPLY,
            json!(AccessChange::SetPrincipalStatus {
                id: user,
                status: PrincipalStatus::Disabled
            })
        )
        .await,
        Err(AgentApiErrorKind::Rejected)
    );
    assert_eq!(
        f.access.principal(user).await?.unwrap().status,
        PrincipalStatus::Active
    );
    let provider = format!("provider-{}", Uuid::new_v4());
    let binding = format!("binding-{}", Uuid::new_v4());
    rpc(&f.endpoint, h.clone(), api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT, json!({
        "providerId":provider,"displayName":marker,"metadata":{"sensitive":marker},
        "controllerConnection":{"endpoint":format!("http://127.0.0.1:1/{marker}"),"transport":{"type":"http"}}
    })).await.unwrap();
    rpc(&f.endpoint, h.clone(), api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT, json!({"universeId":universe,"bindingId":binding,"providerId":provider,"status":"enabled","metadata":{"sensitive":marker}})).await.unwrap();
    rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST,
        json!({"universeId":universe}),
    )
    .await
    .unwrap();
    rpc(&f.endpoint, h.clone(), api::METHOD_DEPLOYMENT_ENVIRONMENTS_ADOPT, json!({"universeId":universe,"requestId":format!("adopt-{}",Uuid::new_v4()),"bindingId":binding,"sourceTarget":marker,"takeOwnership":true,"metadata":{"sensitive":marker}})).await.unwrap();
    let created_key = rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_API_KEYS_CREATE,
        json!({"scope":AccessScope::Deployment,"principalId":service,"displayName":marker}),
    )
    .await
    .unwrap();
    let minted_secret = created_key["secret"].as_str().unwrap();
    rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_API_KEYS_REVOKE,
        json!({"scope":AccessScope::Deployment,"keyPrefix":created_key["apiKey"]["keyPrefix"]}),
    )
    .await
    .unwrap();
    let group = Uuid::new_v4();
    rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_IDENTITY_APPLY,
        json!(AccessChange::CreateGroup {
            id: group,
            display_name: marker.clone()
        }),
    )
    .await
    .unwrap();
    // Failures are not reported as successful deployment operations.
    let missing = format!("missing-{}", Uuid::new_v4());
    assert_eq!(
        rpc(
            &f.endpoint,
            h.clone(),
            api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ,
            json!({"providerId":missing})
        )
        .await,
        Err(AgentApiErrorKind::NotFound)
    );
    rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_UNIVERSES_DELETE,
        json!({"universeId":universe}),
    )
    .await
    .unwrap();
    rpc(
        &f.endpoint,
        h.clone(),
        api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE,
        json!({"providerId":provider}),
    )
    .await
    .unwrap();

    let rows: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(a) FROM access_audit_events a ORDER BY audit_id")
            .fetch_all(&f.pool)
            .await?;
    let text = json!(rows).to_string();
    assert!(
        !text.contains(&marker),
        "audit must exclude request bodies, display names, endpoints, metadata and raw method names"
    );
    assert!(
        !text.contains(minted_secret),
        "audit must exclude generated credentials"
    );
    assert!(
        !text.contains(service_key.secret.expose()),
        "audit must exclude authentication credentials"
    );
    for method in [
        api::METHOD_DEPLOYMENT_UNIVERSES_CREATE,
        api::METHOD_DEPLOYMENT_UNIVERSES_DELETE,
        api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT,
        api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE,
        api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT,
        api::METHOD_DEPLOYMENT_ENVIRONMENTS_ADOPT,
        api::METHOD_DEPLOYMENT_IDENTITY_APPLY,
        api::METHOD_DEPLOYMENT_API_KEYS_CREATE,
        api::METHOD_DEPLOYMENT_API_KEYS_REVOKE,
    ] {
        let completed = rows
            .iter()
            .find(|r| {
                r["method"] == method
                    && r["stage"] == "completion"
                    && r["outcome"] == "succeeded"
                    && r["identity"]["authenticatedPrincipal"] == service.to_string()
            })
            .expect(method);
        assert_eq!(
            completed["identity"]["actingPrincipal"],
            f.admin.id.to_string()
        );
        assert!(
            rows.iter()
                .any(|r| r["attempt_id"] == completed["attempt_id"]
                    && r["stage"] == "admission"
                    && r["outcome"] == "allowed")
        );
    }
    assert!(
        rows.iter()
            .any(|r| r["target"]["universeId"] == universe.to_string()
                && r["method"] == api::METHOD_DEPLOYMENT_UNIVERSES_DELETE
                && r["outcome"] == "succeeded"),
        "purge must retain its own target and attribution"
    );
    assert!(
        !rows.iter().any(|r| r["target"]["providerId"] == missing),
        "a routine missing lookup is not a security event"
    );
    assert!(
        !rows
            .iter()
            .any(|r| r["method"] == api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST),
        "successful administrative inventory is quiet"
    );
    for completed in rows
        .iter()
        .filter(|r| r["stage"] == "completion" && r["outcome"] == "succeeded")
    {
        assert_eq!(
            rows.iter()
                .filter(|r| r["attempt_id"] == completed["attempt_id"])
                .count(),
            2,
            "significant deployment mutation has exactly admission and completion"
        );
    }
    assert_eq!(
        rows.iter()
            .filter(|r| r["method"] == api::METHOD_SESSION_RENAME
                && r["identity"]["actingPrincipal"] == user.to_string())
            .count(),
        1,
        "a rejected action has one denial"
    );
    assert!(rows.iter().any(|r| r["method"].is_null()
        && r["stage"] == "authentication"
        && r["identity"]["authenticatedPrincipal"].is_null()
        && r["identity"]["actingPrincipal"].is_null()));
    assert!(
        rows.iter().any(|r| r["stage"] == "authentication"
            && r["identity"]["authenticatedPrincipal"] == user.to_string()
            && r["identity"]["actingPrincipal"].is_null()),
        "untrusted impersonation must not acquire the claimed actor"
    );
    assert!(
        rows.iter()
            .any(|r| r["method"] == api::METHOD_DEPLOYMENT_IDENTITY_DIRECTORY
                && r["outcome"] == "denied"
                && r["identity"]["actingPrincipal"] == user.to_string())
    );

    // An already captured deployment context cannot outlive its service key.
    f.keys
        .revoke_managed_key(
            f.admin.id,
            AccessScope::Deployment,
            AccessScope::Deployment,
            &service_key.record.key_prefix,
            20,
        )
        .await?;
    assert_eq!(
        with_request_context(
            captured,
            f.deployment
                .list_universes(api::DeploymentUniverseListParams {})
        )
        .await
        .unwrap_err()
        .kind,
        AgentApiErrorKind::Rejected
    );
    // Audit admission is mandatory before starting a privileged side effect.
    let blocked_universe = Uuid::new_v4();
    sqlx::query("ALTER TABLE access_audit_events RENAME TO unavailable_access_audit_events")
        .execute(&f.pool)
        .await?;
    let result = with_request_context(
        local_context(f.admin.clone(), AccessScope::Deployment),
        f.deployment
            .create_universe(api::DeploymentUniverseCreateParams {
                universe_id: blocked_universe.to_string(),
            }),
    )
    .await;
    sqlx::query("ALTER TABLE unavailable_access_audit_events RENAME TO access_audit_events")
        .execute(&f.pool)
        .await?;
    assert_eq!(result.unwrap_err().kind, AgentApiErrorKind::Internal);
    assert!(!store_pg::universe_exists(&f.pool, blocked_universe).await?);
    println!(
        "Deployment admission/outcome, authentication denial, attribution, redaction, deletion survival and audit failure checks passed"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly selected disposable PostgreSQL/Temporal; serialize; no providers"]
async fn routine_traffic_is_quiet_and_noop_revocation_does_not_change_history() -> anyhow::Result<()>
{
    let f = Fixture::new().await?;
    let session = f.session().await;
    let admin_key = f.key(f.admin.id, AccessScope::Deployment).await;
    let admin_headers = headers(&admin_key, Some(f.universe), None);
    let service = f.principal(PrincipalKind::Service).await;
    f.change(AccessChange::AssignCapability {
        assignment: CapabilityAssignment {
            scope: f.scope(),
            principal_id: service,
            capability: ServiceCapability::LeaseCredentials,
        },
    })
    .await;
    let service_key = f.key(service, f.scope()).await;
    let grant = format!("audit-grant-{}", Uuid::new_v4());
    rpc(
        &f.endpoint,
        admin_headers.clone(),
        api::METHOD_AUTH_GRANTS_IMPORT,
        json!({"grantId":grant,"token":"disposable-fixture-token","exposure":"retrievable"}),
    )
    .await
    .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_events")
        .fetch_one(&f.pool)
        .await?;
    for _ in 0..5 {
        for (method, params) in [
            (
                api::METHOD_SESSION_EVENTS_READ,
                json!({"sessionId":session,"afterSeq":0}),
            ),
            (
                api::METHOD_SESSION_RENAME,
                json!({"sessionId":session,"displayName":"Renamed"}),
            ),
            (api::METHOD_PROFILES_LIST, json!({})),
            (api::METHOD_ACCESS_READ, json!({})),
            (
                api::METHOD_DEPLOYMENT_IDENTITY_SELF,
                json!({"scope":f.scope()}),
            ),
            (
                api::METHOD_DEPLOYMENT_IDENTITY_DIRECTORY,
                json!({"scope":f.scope()}),
            ),
            (
                api::METHOD_DEPLOYMENT_API_KEYS_LIST,
                json!({"scope":f.scope()}),
            ),
            (api::METHOD_DEPLOYMENT_UNIVERSES_LIST, json!({})),
        ] {
            let caller = if api::is_deployment_method(method) {
                headers(&admin_key, None, None)
            } else {
                admin_headers.clone()
            };
            rpc(&f.endpoint, caller, method, params).await.unwrap();
        }
        rpc(
            &f.endpoint,
            headers(&service_key, None, None),
            api::METHOD_AUTH_GRANTS_LEASE,
            json!({"grantId":grant}),
        )
        .await
        .unwrap();
        with_request_context(
            local_context(f.admin.clone(), f.scope()),
            f.api.list_profiles(api::ProfileListParams {}),
        )
        .await?;
    }
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_events")
        .fetch_one(&f.pool)
        .await?;
    assert_eq!(
        before, after,
        "50 successful routine calls must add no access-audit events"
    );

    let first = f
        .keys
        .revoke_managed_key(
            f.admin.id,
            AccessScope::Deployment,
            f.scope(),
            &service_key.record.key_prefix,
            20,
        )
        .await?
        .unwrap();
    let revision = f
        .access
        .effective_access(f.admin.id, f.scope())
        .await?
        .policy_revision;
    let again = f
        .keys
        .revoke_managed_key(
            f.admin.id,
            AccessScope::Deployment,
            f.scope(),
            &service_key.record.key_prefix,
            30,
        )
        .await?
        .unwrap();
    assert_eq!(first.revoked_at_ms, again.revoked_at_ms);
    assert_eq!(
        revision,
        f.access
            .effective_access(f.admin.id, f.scope())
            .await?
            .policy_revision
    );
    let changes: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_changes WHERE event->>'operation'='key_revoked' AND event->>'keyPrefix'=$1")
        .bind(&service_key.record.key_prefix).fetch_one(&f.pool).await?;
    assert_eq!(
        changes, 1,
        "idempotent revocation adds no duplicate change fact"
    );
    // The same routine method still records a denial after credential revocation.
    assert_eq!(
        rpc(
            &f.endpoint,
            headers(&service_key, None, None),
            api::METHOD_AUTH_GRANTS_LEASE,
            json!({"grantId":grant})
        )
        .await,
        Err(AgentApiErrorKind::Rejected)
    );
    let denied: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_events")
        .fetch_one(&f.pool)
        .await?;
    assert_eq!(denied, after + 1, "one denial for the revoked credential");
    Ok(())
}
