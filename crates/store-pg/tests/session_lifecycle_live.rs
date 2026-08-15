use engine::{
    CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND, CoreAgentCodec,
    CoreAgentEvent, CoreAgentJoins, StoredEvent, UncommittedCoreAgentEvent, WorkflowEndpointRef,
    WorkflowToolConfigEvent,
    session::{EventSeq, SessionId, StoredJoins, UncommittedStoredEvent},
    storage::{
        AppendSessionEvents, CreateClonedSession, CreateForkedSession, CreateSession, ListSessions,
        SessionLifecycleStatus, SessionStore, SessionStoreError,
    },
};
use sqlx::postgres::PgPoolOptions;
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Postgres env"]
async fn pg_live_lifecycle_projection_rejects_managed_branches() {
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
                managed_bindings_event(
                    11,
                    Some(WorkflowEndpointRef {
                        workflow_id: "lifecycle/controller-1".to_owned(),
                        workflow_kind: "lifecycle.workflow.v1".to_owned(),
                    }),
                ),
                lifecycle_event(12, "lightspeed.test.work"),
                lifecycle_event(13, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
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
    assert_eq!(closed_record.closed_at_seq, Some(EventSeq::new(4)));
    assert!(closed_record.managed);

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
    assert!(listed_parent.managed);

    let clone_error = store
        .create_cloned_session(CreateClonedSession {
            source_session_id: parent.clone(),
            session_id: SessionId::new("managed-clone"),
            created_at_ms: 19,
            opening_events: vec![lifecycle_event(19, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND)],
        })
        .await
        .expect_err("managed session cannot be cloned");
    assert!(matches!(
        clone_error,
        SessionStoreError::ManagedSessionCannotBranch { .. }
    ));

    let fork_error = store
        .create_forked_session(CreateForkedSession {
            source_session_id: parent.clone(),
            session_id: SessionId::new("managed-fork"),
            source_seq: EventSeq::new(4),
            created_at_ms: 20,
        })
        .await
        .expect_err("managed session cannot be forked");
    assert!(matches!(
        fork_error,
        SessionStoreError::ManagedSessionCannotBranch { .. }
    ));

    let tool_only = SessionId::new("workflow-tools-without-controller");
    store
        .create_session(CreateSession {
            session_id: tool_only.clone(),
            display_name: None,
            created_at_ms: 30,
        })
        .await
        .expect("create tool-only session");
    store
        .append(AppendSessionEvents {
            session_id: tool_only.clone(),
            expected_head: None,
            events: vec![
                lifecycle_event(31, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND),
                managed_bindings_event(32, None),
            ],
        })
        .await
        .expect("append tool-only workflow declaration");
    let tool_only_record = store
        .load_session(&tool_only)
        .await
        .expect("load tool-only session")
        .expect("tool-only session exists");
    assert!(!tool_only_record.managed);
    let tool_only_fork = store
        .create_forked_session(CreateForkedSession {
            source_session_id: tool_only,
            session_id: SessionId::new("tool-only-fork"),
            source_seq: EventSeq::new(2),
            created_at_ms: 33,
        })
        .await
        .expect("tool-only session remains forkable");
    assert!(!tool_only_fork.managed);

    cleanup_universe(&store).await;
}

fn managed_bindings_event(
    at_ms: u64,
    lifecycle_controller: Option<WorkflowEndpointRef>,
) -> UncommittedStoredEvent {
    CoreAgentCodec
        .encode_uncommitted(&UncommittedCoreAgentEvent {
            observed_at_ms: at_ms,
            joins: CoreAgentJoins::default(),
            event: CoreAgentEvent::WorkflowToolConfig(
                WorkflowToolConfigEvent::ManagedBindingsAdmitted {
                    session_universe_id: Uuid::from_u128(1),
                    declaration_version: 1,
                    lifecycle_controller,
                    creation_fingerprint: "test-creation-fingerprint".to_owned(),
                    bindings: Vec::new(),
                },
            ),
        })
        .expect("encode managed bindings event")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/up.sh or compatible Postgres env"]
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
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run dev/up.sh and source dev/env.sh");
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
