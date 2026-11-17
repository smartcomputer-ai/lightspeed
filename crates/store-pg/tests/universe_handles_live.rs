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

    // Declaration updates are partial and preserve unrelated sections and tenants.
    let configuration = api::UniverseConfiguration {
        display_name: Some("Fresh universe".into()),
        metadata: Some(std::collections::BTreeMap::from([(
            "purpose".into(),
            "research".into(),
        )])),
        protected: Some(false),
    };
    assert!(
        store_pg::put_universe_configuration(pool, fresh_id, &configuration)
            .await
            .unwrap()
    );
    assert_eq!(
        store_pg::read_universe_stats(pool, fresh_id)
            .await
            .unwrap()
            .unwrap()
            .configuration,
        configuration
    );
    let clear = api::UniverseConfiguration {
        metadata: Some(Default::default()),
        ..Default::default()
    };
    for _ in 0..2 {
        assert!(
            store_pg::put_universe_configuration(pool, fresh_id, &clear)
                .await
                .unwrap()
        );
    }
    let actual = store_pg::read_universe_stats(pool, fresh_id)
        .await
        .unwrap()
        .unwrap()
        .configuration;
    assert_eq!(actual.display_name, configuration.display_name);
    assert_eq!(actual.metadata, Some(Default::default()));
    assert_eq!(actual.protected, Some(false));
    assert_eq!(
        store_pg::read_universe_stats(pool, id)
            .await
            .unwrap()
            .unwrap()
            .configuration,
        api::UniverseConfiguration {
            metadata: Some(Default::default()),
            protected: Some(false),
            ..Default::default()
        }
    );

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
