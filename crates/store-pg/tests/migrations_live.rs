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
    run_migration_test(false, false).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn released_schema_upgrade_resets_only_environment_records() {
    run_migration_test(true, false).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn environment_reset_rolls_back_on_unexpected_dependencies_and_can_retry() {
    run_migration_test(true, true).await;
}

async fn run_migration_test(upgrade: bool, block_reset: bool) {
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

    let result = exercise_migrator(&pool, upgrade, block_reset).await;
    pool.close().await;
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            r#"DROP SCHEMA "{schema}" CASCADE"#
        ))))
        .await
        .expect("drop isolated migration schema");
    result.unwrap_or_else(|message| panic!("migration acceptance failed: {message}"));
}

async fn exercise_migrator(
    pool: &sqlx::PgPool,
    upgrade: bool,
    block_reset: bool,
) -> Result<(), String> {
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

    let retained = if upgrade {
        seed_released_schema(pool).await?;
        Some(retained_rows(pool).await?)
    } else {
        None
    };

    if block_reset {
        exercise_unexpected_dependency(pool).await?;
    }

    let (left, right) = tokio::join!(PgStore::migrate(pool), PgStore::migrate(pool));
    left.map_err(|error| error.to_string())?;
    right.map_err(|error| error.to_string())?;

    let status = store_pg::verify_schema(pool)
        .await
        .map_err(|error| error.to_string())?;
    if status.current_revision != REQUIRED_SCHEMA_REVISION || !status.pending.is_empty() {
        return Err(format!("unexpected migrated status: {status:?}"));
    }

    // The forward environment reset follows session deletion.
    if REQUIRED_SCHEMA_REVISION != 12 {
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

    if let Some(retained) = retained {
        if retained_rows(pool).await? != retained {
            return Err("upgrade changed unrelated records".to_owned());
        }
        for table in [
            "environment_providers",
            "environment_provider_bindings",
            "environment_registration_keys",
            "environment_registration_key_secrets",
            "environments",
            "environment_credentials",
        ] {
            let count: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                    .fetch_one(pool)
                    .await
                    .map_err(|error| error.to_string())?;
            if count != 0 {
                return Err(format!(
                    "upgrade retained old environment records in {table}"
                ));
            }
        }
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

// Frozen release checksums keep this upgrade test anchored to the shipped
// schema, rather than silently testing a subsequently edited baseline.
async fn seed_released_schema(pool: &sqlx::PgPool) -> Result<(), String> {
    use sha2::{Digest as _, Sha256};
    let released_checksums = [
        "7ad7483ce1ed32e23db6e1472d655a44e85c967f6c08bb44c52be74023891663",
        "8336521eb3bd742760bebc26be6f6923097a6507181274944c0b31d7ed64bbe9",
        "b90d5fd104e95d2a347b6a940741203c1bba42c71aed0fd073dd5806385d1df6",
        "523a447a8c4d03af459cc7e6ae6fbeed0dde7d3f8b6b29bab08a238ce5a92f67",
        "1438020d90565b50d8a93514d2080d46f29e3430620df6bb80780ac6c79879be",
        "6c2e39070aa95c0a8c0684919450d4943209f1afe15e9dc75635a87224b9a7ae",
        "5fc53c6cdbff5a1243cfdaad26697148551c90f3f3c3fcad857d8c81b4fe1c1b",
        "9c956754e303234fe37c4d135a0b759e007863b713009be5865c9e43bb750dc1",
        "a6c03027b1d98a3e555604168ce7268cde91dc4cdf724d400e42b390e517a0b9",
        "4d7f75004d1fa1f4d64fda2bd63ea60989abca87f0bd4bd590a747fa1dfb1f1d",
    ];
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (
        version bigint PRIMARY KEY, name text NOT NULL, checksum text NOT NULL,
        applied_at timestamptz NOT NULL DEFAULT now(),
        CONSTRAINT schema_migrations_version_positive CHECK (version > 0),
        CONSTRAINT schema_migrations_name_present CHECK (name <> ''),
        CONSTRAINT schema_migrations_checksum_sha256 CHECK (checksum ~ '^[0-9a-f]{64}$'))",
    )
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;
    for (migration, expected) in store_pg::MIGRATIONS.iter().zip(released_checksums) {
        let checksum = hex::encode(Sha256::digest(migration.sql.as_bytes()));
        if checksum != expected {
            return Err(format!("released migration {} changed", migration.version));
        }
        let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql))
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;
        sqlx::query("INSERT INTO schema_migrations (version, name, checksum) VALUES ($1, $2, $3)")
            .bind(migration.version)
            .bind(migration.name)
            .bind(checksum)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;
        tx.commit().await.map_err(|error| error.to_string())?;
    }
    sqlx::raw_sql(include_str!("fixtures/released_environment_upgrade.sql"))
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    let status = store_pg::schema_status(pool)
        .await
        .map_err(|error| error.to_string())?;
    if status.current_revision != 10 || status.pending != [11, 12] {
        return Err(format!("unexpected released schema status: {status:?}"));
    }

    Ok(())
}

async fn exercise_unexpected_dependency(pool: &sqlx::PgPool) -> Result<(), String> {
    // A downstream installation's unexpected dependency must fail closed.
    // Migration 11 commits, but the entire environment reset must roll back.
    sqlx::raw_sql(
        "CREATE TABLE unrelated_dependency (
        provider_id text REFERENCES environment_providers(provider_id))",
    )
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;
    let error = PgStore::migrate(pool)
        .await
        .expect_err("unexpected dependency must block reset");
    if !matches!(&error, PgStoreError::Postgres(source)
        if source.as_database_error().and_then(|error| error.code()).as_deref() == Some("2BP01"))
    {
        return Err(format!("unexpected dependency error: {error}"));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM environment_credentials")
        .fetch_one(pool)
        .await
        .map_err(|error| error.to_string())?;
    if count != 1 {
        return Err("failed reset did not roll back environment records".to_owned());
    }
    let status = store_pg::schema_status(pool)
        .await
        .map_err(|error| error.to_string())?;
    if status.current_revision != 11 || status.pending != [12] {
        return Err(format!(
            "failed reset changed the ledger unexpectedly: {status:?}"
        ));
    }
    sqlx::query("DROP TABLE unrelated_dependency")
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn retained_rows(pool: &sqlx::PgPool) -> Result<Vec<(String, Vec<String>)>, String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables
        WHERE schemaname = current_schema() AND tablename NOT LIKE 'environment%'
        AND tablename <> 'schema_migrations' ORDER BY tablename",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for table in tables {
        let data: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT (to_jsonb(t) - 'deleted_at_ms')::text FROM \"{table}\" t ORDER BY 1"
        )))
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?;
        rows.push((table, data));
    }
    Ok(rows)
}
