use harness::{
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
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn pg_live_lifecycle_projection_rejects_managed_branches() {
    let store = live_store().await;
    let parent = SessionId::new("lifecycle-parent");
    let created = store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: parent.clone(),
            display_name: Some("Lifecycle parent".to_owned()),
            origin: None,
            delete_after_close_ms: None,
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
            limit: 10,
            ..Default::default()
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
            metadata: Default::default(),
            session_id: tool_only.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
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
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn pg_live_delete_is_closed_only_and_preserves_fork_history() {
    let store = live_store().await;
    let parent = SessionId::new("delete-parent");
    store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: parent.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
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
        Err(SessionStoreError::SessionHasChildren { .. })
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn pg_live_retention_deadline_follows_close_time_and_policy() {
    let store = live_store().await;
    let session_id = SessionId::new("retention-root");
    let created = store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: Some(100),
            created_at_ms: 1,
        })
        .await
        .expect("create retention root");
    assert_eq!(created.delete_at_ms, None);

    let closed = store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![
                lifecycle_event(10, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND),
                lifecycle_event(20, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
            ],
        })
        .await
        .expect("close retention root");
    let record = store.load_session(&session_id).await.unwrap().unwrap();
    assert_eq!(record.closed_at_ms, Some(20));
    assert_eq!(record.delete_at_ms, Some(120));

    let fork = store
        .create_forked_session(CreateForkedSession {
            source_session_id: session_id.clone(),
            session_id: SessionId::new("retention-fork"),
            source_seq: EventSeq::new(2),
            created_at_ms: 21,
        })
        .await
        .expect("fork retention root");
    assert_eq!(fork.retention_root_session_id, session_id);
    assert_eq!(fork.closed_at_ms, Some(20));
    assert_eq!(fork.delete_after_close_ms, None);
    assert_eq!(fork.delete_at_ms, None);

    for (duration, deadline) in [(None, None), (Some(50), Some(70))] {
        let updated = store
            .set_session_retention(&session_id, duration)
            .await
            .expect("replace retention policy on closed root");
        assert_eq!(updated.delete_at_ms, deadline);
        assert_eq!(updated.head, closed.head);
        assert_eq!(updated.updated_at_ms, 20);
    }
    assert!(
        store
            .list_retention_roots_due_for_deletion(69, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let due = store
        .list_retention_roots_due_for_deletion(70, 10)
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].session_id, session_id);

    let opened = store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: closed.head,
            events: vec![lifecycle_event(30, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND)],
        })
        .await
        .expect("reopen retention root");
    let record = store.load_session(&session_id).await.unwrap().unwrap();
    assert_eq!(record.closed_at_ms, None);
    assert_eq!(record.delete_after_close_ms, Some(50));
    assert_eq!(record.delete_at_ms, None);
    assert!(
        store
            .list_retention_roots_due_for_deletion(100, 10)
            .await
            .unwrap()
            .is_empty()
    );

    store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: opened.head,
            events: vec![lifecycle_event(40, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND)],
        })
        .await
        .expect("close retention root again");
    let record = store.load_session(&session_id).await.unwrap().unwrap();
    assert_eq!(record.delete_at_ms, Some(90));

    // The database derives the deadline even when a writer only knows the policy.
    let deadline: Option<i64> = sqlx::query_scalar(
        "UPDATE sessions SET delete_after_close_ms = 25
         WHERE universe_id = $1 AND session_id = $2 RETURNING delete_at_ms",
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("change policy directly");
    assert_eq!(deadline, Some(65));
    let error = sqlx::query(
        "UPDATE sessions SET delete_at_ms = 999
         WHERE universe_id = $1 AND session_id = $2",
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .execute(store.pool())
    .await
    .expect_err("generated deadline cannot be independently written");
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("428C9")
    );
    assert!(matches!(
        store
            .set_session_retention(&session_id, Some(i64::MAX as u64))
            .await,
        Err(SessionStoreError::Store { .. })
    ));
    let record = store.load_session(&session_id).await.unwrap().unwrap();
    assert_eq!(record.delete_after_close_ms, Some(25));
    assert_eq!(record.delete_at_ms, Some(65));

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
        .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh");
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn permanent_delete_checks_lifecycle_and_removes_retained_descendants() {
    use harness::{session::SessionPosition, storage::DeleteClosedSessions};
    let store = live_store().await;
    let root = SessionId::new("permanent-root");
    let fork = SessionId::new("permanent-fork");
    let clone_id = SessionId::new("independent-clone");
    let create = CreateSession {
        session_id: root.clone(),
        display_name: None,
        metadata: Default::default(),
        origin: None,
        delete_after_close_ms: None,
        created_at_ms: 1,
    };
    store.create_session(create.clone()).await.unwrap();
    store
        .append(AppendSessionEvents {
            session_id: root.clone(),
            expected_head: None,
            events: vec![lifecycle_event(10, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND)],
        })
        .await
        .unwrap();
    let deletion = DeleteClosedSessions {
        session_id: root.clone(),
        cascade: true,
        due_at_or_before_ms: None,
    };
    assert!(matches!(
        store.delete_closed_sessions(deletion.clone()).await,
        Err(SessionStoreError::SessionNotClosed { .. })
    ));
    store
        .create_forked_session(CreateForkedSession {
            session_id: fork.clone(),
            source_session_id: root.clone(),
            source_seq: EventSeq::new(1),
            created_at_ms: 11,
        })
        .await
        .unwrap();
    store
        .create_cloned_session(CreateClonedSession {
            session_id: clone_id.clone(),
            source_session_id: root.clone(),
            created_at_ms: 12,
            opening_events: vec![],
        })
        .await
        .unwrap();
    store
        .append(AppendSessionEvents {
            session_id: root.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(1),
            }),
            events: vec![lifecycle_event(20, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND)],
        })
        .await
        .unwrap();
    assert!(matches!(
        store.delete_closed_sessions(deletion.clone()).await,
        Err(SessionStoreError::SessionTreeNotClosed { .. })
    ));
    assert!(store.load_session(&root).await.unwrap().is_some());
    store
        .append(AppendSessionEvents {
            session_id: fork.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(1),
            }),
            events: vec![lifecycle_event(21, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND)],
        })
        .await
        .unwrap();
    store
        .delete_closed_sessions_with_options(
            DeleteClosedSessions {
                session_id: fork.clone(),
                cascade: false,
                due_at_or_before_ms: None,
            },
            true,
            false,
        )
        .await
        .unwrap();
    // A retained fork still depends on the parent's events, even while hidden.
    assert!(matches!(
        store
            .delete_closed_sessions(DeleteClosedSessions {
                cascade: false,
                ..deletion.clone()
            })
            .await,
        Err(SessionStoreError::SessionHasChildren { .. })
    ));
    let deleted = store.delete_closed_sessions(deletion).await.unwrap();
    assert_eq!(deleted.deleted_session_ids.len(), 2);
    let retained: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE universe_id=$1 AND session_id=ANY($2)",
    )
    .bind(store.config().universe_id)
    .bind([root.as_str(), fork.as_str()])
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(retained, 0);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM session_events WHERE universe_id=$1 AND session_id=ANY($2)",
    )
    .bind(store.config().universe_id)
    .bind([root.as_str(), fork.as_str()])
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(events, 0);
    assert!(
        store
            .load_session(&clone_id)
            .await
            .unwrap()
            .unwrap()
            .source_session_id
            .is_none()
    );
    store
        .create_session(create)
        .await
        .expect("permanent deletion releases the session ID");
    cleanup_universe(&store).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn soft_delete_hides_history_until_explicit_permanent_deletion() {
    use harness::storage::{DeleteClosedSessions, ReadSessionEventRange, ReadSessionEvents};
    let store = live_store().await;
    let root = SessionId::new("deleted-root");
    let create = CreateSession {
        session_id: root.clone(),
        display_name: None,
        metadata: Default::default(),
        origin: None,
        delete_after_close_ms: Some(100),
        created_at_ms: 1,
    };
    store.create_session(create.clone()).await.unwrap();
    let deletion = DeleteClosedSessions {
        session_id: root.clone(),
        cascade: true,
        due_at_or_before_ms: None,
    };
    assert!(matches!(
        store.delete_closed_sessions(deletion.clone()).await,
        Err(SessionStoreError::SessionNotClosed { .. })
    ));
    assert!(matches!(
        store.purge_deleted_session(root.as_str()).await,
        Err(store_pg::PurgeSessionError::NotDeleted { .. })
    ));
    store
        .append(AppendSessionEvents {
            session_id: root.clone(),
            expected_head: None,
            events: vec![
                lifecycle_event(10, CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND),
                lifecycle_event(20, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
            ],
        })
        .await
        .unwrap();
    let fork = SessionId::new("deleted-fork");
    store
        .create_forked_session(CreateForkedSession {
            session_id: fork.clone(),
            source_session_id: root.clone(),
            source_seq: EventSeq::new(2),
            created_at_ms: 21,
        })
        .await
        .unwrap();
    let clone_id = SessionId::new("independent-clone");
    store
        .create_cloned_session(CreateClonedSession {
            session_id: clone_id.clone(),
            source_session_id: root.clone(),
            created_at_ms: 22,
            opening_events: vec![],
        })
        .await
        .unwrap();
    assert!(matches!(
        store
            .delete_closed_sessions(DeleteClosedSessions {
                cascade: false,
                ..deletion.clone()
            })
            .await,
        Err(SessionStoreError::SessionHasChildren { .. })
    ));
    let access = store_pg::PgAccessStore::new(store.pool().clone());
    assert!(
        access
            .share_session(store.config().universe_id, root.as_str())
            .await
            .unwrap()
    );
    assert!(matches!(
        store
            .delete_closed_sessions_with_options(deletion.clone(), true, true)
            .await,
        Err(SessionStoreError::SessionNotFound { .. })
    ));
    assert!(store.load_session(&root).await.unwrap().is_some());
    assert!(store.load_session(&fork).await.unwrap().is_some());
    assert!(
        access
            .share_session(store.config().universe_id, fork.as_str())
            .await
            .unwrap()
    );
    let deleted = store
        .delete_closed_sessions_with_options(deletion.clone(), true, true)
        .await
        .unwrap();
    assert_eq!(deleted.deleted_session_ids.len(), 2);
    for id in [&root, &fork] {
        assert!(store.load_session(id).await.unwrap().is_none());
        assert!(matches!(
            store
                .read_after(ReadSessionEvents {
                    session_id: id.clone(),
                    after: None,
                    limit: 10
                })
                .await,
            Err(SessionStoreError::SessionNotFound { .. })
        ));
        assert!(matches!(
            store
                .read_range(ReadSessionEventRange {
                    session_id: id.clone(),
                    after: EventSeq::new(2),
                    through: EventSeq::new(2),
                    limit: 10
                })
                .await,
            Err(SessionStoreError::SessionNotFound { .. })
        ));
        assert!(
            store_pg::PgAccessStore::new(store.pool().clone())
                .session_access(store.config().universe_id, id.as_str())
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store
                .set_session_display_name(id, Some("resurrect".into()))
                .await,
            Err(SessionStoreError::SessionNotFound { .. })
        ));
    }
    assert_eq!(
        store
            .list_sessions(ListSessions {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    assert!(
        store
            .load_session(&clone_id)
            .await
            .unwrap()
            .unwrap()
            .source_session_id
            .is_none()
    );
    assert!(
        store
            .list_retention_roots_due_for_deletion(u64::MAX / 2, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let retained: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE universe_id=$1 AND deleted_at_ms IS NOT NULL",
    )
    .bind(store.config().universe_id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(retained, 2);
    assert!(matches!(
        store.create_session(create).await,
        Err(SessionStoreError::SessionAlreadyExists { .. })
    ));
    assert!(matches!(
        store.delete_closed_sessions(deletion).await,
        Err(SessionStoreError::SessionNotFound { .. })
    ));
    let listed = store.list_deleted_sessions(None, 100).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        store
            .list_deleted_sessions(Some(&listed[0].0), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .purge_deleted_session(root.as_str())
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(
        store
            .purge_deleted_session(root.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_deleted_sessions(None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM session_events WHERE universe_id=$1 AND session_id=ANY($2)",
    )
    .bind(store.config().universe_id)
    .bind(vec![root.as_str(), fork.as_str()])
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(remaining, 0);
    assert!(store.load_session(&clone_id).await.unwrap().is_some());
    cleanup_universe(&store).await;
}
