use std::{path::PathBuf, sync::Arc};

use auth::{
    AuthFlowId, AuthFlowStore, AuthGrantId, AuthGrantStatus, AuthGrantStore, AuthGrantTokenRefresh,
    AuthProviderKind, AuthRegistryError, CreateAuthFlowRecord, CreateAuthGrantRecord,
    CreateOAuthClientRecord, FinishAuthFlow, GrantRefreshLock, ListAuthGrants, OAuthClientId,
    OAuthClientStore, PutSecretRecord, SECRET_KIND_STATIC_BEARER, SecretId, SecretStore,
    SecretValue, TokenEndpointAuthMethod, state_hash,
};
use environment_protocol::{ids::ProviderTargetId, routes::DataRouteBase};
use environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment, DaemonBuild,
    EnvironmentCloseReason, EnvironmentEndpoint, EnvironmentId, EnvironmentProviderBindingStore,
    EnvironmentProviderId, EnvironmentProviderStore, EnvironmentProvisionRequestId,
    EnvironmentRegistryError, EnvironmentState, EnvironmentStore, EnvironmentTemplateId,
    FinishCloseEnvironment, ListEnvironments, ProviderCredentialChange, PutEnvironmentProvider,
    PutEnvironmentProviderBinding,
};
use harness::{
    BlobRef, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND,
    session::{
        EventSeq, SessionId, SessionPosition, StoredEvent, StoredJoins, UncommittedStoredEvent,
    },
    storage::{
        AdvanceSessionCheckpoint, AppendSessionEvents, BlobEdge, BlobGraphStore, BlobStore,
        CreateClonedSession, CreateForkedSession, CreateSession, DeleteClosedSessions,
        ListSessions, ReadSessionEventRange, ReadSessionEvents, SessionCheckpoint, SessionOrigin,
        SessionOriginKind, SessionStore, SessionStoreError, ensure_harness_blobs,
        harness_blob_refs,
    },
};
use mcp::{
    ListMcpServers, McpApprovalPolicy, McpRegistryError, McpRegistryStore, McpServerAuthPolicy,
    McpServerId, McpServerStatus, PutMcpServerRecord, RemoteMcpTransport,
};
use object_store::{ObjectStore, aws::AmazonS3Builder};
use profiles::{ProfileError, ProfileStore};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use store_pg::{PgStore, PgStoreConfig, SecretsMasterKey};
use tokio::sync::OnceCell;
use uuid::Uuid;
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsSnapshotRecord,
    VfsSnapshotSource, VfsSnapshotStore, VfsTotals, VfsWorkspaceId, VfsWorkspaceStore,
};

static MIGRATED: OnceCell<()> = OnceCell::const_new();

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_sessions_are_isolated_by_universe() {
    let left = live_store("sessions-left", 64).await;
    let right = live_store("sessions-right", 64).await;
    let session_id = SessionId::new("same-session");

    left.create_session(CreateSession {
        metadata: Default::default(),
        session_id: session_id.clone(),
        display_name: None,
        origin: None,
        delete_after_close_ms: None,
        created_at_ms: 1,
    })
    .await
    .expect("create left session");
    let appended = left
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![open_event(10), open_event(11)],
        })
        .await
        .expect("append left events");

    assert_eq!(
        appended.head.as_ref().map(|head| head.seq),
        Some(EventSeq::new(2))
    );
    assert!(
        right
            .load_session(&session_id)
            .await
            .expect("load right session")
            .is_none(),
        "same session id must not leak across universes"
    );

    right
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 20,
        })
        .await
        .expect("create right session");
    let right_page = right
        .read_after(ReadSessionEvents {
            session_id,
            after: None,
            limit: 10,
        })
        .await
        .expect("read right events");
    assert!(right_page.entries.is_empty());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_session_ranges_are_fenced_and_checkpoint_pointers_only_advance() {
    let store = live_store("session-checkpoints", 64).await;
    let session_id = SessionId::new("checkpoint-session");
    store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create checkpoint session");
    store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: None,
            events: (1..=5).map(open_event).collect(),
        })
        .await
        .expect("append checkpoint session events");

    let first = store
        .read_range(ReadSessionEventRange {
            session_id: session_id.clone(),
            after: EventSeq::new(1),
            through: EventSeq::new(4),
            limit: 2,
        })
        .await
        .expect("read first fenced range page");
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.position.seq.as_u64())
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(first.next_after, Some(EventSeq::new(3)));
    assert!(!first.complete);

    let second = store
        .read_range(ReadSessionEventRange {
            session_id: session_id.clone(),
            after: first.next_after.expect("first range cursor"),
            through: EventSeq::new(4),
            limit: 2,
        })
        .await
        .expect("read second fenced range page");
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.position.seq.as_u64())
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(second.complete);

    let newest_bytes = b"newest checkpoint".to_vec();
    let newest_ref = store
        .put_bytes(newest_bytes.clone())
        .await
        .expect("put newest checkpoint blob");
    let newest = SessionCheckpoint {
        session_id: session_id.clone(),
        through_seq: EventSeq::new(4),
        format_version: 1,
        state_ref: newest_ref.clone(),
        lineage_source_session_id: None,
        lineage_source_seq: None,
        byte_len: newest_bytes.len() as u64,
        created_at_ms: 20,
    };
    assert!(
        store
            .advance_checkpoint(AdvanceSessionCheckpoint {
                checkpoint: newest.clone(),
            })
            .await
            .expect("advance checkpoint")
    );

    let stale_bytes = b"stale checkpoint".to_vec();
    let stale_ref = store
        .put_bytes(stale_bytes.clone())
        .await
        .expect("put stale checkpoint blob");
    assert!(
        !store
            .advance_checkpoint(AdvanceSessionCheckpoint {
                checkpoint: SessionCheckpoint {
                    session_id: session_id.clone(),
                    through_seq: EventSeq::new(2),
                    format_version: 1,
                    state_ref: stale_ref,
                    lineage_source_session_id: None,
                    lineage_source_seq: None,
                    byte_len: stale_bytes.len() as u64,
                    created_at_ms: 30,
                },
            })
            .await
            .expect("reject stale checkpoint")
    );
    assert_eq!(
        store
            .load_checkpoint(&session_id)
            .await
            .expect("load checkpoint"),
        Some(newest)
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_session_list_pages_newest_first_and_rename_persists() {
    use harness::storage::{ListSessions, SessionListCursor};

    let store = live_store("session-list", 64).await;
    let other = live_store("session-list-other", 64).await;
    other
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: SessionId::new("other-universe"),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 999,
        })
        .await
        .expect("create other-universe session");

    for (name, created_at_ms) in [("list-a", 10), ("list-b", 20), ("list-c", 30)] {
        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: SessionId::new(name),
                display_name: Some(format!("Session {name}")),
                origin: None,
                delete_after_close_ms: None,
                created_at_ms,
            })
            .await
            .expect("create session");
    }
    // Activity on the oldest session moves it to the top of the list.
    store
        .append(AppendSessionEvents {
            session_id: SessionId::new("list-a"),
            expected_head: None,
            events: vec![open_event(40)],
        })
        .await
        .expect("append to list-a");

    let first = store
        .list_sessions(ListSessions {
            limit: 2,
            ..Default::default()
        })
        .await
        .expect("first page");
    assert_eq!(
        first
            .sessions
            .iter()
            .map(|record| record.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["list-a", "list-c"]
    );
    assert_eq!(
        first.sessions[0].display_name.as_deref(),
        Some("Session list-a")
    );
    let cursor: SessionListCursor = first.next_cursor.expect("more rows remain");

    let second = store
        .list_sessions(ListSessions {
            cursor: Some(cursor),
            limit: 2,
            ..Default::default()
        })
        .await
        .expect("second page");
    assert_eq!(
        second
            .sessions
            .iter()
            .map(|record| record.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["list-b"],
        "other universes must not leak into the page"
    );
    assert!(second.next_cursor.is_none());

    let renamed = store
        .set_session_display_name(&SessionId::new("list-b"), Some("Renamed".to_owned()))
        .await
        .expect("rename session");
    assert_eq!(renamed.display_name.as_deref(), Some("Renamed"));
    assert_eq!(
        renamed.updated_at_ms, 20,
        "rename must not count as session activity"
    );
    let cleared = store
        .set_session_display_name(&SessionId::new("list-b"), None)
        .await
        .expect("clear display name");
    assert_eq!(cleared.display_name, None);
    assert!(matches!(
        store
            .set_session_display_name(&SessionId::new("missing"), None)
            .await,
        Err(harness::storage::SessionStoreError::SessionNotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_clone_copies_resources_and_links_sessions() {
    let store = live_store("session-graph-clone", 1024).await;
    let source_id = SessionId::new("source-session");
    let clone_id = SessionId::new("clone-session");
    let peer_id = SessionId::new("peer-session");
    for session_id in [&source_id, &peer_id] {
        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
    }
    store
        .append(AppendSessionEvents {
            session_id: source_id.clone(),
            expected_head: None,
            events: vec![open_event(10), open_event(11)],
        })
        .await
        .expect("append source events");

    let snapshot_ref = store
        .put_bytes(b"snapshot manifest".to_vec())
        .await
        .expect("put snapshot");
    store
        .record_snapshot(VfsSnapshotRecord {
            snapshot_ref: snapshot_ref.clone(),
            source: VfsSnapshotSource::new("inline").with_subject("seed"),
            display_name: Some("Seed".to_owned()),
            created_at_ms: 12,
        })
        .await
        .expect("record snapshot");
    let workspace_id = VfsWorkspaceId::new("workspace-graph");
    store
        .create_workspace(CreateVfsWorkspaceRecord {
            workspace_id: workspace_id.clone(),
            display_name: None,
            base_snapshot_ref: Some(snapshot_ref.clone()),
            head_snapshot_ref: snapshot_ref.clone(),
            head_totals: VfsTotals::default(),
            created_at_ms: 13,
        })
        .await
        .expect("create workspace");
    let clone = store
        .create_cloned_session(CreateClonedSession {
            source_session_id: source_id.clone(),
            session_id: clone_id.clone(),
            created_at_ms: 20,
            opening_events: vec![open_event(21)],
        })
        .await
        .expect("clone session");
    assert_eq!(clone.source_session_id, Some(source_id.clone()));
    assert_eq!(clone.source_seq, None);
    assert_eq!(
        clone.head.as_ref().map(|head| head.seq),
        Some(EventSeq::new(1))
    );

    let workspace_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vfs_workspaces WHERE universe_id = $1")
            .bind(store.config().universe_id)
            .fetch_one(store.pool())
            .await
            .expect("count workspaces");
    assert_eq!(workspace_count, 1);
    // Sub-agent lineage: the child row is the root-scoped reservation.
    let limits = harness::SubagentLimits {
        max_depth: 1,
        max_descendants: 2,
        max_concurrent: 1,
        deadline_ms: 1_000,
    };
    let origin = |invocation: &str, depth: u32| SessionOrigin {
        kind: SessionOriginKind::Subagent,
        parent_session_id: peer_id.clone(),
        parent_run_id: 1,
        root_session_id: peer_id.clone(),
        depth,
        invocation_id: invocation.to_owned(),
        profile_id: "reviewer".to_owned(),
        profile_revision: 3,
        limits,
    };
    let child = store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: SessionId::new("child-1"),
            display_name: Some("reviewer: first".to_owned()),
            origin: Some(origin("inv-1", 1)),
            delete_after_close_ms: None,
            created_at_ms: 40,
        })
        .await
        .expect("create child with origin");
    assert_eq!(child.origin, Some(origin("inv-1", 1)));
    assert_eq!(
        store
            .load_session(&SessionId::new("child-1"))
            .await
            .expect("load child")
            .and_then(|record| record.origin),
        Some(origin("inv-1", 1))
    );
    let listed = store
        .list_sessions(ListSessions {
            limit: 10,
            trees: vec![peer_id.clone()],
            subagent: Some(true),
            ..Default::default()
        })
        .await
        .expect("list by root");
    assert_eq!(
        listed
            .sessions
            .iter()
            .map(|record| record.session_id.clone())
            .collect::<Vec<_>>(),
        vec![SessionId::new("child-1")]
    );
    // max_concurrent = 1: a second open child is refused.
    let refused = store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: SessionId::new("child-2"),
            display_name: None,
            origin: Some(origin("inv-2", 1)),
            delete_after_close_ms: None,
            created_at_ms: 41,
        })
        .await
        .expect_err("second concurrent child is refused");
    assert!(matches!(
        refused,
        SessionStoreError::OriginLimitExceeded {
            limit: harness::storage::SessionOriginLimit::MaxConcurrent,
            ..
        }
    ));
    // max_depth = 1: depth 2 is refused before any counting.
    let too_deep = store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: SessionId::new("child-3"),
            display_name: None,
            origin: Some(origin("inv-3", 2)),
            delete_after_close_ms: None,
            created_at_ms: 42,
        })
        .await
        .expect_err("depth beyond the limit is refused");
    assert!(matches!(
        too_deep,
        SessionStoreError::OriginLimitExceeded {
            limit: harness::storage::SessionOriginLimit::MaxDepth,
            ..
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_fork_stitches_reads_and_clamps_parent_tail() {
    let store = live_store("session-graph-fork", 1024).await;
    let root = SessionId::new("root-session");
    store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: root.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create root");
    store
        .append(AppendSessionEvents {
            session_id: root.clone(),
            expected_head: None,
            events: vec![open_event(10), open_event(11), open_event(12)],
        })
        .await
        .expect("append root");

    let fork = SessionId::new("fork-session");
    let fork_record = store
        .create_forked_session(CreateForkedSession {
            source_session_id: root.clone(),
            session_id: fork.clone(),
            source_seq: EventSeq::new(2),
            created_at_ms: 20,
        })
        .await
        .expect("fork root");
    assert_eq!(fork_record.source_session_id, Some(root.clone()));
    assert_eq!(fork_record.source_seq, Some(EventSeq::new(2)));
    assert_eq!(
        fork_record.head.as_ref().map(|head| head.seq),
        Some(EventSeq::new(2))
    );
    let appended = store
        .append(AppendSessionEvents {
            session_id: fork.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(2),
            }),
            events: vec![open_event(21), open_event(22)],
        })
        .await
        .expect("append fork");
    assert_eq!(
        appended
            .entries
            .iter()
            .map(|entry| entry.position.seq)
            .collect::<Vec<_>>(),
        vec![EventSeq::new(3), EventSeq::new(4)]
    );

    store
        .append(AppendSessionEvents {
            session_id: root,
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(3),
            }),
            events: vec![open_event(30)],
        })
        .await
        .expect("append hidden parent tail");

    let child = SessionId::new("fork-child-session");
    store
        .create_forked_session(CreateForkedSession {
            source_session_id: fork.clone(),
            session_id: child.clone(),
            source_seq: EventSeq::new(3),
            created_at_ms: 40,
        })
        .await
        .expect("fork fork");
    store
        .append(AppendSessionEvents {
            session_id: child.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(3),
            }),
            events: vec![open_event(41)],
        })
        .await
        .expect("append child");

    let page = store
        .read_after(ReadSessionEvents {
            session_id: child,
            after: Some(EventSeq::new(1)),
            limit: 10,
        })
        .await
        .expect("read stitched child");
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.position.seq.as_u64())
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.observed_at_ms)
            .collect::<Vec<_>>(),
        vec![11, 21, 41]
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_deployment_universe_lifecycle_stats_and_purge() {
    // inline_threshold 8: the second blob lands in the object store, so the
    // purge has real external bytes to sweep.
    let store = live_store("operator-universe", 8).await;
    let pool = store.pool().clone();
    let universe_id = store.config().universe_id;

    // create_universe is idempotent; the live_store already ensured the row.
    assert!(
        !store_pg::create_universe(&pool, universe_id)
            .await
            .expect("create existing universe"),
        "existing universe must not report created"
    );
    let fresh_id = Uuid::new_v4();
    assert!(
        store_pg::create_universe(&pool, fresh_id)
            .await
            .expect("create fresh universe")
    );
    let fresh = store_pg::read_universe_stats(&pool, fresh_id)
        .await
        .expect("read fresh stats")
        .expect("fresh universe exists");
    assert!(fresh.created_at_ms > 0, "creation must be timestamped");
    assert_eq!(
        (fresh.sessions, fresh.workspaces, fresh.profiles),
        (0, 0, 0)
    );
    assert_eq!(fresh.last_activity_at_ms, None);

    // Populate the universe: a session with events, blobs (inline + object).
    let session_id = SessionId::new("operator-session");
    store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 5,
        })
        .await
        .expect("create session");
    store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![open_event(42)],
        })
        .await
        .expect("append event");
    store
        .put_bytes(b"tiny".to_vec())
        .await
        .expect("put inline blob");
    store
        .put_bytes(b"external object payload".to_vec())
        .await
        .expect("put object blob");

    let stats = store_pg::read_universe_stats(&pool, universe_id)
        .await
        .expect("read stats")
        .expect("universe exists");
    assert_eq!(stats.sessions, 1);
    assert_eq!(stats.last_activity_at_ms, Some(42));
    assert!(stats.blob_bytes >= 27, "both blobs count into blob_bytes");
    let listed = store_pg::list_universe_stats(&pool)
        .await
        .expect("list stats");
    assert!(listed.iter().any(|entry| entry.universe_id == universe_id));

    let object_keys = store_pg::list_universe_object_keys(&pool, universe_id)
        .await
        .expect("list object keys");
    assert_eq!(object_keys.len(), 1, "only the large blob is external");
    let session_ids = store_pg::list_universe_session_ids(&pool, universe_id)
        .await
        .expect("list session ids");
    assert_eq!(session_ids, vec!["operator-session".to_owned()]);

    // Purge: sweep the external object, then delete the row (cascade).
    let object_store = store.object_store().expect("live object store").clone();
    for key in &object_keys {
        use object_store::ObjectStoreExt as _;
        object_store
            .delete(&object_store::path::Path::from(key.as_str()))
            .await
            .expect("delete external object");
    }
    assert!(
        store_pg::delete_universe(&pool, universe_id)
            .await
            .expect("delete universe")
    );
    assert!(
        !store_pg::delete_universe(&pool, universe_id)
            .await
            .expect("re-delete universe"),
        "delete is idempotent"
    );
    assert!(
        store_pg::read_universe_stats(&pool, universe_id)
            .await
            .expect("read purged stats")
            .is_none()
    );
    assert!(
        store
            .load_session(&session_id)
            .await
            .expect("load purged session")
            .is_none(),
        "sessions cascade with the universe row"
    );

    store_pg::delete_universe(&pool, fresh_id)
        .await
        .expect("clean up fresh universe");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_blobs_use_inline_and_object_storage() {
    let store = live_store("blobs", 8).await;

    let inline_ref = store
        .put_bytes(b"small".to_vec())
        .await
        .expect("put inline blob");
    let object_ref = store
        .put_bytes(b"large object payload".to_vec())
        .await
        .expect("put object blob");

    assert_eq!(
        store
            .read_text(&inline_ref)
            .await
            .expect("read inline blob"),
        "small"
    );
    assert_eq!(
        store
            .read_text(&object_ref)
            .await
            .expect("read object blob"),
        "large object payload"
    );
    assert_eq!(
        store
            .stat_blob(&object_ref)
            .await
            .expect("stat object blob")
            .byte_len,
        20
    );

    let inline_layout = blob_layout(&store, &inline_ref).await;
    assert_eq!(inline_layout.storage_kind, "inline");
    assert!(inline_layout.has_inline_bytes);
    assert!(inline_layout.object_key.is_none());

    let object_layout = blob_layout(&store, &object_ref).await;
    assert_eq!(object_layout.storage_kind, "object");
    assert!(!object_layout.has_inline_bytes);
    assert!(
        object_layout
            .object_key
            .as_deref()
            .expect("object key")
            .contains(&format!(
                "universes/{}/cas/blobs",
                store.config().universe_id
            ))
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_streamed_blobs_verify_ranges_reuse_and_reject_corruption() {
    use harness::storage::{BlobSource, BlobStoreError};

    struct Source {
        bytes: Vec<u8>,
        offset: usize,
        reads: usize,
    }
    #[async_trait::async_trait]
    impl BlobSource for Source {
        async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
            assert!(max_bytes <= 256 * 1024, "stream reads must stay bounded");
            self.reads += 1;
            let end = (self.offset + max_bytes).min(self.bytes.len());
            let chunk = self.bytes[self.offset..end].to_vec();
            self.offset = end;
            Ok(chunk)
        }
    }

    let store = live_store("streamed-blobs", 64).await;
    // Exercise PostgreSQL inline slicing and a real multipart MinIO upload.
    for size in [31, 10 * 1024 * 1024 + 13] {
        let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let expected = BlobRef::from_bytes(&bytes);
        let mut source = Source {
            bytes: bytes.clone(),
            offset: 0,
            reads: 0,
        };
        assert_eq!(
            store
                .put_stream(&expected, size as u64, &mut source)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(source.offset, size);
        let layout = blob_layout(&store, &expected).await;
        assert_eq!(
            layout.storage_kind,
            if size <= 64 { "inline" } else { "object" }
        );
        for offset in [0, 17, size.saturating_sub(7), size, size + 1] {
            let range = store
                .read_blob_range(&expected, offset as u64, 256 * 1024)
                .await
                .unwrap();
            assert_eq!(
                range,
                bytes[offset.min(size)..(offset + 256 * 1024).min(size)]
            );
        }
        assert_eq!(store.read_bytes(&expected).await.unwrap(), bytes);

        age_all_blobs(&store, 1_000).await;
        let mut unused = Source {
            bytes: Vec::new(),
            offset: 0,
            reads: 0,
        };
        assert_eq!(
            store
                .put_stream(&expected, size as u64, &mut unused)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            unused.reads, 0,
            "existing content must not be uploaded again"
        );
        assert_eq!(
            blob_layout(&store, &expected).await.object_key,
            layout.object_key
        );
        assert!(store.blob_timestamps(&expected).await.unwrap().unwrap().1 > 1_000);
        assert_eq!(store.put_bytes(bytes.clone()).await.unwrap(), expected);

        // Use an absent digest so both inline and multipart paths must verify
        // the input, rather than returning an already stored blob.
        let bad_ref = BlobRef::from_bytes(format!("wrong digest for {size}").as_bytes());
        let mut corrupted = Source {
            bytes,
            offset: 0,
            reads: 0,
        };
        assert!(matches!(
            store
                .put_stream(&bad_ref, size as u64, &mut corrupted)
                .await,
            Err(BlobStoreError::Store { .. })
        ));
        assert!(!store.has_blob(&bad_ref).await.unwrap());
        assert!(matches!(store.read_blob_range(&bad_ref, 0, 16).await,
            Err(BlobStoreError::NotFound { blob_ref }) if blob_ref == bad_ref));
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_event_appends_record_roots_and_reject_missing_blobs() {
    let store = live_store("graph", 1024).await;
    let session_id = SessionId::new("session-graph");
    store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create session");
    let first = store
        .put_bytes(b"first referenced payload".to_vec())
        .await
        .expect("put first");
    let second = store
        .put_bytes(b"second referenced payload".to_vec())
        .await
        .expect("put second");

    // seq 1 names `first`; seq 2 names both, nested and inside an array;
    // seq 3 names `second` again and mentions `first` only inside prose.
    store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![
                ref_event(10, serde_json::json!({ "content_ref": first.as_str() })),
                ref_event(
                    11,
                    serde_json::json!({
                        "nested": { "content_ref": first.as_str() },
                        "items": [{ "ref": second.as_str() }]
                    }),
                ),
                ref_event(
                    12,
                    serde_json::json!({
                        "content_ref": second.as_str(),
                        "preview": format!("mentions {first} in prose")
                    }),
                ),
            ],
        })
        .await
        .expect("append events");

    assert_eq!(
        events_embedding(&store, &session_id, &first).await,
        vec![1, 2],
        "first is embedded by seq 1 and 2, not by the prose mention in seq 3"
    );
    assert_eq!(
        events_embedding(&store, &session_id, &second).await,
        vec![2, 3],
        "second is embedded by seq 2 and 3"
    );

    // A later append retains the same session root without duplication.
    store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(3),
            }),
            events: vec![ref_event(
                13,
                serde_json::json!({ "content_ref": first.as_str() }),
            )],
        })
        .await
        .expect("append widening event");
    assert_eq!(
        events_embedding(&store, &session_id, &first).await,
        vec![1, 2, 4]
    );

    assert_eq!(
        session_root_count(&store, &session_id).await,
        2,
        "one root per session and digest"
    );

    // A ref the catalog does not hold fails the whole append and writes
    // nothing: the head stays where it was.
    let missing = BlobRef::from_bytes(b"never stored");
    let error = store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(4),
            }),
            events: vec![
                ref_event(14, serde_json::json!({ "content_ref": second.as_str() })),
                ref_event(15, serde_json::json!({ "content_ref": missing.as_str() })),
            ],
        })
        .await
        .expect_err("dangling ref must fail the append");
    match error {
        SessionStoreError::MissingBlobs {
            session_id: failed,
            blob_refs,
        } => {
            assert_eq!(failed, session_id);
            assert_eq!(blob_refs, vec![missing]);
        }
        other => panic!("expected MissingBlobs, got {other:?}"),
    }
    assert_eq!(
        store
            .head(&session_id)
            .await
            .expect("head")
            .map(|head| head.seq.as_u64()),
        Some(4),
        "a rejected append leaves no events behind"
    );
    assert_eq!(
        events_embedding(&store, &session_id, &second).await,
        vec![2, 3]
    );

    // Edges are still the writer's job.
    let parent = store
        .put_bytes(b"parent manifest".to_vec())
        .await
        .expect("put parent");
    let child = store
        .put_bytes(b"child payload".to_vec())
        .await
        .expect("put child");
    store
        .record_blob_edges(vec![BlobEdge::contains(parent.clone(), child.clone())])
        .await
        .expect("record edge");
    let edge_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM cas_blob_edges
        WHERE universe_id = $1
          AND parent_digest = $2
          AND child_digest = $3
          AND edge_kind = 'contains'
        "#,
    )
    .bind(store.config().universe_id)
    .bind(digest(&parent))
    .bind(digest(&child))
    .fetch_one(store.pool())
    .await
    .expect("count edge rows");
    assert_eq!(edge_count, 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_put_of_existing_content_touches_without_rewriting() {
    let store = live_store("touch", 8).await;
    let inline_ref = store
        .put_bytes(b"tiny".to_vec())
        .await
        .expect("put inline blob");
    let object_ref = store
        .put_bytes(b"large object payload".to_vec())
        .await
        .expect("put object blob");
    for blob_ref in [&inline_ref, &object_ref] {
        let (created_at_ms, touched_at_ms) = store
            .blob_timestamps(blob_ref)
            .await
            .expect("timestamps")
            .expect("blob exists");
        assert!(created_at_ms > 0);
        assert_eq!(created_at_ms, touched_at_ms, "a first put stamps both");
    }
    let object_etag_before = blob_object_etag(&store, &object_ref).await;

    // Age both rows, then put the same content again: only the touch
    // moves, creation time stays, and the object row keeps its upload.
    age_all_blobs(&store, 1_000).await;
    assert_eq!(
        store
            .put_bytes(b"tiny".to_vec())
            .await
            .expect("re-put inline"),
        inline_ref
    );
    assert_eq!(
        store
            .put_bytes(b"large object payload".to_vec())
            .await
            .expect("re-put object"),
        object_ref
    );
    for blob_ref in [&inline_ref, &object_ref] {
        let (created_at_ms, touched_at_ms) = store
            .blob_timestamps(blob_ref)
            .await
            .expect("timestamps")
            .expect("blob exists");
        assert_eq!(created_at_ms, 1_000, "creation time is never rewritten");
        assert!(touched_at_ms > 1_000, "a repeated put touches the row");
    }
    assert_eq!(
        blob_object_etag(&store, &object_ref).await,
        object_etag_before,
        "existing objects are not re-uploaded"
    );
    assert_eq!(
        store.read_bytes(&object_ref).await.expect("read object"),
        b"large object payload".to_vec()
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_sweep_frees_only_unreachable_blobs_after_grace() {
    let store = live_store("sweep", 8).await;
    let universe_id = store.config().universe_id;
    let pinned = harness_blob_refs();
    ensure_harness_blobs(&store).await.expect("engine blobs");

    // Holders of every kind. Payloads exceed the 8-byte inline threshold so
    // the object phase is exercised too.
    let put = |content: &'static str| put_blob(&store, content);
    let only_doomed = put("referenced only by the doomed session").await;
    let shared = put("referenced by the doomed and the surviving session").await;
    let checkpoint_state = put("checkpoint state of the surviving session").await;
    let snapshot_manifest = put("vfs snapshot manifest").await;
    let workspace_head = put("vfs workspace head manifest").await;
    let workspace_base = put("vfs workspace base manifest").await;
    let bot_document = put("bot event document").await;
    let bot_prompt = put("bot event prompt rendering").await;
    let bot_media = put("bot event media attachment").await;
    let parent = put("nested parent that embeds a child ref").await;
    let child = put("nested child referenced by an edge only").await;
    let unreferenced_object = put("unreferenced object-backed payload").await;
    let unreferenced_inline = put("tiny").await;

    let doomed = SessionId::new("doomed");
    let survivor = SessionId::new("survivor");
    for (session_id, refs) in [
        (&doomed, vec![&only_doomed, &shared]),
        (&survivor, vec![&shared]),
    ] {
        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let mut events = refs
            .into_iter()
            .enumerate()
            .map(|(index, blob_ref)| {
                ref_event(
                    10 + index as u64,
                    serde_json::json!({ "content_ref": blob_ref.as_str() }),
                )
            })
            .collect::<Vec<_>>();
        events.push(lifecycle_event(20, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND));
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events,
            })
            .await
            .expect("append session events");
    }
    assert!(
        store
            .advance_checkpoint(AdvanceSessionCheckpoint {
                checkpoint: SessionCheckpoint {
                    session_id: survivor.clone(),
                    through_seq: EventSeq::new(1),
                    format_version: 1,
                    state_ref: checkpoint_state.clone(),
                    lineage_source_session_id: None,
                    lineage_source_seq: None,
                    byte_len: 1,
                    created_at_ms: 20,
                },
            })
            .await
            .expect("advance checkpoint")
    );
    sqlx::query(
        r#"
        INSERT INTO vfs_snapshots (universe_id, digest, source_json, created_at_ms)
        VALUES ($1, $2, '{"kind":"test"}', 1)
        "#,
    )
    .bind(universe_id)
    .bind(digest(&snapshot_manifest))
    .execute(store.pool())
    .await
    .expect("insert vfs snapshot");
    sqlx::query(
        r#"
        INSERT INTO vfs_workspaces (
            universe_id, workspace_id, base_snapshot_digest, head_snapshot_digest,
            head_files, head_bytes, revision, created_at_ms, updated_at_ms
        )
        VALUES ($1, 'ws-sweep', $2, $3, 0, 0, 0, 1, 1)
        "#,
    )
    .bind(universe_id)
    .bind(digest(&workspace_base))
    .bind(digest(&workspace_head))
    .execute(store.pool())
    .await
    .expect("insert vfs workspace");
    sqlx::query(
        r#"
        INSERT INTO bots (universe_id, bot_id, revision, document_json, created_at_ms, updated_at_ms)
        VALUES ($1, 'sweep-bot', 1, '{}', 1, 1)
        "#,
    )
    .bind(universe_id)
    .execute(store.pool())
    .await
    .expect("insert bot");
    sqlx::query(
        r#"
        INSERT INTO bot_events (
            universe_id, bot_id, event_id, seq, kind, summary, occurred_at_ms, received_at_ms,
            document_ref, prompt_ref, media_json
        )
        VALUES ($1, 'sweep-bot', 'evt-1', 1, 'test.event', 'holds blobs', 1, 1, $2, $3, $4)
        "#,
    )
    .bind(universe_id)
    .bind(bot_document.as_str())
    .bind(bot_prompt.as_str())
    .bind(serde_json::json!([{ "blobRef": bot_media.as_str(), "kind": "image", "mime": "image/png" }]))
    .execute(store.pool())
    .await
    .expect("insert bot event");
    // This fixture writes the event directly; production insertion records these
    // roots in the same transaction (covered by the bot-store live tests).
    sqlx::query("INSERT INTO cas_bot_event_roots (universe_id, bot_id, event_id, digest) SELECT $1, 'sweep-bot', 'evt-1', unnest($2::text[])")
        .bind(universe_id)
        .bind([&bot_document, &bot_prompt, &bot_media].map(digest))
        .execute(store.pool()).await.expect("fixture bot roots");
    store
        .record_blob_edges(vec![BlobEdge::contains(parent.clone(), child.clone())])
        .await
        .expect("record edge");

    // Everything is old; the cutoff is far in the future of the aged rows
    // but before "now", so a fresh put stays inside the grace.
    age_all_blobs(&store, 1_000).await;
    let fresh = put("fresh unreferenced upload inside the grace").await;
    let cutoff_ms = 2_000;

    // With the doomed session still present nothing it references is
    // collectable; only the truly unreferenced blobs and the parent are.
    let before_delete = candidate_refs(&store, cutoff_ms, &pinned).await;
    assert_eq!(
        before_delete,
        sorted([&parent, &unreferenced_object, &unreferenced_inline])
    );

    store
        .delete_closed_sessions_with_options(
            DeleteClosedSessions {
                session_id: doomed.clone(),
                cascade: true,
                due_at_or_before_ms: None,
            },
            true,
            false,
        )
        .await
        .expect("delete doomed session");
    assert_eq!(
        candidate_refs(&store, cutoff_ms, &pinned).await,
        before_delete
    );
    store
        .purge_deleted_session(doomed.as_str())
        .await
        .expect("purge doomed session");
    let candidates = store
        .list_sweep_candidates(cutoff_ms, &pinned, 1024)
        .await
        .expect("list candidates");
    let candidate_set = sorted(candidates.iter().map(|candidate| &candidate.blob_ref));
    assert_eq!(
        candidate_set,
        sorted([
            &only_doomed,
            &parent,
            &unreferenced_object,
            &unreferenced_inline
        ]),
        "shared, held, pinned, fresh, and edge-protected blobs are not candidates"
    );
    let object_keys = candidates
        .iter()
        .filter_map(|candidate| candidate.object_key.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        object_keys.len(),
        3,
        "three candidates exceed the inline threshold"
    );
    let inline_candidate = candidates
        .iter()
        .find(|candidate| candidate.blob_ref == unreferenced_inline)
        .expect("inline candidate");
    assert!(inline_candidate.object_key.is_none());
    assert_eq!(inline_candidate.byte_len, 4);

    let deleted = store
        .delete_dead_blobs(&candidate_set, cutoff_ms, &pinned)
        .await
        .expect("delete dead blobs");
    assert_eq!(
        sorted(deleted.iter().map(|candidate| &candidate.blob_ref)),
        candidate_set,
        "the guarded delete removes exactly the listed candidates"
    );
    let outcome = store.delete_blob_objects(&object_keys).await;
    assert_eq!(outcome.deleted, 3);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    for key in &object_keys {
        use object_store::ObjectStoreExt as _;
        let head = store
            .object_store()
            .expect("live object store")
            .head(&object_store::path::Path::from(key.as_str()))
            .await;
        assert!(
            matches!(head, Err(object_store::Error::NotFound { .. })),
            "object {key} must be gone, got {head:?}"
        );
    }
    for gone in [
        &only_doomed,
        &parent,
        &unreferenced_object,
        &unreferenced_inline,
    ] {
        assert!(
            !store.has_blob(gone).await.expect("has"),
            "{gone} must be collected"
        );
    }
    for kept in [
        &shared,
        &checkpoint_state,
        &snapshot_manifest,
        &workspace_head,
        &workspace_base,
        &bot_document,
        &bot_prompt,
        &bot_media,
        &child,
        &fresh,
    ]
    .into_iter()
    .chain(pinned.iter())
    {
        assert!(
            store.has_blob(kept).await.expect("has"),
            "{kept} must survive"
        );
    }

    // The deleted parent's edge cascaded, so the child drains next; then
    // the sweep is a no-op.
    assert_eq!(
        candidate_refs(&store, cutoff_ms, &pinned).await,
        vec![child.clone()]
    );
    let deleted = store
        .delete_dead_blobs(std::slice::from_ref(&child), cutoff_ms, &pinned)
        .await
        .expect("delete child");
    assert_eq!(deleted.len(), 1);
    assert!(candidate_refs(&store, cutoff_ms, &pinned).await.is_empty());
    assert!(
        store
            .delete_dead_blobs(&[shared.clone(), fresh.clone()], cutoff_ms, &pinned)
            .await
            .expect("guarded delete of live blobs")
            .is_empty(),
        "the delete statement re-checks liveness and age"
    );

    // Advancing the checkpoint releases the previous state blob.
    let next_state = put("next checkpoint state of the surviving session").await;
    assert!(
        store
            .advance_checkpoint(AdvanceSessionCheckpoint {
                checkpoint: SessionCheckpoint {
                    session_id: survivor.clone(),
                    through_seq: EventSeq::new(2),
                    format_version: 1,
                    state_ref: next_state.clone(),
                    lineage_source_session_id: None,
                    lineage_source_seq: None,
                    byte_len: 1,
                    created_at_ms: 30,
                },
            })
            .await
            .expect("advance checkpoint again")
    );
    assert_eq!(
        candidate_refs(&store, cutoff_ms, &pinned).await,
        vec![checkpoint_state.clone()],
        "the superseded state is collectable; the current one is fresh and held"
    );

    // Once the fresh upload ages past the grace it goes too.
    age_all_blobs(&store, 1_000).await;
    assert_eq!(
        candidate_refs(&store, cutoff_ms, &pinned).await,
        sorted([&checkpoint_state, &fresh])
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_sweep_follows_fork_trees_and_clone_roots() {
    let store = live_store("sweep-lineage", 8).await;
    let pinned = harness_blob_refs();
    let config_blob = store
        .put_bytes(b"session config blob shared by lineage".to_vec())
        .await
        .expect("put config");
    let fork_only = store
        .put_bytes(b"blob only the fork references".to_vec())
        .await
        .expect("put fork blob");

    let root = SessionId::new("lineage-root");
    store
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: root.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create root");
    store
        .append(AppendSessionEvents {
            session_id: root.clone(),
            expected_head: None,
            events: vec![
                ref_event(
                    10,
                    serde_json::json!({ "content_ref": config_blob.as_str() }),
                ),
                lifecycle_event(11, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
            ],
        })
        .await
        .expect("append root events");
    // A history fork reads the root's events by reference and roots only
    // what it appends itself.
    let fork = SessionId::new("lineage-fork");
    store
        .create_forked_session(CreateForkedSession {
            source_session_id: root.clone(),
            session_id: fork.clone(),
            source_seq: EventSeq::new(1),
            created_at_ms: 20,
        })
        .await
        .expect("fork root");
    store
        .append(AppendSessionEvents {
            session_id: fork.clone(),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(1),
            }),
            events: vec![
                ref_event(21, serde_json::json!({ "content_ref": fork_only.as_str() })),
                lifecycle_event(22, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
            ],
        })
        .await
        .expect("append fork events");
    assert!(
        events_embedding(&store, &fork, &config_blob)
            .await
            .is_empty(),
        "inherited events are the source's rows"
    );
    assert_eq!(events_embedding(&store, &fork, &fork_only).await, vec![2]);
    // A config-only clone re-appends the config and so re-roots it.
    let clone = SessionId::new("lineage-clone");
    store
        .create_cloned_session(CreateClonedSession {
            source_session_id: root.clone(),
            session_id: clone.clone(),
            created_at_ms: 30,
            opening_events: vec![
                ref_event(
                    31,
                    serde_json::json!({ "content_ref": config_blob.as_str() }),
                ),
                lifecycle_event(32, CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
            ],
        })
        .await
        .expect("clone root");
    assert_eq!(
        events_embedding(&store, &clone, &config_blob).await,
        vec![1]
    );

    age_all_blobs(&store, 1_000).await;
    let cutoff_ms = 2_000;
    assert!(candidate_refs(&store, cutoff_ms, &pinned).await.is_empty());

    // Deleting the fork tree (root + fork) frees what only the fork used;
    // the clone keeps the config blob alive after its source is gone.
    let deleted = store
        .delete_closed_sessions(DeleteClosedSessions {
            session_id: root.clone(),
            cascade: true,
            due_at_or_before_ms: None,
        })
        .await
        .expect("delete fork tree");
    assert_eq!(deleted.deleted_session_ids.len(), 2);
    assert_eq!(
        candidate_refs(&store, cutoff_ms, &pinned).await,
        vec![fork_only.clone()]
    );
    store
        .delete_closed_sessions(DeleteClosedSessions {
            session_id: clone.clone(),
            cascade: false,
            due_at_or_before_ms: None,
        })
        .await
        .expect("delete clone");
    assert_eq!(
        candidate_refs(&store, cutoff_ms, &pinned).await,
        sorted([&config_blob, &fork_only])
    );
}

async fn put_blob(store: &PgStore, content: &str) -> BlobRef {
    store
        .put_bytes(content.as_bytes().to_vec())
        .await
        .expect("put blob")
}

/// Inspect event JSON independently of the store-maintained roots.
async fn events_embedding(store: &PgStore, session_id: &SessionId, blob_ref: &BlobRef) -> Vec<i64> {
    sqlx::query_scalar(
        r#"
        SELECT seq
        FROM session_events
        WHERE universe_id = $1
          AND session_id = $2
          AND jsonb_path_query_array(entry_json,
              '$.** ? (@.type() == "string" && @ like_regex "^sha256:[0-9a-f]{64}$")')
              @> jsonb_build_array($3::text)
        ORDER BY seq
        "#,
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .bind(blob_ref.as_str())
    .fetch_all(store.pool())
    .await
    .expect("load embedding events")
}

async fn candidate_refs(store: &PgStore, cutoff_ms: u64, pinned: &[BlobRef]) -> Vec<BlobRef> {
    sorted(
        store
            .list_sweep_candidates(cutoff_ms, pinned, 1024)
            .await
            .expect("list sweep candidates")
            .iter()
            .map(|candidate| &candidate.blob_ref),
    )
}

fn sorted<'a>(refs: impl IntoIterator<Item = &'a BlobRef>) -> Vec<BlobRef> {
    let mut refs = refs.into_iter().cloned().collect::<Vec<_>>();
    refs.sort();
    refs
}

async fn age_all_blobs(store: &PgStore, at_ms: i64) {
    sqlx::query(
        r#"
        UPDATE cas_blobs
        SET created_at_ms = LEAST(created_at_ms, $2), touched_at_ms = $2
        WHERE universe_id = $1
        "#,
    )
    .bind(store.config().universe_id)
    .bind(at_ms)
    .execute(store.pool())
    .await
    .expect("age blobs");
}

async fn blob_object_etag(store: &PgStore, blob_ref: &BlobRef) -> Option<String> {
    sqlx::query_scalar(
        r#"
        SELECT object_etag
        FROM cas_blobs
        WHERE universe_id = $1 AND digest = $2
        "#,
    )
    .bind(store.config().universe_id)
    .bind(digest(blob_ref))
    .fetch_one(store.pool())
    .await
    .expect("load object etag")
}

fn ref_event(at_ms: u64, payload: serde_json::Value) -> UncommittedStoredEvent {
    UncommittedStoredEvent {
        observed_at_ms: at_ms,
        joins: StoredJoins::default(),
        event: StoredEvent::new("lightspeed.test.references", 1, payload),
    }
}

fn lifecycle_event(at_ms: u64, kind: &'static str) -> UncommittedStoredEvent {
    UncommittedStoredEvent {
        observed_at_ms: at_ms,
        joins: StoredJoins::default(),
        event: StoredEvent::new(kind, 1, serde_json::Value::Object(Default::default())),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_vfs_catalog_tracks_workspace_heads() {
    let store = live_store("vfs-catalog", 1024).await;
    let snapshot_ref = store
        .put_bytes(b"snapshot manifest".to_vec())
        .await
        .expect("put snapshot manifest");
    let next_ref = store
        .put_bytes(b"next snapshot manifest".to_vec())
        .await
        .expect("put next snapshot manifest");

    let snapshot = VfsSnapshotRecord {
        snapshot_ref: snapshot_ref.clone(),
        source: VfsSnapshotSource::new("inline").with_subject("seed"),
        display_name: Some("Seed".to_string()),
        created_at_ms: 1,
    };
    store
        .record_snapshot(snapshot.clone())
        .await
        .expect("record snapshot");
    assert_eq!(
        store
            .read_snapshot(&snapshot_ref)
            .await
            .expect("read snapshot"),
        snapshot
    );

    let workspace_id = VfsWorkspaceId::new("workspace-1");
    let workspace = store
        .create_workspace(CreateVfsWorkspaceRecord {
            workspace_id: workspace_id.clone(),
            display_name: Some("Scratch".to_string()),
            base_snapshot_ref: Some(snapshot_ref.clone()),
            head_snapshot_ref: snapshot_ref.clone(),
            head_totals: VfsTotals { files: 1, bytes: 8 },
            created_at_ms: 2,
        })
        .await
        .expect("create workspace");
    assert_eq!(workspace.revision, 0);
    assert_eq!(workspace.display_name.as_deref(), Some("Scratch"));
    assert_eq!(workspace.head_totals, VfsTotals { files: 1, bytes: 8 });
    assert!(matches!(
        store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                display_name: None,
                base_snapshot_ref: None,
                head_snapshot_ref: snapshot_ref.clone(),
                head_totals: VfsTotals::default(),
                created_at_ms: 3,
            })
            .await,
        Err(VfsCatalogError::AlreadyExists { .. })
    ));

    let updated = store
        .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
            workspace_id: workspace_id.clone(),
            expected_revision: Some(0),
            display_name: Some("Scratch v2".to_string()),
            new_head_snapshot_ref: next_ref,
            new_head_totals: VfsTotals {
                files: 2,
                bytes: 16,
            },
            updated_at_ms: 4,
        })
        .await
        .expect("advance workspace head");
    assert_eq!(updated.revision, 1);
    assert_eq!(updated.display_name.as_deref(), Some("Scratch v2"));
    assert_eq!(
        updated.head_totals,
        VfsTotals {
            files: 2,
            bytes: 16
        }
    );
    assert!(matches!(
        store
            .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                workspace_id: workspace_id.clone(),
                expected_revision: Some(0),
                display_name: None,
                new_head_snapshot_ref: snapshot_ref.clone(),
                new_head_totals: VfsTotals::default(),
                updated_at_ms: 5,
            })
            .await,
        Err(VfsCatalogError::RevisionConflict {
            actual_revision: 1,
            ..
        })
    ));

    let listed = store.list_workspaces().await.expect("list workspaces");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].workspace_id, workspace_id);
    assert_eq!(listed[0].display_name.as_deref(), Some("Scratch v2"));
    assert_eq!(
        listed[0].head_totals,
        VfsTotals {
            files: 2,
            bytes: 16
        }
    );

    let deleted = store
        .delete_workspace(&workspace_id)
        .await
        .expect("delete workspace");
    assert_eq!(deleted.workspace_id, workspace_id);
    assert!(matches!(
        store.read_workspace(&deleted.workspace_id).await,
        Err(VfsCatalogError::NotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_profile_put_creates_and_replaces() {
    use api::{AgentProfileInput, ProfileDocument, ProfileId, ProfileInstructions};

    let store = live_store("profile-put", 1024).await;
    let profile_id = ProfileId::new("support");
    let input = |instructions: &str| AgentProfileInput {
        profile_id: profile_id.clone(),
        display_name: Some("Support".to_owned()),
        description: None,
        document: ProfileDocument {
            instructions: Some(ProfileInstructions::Text {
                text: instructions.to_owned(),
            }),
            ..ProfileDocument::default()
        },
    };

    // Absent profile: put creates at revision 1 even with an expected revision.
    let created = store
        .put_agent_profile(input("v1"), Some(7), 10)
        .await
        .expect("put creates absent profile");
    assert_eq!(created.revision, 1);
    assert_eq!(created.created_at_ms, 10);

    // Existing profile: put replaces the whole document and bumps the revision.
    let replaced = store
        .put_agent_profile(
            AgentProfileInput {
                display_name: Some("Support v2".to_owned()),
                ..input("v2")
            },
            None,
            20,
        )
        .await
        .expect("put replaces existing profile");
    assert_eq!(replaced.revision, 2);
    assert_eq!(replaced.created_at_ms, 10);
    assert_eq!(replaced.updated_at_ms, 20);
    assert_eq!(replaced.display_name.as_deref(), Some("Support v2"));
    assert!(matches!(
        replaced.document.instructions,
        Some(ProfileInstructions::Text { ref text }) if text == "v2"
    ));

    // Stale expected revision on an existing profile is a conflict.
    assert!(matches!(
        store.put_agent_profile(input("v3"), Some(1), 30).await,
        Err(ProfileError::RevisionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));

    // Matching expected revision replaces.
    let third = store
        .put_agent_profile(input("v3"), Some(2), 30)
        .await
        .expect("put with matching expected revision");
    assert_eq!(third.revision, 3);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_mcp_crud_and_universe_isolation() {
    let left = live_store("mcp-left", 1024).await;
    let right = live_store("mcp-right", 1024).await;
    let server_id = McpServerId::new("crm");

    // Absent record: put creates at revision 1 even with an expected revision.
    let created = left
        .put_server(put_mcp_server("crm", McpServerStatus::Active), Some(7))
        .await
        .expect("create MCP server");
    assert_eq!(created.server_id, server_id);
    assert_eq!(created.revision, 1);
    assert_eq!(created.created_at_ms, 10);
    assert_eq!(created.updated_at_ms, 10);

    assert_eq!(
        left.read_server(&server_id).await.expect("read MCP server"),
        created
    );
    assert!(matches!(
        right.read_server(&server_id).await,
        Err(McpRegistryError::NotFound { server_id }) if server_id.as_str() == "crm"
    ));

    let oauth = left
        .put_server(
            put_oauth_mcp_server("docs", McpServerStatus::NeedsAuthConfig),
            None,
        )
        .await
        .expect("create OAuth MCP server");
    assert!(matches!(
        oauth.auth_policy,
        McpServerAuthPolicy::RequiredOAuth { .. }
    ));

    let active = left
        .list_servers(ListMcpServers {
            status: Some(McpServerStatus::Active),
        })
        .await
        .expect("list active MCP servers");
    assert_eq!(active, vec![created.clone()]);

    left.put_secret(PutSecretRecord {
        secret_id: SecretId::new("authsec_mcp_crm"),
        secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
        value: SecretValue::new("crm-token"),
        created_at_ms: 10,
    })
    .await
    .expect("store MCP bearer secret");
    left.create_grant(CreateAuthGrantRecord {
        grant_id: AuthGrantId::new("authgrant_mcp_crm"),
        provider_id: "static".to_owned(),
        provider_kind: AuthProviderKind::StaticBearer,
        exposure: auth::AuthGrantExposure::Brokered,
        created_by: api::Attribution::Local,
        display_name: Some("CRM MCP".to_owned()),
        subject_hint: None,
        scopes: Vec::new(),
        audience: Some("https://crm2.example.com".to_owned()),
        access_token_secret: Some(SecretId::new("authsec_mcp_crm")),
        refresh_token_secret: None,
        oauth_client: None,
        expires_at_ms: None,
        status: AuthGrantStatus::Active,
        metadata: serde_json::Value::Object(Default::default()),
        created_at_ms: 10,
    })
    .await
    .expect("create MCP bearer grant");

    // Existing record: put replaces the whole document and bumps the revision.
    let mut replacement = put_mcp_server("crm", McpServerStatus::Disabled);
    replacement.server_url = "https://crm2.example.com/mcp".to_owned();
    replacement.description = None;
    replacement.approval = McpApprovalPolicy::Always;
    replacement.auth_policy = McpServerAuthPolicy::OptionalBearer;
    replacement.auth_grant_id = Some(AuthGrantId::new("authgrant_mcp_crm"));
    replacement.now_ms = created.updated_at_ms + 5;
    let replaced = left
        .put_server(replacement, Some(created.revision))
        .await
        .expect("replace MCP server");
    assert_eq!(replaced.revision, 2);
    assert_eq!(replaced.server_url, "https://crm2.example.com/mcp");
    assert_eq!(replaced.description, None);
    assert_eq!(replaced.approval, McpApprovalPolicy::Always);
    assert_eq!(
        replaced.auth_grant_id,
        Some(AuthGrantId::new("authgrant_mcp_crm"))
    );
    assert_eq!(replaced.status, McpServerStatus::Disabled);
    assert_eq!(replaced.display_name, created.display_name);
    assert_eq!(replaced.created_at_ms, created.created_at_ms);
    assert_eq!(replaced.updated_at_ms, created.updated_at_ms + 5);
    assert_eq!(
        left.read_server(&server_id).await.expect("re-read"),
        replaced
    );

    // Stale expected revision on an existing record is a conflict.
    assert!(matches!(
        left.put_server(put_mcp_server("crm", McpServerStatus::Active), Some(1))
            .await,
        Err(McpRegistryError::RevisionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));

    // The composite FK prevents a server from selecting another universe's
    // grant even when the grant id is otherwise valid.
    let mut cross_universe = put_mcp_server("foreign", McpServerStatus::Active);
    cross_universe.auth_policy = McpServerAuthPolicy::OptionalBearer;
    cross_universe.auth_grant_id = Some(AuthGrantId::new("authgrant_mcp_crm"));
    assert!(
        right.put_server(cross_universe, None).await.is_err(),
        "MCP grant bindings must remain in the server's universe"
    );

    // Universe isolation: the sibling store creates its own record at
    // revision 1 instead of replacing the left store's document.
    let sibling = right
        .put_server(put_mcp_server("crm", McpServerStatus::Active), None)
        .await
        .expect("sibling universe put");
    assert_eq!(sibling.revision, 1);
    assert_eq!(
        left.read_server(&server_id).await.expect("left unchanged"),
        replaced
    );

    let deleted = left
        .delete_server(&server_id)
        .await
        .expect("delete MCP server");
    assert_eq!(deleted, replaced);
    assert!(matches!(
        left.read_server(&server_id).await,
        Err(McpRegistryError::NotFound { server_id }) if server_id.as_str() == "crm"
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_auth_secrets_are_encrypted_and_universe_scoped() {
    let left = live_store("auth-left", 1024).await;
    let right = live_store("auth-right", 1024).await;
    let secret_id = SecretId::new("authsec_crm");
    let token = "live-test-bearer-token-12345";

    left.put_secret(PutSecretRecord {
        secret_id: secret_id.clone(),
        secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
        value: SecretValue::new(token),
        created_at_ms: 10,
    })
    .await
    .expect("put secret");

    assert!(matches!(
        left.put_secret(PutSecretRecord {
            secret_id: secret_id.clone(),
            secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
            value: SecretValue::new(token),
            created_at_ms: 11,
        })
        .await,
        Err(AuthRegistryError::SecretAlreadyExists { .. })
    ));

    let (meta, value) = left.read_secret(&secret_id).await.expect("read secret");
    assert_eq!(meta.secret_kind, SECRET_KIND_STATIC_BEARER);
    assert_eq!(value.expose(), token);

    let row = sqlx::query(
        "SELECT ciphertext FROM auth_secrets WHERE universe_id = $1 AND secret_id = $2",
    )
    .bind(left.config().universe_id)
    .bind(secret_id.as_str())
    .fetch_one(left.pool())
    .await
    .expect("read raw secret row");
    let ciphertext: Vec<u8> = row.try_get("ciphertext").expect("decode ciphertext");
    assert!(
        !ciphertext
            .windows(token.len())
            .any(|window| window == token.as_bytes()),
        "ciphertext must not contain the plaintext token"
    );

    assert!(matches!(
        right.read_secret(&secret_id).await,
        Err(AuthRegistryError::SecretNotFound { .. })
    ));

    let wrong_key_store = PgStore::with_object_store(
        left.pool().clone(),
        live_object_store(),
        left.config()
            .clone()
            .with_secrets_master_key(random_master_key()),
    );
    assert!(matches!(
        wrong_key_store.read_secret(&secret_id).await,
        Err(AuthRegistryError::Store { .. })
    ));

    left.delete_secret(&secret_id).await.expect("delete secret");
    assert!(matches!(
        left.read_secret(&secret_id).await,
        Err(AuthRegistryError::SecretNotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_auth_grants_crud_and_status_updates() {
    let store = live_store("auth-grants", 1024).await;
    let grant_id = AuthGrantId::new("authgrant_crm");

    let created = store
        .create_grant(CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            exposure: auth::AuthGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            display_name: Some("CRM token".to_owned()),
            subject_hint: None,
            scopes: vec!["contacts.read".to_owned()],
            audience: Some("https://crm.example.com/mcp".to_owned()),
            access_token_secret: Some(SecretId::new("authsec_crm")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 10,
        })
        .await
        .expect("create grant");
    assert_eq!(created.updated_at_ms, created.created_at_ms);

    assert!(matches!(
        store
            .create_grant(CreateAuthGrantRecord {
                created_at_ms: 11,
                ..CreateAuthGrantRecord {
                    grant_id: grant_id.clone(),
                    provider_id: "static".to_owned(),
                    provider_kind: AuthProviderKind::StaticBearer,
                    exposure: auth::AuthGrantExposure::Brokered,
                    created_by: api::Attribution::Local,
                    display_name: None,
                    subject_hint: None,
                    scopes: Vec::new(),
                    audience: None,
                    access_token_secret: None,
                    refresh_token_secret: None,
                    oauth_client: None,
                    expires_at_ms: None,
                    status: AuthGrantStatus::Active,
                    metadata: serde_json::Value::Object(Default::default()),
                    created_at_ms: 11,
                }
            })
            .await,
        Err(AuthRegistryError::GrantAlreadyExists { .. })
    ));

    assert_eq!(
        store.read_grant(&grant_id).await.expect("read grant"),
        created
    );
    assert_eq!(
        store
            .list_grants(ListAuthGrants {
                status: Some(AuthGrantStatus::Active),
            })
            .await
            .expect("list active grants"),
        vec![created.clone()]
    );

    let revoked = store
        .update_grant_status(&grant_id, AuthGrantStatus::Revoked, 20)
        .await
        .expect("revoke grant");
    assert_eq!(revoked.status, AuthGrantStatus::Revoked);
    assert_eq!(revoked.updated_at_ms, 20);
    assert!(
        store
            .list_grants(ListAuthGrants {
                status: Some(AuthGrantStatus::Active),
            })
            .await
            .expect("list active grants after revoke")
            .is_empty()
    );

    let deleted = store.delete_grant(&grant_id).await.expect("delete grant");
    assert_eq!(deleted.grant_id, grant_id);
    assert!(matches!(
        store.read_grant(&grant_id).await,
        Err(AuthRegistryError::GrantNotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_oauth_clients_crud() {
    let store = live_store("oauth-clients", 1024).await;
    let client_id = OAuthClientId::new("crm");

    let created = store
        .create_oauth_client(create_oauth_client_record(&client_id))
        .await
        .expect("create oauth client");
    assert_eq!(created.updated_at_ms, created.created_at_ms);
    assert_eq!(
        created.token_endpoint_auth_method,
        TokenEndpointAuthMethod::None
    );

    assert!(matches!(
        store
            .create_oauth_client(create_oauth_client_record(&client_id))
            .await,
        Err(AuthRegistryError::ClientAlreadyExists { .. })
    ));

    assert_eq!(
        store
            .read_oauth_client(&client_id)
            .await
            .expect("read oauth client"),
        created
    );
    assert_eq!(
        store
            .list_oauth_clients()
            .await
            .expect("list oauth clients"),
        vec![created.clone()]
    );

    let deleted = store
        .delete_oauth_client(&client_id)
        .await
        .expect("delete oauth client");
    assert_eq!(deleted.client_id, client_id);
    assert!(matches!(
        store.read_oauth_client(&client_id).await,
        Err(AuthRegistryError::ClientNotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_auth_flows_are_one_time_use() {
    let store = live_store("auth-flows", 1024).await;
    let flow_id = AuthFlowId::new("authflow_live");
    let state = "state-live-1";

    let created = store
        .create_flow(CreateAuthFlowRecord {
            flow_id: flow_id.clone(),
            client_id: OAuthClientId::new("crm"),
            provider_id: "crm".to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            grant_exposure: auth::AuthGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            state_hash: state_hash(state),
            pkce_verifier_secret: SecretId::new("authsec_pkce_live"),
            redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
            scopes: vec!["contacts.read".to_owned()],
            audience: Some("https://crm.example.com/mcp".to_owned()),
            expected_issuer: Some("https://as.example.com".to_owned()),
            require_issuer: true,
            expires_at_ms: 10_000,
            created_at_ms: 10,
        })
        .await
        .expect("create flow");
    assert!(created.consumed_at_ms.is_none());

    let by_state = store
        .read_flow_by_state_hash(&state_hash(state))
        .await
        .expect("lookup by state hash")
        .expect("flow found");
    assert_eq!(by_state.flow_id, flow_id);
    assert!(
        store
            .read_flow_by_state_hash(&state_hash("forged"))
            .await
            .expect("lookup forged state")
            .is_none()
    );

    let consumed = store.consume_flow(&flow_id, 100).await.expect("consume");
    assert_eq!(consumed.consumed_at_ms, Some(100));
    assert!(matches!(
        store.consume_flow(&flow_id, 101).await,
        Err(AuthRegistryError::FlowAlreadyConsumed { .. })
    ));

    let finished = store
        .finish_flow(
            &flow_id,
            FinishAuthFlow {
                grant_id: Some(AuthGrantId::new("authgrant_flow_live")),
                error: None,
                completed_at_ms: 150,
            },
        )
        .await
        .expect("finish flow");
    assert_eq!(
        finished.grant_id,
        Some(AuthGrantId::new("authgrant_flow_live"))
    );
    assert!(matches!(
        store
            .finish_flow(
                &flow_id,
                FinishAuthFlow {
                    grant_id: None,
                    error: Some("late".to_owned()),
                    completed_at_ms: 160,
                }
            )
            .await,
        Err(AuthRegistryError::FlowAlreadyCompleted { .. })
    ));

    // A separate expired flow cannot be consumed.
    let expired_id = AuthFlowId::new("authflow_live_expired");
    store
        .create_flow(CreateAuthFlowRecord {
            flow_id: expired_id.clone(),
            client_id: OAuthClientId::new("crm"),
            provider_id: "crm".to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            grant_exposure: auth::AuthGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            state_hash: state_hash("state-live-2"),
            pkce_verifier_secret: SecretId::new("authsec_pkce_live2"),
            redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
            scopes: Vec::new(),
            audience: Some("https://crm.example.com/mcp".to_owned()),
            expected_issuer: None,
            require_issuer: false,
            expires_at_ms: 50,
            created_at_ms: 10,
        })
        .await
        .expect("create expired flow");
    assert!(matches!(
        store.consume_flow(&expired_id, 1_000).await,
        Err(AuthRegistryError::FlowExpired { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_grant_refresh_updates_token_refs_and_lock_serializes() {
    let store = Arc::new(live_store("grant-refresh", 1024).await);
    let grant_id = AuthGrantId::new("authgrant_refresh_live");
    store
        .create_grant(CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: "crm".to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            exposure: auth::AuthGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: Some("https://crm.example.com/mcp".to_owned()),
            access_token_secret: Some(SecretId::new("authsec_old_access")),
            refresh_token_secret: Some(SecretId::new("authsec_old_refresh")),
            oauth_client: Some(OAuthClientId::new("crm")),
            expires_at_ms: Some(1_000),
            status: AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 10,
        })
        .await
        .expect("create oauth grant");

    let refreshed = store
        .record_grant_refresh(
            &grant_id,
            AuthGrantTokenRefresh {
                access_token_secret: SecretId::new("authsec_new_access"),
                refresh_token_secret: None,
                expires_at_ms: Some(5_000),
                updated_at_ms: 2_000,
            },
        )
        .await
        .expect("record refresh without rotation");
    assert_eq!(
        refreshed.access_token_secret,
        Some(SecretId::new("authsec_new_access"))
    );
    // No rotation: the refresh token reference is preserved.
    assert_eq!(
        refreshed.refresh_token_secret,
        Some(SecretId::new("authsec_old_refresh"))
    );
    assert_eq!(refreshed.expires_at_ms, Some(5_000));

    let rotated = store
        .record_grant_refresh(
            &grant_id,
            AuthGrantTokenRefresh {
                access_token_secret: SecretId::new("authsec_newer_access"),
                refresh_token_secret: Some(SecretId::new("authsec_new_refresh")),
                expires_at_ms: Some(9_000),
                updated_at_ms: 3_000,
            },
        )
        .await
        .expect("record refresh with rotation");
    assert_eq!(
        rotated.refresh_token_secret,
        Some(SecretId::new("authsec_new_refresh"))
    );

    // The advisory lock serializes concurrent holders for the same grant.
    let guard = store.lock_grant(&grant_id).await.expect("first lock");
    let contender_store = store.clone();
    let contender_grant = grant_id.clone();
    let contender = tokio::spawn(async move {
        contender_store
            .lock_grant(&contender_grant)
            .await
            .map(|_| ())
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!contender.is_finished(), "advisory lock must block");
    drop(guard);
    contender
        .await
        .expect("join contender")
        .expect("second lock after release");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_auth_providers_crud_and_credential_fk() {
    use auth::{
        AuthProviderConfig, AuthProviderId, AuthProviderStatus, AuthProviderStore,
        CreateAuthProviderRecord, GitHubAppConfig, SECRET_KIND_GITHUB_APP_PRIVATE_KEY,
    };

    let store = live_store("auth-providers", 1024).await;
    let provider_id = AuthProviderId::new("lightspeed-github");
    let key_secret = SecretId::new("authsec_github_key_live");
    store
        .put_secret(PutSecretRecord {
            secret_id: key_secret.clone(),
            secret_kind: SECRET_KIND_GITHUB_APP_PRIVATE_KEY.to_owned(),
            value: SecretValue::new(
                "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----",
            ),
            created_at_ms: 10,
        })
        .await
        .expect("put app key secret");

    let created = store
        .create_auth_provider(CreateAuthProviderRecord {
            provider_id: provider_id.clone(),
            display_name: Some("Lightspeed GitHub App".to_owned()),
            config: AuthProviderConfig::GitHubApp(GitHubAppConfig {
                app_id: "12345".to_owned(),
                api_base_url: "https://api.github.com".to_owned(),
            }),
            credential_secret: Some(key_secret.clone()),
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        })
        .await
        .expect("create provider");
    assert_eq!(created.provider_kind, AuthProviderKind::GitHubApp);

    assert!(matches!(
        store
            .create_auth_provider(CreateAuthProviderRecord {
                provider_id: provider_id.clone(),
                display_name: None,
                config: AuthProviderConfig::GitHubApp(GitHubAppConfig {
                    app_id: "12345".to_owned(),
                    api_base_url: "https://api.github.com".to_owned(),
                }),
                credential_secret: Some(key_secret.clone()),
                status: AuthProviderStatus::Active,
                created_at_ms: 11,
            })
            .await,
        Err(AuthRegistryError::ProviderAlreadyExists { .. })
    ));

    assert_eq!(
        store
            .read_auth_provider(&provider_id)
            .await
            .expect("read provider"),
        created
    );
    assert_eq!(
        store.list_auth_providers().await.expect("list providers"),
        vec![created.clone()]
    );

    // Referential integrity: the credential secret cannot be deleted while
    // the provider references it.
    assert!(matches!(
        store.delete_secret(&key_secret).await,
        Err(AuthRegistryError::Store { .. })
    ));

    let deleted = store
        .delete_auth_provider(&provider_id)
        .await
        .expect("delete provider");
    assert_eq!(deleted.provider_id, provider_id);
    // After the provider is gone the secret is deletable.
    store
        .delete_secret(&key_secret)
        .await
        .expect("delete app key secret after provider");
    assert!(matches!(
        store.read_auth_provider(&provider_id).await,
        Err(AuthRegistryError::ProviderNotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_grant_metadata_round_trips() {
    let store = live_store("grant-metadata", 1024).await;
    let grant_id = AuthGrantId::new("authgrant_install_live");

    let created = store
        .create_grant(CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: "lightspeed-github".to_owned(),
            provider_kind: AuthProviderKind::GitHubApp,
            exposure: auth::AuthGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: Some("acme".to_owned()),
            scopes: Vec::new(),
            audience: Some("https://api.github.com".to_owned()),
            access_token_secret: None,
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
            metadata: serde_json::json!({
                "installation_id": 678,
                "account_login": "acme",
                "permissions": {"contents": "read"},
            }),
            created_at_ms: 10,
        })
        .await
        .expect("create installation grant");

    assert_eq!(created.metadata["installation_id"], 678);
    let read = store.read_grant(&grant_id).await.expect("read grant");
    assert_eq!(read.metadata, created.metadata);
}

fn create_oauth_client_record(client_id: &OAuthClientId) -> CreateOAuthClientRecord {
    CreateOAuthClientRecord {
        client_id: client_id.clone(),
        provider_id: "crm".to_owned(),
        provider_kind: AuthProviderKind::McpOAuth,
        display_name: Some("CRM".to_owned()),
        authorization_endpoint: "https://as.example.com/authorize".to_owned(),
        token_endpoint: "https://as.example.com/token".to_owned(),
        remote_client_id: "client-live-1".to_owned(),
        client_secret: None,
        token_endpoint_auth_method: TokenEndpointAuthMethod::None,
        scopes_default: vec!["contacts.read".to_owned()],
        audience: Some("https://crm.example.com/mcp".to_owned()),
        authorization_server_issuer: Some("https://as.example.com".to_owned()),
        authorization_response_iss_parameter_supported: true,
        authorization_server_scopes_supported: vec!["contacts.read".to_owned()],
        created_at_ms: 10,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn pg_live_environment_credentials_round_trip() {
    use environments::{
        EnvironmentCredentialSource, EnvironmentCredentialStore, ListEnvironmentCredentials,
        PutEnvironmentCredential,
    };

    let store = live_store("environment-credentials", 1024).await;
    let provider_id = bound_test_provider(&store, "cred-provider").await;
    let environment_id = EnvironmentId::new("cred-env");
    store
        .create_environment(create_environment(
            &provider_id,
            "cred-request",
            "cred-env",
            3,
        ))
        .await
        .expect("create environment");

    // Subscription credentials are ordinary static_bearer grants carrying
    // metadata, so exercise two stored-secret grants.
    for (grant_id, kind, provider) in [
        ("authgrant_codex", AuthProviderKind::StaticBearer, "openai"),
        (
            "authgrant_claude",
            AuthProviderKind::StaticBearer,
            "anthropic",
        ),
    ] {
        store
            .create_grant(CreateAuthGrantRecord {
                grant_id: AuthGrantId::new(grant_id),
                provider_id: provider.to_owned(),
                provider_kind: kind,
                exposure: auth::AuthGrantExposure::Brokered,
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
                metadata: serde_json::json!({ "subscription": "codex" }),
                created_at_ms: 4,
            })
            .await
            .expect("create subscription grant");
    }

    let bound = store
        .bind_credential(PutEnvironmentCredential {
            environment_id: environment_id.clone(),
            env_name: "CODEX_AUTH_JSON".to_owned(),
            source: EnvironmentCredentialSource::AuthGrant {
                grant_id: AuthGrantId::new("authgrant_codex"),
            },
            created_at_ms: 5,
        })
        .await
        .expect("bind codex credential");
    assert_eq!(
        bound.source,
        EnvironmentCredentialSource::AuthGrant {
            grant_id: AuthGrantId::new("authgrant_codex"),
        }
    );
    store
        .bind_credential(PutEnvironmentCredential {
            environment_id: environment_id.clone(),
            env_name: "CLAUDE_CODE_OAUTH_TOKEN".to_owned(),
            source: EnvironmentCredentialSource::AuthGrant {
                grant_id: AuthGrantId::new("authgrant_claude"),
            },
            created_at_ms: 6,
        })
        .await
        .expect("bind plain credential");

    // Rebinding the same name replaces the source.
    let replaced = store
        .bind_credential(PutEnvironmentCredential {
            environment_id: environment_id.clone(),
            env_name: "CODEX_AUTH_JSON".to_owned(),
            source: EnvironmentCredentialSource::AuthGrant {
                grant_id: AuthGrantId::new("authgrant_claude"),
            },
            created_at_ms: 7,
        })
        .await
        .expect("rebind to another grant");
    assert_eq!(
        replaced.source,
        EnvironmentCredentialSource::AuthGrant {
            grant_id: AuthGrantId::new("authgrant_claude"),
        }
    );

    let listed = store
        .list_credentials(ListEnvironmentCredentials {
            environment_id: environment_id.clone(),
        })
        .await
        .expect("list credentials");
    assert_eq!(
        listed
            .iter()
            .map(|c| c.env_name.as_str())
            .collect::<Vec<_>>(),
        vec!["CLAUDE_CODE_OAUTH_TOKEN", "CODEX_AUTH_JSON"]
    );

    store
        .unbind_credential(&environment_id, "CODEX_AUTH_JSON")
        .await
        .expect("unbind");
    assert_eq!(
        store
            .list_credentials(ListEnvironmentCredentials { environment_id })
            .await
            .expect("list after unbind")
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn pg_live_universe_delete_removes_registered_environments_and_credential_bindings() {
    use environments::{
        CreateEnvironmentRegistrationKey, CreateRegisteredEnvironment, EnvironmentCredentialSource,
        EnvironmentCredentialStore, EnvironmentRegistrationKeyId, EnvironmentRegistrationKeyStore,
        PutEnvironmentCredential, RegisteredIdentityMode, RegistrationKeyPolicy,
        mint_registration_key,
    };

    let store = live_store("environment-universe-delete", 1024).await;
    let universe_id = store.config().universe_id;
    let minted = mint_registration_key(
        EnvironmentRegistrationKeyId::new("fleet"),
        RegistrationKeyPolicy {
            display_name: "Fleet".to_owned(),
            identity_mode: RegisteredIdentityMode::Persistent,
            max_active_environments: None,
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: None,
            expires_at_ms: None,
        },
        1,
    )
    .expect("mint key");
    store
        .create_registration_key(CreateEnvironmentRegistrationKey {
            secret_hash: minted.secret.secret_hash.clone(),
            record: minted.record.clone(),
        })
        .await
        .expect("create key");
    let environment_id = EnvironmentId::new("pod-0");
    store
        .create_registered_environment(CreateRegisteredEnvironment {
            registration_key_id: minted.record.registration_key_id.clone(),
            secret_hash: minted.secret.secret_hash.clone(),
            environment_id: environment_id.clone(),
            daemon_public_key: test_daemon_public_key(),
            display_name: None,
            metadata: Default::default(),
            daemon_build: test_build("1.0.0"),
            created_at_ms: 2,
        })
        .await
        .expect("admit registered environment");
    let secret_id = SecretId::new("authsec_pod_token");
    store
        .put_secret(PutSecretRecord {
            secret_id: secret_id.clone(),
            secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
            value: SecretValue::new("token"),
            created_at_ms: 3,
        })
        .await
        .expect("put secret");
    store
        .bind_credential(PutEnvironmentCredential {
            environment_id: environment_id.clone(),
            env_name: "POD_TOKEN".to_owned(),
            source: EnvironmentCredentialSource::DirectSecret {
                secret_id: secret_id.clone(),
            },
            created_at_ms: 4,
        })
        .await
        .expect("bind credential");

    // Outside a universe delete the references still hold.
    assert!(matches!(
        store.delete_secret(&secret_id).await,
        Err(AuthRegistryError::Store { .. })
    ));
    assert_eq!(
        violated_constraint(
            sqlx::query(
                "DELETE FROM environment_registration_keys WHERE universe_id = $1 AND registration_key_id = 'fleet'",
            )
            .bind(universe_id)
            .execute(store.pool())
            .await
        )
        .as_deref(),
        Some("environments_registration_key_fk")
    );

    assert!(
        store_pg::delete_universe(store.pool(), universe_id)
            .await
            .expect("delete universe with registered environment and credential binding")
    );
    for table in [
        "environments",
        "environment_registration_keys",
        "environment_registration_key_secrets",
        "environment_credentials",
        "auth_secrets",
    ] {
        let remaining: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE universe_id = $1"
        )))
        .bind(universe_id)
        .fetch_one(store.pool())
        .await
        .expect("count remaining rows");
        assert_eq!(remaining, 0, "{table} cascades with the universe");
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_session_metadata_filters_by_containment_and_put_replaces() {
    use harness::storage::{ListSessions, SessionListPage};
    use std::collections::BTreeMap;

    let store = live_store("session-metadata", 64).await;
    let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());
    let harbor = BTreeMap::from([
        pair("source", "harbor"),
        pair("job", "nightly"),
        pair("trial", "1"),
    ]);
    for (name, metadata, created_at_ms) in [
        ("meta-harbor", harbor.clone(), 10),
        (
            "meta-bot",
            BTreeMap::from([pair("source", "bot"), pair("bot", "triage")]),
            20,
        ),
        ("meta-none", BTreeMap::new(), 30),
    ] {
        store
            .create_session(CreateSession {
                session_id: SessionId::new(name),
                display_name: None,
                metadata,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms,
            })
            .await
            .expect("create session");
    }
    let list = |metadata: BTreeMap<String, String>| ListSessions {
        limit: 10,
        metadata,
        ..Default::default()
    };
    let ids = |page: SessionListPage| {
        page.sessions
            .into_iter()
            .map(|record| record.session_id.as_str().to_owned())
            .collect::<Vec<_>>()
    };

    // The stored map comes back verbatim through load and list.
    let loaded = store
        .load_session(&SessionId::new("meta-harbor"))
        .await
        .expect("load")
        .expect("exists");
    assert_eq!(loaded.metadata, harbor);

    // One pair, two pairs (AND), a mismatched value, a missing key, no filter.
    for (filter, expected) in [
        (
            BTreeMap::from([pair("source", "harbor")]),
            vec!["meta-harbor"],
        ),
        (
            BTreeMap::from([pair("source", "harbor"), pair("job", "nightly")]),
            vec!["meta-harbor"],
        ),
        (
            BTreeMap::from([pair("source", "harbor"), pair("job", "weekly")]),
            vec![],
        ),
        (BTreeMap::from([pair("job", "")]), vec!["meta-harbor"]),
        (
            BTreeMap::from([pair("source", "")]),
            vec!["meta-bot", "meta-harbor"],
        ),
        (BTreeMap::from([pair("task", "")]), vec![]),
        (BTreeMap::from([pair("task", "x")]), vec![]),
        (
            BTreeMap::new(),
            vec!["meta-none", "meta-bot", "meta-harbor"],
        ),
    ] {
        let page = store
            .list_sessions(list(filter.clone()))
            .await
            .expect("list");
        assert_eq!(ids(page), expected, "filter {filter:?}");
    }

    // Put replaces the whole map and leaves updated_at_ms alone.
    let replaced = store
        .set_session_metadata(
            &SessionId::new("meta-harbor"),
            BTreeMap::from([pair("owner", "lukas")]),
        )
        .await
        .expect("set metadata");
    assert_eq!(replaced.metadata, BTreeMap::from([pair("owner", "lukas")]));
    assert_eq!(replaced.updated_at_ms, 10);
    assert!(
        ids(store
            .list_sessions(list(BTreeMap::from([pair("job", "nightly")])))
            .await
            .expect("list after put"))
        .is_empty()
    );
    assert_eq!(
        ids(store
            .list_sessions(list(BTreeMap::from([pair("owner", "lukas")])))
            .await
            .expect("list by new key")),
        vec!["meta-harbor"]
    );
    let cleared = store
        .set_session_metadata(&SessionId::new("meta-harbor"), BTreeMap::new())
        .await
        .expect("clear metadata");
    assert!(cleared.metadata.is_empty());
    assert!(matches!(
        store
            .set_session_metadata(&SessionId::new("meta-missing"), BTreeMap::new())
            .await,
        Err(SessionStoreError::SessionNotFound { .. })
    ));
}

async fn live_store(test_name: &str, inline_threshold_bytes: usize) -> PgStore {
    let database_url = env_or_dotenv_var("LIGHTSPEED_TEST_POSTGRES_URL").expect(
        "LIGHTSPEED_TEST_POSTGRES_URL must be set in env or root .env to run store-pg live tests; run ./dev.sh infra and source scripts/dev/env.sh",
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    migrate_once(&pool).await;

    let prefix = env_or_dotenv_var("LIGHTSPEED_OBJECT_STORE_PREFIX")
        .unwrap_or_else(|_| "lightspeed".to_string());
    let config = PgStoreConfig::new(Uuid::new_v4())
        .with_inline_threshold_bytes(inline_threshold_bytes)
        .with_object_prefix(format!("{}/tests/{}", prefix.trim_matches('/'), test_name))
        .with_secrets_master_key(random_master_key());
    let store = PgStore::with_object_store(pool, live_object_store(), config);
    store.ensure_universe().await.expect("ensure test universe");
    store
}

fn random_master_key() -> SecretsMasterKey {
    let mut bytes = [0u8; 32];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes)
        .expect("OS random source unavailable");
    SecretsMasterKey::from_bytes(bytes)
}

async fn migrate_once(pool: &PgPool) {
    MIGRATED
        .get_or_try_init(|| async {
            PgStore::migrate(pool)
                .await
                .map_err(|error| format!("apply store-pg migration: {error}"))
        })
        .await
        .expect("apply store-pg migration");
}

fn live_object_store() -> Arc<dyn ObjectStore> {
    let endpoint = env_or_dotenv_var("LIGHTSPEED_OBJECT_STORE_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:29000".to_string());
    let bucket = env_or_dotenv_var("LIGHTSPEED_OBJECT_STORE_BUCKET")
        .unwrap_or_else(|_| "lightspeed-dev".to_string());
    let region = env_or_dotenv_var("LIGHTSPEED_OBJECT_STORE_REGION")
        .unwrap_or_else(|_| "us-east-1".to_string());
    let access_key =
        env_or_dotenv_var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "minioadmin".to_string());
    let secret_key =
        env_or_dotenv_var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let force_path_style = env_or_dotenv_var("LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE")
        .unwrap_or_else(|_| "true".to_string())
        .parse::<bool>()
        .expect("LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE must be true or false");

    let store = AmazonS3Builder::new()
        .with_bucket_name(bucket)
        .with_region(region)
        .with_endpoint(endpoint.clone())
        .with_access_key_id(access_key)
        .with_secret_access_key(secret_key)
        .with_allow_http(endpoint.starts_with("http://"))
        .with_virtual_hosted_style_request(!force_path_style)
        .build()
        .expect("build MinIO/S3 object store");
    Arc::new(store)
}

fn open_event(at_ms: u64) -> UncommittedStoredEvent {
    UncommittedStoredEvent {
        observed_at_ms: at_ms,
        joins: StoredJoins::default(),
        event: StoredEvent::new(
            "lightspeed.test.lifecycle.closed",
            1,
            serde_json::Value::Object(Default::default()),
        ),
    }
}

fn put_mcp_server(server_id: &str, status: McpServerStatus) -> PutMcpServerRecord {
    PutMcpServerRecord {
        server_id: McpServerId::new(server_id),
        display_name: Some(format!("{server_id} MCP")),
        server_url: format!("https://{server_id}.example.com/mcp"),
        transport: RemoteMcpTransport::StreamableHttp,
        default_server_label: server_id.to_owned(),
        description: Some(format!("{server_id} remote MCP server")),
        allowed_tools: Some(vec!["lookup_customer".to_owned()]),
        execution: mcp::McpExecution::Provider,
        exposure: mcp::McpExposure::Inject,
        approval: McpApprovalPolicy::Never,
        defer_loading: Some(true),
        allow_private_network: false,
        auth_policy: McpServerAuthPolicy::None,
        auth_grant_id: None,
        status,
        now_ms: 10,
    }
}

fn put_oauth_mcp_server(server_id: &str, status: McpServerStatus) -> PutMcpServerRecord {
    let mut record = put_mcp_server(server_id, status);
    record.auth_policy = McpServerAuthPolicy::RequiredOAuth {
        resource: format!("https://{server_id}.example.com"),
        scopes_default: Vec::new(),
        protected_resource_metadata_url: Some(format!(
            "https://{server_id}.example.com/.well-known/oauth-protected-resource"
        )),
        authorization_server: Some("https://login.example.com".to_owned()),
    };
    record
}

struct BlobLayoutRow {
    storage_kind: String,
    has_inline_bytes: bool,
    object_key: Option<String>,
}

async fn blob_layout(store: &PgStore, blob_ref: &BlobRef) -> BlobLayoutRow {
    let row = sqlx::query(
        r#"
        SELECT storage_kind, inline_bytes IS NOT NULL AS has_inline_bytes, object_key
        FROM cas_blobs
        WHERE universe_id = $1 AND blob_ref = $2
        "#,
    )
    .bind(store.config().universe_id)
    .bind(blob_ref.as_str())
    .fetch_one(store.pool())
    .await
    .expect("load blob layout row");

    BlobLayoutRow {
        storage_kind: row.try_get("storage_kind").expect("storage_kind"),
        has_inline_bytes: row.try_get("has_inline_bytes").expect("has_inline_bytes"),
        object_key: row.try_get("object_key").expect("object_key"),
    }
}

fn digest(blob_ref: &BlobRef) -> &str {
    blob_ref
        .as_str()
        .strip_prefix("sha256:")
        .expect("sha256 blob ref")
}

fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(root_dotenv_path()).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() == name {
            return Some(unquote_dotenv_value(value.trim()));
        }
    }
    None
}

fn root_dotenv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .join(".env")
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_delayed_object_deletion_cannot_delete_a_reupload() {
    let store = live_store("sweep-reupload", 8).await;
    let bytes = b"object reuploaded after catalog deletion".to_vec();
    let blob_ref = store.put_bytes(bytes.clone()).await.expect("initial put");
    age_all_blobs(&store, 1_000).await;
    let removed = store
        .delete_dead_blobs(std::slice::from_ref(&blob_ref), 2_000, &[])
        .await
        .expect("delete old catalog row");
    let old_key = removed[0].object_key.clone().expect("object key");
    assert_eq!(
        store.put_bytes(bytes.clone()).await.expect("reupload"),
        blob_ref
    );
    let layout = blob_layout(&store, &blob_ref).await;
    assert_ne!(layout.object_key.as_deref(), Some(old_key.as_str()));
    let cleanup = store.delete_blob_objects(&[old_key]).await;
    assert!(cleanup.failures.is_empty());
    // A separate store has no cached bytes to mask object loss.
    let cold = PgStore::with_object_store(
        store.pool().clone(),
        live_object_store(),
        store.config().clone(),
    );
    assert_eq!(
        cold.read_bytes(&blob_ref)
            .await
            .expect("cold read after cleanup"),
        bytes
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_sweep_pages_advance_past_live_rows_and_wrap() {
    let store = live_store("sweep-pages", 1024).await;
    let live = put_blob(&store, "old pinned blob").await;
    let dead = put_blob(&store, "later unreachable blob").await;
    age_all_blobs(&store, 1_000).await;
    sqlx::query("UPDATE cas_blobs SET touched_at_ms = 1001 WHERE universe_id = $1 AND digest = $2")
        .bind(store.config().universe_id)
        .bind(digest(&dead))
        .execute(store.pool())
        .await
        .expect("order rows");
    let pinned = [live.clone()];
    let first = store
        .scan_sweep_candidates(2_000, &pinned, None, 1)
        .await
        .expect("first page");
    assert_eq!(first.scanned, 1);
    assert!(first.candidates.is_empty());
    let second = store
        .scan_sweep_candidates(2_000, &pinned, first.next_cursor.as_ref(), 1)
        .await
        .expect("second page");
    assert_eq!(second.scanned, 1);
    assert_eq!(second.candidates[0].blob_ref, dead);
    let end = store
        .scan_sweep_candidates(2_000, &pinned, second.next_cursor.as_ref(), 1)
        .await
        .expect("end page");
    assert_eq!(end.scanned, 0);
    assert!(end.next_cursor.is_none());
    let wrapped = store
        .scan_sweep_candidates(2_000, &[], end.next_cursor.as_ref(), 1)
        .await
        .expect("wrapped page");
    assert_eq!(
        wrapped.candidates[0].blob_ref, live,
        "released roots are revisited"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_admission_touch_refreshes_grace_and_rejects_missing_refs() {
    let store = live_store("admission-touch", 1024).await;
    let first = put_blob(&store, "first input").await;
    let second = put_blob(&store, "second input").await;
    age_all_blobs(&store, 1_000).await;
    store
        .touch_blob_refs(&[first.clone(), first.clone(), second.clone()])
        .await
        .expect("batched touch");
    assert!(
        store
            .delete_dead_blobs(&[first, second], 2_000, &[])
            .await
            .expect("sweep")
            .is_empty()
    );
    let missing = BlobRef::from_bytes(b"missing input");
    assert!(
        matches!(store.touch_blob_refs(std::slice::from_ref(&missing)).await,
        Err(harness::storage::BlobStoreError::NotFound { blob_ref }) if blob_ref == missing)
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_concurrent_deletion_rolls_back_session_attachment() {
    let store = live_store("attach-race", 1024).await;
    let blob_ref = put_blob(&store, "attachment racing a delete").await;
    let session_id = SessionId::new("attach-race");
    store
        .create_session(CreateSession {
            session_id: session_id.clone(),
            metadata: Default::default(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create session");
    let mut deleting = store.pool().begin().await.expect("begin deletion");
    sqlx::query("SELECT digest FROM cas_blobs WHERE universe_id = $1 AND digest = $2 FOR UPDATE")
        .bind(store.config().universe_id)
        .bind(digest(&blob_ref))
        .execute(&mut *deleting)
        .await
        .expect("lock blob");
    let appender = store.clone();
    let append_id = session_id.clone();
    let append_ref = blob_ref.clone();
    let append = tokio::spawn(async move {
        appender
            .append(AppendSessionEvents {
                session_id: append_id,
                expected_head: None,
                events: vec![ref_event(2, serde_json::json!({"content_ref": append_ref}))],
            })
            .await
    });
    // Wait until the real append has inserted its events and is trying to
    // acquire its blob lock. This makes the destructive interleaving explicit.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE '%WITH requested AS%')")
                .fetch_one(&mut *deleting).await.expect("inspect blocked append");
            if blocked { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            sqlx::query("SELECT pg_stat_clear_snapshot()").execute(&mut *deleting).await.expect("refresh activity snapshot");
        }
    }).await.expect("append must lock the blob before committing");
    sqlx::query("DELETE FROM cas_blobs WHERE universe_id = $1 AND digest = $2")
        .bind(store.config().universe_id)
        .bind(digest(&blob_ref))
        .execute(&mut *deleting)
        .await
        .expect("delete blob");
    deleting.commit().await.expect("commit deletion");
    assert!(
        matches!(append.await.expect("append task"), Err(SessionStoreError::MissingBlobs { blob_refs, .. }) if blob_refs == vec![blob_ref])
    );
    assert!(
        store.head(&session_id).await.expect("head").is_none(),
        "no dangling event commits"
    );
    assert_eq!(session_root_count(&store, &session_id).await, 0);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_sweep_leadership_is_exclusive_and_released_on_drop() {
    let store = live_store("sweep-leader", 1024).await;
    let mut leader = store_pg::CasSweepLeader::try_acquire(store.pool())
        .await
        .expect("acquire")
        .expect("leader");
    assert!(
        store_pg::CasSweepLeader::try_acquire(store.pool())
            .await
            .expect("competing acquire")
            .is_none()
    );
    leader.check().await.expect("leader connection");
    drop(leader);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(_next) = store_pg::CasSweepLeader::try_acquire(store.pool())
                .await
                .expect("successor acquire")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("leadership must be released on drop");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_profile_refs_are_borrowed_and_do_not_prevent_collection() {
    let store = live_store("profile-borrowed", 1024).await;
    let blob_ref = put_blob(&store, "borrowed profile instructions").await;
    let id = api::ProfileId::new("borrowed");
    store
        .put_agent_profile(
            api::AgentProfileInput {
                profile_id: id.clone(),
                display_name: None,
                description: None,
                document: api::ProfileDocument {
                    instructions: Some(api::ProfileInstructions::TextRef {
                        blob_ref: blob_ref.to_string(),
                    }),
                    ..Default::default()
                },
            },
            None,
            1,
        )
        .await
        .expect("create profile");
    age_all_blobs(&store, 1_000).await;
    assert_eq!(
        store
            .delete_dead_blobs(&[blob_ref], 2_000, &[])
            .await
            .expect("sweep")
            .len(),
        1
    );
    store
        .read_agent_profile(&id)
        .await
        .expect("profile still exists");
}

async fn session_root_count(store: &PgStore, session_id: &SessionId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM cas_session_roots WHERE universe_id = $1 AND session_id = $2",
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("count session roots")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_concurrent_holder_commit_blocks_a_stale_sweep() {
    let store = live_store("holder-wins", 1024).await;
    let blob_ref = put_blob(&store, "holder commits after sweep snapshot").await;
    let session_id = SessionId::new("holder-wins");
    store
        .create_session(CreateSession {
            session_id: session_id.clone(),
            metadata: Default::default(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .expect("create session");
    age_all_blobs(&store, 1_000).await;
    // Hold the same locks an append takes, then let a real sweep take its
    // snapshot before this holder commits. The FK must catch that stale read.
    let mut attaching = store.pool().begin().await.expect("attachment transaction");
    sqlx::query(
        "SELECT digest FROM cas_blobs WHERE universe_id = $1 AND digest = $2 FOR KEY SHARE",
    )
    .bind(store.config().universe_id)
    .bind(digest(&blob_ref))
    .execute(&mut *attaching)
    .await
    .expect("hold blob");
    sqlx::query(
        "INSERT INTO cas_session_roots (universe_id, session_id, digest) VALUES ($1, $2, $3)",
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .bind(digest(&blob_ref))
    .execute(&mut *attaching)
    .await
    .expect("insert holder");
    let sweeper = store.clone();
    let candidate = blob_ref.clone();
    let sweep =
        tokio::spawn(async move { sweeper.delete_dead_blobs(&[candidate], 2_000, &[]).await });
    tokio::time::timeout(std::time::Duration::from_millis(800), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE '%DELETE FROM cas_blobs AS b%')")
                .fetch_one(&mut *attaching).await.expect("inspect blocked sweep");
            if blocked { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            sqlx::query("SELECT pg_stat_clear_snapshot()").execute(&mut *attaching).await.expect("refresh activity snapshot");
        }
    }).await.expect("sweep must wait for attachment");
    attaching.commit().await.expect("commit holder");
    assert!(matches!(
        sweep.await.expect("sweep task"),
        Err(store_pg::CasSweepError::HolderConflict { .. })
    ));
    assert!(store.has_blob(&blob_ref).await.expect("held blob survives"));
    assert_eq!(session_root_count(&store, &session_id).await, 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres + MinIO"]
async fn pg_live_concurrent_puts_leave_one_readable_object() {
    use futures_util::TryStreamExt;
    let store = live_store("concurrent-puts", 8).await;
    let bytes = b"the same concurrent object upload".to_vec();
    let expected = BlobRef::from_bytes(&bytes);
    let results =
        futures_util::future::join_all((0..16).map(|_| store.put_bytes(bytes.clone()))).await;
    for result in results {
        assert_eq!(result.expect("concurrent put"), expected);
    }
    let prefix = object_store::path::Path::from(store_pg::universe_cas_object_prefix(
        &store.config().object_prefix,
        store.config().universe_id,
    ));
    let objects = live_object_store()
        .list(Some(&prefix))
        .try_collect::<Vec<_>>()
        .await
        .expect("list uploads");
    assert_eq!(
        objects.len(),
        1,
        "losing uploads must clean up their private keys"
    );
    let cold = PgStore::with_object_store(
        store.pool().clone(),
        live_object_store(),
        store.config().clone(),
    );
    assert_eq!(cold.read_bytes(&expected).await.expect("cold read"), bytes);
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

/// Register a provider with a unique id (providers are deployment-wide) and
/// bind it, enabled, to the store's universe.
async fn bound_test_provider(store: &PgStore, name: &str) -> EnvironmentProviderId {
    let provider_id = EnvironmentProviderId::new(format!("{name}-{}", Uuid::new_v4().simple()));
    store
        .put_provider(PutEnvironmentProvider {
            provider_id: provider_id.clone(),
            display_name: None,
            controller_endpoint: EnvironmentEndpoint::parse("ws://127.0.0.1:9100/control")
                .expect("endpoint"),
            data_route_base: None,
            credential: ProviderCredentialChange::Keep,
            updated_at_ms: 1,
        })
        .await
        .expect("register provider");
    store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id: store.config().universe_id,
            provider_id: provider_id.clone(),
            enabled: true,
            updated_at_ms: 2,
        })
        .await
        .expect("bind provider");
    provider_id
}

fn create_environment(
    provider_id: &EnvironmentProviderId,
    request_id: &str,
    environment_id: &str,
    created_at_ms: i64,
) -> CreateEnvironment {
    CreateEnvironment {
        request_id: EnvironmentProvisionRequestId::new(request_id),
        environment_id: EnvironmentId::new(environment_id),
        provider_id: provider_id.clone(),
        template_id: EnvironmentTemplateId::new("rust-v1"),
        idle: None,
        display_name: None,
        metadata: Default::default(),
        created_at_ms,
    }
}

fn test_build(version: &str) -> DaemonBuild {
    DaemonBuild {
        version: Some(version.to_owned()),
        git_sha: None,
        protocol_version: environment_protocol::ENVD_PROTOCOL_VERSION,
    }
}

/// A fresh lowercase-hex daemon public key; unique deployment-wide.
fn test_daemon_public_key() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// The constraint a refused statement violated.
fn violated_constraint<T>(result: Result<T, sqlx::Error>) -> Option<String> {
    result.err().and_then(|error| {
        error
            .as_database_error()
            .and_then(|database| database.constraint().map(str::to_owned))
    })
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_environment_provider_credentials_are_optional_and_sealed() {
    let store = live_store("environment-providers", 1024).await;
    let universe_id = store.config().universe_id;
    let provider_id = EnvironmentProviderId::new(format!("provider-{}", Uuid::new_v4().simple()));
    let endpoint = EnvironmentEndpoint::parse("ws://127.0.0.1:9100/control").expect("endpoint");
    let put = |display_name: Option<&str>,
               data_route_base: Option<DataRouteBase>,
               credential: ProviderCredentialChange,
               updated_at_ms: i64| PutEnvironmentProvider {
        provider_id: provider_id.clone(),
        display_name: display_name.map(str::to_owned),
        controller_endpoint: endpoint.clone(),
        data_route_base,
        credential,
        updated_at_ms,
    };

    // A new provider needs no credential, and its data route base derives
    // from the controller endpoint.
    let bare = store
        .put_provider(put(None, None, ProviderCredentialChange::Keep, 1))
        .await
        .expect("register provider without a credential");
    assert!(!bare.has_credential);
    assert!(
        store
            .read_provider_credential(&provider_id)
            .await
            .expect("read absent credential")
            .is_none()
    );
    assert_eq!(
        bare.effective_data_route_base()
            .expect("derived data route base")
            .as_str(),
        "ws://127.0.0.1:9100/routes"
    );

    // A set credential is sealed at rest.
    let base = DataRouteBase::new("ws://127.0.0.1:9100/data").expect("base");
    let sealed = store
        .put_provider(put(
            Some("Guard"),
            Some(base.clone()),
            ProviderCredentialChange::Set(SecretValue::new("guard-bearer-secret")),
            2,
        ))
        .await
        .expect("set credential");
    assert!(sealed.has_credential);
    assert_eq!(sealed.data_route_base, Some(base.clone()));
    assert_eq!((sealed.created_at_ms, sealed.updated_at_ms), (1, 2));
    let ciphertext: Vec<u8> = sqlx::query_scalar(
        "SELECT credential_ciphertext FROM environment_providers WHERE provider_id = $1",
    )
    .bind(provider_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("read sealed credential");
    assert!(
        !ciphertext
            .windows(b"guard-bearer-secret".len())
            .any(|window| window == b"guard-bearer-secret")
    );

    // Keep leaves the stored credential; Clear removes all three columns.
    let renamed = store
        .put_provider(put(
            Some("Renamed"),
            Some(base),
            ProviderCredentialChange::Keep,
            3,
        ))
        .await
        .expect("rename provider");
    assert!(renamed.has_credential);
    assert_eq!(
        store
            .read_provider_credential(&provider_id)
            .await
            .expect("read credential")
            .expect("credential kept")
            .expose(),
        "guard-bearer-secret"
    );
    let cleared = store
        .put_provider(put(None, None, ProviderCredentialChange::Clear, 4))
        .await
        .expect("clear credential");
    assert!(!cleared.has_credential);
    assert!(cleared.data_route_base.is_none());
    let columns: (Option<String>, Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT credential_key_id, credential_nonce, credential_ciphertext FROM environment_providers WHERE provider_id = $1",
    )
    .bind(provider_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("read credential columns");
    assert_eq!(columns, (None, None, None));
    assert!(
        store
            .list_providers()
            .await
            .expect("list providers")
            .contains(&cleared)
    );

    // Bindings are keyed by provider; a missing provider is NotFound.
    let binding = store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id,
            provider_id: provider_id.clone(),
            enabled: true,
            updated_at_ms: 5,
        })
        .await
        .expect("bind provider");
    assert!(binding.enabled);
    let disabled = store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id,
            provider_id: provider_id.clone(),
            enabled: false,
            updated_at_ms: 6,
        })
        .await
        .expect("disable binding");
    assert!(!disabled.enabled);
    assert_eq!((disabled.created_at_ms, disabled.updated_at_ms), (5, 6));
    assert_eq!(
        store
            .read_provider_binding(universe_id, &provider_id)
            .await
            .expect("read binding"),
        disabled
    );
    assert_eq!(
        store
            .list_provider_bindings(universe_id)
            .await
            .expect("list bindings"),
        vec![disabled]
    );
    assert!(matches!(
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id,
                provider_id: EnvironmentProviderId::new(format!(
                    "missing-{}",
                    Uuid::new_v4().simple()
                )),
                enabled: true,
                updated_at_ms: 6,
            })
            .await,
        Err(EnvironmentRegistryError::NotFound {
            kind: "environment_provider",
            ..
        })
    ));

    // Creates need an enabled binding; deletes are guarded.
    assert!(matches!(
        store
            .create_environment(create_environment(
                &provider_id,
                "provider-request",
                "provider-env",
                10,
            ))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id,
            provider_id: provider_id.clone(),
            enabled: true,
            updated_at_ms: 7,
        })
        .await
        .expect("enable binding");
    let environment = store
        .create_environment(create_environment(
            &provider_id,
            "provider-request",
            "provider-env",
            10,
        ))
        .await
        .expect("create environment");
    assert!(matches!(
        store.delete_provider(&provider_id).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert!(matches!(
        store
            .delete_provider_binding(universe_id, &provider_id)
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: environment.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 11,
        })
        .await
        .expect("begin close");
    let closed = store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: environment.environment_id.clone(),
            at_ms: 12,
        })
        .await
        .expect("finish close");
    assert_eq!(closed.state, EnvironmentState::Closed);
    store
        .delete_provider_binding(universe_id, &provider_id)
        .await
        .expect("delete binding once its environments are closed");
    store
        .delete_provider(&provider_id)
        .await
        .expect("closed environments do not hold a provider");

    // The all-or-none credential CHECK names its NULL case explicitly.
    let orphan_id = format!("check-{}", Uuid::new_v4().simple());
    let refused = sqlx::query(
        "INSERT INTO environment_providers (provider_id, controller_endpoint, credential_nonce, credential_ciphertext, created_at_ms, updated_at_ms) VALUES ($1, 'ws://127.0.0.1:1/control', $2, $3, 1, 1)",
    )
    .bind(&orphan_id)
    .bind(vec![0_u8; 12])
    .bind(vec![1_u8; 16])
    .execute(store.pool())
    .await;
    assert_eq!(
        violated_constraint(refused).as_deref(),
        Some("environment_providers_credential_all_or_none")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_provisioned_environment_lifecycle_follows_the_domain_rules() {
    use environments::{
        EndProvisionedEnvironment, EnvironmentDisplayStatus, FailEnvironment, IdlePolicy,
        ObservedTarget, OpenEnvironment, PowerState, RecordTargetObservation,
        ScheduleEnvironmentAttempt, TargetPhase, UpdateEnvironment,
    };

    let store = live_store("environment-lifecycle", 1024).await;
    let provider_id = bound_test_provider(&store, "lifecycle").await;
    let idle = IdlePolicy {
        pause_after_ms: Some(60_000),
        ..IdlePolicy::default()
    };
    let request = CreateEnvironment {
        idle: Some(idle.clone()),
        display_name: Some("Sandbox".to_owned()),
        ..create_environment(&provider_id, "lifecycle-a", "lifecycle-a", 100)
    };

    // Creates are idempotent per request id with identical inputs.
    let created = store
        .create_environment(request.clone())
        .await
        .expect("create");
    assert_eq!(created.state, EnvironmentState::Provisioning);
    let source = created.provisioned().expect("provisioned").clone();
    assert_eq!(source.idle, Some(idle));
    assert_eq!((source.next_attempt_at_ms, source.attempts), (Some(100), 0));
    assert_eq!(
        store
            .create_environment(CreateEnvironment {
                created_at_ms: 200,
                ..request.clone()
            })
            .await
            .expect("retry create"),
        created
    );
    assert!(matches!(
        store
            .create_environment(CreateEnvironment {
                template_id: EnvironmentTemplateId::new("other-template"),
                ..request.clone()
            })
            .await,
        Err(EnvironmentRegistryError::AlreadyExists {
            kind: "environment_request",
            ..
        })
    ));
    assert_eq!(
        store
            .read_environment(&created.environment_id)
            .await
            .expect("read back"),
        created
    );
    assert!(matches!(
        store
            .create_environment(create_environment(
                &EnvironmentProviderId::new(format!("unbound-{}", Uuid::new_v4().simple())),
                "lifecycle-unbound",
                "lifecycle-unbound",
                100,
            ))
            .await,
        Err(EnvironmentRegistryError::NotFound {
            kind: "environment_provider_binding",
            ..
        })
    ));

    // The work queue holds due provisioning rows only.
    let due = |rows: Vec<environments::EnvironmentRecord>| {
        rows.iter()
            .any(|record| record.environment_id == created.environment_id)
    };
    assert!(!due(store
        .list_environment_work(99, 10)
        .await
        .expect("work")));
    assert!(due(store
        .list_environment_work(100, 10)
        .await
        .expect("work")));
    let scheduled = store
        .schedule_environment_attempt(ScheduleEnvironmentAttempt {
            environment_id: created.environment_id.clone(),
            expected_state: EnvironmentState::Provisioning,
            next_attempt_at_ms: 2_100,
            attempts: 1,
            state_message: Some("waiting for the provider: booting".to_owned()),
            at_ms: 101,
        })
        .await
        .expect("schedule");
    let source = scheduled.provisioned().expect("provisioned");
    assert_eq!(
        (source.next_attempt_at_ms, source.attempts),
        (Some(2_100), 1)
    );
    assert_eq!(
        scheduled.state_message.as_deref(),
        Some("waiting for the provider: booting")
    );
    assert!(!due(store
        .list_environment_work(2_099, 10)
        .await
        .expect("work")));

    // Observations record the target id and keep the freshest report.
    let target = ProviderTargetId::new("pod-a").expect("target id");
    let observe = |target_id: Option<ProviderTargetId>, phase, power, at| RecordTargetObservation {
        environment_id: created.environment_id.clone(),
        provider_target_id: target_id,
        observed: ObservedTarget {
            phase,
            power,
            message: None,
            observed_at_ms: 0,
        },
        public_endpoint: Some("https://a.example.test".to_owned()),
        observed_at_ms: at,
    };
    let observed = store
        .record_target_observation(observe(
            Some(target.clone()),
            TargetPhase::Active,
            Some(PowerState::Running),
            102,
        ))
        .await
        .expect("observe");
    let source = observed.provisioned().expect("provisioned");
    assert_eq!(source.provider_target_id, Some(target.clone()));
    assert_eq!(
        source
            .observed
            .as_ref()
            .map(|observed| observed.observed_at_ms),
        Some(102)
    );
    assert_eq!(
        source.public_endpoint.as_deref(),
        Some("https://a.example.test")
    );
    let stale = store
        .record_target_observation(observe(None, TargetPhase::Provisioning, None, 90))
        .await
        .expect("stale observation");
    assert_eq!(stale, observed);
    assert!(matches!(
        store
            .record_target_observation(observe(
                Some(ProviderTargetId::new("pod-other").expect("target id")),
                TargetPhase::Active,
                None,
                103,
            ))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    let second = store
        .create_environment(create_environment(
            &provider_id,
            "lifecycle-b",
            "lifecycle-b",
            100,
        ))
        .await
        .expect("create second");
    assert!(matches!(
        store
            .record_target_observation(RecordTargetObservation {
                environment_id: second.environment_id.clone(),
                ..observe(Some(target.clone()), TargetPhase::Active, None, 103)
            })
            .await,
        Err(EnvironmentRegistryError::AlreadyExists {
            kind: "provider_target",
            ..
        })
    ));

    // Open clears the schedule and records the build; a second open skips.
    let opened = store
        .open_environment(OpenEnvironment {
            environment_id: created.environment_id.clone(),
            daemon_build: test_build("1.2.3"),
            at_ms: 104,
        })
        .await
        .expect("open");
    assert_eq!(opened.state, EnvironmentState::Open);
    assert_eq!(opened.state_message, None);
    assert_eq!(opened.daemon_build, Some(test_build("1.2.3")));
    let source = opened.provisioned().expect("provisioned");
    assert_eq!((source.next_attempt_at_ms, source.attempts), (None, 0));
    assert_eq!(opened.display_status(), EnvironmentDisplayStatus::Ready);
    assert!(
        store
            .list_open_provisioned_environments()
            .await
            .expect("open provisioned")
            .contains(&opened)
    );
    assert_eq!(
        store
            .open_environment(OpenEnvironment {
                environment_id: created.environment_id.clone(),
                daemon_build: test_build("9.9.9"),
                at_ms: 105,
            })
            .await
            .expect("open again"),
        opened
    );

    // Fail keeps the build; the provider ending the target closes it.
    let failed = store
        .fail_environment(FailEnvironment {
            environment_id: created.environment_id.clone(),
            message: "broken".to_owned(),
            daemon_build: None,
            at_ms: 106,
        })
        .await
        .expect("fail");
    assert_eq!(failed.state, EnvironmentState::Failed);
    assert_eq!(failed.state_message.as_deref(), Some("broken"));
    assert_eq!(failed.daemon_build, Some(test_build("1.2.3")));
    let ended = store
        .end_provisioned_environment(EndProvisionedEnvironment {
            environment_id: created.environment_id.clone(),
            at_ms: 107,
        })
        .await
        .expect("end");
    assert_eq!(ended.state, EnvironmentState::Closed);
    assert_eq!(
        ended.close_reason,
        Some(EnvironmentCloseReason::ProviderEnded)
    );
    assert_eq!(
        (ended.closed_at_ms, ended.state_message.as_deref()),
        (Some(107), None)
    );
    // A closed row keeps no public endpoint, even after a late observation.
    assert_eq!(
        ended.provisioned().expect("provisioned").public_endpoint,
        None
    );
    let late = store
        .record_target_observation(observe(None, TargetPhase::Closed, None, 108))
        .await
        .expect("late observation");
    assert_eq!(
        late.provisioned().expect("provisioned").public_endpoint,
        None
    );
    assert!(matches!(
        store
            .update_environment(UpdateEnvironment {
                environment_id: created.environment_id.clone(),
                display_name: None,
                metadata: Default::default(),
                updated_at_ms: 108,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));

    // A close accepted while a create was in flight wins over its answer.
    let closing = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: second.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 110,
        })
        .await
        .expect("begin close");
    assert_eq!(closing.state, EnvironmentState::Closing);
    assert_eq!(
        closing
            .provisioned()
            .and_then(|source| source.next_attempt_at_ms),
        Some(110)
    );
    assert_eq!(
        store
            .open_environment(OpenEnvironment {
                environment_id: second.environment_id.clone(),
                daemon_build: test_build("1.2.3"),
                at_ms: 111,
            })
            .await
            .expect("late open"),
        closing
    );
    assert_eq!(
        store
            .fail_environment(FailEnvironment {
                environment_id: second.environment_id.clone(),
                message: "late failure".to_owned(),
                daemon_build: None,
                at_ms: 111,
            })
            .await
            .expect("late failure"),
        closing
    );
    let finished = store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: second.environment_id.clone(),
            at_ms: 112,
        })
        .await
        .expect("finish close");
    assert_eq!(
        finished.close_reason,
        Some(EnvironmentCloseReason::Requested)
    );
    assert_eq!(finished.closed_at_ms, Some(112));

    // A lagging clock is clamped to the row's own times.
    let late = store
        .create_environment(create_environment(
            &provider_id,
            "lifecycle-c",
            "lifecycle-c",
            500,
        ))
        .await
        .expect("create third");
    let clamped = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: late.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 10,
        })
        .await
        .expect("close with a lagging clock");
    assert_eq!(clamped.state_changed_at_ms, 500);
    assert_eq!(
        clamped
            .provisioned()
            .and_then(|source| source.next_attempt_at_ms),
        Some(500)
    );

    // Updates replace the display name and caller metadata.
    let renamed = store
        .create_environment(create_environment(
            &provider_id,
            "lifecycle-d",
            "lifecycle-d",
            600,
        ))
        .await
        .expect("create fourth");
    let updated = store
        .update_environment(UpdateEnvironment {
            environment_id: renamed.environment_id.clone(),
            display_name: Some("Renamed".to_owned()),
            metadata: [("team".to_owned(), "infra".to_owned())].into(),
            updated_at_ms: 601,
        })
        .await
        .expect("update");
    assert_eq!(updated.display_name.as_deref(), Some("Renamed"));
    assert_eq!(
        updated.metadata.get("team").map(String::as_str),
        Some("infra")
    );
    assert!(matches!(
        store
            .update_environment(UpdateEnvironment {
                environment_id: renamed.environment_id.clone(),
                display_name: None,
                metadata: [("lightspeed.reserved".to_owned(), "x".to_owned())].into(),
                updated_at_ms: 602,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));

    // External environments are open at once and close in one step.
    let external = CreateExternalEnvironment {
        request_id: EnvironmentProvisionRequestId::new("lifecycle-external"),
        environment_id: EnvironmentId::new("lifecycle-external"),
        endpoint: EnvironmentEndpoint::parse("ws://127.0.0.1:9200/envd").expect("endpoint"),
        display_name: None,
        metadata: Default::default(),
        created_at_ms: 700,
    };
    let opened_external = store
        .create_external_environment(external.clone())
        .await
        .expect("create external");
    assert_eq!(opened_external.state, EnvironmentState::Open);
    assert_eq!(
        store
            .create_external_environment(external.clone())
            .await
            .expect("retry external"),
        opened_external
    );
    assert!(matches!(
        store
            .fail_environment(FailEnvironment {
                environment_id: external.environment_id.clone(),
                message: "external".to_owned(),
                daemon_build: None,
                at_ms: 701,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    let closed_external = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: external.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 702,
        })
        .await
        .expect("close external");
    assert_eq!(closed_external.state, EnvironmentState::Closed);
    assert_eq!(closed_external.closed_at_ms, Some(702));

    // Leave nothing for a running dev reconciler to chase.
    store_pg::delete_universe(store.pool(), store.config().universe_id)
        .await
        .expect("delete test universe");
    store
        .delete_provider(&provider_id)
        .await
        .expect("delete test provider");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_environment_listing_pages_filters_and_skips_bad_rows() {
    let store = live_store("environment-listing", 1024).await;
    let provider_id = bound_test_provider(&store, "listing").await;
    for index in 1..=5 {
        let team = if index % 2 == 1 { "a" } else { "b" };
        store
            .create_environment(CreateEnvironment {
                metadata: [("team".to_owned(), team.to_owned())].into(),
                ..create_environment(
                    &provider_id,
                    &format!("list-request-{index}"),
                    &format!("list-{index}"),
                    10,
                )
            })
            .await
            .expect("create listed environment");
    }
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: EnvironmentId::new("list-2"),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 11,
        })
        .await
        .expect("begin close");
    store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: EnvironmentId::new("list-2"),
            at_ms: 12,
        })
        .await
        .expect("finish close");
    let ids = |page: &environments::EnvironmentPage| {
        page.environments
            .iter()
            .map(|record| record.environment_id.as_str().to_owned())
            .collect::<Vec<_>>()
    };

    // Closed rows are hidden by default; pages continue after an id.
    let first = store
        .list_environments(ListEnvironments {
            limit: 2,
            ..ListEnvironments::default()
        })
        .await
        .expect("first page");
    assert_eq!(ids(&first), ["list-1", "list-3"]);
    assert_eq!(first.next_after, Some(EnvironmentId::new("list-3")));
    let second = store
        .list_environments(ListEnvironments {
            limit: 2,
            after: first.next_after.clone(),
            ..ListEnvironments::default()
        })
        .await
        .expect("second page");
    assert_eq!(ids(&second), ["list-4", "list-5"]);
    assert_eq!(second.next_after, None);
    let all = store
        .list_environments(ListEnvironments {
            include_closed: true,
            ..ListEnvironments::default()
        })
        .await
        .expect("include closed");
    assert_eq!(all.environments.len(), 5);
    let closed = store
        .list_environments(ListEnvironments {
            state: Some(EnvironmentState::Closed),
            ..ListEnvironments::default()
        })
        .await
        .expect("closed only");
    assert_eq!(ids(&closed), ["list-2"]);
    let team_a = store
        .list_environments(ListEnvironments {
            metadata: [("team".to_owned(), "a".to_owned())].into(),
            ..ListEnvironments::default()
        })
        .await
        .expect("by metadata");
    assert_eq!(ids(&team_a), ["list-1", "list-3", "list-5"]);
    let by_provider = store
        .list_environments(ListEnvironments {
            provider_id: Some(provider_id.clone()),
            ..ListEnvironments::default()
        })
        .await
        .expect("by provider");
    assert_eq!(ids(&by_provider), ["list-1", "list-3", "list-4", "list-5"]);
    let other_provider = store
        .list_environments(ListEnvironments {
            provider_id: Some(EnvironmentProviderId::new("listing-other")),
            ..ListEnvironments::default()
        })
        .await
        .expect("by another provider");
    assert!(other_provider.environments.is_empty());
    for limit in [0, 501] {
        assert!(matches!(
            store
                .list_environments(ListEnvironments {
                    limit,
                    ..ListEnvironments::default()
                })
                .await,
            Err(EnvironmentRegistryError::InvalidInput { .. })
        ));
    }

    // A row that no longer decodes is skipped by listings instead of
    // failing them.
    let oversized: serde_json::Map<String, serde_json::Value> = (0..40)
        .map(|index| (format!("key{index}"), serde_json::json!("v")))
        .collect();
    sqlx::query(
        "UPDATE environments SET metadata_json = $3 WHERE universe_id = $1 AND environment_id = $2",
    )
    .bind(store.config().universe_id)
    .bind("list-4")
    .bind(serde_json::Value::Object(oversized))
    .execute(store.pool())
    .await
    .expect("poison row");
    let listed = store
        .list_environments(ListEnvironments::default())
        .await
        .expect("list despite a bad row");
    assert_eq!(ids(&listed), ["list-1", "list-3", "list-5"]);

    store_pg::delete_universe(store.pool(), store.config().universe_id)
        .await
        .expect("delete test universe");
    store
        .delete_provider(&provider_id)
        .await
        .expect("delete test provider");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_environment_checks_refuse_null_positive_disjuncts() {
    use environments::{
        CreateEnvironmentRegistrationKey, EnvironmentRegistrationKeyId,
        EnvironmentRegistrationKeyStore, RegisteredIdentityMode, RegistrationKeyPolicy,
        mint_registration_key,
    };

    let store = live_store("environment-checks", 1024).await;
    let universe_id = store.config().universe_id;
    let minted = mint_registration_key(
        EnvironmentRegistrationKeyId::new("checks"),
        RegistrationKeyPolicy {
            display_name: "Checks".to_owned(),
            identity_mode: RegisteredIdentityMode::Persistent,
            max_active_environments: None,
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: None,
            expires_at_ms: None,
        },
        1,
    )
    .expect("mint key");
    store
        .create_registration_key(CreateEnvironmentRegistrationKey {
            secret_hash: minted.secret.secret_hash.clone(),
            record: minted.record.clone(),
        })
        .await
        .expect("create key");

    let external = sqlx::query(
        "INSERT INTO environments (universe_id, environment_id, source_kind, state, state_changed_at_ms, request_id, created_at_ms, updated_at_ms) VALUES ($1, 'check-external', 'external', 'open', 1, 'check-external', 1, 1)",
    )
    .bind(universe_id)
    .execute(store.pool())
    .await;
    assert_eq!(
        violated_constraint(external).as_deref(),
        Some("environments_source_fields")
    );

    let public_key = test_daemon_public_key();
    for (missing, daemon_public_key, connection_changed_at_ms, last_seen_at_ms) in [
        ("daemon_public_key", None, Some(1_i64), Some(1_i64)),
        (
            "connection_changed_at_ms",
            Some(public_key.as_str()),
            None,
            Some(1),
        ),
        ("last_seen_at_ms", Some(public_key.as_str()), Some(1), None),
    ] {
        let registered = sqlx::query(
            "INSERT INTO environments (universe_id, environment_id, source_kind, state, state_changed_at_ms, registration_key_id, daemon_public_key, connected, connection_changed_at_ms, last_seen_at_ms, created_at_ms, updated_at_ms) VALUES ($1, $2, 'registered', 'open', 1, 'checks', $3, true, $4, $5, 1, 1)",
        )
        .bind(universe_id)
        .bind(format!("check-registered-{missing}").replace('_', "-"))
        .bind(daemon_public_key)
        .bind(connection_changed_at_ms)
        .bind(last_seen_at_ms)
        .execute(store.pool())
        .await;
        assert_eq!(
            violated_constraint(registered).as_deref(),
            Some("environments_source_fields"),
            "a registered row without {missing} must be refused"
        );
    }

    let observed = sqlx::query(
        "INSERT INTO environments (universe_id, environment_id, source_kind, state, state_changed_at_ms, request_id, provider_id, template_id, next_attempt_at_ms, observed_power, created_at_ms, updated_at_ms) VALUES ($1, 'check-observed', 'provisioned', 'provisioning', 1, 'check-observed', 'check-provider', 'rust-v1', 1, 'running', 1, 1)",
    )
    .bind(universe_id)
    .execute(store.pool())
    .await;
    assert_eq!(
        violated_constraint(observed).as_deref(),
        Some("environments_observation")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_registered_environments_track_connections_and_sweep() {
    use environments::{
        CreateEnvironmentRegistrationKey, CreateRegisteredEnvironment,
        EnvironmentRegistrationKeyId, EnvironmentRegistrationKeyStore, EnvironmentUse,
        EnvironmentWaitReason, MarkRegisteredConnected, MarkRegisteredDisconnected,
        REGISTERED_RECONNECT_WAIT_MS, RegisteredIdentityMode, RegistrationKeyPolicy,
        StampRegisteredSeen, mint_registration_key,
    };

    let store = live_store("environment-registered", 1024).await;
    let universe_id = store.config().universe_id;
    let create_key = |id: &str, identity_mode, grace, offline_close| {
        mint_registration_key(
            EnvironmentRegistrationKeyId::new(id),
            RegistrationKeyPolicy {
                display_name: id.to_owned(),
                identity_mode,
                max_active_environments: Some(2),
                ephemeral_disconnect_grace_ms: grace,
                offline_close_after_ms: offline_close,
                expires_at_ms: None,
            },
            1,
        )
        .expect("mint key")
    };
    let mut secrets = std::collections::BTreeMap::new();
    for minted in [
        create_key(
            "fleet",
            RegisteredIdentityMode::Persistent,
            None,
            Some(3_600_000),
        ),
        create_key(
            "sandboxes",
            RegisteredIdentityMode::Ephemeral,
            Some(60_000),
            None,
        ),
    ] {
        store
            .create_registration_key(CreateEnvironmentRegistrationKey {
                secret_hash: minted.secret.secret_hash.clone(),
                record: minted.record.clone(),
            })
            .await
            .expect("create key");
        secrets.insert(
            minted.record.registration_key_id.to_string(),
            minted.secret.secret_hash,
        );
    }
    let admit =
        |key: &str, environment_id: &str, public_key: &str, at: i64| CreateRegisteredEnvironment {
            registration_key_id: EnvironmentRegistrationKeyId::new(key),
            secret_hash: secrets[key].clone(),
            environment_id: EnvironmentId::new(environment_id),
            daemon_public_key: public_key.to_owned(),
            display_name: Some(environment_id.to_owned()),
            metadata: Default::default(),
            daemon_build: test_build("1.0.0"),
            created_at_ms: at,
        };

    // Admission inserts an open, connected row; a known identity converges.
    let key_0 = test_daemon_public_key();
    let pod_0 = store
        .create_registered_environment(admit("fleet", "pod-0", &key_0, 100))
        .await
        .expect("admit pod-0");
    assert_eq!(pod_0.state, EnvironmentState::Open);
    assert_eq!(pod_0.request_id, None);
    let source = pod_0.registered().expect("registered").clone();
    assert!(source.connected);
    assert_eq!(source.identity_mode, RegisteredIdentityMode::Persistent);
    assert_eq!(
        (source.connection_changed_at_ms, source.last_seen_at_ms),
        (100, 100)
    );
    assert_eq!(
        store
            .create_registered_environment(admit("fleet", "pod-0-retry", &key_0, 200))
            .await
            .expect("retry admission"),
        pod_0
    );
    assert_eq!(
        store
            .read_environment_by_daemon_public_key(&key_0)
            .await
            .expect("read by daemon key"),
        Some(pod_0.clone())
    );
    assert_eq!(
        store_pg::find_registered_environment_universe(store.pool(), &key_0)
            .await
            .expect("find universe by daemon key"),
        Some(universe_id)
    );
    store
        .create_registered_environment(admit("fleet", "pod-1", &test_daemon_public_key(), 150))
        .await
        .expect("admit pod-1");
    assert!(matches!(
        store
            .create_registered_environment(admit("fleet", "pod-2", &test_daemon_public_key(), 160,))
            .await,
        Err(EnvironmentRegistryError::RegistrationCapacityExhausted { limit: 2, .. })
    ));
    let listed_universes = store_pg::list_universes_with_registered_environments(store.pool())
        .await
        .expect("universes with registered environments");
    assert!(listed_universes.contains(&universe_id));

    // The liveness stamp is coarse and conditional.
    let stamp = |at_ms| StampRegisteredSeen {
        environment_id: EnvironmentId::new("pod-0"),
        at_ms,
    };
    assert!(
        !store
            .stamp_registered_seen(stamp(300_099))
            .await
            .expect("early stamp")
    );
    assert!(
        store
            .stamp_registered_seen(stamp(300_100))
            .await
            .expect("due stamp")
    );
    assert_eq!(
        store
            .read_environment(&EnvironmentId::new("pod-0"))
            .await
            .expect("read stamped")
            .registered()
            .map(|source| source.last_seen_at_ms),
        Some(300_100)
    );
    assert!(matches!(
        store
            .stamp_registered_seen(StampRegisteredSeen {
                environment_id: EnvironmentId::new("missing"),
                at_ms: 1,
            })
            .await,
        Err(EnvironmentRegistryError::NotFound { .. })
    ));

    // Disconnects and reconnects are written on the transition only.
    let disconnected = store
        .mark_registered_disconnected(MarkRegisteredDisconnected {
            environment_id: EnvironmentId::new("pod-0"),
            at_ms: 400_000,
        })
        .await
        .expect("disconnect");
    let source = disconnected.registered().expect("registered");
    assert!(!source.connected);
    assert_eq!(source.connection_changed_at_ms, 400_000);
    assert_eq!(
        store
            .mark_registered_disconnected(MarkRegisteredDisconnected {
                environment_id: EnvironmentId::new("pod-0"),
                at_ms: 400_500,
            })
            .await
            .expect("disconnect again"),
        disconnected
    );
    assert_eq!(
        disconnected.use_gate(400_001),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Disconnected,
            retry_after_ms: environments::WAIT_RETRY_AFTER_MS,
            until_ms: Some(400_000 + REGISTERED_RECONNECT_WAIT_MS),
        }
    );
    assert!(
        !store
            .stamp_registered_seen(stamp(900_000))
            .await
            .expect("stamp while away")
    );
    let reconnected = store
        .mark_registered_connected(MarkRegisteredConnected {
            environment_id: EnvironmentId::new("pod-0"),
            daemon_build: test_build("1.1.0"),
            at_ms: 410_000,
        })
        .await
        .expect("reconnect");
    let source = reconnected.registered().expect("registered");
    assert!(source.connected);
    assert_eq!(
        (source.connection_changed_at_ms, source.last_seen_at_ms),
        (410_000, 410_000)
    );
    assert_eq!(reconnected.daemon_build, Some(test_build("1.1.0")));

    // A gateway restart disconnects every connected row as of now.
    assert_eq!(
        store
            .repair_registered_connections(500_000)
            .await
            .expect("repair"),
        2
    );
    let repaired = store
        .read_environment(&EnvironmentId::new("pod-0"))
        .await
        .expect("read repaired");
    let source = repaired.registered().expect("registered");
    assert!(!source.connected);
    assert_eq!(
        (source.connection_changed_at_ms, source.last_seen_at_ms),
        (500_000, 410_000)
    );

    // Sweep rule 2: an ephemeral daemon away past its grace closes.
    store
        .create_registered_environment(admit(
            "sandboxes",
            "sbx-0",
            &test_daemon_public_key(),
            1_000_000,
        ))
        .await
        .expect("admit sbx-0");
    store
        .create_registered_environment(admit(
            "sandboxes",
            "sbx-1",
            &test_daemon_public_key(),
            1_000_000,
        ))
        .await
        .expect("admit sbx-1");
    store
        .mark_registered_disconnected(MarkRegisteredDisconnected {
            environment_id: EnvironmentId::new("sbx-0"),
            at_ms: 1_000_000,
        })
        .await
        .expect("disconnect sbx-0");
    let sweep = store
        .sweep_registered_environments(1_060_001)
        .await
        .expect("first sweep");
    assert!(sweep.disconnected.is_empty());
    assert_eq!(
        sweep.closed,
        vec![(
            EnvironmentId::new("sbx-0"),
            EnvironmentCloseReason::EphemeralGrace
        )]
    );

    // Rule 1 flips a stale connected row, which rule 2 then closes; rule 3
    // closes persistent rows away past the key's offline close-after.
    let sweep = store
        .sweep_registered_environments(4_200_000)
        .await
        .expect("second sweep");
    assert_eq!(sweep.disconnected, vec![EnvironmentId::new("sbx-1")]);
    assert_eq!(
        sweep.closed,
        vec![
            (
                EnvironmentId::new("pod-0"),
                EnvironmentCloseReason::OfflineTimeout
            ),
            (
                EnvironmentId::new("pod-1"),
                EnvironmentCloseReason::OfflineTimeout
            ),
            (
                EnvironmentId::new("sbx-1"),
                EnvironmentCloseReason::EphemeralGrace
            ),
        ]
    );
    let closed = store
        .read_environment(&EnvironmentId::new("pod-0"))
        .await
        .expect("read swept");
    assert_eq!(closed.state, EnvironmentState::Closed);
    assert_eq!(closed.closed_at_ms, Some(4_200_000));
    assert!(
        store
            .sweep_registered_environments(4_200_000)
            .await
            .expect("idle sweep")
            .closed
            .is_empty()
    );

    // A closed identity is spent.
    assert!(matches!(
        store
            .mark_registered_connected(MarkRegisteredConnected {
                environment_id: EnvironmentId::new("pod-0"),
                daemon_build: test_build("1.1.0"),
                at_ms: 4_300_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert_eq!(
        store
            .create_registered_environment(admit("fleet", "pod-0-again", &key_0, 4_300_000))
            .await
            .expect("spent identity converges"),
        closed
    );
    let usage = store
        .registration_key_usage(&EnvironmentRegistrationKeyId::new("fleet"))
        .await
        .expect("usage");
    assert_eq!((usage.registered, usage.active), (2, 0));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_registration_keys_rotate_update_and_resolve_live_secrets() {
    use environments::{
        CreateEnvironmentRegistrationKey, CreateRegisteredEnvironment,
        EnvironmentRegistrationKeyId, EnvironmentRegistrationKeyStore, RegisteredIdentityMode,
        RegistrationKeyPolicy, RegistrationKeySettings, RevokeEnvironmentRegistrationKey,
        RotateEnvironmentRegistrationKey, UpdateEnvironmentRegistrationKey, mint_registration_key,
        mint_registration_secret,
    };

    let store = live_store("environment-keys", 1024).await;
    let universe_id = store.config().universe_id;
    let key_id = EnvironmentRegistrationKeyId::new("rotating");
    let policy = |identity_mode| RegistrationKeyPolicy {
        display_name: "Rotating".to_owned(),
        identity_mode,
        max_active_environments: None,
        ephemeral_disconnect_grace_ms: None,
        offline_close_after_ms: None,
        expires_at_ms: None,
    };
    let minted = mint_registration_key(
        key_id.clone(),
        policy(RegisteredIdentityMode::Persistent),
        1,
    )
    .expect("mint key");
    let created = store
        .create_registration_key(CreateEnvironmentRegistrationKey {
            secret_hash: minted.secret.secret_hash.clone(),
            record: minted.record.clone(),
        })
        .await
        .expect("create key");
    assert_eq!(created, minted.record);
    assert_eq!(
        store
            .resolve_registration_key(&minted.secret.secret_hash)
            .await
            .expect("resolve"),
        Some(created.clone())
    );
    assert_eq!(
        store_pg::find_registration_key_universe(store.pool(), &minted.secret.secret_hash)
            .await
            .expect("find universe"),
        Some(universe_id)
    );
    assert!(matches!(
        store
            .create_registration_key(CreateEnvironmentRegistrationKey {
                secret_hash: mint_registration_secret().secret_hash,
                record: minted.record.clone(),
            })
            .await,
        Err(EnvironmentRegistryError::AlreadyExists {
            kind: "environment_registration_key",
            ..
        })
    ));

    // Rotation retires the live secret and keeps the key.
    let rotated_secret = mint_registration_secret();
    let rotated = store
        .rotate_registration_key(RotateEnvironmentRegistrationKey {
            registration_key_id: key_id.clone(),
            secret_hash: rotated_secret.secret_hash.clone(),
            key_prefix: rotated_secret.key_prefix.clone(),
            rotated_at_ms: 10,
        })
        .await
        .expect("rotate");
    assert_eq!(rotated.key_prefix, rotated_secret.key_prefix);
    assert_eq!((rotated.created_at_ms, rotated.updated_at_ms), (1, 10));
    assert_eq!(
        store
            .read_registration_key(&key_id)
            .await
            .expect("read rotated"),
        rotated
    );
    assert_eq!(
        store
            .resolve_registration_key(&minted.secret.secret_hash)
            .await
            .expect("resolve retired"),
        None
    );
    assert_eq!(
        store
            .resolve_registration_key(&rotated_secret.secret_hash)
            .await
            .expect("resolve live"),
        Some(rotated.clone())
    );
    assert_eq!(
        store_pg::find_registration_key_universe(store.pool(), &minted.secret.secret_hash)
            .await
            .expect("find by retired secret"),
        None
    );
    // An admission that resolved the key before the rotation committed
    // presents the retired secret: refused inside the key-locked
    // transaction. The live secret admits.
    let admit = |secret_hash: &str| CreateRegisteredEnvironment {
        registration_key_id: key_id.clone(),
        secret_hash: secret_hash.to_owned(),
        environment_id: EnvironmentId::new("rotating-pod"),
        daemon_public_key: test_daemon_public_key(),
        display_name: None,
        metadata: Default::default(),
        daemon_build: test_build("1.0.0"),
        created_at_ms: 12,
    };
    assert!(matches!(
        store
            .create_registered_environment(admit(&minted.secret.secret_hash))
            .await,
        Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "rotated",
            ..
        })
    ));
    assert_eq!(
        store
            .create_registered_environment(admit(&rotated_secret.secret_hash))
            .await
            .expect("admit with the live secret")
            .state,
        EnvironmentState::Open
    );
    assert!(matches!(
        store
            .rotate_registration_key(RotateEnvironmentRegistrationKey {
                registration_key_id: key_id.clone(),
                secret_hash: minted.secret.secret_hash.clone(),
                key_prefix: minted.secret.key_prefix.clone(),
                rotated_at_ms: 11,
            })
            .await,
        Err(EnvironmentRegistryError::AlreadyExists {
            kind: "environment_registration_key_secret",
            ..
        })
    ));

    // Updates replace the editable policy; the identity mode bounds it.
    let updated = store
        .update_registration_key(UpdateEnvironmentRegistrationKey {
            registration_key_id: key_id.clone(),
            settings: RegistrationKeySettings {
                display_name: "Renamed".to_owned(),
                max_active_environments: Some(5),
                ephemeral_disconnect_grace_ms: None,
                offline_close_after_ms: Some(60_000),
                expires_at_ms: Some(1_000_000),
            },
            updated_at_ms: 20,
        })
        .await
        .expect("update");
    assert_eq!(updated.display_name, "Renamed");
    assert_eq!(updated.offline_close_after_ms, Some(60_000));
    assert_eq!(updated.updated_at_ms, 20);
    assert_eq!(
        store
            .read_registration_key(&key_id)
            .await
            .expect("read updated"),
        updated
    );
    let ephemeral_id = EnvironmentRegistrationKeyId::new("ephemeral");
    let ephemeral = mint_registration_key(
        ephemeral_id.clone(),
        policy(RegisteredIdentityMode::Ephemeral),
        1,
    )
    .expect("mint ephemeral key");
    store
        .create_registration_key(CreateEnvironmentRegistrationKey {
            secret_hash: ephemeral.secret.secret_hash.clone(),
            record: ephemeral.record.clone(),
        })
        .await
        .expect("create ephemeral key");
    assert!(matches!(
        store
            .update_registration_key(UpdateEnvironmentRegistrationKey {
                registration_key_id: ephemeral_id,
                settings: RegistrationKeySettings {
                    display_name: "Ephemeral".to_owned(),
                    max_active_environments: None,
                    ephemeral_disconnect_grace_ms: None,
                    offline_close_after_ms: Some(60_000),
                    expires_at_ms: None,
                },
                updated_at_ms: 21,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert_eq!(
        store
            .list_registration_keys()
            .await
            .expect("list keys")
            .len(),
        2
    );

    // Revocation is idempotent and stops rotation; resolve still answers so
    // the caller can refuse with the reason.
    let revoked = store
        .revoke_registration_key(RevokeEnvironmentRegistrationKey {
            registration_key_id: key_id.clone(),
            revoked_at_ms: 30,
        })
        .await
        .expect("revoke");
    assert_eq!(revoked.revoked_at_ms, Some(30));
    assert_eq!(
        store
            .revoke_registration_key(RevokeEnvironmentRegistrationKey {
                registration_key_id: key_id.clone(),
                revoked_at_ms: 40,
            })
            .await
            .expect("revoke again")
            .revoked_at_ms,
        Some(30)
    );
    assert!(matches!(
        store
            .rotate_registration_key(RotateEnvironmentRegistrationKey {
                registration_key_id: key_id.clone(),
                secret_hash: mint_registration_secret().secret_hash,
                key_prefix: "lsrk_revoked".to_owned(),
                rotated_at_ms: 50,
            })
            .await,
        Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "revoked",
            ..
        })
    ));
    assert!(
        store
            .resolve_registration_key(&rotated_secret.secret_hash)
            .await
            .expect("resolve revoked key")
            .is_some_and(|key| key.revoked_at_ms == Some(30))
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres + MinIO env"]
async fn pg_live_environment_deployment_queries_find_work_and_revoked_routes() {
    use environments::{ObservedTarget, OpenEnvironment, RecordTargetObservation, TargetPhase};

    let store = live_store("environment-deployment-queries", 1024).await;
    let universe_id = store.config().universe_id;
    let provider_id = bound_test_provider(&store, "deployment-queries").await;
    let opened = store
        .create_environment(create_environment(
            &provider_id,
            "deployment-opened",
            "deployment-opened",
            100,
        ))
        .await
        .expect("create opened environment");
    let closing = store
        .create_environment(create_environment(
            &provider_id,
            "deployment-closing",
            "deployment-closing",
            100,
        ))
        .await
        .expect("create closing environment");
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: closing.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 100,
        })
        .await
        .expect("begin close");
    assert!(
        store_pg::list_universes_with_environment_work(store.pool(), 10_000_000)
            .await
            .expect("universes with work")
            .contains(&universe_id)
    );

    // Revoked routes: gone or not open rows, and rows whose binding is
    // disabled.
    store
        .record_target_observation(RecordTargetObservation {
            environment_id: opened.environment_id.clone(),
            provider_target_id: Some(
                ProviderTargetId::new("deployment-target").expect("target id"),
            ),
            observed: ObservedTarget {
                phase: TargetPhase::Active,
                power: None,
                message: None,
                observed_at_ms: 0,
            },
            public_endpoint: None,
            observed_at_ms: 101,
        })
        .await
        .expect("observe opened");
    store
        .open_environment(OpenEnvironment {
            environment_id: opened.environment_id.clone(),
            daemon_build: test_build("1.0.0"),
            at_ms: 102,
        })
        .await
        .expect("open opened");
    assert!(
        store_pg::list_universes_with_open_provisioned_environments(store.pool())
            .await
            .expect("universes with open provisioned environments")
            .contains(&universe_id)
    );
    let routes = vec![
        (universe_id, opened.environment_id.as_str().to_owned()),
        (universe_id, closing.environment_id.as_str().to_owned()),
        (universe_id, "deployment-missing".to_owned()),
    ];
    assert_eq!(
        store_pg::list_revoked_environment_routes(store.pool(), &routes)
            .await
            .expect("revoked routes"),
        vec![
            (universe_id, closing.environment_id.as_str().to_owned()),
            (universe_id, "deployment-missing".to_owned()),
        ]
    );
    store
        .put_provider_binding(PutEnvironmentProviderBinding {
            universe_id,
            provider_id: provider_id.clone(),
            enabled: false,
            updated_at_ms: 103,
        })
        .await
        .expect("disable binding");
    assert!(
        store_pg::list_revoked_environment_routes(store.pool(), &routes)
            .await
            .expect("revoked routes after disable")
            .contains(&(universe_id, opened.environment_id.as_str().to_owned()))
    );

    store_pg::delete_universe(store.pool(), universe_id)
        .await
        .expect("delete test universe");
    store
        .delete_provider(&provider_id)
        .await
        .expect("delete test provider");
}
