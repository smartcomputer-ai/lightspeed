//! Configured tokens and universe resolution against a disposable schema. No .env,
//! Temporal, model providers or external credentials are used.
use std::{panic::AssertUnwindSafe, str::FromStr};

use api::{AccessScope, Attribution};
use api::{AgentApiError, AgentApiErrorKind};
use axum::http::HeaderMap;
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::PgStore;
use temporal_runtime::config::RpcTokens;
use temporal_runtime::gateway::authentication::{
    ACTOR_HEADER, UNIVERSE_HEADER, request_context, resolve_caller,
};
use temporal_runtime::gateway::request_context::RequestContext;
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn tokens_reach_every_universe_with_explicit_targets() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("lightspeed_auth_test_{}", Uuid::new_v4().simple());
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA \"{schema}\""
        ))))
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
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA \"{schema}\" CASCADE"
        ))))
        .await
        .unwrap();
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn authenticate(
    tokens: &RpcTokens,
    pool: &sqlx::PgPool,
    secret: &str,
    universe: Option<&str>,
    actor: Option<&str>,
    method: &str,
) -> Result<RequestContext, AgentApiError> {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {secret}").parse().unwrap());
    if let Some(universe) = universe {
        headers.insert(UNIVERSE_HEADER, universe.parse().unwrap());
    }
    if let Some(actor) = actor {
        headers.insert(ACTOR_HEADER, actor.parse().unwrap());
    }
    request_context(resolve_caller(tokens, pool, &headers).await?, method)
}
async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    let secrets = [
        ("platform", "platform-secret-with-at-least-32-characters"),
        ("operator", "operator-secret-with-at-least-32-characters"),
    ];
    let tokens = RpcTokens::from_configuration(
        secrets.map(|(name, secret)| {
            (
                format!("LIGHTSPEED_RPC_TOKEN_{}", name.to_uppercase()),
                secret.into(),
            )
        }),
        |_| unreachable!(),
    )
    .unwrap();
    let universes = [Uuid::new_v4(), Uuid::new_v4()];
    for universe in universes {
        store_pg::create_universe(pool, universe).await.unwrap();
    }
    for (name, secret) in secrets {
        for universe in universes {
            for reference in [
                universe.to_string(),
                store_pg::default_universe_handle(universe),
            ] {
                for actor in [None, Some("alice"), Some("bob")] {
                    for method in [
                        "session/read",
                        "session/delete",
                        "transcriptions/read",
                        "transcriptions/cancel",
                        "auth/grants/lease",
                    ] {
                        let context =
                            authenticate(&tokens, pool, secret, Some(&reference), actor, method)
                                .await
                                .unwrap();
                        assert_eq!(
                            context.scope,
                            AccessScope::Universe {
                                universe_id: universe
                            }
                        );
                        assert_eq!(
                            context.attribution(),
                            actor.map_or(Attribution::Token { name: name.into() }, |id| {
                                Attribution::Actor { id: id.into() }
                            })
                        );
                    }
                }
            }
        }
        for method in ["initialize", "deployment/universes/list"] {
            assert_eq!(
                authenticate(&tokens, pool, secret, None, None, method)
                    .await
                    .unwrap()
                    .scope,
                AccessScope::Deployment
            );
        }
        assert_eq!(
            authenticate(&tokens, pool, secret, None, None, "session/read")
                .await
                .unwrap_err()
                .kind,
            AgentApiErrorKind::InvalidRequest
        );
        assert_eq!(
            authenticate(&tokens, pool, secret, Some("missing"), None, "session/read")
                .await
                .unwrap_err()
                .kind,
            AgentApiErrorKind::NotFound
        );
        assert_eq!(
            authenticate(
                &tokens,
                pool,
                secret,
                None,
                None,
                "deployment/api-keys/list"
            )
            .await
            .unwrap_err()
            .kind,
            AgentApiErrorKind::InvalidRequest
        );
    }
    let before = store_pg::default_universe_handle(universes[0]);
    store_pg::put_universe_handle(pool, universes[0], "renamed")
        .await
        .unwrap();
    assert!(
        authenticate(
            &tokens,
            pool,
            secrets[0].1,
            Some("renamed"),
            None,
            "session/read"
        )
        .await
        .is_ok()
    );
    assert_eq!(
        authenticate(
            &tokens,
            pool,
            secrets[0].1,
            Some(&before),
            None,
            "session/read"
        )
        .await
        .unwrap_err()
        .kind,
        AgentApiErrorKind::NotFound
    );
    store_pg::delete_universe(pool, universes[0]).await.unwrap();
    assert_eq!(
        authenticate(
            &tokens,
            pool,
            secrets[0].1,
            Some("renamed"),
            None,
            "session/read"
        )
        .await
        .unwrap_err()
        .kind,
        AgentApiErrorKind::NotFound
    );
    // Deleting any universe does not revoke deployment component credentials.
    assert!(
        authenticate(&tokens, pool, secrets[0].1, None, None, "initialize")
            .await
            .is_ok()
    );
}
