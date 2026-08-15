use std::str::FromStr as _;

use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgStore, PgStoreError, REQUIRED_SCHEMA_REVISION};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires npm run dev -- infra or compatible Postgres env"]
async fn embedded_migrations_are_locked_idempotent_and_checksum_guarded() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run npm run dev -- infra and source dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!("lightspeed_migration_test_{}", Uuid::new_v4().simple());
    admin
        .execute(format!(r#"CREATE SCHEMA "{schema}""#).as_str())
        .await
        .expect("create isolated migration schema");

    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse Postgres URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect to isolated migration schema");

    let result = exercise_migrator(&pool).await;
    pool.close().await;
    admin
        .execute(format!(r#"DROP SCHEMA "{schema}" CASCADE"#).as_str())
        .await
        .expect("drop isolated migration schema");
    result.unwrap_or_else(|message| panic!("migration acceptance failed: {message}"));
}

async fn exercise_migrator(pool: &sqlx::PgPool) -> Result<(), String> {
    let before = store_pg::verify_schema(pool)
        .await
        .expect_err("empty schema must require migration");
    if !matches!(
        before,
        PgStoreError::MigrationRequired {
            current_revision: 0,
            required_revision: REQUIRED_SCHEMA_REVISION,
        }
    ) {
        return Err(format!("unexpected pre-migration error: {before}"));
    }

    sqlx::query("CREATE TABLE universes (id text PRIMARY KEY)")
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    let unledgered_status = store_pg::verify_schema(pool)
        .await
        .expect_err("startup verification must reject an unledgered Lightspeed schema");
    if !matches!(
        unledgered_status,
        PgStoreError::UnledgeredSchema { ref relations }
            if relations == &["universes".to_owned()]
    ) {
        return Err(format!(
            "unexpected unledgered startup error: {unledgered_status}"
        ));
    }
    let unledgered = PgStore::migrate(pool)
        .await
        .expect_err("an unledgered Lightspeed schema must not be baselined");
    if !matches!(
        unledgered,
        PgStoreError::UnledgeredSchema { ref relations }
            if relations == &["universes".to_owned()]
    ) {
        return Err(format!("unexpected unledgered-schema error: {unledgered}"));
    }
    let ledger: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('schema_migrations')::text")
            .fetch_one(pool)
            .await
            .map_err(|error| error.to_string())?;
    if ledger.is_some() {
        return Err("refused migration unexpectedly created a migration ledger".to_owned());
    }
    sqlx::query("DROP TABLE universes")
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;

    let (left, right) = tokio::join!(PgStore::migrate(pool), PgStore::migrate(pool));
    left.map_err(|error| error.to_string())?;
    right.map_err(|error| error.to_string())?;

    let status = store_pg::verify_schema(pool)
        .await
        .map_err(|error| error.to_string())?;
    if status.current_revision != REQUIRED_SCHEMA_REVISION || !status.pending.is_empty() {
        return Err(format!("unexpected migrated status: {status:?}"));
    }

    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .map_err(|error| error.to_string())?;
    if rows.len() != REQUIRED_SCHEMA_REVISION as usize
        || rows
            .iter()
            .enumerate()
            .any(|(index, (version, name, checksum))| {
                *version != index as i64 + 1 || name.is_empty() || checksum.len() != 64
            })
    {
        return Err(format!("unexpected migration ledger: {rows:?}"));
    }

    sqlx::query("UPDATE schema_migrations SET checksum = repeat('0', 64) WHERE version = 1")
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    let tampered = store_pg::schema_status(pool)
        .await
        .expect_err("changed checksum must be rejected");
    if !matches!(
        tampered,
        PgStoreError::MigrationChecksumChanged { version: 1, .. }
    ) {
        return Err(format!("unexpected checksum error: {tampered}"));
    }
    Ok(())
}
