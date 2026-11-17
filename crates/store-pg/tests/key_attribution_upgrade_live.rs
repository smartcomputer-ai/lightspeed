//! Runtime-token migration: resource attributions and durable session events.
use api::Attribution;
use futures_util::FutureExt as _;
use harness::storage::{
    AppendSessionEvents, CreateSession, ReadSessionEvents, SessionStore as _,
    UncommittedStoredEvent,
};
use harness::{ApprovalEvent, CoreAgentCodec, CoreAgentEvent, RunEvent, RunId, SessionId};
use serde_json::{Value, json};
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{panic::AssertUnwindSafe, str::FromStr};
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; creates an isolated schema"]
async fn stored_key_attributions_become_token_names_and_key_storage_is_removed() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect to live Postgres");
    let schema = format!(
        "lightspeed_key_attribution_test_{}",
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
            PgConnectOptions::from_str(&url)
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
    for migration in store_pg::MIGRATIONS
        .iter()
        .filter(|migration| migration.version < 26)
    {
        let mut transaction = pool.begin().await.unwrap();
        transaction
            .execute(sqlx::raw_sql(migration.sql))
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }
    let here = Uuid::new_v4();
    let sessions = PgStore::new(pool.clone(), PgStoreConfig::new(here));
    // Seed the historical schema directly; current provisioning writes newer columns.
    sqlx::query("INSERT INTO universes (universe_id, handle) VALUES ($1, $2)")
        .bind(here)
        .bind(store_pg::default_universe_handle(here))
        .execute(pool)
        .await
        .unwrap();
    let log = SessionId::new("migration-log");
    sessions
        .create_session(CreateSession {
            session_id: log.clone(),
            display_name: None,
            metadata: Default::default(),
            origin: None,
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let stale = json!({"kind":"key", "keyId":"ci", "scopeKind":"universe"});
    sqlx::query("UPDATE sessions SET created_by = $2, metadata_json = $3 WHERE universe_id = $1")
        .bind(here)
        .bind(&stale)
        .bind(json!({"example": stale.to_string()}))
        .execute(pool)
        .await
        .unwrap();
    for statement in [
        "INSERT INTO auth_grants (universe_id, grant_id, provider_id, provider_kind, created_by, created_at_ms, updated_at_ms) VALUES ($1, 'grant', 'static', 'static_token', $2, 1, 1)",
        "INSERT INTO auth_flows (universe_id, flow_id, client_id, provider_id, provider_kind, created_by, state_hash, pkce_verifier_secret_id, redirect_uri, expires_at_ms, created_at_ms, updated_at_ms) VALUES ($1, 'flow', 'client', 'provider', 'mcp_oauth', $2, 'state', 'verifier', 'https://example.com/callback', 2, 1, 1)",
        "INSERT INTO bots (universe_id, bot_id, revision, document_json, created_by, created_at_ms, updated_at_ms) VALUES ($1, 'bot', 1, '{}', $2, 1, 1)",
    ] {
        sqlx::query(statement)
            .bind(here)
            .bind(&stale)
            .execute(pool)
            .await
            .unwrap();
    }
    // Nested user document JSON is opaque, even when it resembles attribution.
    sqlx::query("UPDATE bots SET document_json = $2 WHERE universe_id = $1")
        .bind(here)
        .bind(json!({"example": stale}))
        .execute(pool)
        .await
        .unwrap();
    let run_id = RunId::new(1);
    let asked_by = Some(harness::Attribution::Token {
        name: "placeholder".into(),
    });
    let attributed: Vec<(CoreAgentEvent, [&str; 5], &str)> = vec![
        (
            CoreAgentEvent::Run(RunEvent::Accepted(harness::AcceptedRunEvent {
                run_id,
                submission_id: None,
                source: harness::RunSource::Input { input: Vec::new() },
                run_config: Default::default(),
                input_limit_tokens: None,
                config_revision: 0,
                notify_on_terminal: Vec::new(),
                requested_by: asked_by.clone(),
            })),
            ["event", "payload", "run", "accepted", "requested_by"],
            "ci",
        ),
        (
            CoreAgentEvent::Run(RunEvent::SteeringAccepted {
                run_id,
                steering_id: harness::SteeringId::new(1),
                submission_id: harness::SubmissionId::new("steer-1"),
                input: Vec::new(),
                requested_by: asked_by.clone(),
            }),
            [
                "event",
                "payload",
                "run",
                "steering_accepted",
                "requested_by",
            ],
            "platform",
        ),
        (
            CoreAgentEvent::Run(RunEvent::CancellationRequested {
                run_id,
                requested_by: asked_by.clone(),
            }),
            [
                "event",
                "payload",
                "run",
                "cancellation_requested",
                "requested_by",
            ],
            "other",
        ),
        (
            CoreAgentEvent::Run(RunEvent::QueuedCancelled {
                run_id,
                requested_by: asked_by.clone(),
            }),
            [
                "event",
                "payload",
                "run",
                "queued_cancelled",
                "requested_by",
            ],
            "retired",
        ),
        (
            CoreAgentEvent::Approval(ApprovalEvent::Decided {
                approval_id: harness::ApprovalId::new("approval_1"),
                run_id,
                decision: harness::ApprovalDecision::Approved,
                note: None,
                decided_by: asked_by.clone(),
            }),
            ["event", "payload", "approval", "decided", "decided_by"],
            "ci",
        ),
        // An event of the same kind attributed to an actor is left alone.
        (
            CoreAgentEvent::Run(RunEvent::CancellationRequested {
                run_id,
                requested_by: Some(harness::Attribution::Actor {
                    id: "alice".to_owned(),
                }),
            }),
            [
                "event",
                "payload",
                "run",
                "cancellation_requested",
                "requested_by",
            ],
            "",
        ),
    ];
    let appended = sessions
        .append(AppendSessionEvents {
            session_id: log.clone(),
            expected_head: None,
            events: attributed
                .iter()
                .map(|(event, _, _)| UncommittedStoredEvent {
                    observed_at_ms: 5,
                    joins: Default::default(),
                    event: CoreAgentCodec.encode_event(event).expect("encode event"),
                })
                .collect(),
        })
        .await
        .expect("append attributed events");
    assert_eq!(appended.entries.len(), attributed.len());
    for (entry, (_, path, prefix)) in appended.entries.iter().zip(&attributed) {
        if prefix.is_empty() {
            continue;
        }
        sqlx::query(
            "UPDATE session_events SET entry_json = jsonb_set(entry_json, $4::text[], $5)
             WHERE universe_id = $1 AND session_id = $2 AND seq = $3",
        )
        .bind(here)
        .bind(log.as_str())
        .bind(entry.position.seq.as_u64() as i64)
        .bind(path.as_slice())
        .bind(json!({"kind":"key", "keyId":prefix, "scopeKind":"universe"}))
        .execute(pool)
        .await
        .expect("restore the previous event shape");
    }

    let deleted = Uuid::new_v4();
    sqlx::query("INSERT INTO universes_deleted (universe_id, handle, created_at_ms, deleting_at_ms, deleted_at_ms, requested_by, summary) VALUES ($1, 'deleted', 1, 2, 3, $2, '{}')")
        .bind(deleted).bind(&stale).execute(pool).await.unwrap();
    sqlx::query(
        "UPDATE universes SET deleting_at_ms = 2, deletion_json = $2 WHERE universe_id = $1",
    )
    .bind(here)
    .bind(json!({"deletion":{"requestedBy":stale}}))
    .execute(pool)
    .await
    .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    transaction
        .execute(sqlx::raw_sql(include_str!(
            "../migrations/026_runtime_tokens.sql"
        )))
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('api_keys') IS NOT NULL")
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(!exists, "the caller key table must be removed");
    for statement in [
        "SELECT created_by FROM sessions WHERE universe_id = $1",
        "SELECT created_by FROM auth_grants WHERE universe_id = $1",
        "SELECT created_by FROM auth_flows WHERE universe_id = $1",
        "SELECT created_by FROM bots WHERE universe_id = $1",
        "SELECT deletion_json #> '{deletion,requestedBy}' FROM universes WHERE universe_id = $1",
    ] {
        let stored: Value = sqlx::query_scalar(statement)
            .bind(here)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_value::<Attribution>(stored).unwrap(),
            Attribution::token("ci")
        );
    }
    let stored: Value =
        sqlx::query_scalar("SELECT requested_by FROM universes_deleted WHERE universe_id = $1")
            .bind(deleted)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_value::<Attribution>(stored).unwrap(),
        Attribution::token("ci")
    );
    let metadata: Value =
        sqlx::query_scalar("SELECT metadata_json FROM sessions WHERE universe_id = $1")
            .bind(here)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        metadata,
        json!({"example": stale.to_string()}),
        "session metadata is not attribution"
    );
    let document: Value =
        sqlx::query_scalar("SELECT document_json FROM bots WHERE universe_id = $1")
            .bind(here)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        document,
        json!({"example": stale}),
        "user JSON is not attribution"
    );
    let page = sessions
        .read_after(ReadSessionEvents {
            session_id: log,
            after: None,
            limit: 100,
        })
        .await
        .expect("read the session log");
    let recorded: Vec<Option<harness::Attribution>> = page
        .entries
        .iter()
        .map(|entry| {
            match CoreAgentCodec
                .decode_entry(entry)
                .expect("every event decodes")
                .event
            {
                CoreAgentEvent::Run(RunEvent::Accepted(accepted)) => accepted.requested_by,
                CoreAgentEvent::Run(
                    RunEvent::SteeringAccepted { requested_by, .. }
                    | RunEvent::CancellationRequested { requested_by, .. }
                    | RunEvent::QueuedCancelled { requested_by, .. },
                ) => requested_by,
                CoreAgentEvent::Approval(ApprovalEvent::Decided { decided_by, .. }) => decided_by,
                other => panic!("unexpected event {other:?}"),
            }
        })
        .collect();

    let token = |name: &str| Some(harness::Attribution::Token { name: name.into() });
    assert_eq!(
        recorded,
        [
            token("ci"),
            token("platform"),
            token("other"),
            token("retired"),
            token("ci"),
            Some(harness::Attribution::Actor { id: "alice".into() })
        ]
    );
}
