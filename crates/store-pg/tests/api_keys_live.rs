use std::{collections::BTreeSet, panic::AssertUnwindSafe, str::FromStr};

use api::{AccessScope, Attribution, MethodGroup};
use auth::{ApiKeySpec, mint_api_key};
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgApiKeyStore, PgStore};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn keys_persist_their_scope_groups_and_actor_flag() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!("lightspeed_api_keys_test_{}", Uuid::new_v4().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create isolated schema");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .expect("parse Postgres URL")
                .options([("search_path", schema.as_str())]),
        )
        .await
        .expect("connect to isolated schema");
    let outcome = AssertUnwindSafe(exercise(&pool)).catch_unwind().await;
    pool.close().await;
    admin
        .execute(format!("DROP SCHEMA \"{schema}\" CASCADE").as_str())
        .await
        .expect("drop isolated schema");
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.expect("apply migrations");

    let left_universe = Uuid::new_v4();
    let right_universe = Uuid::new_v4();
    store_pg::create_universe(pool, left_universe)
        .await
        .expect("create left universe");
    store_pg::create_universe(pool, right_universe)
        .await
        .expect("create right universe");
    let left = AccessScope::Universe {
        universe_id: left_universe,
    };
    let right = AccessScope::Universe {
        universe_id: right_universe,
    };

    let api_keys = PgApiKeyStore::new(pool.clone());
    let minted = |scope, groups: Option<&[MethodGroup]>, assert_actor, created_at_ms| {
        mint_api_key(
            ApiKeySpec {
                scope,
                groups: groups.map(|groups| groups.iter().copied().collect()),
                assert_actor,
                created_by: Attribution::Actor {
                    id: "operator".into(),
                },
                display_name: Some("test key".into()),
            },
            created_at_ms,
        )
        .expect("mint")
    };
    let left_key = minted(left, None, false, 10);
    let right_key = minted(right, Some(&[MethodGroup::BlobsPut]), false, 11);
    let gate_key = minted(AccessScope::Deployment, None, true, 12);
    for key in [&left_key, &right_key, &gate_key] {
        api_keys
            .create_api_key(&key.key_hash, &key.record)
            .await
            .expect("create api key");
    }
    // A prefix is unique: the minter retries rather than overwriting.
    assert!(matches!(
        api_keys
            .create_api_key(&minted(left, None, false, 13).key_hash, &left_key.record)
            .await,
        Err(auth::ApiKeyError::AlreadyExists { .. })
    ));

    assert_eq!(
        api_keys.list_api_keys(Some(left)).await.expect("list left"),
        vec![left_key.record.clone()]
    );
    let deployment_keys = api_keys
        .list_api_keys(Some(AccessScope::Deployment))
        .await
        .expect("list deployment keys");
    assert_eq!(deployment_keys, vec![gate_key.record.clone()]);
    assert!(deployment_keys[0].assert_actor);
    assert_eq!(
        deployment_keys[0].groups,
        MethodGroup::ALL.into_iter().collect::<BTreeSet<_>>()
    );

    // Resolution is a read; the usage stamp is coarse so a busy key is not
    // rewritten on every request.
    let resolved = api_keys
        .resolve_api_key(&right_key.key_hash, 1_000)
        .await
        .expect("resolve right key")
        .expect("right key is active");
    assert_eq!(resolved.key_prefix, right_key.record.key_prefix);
    assert_eq!(
        resolved.groups,
        [MethodGroup::BlobsPut].into_iter().collect::<BTreeSet<_>>()
    );
    assert_eq!(
        resolved.created_by,
        Attribution::Actor {
            id: "operator".into()
        }
    );
    let last_used = async || {
        api_keys
            .list_api_keys(Some(right))
            .await
            .expect("list right")[0]
            .last_used_at_ms
    };
    assert_eq!(last_used().await, Some(1_000));
    api_keys
        .resolve_api_key(&right_key.key_hash, 2_000)
        .await
        .expect("resolve within the usage resolution");
    assert_eq!(last_used().await, Some(1_000));
    let later = 1_000 + auth::API_KEY_LAST_USED_RESOLUTION_MS;
    api_keys
        .resolve_api_key(&right_key.key_hash, later)
        .await
        .expect("resolve after the usage resolution");
    assert_eq!(last_used().await, Some(later));

    // Revocation keeps its first time and stops resolution.
    let revoked = api_keys
        .revoke_api_key(&left_key.record.key_prefix, 20)
        .await
        .expect("revoke left key")
        .expect("left key exists");
    assert_eq!(revoked.revoked_at_ms, Some(20));
    let revoked_again = api_keys
        .revoke_api_key(&left_key.record.key_prefix, 30)
        .await
        .expect("revoke left key again")
        .expect("left key still exists");
    assert_eq!(revoked_again.revoked_at_ms, Some(20));
    assert!(
        api_keys
            .resolve_api_key(&left_key.key_hash, later)
            .await
            .expect("resolve revoked key")
            .is_none()
    );
    assert!(
        api_keys
            .revoke_api_key("lsk_unknown00", 40)
            .await
            .expect("revoke unknown")
            .is_none()
    );

    // Deleting a universe removes its keys.
    store_pg::delete_universe(pool, left_universe)
        .await
        .expect("delete left universe");
    store_pg::delete_universe(pool, right_universe)
        .await
        .expect("delete right universe");
    assert!(
        api_keys
            .resolve_api_key(&right_key.key_hash, later)
            .await
            .expect("resolve after delete")
            .is_none()
    );
    api_keys
        .revoke_api_key(&gate_key.record.key_prefix, later)
        .await
        .expect("revoke gate key");
}
