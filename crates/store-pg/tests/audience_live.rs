//! Session and bot access facts against an isolated schema. Never loads .env.
use std::{panic::AssertUnwindSafe, str::FromStr};

use api::{Attribution, ResourceRef, Visibility};
use engine::StoredEvent;
use engine::storage::{
    AppendSessionEvents, CreateSession, ListSessions, SessionActivity, SessionOrigin,
    SessionOriginKind, SessionStore as _, UncommittedStoredEvent,
};
use engine::{SessionId, SubagentLimits};
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{AccessFilter, PgAccessStore, PgStore, PgStoreConfig};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; creates an isolated schema"]
async fn sessions_record_their_creator_and_children_follow_their_root() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("lightspeed_audience_test_{}", Uuid::new_v4().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    let outcome = AssertUnwindSafe(exercise(&pool)).catch_unwind().await;
    pool.close().await;
    admin
        .execute(format!("DROP SCHEMA \"{schema}\" CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn create(store: &PgStore, id: &str, parent: Option<&str>) {
    store
        .create_session(CreateSession {
            session_id: SessionId::new(id),
            display_name: None,
            metadata: Default::default(),
            origin: parent.map(|parent| SessionOrigin {
                kind: SessionOriginKind::Subagent,
                parent_session_id: SessionId::new(parent),
                parent_run_id: 1,
                root_session_id: SessionId::new(parent),
                depth: 1,
                invocation_id: format!("call-{id}"),
                profile_id: "child".into(),
                profile_revision: 1,
                limits: SubagentLimits::default(),
            }),
            delete_after_close_ms: None,
            created_at_ms: 1,
        })
        .await
        .unwrap();
}

async fn listed(store: &PgStore, filter: AccessFilter) -> Vec<String> {
    listed_where(store, filter, ListSessions::default()).await
}

async fn listed_where(store: &PgStore, filter: AccessFilter, request: ListSessions) -> Vec<String> {
    let mut ids: Vec<String> = store
        .list_sessions_for(
            ListSessions {
                limit: 100,
                ..request
            },
            &filter,
        )
        .await
        .unwrap()
        .sessions
        .into_iter()
        .map(|(record, _)| record.session_id.as_str().to_owned())
        .collect();
    ids.sort();
    ids
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    let universe = Uuid::new_v4();
    let sessions = PgStore::new(pool.clone(), PgStoreConfig::new(universe));
    sessions.ensure_universe().await.unwrap();
    let store = PgAccessStore::new(pool.clone());
    let alice = Attribution::Actor { id: "alice".into() };
    let bob = Attribution::Actor { id: "bob".into() };

    // A root not yet stamped reads as unshared and created by no one.
    create(&sessions, "draft", None).await;
    let access = store
        .session_access(universe, "draft")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(access.audience.visibility, Visibility::Restricted);
    assert_eq!(access.audience.created_by, None);
    // The first stamp wins; a retry keeps it.
    store
        .stamp_session(
            universe,
            "draft",
            &alice,
            Some(Visibility::Restricted),
            None,
        )
        .await
        .unwrap();
    store
        .stamp_session(universe, "draft", &bob, Some(Visibility::Universe), None)
        .await
        .unwrap();
    let access = store
        .session_access(universe, "draft")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(access.audience.visibility, Visibility::Restricted);
    assert_eq!(access.audience.created_by, Some(alice.clone()));
    assert_eq!(access.root, ResourceRef::Session("draft".into()));

    // A delegated child reads its root and names its parent.
    create(&sessions, "agent_child", Some("draft")).await;
    let child = store
        .session_access(universe, "agent_child")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.root, ResourceRef::Session("draft".into()));
    assert_eq!(child.parent.as_deref(), Some("draft"));
    assert_eq!(child.audience.created_by, Some(alice.clone()));

    // Bob's shared session and a bot's session.
    create(&sessions, "bob-shared", None).await;
    store
        .stamp_session(
            universe,
            "bob-shared",
            &bob,
            Some(Visibility::Universe),
            None,
        )
        .await
        .unwrap();
    create(&sessions, "bot:v1:helper:main", None).await;
    store
        .stamp_session(
            universe,
            "bot:v1:helper:main",
            &Attribution::Internal {
                component: "controller".into(),
                cause: "test".into(),
            },
            None,
            Some("helper"),
        )
        .await
        .unwrap();
    let bot_session = store
        .session_access(universe, "bot:v1:helper:main")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bot_session.root, ResourceRef::Bot("helper".into()));
    assert_eq!(bot_session.audience.visibility, Visibility::Universe);

    // Filters apply to each session's root.
    let everything = listed(&sessions, AccessFilter::default()).await;
    assert_eq!(everything.len(), 4);
    assert_eq!(
        listed(
            &sessions,
            AccessFilter {
                visible_to: Some("bob".into()),
                ..Default::default()
            }
        )
        .await,
        vec!["bob-shared", "bot:v1:helper:main"]
    );
    // Internal work sees shared work and its own root.
    assert_eq!(
        listed(
            &sessions,
            AccessFilter {
                within_root: Some(ResourceRef::Session("draft".into())),
                ..Default::default()
            }
        )
        .await,
        everything
    );
    assert_eq!(
        listed(
            &sessions,
            AccessFilter {
                within_root: Some(ResourceRef::Bot("helper".into())),
                ..Default::default()
            }
        )
        .await,
        vec!["bob-shared", "bot:v1:helper:main"]
    );

    // Managed work can be left out of a list or listed alone, and trees
    // listed by their roots; sub-agents follow their root either way.
    create(&sessions, "bot-child", Some("bot:v1:helper:main")).await;
    sqlx::query("UPDATE sessions SET managed = true WHERE session_id = 'bot:v1:helper:main'")
        .execute(pool)
        .await
        .unwrap();
    let lineage = |request: ListSessions| {
        let sessions = sessions.clone();
        async move { listed_where(&sessions, AccessFilter::default(), request).await }
    };
    let ids = |ids: &[&str]| ids.iter().map(|id| SessionId::new(*id)).collect::<Vec<_>>();
    assert_eq!(
        lineage(ListSessions {
            managed: Some(false),
            ..Default::default()
        })
        .await,
        vec!["agent_child", "bob-shared", "draft"]
    );
    assert_eq!(
        lineage(ListSessions {
            managed: Some(true),
            ..Default::default()
        })
        .await,
        vec!["bot-child", "bot:v1:helper:main"]
    );
    assert_eq!(
        lineage(ListSessions {
            trees: ids(&["bot:v1:helper:main", "draft"]),
            ..Default::default()
        })
        .await,
        vec!["agent_child", "bot-child", "bot:v1:helper:main", "draft"]
    );
    assert_eq!(
        lineage(ListSessions {
            trees: ids(&["draft"]),
            subagent: Some(true),
            ..Default::default()
        })
        .await,
        vec!["agent_child"]
    );
    assert_eq!(
        lineage(ListSessions {
            subagent: Some(false),
            ..Default::default()
        })
        .await,
        vec!["bob-shared", "bot:v1:helper:main", "draft"]
    );
    assert_eq!(
        lineage(ListSessions {
            parent: Some(SessionId::new("bot:v1:helper:main")),
            ..Default::default()
        })
        .await,
        vec!["bot-child"]
    );
    sqlx::query(
        "UPDATE sessions SET lifecycle_status = 'closed', closed_at_seq = 1, closed_at_ms = 1
         WHERE session_id = 'bob-shared'",
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        lineage(ListSessions {
            closed: Some(true),
            ..Default::default()
        })
        .await,
        vec!["bob-shared"]
    );
    assert_eq!(
        lineage(ListSessions {
            closed: Some(false),
            subagent: Some(false),
            ..Default::default()
        })
        .await,
        vec!["bot:v1:helper:main", "draft"]
    );
    sqlx::query(
        "UPDATE sessions SET lifecycle_status = 'new', closed_at_seq = NULL, closed_at_ms = NULL
         WHERE session_id = 'bob-shared'",
    )
    .execute(pool)
    .await
    .unwrap();

    // The row keeps what a session is doing now, for lists.
    let append = |kinds: &'static [&'static str]| {
        let sessions = sessions.clone();
        async move {
            let id = SessionId::new("bob-shared");
            let head = sessions.load_session(&id).await.unwrap().unwrap().head;
            sessions
                .append(AppendSessionEvents {
                    session_id: id.clone(),
                    expected_head: head,
                    events: kinds
                        .iter()
                        .map(|kind| UncommittedStoredEvent {
                            observed_at_ms: 5,
                            joins: Default::default(),
                            event: StoredEvent::new(*kind, 1, serde_json::json!({})),
                        })
                        .collect(),
                })
                .await
                .unwrap();
            sessions.load_session(&id).await.unwrap().unwrap().activity
        }
    };
    assert_eq!(
        append(&["lightspeed.core.run.started"]).await,
        SessionActivity::Working
    );
    assert_eq!(
        append(&[
            "lightspeed.core.approval.requested",
            "lightspeed.core.approval.run_parked"
        ])
        .await,
        SessionActivity::Waiting
    );
    assert_eq!(
        append(&[
            "lightspeed.core.approval.decided",
            "lightspeed.core.run.completed"
        ])
        .await,
        SessionActivity::Idle
    );

    // Sharing is one way and only for root sessions; children follow.
    assert!(!store.share_session(universe, "agent_child").await.unwrap());
    assert!(
        !store
            .share_session(universe, "bot:v1:helper:main")
            .await
            .unwrap()
    );
    assert!(store.share_session(universe, "draft").await.unwrap());
    assert!(!store.share_session(universe, "draft").await.unwrap());
    assert_eq!(
        store
            .session_access(universe, "agent_child")
            .await
            .unwrap()
            .unwrap()
            .audience
            .visibility,
        Visibility::Universe
    );
    assert!(
        store
            .session_access(universe, "missing")
            .await
            .unwrap()
            .is_none()
    );
}
