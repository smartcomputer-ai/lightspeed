use futures_util::FutureExt as _;
use harness::storage::{BlobStore as _, BlobStoreError};
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{panic::AssertUnwindSafe, str::FromStr};
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; isolated schema"]
async fn deletion_fences_inventory_and_permanently_retires_the_uuid() {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    let schema = format!(
        "lightspeed_universe_delete_test_{}",
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
            PgConnectOptions::from_str(&database_url)
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

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    let absent_id = Uuid::new_v4();
    let absent = PgStore::new(pool.clone(), PgStoreConfig::new(absent_id));
    assert!(matches!(
        absent.put_bytes(vec![1, 2, 3]).await,
        Err(BlobStoreError::Store { .. })
    ));
    assert!(!store_pg::universe_exists(pool, absent_id).await.unwrap());
    let id = Uuid::new_v4();
    let replacement = Uuid::new_v4();
    store_pg::create_universe_with_handle(pool, id, Some("reusable"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions (universe_id, session_id, retention_root_session_id, created_at_ms, updated_at_ms) VALUES ($1, 'a', 'a', 1, 1), ($1, 'b', 'b', 1, 1)")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    let actor = api::Attribution::Actor {
        id: "operator".into(),
    };
    assert_eq!(
        store_pg::begin_universe_deletion(pool, id, &actor, 100)
            .await
            .unwrap(),
        Some(api::UniverseDeletionState::Deleting)
    );
    assert!(!store_pg::universe_exists(pool, id).await.unwrap());
    let scoped = PgStore::new(pool.clone(), PgStoreConfig::new(id));
    assert!(matches!(
        scoped.ensure_universe().await,
        Err(store_pg::PgStoreError::UniverseDeleted { .. })
    ));
    assert_eq!(
        store_pg::resolve_universe_reference(pool, "reusable")
            .await
            .unwrap(),
        None
    );
    assert!(store_pg::list_universes(pool).await.unwrap().is_empty());
    assert_eq!(store_pg::list_universe_stats(pool).await.unwrap().len(), 1);
    assert!(
        matches!(store_pg::create_universe(pool, id).await, Err(store_pg::PgStoreError::UniverseDeleted { universe_id }) if universe_id == id)
    );
    assert!(
        !store_pg::put_universe_handle(pool, id, "changed")
            .await
            .unwrap()
    );
    store_pg::create_universe_with_handle(pool, replacement, Some("reusable"))
        .await
        .unwrap();
    // A repeated request keeps the original attribution, timestamp and counts.
    assert_eq!(
        store_pg::begin_universe_deletion(pool, id, &api::Attribution::Local, 200)
            .await
            .unwrap(),
        Some(api::UniverseDeletionState::Deleting)
    );
    let mut record = store_pg::list_deleting_universes(pool)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(record.deletion.started_at_ms, 100);
    assert_eq!(record.deletion.requested_by, actor);
    assert_eq!(record.summary["contained"]["sessions"], 2);
    assert!(
        !store_pg::finish_universe_deletion(pool, id, 300)
            .await
            .unwrap()
    );
    for step in [
        api::UniverseDeletionStep::Workflows,
        api::UniverseDeletionStep::Environments,
        api::UniverseDeletionStep::Storage,
    ] {
        record.deletion.step = step;
        record.progress = serde_json::json!({"attempts": 2});
        store_pg::update_universe_deletion(pool, id, &record)
            .await
            .unwrap();
        let resumed = store_pg::list_deleting_universes(pool)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(resumed.deletion.step, step);
        assert_eq!(resumed.progress["attempts"], 2);
    }
    assert!(
        store_pg::finish_universe_deletion(pool, id, 300)
            .await
            .unwrap()
    );
    assert!(
        !store_pg::finish_universe_deletion(pool, id, 400)
            .await
            .unwrap()
    );
    assert_eq!(
        store_pg::begin_universe_deletion(pool, id, &actor, 400)
            .await
            .unwrap(),
        Some(api::UniverseDeletionState::Deleted)
    );
    assert_eq!(
        store_pg::begin_universe_deletion(pool, Uuid::new_v4(), &actor, 400)
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        store_pg::create_universe(pool, id).await,
        Err(store_pg::PgStoreError::UniverseDeleted { .. })
    ));
    let deleted = store_pg::read_deleted_universe(pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.requested_by, actor);
    assert_eq!(deleted.deleting_at_ms, 100);
    assert_eq!(deleted.deleted_at_ms, 300);
    assert_eq!(deleted.handle, "reusable");
    assert_eq!(deleted.summary["contained"]["sessions"], 2);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE universe_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    assert_eq!(
        store_pg::list_deleted_universes(pool, None)
            .await
            .unwrap()
            .universes,
        vec![deleted]
    );
    assert_eq!(
        store_pg::resolve_universe_reference(pool, "reusable")
            .await
            .unwrap(),
        Some(replacement)
    );
    // Paging stays stable across equal timestamps and never repeats a record.
    for _ in 0..101 {
        let next = Uuid::new_v4();
        store_pg::create_universe(pool, next).await.unwrap();
        store_pg::begin_universe_deletion(pool, next, &actor, 400)
            .await
            .unwrap();
        let mut next_record = store_pg::list_deleting_universes(pool)
            .await
            .unwrap()
            .remove(0);
        next_record.deletion.step = api::UniverseDeletionStep::Storage;
        store_pg::update_universe_deletion(pool, next, &next_record)
            .await
            .unwrap();
        store_pg::finish_universe_deletion(pool, next, 500)
            .await
            .unwrap();
    }
    let page = store_pg::list_deleted_universes(pool, None).await.unwrap();
    assert_eq!(page.universes.len(), 100);
    let second = store_pg::list_deleted_universes(pool, page.next_after.as_deref())
        .await
        .unwrap();
    assert_eq!(second.universes.len(), 2);
    assert!(second.next_after.is_none());
    assert!(!page.universes.iter().any(|a| {
        second
            .universes
            .iter()
            .any(|b| a.universe_id == b.universe_id)
    }));
    exercise_upload_fence(pool).await;
    exercise_cached_write_fence(pool).await;
}

/// An upload admitted before the fence may finish, but storage teardown must
/// observe its catalog publication before removing the object namespace.
async fn exercise_upload_fence(pool: &sqlx::PgPool) {
    use futures_util::TryStreamExt as _;
    use object_store::ObjectStore as _;
    use std::{sync::Arc, time::Duration};
    use tokio::sync::oneshot;

    struct PausedSource {
        started: Option<oneshot::Sender<()>>,
        resume: Option<oneshot::Receiver<()>>,
        bytes: Vec<u8>,
    }
    #[async_trait::async_trait]
    impl harness::storage::BlobSource for PausedSource {
        async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                self.resume.take().unwrap().await.unwrap();
            }
            assert!(self.bytes.len() <= max_bytes);
            Ok(std::mem::take(&mut self.bytes))
        }
    }

    let id = Uuid::new_v4();
    store_pg::create_universe(pool, id).await.unwrap();
    let objects = Arc::new(object_store::memory::InMemory::new());
    let store = PgStore::with_object_store(
        pool.clone(),
        objects.clone(),
        PgStoreConfig::new(id).with_inline_threshold_bytes(0),
    );
    let bytes = vec![0x73; 32];
    let expected = harness::BlobRef::from_bytes(&bytes);
    let (started, begun) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let writer = store.clone();
    let upload = tokio::spawn(async move {
        writer
            .put_stream(
                &expected,
                32,
                &mut PausedSource {
                    started: Some(started),
                    resume: Some(resumed),
                    bytes,
                },
            )
            .await
    });
    begun.await.unwrap();
    // Fence remains immediate even while a source has stopped supplying bytes.
    tokio::time::timeout(
        Duration::from_secs(2),
        store_pg::begin_universe_deletion(pool, id, &api::Attribution::Local, 600),
    )
    .await
    .expect("fence must not wait for an upload")
    .unwrap();
    let mut record = store_pg::list_deleting_universes(pool)
        .await
        .unwrap()
        .remove(0);
    record.deletion.step = api::UniverseDeletionStep::Storage;
    store_pg::update_universe_deletion(pool, id, &record)
        .await
        .unwrap();
    let cleanup = store_pg::lock_universe_objects_for_deletion(pool, id);
    tokio::pin!(cleanup);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut cleanup)
            .await
            .is_err(),
        "storage must wait for the admitted upload"
    );
    resume.send(()).unwrap();
    upload.await.unwrap().unwrap();
    let mut cleanup = cleanup.await.unwrap();
    assert_eq!(
        objects
            .list(None)
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .len(),
        1
    );
    let prefix = store_pg::universe_cas_object_prefix("", id);
    assert_eq!(
        store_pg::delete_objects_under_prefix(objects.as_ref(), &prefix)
            .await
            .unwrap(),
        1
    );
    assert!(
        store_pg::finish_universe_deletion_in_transaction(&mut cleanup, id, 700)
            .await
            .unwrap()
    );
    cleanup.commit().await.unwrap();
    assert!(store.put_bytes(vec![0x74; 32]).await.is_err());
    assert!(
        objects
            .list(None)
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty()
    );
}

async fn exercise_cached_write_fence(pool: &sqlx::PgPool) {
    let id = Uuid::new_v4();
    store_pg::create_universe(pool, id).await.unwrap();
    let cache = std::sync::Arc::new(store_pg::BlobCache::new(1024, 128));
    let store = PgStore::new(pool.clone(), PgStoreConfig::new(id)).with_blob_cache(cache.clone());
    let bytes = vec![0x26; 16];
    let reference = store.put_bytes(bytes.clone()).await.unwrap();
    assert!(cache.get(id, &reference).is_some());
    store_pg::begin_universe_deletion(pool, id, &api::Attribution::Local, 800)
        .await
        .unwrap();
    cache.evict_universe(id);
    assert!(
        store.put_bytes(bytes).await.is_err(),
        "deduplicated writes still require an active universe"
    );
    assert!(
        cache.get(id, &reference).is_none(),
        "a refused write must not repopulate evicted cache entries"
    );
    let mut record = store_pg::list_deleting_universes(pool)
        .await
        .unwrap()
        .remove(0);
    record.deletion.step = api::UniverseDeletionStep::Storage;
    store_pg::update_universe_deletion(pool, id, &record)
        .await
        .unwrap();
    store_pg::finish_universe_deletion(pool, id, 900)
        .await
        .unwrap();
}
