use auth::{ApiKeyStore, CreateApiKey, PrincipalRef, mint_api_key};
use sqlx::postgres::PgPoolOptions;
use store_pg::{PgApiKeyStore, PgStore};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres"]
async fn api_key_management_is_scoped_by_universe() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect(
        "LIGHTSPEED_TEST_POSTGRES_URL must be set; run local/up.sh and source local/env.sh",
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    PgStore::migrate(&pool).await.expect("apply migrations");

    let left_universe = Uuid::new_v4();
    let right_universe = Uuid::new_v4();
    store_pg::create_universe(&pool, left_universe)
        .await
        .expect("create left universe");
    store_pg::create_universe(&pool, right_universe)
        .await
        .expect("create right universe");

    let api_keys = PgApiKeyStore::new(pool.clone());
    let left_key = mint_api_key(
        left_universe,
        PrincipalRef::universe_default(),
        Some("left key".to_owned()),
        10,
    );
    let right_key = mint_api_key(
        right_universe,
        PrincipalRef::universe_default(),
        Some("right key".to_owned()),
        11,
    );
    for minted in [&left_key, &right_key] {
        api_keys
            .create_api_key(CreateApiKey {
                key_hash: minted.key_hash.clone(),
                record: minted.record.clone(),
            })
            .await
            .expect("create api key");
    }

    let listed = api_keys
        .list_api_keys_for_universe(left_universe)
        .await
        .expect("list left keys");
    assert_eq!(listed, vec![left_key.record.clone()]);

    assert!(
        api_keys
            .revoke_api_key_for_universe(left_universe, &right_key.record.key_prefix, 20)
            .await
            .expect("foreign-universe revoke")
            .is_none()
    );
    let revoked = api_keys
        .revoke_api_key_for_universe(left_universe, &left_key.record.key_prefix, 20)
        .await
        .expect("revoke left key")
        .expect("left key exists");
    assert_eq!(revoked.revoked_at_ms, Some(20));
    let revoked_again = api_keys
        .revoke_api_key_for_universe(left_universe, &left_key.record.key_prefix, 30)
        .await
        .expect("revoke left key again")
        .expect("left key still exists");
    assert_eq!(revoked_again.revoked_at_ms, Some(20));

    store_pg::delete_universe(&pool, left_universe)
        .await
        .expect("delete left universe");
    store_pg::delete_universe(&pool, right_universe)
        .await
        .expect("delete right universe");
}
