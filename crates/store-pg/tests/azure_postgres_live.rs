//! Workload identity against a real Azure Database for PostgreSQL server. Run it
//! from a workload holding a federated identity that exists as a database role.
//! Never reads .env.
use std::env;

use store_pg::{AzureWorkloadIdentity, connect_postgres_with_workload_identity};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires LIGHTSPEED_TEST_AZURE_POSTGRES_URL (user is the identity name, sslmode=verify-full) and AZURE_*"]
async fn workload_identity_opens_a_verified_session_as_the_managed_identity() {
    let url = env::var("LIGHTSPEED_TEST_AZURE_POSTGRES_URL")
        .expect("explicit Azure PostgreSQL test URL required");
    let identity = AzureWorkloadIdentity {
        client_id: env::var("AZURE_CLIENT_ID").expect("AZURE_CLIENT_ID required"),
        tenant_id: env::var("AZURE_TENANT_ID").expect("AZURE_TENANT_ID required"),
        federated_token_file: env::var("AZURE_FEDERATED_TOKEN_FILE")
            .expect("AZURE_FEDERATED_TOKEN_FILE required"),
        authority_host: env::var("AZURE_AUTHORITY_HOST").ok(),
    };

    let pool = connect_postgres_with_workload_identity(&url, identity)
        .await
        .unwrap();

    let expected = url::Url::parse(&url).unwrap();
    let user: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(user, expected.username());
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(encrypted);
    pool.close().await;
}
