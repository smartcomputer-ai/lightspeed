use std::{collections::BTreeSet, panic::AssertUnwindSafe, str::FromStr};

use api::{AccessScope, Attribution, MethodGroup};
use auth::{ApiKeySpec, import_api_key, mint_api_key};
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgApiKeyStore, PgStore};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn keys_keep_their_id_and_authority_while_their_secret_changes() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!("lightspeed_api_keys_test_{}", Uuid::new_v4().simple());
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA \"{schema}\""
        ))))
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
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA \"{schema}\" CASCADE"
        ))))
        .await
        .expect("drop isolated schema");
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.expect("apply migrations");
    upgrade_gives_existing_keys_an_allocated_id(pool).await;

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
    let spec =
        |key_id: Option<&str>, scope, groups: Option<&[MethodGroup]>, assert_actor| ApiKeySpec {
            key_id: key_id.map(str::to_owned),
            scope,
            groups: groups.map(|groups| groups.iter().copied().collect()),
            assert_actor,
            created_by: Attribution::Actor {
                id: "operator".into(),
            },
            display_name: Some("test key".into()),
        };
    let minted = |key_id, scope, groups, assert_actor, created_at_ms| {
        mint_api_key(spec(key_id, scope, groups, assert_actor), created_at_ms).expect("mint")
    };
    // Resolving a key stamps its last use, which these comparisons are not
    // about.
    let unused = |mut record: auth::ApiKeyRecord| {
        record.last_used_at_ms = None;
        record
    };
    let conflict = |result: Result<_, auth::ApiKeyError>| {
        matches!(result, Err(auth::ApiKeyError::Conflict { .. }))
    };

    // A key is created under the id it is given, or an allocated one. An id
    // names one key per scope, so two universes and the deployment may each
    // have their own key of the same id.
    let left_key = minted(Some("ci"), left, None, false, 10);
    let right_key = minted(Some("ci"), right, Some(&[MethodGroup::BlobsPut]), false, 11);
    let gate_key = minted(Some("ci"), AccessScope::Deployment, None, true, 12);
    let unnamed = minted(None, left, None, false, 13);
    for key in [&left_key, &right_key, &gate_key, &unnamed] {
        api_keys
            .create_api_key(&key.key_hash, &key.record)
            .await
            .expect("create api key");
    }
    assert!(auth::is_allocated_api_key_id(&unnamed.record.key_id));
    let duplicate = minted(Some("ci"), left, None, false, 14);
    assert!(matches!(
        api_keys
            .create_api_key(&duplicate.key_hash, &duplicate.record)
            .await,
        Err(auth::ApiKeyError::KeyIdInUse { .. })
    ));
    // A display prefix is unique: a minter generates again rather than
    // overwriting.
    let mut colliding = minted(Some("other"), left, None, false, 14);
    colliding.record.key_prefix = left_key.record.key_prefix.clone();
    assert!(matches!(
        api_keys
            .create_api_key(&colliding.key_hash, &colliding.record)
            .await,
        Err(auth::ApiKeyError::AlreadyExists { .. })
    ));

    assert_eq!(
        api_keys
            .list_api_keys(Some(right))
            .await
            .expect("list right"),
        vec![right_key.record.clone()]
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

    // Declaring a key that is missing creates it under the supplied secret.
    let secret = |n: u8| {
        use base64::Engine as _;
        format!(
            "lsk_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([n; 32])
        )
    };
    let declare = |key_id: &str, scope, groups, assert_actor, n: u8, created_at_ms| {
        import_api_key(
            spec(Some(key_id), scope, groups, assert_actor),
            created_at_ms,
            &secret(n),
        )
        .expect("import")
    };
    let platform = declare(
        "platform",
        AccessScope::Deployment,
        Some(&[MethodGroup::Session]),
        true,
        1,
        20,
    );
    let put = api_keys.put_api_key(&platform).await.unwrap();
    assert!(put.created && !put.secret_replaced);
    assert_eq!(put.record, platform.record);
    assert_eq!(
        api_keys
            .resolve_api_key(&platform.key_hash, 0)
            .await
            .unwrap(),
        Some(platform.record.clone())
    );
    assert_eq!(
        unused(api_keys.verify_api_key(&platform).await.unwrap()),
        platform.record
    );

    // Declaring it again changes nothing, whenever and by whomever.
    let mut again = declare(
        "platform",
        AccessScope::Deployment,
        Some(&[MethodGroup::Session]),
        true,
        1,
        99,
    );
    again.record.created_by = Attribution::Local;
    let put = api_keys.put_api_key(&again).await.unwrap();
    assert!(!put.created && !put.secret_replaced);
    assert_eq!(unused(put.record), platform.record);

    // Its groups and name converge on the declaration; the secret, creator
    // and creation time stay.
    let mut widened = declare(
        "platform",
        AccessScope::Deployment,
        Some(&[MethodGroup::Session, MethodGroup::Profiles]),
        true,
        1,
        99,
    );
    widened.record.display_name = Some("Platform".into());
    let put = api_keys.put_api_key(&widened).await.unwrap();
    assert!(!put.created && !put.secret_replaced);
    let mut expected = platform.record.clone();
    expected.groups = widened.record.groups.clone();
    expected.display_name = Some("Platform".into());
    assert_eq!(unused(put.record), expected);
    assert_eq!(
        api_keys
            .resolve_api_key(&platform.key_hash, 0)
            .await
            .unwrap()
            .map(unused),
        Some(expected.clone())
    );
    // A check that the key is as described writes nothing and notices the
    // difference.
    assert!(matches!(
        api_keys.verify_api_key(&platform).await,
        Err(auth::ApiKeyError::Invalid { .. })
    ));
    assert!(matches!(
        api_keys
            .verify_api_key(&declare(
                "absent",
                AccessScope::Deployment,
                None,
                false,
                9,
                1
            ))
            .await,
        Err(auth::ApiKeyError::Invalid { .. })
    ));

    // A different secret replaces the one the key held, with no overlap.
    let mut replaced = declare(
        "platform",
        AccessScope::Deployment,
        Some(&[MethodGroup::Session, MethodGroup::Profiles]),
        true,
        2,
        99,
    );
    replaced.record.display_name = Some("Platform".into());
    api_keys
        .resolve_api_key(&platform.key_hash, 5_000)
        .await
        .unwrap();
    let put = api_keys.put_api_key(&replaced).await.unwrap();
    assert!(!put.created && put.secret_replaced);
    expected.key_prefix = replaced.record.key_prefix.clone();
    assert_eq!(put.record, expected);
    assert_eq!(put.record.last_used_at_ms, None);
    assert!(
        api_keys
            .resolve_api_key(&platform.key_hash, 0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        api_keys
            .resolve_api_key(&replaced.key_hash, 0)
            .await
            .unwrap()
            .is_some()
    );

    // What a key reaches and whether it asserts actors never change.
    assert!(conflict(
        api_keys
            .put_api_key(&declare(
                "platform",
                AccessScope::Deployment,
                None,
                false,
                2,
                99
            ))
            .await
    ));
    // A secret identifies one key: it cannot be declared for a second one,
    // in this scope or another.
    for (key_id, scope) in [("second", AccessScope::Deployment), ("second", left)] {
        assert!(conflict(
            api_keys
                .put_api_key(&declare(key_id, scope, None, true, 2, 99))
                .await
        ));
    }
    let other = declare("other", AccessScope::Deployment, None, false, 3, 30);
    assert!(api_keys.put_api_key(&other).await.unwrap().created);
    assert!(conflict(
        api_keys
            .put_api_key(&declare(
                "platform",
                AccessScope::Deployment,
                None,
                true,
                3,
                99
            ))
            .await
    ));
    assert!(
        api_keys
            .resolve_api_key(&replaced.key_hash, 0)
            .await
            .unwrap()
            .is_some()
    );

    // A key that was never named takes the id it is declared under when its
    // own secret is supplied, instead of a second key being made.
    let mut adopting = import_api_key(
        spec(Some("left-ci-2"), left, Some(&[MethodGroup::Vfs]), false),
        99,
        unnamed.secret.expose(),
    )
    .unwrap();
    adopting.record.display_name = Some("adopted".into());
    let put = api_keys.put_api_key(&adopting).await.unwrap();
    assert!(!put.created && !put.secret_replaced);
    assert_eq!(put.record.key_id, "left-ci-2");
    assert_eq!(put.record.created_at_ms, unnamed.record.created_at_ms);
    assert_eq!(put.record.groups, adopting.record.groups);
    assert_eq!(api_keys.list_api_keys(Some(left)).await.unwrap().len(), 2);
    // A named key keeps its name: its secret under another id is a conflict.
    assert!(conflict(
        api_keys
            .put_api_key(
                &import_api_key(
                    spec(Some("left-ci-3"), left, None, false),
                    99,
                    unnamed.secret.expose()
                )
                .unwrap()
            )
            .await
    ));

    // Resolution is a read; the usage stamp is coarse so a busy key is not
    // rewritten on every request.
    let resolved = api_keys
        .resolve_api_key(&right_key.key_hash, 1_000)
        .await
        .expect("resolve right key")
        .expect("right key is active");
    assert_eq!(resolved.key_id, "ci");
    assert_eq!(resolved.key_prefix, right_key.record.key_prefix);
    assert_eq!(
        resolved.groups,
        [MethodGroup::BlobsPut].into_iter().collect::<BTreeSet<_>>()
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

    // Revocation addresses one scope's key, keeps its first time and stops
    // resolution; the other keys of that id are untouched.
    let revoked = api_keys
        .revoke_api_key(left, "ci", 20)
        .await
        .expect("revoke left key")
        .expect("left key exists");
    assert_eq!(revoked.revoked_at_ms, Some(20));
    let revoked_again = api_keys
        .revoke_api_key(left, "ci", 30)
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
            .resolve_api_key(&right_key.key_hash, later)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        api_keys
            .revoke_api_key(left, "unknown", 40)
            .await
            .expect("revoke unknown")
            .is_none()
    );
    // A revoked key is never restored and keeps its id, whether its own
    // secret or a new one is declared for it.
    for n in [4, 5] {
        let mut declared = declare("ci", left, None, false, n, 99);
        if n == 4 {
            declared = import_api_key(
                spec(Some("ci"), left, None, false),
                99,
                left_key.secret.expose(),
            )
            .unwrap();
        }
        assert!(conflict(api_keys.put_api_key(&declared).await));
    }
    assert!(conflict(
        api_keys
            .put_api_key(
                &import_api_key(
                    spec(Some("ci-again"), left, None, false),
                    99,
                    left_key.secret.expose()
                )
                .unwrap()
            )
            .await
    ));
    assert!(matches!(
        api_keys.verify_api_key(&left_key).await,
        Err(auth::ApiKeyError::Invalid { .. })
    ));

    // Rotation replaces the secret of the same row, keeping the id,
    // authority and creation metadata.
    let rotated = api_keys
        .rotate_api_key(right, "ci")
        .await
        .expect("rotate")
        .expect("active key");
    let mut expected = right_key.record.clone();
    expected.key_prefix = rotated.record.key_prefix.clone();
    assert_eq!(rotated.record, expected);
    assert_ne!(rotated.record.key_prefix, right_key.record.key_prefix);
    assert_ne!(rotated.secret.expose(), right_key.secret.expose());
    assert_eq!(
        rotated.key_hash,
        auth::api_key_hash(rotated.secret.expose())
    );
    assert!(
        api_keys
            .resolve_api_key(&right_key.key_hash, later)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        api_keys
            .resolve_api_key(&rotated.key_hash, later)
            .await
            .unwrap(),
        Some(expected)
    );
    assert_eq!(api_keys.list_api_keys(Some(right)).await.unwrap().len(), 1);
    for (scope, key_id) in [(left, "ci"), (right, "unknown")] {
        assert!(
            api_keys
                .rotate_api_key(scope, key_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    // Concurrent rotations leave exactly one secret working.
    let (first, second) = tokio::join!(
        api_keys.rotate_api_key(right, "ci"),
        api_keys.rotate_api_key(right, "ci"),
    );
    let mut working = 0;
    for candidate in [first.unwrap().unwrap(), second.unwrap().unwrap()] {
        if api_keys
            .resolve_api_key(&candidate.key_hash, later)
            .await
            .unwrap()
            .is_some()
        {
            working += 1;
        }
    }
    assert_eq!(working, 1);
    assert!(
        api_keys
            .resolve_api_key(&rotated.key_hash, later)
            .await
            .unwrap()
            .is_none()
    );

    // Deleting a universe removes its keys and leaves the deployment's.
    store_pg::delete_universe(pool, left_universe)
        .await
        .expect("delete left universe");
    store_pg::delete_universe(pool, right_universe)
        .await
        .expect("delete right universe");
    assert!(
        api_keys
            .list_api_keys(Some(right))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        api_keys
            .resolve_api_key(&gate_key.key_hash, later)
            .await
            .expect("resolve after delete")
            .is_some()
    );
}

/// Rebuild the key table as the previous revision left it, then apply the
/// key id migration over a key that existed before it.
async fn upgrade_gives_existing_keys_an_allocated_id(pool: &sqlx::PgPool) {
    pool.execute(sqlx::raw_sql(
        r#"
        DROP INDEX api_keys_scope_key_id_idx;
        ALTER TABLE api_keys DROP COLUMN key_id;
        INSERT INTO api_keys (key_hash, key_prefix, groups, created_by, created_at_ms)
        VALUES (repeat('a', 64), 'lsk_existing', ARRAY['session'], '{"kind":"local"}', 1),
               (repeat('b', 64), 'lsk_another1', ARRAY['session'], '{"kind":"local"}', 2);
        "#,
    ))
    .await
    .expect("restore the previous key table");

    apply_migration(pool, 17).await;

    let keys = PgApiKeyStore::new(pool.clone())
        .list_api_keys(Some(AccessScope::Deployment))
        .await
        .expect("list migrated keys");
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .all(|key| auth::is_allocated_api_key_id(&key.key_id))
    );
    assert_ne!(keys[0].key_id, keys[1].key_id);
    pool.execute("DELETE FROM api_keys")
        .await
        .expect("clear migrated keys");
}

/// Run one embedded migration's statements over the schema as it stands. The
/// ledger is left alone, so migrations added later do not affect this.
async fn apply_migration(pool: &sqlx::PgPool, version: i64) {
    let migration = store_pg::MIGRATIONS
        .iter()
        .find(|migration| migration.version == version)
        .expect("embedded migration");
    pool.execute(sqlx::raw_sql(migration.sql))
        .await
        .expect("apply migration");
}
