use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{panic::AssertUnwindSafe, str::FromStr};
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn host_provisioning_fills_missing_slugs_without_renaming() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!("lightspeed_universe_slug_test_{}", Uuid::new_v4().simple());
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
    let id = Uuid::new_v4();
    let unnamed = PgStore::new(pool.clone(), PgStoreConfig::new(id));
    unnamed.ensure_universe().await.unwrap();
    assert_eq!(slug(pool, id).await, None);
    let named = PgStore::new(
        pool.clone(),
        PgStoreConfig::new(id).with_universe_slug("development"),
    );
    named.ensure_universe().await.unwrap();
    assert_eq!(slug(pool, id).await.as_deref(), Some("development"));
    named.ensure_universe().await.unwrap();
    unnamed.ensure_universe().await.unwrap();
    let renamed = PgStore::new(
        pool.clone(),
        PgStoreConfig::new(id).with_universe_slug("replacement"),
    );
    renamed.ensure_universe().await.unwrap();
    assert_eq!(slug(pool, id).await.as_deref(), Some("development"));
    let fresh_id = Uuid::new_v4();
    let fresh = PgStore::new(
        pool.clone(),
        PgStoreConfig::new(fresh_id).with_universe_slug("fresh"),
    );
    fresh.ensure_universe().await.unwrap();
    assert_eq!(slug(pool, fresh_id).await.as_deref(), Some("fresh"));
    let duplicate = PgStore::new(
        pool.clone(),
        PgStoreConfig::new(Uuid::new_v4()).with_universe_slug("development"),
    );
    let error = duplicate
        .ensure_universe()
        .await
        .expect_err("slugs remain unique across universes");
    assert!(
        matches!(error, store_pg::PgStoreError::Postgres(sqlx::Error::Database(error)) if error.is_unique_violation())
    );
    assert_eq!(
        store_pg::put_universe_slug(pool, Uuid::new_v4(), "absent", false)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store_pg::put_universe_slug(pool, id, "new-name", true)
            .await
            .unwrap()
            .as_deref(),
        Some("development")
    );
    assert_eq!(
        store_pg::put_universe_slug(pool, id, "new-name", false)
            .await
            .unwrap()
            .as_deref(),
        Some("new-name")
    );
    assert_eq!(
        store_pg::put_universe_slug(pool, id, "new-name", true)
            .await
            .unwrap()
            .as_deref(),
        Some("new-name")
    );
    // The old slug is released; unique collisions leave the current slug intact.
    assert_eq!(
        store_pg::put_universe_slug(pool, fresh_id, "development", false)
            .await
            .unwrap()
            .as_deref(),
        Some("development")
    );
    let error = store_pg::put_universe_slug(pool, id, "development", false)
        .await
        .unwrap_err();
    assert!(
        matches!(error, store_pg::PgStoreError::Postgres(sqlx::Error::Database(error)) if error.is_unique_violation())
    );
    assert_eq!(slug(pool, id).await.as_deref(), Some("new-name"));
    // Two adopters racing to name an unnamed universe both observe the winner.
    let race_id = Uuid::new_v4();
    store_pg::create_universe(pool, race_id).await.unwrap();
    let (left, right) = tokio::join!(
        store_pg::put_universe_slug(pool, race_id, "race-left", true),
        store_pg::put_universe_slug(pool, race_id, "race-right", true),
    );
    assert_eq!(left.unwrap(), right.unwrap());
}

async fn slug(pool: &sqlx::PgPool, id: Uuid) -> Option<String> {
    store_pg::read_universe_stats(pool, id)
        .await
        .unwrap()
        .unwrap()
        .slug
}
