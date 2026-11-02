//! Explicit TLS-only PostgreSQL verification. Never reads .env or changes schemas.
use std::{env, str::FromStr, time::Duration};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires LIGHTSPEED_TEST_POSTGRES_TLS_URL, LIGHTSPEED_TEST_POSTGRES_WRONG_CA_FILE, and LIGHTSPEED_TEST_POSTGRES_WRONG_HOST"]
async fn verify_full_requires_a_trusted_ca_and_matching_hostname() {
    let url = env::var("LIGHTSPEED_TEST_POSTGRES_TLS_URL")
        .expect("explicit TLS-required test PostgreSQL URL required");
    let options = PgConnectOptions::from_str(&url).unwrap();
    assert!(matches!(options.get_ssl_mode(), PgSslMode::VerifyFull));
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .expect("connect over verified TLS");
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(encrypted);
    pool.close().await;

    let wrong_ca = env::var("LIGHTSPEED_TEST_POSTGRES_WRONG_CA_FILE")
        .expect("path to a valid but unrelated CA certificate required");
    assert!(
        std::fs::read_to_string(&wrong_ca)
            .unwrap()
            .contains("BEGIN CERTIFICATE")
    );
    let error = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options.clone().ssl_root_cert(wrong_ca))
        .await
        .unwrap_err();
    assert_certificate_failure(&error);

    let wrong_host = env::var("LIGHTSPEED_TEST_POSTGRES_WRONG_HOST")
        .expect("test server address not covered by its certificate required");
    let error = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options.host(&wrong_host))
        .await
        .unwrap_err();
    assert_certificate_failure(&error);
}

fn assert_certificate_failure(error: &sqlx::Error) {
    // Handshake validation errors may be wrapped by SQLx as invalid-data I/O.
    let certificate_failure = match error {
        sqlx::Error::Tls(_) => true,
        sqlx::Error::Io(error) => error.kind() == std::io::ErrorKind::InvalidData,
        _ => false,
    };
    assert!(
        certificate_failure,
        "expected certificate validation failure: {error:?}"
    );
}
