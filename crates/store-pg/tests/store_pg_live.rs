use std::{path::PathBuf, sync::Arc};

use engine::{
    BlobRef,
    session::{
        AgentHandle, DynamicEvent, DynamicJoins, DynamicUncommittedSessionEvent, EventSeq,
        SessionId,
    },
    storage::{
        AppendSessionEvents, BlobEdge, BlobGraphStore, BlobStore, CreateSession, ReadSessionEvents,
        SessionBlobRoot, SessionStore,
    },
};
use mcp_registry::{
    CreateMcpServerRecord, ListMcpServers, McpApprovalPolicy, McpRegistryError, McpRegistryStore,
    McpServerAuthPolicy, McpServerId, McpServerStatus, RemoteMcpTransport,
};
use object_store::{ObjectStore, aws::AmazonS3Builder};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use store_pg::{PgStore, PgStoreConfig};
use tokio::sync::OnceCell;
use uuid::Uuid;
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountAccess,
    VfsMountRecord, VfsMountSource, VfsMountStore, VfsPath, VfsSnapshotRecord, VfsSnapshotSource,
    VfsSnapshotStore, VfsWorkspaceId, VfsWorkspaceStore,
};

static MIGRATED: OnceCell<()> = OnceCell::const_new();

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires dev/local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_sessions_are_isolated_by_universe() {
    let left = live_store("sessions-left", 64).await;
    let right = live_store("sessions-right", 64).await;
    let session_id = SessionId::new("same-session");

    left.create_session(CreateSession {
        session_id: session_id.clone(),
        agent_handle: AgentHandle::new("forge.default"),
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
            agent_handle: AgentHandle::new("forge.default"),
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
#[ignore = "requires dev/local/up.sh or compatible Postgres + MinIO env"]
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
#[ignore = "requires dev/local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_records_session_roots_and_blob_edges() {
    let store = live_store("graph", 1024).await;
    let session_id = SessionId::new("session-graph");
    store
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("forge.default"),
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
        FROM session_blob_roots
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
#[ignore = "requires dev/local/up.sh or compatible Postgres + MinIO env"]
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
#[ignore = "requires dev/local/up.sh or compatible Postgres + MinIO env"]
async fn pg_live_mcp_registry_crud_and_universe_isolation() {
    let left = live_store("mcp-registry-left", 1024).await;
    let right = live_store("mcp-registry-right", 1024).await;
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

async fn live_store(test_name: &str, inline_threshold_bytes: usize) -> PgStore {
    let database_url = env_or_dotenv_var("FORGE_TEST_POSTGRES_URL").expect(
        "FORGE_TEST_POSTGRES_URL must be set in env or root .env to run store-pg live tests; run dev/local/up.sh and source dev/local/env.sh",
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    migrate_once(&pool).await;

    let prefix =
        env_or_dotenv_var("FORGE_OBJECT_STORE_PREFIX").unwrap_or_else(|_| "forge".to_string());
    let config = PgStoreConfig::new(Uuid::new_v4())
        .with_inline_threshold_bytes(inline_threshold_bytes)
        .with_object_prefix(format!("{}/tests/{}", prefix.trim_matches('/'), test_name));
    let store = PgStore::with_object_store(pool, live_object_store(), config);
    store.ensure_universe().await.expect("ensure test universe");
    store
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
    let endpoint = env_or_dotenv_var("FORGE_OBJECT_STORE_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:29000".to_string());
    let bucket =
        env_or_dotenv_var("FORGE_OBJECT_STORE_BUCKET").unwrap_or_else(|_| "forge-dev".to_string());
    let region =
        env_or_dotenv_var("FORGE_OBJECT_STORE_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let access_key =
        env_or_dotenv_var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "minioadmin".to_string());
    let secret_key =
        env_or_dotenv_var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
    let force_path_style = env_or_dotenv_var("FORGE_OBJECT_STORE_FORCE_PATH_STYLE")
        .unwrap_or_else(|_| "true".to_string())
        .parse::<bool>()
        .expect("FORGE_OBJECT_STORE_FORCE_PATH_STYLE must be true or false");

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
            "forge.test.lifecycle.closed",
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
