//! Environment credential bindings lose the source that named a stored
//! secret directly, and a grant that is already bound into an environment is
//! recorded as retrievable. Runs against an isolated schema and never loads
//! .env.
use std::{panic::AssertUnwindSafe, str::FromStr};

use auth::{
    AuthGrantExposure, AuthGrantId, AuthGrantStatus, AuthGrantStore as _, AuthProviderKind,
    CreateAuthGrantRecord, PutSecretRecord, SECRET_KIND_STATIC_TOKEN, SecretId, SecretStore as _,
    SecretValue,
};
use environments::{
    CreateExternalEnvironment, EnvironmentCredentialSource, EnvironmentCredentialStore as _,
    EnvironmentEndpoint, EnvironmentId, EnvironmentProvisionRequestId, EnvironmentStore as _,
    ListEnvironmentCredentials, PutEnvironmentCredential,
};
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgStore, PgStoreConfig, SecretsMasterKey};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; creates an isolated schema"]
async fn bound_grants_become_retrievable_and_stored_secret_bindings_are_dropped() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect to live Postgres");
    let schema = format!(
        "lightspeed_credential_sources_test_{}",
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

const ENVIRONMENT: &str = "build-box";

/// The binding table as the previous release had it: a third source kind
/// that names a stored secret, held by its own column and foreign key.
const PREVIOUS_SHAPE: &str = r#"
ALTER TABLE environment_credentials
    DROP CONSTRAINT environment_credentials_source_kind_known,
    DROP CONSTRAINT environment_credentials_source_exactly_one;
ALTER TABLE environment_credentials ADD COLUMN secret_id text;
ALTER TABLE environment_credentials
    ADD FOREIGN KEY (universe_id, secret_id)
        REFERENCES auth_secrets (universe_id, secret_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    ADD CONSTRAINT environment_credentials_source_kind_known
        CHECK (source_kind IN ('auth_grant', 'auth_provider_credential', 'direct_secret')),
    ADD CONSTRAINT environment_credentials_source_exactly_one CHECK (
        (source_kind = 'auth_grant' AND grant_id IS NOT NULL AND auth_provider_id IS NULL AND secret_id IS NULL)
        OR (source_kind = 'auth_provider_credential' AND grant_id IS NULL AND auth_provider_id IS NOT NULL AND secret_id IS NULL)
        OR (source_kind = 'direct_secret' AND grant_id IS NULL AND auth_provider_id IS NULL AND secret_id IS NOT NULL)
    );
"#;

async fn universe(pool: &sqlx::PgPool) -> PgStore {
    let store = PgStore::new(
        pool.clone(),
        PgStoreConfig::new(Uuid::new_v4())
            .with_secrets_master_key(SecretsMasterKey::from_bytes([7; 32])),
    );
    store.ensure_universe().await.expect("create universe");
    store
        .create_external_environment(CreateExternalEnvironment {
            request_id: EnvironmentProvisionRequestId::new(format!(
                "request-{}",
                Uuid::new_v4().simple()
            )),
            environment_id: EnvironmentId::new(ENVIRONMENT),
            endpoint: EnvironmentEndpoint::parse("ws://127.0.0.1:9200/envd").expect("endpoint"),
            display_name: None,
            metadata: Default::default(),
            created_at_ms: 1,
        })
        .await
        .expect("create environment");
    store
}

async fn grant(store: &PgStore, grant_id: &str, exposure: AuthGrantExposure) {
    store
        .create_grant(CreateAuthGrantRecord {
            grant_id: AuthGrantId::new(grant_id),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticToken,
            exposure,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: Some(SecretId::new(format!("{grant_id}_access"))),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
            metadata: serde_json::json!({}),
            created_at_ms: 2,
        })
        .await
        .expect("create grant");
}

async fn bind(store: &PgStore, env_name: &str, grant_id: &str) {
    store
        .bind_credential(PutEnvironmentCredential {
            environment_id: EnvironmentId::new(ENVIRONMENT),
            env_name: env_name.to_owned(),
            source: EnvironmentCredentialSource::AuthGrant {
                grant_id: AuthGrantId::new(grant_id),
            },
            created_at_ms: 3,
        })
        .await
        .expect("bind grant");
}

async fn exposure(store: &PgStore, grant_id: &str) -> AuthGrantExposure {
    store
        .read_grant(&AuthGrantId::new(grant_id))
        .await
        .expect("read grant")
        .exposure
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.expect("apply migrations");
    let here = universe(pool).await;
    let elsewhere = universe(pool).await;

    // Grants as the previous release let them be: brokered ones bound into
    // an environment, which then received their plain value anyway.
    grant(&here, "bound", AuthGrantExposure::Brokered).await;
    grant(&here, "unbound", AuthGrantExposure::Brokered).await;
    grant(&here, "already", AuthGrantExposure::Retrievable).await;
    // The same id in two universes, bound in only one of them.
    grant(&here, "shared-id", AuthGrantExposure::Brokered).await;
    grant(&elsewhere, "shared-id", AuthGrantExposure::Brokered).await;
    bind(&here, "TOKEN_BOUND", "bound").await;
    bind(&here, "TOKEN_ALREADY", "already").await;
    bind(&elsewhere, "TOKEN", "shared-id").await;

    // A binding that named a stored secret directly, in the table as the
    // previous release had it.
    let secret_id = SecretId::new("authsec_direct");
    here.put_secret(PutSecretRecord {
        secret_id: secret_id.clone(),
        secret_kind: SECRET_KIND_STATIC_TOKEN.to_owned(),
        value: SecretValue::new("plain-value"),
        created_at_ms: 2,
    })
    .await
    .expect("store secret");
    pool.execute(sqlx::raw_sql(PREVIOUS_SHAPE))
        .await
        .expect("put the binding table back into its previous shape");
    sqlx::query(
        "INSERT INTO environment_credentials
             (universe_id, environment_id, env_name, source_kind, secret_id, created_at_ms, updated_at_ms)
         VALUES ($1, $2, 'DIRECT', 'direct_secret', $3, 3, 3)",
    )
    .bind(here.config().universe_id)
    .bind(ENVIRONMENT)
    .bind(secret_id.as_str())
    .execute(pool)
    .await
    .expect("insert a stored-secret binding as the previous release did");
    assert!(
        here.delete_secret(&secret_id).await.is_err(),
        "the previous shape held the secret through the binding"
    );

    let migration = store_pg::MIGRATIONS
        .iter()
        .find(|migration| migration.name == "environment_credential_sources")
        .expect("embedded migration");
    pool.execute(sqlx::raw_sql(migration.sql))
        .await
        .expect("apply migration");

    // The stored-secret binding is gone; grant bindings are untouched and
    // decode through the store.
    let bound: Vec<(String, EnvironmentCredentialSource)> = here
        .list_credentials(ListEnvironmentCredentials {
            environment_id: EnvironmentId::new(ENVIRONMENT),
        })
        .await
        .expect("list bindings")
        .into_iter()
        .map(|record| (record.env_name, record.source))
        .collect();
    assert_eq!(
        bound,
        vec![
            (
                "TOKEN_ALREADY".to_owned(),
                EnvironmentCredentialSource::AuthGrant {
                    grant_id: AuthGrantId::new("already"),
                }
            ),
            (
                "TOKEN_BOUND".to_owned(),
                EnvironmentCredentialSource::AuthGrant {
                    grant_id: AuthGrantId::new("bound"),
                }
            ),
        ]
    );

    // The column went with its foreign key, so nothing holds the secret.
    let secret_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'environment_credentials'
           AND column_name = 'secret_id'",
    )
    .fetch_one(pool)
    .await
    .expect("inspect binding columns");
    assert_eq!(secret_columns, 0);
    here.delete_secret(&secret_id)
        .await
        .expect("the secret is no longer referenced");

    // A stored-secret binding cannot be written any more.
    let refused = sqlx::query(
        "INSERT INTO environment_credentials
             (universe_id, environment_id, env_name, source_kind, grant_id, created_at_ms, updated_at_ms)
         VALUES ($1, $2, 'DIRECT', 'direct_secret', 'bound', 3, 3)",
    )
    .bind(here.config().universe_id)
    .bind(ENVIRONMENT)
    .execute(pool)
    .await
    .expect_err("the source kind is refused");
    // Such a row breaks both checks on the source, and which of the two
    // Postgres names is its own evaluation order, so only the refusal by one
    // of them is asserted.
    let refusal = refused.as_database_error().expect("a database refusal");
    assert!(refusal.is_check_violation());
    assert!(matches!(
        refusal.constraint(),
        Some(
            "environment_credentials_source_kind_known"
                | "environment_credentials_source_exactly_one"
        )
    ));

    // A bound grant is recorded as what it already was in effect; a grant
    // that is not bound keeps the exposure it was created with, also when a
    // grant of the same id is bound in another universe.
    assert_eq!(
        exposure(&here, "bound").await,
        AuthGrantExposure::Retrievable
    );
    assert_eq!(
        exposure(&here, "already").await,
        AuthGrantExposure::Retrievable
    );
    assert_eq!(
        exposure(&here, "unbound").await,
        AuthGrantExposure::Brokered
    );
    assert_eq!(
        exposure(&here, "shared-id").await,
        AuthGrantExposure::Brokered
    );
    assert_eq!(
        exposure(&elsewhere, "shared-id").await,
        AuthGrantExposure::Retrievable
    );
}
