//! Key authentication against a disposable schema: scope, universe header,
//! method groups, asserted actors and revocation. No .env,
//! Temporal, model providers or external credentials are used.
use std::collections::BTreeSet;
use std::{panic::AssertUnwindSafe, str::FromStr};

use api::AgentApiErrorKind;
use api::{AccessScope, Attribution, MethodGroup};
use auth::{ApiKeySpec, MintedApiKey};
use axum::http::HeaderMap;
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgApiKeyStore, PgStore};
use temporal_server::gateway::authentication::{ACTOR_HEADER, UNIVERSE_HEADER, authenticate};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn keys_scope_groups_actors_and_revocation() {
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

fn headers(key: &MintedApiKey, universe: Option<Uuid>, actor: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", key.secret.expose()).parse().unwrap(),
    );
    if let Some(id) = universe {
        headers.insert(UNIVERSE_HEADER, id.to_string().parse().unwrap());
    }
    if let Some(actor) = actor {
        headers.insert(ACTOR_HEADER, actor.parse().unwrap());
    }
    headers
}

async fn issue(
    keys: &PgApiKeyStore,
    scope: AccessScope,
    groups: Option<&[MethodGroup]>,
    assert_actor: bool,
) -> MintedApiKey {
    let key = auth::mint_api_key(
        ApiKeySpec {
            scope,
            groups: groups.map(|groups| groups.iter().copied().collect::<BTreeSet<_>>()),
            assert_actor,
            created_by: Attribution::Local,
            display_name: None,
        },
        10,
    )
    .unwrap();
    keys.create_api_key(&key.key_hash, &key.record)
        .await
        .unwrap();
    key
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    let keys = PgApiKeyStore::new(pool.clone());
    let universe = Uuid::new_v4();
    let other = Uuid::new_v4();
    for id in [universe, other] {
        store_pg::create_universe(pool, id).await.unwrap();
    }
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let universe_key = issue(&keys, scope, None, false).await;
    let deployment_key = issue(&keys, AccessScope::Deployment, None, true).await;
    let connector = issue(
        &keys,
        AccessScope::Deployment,
        Some(&[
            MethodGroup::ChannelsInbound,
            MethodGroup::BlobsPut,
            MethodGroup::DeploymentChannels,
        ]),
        false,
    )
    .await;

    let allowed = |headers: HeaderMap, method: &'static str| {
        let keys = keys.clone();
        async move {
            authenticate(&keys, &headers, method, 20)
                .await
                .unwrap_or_else(|refusal| panic!("{method} refused: {refusal}"))
        }
    };
    let refused = |headers: HeaderMap, method: &'static str| {
        let keys = keys.clone();
        async move {
            authenticate(&keys, &headers, method, 20)
                .await
                .expect_err(&format!("{method} allowed"))
        }
    };

    // A universe key reaches its universe and takes no universe header,
    // not even its own.
    let context = allowed(headers(&universe_key, None, None), "session/read").await;
    assert_eq!(context.scope, scope);
    assert_eq!(context.actor, None);
    assert_eq!(
        context.attribution(),
        Attribution::Key {
            prefix: universe_key.record.key_prefix.clone()
        }
    );
    for selected in [universe, other] {
        let refusal = refused(headers(&universe_key, Some(selected), None), "session/read").await;
        assert_eq!(refusal.kind, AgentApiErrorKind::Forbidden);
    }
    // It holds no deployment group and never addresses the deployment.
    refused(
        headers(&universe_key, None, None),
        "deployment/universes/list",
    )
    .await;

    // A deployment key names the universe of a universe method, and takes
    // none on a deployment method.
    refused(headers(&deployment_key, None, None), "session/read").await;
    let context = allowed(headers(&deployment_key, Some(other), None), "session/read").await;
    assert_eq!(context.scope, AccessScope::Universe { universe_id: other });
    let context = allowed(
        headers(&deployment_key, None, None),
        "deployment/universes/list",
    )
    .await;
    assert_eq!(context.scope, AccessScope::Deployment);
    refused(
        headers(&deployment_key, Some(other), None),
        "deployment/universes/list",
    )
    .await;

    // Only a key allowed to assert actors names one, and only a well-formed
    // one.
    let context = allowed(
        headers(&deployment_key, Some(universe), Some("user-42")),
        "session/start",
    )
    .await;
    assert_eq!(context.actor.as_deref(), Some("user-42"));
    assert_eq!(
        context.attribution(),
        Attribution::Actor {
            id: "user-42".into()
        }
    );
    let refusal = refused(
        headers(&universe_key, None, Some("user-42")),
        "session/start",
    )
    .await;
    assert_eq!(refusal.kind, AgentApiErrorKind::Forbidden);
    refused(
        headers(&deployment_key, Some(universe), Some(&"x".repeat(300))),
        "session/start",
    )
    .await;
    let mut removed_header = headers(&deployment_key, Some(universe), None);
    removed_header.insert("x-lightspeed-principal", "user:old".parse().unwrap());
    refused(removed_header, "session/start").await;

    // A connector's key calls its groups and `initialize`, nothing else.
    allowed(
        headers(&connector, Some(universe), None),
        "channels/inbound/admit",
    )
    .await;
    allowed(headers(&connector, Some(universe), None), "blobs/put").await;
    allowed(headers(&connector, Some(universe), None), "initialize").await;
    allowed(
        headers(&connector, None, None),
        "deployment/channels/accounts/list",
    )
    .await;
    for method in ["session/read", "blobs/read", "auth/grants/lease"] {
        let refusal = refused(headers(&connector, Some(universe), None), method).await;
        assert_eq!(refusal.kind, AgentApiErrorKind::Forbidden);
    }

    // Unknown methods, missing or duplicated credentials and revoked keys
    // are refused before anyone is identified.
    let refusal = refused(headers(&universe_key, None, None), "access/policy/put").await;
    assert_eq!(refusal.kind, AgentApiErrorKind::InvalidRequest);
    let refusal = refused(HeaderMap::new(), "session/read").await;
    assert_eq!(refusal.kind, AgentApiErrorKind::Unauthenticated);
    let mut duplicated = headers(&universe_key, None, None);
    duplicated.append(
        "authorization",
        format!("Bearer {}", universe_key.secret.expose())
            .parse()
            .unwrap(),
    );
    assert_eq!(
        refused(duplicated, "session/read").await.kind,
        AgentApiErrorKind::Unauthenticated
    );
    keys.revoke_api_key(&universe_key.record.key_prefix, 30)
        .await
        .unwrap();
    let refusal = refused(headers(&universe_key, None, None), "session/read").await;
    assert_eq!(refusal.kind, AgentApiErrorKind::Unauthenticated);

    // Deleting a universe removes its keys.
    let scoped = issue(
        &keys,
        AccessScope::Universe { universe_id: other },
        None,
        false,
    )
    .await;
    store_pg::delete_universe(pool, other).await.unwrap();
    assert_eq!(
        refused(headers(&scoped, None, None), "session/read")
            .await
            .kind,
        AgentApiErrorKind::Unauthenticated
    );
}
