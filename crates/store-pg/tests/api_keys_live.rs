use auth::{ApiKeyStore, CreateApiKey, mint_api_key};
use sqlx::postgres::PgPoolOptions;
use store_pg::{PgApiKeyStore, PgStore};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres"]
async fn api_key_management_is_scoped_by_universe() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
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

    let actor = store_pg::PgAccessStore::new(pool.clone())
        .initialize_local_development(left_universe, 1)
        .await
        .unwrap()
        .id;
    let api_keys = PgApiKeyStore::new(pool.clone());
    let left_key = mint_api_key(
        access::AccessScope::Universe {
            universe_id: left_universe,
        },
        actor,
        actor,
        Some("left key".to_owned()),
        10,
    );
    let right_key = mint_api_key(
        access::AccessScope::Universe {
            universe_id: right_universe,
        },
        actor,
        actor,
        Some("right key".to_owned()),
        11,
    );
    for minted in [&left_key, &right_key] {
        api_keys
            .create_api_key(CreateApiKey {
                authority_scope: access::AccessScope::Deployment,
                key_hash: minted.key_hash.clone(),
                record: minted.record.clone(),
            })
            .await
            .expect("create api key");
    }

    let left = access::AccessScope::Universe {
        universe_id: left_universe,
    };
    let right = access::AccessScope::Universe {
        universe_id: right_universe,
    };
    let deployment = access::AccessScope::Deployment;
    let listed = api_keys
        .list_managed_keys(actor, deployment, left)
        .await
        .expect("list left keys");
    assert_eq!(listed, vec![left_key.record.clone()]);

    // Resolution is a read that also yields the active principal; the usage
    // stamp is coarse so a busy key is not rewritten on every request.
    let resolved = api_keys
        .resolve_api_key(&right_key.key_hash, 1_000)
        .await
        .expect("resolve right key")
        .expect("right key is active");
    assert_eq!(resolved.principal.id, actor);
    assert_eq!(resolved.record.key_prefix, right_key.record.key_prefix);
    let last_used = async || {
        api_keys
            .list_managed_keys(actor, deployment, right)
            .await
            .expect("list right keys")[0]
            .last_used_at_ms
    };
    assert_eq!(last_used().await, Some(1_000));
    api_keys
        .resolve_api_key(&right_key.key_hash, 2_000)
        .await
        .expect("resolve within the usage resolution");
    assert_eq!(last_used().await, Some(1_000));
    let later = 1_000 + auth::API_KEY_LAST_USED_RESOLUTION_MS;
    api_keys
        .resolve_api_key(&right_key.key_hash, later)
        .await
        .expect("resolve after the usage resolution");
    assert_eq!(last_used().await, Some(later));
    assert!(
        api_keys
            .key_is_active(&right_key.record.key_prefix, actor, right)
            .await
            .expect("right key is active")
    );
    // A key never authenticates outside its recorded binding.
    assert!(
        !api_keys
            .key_is_active(&right_key.record.key_prefix, actor, left)
            .await
            .expect("right key is foreign to the left universe")
    );

    assert!(
        api_keys
            .revoke_managed_key(actor, deployment, left, &right_key.record.key_prefix, 20)
            .await
            .expect("foreign-universe revoke")
            .is_none()
    );
    let revoked = api_keys
        .revoke_managed_key(actor, deployment, left, &left_key.record.key_prefix, 20)
        .await
        .expect("revoke left key")
        .expect("left key exists");
    assert_eq!(revoked.revoked_at_ms, Some(20));
    let revoked_again = api_keys
        .revoke_managed_key(actor, deployment, left, &left_key.record.key_prefix, 30)
        .await
        .expect("revoke left key again")
        .expect("left key still exists");
    assert_eq!(revoked_again.revoked_at_ms, Some(20));
    assert!(
        api_keys
            .resolve_api_key(&left_key.key_hash, later)
            .await
            .expect("resolve revoked key")
            .is_none()
    );
    assert!(
        !api_keys
            .key_is_active(&left_key.record.key_prefix, actor, left)
            .await
            .expect("revoked key is inactive")
    );

    // Removing a universe removes its assignments and keys, so it advances the
    // policy revision that parked requests revalidate against.
    let access_store = store_pg::PgAccessStore::new(pool.clone());
    let before = access_store.policy_revision().await.expect("revision");
    store_pg::delete_universe(&pool, left_universe)
        .await
        .expect("delete left universe");
    assert!(access_store.policy_revision().await.expect("revision") > before);
    store_pg::delete_universe(&pool, right_universe)
        .await
        .expect("delete right universe");
    let settled = access_store.policy_revision().await.expect("revision");
    assert!(
        !store_pg::delete_universe(&pool, right_universe)
            .await
            .expect("delete missing universe")
    );
    assert_eq!(
        access_store.policy_revision().await.expect("revision"),
        settled
    );
}
