use engine::{
    CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND, StoredEvent,
    session::{EventSeq, SessionId, StoredJoins, UncommittedStoredEvent},
    storage::{
        AppendSessionEvents, CreateForkedSession, CreateSession, ListSessions,
        SessionLifecycleStatus, SessionStore, SessionStoreError,
    },
};
use sqlx::postgres::PgPoolOptions;
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres env"]
async fn pg_live_lifecycle_projection_lists_and_forks_without_lifecycle_replay() {
    let store = live_store().await;
    let parent = SessionId::new("lifecycle-parent");
    let created = store
        .create_session(CreateSession {
            session_id: parent.clone(),
            display_name: Some("Lifecycle parent".to_owned()),
            created_at_ms: 1,
        })
        .await
        .expect("create parent");
    assert_eq!(created.lifecycle_status, SessionLifecycleStatus::New);
    assert_eq!(created.closed_at_seq, None);

    let opened = store
        .append(AppendSessionEvents {
            session_id: parent.clone(),
            expected_head: None,
            events: vec![lifecycle_event(10, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND)],
        })
        .await
        .expect("append opened event");
    let open_record = store
        .load_session(&parent)
        .await
        .expect("load open parent")
        .expect("parent exists");
    assert_eq!(open_record.lifecycle_status, SessionLifecycleStatus::Open);
    assert_eq!(open_record.closed_at_seq, None);

    store
        .append(AppendSessionEvents {
            session_id: parent.clone(),
            expected_head: opened.head,
            events: vec![
                lifecycle_event(11, "lightspeed.test.work"),
                lifecycle_event(12, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
            ],
        })
        .await
        .expect("append work and close events");
    let closed_record = store
        .load_session(&parent)
        .await
        .expect("load closed parent")
        .expect("parent exists");
    assert_eq!(
        closed_record.lifecycle_status,
        SessionLifecycleStatus::Closed
    );
    assert_eq!(closed_record.closed_at_seq, Some(EventSeq::new(3)));

    let listed = store
        .list_sessions(ListSessions {
            cursor: None,
            limit: 10,
        })
        .await
        .expect("list sessions");
    let listed_parent = listed
        .sessions
        .iter()
        .find(|record| record.session_id == parent)
        .expect("parent is listed");
    assert_eq!(
        listed_parent.lifecycle_status,
        SessionLifecycleStatus::Closed
    );

    let before_close = store
        .create_forked_session(CreateForkedSession {
            source_session_id: parent.clone(),
            session_id: SessionId::new("fork-before-close"),
            source_seq: EventSeq::new(2),
            created_at_ms: 20,
        })
        .await
        .expect("fork before close event");
    assert_eq!(before_close.lifecycle_status, SessionLifecycleStatus::Open);
    assert_eq!(before_close.closed_at_seq, None);

    let through_close = store
        .create_forked_session(CreateForkedSession {
            source_session_id: parent,
            session_id: SessionId::new("fork-through-close"),
            source_seq: EventSeq::new(3),
            created_at_ms: 21,
        })
        .await
        .expect("fork through close event");
    assert_eq!(
        through_close.lifecycle_status,
        SessionLifecycleStatus::Closed
    );
    assert_eq!(through_close.closed_at_seq, Some(EventSeq::new(3)));

    cleanup_universe(&store).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres env"]
async fn pg_live_delete_is_closed_only_and_preserves_fork_history() {
    let store = live_store().await;
    let parent = SessionId::new("delete-parent");
    store
        .create_session(CreateSession {
            session_id: parent.clone(),
            display_name: None,
            created_at_ms: 1,
        })
        .await
        .expect("create parent");
    let opened = store
        .append(AppendSessionEvents {
            session_id: parent.clone(),
            expected_head: None,
            events: vec![lifecycle_event(10, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND)],
        })
        .await
        .expect("open parent");

    assert!(matches!(
        store.delete_closed_session(&parent).await,
        Err(SessionStoreError::SessionNotClosed {
            lifecycle_status: SessionLifecycleStatus::Open,
            ..
        })
    ));

    store
        .append(AppendSessionEvents {
            session_id: parent.clone(),
            expected_head: opened.head,
            events: vec![lifecycle_event(11, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND)],
        })
        .await
        .expect("close parent");
    let child = SessionId::new("delete-child");
    store
        .create_forked_session(CreateForkedSession {
            source_session_id: parent.clone(),
            session_id: child.clone(),
            source_seq: EventSeq::new(2),
            created_at_ms: 20,
        })
        .await
        .expect("create closed fork child");

    assert!(matches!(
        store.delete_closed_session(&parent).await,
        Err(SessionStoreError::SessionHasForkChildren { .. })
    ));
    let deleted_child = store
        .delete_closed_session(&child)
        .await
        .expect("delete closed leaf");
    assert_eq!(
        deleted_child.lifecycle_status,
        SessionLifecycleStatus::Closed
    );
    assert!(
        store
            .load_session(&child)
            .await
            .expect("load deleted child")
            .is_none()
    );

    let deleted_parent = store
        .delete_closed_session(&parent)
        .await
        .expect("delete parent after leaf");
    assert_eq!(
        deleted_parent.lifecycle_status,
        SessionLifecycleStatus::Closed
    );
    assert!(matches!(
        store.delete_closed_session(&parent).await,
        Err(SessionStoreError::SessionNotFound { .. })
    ));

    cleanup_universe(&store).await;
}

fn lifecycle_event(at_ms: u64, kind: &'static str) -> UncommittedStoredEvent {
    UncommittedStoredEvent {
        observed_at_ms: at_ms,
        joins: StoredJoins::default(),
        event: StoredEvent::new(kind, 1, serde_json::Value::Object(Default::default())),
    }
}

async fn live_store() -> PgStore {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect(
        "LIGHTSPEED_TEST_POSTGRES_URL must be set; run local/up.sh and source local/env.sh",
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    PgStore::migrate(&pool)
        .await
        .expect("apply store-pg migrations");
    let store = PgStore::new(pool, PgStoreConfig::new(Uuid::new_v4()));
    store.ensure_universe().await.expect("ensure test universe");
    store
}

async fn cleanup_universe(store: &PgStore) {
    sqlx::query("DELETE FROM universes WHERE universe_id = $1")
        .bind(store.config().universe_id)
        .execute(store.pool())
        .await
        .expect("clean up test universe");
}
