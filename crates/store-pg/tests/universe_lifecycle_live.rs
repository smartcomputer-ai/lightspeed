use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::collections::BTreeMap;
use std::{panic::AssertUnwindSafe, str::FromStr};
use store_pg::{PgStore, PgStoreError};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn metadata_protection_and_expiry_are_atomic_resource_guards() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!(
        "lightspeed_universe_lifecycle_test_{}",
        Uuid::new_v4().simple()
    );
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA \"{schema}\""
        ))))
        .await
        .expect("create isolated schema");
    let pool = PgPoolOptions::new()
        .max_connections(4)
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
    PgStore::migrate(pool).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let id = Uuid::new_v4();
    let metadata = BTreeMap::from([
        ("purpose".into(), "ci".into()),
        ("team".into(), "runtime".into()),
    ]);
    assert!(
        store_pg::create_universe_with_configuration(pool, id, None, &metadata, true, None)
            .await
            .unwrap()
    );
    assert!(
        !store_pg::create_universe_with_configuration(
            pool,
            id,
            None,
            &BTreeMap::new(),
            false,
            None
        )
        .await
        .unwrap()
    );
    let view = store_pg::read_universe_stats(pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.configuration.metadata.as_ref(), Some(&metadata));
    assert_eq!(view.configuration.protected, Some(true));
    assert_eq!(
        store_pg::list_universe_stats_matching(
            pool,
            &BTreeMap::from([("purpose".into(), "ci".into())])
        )
        .await
        .unwrap()
        .len(),
        1
    );
    assert!(
        store_pg::list_universe_stats_matching(
            pool,
            &BTreeMap::from([("purpose".into(), "production".into())])
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        matches!(store_pg::begin_universe_deletion(pool,id,&api::Attribution::Local,now).await,Err(PgStoreError::UniverseProtected {universe_id}) if universe_id == id)
    );
    assert!(matches!(
        store_pg::delete_universe(pool, id).await,
        Err(PgStoreError::UniverseProtected { .. })
    ));
    assert!(matches!(
        store_pg::put_universe_expiry(pool, id, Some(now + 60_000)).await,
        Err(PgStoreError::UniverseProtectionExpiryConflict)
    ));
    assert!(
        store_pg::put_universe_configuration(
            pool,
            id,
            &api::UniverseConfiguration {
                metadata: Some(BTreeMap::new()),
                ..Default::default()
            }
        )
        .await
        .unwrap()
    );
    let view = store_pg::read_universe_stats(pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.configuration.metadata, Some(BTreeMap::new()));
    assert_eq!(view.configuration.protected, Some(true));
    assert!(
        store_pg::put_universe_configuration(
            pool,
            id,
            &api::UniverseConfiguration {
                metadata: Some(metadata.clone()),
                protected: Some(false),
                ..Default::default()
            }
        )
        .await
        .unwrap()
    );
    assert!(
        store_pg::put_universe_expiry(pool, id, Some(now + 60_000))
            .await
            .unwrap()
    );
    assert!(matches!(
        store_pg::put_universe_configuration(
            pool,
            id,
            &api::UniverseConfiguration {
                protected: Some(true),
                ..Default::default()
            }
        )
        .await,
        Err(PgStoreError::UniverseProtectionExpiryConflict)
    ));
    assert!(matches!(
        store_pg::put_universe_expiry(pool, id, Some(now - 1)).await,
        Err(PgStoreError::InvalidUniverseConfiguration { .. })
    ));
    assert!(
        store_pg::expire_universes(pool, now)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store_pg::expire_universes(pool, now + 60_001)
            .await
            .unwrap(),
        vec![id]
    );
    assert!(
        store_pg::expire_universes(pool, now + 60_002)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!store_pg::put_universe_expiry(pool, id, None).await.unwrap());
    let mut record = store_pg::list_deleting_universes(pool)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        record.deletion.requested_by,
        api::Attribution::Internal {
            component: "universe-expiry".into(),
            cause: "expired".into()
        }
    );
    record.deletion.step = api::UniverseDeletionStep::Storage;
    store_pg::update_universe_deletion(pool, id, &record)
        .await
        .unwrap();
    assert!(
        store_pg::finish_universe_deletion(pool, id, now + 60_003)
            .await
            .unwrap()
    );
    let deleted = store_pg::read_deleted_universe(pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.metadata, metadata);
    assert_eq!(deleted.requested_by, record.deletion.requested_by);

    // A protection edit and deletion admission cannot both succeed.
    for _ in 0..8 {
        let id = Uuid::new_v4();
        store_pg::create_universe(pool, id).await.unwrap();
        let config = api::UniverseConfiguration {
            protected: Some(true),
            ..Default::default()
        };
        let (protect, delete) = tokio::join!(
            store_pg::put_universe_configuration(pool, id, &config),
            store_pg::begin_universe_deletion(pool, id, &api::Attribution::Local, now)
        );
        if protect.unwrap() {
            assert!(matches!(
                delete,
                Err(PgStoreError::UniverseProtected { .. })
            ));
        } else {
            assert_eq!(delete.unwrap(), Some(api::UniverseDeletionState::Deleting));
        }
    }
    // Expiry rechecks the locked row: an extension cannot be overwritten by a stale scan.
    let id = Uuid::new_v4();
    store_pg::create_universe_with_configuration(
        pool,
        id,
        None,
        &BTreeMap::new(),
        false,
        Some(now + 120_000),
    )
    .await
    .unwrap();
    let (extend, expired) = tokio::join!(
        store_pg::put_universe_expiry(pool, id, Some(now + 600_000)),
        store_pg::expire_universes(pool, now + 120_001)
    );
    if extend.unwrap() {
        assert!(!expired.unwrap().contains(&id));
    } else {
        assert!(expired.unwrap().contains(&id));
    }
}
