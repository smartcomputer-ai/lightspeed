//! Real HTTP watches with independent PostgreSQL publishers and listeners.
//! No workers, providers, or model credentials are used.
use std::{sync::Arc, time::Duration};

use bots::BotStore;
use environments::{
    CreateExternalEnvironment, EnvironmentEndpoint, EnvironmentId, EnvironmentProvisionRequestId,
    EnvironmentStore,
};
use harness::{
    SessionId,
    storage::{BlobStore, CreateSession, SessionStore},
};
use serde_json::{Value, json};
use store_pg::{
    PgAccessStore, PgStore, PgStoreConfig,
    changes::{Change, ChangeBus, ChangeKind},
};
use temporal_runtime::gateway::{GatewayAgentApi, GatewayRoutes, GatewayState, gateway_router};
use temporal_workflow::{TemporalConnectionConfig, connect_temporal};
use uuid::Uuid;
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsTotals,
    VfsWorkspaceId, VfsWorkspaceStore,
};

struct Watch {
    response: reqwest::Response,
    pending: String,
}
impl Watch {
    async fn open(url: &str, params: Value) -> anyhow::Result<Self> {
        let response = reqwest::Client::new()
            .post(url)
            .header("accept", "text/event-stream")
            .json(&json!({"id": 1, "method": "universe/watch", "params": params}))
            .send()
            .await?;
        anyhow::ensure!(response.headers()["content-type"] == "text/event-stream");
        let mut watch = Self {
            response,
            pending: String::new(),
        };
        assert_eq!(watch.next().await?["method"], "watch/reset");
        Ok(watch)
    }
    async fn next(&mut self) -> anyhow::Result<Value> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                while let Some(end) = self.pending.find("\n\n") {
                    let frame = self.pending[..end].to_owned();
                    self.pending.drain(..end + 2);
                    if let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) {
                        return Ok(serde_json::from_str(data)?);
                    }
                }
                let chunk = self
                    .response
                    .chunk()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("watch ended"))?;
                self.pending.push_str(std::str::from_utf8(&chunk)?);
            }
        })
        .await?
    }
    async fn method(&mut self, method: &str) -> anyhow::Result<Value> {
        // A listener may reconnect while the stream is starting.
        loop {
            let event = self.next().await?;
            if event["method"] == "watch/reset" && method != "watch/reset" {
                continue;
            }
            anyhow::ensure!(event["method"] == method, "unexpected event: {event}");
            return Ok(event["params"].clone());
        }
    }
}
async fn rpc(url: &str, method: &str, params: Value) -> anyhow::Result<Value> {
    let value: Value = reqwest::Client::new()
        .post(url)
        .json(&json!({"id": 2, "method": method, "params": params}))
        .send()
        .await?
        .json()
        .await?;
    anyhow::ensure!(value.get("error").is_none(), "{value}");
    Ok(value["result"]["result"].clone())
}
async fn ready(bus: &ChangeBus) -> anyhow::Result<()> {
    let mut changes = bus.subscribe_all();
    tokio::time::timeout(Duration::from_secs(5), async {
        while changes.recv().await?.is_some() {}
        anyhow::Ok(())
    })
    .await??;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL and Temporal; serialized; no model calls"]
async fn committed_rows_reach_http_watch_across_connections() -> anyhow::Result<()> {
    let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")?;
    let reader_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await?;
    PgStore::migrate(&reader_pool).await?;
    let writer_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await?;
    let universe = Uuid::new_v4();
    let reader_bus = ChangeBus::postgres(reader_pool.clone());
    ready(&reader_bus).await?;
    let writer_bus = ChangeBus::postgres(writer_pool.clone());
    ready(&writer_bus).await?;
    let reader = Arc::new(
        PgStore::new(reader_pool.clone(), PgStoreConfig::new(universe))
            .with_changes(reader_bus.clone()),
    );
    let writer =
        PgStore::new(writer_pool.clone(), PgStoreConfig::new(universe)).with_changes(writer_bus);
    writer.ensure_universe().await?;
    let api = GatewayAgentApi::builder(
        connect_temporal(&TemporalConnectionConfig::from_env()?).await?,
        reader,
    )
    .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/rpc", listener.local_addr()?);
    let router = gateway_router(
        Arc::new(GatewayState::for_api(Arc::new(api))),
        1024 * 1024,
        GatewayRoutes {
            api: true,
            environment: false,
        },
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let outcome = async {
        let refused: Value = reqwest::Client::new()
            .post(&endpoint)
            .json(&json!({"id":1,"method":"universe/watch","params":{}}))
            .send()
            .await?
            .json()
            .await?;
        assert!(refused.get("error").is_some());
        let mut workspaces = Watch::open(&endpoint, json!({"kinds":["workspace"]})).await?;
        let snapshot = writer.put_bytes(b"manifest one".to_vec()).await?;
        let workspace_id = VfsWorkspaceId::new("watched");
        let created = writer
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                display_name: Some("First".into()),
                base_snapshot_ref: None,
                head_snapshot_ref: snapshot.clone(),
                head_totals: VfsTotals::default(),
                created_at_ms: 1,
            })
            .await?;
        let changed = workspaces.method("workspaces/changed").await?;
        let listed = rpc(&endpoint, "vfs/workspaces/list", json!({})).await?;
        assert_eq!(changed["workspace"], listed["workspaces"][0]);
        let next = writer.put_bytes(b"manifest two".to_vec()).await?;
        let request = CompareAndSetVfsWorkspaceHead {
            workspace_id: workspace_id.clone(),
            expected_revision: Some(created.revision),
            display_name: Some("Updated".into()),
            new_head_snapshot_ref: next.clone(),
            new_head_totals: VfsTotals {
                files: 1,
                bytes: 10,
            },
            updated_at_ms: 2,
        };
        writer.compare_and_set_head(request.clone()).await?;
        let changed = workspaces.method("workspaces/changed").await?;
        assert_eq!(changed["workspace"]["headSnapshotRef"], next.to_string());
        assert_eq!(changed["workspace"]["revision"], 1);
        assert!(matches!(
            writer.compare_and_set_head(request).await,
            Err(VfsCatalogError::RevisionConflict { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(600), workspaces.next())
                .await
                .is_err(),
            "failed CAS must not publish"
        );
        writer.delete_workspace(&workspace_id).await?;
        assert_eq!(
            workspaces.method("workspaces/removed").await?["workspaceId"],
            "watched"
        );
        reader_bus.reset();
        workspaces.method("watch/reset").await?;

        let access = PgAccessStore::new(writer_pool.clone()).with_changes(writer.changes().clone());
        for (id, actor, visibility) in [
            ("own", "alice", api::Visibility::Restricted),
            ("private", "bob", api::Visibility::Restricted),
            ("shared", "bob", api::Visibility::Universe),
        ] {
            writer
                .create_session(CreateSession {
                    session_id: SessionId::new(id),
                    display_name: None,
                    metadata: Default::default(),
                    origin: None,
                    delete_after_close_ms: None,
                    created_at_ms: 1,
                })
                .await?;
            access
                .stamp_session(
                    universe,
                    id,
                    &api::Attribution::Actor { id: actor.into() },
                    Some(visibility),
                    None,
                )
                .await?;
        }
        writer
            .create_session(CreateSession {
                session_id: SessionId::new("child"),
                display_name: None,
                metadata: Default::default(),
                origin: Some(harness::storage::SessionOrigin {
                    kind: harness::storage::SessionOriginKind::Subagent,
                    parent_session_id: SessionId::new("private"),
                    parent_run_id: 1,
                    root_session_id: SessionId::new("private"),
                    depth: 1,
                    invocation_id: "tool-call".into(),
                    profile_id: "profile".into(),
                    profile_revision: 1,
                    limits: harness::SubagentLimits::default(),
                }),
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await?;
        // Drain publication of the setup before subscribing to changes under test.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut sessions =
            Watch::open(&endpoint, json!({"kinds":["session"], "visibleTo":"alice"})).await?;
        for id in ["own", "private", "shared", "child"] {
            writer
                .set_session_display_name(&SessionId::new(id), Some("Renamed".into()))
                .await?;
        }
        let first = sessions.method("session/changed").await?;
        let second = sessions.method("session/changed").await?;
        let mut received = vec![
            first["session"]["id"].as_str().unwrap().to_owned(),
            second["session"]["id"].as_str().unwrap().to_owned(),
        ];
        received.sort();
        assert_eq!(received, ["own", "shared"]);
        let listed = rpc(&endpoint, "session/list", json!({"visibleTo":"alice"})).await?;
        for event in [first, second] {
            assert!(
                listed["sessions"]
                    .as_array()
                    .unwrap()
                    .contains(&event["session"])
            );
        }
        let mut log_only = Change::new(universe, ChangeKind::Session, "own");
        log_only.seq = Some(3);
        writer.changes().publish(log_only);
        assert!(
            tokio::time::timeout(Duration::from_millis(600), sessions.next())
                .await
                .is_err(),
            "private and log-only rows must not leak"
        );
        access.share_session(universe, "private").await?;
        let shared_child = sessions.method("session/changed").await?;
        let shared_root = sessions.method("session/changed").await?;
        assert_eq!(shared_child["session"]["id"], "child");
        assert_eq!(shared_root["session"]["id"], "private");
        assert_eq!(
            shared_child["session"]["access"],
            shared_root["session"]["access"]
        );
        assert_eq!(
            shared_child["session"]["retention"]["rootSessionId"],
            "private"
        );
        writer
            .discard_unopened_session(&SessionId::new("own"))
            .await?;
        sessions.method("watch/reset").await?;

        let mut others = Watch::open(&endpoint, json!({"kinds":["bot","environment"]})).await?;
        let bot_id = api::BotId::new("watched");
        writer
            .create_bot(
                bot_id.clone(),
                api::BotDocument {
                    display_name: None,
                    description: None,
                    profile_id: api::ProfileId::new("profile"),
                    brief: None,
                    runs_per_day: None,
                    breaker: None,
                    routed_session_close_after_ms: None,
                    self_config: false,
                    emit: false,
                    enabled: true,
                },
                1,
            )
            .await?;
        let event = others.method("bots/changed").await?;
        assert_eq!(
            event["bot"],
            rpc(&endpoint, "bots/list", json!({})).await?["bots"][0]
        );
        writer.delete_bot(&bot_id).await?;
        assert_eq!(others.method("bots/removed").await?["botId"], "watched");
        writer
            .create_external_environment(CreateExternalEnvironment {
                request_id: EnvironmentProvisionRequestId::new("watched"),
                environment_id: EnvironmentId::new("watched"),
                endpoint: EnvironmentEndpoint::parse("ws://127.0.0.1:9200/envd")?,
                display_name: None,
                metadata: Default::default(),
                created_at_ms: 1,
            })
            .await?;
        let event = others.method("environments/changed").await?;
        assert_eq!(
            event["environment"],
            rpc(&endpoint, "environments/list", json!({})).await?["environments"][0]
        );
        anyhow::Ok(())
    }
    .await;
    server.abort();
    let _ = server.await;
    store_pg::delete_universe(&writer_pool, universe).await?;
    writer_pool.close().await;
    reader_pool.close().await;
    outcome
}
