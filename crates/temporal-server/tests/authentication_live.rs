//! Request identity and key authorization against a disposable schema; no .env,
//! Temporal, model providers or external credentials are used.
use access::*;
use auth::{ApiKeyStore, CreateApiKey, MintedApiKey};
use axum::http::HeaderMap;
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{panic::AssertUnwindSafe, str::FromStr};
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore};
use temporal_server::gateway::authentication::{PRINCIPAL_HEADER, UNIVERSE_HEADER, authenticate};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn request_identity_keys_assertions_and_revocation() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("lightspeed_auth_test_{}", Uuid::new_v4().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(
            PgConnectOptions::from_str(&url)
                .unwrap()
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    let outcome = AssertUnwindSafe(exercise(&pool)).catch_unwind().await;
    pool.close().await;
    admin
        .execute(format!("DROP SCHEMA \"{schema}\" CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn headers(key: &MintedApiKey, universe: Option<Uuid>, asserted: Option<Uuid>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", key.secret.expose()).parse().unwrap(),
    );
    if let Some(id) = universe {
        headers.insert(UNIVERSE_HEADER, id.to_string().parse().unwrap());
    }
    if let Some(id) = asserted {
        headers.insert(PRINCIPAL_HEADER, format!("user:{id}").parse().unwrap());
    }
    headers
}
async fn issue(
    keys: &PgApiKeyStore,
    scope: AccessScope,
    principal: Uuid,
    actor: Uuid,
) -> MintedApiKey {
    let key = auth::mint_api_key(scope, principal, actor, None, 10);
    keys.create_api_key(CreateApiKey {
        authority_scope: access::AccessScope::Deployment,
        key_hash: key.key_hash.clone(),
        record: key.record.clone(),
    })
    .await
    .unwrap();
    key
}
async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    let identities = PgAccessStore::new(pool.clone());
    let keys = PgApiKeyStore::new(pool.clone());
    let disabled_bootstrap = Uuid::new_v4();
    sqlx::query("INSERT INTO access_principals(principal_id,kind,status,display_name,created_at_ms) VALUES($1,'user','disabled','Disabled bootstrap',0)")
        .bind(disabled_bootstrap).execute(pool).await.unwrap();
    assert!(matches!(
        identities
            .bootstrap(disabled_bootstrap, "Disabled bootstrap".into(), 1)
            .await,
        Err(AccessError::Denied)
    ));
    let admin = Uuid::new_v4();
    identities
        .bootstrap(admin, "Administrator".into(), 1)
        .await
        .unwrap();
    let universe = Uuid::new_v4();
    let other = Uuid::new_v4();
    for id in [universe, other] {
        identities
            .apply(
                admin,
                AccessChange::CreateUniverse {
                    universe_id: id,
                    slug: None,
                },
                2,
            )
            .await
            .unwrap();
    }
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let user = Uuid::new_v4();
    let service = Uuid::new_v4();
    let managed = Uuid::new_v4();
    let outsider = Uuid::new_v4();
    for (id, kind, management_scope) in [
        (user, PrincipalKind::User, AccessScope::Deployment),
        (service, PrincipalKind::Service, AccessScope::Deployment),
        (managed, PrincipalKind::Service, scope),
        (outsider, PrincipalKind::User, AccessScope::Deployment),
    ] {
        identities
            .apply(
                admin,
                AccessChange::CreatePrincipal {
                    id,
                    kind,
                    management_scope,
                    display_name: format!("Identity {id}"),
                },
                3,
            )
            .await
            .unwrap();
    }
    let retry = AccessChange::CreatePrincipal {
        id: service,
        kind: PrincipalKind::Service,
        management_scope: AccessScope::Deployment,
        display_name: format!("Identity {service}"),
    };
    assert!(!identities.apply(admin, retry, 4).await.unwrap().changed);
    assert!(matches!(
        identities
            .apply(
                admin,
                AccessChange::CreatePrincipal {
                    id: service,
                    kind: PrincipalKind::User,
                    management_scope: AccessScope::Deployment,
                    display_name: format!("Identity {service}")
                },
                4
            )
            .await,
        Err(AccessError::Conflict)
    ));
    let membership = RoleAssignment {
        scope,
        subject: Subject::Principal(user),
        role: Role::Viewer,
    };
    identities
        .apply(
            admin,
            AccessChange::AssignRole {
                assignment: membership,
            },
            4,
        )
        .await
        .unwrap();
    let user_key = issue(&keys, scope, user, user).await;
    let service_key = issue(&keys, AccessScope::Deployment, service, admin).await;
    let root_key = issue(&keys, AccessScope::Deployment, admin, admin).await;
    let read = api::METHOD_SESSION_READ;
    let lease = api::METHOD_AUTH_GRANTS_LEASE;
    let user_headers = headers(&user_key, None, None);
    let context = authenticate(&keys, &identities, &user_headers, read, 20)
        .await
        .unwrap();
    assert_eq!(context.acting_principal().id, user);
    assert_eq!(context.authenticated_principal.id, user);
    assert_eq!(context.credential_scope, scope);
    assert!(matches!(
        context.authentication,
        AuthenticationReference::ApiKey { .. }
    ));
    // Header claims alone, wrong scopes, missing membership and duplicate credentials fail closed.
    let mut bare = HeaderMap::new();
    bare.insert(UNIVERSE_HEADER, universe.to_string().parse().unwrap());
    bare.insert(PRINCIPAL_HEADER, user.to_string().parse().unwrap());
    assert!(
        authenticate(&keys, &identities, &bare, read, 20)
            .await
            .is_err()
    );
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&user_key, Some(other), None),
            read,
            20
        )
        .await
        .is_err()
    );
    assert!(
        authenticate(
            &keys,
            &identities,
            &user_headers,
            api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
            20
        )
        .await
        .is_err()
    );
    let mut duplicate = user_headers.clone();
    duplicate.append("authorization", "Bearer lsk_other".parse().unwrap());
    assert!(
        authenticate(&keys, &identities, &duplicate, read, 20)
            .await
            .is_err()
    );
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&service_key, Some(universe), None),
            read,
            20
        )
        .await
        .is_err()
    );
    // Service kind and administrator role cannot substitute for a service capability.
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&service_key, Some(universe), None),
            lease,
            20
        )
        .await
        .is_err()
    );
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&root_key, Some(universe), None),
            lease,
            20
        )
        .await
        .is_err()
    );
    let assertion = CapabilityAssignment {
        scope,
        principal_id: service,
        capability: Capability::AssertUser,
    };
    let asserted_headers = headers(&service_key, Some(universe), Some(user));
    assert!(
        authenticate(&keys, &identities, &asserted_headers, read, 20)
            .await
            .is_err()
    );
    identities
        .apply(
            admin,
            AccessChange::AssignCapability {
                assignment: assertion,
            },
            21,
        )
        .await
        .unwrap();
    let context = authenticate(&keys, &identities, &asserted_headers, read, 22)
        .await
        .unwrap();
    assert_eq!(context.acting_principal().id, user);
    assert_eq!(context.authenticated_principal.id, service);
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&service_key, Some(universe), Some(outsider)),
            read,
            22
        )
        .await
        .is_err()
    );
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&service_key, Some(other), Some(user)),
            read,
            22
        )
        .await
        .is_err()
    );
    identities
        .apply(
            admin,
            AccessChange::AssignCapability {
                assignment: CapabilityAssignment {
                    scope,
                    principal_id: service,
                    capability: Capability::LeaseCredentials,
                },
            },
            23,
        )
        .await
        .unwrap();
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&service_key, Some(universe), None),
            lease,
            24
        )
        .await
        .is_ok()
    );
    assert!(
        authenticate(&keys, &identities, &asserted_headers, lease, 24)
            .await
            .is_err(),
        "service powers never union with asserted user"
    );
    assert!(
        authenticate(
            &keys,
            &identities,
            &headers(&user_key, None, Some(admin)),
            read,
            24
        )
        .await
        .is_err()
    );
    // Commit membership revocation: the same key and assertion stop admitting immediately.
    identities
        .apply(
            admin,
            AccessChange::RevokeRole {
                assignment: membership,
            },
            25,
        )
        .await
        .unwrap();
    assert!(
        authenticate(&keys, &identities, &user_headers, read, 26)
            .await
            .is_err()
    );
    assert!(
        authenticate(&keys, &identities, &asserted_headers, read, 26)
            .await
            .is_err()
    );
    identities
        .apply(
            admin,
            AccessChange::AssignRole {
                assignment: membership,
            },
            27,
        )
        .await
        .unwrap();
    identities
        .apply(
            admin,
            AccessChange::RevokeCapability {
                assignment: assertion,
            },
            28,
        )
        .await
        .unwrap();
    assert!(
        authenticate(&keys, &identities, &asserted_headers, read, 29)
            .await
            .is_err()
    );
    identities
        .apply(
            admin,
            AccessChange::SetPrincipalStatus {
                id: user,
                status: PrincipalStatus::Disabled,
            },
            30,
        )
        .await
        .unwrap();
    assert!(
        authenticate(&keys, &identities, &user_headers, read, 31)
            .await
            .is_err()
    );
    identities
        .apply(
            admin,
            AccessChange::SetPrincipalStatus {
                id: user,
                status: PrincipalStatus::Active,
            },
            32,
        )
        .await
        .unwrap();
    assert!(
        authenticate(&keys, &identities, &user_headers, read, 33)
            .await
            .is_ok()
    );
    keys.revoke_managed_key(
        user,
        AccessScope::Deployment,
        scope,
        &user_key.record.key_prefix,
        34,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        authenticate(&keys, &identities, &user_headers, read, 35)
            .await
            .is_err()
    );
    // Universe administration can mint a locally managed service, never an integration identity.
    identities
        .apply(
            admin,
            AccessChange::AssignRole {
                assignment: RoleAssignment {
                    scope,
                    subject: Subject::Principal(user),
                    role: Role::Admin,
                },
            },
            36,
        )
        .await
        .unwrap();
    let managed_key = issue(&keys, scope, managed, user).await;
    assert_eq!(managed_key.record.created_by, user);
    assert_eq!(managed_key.record.principal_id, managed);
    for (target, ceiling) in [
        (service, scope),
        (user, AccessScope::Deployment),
        (outsider, scope),
    ] {
        let key = auth::mint_api_key(ceiling, target, user, None, 37);
        assert!(matches!(
            keys.create_api_key(CreateApiKey {
                authority_scope: access::AccessScope::Deployment,
                key_hash: key.key_hash,
                record: key.record
            })
            .await,
            Err(auth::ApiKeyError::Denied)
        ));
    }
    // A universe credential cannot borrow the holder's deployment privileges
    // for key issuance, even after that holder becomes a deployment admin.
    identities
        .apply(
            admin,
            AccessChange::AssignRole {
                assignment: RoleAssignment {
                    scope: AccessScope::Deployment,
                    subject: Subject::Principal(user),
                    role: Role::DeploymentAdmin,
                },
            },
            37,
        )
        .await
        .unwrap();
    for (target, requested_scope) in [
        (service, scope),
        (user, AccessScope::Deployment),
        (managed, AccessScope::Universe { universe_id: other }),
    ] {
        let key = auth::mint_api_key(requested_scope, target, user, None, 37);
        assert!(matches!(
            keys.create_api_key(CreateApiKey {
                authority_scope: scope,
                key_hash: key.key_hash,
                record: key.record
            })
            .await,
            Err(auth::ApiKeyError::Denied)
        ));
    }
    assert!(
        keys.list_managed_keys(user, scope, AccessScope::Deployment)
            .await
            .is_err()
    );
    assert!(
        keys.revoke_managed_key(
            user,
            scope,
            AccessScope::Deployment,
            &service_key.record.key_prefix,
            37
        )
        .await
        .is_err()
    );
    let local = identities
        .initialize_local_development(universe, 38)
        .await
        .unwrap();
    assert_eq!(
        identities
            .initialize_local_development(universe, 39)
            .await
            .unwrap()
            .id,
        local.id
    );
    identities
        .apply(
            admin,
            AccessChange::SetPrincipalStatus {
                id: local.id,
                status: PrincipalStatus::Disabled,
            },
            40,
        )
        .await
        .unwrap();
    assert!(matches!(
        identities.initialize_local_development(universe, 41).await,
        Err(AccessError::Denied)
    ));
    let audit_count:i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_changes WHERE event->>'operation' IN ('key_created','key_revoked')").fetch_one(pool).await.unwrap();
    assert!(audit_count >= 5);
}
