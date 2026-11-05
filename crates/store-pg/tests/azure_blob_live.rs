//! Explicit Azure/Azurite and PostgreSQL integration. Never reads .env.
//!
//! With `LIGHTSPEED_TEST_AZURE_AUTH_MODE=workload_identity`, run this from a
//! workload holding a federated identity, against an account with Shared Key
//! access disabled, so that only the projected token can authorize the requests.
use std::{env, panic::AssertUnwindSafe, str::FromStr, sync::Arc};

use futures_util::{FutureExt as _, TryStreamExt as _};
use harness::{
    BlobRef,
    storage::{BlobStore as _, BlobStoreError},
};
use object_store::{ObjectStore, ObjectStoreExt as _};
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{
    AzureObjectStoreConfig, AzureObjectStoreCredential, AzureWorkloadIdentity, PgStore,
    PgStoreConfig, build_azure_object_store, delete_objects_under_prefix,
    universe_cas_object_prefix,
};
use uuid::Uuid;

fn azure_config() -> AzureObjectStoreConfig {
    let access_key = env::var("LIGHTSPEED_TEST_AZURE_ACCESS_KEY").ok();
    let sas_token = env::var("LIGHTSPEED_TEST_AZURE_SAS_TOKEN").ok();
    let credential = match env::var("LIGHTSPEED_TEST_AZURE_AUTH_MODE").ok().as_deref() {
        Some("workload_identity") => {
            assert!(
                access_key.is_none() && sas_token.is_none(),
                "workload identity runs must not configure an access key or SAS token"
            );
            AzureObjectStoreCredential::WorkloadIdentity(AzureWorkloadIdentity {
                client_id: env::var("AZURE_CLIENT_ID").expect("AZURE_CLIENT_ID required"),
                tenant_id: env::var("AZURE_TENANT_ID").expect("AZURE_TENANT_ID required"),
                federated_token_file: env::var("AZURE_FEDERATED_TOKEN_FILE")
                    .expect("AZURE_FEDERATED_TOKEN_FILE required"),
                authority_host: env::var("AZURE_AUTHORITY_HOST").ok(),
            })
        }
        None => match (access_key, sas_token) {
            (Some(key), None) => AzureObjectStoreCredential::AccessKey(key),
            (None, Some(token)) => AzureObjectStoreCredential::SasToken(token),
            _ => panic!(
                "set exactly one of LIGHTSPEED_TEST_AZURE_ACCESS_KEY or LIGHTSPEED_TEST_AZURE_SAS_TOKEN"
            ),
        },
        Some(mode) => panic!("unsupported LIGHTSPEED_TEST_AZURE_AUTH_MODE: {mode}"),
    };
    AzureObjectStoreConfig {
        account: env::var("LIGHTSPEED_TEST_AZURE_ACCOUNT")
            .expect("explicit test Azure account required"),
        container: env::var("LIGHTSPEED_TEST_AZURE_CONTAINER")
            .expect("explicit test Azure container required"),
        endpoint: env::var("LIGHTSPEED_TEST_AZURE_ENDPOINT").ok(),
        credential,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicit LIGHTSPEED_TEST_POSTGRES_URL and LIGHTSPEED_TEST_AZURE_* (plus AZURE_* for workload identity); uses a unique schema and object prefix"]
async fn azure_blobs_survive_reconnection_and_remain_isolated_between_universes() {
    let url =
        env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test PostgreSQL URL required");
    let config = azure_config();
    let objects = build_azure_object_store(config.clone()).unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let id = Uuid::new_v4().simple().to_string();
    let schema = format!("azure_blobs_{id}");
    let prefix = format!("lightspeed-tests/{id}");
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA \"{schema}\""
        ))))
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .options([("search_path", schema.as_str())]);

    let outcome = AssertUnwindSafe(exercise(options, config, objects.clone(), &prefix))
        .catch_unwind()
        .await;
    delete_objects_under_prefix(objects.as_ref(), &prefix)
        .await
        .expect("clean up test blobs");
    admin
        .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA \"{schema}\" CASCADE"
        ))))
        .await
        .unwrap();
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn exercise(
    options: PgConnectOptions,
    azure: AzureObjectStoreConfig,
    objects: Arc<dyn ObjectStore>,
    prefix: &str,
) {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    PgStore::migrate(&pool).await.unwrap();
    let first_config = PgStoreConfig::new(Uuid::new_v4()).with_object_prefix(prefix);
    let second_config = PgStoreConfig::new(Uuid::new_v4()).with_object_prefix(prefix);
    let first = PgStore::with_object_store(pool.clone(), objects.clone(), first_config.clone());
    let second = PgStore::with_object_store(pool.clone(), objects.clone(), second_config.clone());
    first.ensure_universe().await.unwrap();
    second.ensure_universe().await.unwrap();
    let payload = vec![b'x'; 128 * 1024];
    let reference = first.put_bytes(payload.clone()).await.unwrap();
    assert_eq!(first.read_bytes(&reference).await.unwrap(), payload);
    assert!(matches!(
        second.read_bytes(&reference).await,
        Err(BlobStoreError::NotFound { .. })
    ));
    assert_eq!(second.put_bytes(payload.clone()).await.unwrap(), reference);
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT storage_kind FROM cas_blobs WHERE digest = $1")
            .bind(reference.as_str().strip_prefix("sha256:").unwrap())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(kinds, vec!["object", "object"]);
    drop(first);
    drop(second);
    pool.close().await;

    // Recreate both database and Azure clients; no previous process cache may satisfy the read.
    let objects = build_azure_object_store(azure).unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let first = PgStore::with_object_store(pool.clone(), objects.clone(), first_config.clone());
    let second = PgStore::with_object_store(pool.clone(), objects.clone(), second_config.clone());
    assert_eq!(first.read_bytes(&reference).await.unwrap(), payload);
    assert_eq!(second.read_bytes(&reference).await.unwrap(), payload);
    assert!(matches!(
        first.read_bytes(&BlobRef::from_bytes(b"missing")).await,
        Err(BlobStoreError::NotFound { .. })
    ));

    let first_prefix = universe_cas_object_prefix(prefix, first_config.universe_id);
    let second_prefix = universe_cas_object_prefix(prefix, second_config.universe_id);
    let listed = objects
        .list(Some(&object_store::path::Path::from(first_prefix.as_str())))
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        delete_objects_under_prefix(objects.as_ref(), &first_prefix)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        objects.head(&listed[0].location).await,
        Err(object_store::Error::NotFound { .. })
    ));
    assert_eq!(second.read_bytes(&reference).await.unwrap(), payload);
    assert_eq!(
        delete_objects_under_prefix(objects.as_ref(), &second_prefix)
            .await
            .unwrap(),
        1
    );
    pool.close().await;
}
