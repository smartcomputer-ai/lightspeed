//! Renaming the outbound credential namespace preserves data and relationships.
use futures_util::FutureExt as _;
use serde_json::Value;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{panic::AssertUnwindSafe, str::FromStr};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; creates an isolated schema"]
async fn credential_namespace_preserves_rows_encrypted_bytes_and_foreign_keys() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect to live Postgres");
    let schema = format!(
        "lightspeed_credentials_namespace_test_{}",
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
            PgConnectOptions::from_str(&url)
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

const TABLE_SUFFIXES: [&str; 6] = [
    "clients",
    "flows",
    "grants",
    "provider_headers",
    "providers",
    "secrets",
];

async fn exercise(pool: &sqlx::PgPool) {
    for migration in store_pg::MIGRATIONS
        .iter()
        .filter(|migration| migration.version < 27)
    {
        pool.execute(sqlx::raw_sql(migration.sql)).await.unwrap();
    }
    let universe_id = Uuid::new_v4();
    sqlx::query("INSERT INTO universes (universe_id, handle, created_at_ms) VALUES ($1, 'migration-test', 1)")
        .bind(universe_id).execute(pool).await.unwrap();
    for statement in [
        "INSERT INTO auth_secrets (universe_id, secret_id, secret_kind, key_id, nonce, ciphertext, created_at_ms, updated_at_ms) VALUES ($1, 'secret', 'static_token', 'local-v1', decode('000102030405060708090a0b', 'hex'), decode('deadbeef0123456789', 'hex'), 1, 2)",
        "INSERT INTO auth_grants (universe_id, grant_id, provider_id, provider_kind, created_by, access_token_secret_id, exposure, lease_count, last_leased_at_ms, created_at_ms, updated_at_ms) VALUES ($1, 'authgrant_existing', 'provider', 'static_token', '{\"kind\":\"token\",\"name\":\"worker\"}', 'secret', 'retrievable', 7, 2, 1, 2)",
        "INSERT INTO auth_clients (universe_id, client_id, provider_id, provider_kind, authorization_endpoint, token_endpoint, remote_client_id, client_secret_secret_id, created_at_ms, updated_at_ms) VALUES ($1, 'client', 'provider', 'mcp_oauth', 'https://example.com/authorize', 'https://example.com/token', 'remote', 'secret', 1, 2)",
        "INSERT INTO auth_flows (universe_id, flow_id, client_id, provider_id, provider_kind, created_by, state_hash, pkce_verifier_secret_id, redirect_uri, expires_at_ms, created_at_ms, updated_at_ms) VALUES ($1, 'authflow_existing', 'client', 'provider', 'mcp_oauth', '{\"kind\":\"actor\",\"id\":\"alice\"}', 'state-hash', 'secret', 'https://runtime.example.com/credentials/callback', 99, 1, 2)",
        "INSERT INTO auth_providers (universe_id, provider_id, provider_kind, config_json, credential_secret_id, created_at_ms, updated_at_ms) VALUES ($1, 'provider', 'model_api_key', '{\"opaque\":\"unchanged\"}', 'secret', 1, 2)",
        "INSERT INTO auth_provider_headers (universe_id, provider_id, name, grant_id) VALUES ($1, 'provider', 'api-key', 'authgrant_existing')",
        "INSERT INTO mcp_servers (universe_id, server_id, server_url, default_server_label, auth_policy, auth_grant_id, revision, created_at_ms, updated_at_ms) VALUES ($1, 'server', 'https://example.com/mcp', 'server', 'required_bearer', 'authgrant_existing', 1, 1, 2)",
    ] {
        sqlx::query(statement)
            .bind(universe_id)
            .execute(pool)
            .await
            .unwrap();
    }
    let mut before = Vec::new();
    for suffix in TABLE_SUFFIXES {
        before.push(snapshot(pool, &format!("auth_{suffix}")).await);
    }
    let foreign_keys_before: Vec<(i64, i64)> = foreign_keys(pool).await;
    let migration = store_pg::MIGRATIONS
        .iter()
        .find(|migration| migration.version == 27)
        .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    transaction
        .execute(sqlx::raw_sql(migration.sql))
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    for (suffix, expected) in TABLE_SUFFIXES.into_iter().zip(before) {
        let old_name = format!("auth_{suffix}");
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_tables WHERE schemaname = current_schema() AND tablename = $1)")
            .bind(&old_name).fetch_one(pool).await.unwrap();
        assert!(!exists, "old table still exists: {old_name}");
        assert_eq!(
            snapshot(pool, &format!("credentials_{suffix}")).await,
            expected
        );
    }
    assert_eq!(
        foreign_keys(pool).await,
        foreign_keys_before,
        "foreign keys retain the same source and target relations"
    );
    let stale_names: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid JOIN pg_namespace n ON n.oid = t.relnamespace WHERE n.nspname = current_schema() AND starts_with(t.relname, 'credentials_') AND starts_with(c.conname, 'auth_')")
        .fetch_one(pool).await.unwrap();
    assert_eq!(stale_names, 0);
    let stale_indexes: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_indexes WHERE schemaname = current_schema() AND starts_with(tablename, 'credentials_') AND starts_with(indexname, 'auth_')")
        .fetch_one(pool).await.unwrap();
    assert_eq!(stale_indexes, 0);
    let result = sqlx::query(
        "DELETE FROM credentials_grants WHERE universe_id = $1 AND grant_id = 'authgrant_existing'",
    )
    .bind(universe_id)
    .execute(pool)
    .await;
    assert_eq!(
        result
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .code()
            .as_deref(),
        Some("23503")
    );
}

async fn snapshot(pool: &sqlx::PgPool, table: &str) -> Value {
    // Table names come only from the fixed list above.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT to_jsonb(record) FROM {table} record"
    )))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn foreign_keys(pool: &sqlx::PgPool) -> Vec<(i64, i64)> {
    sqlx::query_as("SELECT c.conrelid::bigint, c.confrelid::bigint FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace WHERE n.nspname = current_schema() AND c.contype = 'f' ORDER BY c.oid")
        .fetch_all(pool).await.unwrap()
}
