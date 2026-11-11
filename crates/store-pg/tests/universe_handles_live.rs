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
async fn universes_are_named_at_creation_and_renamed_only_explicitly() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!(
        "lightspeed_universe_handle_test_{}",
        Uuid::new_v4().simple()
    );
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
    upgrade_keeps_fitting_names_and_derives_the_rest(pool).await;

    // A universe created without a handle takes the one derived from its id.
    let id = Uuid::new_v4();
    let unnamed = PgStore::new(pool.clone(), PgStoreConfig::new(id));
    unnamed.ensure_universe().await.unwrap();
    assert_eq!(
        handle(pool, id).await,
        store_pg::default_universe_handle(id)
    );
    assert_eq!(resolve(pool, &id.to_string()).await, Some(id));
    assert_eq!(
        resolve(pool, &store_pg::default_universe_handle(id)).await,
        Some(id)
    );

    // Provisioning an existing universe never renames it.
    let named = PgStore::new(
        pool.clone(),
        PgStoreConfig::new(id).with_universe_handle("development"),
    );
    named.ensure_universe().await.unwrap();
    assert_eq!(
        handle(pool, id).await,
        store_pg::default_universe_handle(id)
    );
    assert!(
        !store_pg::create_universe_with_handle(pool, id, Some("development"))
            .await
            .unwrap()
    );

    let fresh_id = Uuid::new_v4();
    assert!(
        store_pg::create_universe_with_handle(pool, fresh_id, Some("fresh"))
            .await
            .unwrap()
    );
    assert_eq!(handle(pool, fresh_id).await, "fresh");
    assert_eq!(resolve(pool, "fresh").await, Some(fresh_id));

    // Handles are unique across universes, at creation and on rename.
    let error = store_pg::create_universe_with_handle(pool, Uuid::new_v4(), Some("fresh"))
        .await
        .expect_err("a handle names one universe");
    assert!(is_unique_violation(&error));
    assert!(
        !store_pg::put_universe_handle(pool, Uuid::new_v4(), "absent")
            .await
            .unwrap()
    );
    assert!(
        store_pg::put_universe_handle(pool, id, "development")
            .await
            .unwrap()
    );
    assert_eq!(handle(pool, id).await, "development");
    let error = store_pg::put_universe_handle(pool, fresh_id, "development")
        .await
        .unwrap_err();
    assert!(is_unique_violation(&error));
    assert_eq!(handle(pool, fresh_id).await, "fresh");

    // A rename releases the previous handle for another universe to claim.
    assert_eq!(
        resolve(pool, &store_pg::default_universe_handle(id)).await,
        None
    );
    assert!(
        store_pg::put_universe_handle(pool, id, "renamed")
            .await
            .unwrap()
    );
    assert!(
        store_pg::put_universe_handle(pool, fresh_id, "development")
            .await
            .unwrap()
    );
    assert_eq!(resolve(pool, "development").await, Some(fresh_id));
    assert_eq!(resolve(pool, "renamed").await, Some(id));

    // A reference names an existing universe or nothing.
    assert_eq!(resolve(pool, &Uuid::new_v4().to_string()).await, None);
    assert_eq!(resolve(pool, "nobody").await, None);
    assert_eq!(resolve(pool, "Not A Handle").await, None);

    // The store refuses what the validator refuses, whoever writes the row.
    for refused in [
        "Research",
        "my.universe",
        "-edge",
        &Uuid::new_v4().to_string(),
    ] {
        let error = store_pg::create_universe_with_handle(pool, Uuid::new_v4(), Some(refused))
            .await
            .expect_err("the format constraint holds");
        assert!(
            matches!(&error, store_pg::PgStoreError::Postgres(sqlx::Error::Database(error)) if error.is_check_violation()),
            "{refused}: {error}"
        );
    }
}

/// Rebuild the universe table as the previous revision left it, then apply
/// the handle migration over rows of every kind it has to carry.
async fn upgrade_keeps_fitting_names_and_derives_the_rest(pool: &sqlx::PgPool) {
    pool.execute(sqlx::raw_sql(
        r#"
        ALTER TABLE universes DROP CONSTRAINT universes_handle_format;
        ALTER TABLE universes ALTER COLUMN handle DROP NOT NULL;
        ALTER TABLE universes RENAME CONSTRAINT universes_handle_key TO universes_slug_key;
        ALTER TABLE universes RENAME COLUMN handle TO slug;
        ALTER TABLE universes ADD CONSTRAINT universes_slug_format
            CHECK (slug IS NULL OR slug ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$');
        "#,
    ))
    .await
    .expect("restore the previous universe table");
    let kept = Uuid::new_v4();
    let unnamed = Uuid::new_v4();
    let mixed_case = Uuid::new_v4();
    let uuid_shaped = Uuid::new_v4();
    for (id, slug) in [
        (kept, Some("kept-name".to_owned())),
        (unnamed, None),
        (mixed_case, Some("My.Universe:one".to_owned())),
        (uuid_shaped, Some(Uuid::new_v4().simple().to_string())),
    ] {
        sqlx::query("INSERT INTO universes (universe_id, slug) VALUES ($1, $2)")
            .bind(id)
            .bind(slug)
            .execute(pool)
            .await
            .unwrap();
    }

    apply_migration(pool, 16).await;

    assert_eq!(handle(pool, kept).await, "kept-name");
    for id in [unnamed, mixed_case, uuid_shaped] {
        assert_eq!(
            handle(pool, id).await,
            store_pg::default_universe_handle(id)
        );
    }
}

fn is_unique_violation(error: &store_pg::PgStoreError) -> bool {
    matches!(error, store_pg::PgStoreError::Postgres(sqlx::Error::Database(error)) if error.is_unique_violation())
}

async fn resolve(pool: &sqlx::PgPool, reference: &str) -> Option<Uuid> {
    store_pg::resolve_universe_reference(pool, reference)
        .await
        .unwrap()
}

async fn handle(pool: &sqlx::PgPool, id: Uuid) -> String {
    store_pg::read_universe_stats(pool, id)
        .await
        .unwrap()
        .unwrap()
        .handle
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
