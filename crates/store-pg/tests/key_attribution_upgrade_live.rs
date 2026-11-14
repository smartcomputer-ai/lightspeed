//! Stored attributions that named a key by the display prefix of its secret
//! are rewritten to name it by key id and scope kind. Runs against an
//! isolated schema and never loads .env.
use std::{panic::AssertUnwindSafe, str::FromStr};

use api::{AccessScope, AccessScopeKind, Attribution};
use futures_util::FutureExt as _;
use harness::storage::{
    AppendSessionEvents, CreateSession, ReadSessionEvents, SessionStore as _,
    UncommittedStoredEvent,
};
use harness::{
    ApprovalEvent, AttributedKeyScope, CoreAgentCodec, CoreAgentEvent, RunEvent, RunId, SessionId,
};
use serde_json::{Value, json};
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; creates an isolated schema"]
async fn stored_key_attributions_are_renamed_from_secret_prefix_to_key_id() {
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

/// The display prefixes the rows below are attributed to. `DEPLOYMENT` and
/// `HERE` belong to keys that still hold them; `ELSEWHERE` belongs to a key
/// of another universe; `REPLACED` belongs to no key any more.
const DEPLOYMENT: &str = "lsk_deploy00";
const HERE: &str = "lsk_here0000";
const ELSEWHERE: &str = "lsk_elsewher";
const REPLACED: &str = "lsk_replaced";

/// Rows of this universe other than sessions, by id, with the prefix each is
/// attributed to.
const OWNED: [(&str, &str); 4] = [
    ("by-universe-key", HERE),
    ("by-deployment-key", DEPLOYMENT),
    ("by-another-universes-key", ELSEWHERE),
    ("by-replaced-key", REPLACED),
];

fn stale(prefix: &str) -> Value {
    json!({"kind": "key", "prefix": prefix})
}

fn key(key_id: &str, scope_kind: AccessScopeKind) -> Attribution {
    Attribution::Key {
        key_id: key_id.to_owned(),
        scope_kind,
    }
}

/// What a record keeps when the key that made it cannot be named any more.
fn unresolved(prefix: &str) -> Attribution {
    Attribution::Internal {
        component: "migration".to_owned(),
        cause: format!("API key with secret prefix {prefix}, replaced since"),
    }
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.expect("apply migrations");
    let here = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    let sessions = PgStore::new(pool.clone(), PgStoreConfig::new(here));
    sessions.ensure_universe().await.expect("create universe");
    PgStore::new(pool.clone(), PgStoreConfig::new(elsewhere))
        .ensure_universe()
        .await
        .expect("create the other universe");

    // Keys as they are today. Each was minted by someone the old shape named
    // by prefix: a deployment key, a universe key (which can never mint a
    // key, so the prefix cannot be trusted), and a key since replaced.
    for (hash, key_id, prefix, universe, created_by) in [
        ('a', "platform", DEPLOYMENT, None, json!({"kind": "local"})),
        ('b', "ci", HERE, Some(here), stale(DEPLOYMENT)),
        ('c', "ci", ELSEWHERE, Some(elsewhere), stale(HERE)),
        ('d', "pipeline", "lsk_pipeline", None, stale(REPLACED)),
    ] {
        sqlx::query(
            "INSERT INTO api_keys (key_hash, key_id, key_prefix, universe_id, groups, created_by, created_at_ms)
             VALUES (repeat($1, 64), $2, $3, $4, ARRAY['session'], $5, 1)",
        )
        .bind(hash.to_string())
        .bind(key_id)
        .bind(prefix)
        .bind(universe)
        .bind(&created_by)
        .execute(pool)
        .await
        .expect("insert key");
    }

    // Sessions and a bot of this universe, created by each kind of key.
    let created = [
        ("by-universe-key", Some(stale(HERE))),
        ("by-deployment-key", Some(stale(DEPLOYMENT))),
        ("by-another-universes-key", Some(stale(ELSEWHERE))),
        ("by-replaced-key", Some(stale(REPLACED))),
        ("by-actor", Some(json!({"kind": "actor", "id": "alice"}))),
        ("unstamped", None),
    ];
    for (session, created_by) in &created {
        sessions
            .create_session(CreateSession {
                session_id: SessionId::new(*session),
                display_name: None,
                metadata: Default::default(),
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        sqlx::query(
            "UPDATE sessions SET created_by = $3 WHERE universe_id = $1 AND session_id = $2",
        )
        .bind(here)
        .bind(session)
        .bind(created_by)
        .execute(pool)
        .await
        .expect("stamp session as the previous release did");
    }

    // A credential, an OAuth authorization and a bot of this universe for
    // each key the old shape could name.
    for (id, prefix) in OWNED {
        sqlx::query(
            "INSERT INTO auth_grants (universe_id, grant_id, provider_id, provider_kind, created_by, created_at_ms, updated_at_ms)
             VALUES ($1, $2, 'static', 'static_token', $3, 1, 1)",
        )
        .bind(here)
        .bind(id)
        .bind(stale(prefix))
        .execute(pool)
        .await
        .expect("insert credential");
        sqlx::query(
            "INSERT INTO auth_flows (universe_id, flow_id, client_id, provider_id, provider_kind, created_by, state_hash, pkce_verifier_secret_id, redirect_uri, expires_at_ms, created_at_ms, updated_at_ms)
             VALUES ($1, $2, 'client', 'provider', 'mcp_oauth', $3, $2, 'verifier', 'https://runtime.example/callback', 2, 1, 1)",
        )
        .bind(here)
        .bind(id)
        .bind(stale(prefix))
        .execute(pool)
        .await
        .expect("insert authorization");
        sqlx::query(
            "INSERT INTO bots (universe_id, bot_id, revision, document_json, created_by, created_at_ms, updated_at_ms)
             VALUES ($1, $2, 1, '{}', $3, 1, 1)",
        )
        .bind(here)
        .bind(id)
        .bind(stale(prefix))
        .execute(pool)
        .await
        .expect("insert bot");
    }

    // One event of every kind that records who asked, appended in today's
    // shape and then put back into the shape the previous release wrote.
    let log = SessionId::new("by-universe-key");
    let run_id = RunId::new(1);
    let asked_by = Some(harness::Attribution::Key {
        key_id: "placeholder".to_owned(),
        scope_kind: AttributedKeyScope::Universe,
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
            HERE,
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
            DEPLOYMENT,
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
            ELSEWHERE,
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
            REPLACED,
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
            HERE,
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
        .bind(stale(prefix))
        .execute(pool)
        .await
        .expect("restore the previous event shape");
    }

    // The previous shape is refused, which is why it has to be rewritten.
    assert!(
        PgApiKeyStore::new(pool.clone())
            .list_api_keys(None)
            .await
            .is_err()
    );

    apply_migration(pool, 21).await;
    // Applying it again finds nothing left to rewrite.
    apply_migration(pool, 21).await;

    let keys = PgApiKeyStore::new(pool.clone())
        .list_api_keys(None)
        .await
        .expect("every key decodes");
    let minted_by = |key_id: &str, scope: AccessScope| {
        keys.iter()
            .find(|key| key.key_id == key_id && key.scope == scope)
            .unwrap_or_else(|| panic!("key {key_id} is listed"))
            .created_by
            .clone()
    };
    assert_eq!(
        minted_by("platform", AccessScope::Deployment),
        Attribution::Local
    );
    assert_eq!(
        minted_by("ci", AccessScope::Universe { universe_id: here }),
        key("platform", AccessScopeKind::Deployment)
    );
    // A universe key cannot have minted a key, whatever prefix was recorded.
    assert_eq!(
        minted_by(
            "ci",
            AccessScope::Universe {
                universe_id: elsewhere
            }
        ),
        unresolved(HERE)
    );
    assert_eq!(
        minted_by("pipeline", AccessScope::Deployment),
        unresolved(REPLACED)
    );

    let access = PgAccessStore::new(pool.clone());
    for (session, expected) in [
        (
            "by-universe-key",
            Some(key("ci", AccessScopeKind::Universe)),
        ),
        (
            "by-deployment-key",
            Some(key("platform", AccessScopeKind::Deployment)),
        ),
        // Another universe's key cannot have acted here.
        ("by-another-universes-key", Some(unresolved(ELSEWHERE))),
        ("by-replaced-key", Some(unresolved(REPLACED))),
        ("by-actor", Some(Attribution::Actor { id: "alice".into() })),
        ("unstamped", None),
    ] {
        let read = access
            .session_access(here, session)
            .await
            .expect("session access decodes")
            .expect("session exists");
        assert_eq!(read.audience.created_by, expected, "{session}");
    }

    for (table, select) in [
        (
            "auth_grants",
            "SELECT created_by FROM auth_grants WHERE universe_id = $1 AND grant_id = $2",
        ),
        (
            "auth_flows",
            "SELECT created_by FROM auth_flows WHERE universe_id = $1 AND flow_id = $2",
        ),
        (
            "bots",
            "SELECT created_by FROM bots WHERE universe_id = $1 AND bot_id = $2",
        ),
    ] {
        for ((id, _), expected) in OWNED.iter().zip([
            key("ci", AccessScopeKind::Universe),
            key("platform", AccessScopeKind::Deployment),
            unresolved(ELSEWHERE),
            unresolved(REPLACED),
        ]) {
            let stored: Value = sqlx::query_scalar(select)
                .bind(here)
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("read the stored attribution");
            assert_eq!(
                serde_json::from_value::<Attribution>(stored).expect("the attribution decodes"),
                expected,
                "{table} {id}"
            );
        }
    }

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
    let recorded_key = |key_id: &str, scope_kind| {
        Some(harness::Attribution::Key {
            key_id: key_id.to_owned(),
            scope_kind,
        })
    };
    let recorded_unresolved = |prefix: &str| {
        Some(harness::Attribution::Internal {
            component: "migration".to_owned(),
            cause: format!("API key with secret prefix {prefix}, replaced since"),
        })
    };
    assert_eq!(
        recorded,
        [
            recorded_key("ci", AttributedKeyScope::Universe),
            recorded_key("platform", AttributedKeyScope::Deployment),
            recorded_unresolved(ELSEWHERE),
            recorded_unresolved(REPLACED),
            recorded_key("ci", AttributedKeyScope::Universe),
            Some(harness::Attribution::Actor {
                id: "alice".to_owned()
            }),
        ]
    );
}

/// Run one embedded migration's statements over the schema as it stands. The
/// ledger is left alone, so migrations added later do not affect this.
async fn apply_migration(pool: &sqlx::PgPool, version: i64) {
    let migration = store_pg::MIGRATIONS
        .iter()
        .find(|migration| migration.version == version)
        .expect("embedded migration");
    pool.execute(sqlx::raw_sql(migration.sql))
        .await
        .expect("apply migration");
}
