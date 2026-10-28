use std::str::FromStr as _;

use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgStore, PgStoreError, REQUIRED_SCHEMA_REVISION};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn embedded_migrations_are_locked_idempotent_and_checksum_guarded() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!("lightspeed_migration_test_{}", Uuid::new_v4().simple());
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            r#"CREATE SCHEMA "{schema}""#
        ))))
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
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            r#"DROP SCHEMA "{schema}" CASCADE"#
        ))))
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

    // The environment baseline is folded into one migration; the ledger
    // ends at revision 11.
    if REQUIRED_SCHEMA_REVISION != 11 {
        return Err(format!(
            "unexpected required schema revision {REQUIRED_SCHEMA_REVISION}"
        ));
    }

    // The public schema can have these same names. Every newly migrated schema
    // must still receive its own foreign keys.
    let foreign_keys: Vec<(String, bool, bool, bool)> = sqlx::query_as(
        "SELECT constraint_row.conname::text, constraint_row.condeferrable,
                constraint_row.condeferred,
                source.relnamespace = target.relnamespace
         FROM pg_constraint AS constraint_row
         JOIN pg_class AS source ON source.oid = constraint_row.conrelid
         JOIN pg_class AS target ON target.oid = constraint_row.confrelid
         WHERE source.relnamespace = current_schema()::regnamespace
           AND constraint_row.conname IN (
               'environments_registration_key_fk',
               'mcp_servers_auth_grant_fk'
           )
         ORDER BY constraint_row.conname",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    if foreign_keys
        != vec![
            // Deferrable but checked immediately, so only a universe delete
            // (which defers its constraints) can remove keys and
            // environments in either cascade order.
            (
                "environments_registration_key_fk".to_owned(),
                true,
                false,
                true,
            ),
            ("mcp_servers_auth_grant_fk".to_owned(), false, false, true),
        ]
    {
        return Err(format!(
            "missing or incorrect local foreign keys: {foreign_keys:?}"
        ));
    }

    // The environment baseline's named constraints and indexes.
    let mut expected_constraints = vec![
        "environment_providers_controller_endpoint_format",
        "environment_providers_credential_all_or_none",
        "environment_providers_data_route_base_format",
        "environment_providers_display_name_not_empty",
        "environment_providers_provider_id_format",
        "environment_providers_times_valid",
        "environment_provider_bindings_times_valid",
        "environment_registration_keys_display_name_bounded",
        "environment_registration_keys_id_format",
        "environment_registration_keys_identity_mode_known",
        "environment_registration_keys_limits_positive",
        "environment_registration_keys_offline_close_persistent",
        "environment_registration_keys_times_valid",
        "environment_registration_key_secrets_hash_format",
        "environment_registration_key_secrets_prefix_not_empty",
        "environment_registration_key_secrets_times_valid",
        "environments_close_reason",
        "environments_closed_at",
        "environments_daemon_build",
        "environments_ids_format",
        "environments_json_objects",
        "environments_observation",
        "environments_registration_key_fk",
        "environments_source_fields",
        "environments_source_known",
        "environments_state_known",
        "environments_state_message_bounded",
        "environments_state_per_kind",
        "environments_times_valid",
        "environments_work_schedule",
    ];
    expected_constraints.sort_unstable();
    let constraints: Vec<String> = sqlx::query_scalar(
        "SELECT constraint_row.conname::text
         FROM pg_constraint AS constraint_row
         JOIN pg_class AS source ON source.oid = constraint_row.conrelid
         WHERE source.relnamespace = current_schema()::regnamespace
           AND source.relname IN (
               'environment_providers', 'environment_provider_bindings',
               'environment_registration_keys', 'environment_registration_key_secrets',
               'environments'
           )
           AND constraint_row.contype IN ('c', 'f')
           AND constraint_row.conname = ANY($1::text[])
         ORDER BY constraint_row.conname",
    )
    .bind(&expected_constraints)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let mut constraints: Vec<&str> = constraints.iter().map(String::as_str).collect();
    constraints.sort_unstable();
    if constraints != expected_constraints {
        return Err(format!(
            "environment baseline constraints differ: {constraints:?}"
        ));
    }
    let mut expected_indexes = vec![
        "environment_registration_key_secrets_live_idx",
        "environments_daemon_public_key_idx",
        "environments_metadata_idx",
        "environments_provider_state_idx",
        "environments_provider_target_idx",
        "environments_provisioned_open_idx",
        "environments_registered_open_idx",
        "environments_registration_key_state_idx",
        "environments_request_id_idx",
        "environments_work_idx",
    ];
    expected_indexes.sort_unstable();
    let mut indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes
         WHERE schemaname = current_schema() AND indexname = ANY($1::text[])",
    )
    .bind(&expected_indexes)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    indexes.sort_unstable();
    if indexes != expected_indexes {
        return Err(format!("environment baseline indexes differ: {indexes:?}"));
    }
    let incarnations: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('environment_incarnations')::text")
            .fetch_one(pool)
            .await
            .map_err(|error| error.to_string())?;
    if incarnations.is_some() {
        return Err("the environment baseline must not create incarnations".to_owned());
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
