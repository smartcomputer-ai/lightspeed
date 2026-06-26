use std::{path::PathBuf, sync::Arc};

use auth::{
    AuthFlowId, AuthFlowStore, AuthGrantId, AuthGrantStatus, AuthGrantStore, AuthGrantTokenRefresh,
    AuthProviderKind, AuthRegistryError, CreateAuthFlowRecord, CreateAuthGrantRecord,
    CreateOAuthClientRecord, FinishAuthFlow, GrantRefreshLock, ListAuthGrants, OAuthClientId,
    OAuthClientStore, PrincipalRef, PutSecretRecord, SECRET_KIND_STATIC_BEARER, SecretId,
    SecretStore, SecretValue, TokenEndpointAuthMethod, state_hash,
};
use engine::{
    BlobRef, RunId, ToolCallId, TurnId,
    session::{
        AgentHandle, DynamicEvent, DynamicJoins, DynamicUncommittedSessionEvent, EventSeq,
        SessionId, SessionPosition,
    },
    storage::{
        AppendSessionEvents, BlobEdge, BlobGraphStore, BlobStore, CreateClonedSession,
        CreateForkedSession, CreateSession, ListSessionLinks, ReadSessionEvents, SessionBlobRoot,
        SessionLinkDirection, SessionStore, UpsertSessionLink,
    },
};
use environments::{
    CreateJobHandle, CreateSessionEnvironmentBinding, EnvironmentId,
    EnvironmentProviderCapabilities, EnvironmentProviderHeartbeat, EnvironmentProviderId,
    EnvironmentProviderKind, EnvironmentProviderStatus, EnvironmentProviderStore,
    EnvironmentTargetStore, HostControllerConnectionSpec, JobHandleStore, ListEnvironmentProviders,
    ListEnvironmentTargets, ListJobHandles, RegisterEnvironmentProvider,
    SessionEnvironmentBindingStatus, SessionEnvironmentBindingStore,
    SessionEnvironmentCapabilities, SessionEnvironmentFsRoute, SessionEnvironmentFsRouteAccess,
    SessionEnvironmentKind, UpdateEnvironmentProviderStatus, UpdateEnvironmentTargetStatus,
    UpdateSessionEnvironmentBindingStatus, UpsertEnvironmentTargetRecord,
};
use host_protocol::{
    control::targets::HostTargetStatus,
    shared::{
        HostCapabilities, HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport,
        ImplementationInfo, JobId,
    },
};
use mcp::{
    CreateMcpServerRecord, ListMcpServers, McpApprovalPolicy, McpRegistryError, McpRegistryStore,
    McpServerAuthPolicy, McpServerId, McpServerStatus, RemoteMcpTransport,
};
use object_store::{ObjectStore, aws::AmazonS3Builder};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use store_pg::{PgStore, PgStoreConfig, SecretsMasterKey};
use tokio::sync::OnceCell;
use uuid::Uuid;
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountAccess,
    VfsMountRecord, VfsMountSource, VfsMountStore, VfsPath, VfsSnapshotRecord, VfsSnapshotSource,
    VfsSnapshotStore, VfsWorkspaceId, VfsWorkspaceStore,
};

static MIGRATED: OnceCell<()> = OnceCell::const_new();

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_sessions_are_isolated_by_universe() {
    let left = live_store("sessions-left", 64).await;
    let right = live_store("sessions-right", 64).await;
    let session_id = SessionId::new("same-session");

    left.create_session(CreateSession {
        session_id: session_id.clone(),
        agent_handle: AgentHandle::new("lightspeed.default"),
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
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.default"),
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_clone_copies_resources_and_links_sessions() {
    let store = live_store("session-graph-clone", 1024).await;
    let source_id = SessionId::new("source-session");
    let clone_id = SessionId::new("clone-session");
    let peer_id = SessionId::new("peer-session");
    for session_id in [&source_id, &peer_id] {
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("lightspeed.default"),
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
            base_snapshot_ref: Some(snapshot_ref.clone()),
            head_snapshot_ref: snapshot_ref.clone(),
            created_at_ms: 13,
        })
        .await
        .expect("create workspace");
    store
        .put_mount(VfsMountRecord {
            session_id: source_id.clone(),
            mount_path: VfsPath::parse("/workspace").expect("workspace path"),
            source: VfsMountSource::Workspace {
                workspace_id: workspace_id.clone(),
            },
            access: VfsMountAccess::ReadWrite,
        })
        .await
        .expect("put workspace mount");
    store
        .put_mount(VfsMountRecord {
            session_id: source_id.clone(),
            mount_path: VfsPath::parse("/skills").expect("skills path"),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        })
        .await
        .expect("put snapshot mount");

    let provider_id = EnvironmentProviderId::new("bridge-graph");
    let target_id = HostTargetId::new("host-graph");
    let env_id = EnvironmentId::new("local");
    store
        .register_provider(RegisterEnvironmentProvider {
            provider_id: provider_id.clone(),
            provider_kind: EnvironmentProviderKind::Bridge,
            display_name: Some("Graph bridge".to_owned()),
            controller_connection: HostControllerConnectionSpec::new(
                "ws://127.0.0.1:9000/controller",
                HostTransport::WebSocket,
            ),
            capabilities: EnvironmentProviderCapabilities {
                list_targets: true,
                attach_target: true,
                get_target: true,
                ..EnvironmentProviderCapabilities::default()
            },
            implementation: ImplementationInfo {
                name: "test-bridge".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
            lease_ttl_ms: 30_000,
            metadata: Default::default(),
            observed_at_ms: 14,
        })
        .await
        .expect("register provider");
    store
        .upsert_target(UpsertEnvironmentTargetRecord {
            provider_id: provider_id.clone(),
            target_id: target_id.clone(),
            display_name: Some("Graph host".to_owned()),
            status: HostTargetStatus::Ready,
            scope: HostScope::Default,
            capabilities: HostCapabilities::filesystem(true, true).with_process(),
            default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
            metadata: Default::default(),
            observed_at_ms: 15,
        })
        .await
        .expect("upsert target");
    store
        .create_binding(CreateSessionEnvironmentBinding {
            session_id: source_id.clone(),
            env_id: env_id.clone(),
            provider_id: provider_id.clone(),
            target_id: target_id.clone(),
            kind: SessionEnvironmentKind::AttachedHost,
            status: SessionEnvironmentBindingStatus::Ready,
            capabilities: SessionEnvironmentCapabilities {
                fs_read: true,
                fs_write: true,
                process_exec: true,
                process_stdin: true,
                network: false,
                persistent: true,
                ..SessionEnvironmentCapabilities::default()
            },
            connection: HostConnectionSpec {
                target_id: target_id.clone(),
                endpoint: "ws://127.0.0.1:9001/data".to_owned(),
                transport: HostTransport::WebSocket,
                scope: HostScope::Session {
                    session_id: source_id.as_str().to_owned(),
                },
                default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
                capabilities: HostCapabilities::filesystem(true, true).with_process(),
            },
            cwd: Some(HostPath::new("/workspace").expect("cwd")),
            fs_routes: Vec::new(),
            created_at_ms: 16,
        })
        .await
        .expect("create binding");

    let clone = store
        .create_cloned_session(CreateClonedSession {
            source_session_id: source_id.clone(),
            session_id: clone_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.default"),
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

    let clone_mounts = store
        .list_mounts(&clone_id)
        .await
        .expect("list clone mounts");
    assert_eq!(clone_mounts.len(), 2);
    assert!(
        clone_mounts
            .iter()
            .all(|mount| mount.session_id == clone_id)
    );
    assert!(clone_mounts.iter().any(|mount| matches!(
        &mount.source,
        VfsMountSource::Workspace { workspace_id: id } if id == &workspace_id
    )));
    assert!(clone_mounts.iter().any(|mount| matches!(
        &mount.source,
        VfsMountSource::Snapshot { snapshot_ref: reference } if reference == &snapshot_ref
    )));

    let clone_bindings = store
        .list_bindings_for_session(&clone_id)
        .await
        .expect("list clone bindings");
    assert_eq!(clone_bindings.len(), 1);
    assert_eq!(clone_bindings[0].session_id, clone_id);
    assert_eq!(clone_bindings[0].provider_id, provider_id);
    assert_eq!(clone_bindings[0].target_id, target_id);

    let workspace_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vfs_workspaces WHERE universe_id = $1")
            .bind(store.config().universe_id)
            .fetch_one(store.pool())
            .await
            .expect("count workspaces");
    assert_eq!(workspace_count, 1);
    let provider_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM environment_providers WHERE universe_id = $1")
            .bind(store.config().universe_id)
            .fetch_one(store.pool())
            .await
            .expect("count providers");
    assert_eq!(provider_count, 1);

    let link = store
        .upsert_link(UpsertSessionLink {
            from_session_id: clone_id.clone(),
            to_session_id: peer_id.clone(),
            relationship: "can_see".to_owned(),
            created_at_ms: 30,
            metadata: serde_json::json!({"via": "test"}),
        })
        .await
        .expect("upsert link");
    assert_eq!(link.from_session_id, clone_id.clone());
    assert_eq!(link.to_session_id, peer_id.clone());
    assert_eq!(
        store
            .list_links(ListSessionLinks {
                session_id: clone_id,
                direction: SessionLinkDirection::Outgoing,
                relationship: Some("can_see".to_owned()),
                limit: 10,
            })
            .await
            .expect("list outgoing links"),
        vec![link.clone()]
    );
    assert_eq!(
        store
            .list_links(ListSessionLinks {
                session_id: peer_id,
                direction: SessionLinkDirection::Incoming,
                relationship: None,
                limit: 10,
            })
            .await
            .expect("list incoming links"),
        vec![link]
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_fork_stitches_reads_and_clamps_parent_tail() {
    let store = live_store("session-graph-fork", 1024).await;
    let root = SessionId::new("root-session");
    store
        .create_session(CreateSession {
            session_id: root.clone(),
            agent_handle: AgentHandle::new("lightspeed.default"),
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
            agent_handle: AgentHandle::new("lightspeed.default"),
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
            agent_handle: AgentHandle::new("lightspeed.default"),
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_records_session_roots_and_blob_edges() {
    let store = live_store("graph", 1024).await;
    let session_id = SessionId::new("session-graph");
    store
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.default"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");
    store
        .append(AppendSessionEvents {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![open_event(10), open_event(11)],
        })
        .await
        .expect("append events");

    let parent = store
        .put_bytes(b"parent manifest".to_vec())
        .await
        .expect("put parent");
    let child = store
        .put_bytes(b"child payload".to_vec())
        .await
        .expect("put child");

    store
        .record_session_blob_roots(vec![
            SessionBlobRoot::for_seq(
                session_id.clone(),
                parent.clone(),
                "event",
                EventSeq::new(2),
            ),
            SessionBlobRoot::for_seq(
                session_id.clone(),
                parent.clone(),
                "event",
                EventSeq::new(1),
            ),
        ])
        .await
        .expect("record roots");
    store
        .record_blob_edges(vec![BlobEdge::contains(parent.clone(), child.clone())])
        .await
        .expect("record edge");

    let root_row = sqlx::query(
        r#"
        SELECT first_seq, last_seq
        FROM cas_session_roots
        WHERE universe_id = $1
          AND session_id = $2
          AND digest = $3
          AND root_kind = 'event'
        "#,
    )
    .bind(store.config().universe_id)
    .bind(session_id.as_str())
    .bind(digest(&parent))
    .fetch_one(store.pool())
    .await
    .expect("load root row");
    assert_eq!(
        root_row.try_get::<i64, _>("first_seq").expect("first_seq"),
        1
    );
    assert_eq!(root_row.try_get::<i64, _>("last_seq").expect("last_seq"), 2);

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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_vfs_catalog_tracks_workspace_heads_and_mounts() {
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
            base_snapshot_ref: Some(snapshot_ref.clone()),
            head_snapshot_ref: snapshot_ref.clone(),
            created_at_ms: 2,
        })
        .await
        .expect("create workspace");
    assert_eq!(workspace.revision, 0);
    assert!(matches!(
        store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                base_snapshot_ref: None,
                head_snapshot_ref: snapshot_ref.clone(),
                created_at_ms: 3,
            })
            .await,
        Err(VfsCatalogError::AlreadyExists { .. })
    ));

    let updated = store
        .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
            workspace_id: workspace_id.clone(),
            expected_revision: Some(0),
            new_head_snapshot_ref: next_ref,
            updated_at_ms: 4,
        })
        .await
        .expect("advance workspace head");
    assert_eq!(updated.revision, 1);
    assert!(matches!(
        store
            .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                workspace_id: workspace_id.clone(),
                expected_revision: Some(0),
                new_head_snapshot_ref: snapshot_ref.clone(),
                updated_at_ms: 5,
            })
            .await,
        Err(VfsCatalogError::RevisionConflict {
            actual_revision: 1,
            ..
        })
    ));

    let session_id = SessionId::new("session-vfs");
    let workspace_mount = VfsMountRecord {
        session_id: session_id.clone(),
        mount_path: VfsPath::parse("/workspace").expect("workspace mount path"),
        source: VfsMountSource::Workspace {
            workspace_id: workspace_id.clone(),
        },
        access: VfsMountAccess::ReadWrite,
    };
    let snapshot_mount = VfsMountRecord {
        session_id: session_id.clone(),
        mount_path: VfsPath::parse("/skills/openai-docs").expect("skill mount path"),
        source: VfsMountSource::Snapshot {
            snapshot_ref: snapshot_ref.clone(),
        },
        access: VfsMountAccess::ReadOnly,
    };
    store
        .put_mount(workspace_mount.clone())
        .await
        .expect("put workspace mount");
    store
        .put_mount(snapshot_mount.clone())
        .await
        .expect("put snapshot mount");
    assert_eq!(
        store.list_mounts(&session_id).await.expect("list mounts"),
        vec![snapshot_mount.clone(), workspace_mount.clone()]
    );
    store
        .remove_mount(&session_id, &snapshot_mount.mount_path)
        .await
        .expect("remove mount");
    assert_eq!(
        store.list_mounts(&session_id).await.expect("list mounts"),
        vec![workspace_mount.clone()]
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
    assert_eq!(
        store.list_mounts(&session_id).await.expect("list mounts"),
        Vec::<VfsMountRecord>::new()
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_mcp_crud_and_universe_isolation() {
    let left = live_store("mcp-left", 1024).await;
    let right = live_store("mcp-right", 1024).await;
    let server_id = McpServerId::new("crm");

    let created = left
        .create_server(create_mcp_server("crm", McpServerStatus::Active))
        .await
        .expect("create MCP server");
    assert_eq!(created.server_id, server_id);

    assert!(matches!(
        left.create_server(create_mcp_server("crm", McpServerStatus::Active))
            .await,
        Err(McpRegistryError::AlreadyExists { server_id }) if server_id.as_str() == "crm"
    ));

    assert_eq!(
        left.read_server(&server_id).await.expect("read MCP server"),
        created
    );
    assert!(matches!(
        right.read_server(&server_id).await,
        Err(McpRegistryError::NotFound { server_id }) if server_id.as_str() == "crm"
    ));

    let oauth = left
        .create_server(create_oauth_mcp_server(
            "docs",
            McpServerStatus::NeedsAuthConfig,
        ))
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

    let deleted = left
        .delete_server(&server_id)
        .await
        .expect("delete MCP server");
    assert_eq!(deleted, created);
    assert!(matches!(
        left.read_server(&server_id).await,
        Err(McpRegistryError::NotFound { server_id }) if server_id.as_str() == "crm"
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_environments_crud_and_session_bindings() {
    let store = live_store("environments", 1024).await;
    let provider_id = EnvironmentProviderId::new("bridge-local");
    let target_id = HostTargetId::new("local-host");
    let session_id = SessionId::new("session-env");
    let env_id = EnvironmentId::new("local");

    let provider = store
        .register_provider(RegisterEnvironmentProvider {
            provider_id: provider_id.clone(),
            provider_kind: EnvironmentProviderKind::Bridge,
            display_name: Some("Local bridge".to_owned()),
            controller_connection: HostControllerConnectionSpec::new(
                "ws://127.0.0.1:9000/controller",
                HostTransport::WebSocket,
            ),
            capabilities: EnvironmentProviderCapabilities {
                list_targets: true,
                attach_target: true,
                get_target: true,
                ..EnvironmentProviderCapabilities::default()
            },
            implementation: ImplementationInfo {
                name: "test-bridge".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
            lease_ttl_ms: 30_000,
            metadata: Default::default(),
            observed_at_ms: 10,
        })
        .await
        .expect("register provider");
    assert_eq!(provider.status, EnvironmentProviderStatus::Online);

    let heartbeat = store
        .update_provider_heartbeat(EnvironmentProviderHeartbeat {
            provider_id: provider_id.clone(),
            observed_at_ms: 20,
            lease_ttl_ms: Some(30_000),
            observed_targets: Vec::new(),
        })
        .await
        .expect("provider heartbeat");
    assert_eq!(heartbeat.last_seen_ms, 20);
    assert_eq!(heartbeat.lease_expires_ms, 30_020);
    assert_eq!(
        store
            .list_providers(ListEnvironmentProviders {
                status: Some(EnvironmentProviderStatus::Online),
                provider_kind: Some(EnvironmentProviderKind::Bridge),
            })
            .await
            .expect("list providers"),
        vec![heartbeat.clone()]
    );

    let target = store
        .upsert_target(UpsertEnvironmentTargetRecord {
            provider_id: provider_id.clone(),
            target_id: target_id.clone(),
            display_name: Some("Local host".to_owned()),
            status: HostTargetStatus::Ready,
            scope: HostScope::Default,
            capabilities: HostCapabilities::filesystem(true, true).with_process(),
            default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
            metadata: Default::default(),
            observed_at_ms: 30,
        })
        .await
        .expect("upsert target");
    assert_eq!(
        store
            .list_targets(ListEnvironmentTargets {
                provider_id: Some(provider_id.clone()),
                status: Some(HostTargetStatus::Ready),
            })
            .await
            .expect("list targets"),
        vec![target.clone()]
    );

    store
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.default"),
            created_at_ms: 35,
        })
        .await
        .expect("create session");

    let binding = store
        .create_binding(CreateSessionEnvironmentBinding {
            session_id: session_id.clone(),
            env_id: env_id.clone(),
            provider_id: provider_id.clone(),
            target_id: target_id.clone(),
            kind: SessionEnvironmentKind::AttachedHost,
            status: SessionEnvironmentBindingStatus::Ready,
            capabilities: SessionEnvironmentCapabilities {
                fs_read: true,
                fs_write: true,
                process_exec: true,
                process_stdin: true,
                network: false,
                persistent: true,
                ..SessionEnvironmentCapabilities::default()
            },
            connection: HostConnectionSpec {
                target_id: target_id.clone(),
                endpoint: "ws://127.0.0.1:9001/data".to_owned(),
                transport: HostTransport::WebSocket,
                scope: HostScope::Session {
                    session_id: session_id.as_str().to_owned(),
                },
                default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
                capabilities: HostCapabilities::filesystem(true, true).with_process(),
            },
            cwd: Some(HostPath::new("/workspace").expect("cwd")),
            fs_routes: vec![SessionEnvironmentFsRoute {
                path: HostPath::new("/workspace").expect("route"),
                source_path: None,
                access: SessionEnvironmentFsRouteAccess::ReadWrite,
                same_state_as_active_env: Some(env_id.clone()),
            }],
            created_at_ms: 40,
        })
        .await
        .expect("create binding");
    assert_eq!(
        store
            .list_bindings_for_session(&session_id)
            .await
            .expect("list bindings"),
        vec![binding.clone()]
    );

    let job_handle = CreateJobHandle {
        session_id: session_id.clone(),
        env_id: env_id.clone(),
        provider_id: provider_id.clone(),
        target_id: target_id.clone(),
        namespace: session_id.as_str().to_owned(),
        job_id: JobId::new("job-1"),
        name: Some("checkout".to_owned()),
        queue_key: Some("repo".to_owned()),
        created_by_run_id: Some(RunId::new(1)),
        created_by_turn_id: Some(TurnId::new(2)),
        created_by_tool_call_id: Some(ToolCallId::new("call_1")),
        created_at_ms: 45,
        start_request_hash: "hash-1".to_owned(),
    };
    let created_jobs = store
        .create_job_handles(vec![job_handle.clone()])
        .await
        .expect("create job handle");
    assert_eq!(created_jobs.len(), 1);
    assert_eq!(created_jobs[0].job_id.as_str(), "job-1");

    let retried_jobs = store
        .create_job_handles(vec![job_handle])
        .await
        .expect("idempotent create job handle");
    assert_eq!(retried_jobs, created_jobs);

    let listed_jobs = store
        .list_job_handles(ListJobHandles {
            session_id: session_id.clone(),
            env_id: Some(env_id.clone()),
            limit: Some(10),
        })
        .await
        .expect("list job handles");
    assert_eq!(listed_jobs, created_jobs);

    let read_job = store
        .read_job_handle(&session_id, &env_id, &JobId::new("job-1"))
        .await
        .expect("read job handle");
    assert_eq!(read_job.namespace, session_id.as_str());
    assert_eq!(read_job.queue_key.as_deref(), Some("repo"));
    assert_eq!(read_job.start_request_hash, "hash-1");

    let degraded = store
        .update_binding_status(UpdateSessionEnvironmentBindingStatus {
            session_id: session_id.clone(),
            env_id: env_id.clone(),
            status: SessionEnvironmentBindingStatus::Degraded,
            updated_at_ms: 50,
        })
        .await
        .expect("degrade binding");
    assert_eq!(degraded.status, SessionEnvironmentBindingStatus::Degraded);

    let stopped = store
        .update_target_status(UpdateEnvironmentTargetStatus {
            provider_id: provider_id.clone(),
            target_id: target_id.clone(),
            status: HostTargetStatus::Stopped,
            observed_at_ms: 60,
        })
        .await
        .expect("stop target");
    assert_eq!(stopped.status, HostTargetStatus::Stopped);

    let offline = store
        .update_provider_status(UpdateEnvironmentProviderStatus {
            provider_id: provider_id.clone(),
            status: EnvironmentProviderStatus::Offline,
            updated_at_ms: 70,
        })
        .await
        .expect("mark provider offline");
    assert_eq!(offline.status, EnvironmentProviderStatus::Offline);

    let deleted = store
        .delete_binding(&session_id, &env_id)
        .await
        .expect("delete binding");
    assert_eq!(deleted, degraded);
    let deleted_provider = store
        .delete_provider(&provider_id)
        .await
        .expect("delete provider");
    assert_eq!(deleted_provider.status, EnvironmentProviderStatus::Offline);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_auth_grants_crud_and_status_updates() {
    let store = live_store("auth-grants", 1024).await;
    let grant_id = AuthGrantId::new("authgrant_crm");

    let created = store
        .create_grant(CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            principal: PrincipalRef::universe_default(),
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
                    principal: PrincipalRef::universe_default(),
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
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
            principal: PrincipalRef::universe_default(),
            state_hash: state_hash(state),
            pkce_verifier_secret: SecretId::new("authsec_pkce_live"),
            redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
            scopes: vec!["contacts.read".to_owned()],
            audience: Some("https://crm.example.com/mcp".to_owned()),
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
            principal: PrincipalRef::universe_default(),
            state_hash: state_hash("state-live-2"),
            pkce_verifier_secret: SecretId::new("authsec_pkce_live2"),
            redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
            scopes: Vec::new(),
            audience: Some("https://crm.example.com/mcp".to_owned()),
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_grant_refresh_updates_token_refs_and_lock_serializes() {
    let store = Arc::new(live_store("grant-refresh", 1024).await);
    let grant_id = AuthGrantId::new("authgrant_refresh_live");
    store
        .create_grant(CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: "crm".to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            principal: PrincipalRef::universe_default(),
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
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
#[ignore = "requires local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_grant_metadata_round_trips() {
    let store = live_store("grant-metadata", 1024).await;
    let grant_id = AuthGrantId::new("authgrant_install_live");

    let created = store
        .create_grant(CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: "lightspeed-github".to_owned(),
            provider_kind: AuthProviderKind::GitHubApp,
            principal: PrincipalRef::universe_default(),
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
        created_at_ms: 10,
    }
}

async fn live_store(test_name: &str, inline_threshold_bytes: usize) -> PgStore {
    let database_url = env_or_dotenv_var("LIGHTSPEED_TEST_POSTGRES_URL").expect(
        "LIGHTSPEED_TEST_POSTGRES_URL must be set in env or root .env to run store-pg live tests; run local/up.sh and source local/env.sh",
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
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
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

fn open_event(at_ms: u64) -> DynamicUncommittedSessionEvent {
    DynamicUncommittedSessionEvent {
        observed_at_ms: at_ms,
        joins: DynamicJoins::default(),
        event: DynamicEvent::new(
            "lightspeed.test.lifecycle.closed",
            1,
            serde_json::Value::Object(Default::default()),
        ),
    }
}

fn create_mcp_server(server_id: &str, status: McpServerStatus) -> CreateMcpServerRecord {
    CreateMcpServerRecord {
        server_id: McpServerId::new(server_id),
        display_name: Some(format!("{server_id} MCP")),
        server_url: format!("https://{server_id}.example.com/mcp"),
        transport: RemoteMcpTransport::Auto,
        default_server_label: server_id.to_owned(),
        description: Some(format!("{server_id} remote MCP server")),
        allowed_tools: Some(vec!["lookup_customer".to_owned()]),
        approval_default: McpApprovalPolicy::Never,
        defer_loading_default: Some(true),
        auth_policy: McpServerAuthPolicy::None,
        status,
        created_at_ms: 10,
    }
}

fn create_oauth_mcp_server(server_id: &str, status: McpServerStatus) -> CreateMcpServerRecord {
    let mut record = create_mcp_server(server_id, status);
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
