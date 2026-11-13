use harness::{storage::*, *};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use store_pg::{
    PgStore, PgStoreConfig,
    changes::{ChangeBus, ChangeKind},
};
use uuid::Uuid;

async fn store() -> PgStore {
    let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .expect("source scripts/dev/env.sh; PostgreSQL is required");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect PostgreSQL");
    PgStore::migrate(&pool).await.expect("migrate");
    let store = PgStore::new(pool, PgStoreConfig::new(Uuid::new_v4()));
    store.ensure_universe().await.unwrap();
    store
}

fn create(id: &str) -> CreateSession {
    CreateSession {
        session_id: SessionId::new(id),
        display_name: None,
        metadata: Default::default(),
        origin: None,
        delete_after_close_ms: None,
        created_at_ms: 1,
    }
}
fn event(at: u64, event: CoreAgentEvent) -> UncommittedStoredEvent {
    CoreAgentCodec
        .encode_uncommitted(&UncommittedCoreAgentEvent {
            observed_at_ms: at,
            joins: Default::default(),
            event,
        })
        .unwrap()
}
fn config() -> SessionConfig {
    SessionConfig {
        model: ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "test".into(),
            model: "test".into(),
        },
        generation: Default::default(),
        limits: Default::default(),
        context: Default::default(),
        features: FeaturesConfig {
            vfs: Some(VfsFeature {
                workspaces: vec![WorkspaceAttachment {
                    path: "/work".into(),
                    target: WorkspaceAttachmentTarget::Workspace {
                        workspace_id: "workspace".into(),
                    },
                    access: WorkspaceAccess::Read,
                }],
                ..Default::default()
            }),
            environments: Some(EnvironmentsFeature {
                environments: vec![EnvironmentAttachment {
                    environment_id: "machine".into(),
                    access: EnvironmentAccess::Read,
                    working_directory: None,
                    default: false,
                }],
                ..Default::default()
            }),
            mcp: Some(McpFeature {
                servers: vec![McpServerAttachment {
                    server_id: "server".into(),
                    tools: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    }
}
fn message(
    blob: BlobRef,
    role: ContextMessageRole,
    preview: Option<&str>,
    source: ContextEntrySource,
) -> CoreAgentEvent {
    CoreAgentEvent::Context(ContextEvent::EntriesApplied {
        base_revision: 0,
        entries: vec![ContextEntry {
            origin: Some("user:one".into()),
            entry_id: ContextEntryId::new(1),
            key: None,
            kind: ContextEntryKind::Message { role },
            source,
            content: ContentRef {
                content_ref: blob,
                media_type: Some("text/plain".into()),
                provider_kind: None,
            },
            preview: preview.map(str::to_owned),
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }],
    })
}
async fn append(
    store: &dyn SessionStore,
    id: &SessionId,
    entries: Vec<UncommittedStoredEvent>,
) -> SessionRecord {
    let head = store.head(id).await.unwrap();
    store
        .append(AppendSessionEvents {
            session_id: id.clone(),
            expected_head: head,
            events: entries,
        })
        .await
        .unwrap();
    store.load_session(id).await.unwrap().unwrap()
}
async fn cleanup(store: &PgStore) {
    store_pg::delete_universe(store.pool(), store.config().universe_id)
        .await
        .unwrap();
    store.pool().close().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn summaries_match_memory_and_replay_and_preserve_nonvisible_revision() {
    let pg = store().await;
    let blobs = InMemoryBlobStore::new();
    let memory = InMemorySessionStore::new().with_blobs(blobs.clone());
    let id = SessionId::new("summary");
    pg.create_session(create(id.as_str())).await.unwrap();
    memory.create_session(create(id.as_str())).await.unwrap();
    let text = b"# Hello **world**\n[snake_case](https://example.test)".to_vec();
    let reference = pg.put_bytes(text.clone()).await.unwrap();
    assert_eq!(blobs.put_bytes(text).await.unwrap(), reference);
    let events = vec![
        event(
            10,
            CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
        ),
        event(
            20,
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentSet {
                environment_id: EnvironmentId::new("machine"),
            }),
        ),
        event(
            30,
            CoreAgentEvent::Run(RunEvent::Started {
                run_id: RunId::new(1),
            }),
        ),
        event(
            40,
            message(
                reference.clone(),
                ContextMessageRole::User,
                None,
                ContextEntrySource::RunInput {
                    run_id: RunId::new(1),
                    input_index: 0,
                },
            ),
        ),
        event(
            50,
            CoreAgentEvent::Tool(ToolEvent::BatchParked {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                suspension: ToolBatchSuspension::AwaitTool {
                    call_id: ToolCallId::new("call"),
                    spec: AwaitSpec {
                        promise_ids: vec![],
                        mode: AwaitMode::All,
                        deadline_at_ms: Some(100),
                    },
                },
            }),
        ),
    ];
    let parked = append(&pg, &id, events.clone()).await;
    let mem = append(&memory, &id, events.clone()).await;
    assert_eq!(parked, mem);
    assert_eq!(parked.summary.activity_phase.as_deref(), Some("awaiting"));
    assert_eq!(parked.summary.activity_since_ms, Some(50));
    assert_eq!(
        parked.summary.last_message_preview.as_deref(),
        Some("Hello world snake_case")
    );
    assert_eq!(parked.summary.attachments.workspaces, vec!["workspace"]);
    assert_eq!(parked.summary.attachments.environments, vec!["machine"]);
    assert_eq!(parked.summary.attachments.mcp_servers, vec!["server"]);
    let summary_version: String = sqlx::query_scalar(
        "SELECT xmin::text FROM session_summary WHERE universe_id=$1 AND session_id=$2",
    )
    .bind(pg.config().universe_id)
    .bind(id.as_str())
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let log_only = event(
        55,
        CoreAgentEvent::Run(RunEvent::CancellationRequested {
            run_id: RunId::new(1),
            requested_by: None,
        }),
    );
    let advanced = append(&pg, &id, vec![log_only.clone()]).await;
    assert_eq!(advanced.summary, parked.summary);
    assert_eq!(advanced.revision, parked.revision);
    let same_version: String = sqlx::query_scalar(
        "SELECT xmin::text FROM session_summary WHERE universe_id=$1 AND session_id=$2",
    )
    .bind(pg.config().universe_id)
    .bind(id.as_str())
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(
        summary_version, same_version,
        "log-only appends must not rewrite the summary"
    );
    let ignored = event(
        60,
        message(
            reference.clone(),
            ContextMessageRole::Assistant,
            Some("internal summary"),
            ContextEntrySource::Runtime {
                label: "compaction".into(),
            },
        ),
    );
    let unchanged = append(&pg, &id, vec![ignored.clone()]).await;
    assert_eq!(unchanged.revision, parked.revision);
    assert_eq!(unchanged.summary, parked.summary);
    let terminal = vec![
        event(
            70,
            CoreAgentEvent::Tool(ToolEvent::BatchUnparked {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
            }),
        ),
        event(
            80,
            message(
                reference,
                ContextMessageRole::Assistant,
                Some("**Done**"),
                ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
            ),
        ),
        event(
            90,
            CoreAgentEvent::Run(RunEvent::Failed {
                run_id: RunId::new(1),
                failure: RunFailure {
                    kind: RunFailureKind::ModelFailure,
                    message_ref: None,
                },
            }),
        ),
    ];
    let done = append(&pg, &id, terminal.clone()).await;
    assert_eq!(done.activity, SessionActivity::Idle);
    assert_eq!(done.summary.activity_phase, None);
    assert_eq!(done.summary.activity_since_ms, None);
    assert_eq!(
        done.summary.last_run.as_ref().unwrap().failure_kind,
        Some(RunFailureKind::ModelFailure)
    );
    let replay = InMemorySessionStore::new().with_blobs(blobs);
    replay.create_session(create(id.as_str())).await.unwrap();
    let replayed = append(
        &replay,
        &id,
        events
            .into_iter()
            .chain([log_only, ignored])
            .chain(terminal)
            .collect(),
    )
    .await;
    assert_eq!(done, replayed);
    let listed = pg
        .list_sessions(ListSessions {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.sessions[0], done);
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn recovery_is_a_barrier_to_stale_failure_observations() {
    use harness::storage::summary::ExecutionFailure;
    let pg = store().await;
    let id = SessionId::new("failure");
    pg.create_session(create(id.as_str())).await.unwrap();
    let running = append(
        &pg,
        &id,
        vec![
            event(
                1,
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
            ),
            event(
                2,
                CoreAgentEvent::Run(RunEvent::Started {
                    run_id: RunId::new(1),
                }),
            ),
        ],
    )
    .await;
    assert!(
        pg.record_execution_failure(
            &id,
            Some(running.revision),
            Some(ExecutionFailure::Failed),
            3
        )
        .await
        .unwrap()
    );
    let failed = pg.load_session(&id).await.unwrap().unwrap();
    assert_eq!(
        failed.summary.execution_failure,
        Some(ExecutionFailure::Failed)
    );
    assert!(
        pg.record_execution_failure(&id, None, None, 4)
            .await
            .unwrap()
    );
    assert!(
        !pg.record_execution_failure(
            &id,
            Some(failed.revision),
            Some(ExecutionFailure::Failed),
            5
        )
        .await
        .unwrap()
    );
    assert_eq!(
        pg.load_session(&id)
            .await
            .unwrap()
            .unwrap()
            .summary
            .execution_failure,
        None
    );
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn independent_publishers_wake_listeners_and_rollback_publishes_nothing() {
    let pg = store().await;
    let application = format!("summary-listener-{}", Uuid::new_v4());
    let options = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .unwrap()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .application_name(&application);
    let reader_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let reader = ChangeBus::postgres(reader_pool.clone());
    let writer = ChangeBus::postgres(pg.pool().clone());
    let mut ready = reader.subscribe_all();
    // A committed LISTEN emits a reset. This fences listener initialization.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(ready.recv().await, Ok(None)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let mut observed = reader.subscribe(pg.config().universe_id, ChangeKind::Session, "remote");
    let pg = pg.with_changes(writer);
    pg.create_session(create("remote")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), observed.changed())
        .await
        .unwrap()
        .unwrap();
    // Drain any initialization reset; then subscribe specifically after it.
    let local = PgStore::new(pg.pool().clone(), pg.config().clone());
    let mut rejected =
        local
            .changes()
            .subscribe(pg.config().universe_id, ChangeKind::Session, "remote");
    let result = local
        .append(AppendSessionEvents {
            session_id: SessionId::new("remote"),
            expected_head: Some(SessionPosition {
                seq: EventSeq::new(99),
            }),
            events: vec![],
        })
        .await;
    assert!(matches!(
        result,
        Err(SessionStoreError::ExpectedHeadMismatch { .. })
    ));
    assert!(!rejected.has_changed().unwrap());
    store_pg::changes::wait_for_change(&mut rejected, Duration::from_millis(10)).await;
    let mut reconnect = reader.subscribe_all();
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name=$1 AND query LIKE 'LISTEN%' LIMIT 1")
        .bind(application).fetch_one(pg.pool()).await.unwrap();
    assert!(terminated);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(reconnect.recv().await, Ok(None)) {
                break;
            }
        }
    })
    .await
    .expect("reconnect must invalidate all cached observations");
    reader_pool.close().await;
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn fork_and_clone_project_their_own_history_and_inherited_retention_invalidates() {
    let pg = store().await;
    let id = SessionId::new("source");
    pg.create_session(create(id.as_str())).await.unwrap();
    append(
        &pg,
        &id,
        vec![
            event(
                1,
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
            ),
            event(
                2,
                CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentSet {
                    environment_id: EnvironmentId::new("first"),
                }),
            ),
        ],
    )
    .await;
    append(
        &pg,
        &id,
        vec![event(
            3,
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentSet {
                environment_id: EnvironmentId::new("second"),
            }),
        )],
    )
    .await;
    let fork = pg
        .create_forked_session(CreateForkedSession {
            source_session_id: id.clone(),
            session_id: SessionId::new("fork"),
            source_seq: EventSeq::new(2),
            created_at_ms: 4,
        })
        .await
        .unwrap();
    assert_eq!(fork.summary.active_environment_id.as_deref(), Some("first"));
    assert_eq!(fork.summary.attachments.workspaces, vec!["workspace"]);
    let clone = pg
        .create_cloned_session(CreateClonedSession {
            source_session_id: id.clone(),
            session_id: SessionId::new("clone"),
            created_at_ms: 5,
            opening_events: vec![event(
                5,
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
            )],
        })
        .await
        .unwrap();
    assert_eq!(clone.summary.active_environment_id, None);
    assert_eq!(clone.summary.attachments.mcp_servers, vec!["server"]);
    let mut wake = pg
        .changes()
        .subscribe(pg.config().universe_id, ChangeKind::Session, "fork");
    pg.set_session_retention(&id, Some(100)).await.unwrap();
    assert!(wake.has_changed().unwrap());
    wake.changed().await.unwrap();
    assert!(
        pg.load_session(&fork.session_id)
            .await
            .unwrap()
            .unwrap()
            .revision
            > fork.revision
    );
    let mut feed = pg.changes().subscribe_all();
    append(
        &pg,
        &id,
        vec![event(
            6,
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentCleared),
        )],
    )
    .await;
    let visible = feed.recv().await.unwrap().unwrap();
    assert!(visible.rev.is_some());
    append(
        &pg,
        &id,
        vec![event(
            7,
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentCleared),
        )],
    )
    .await;
    let log_only = feed.recv().await.unwrap().unwrap();
    assert_eq!(log_only.rev, None);
    assert!(log_only.seq.is_some());
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn unavailable_or_empty_message_keeps_the_entire_previous_preview() {
    let pg = store().await;
    let id = SessionId::new("preview");
    pg.create_session(create(id.as_str())).await.unwrap();
    let reference = pg.put_bytes(b"hello".to_vec()).await.unwrap();
    let initial = append(
        &pg,
        &id,
        vec![event(
            1,
            message(
                reference.clone(),
                ContextMessageRole::User,
                None,
                ContextEntrySource::ContextEdit,
            ),
        )],
    )
    .await;
    let object_store = PgStore::with_object_store(
        pg.pool().clone(),
        std::sync::Arc::new(object_store::memory::InMemory::new()),
        pg.config().clone().with_inline_threshold_bytes(1),
    );
    let remote = object_store
        .put_bytes(b"object storage content".to_vec())
        .await
        .unwrap();
    let after = append(
        &pg,
        &id,
        vec![
            event(
                2,
                message(
                    remote,
                    ContextMessageRole::User,
                    None,
                    ContextEntrySource::ContextEdit,
                ),
            ),
            event(
                3,
                message(
                    reference,
                    ContextMessageRole::Assistant,
                    Some("```code only"),
                    ContextEntrySource::AssistantOutput {
                        run_id: RunId::new(1),
                        turn_id: TurnId::new(1),
                    },
                ),
            ),
        ],
    )
    .await;
    assert_eq!(initial.summary, after.summary);
    assert_eq!(initial.revision, after.revision);
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn bot_status_and_counts_follow_persistent_idle_summaries_and_reject_old_observations() {
    use bots::{BotStatusProjection, BotStore};
    let pg = store().await;
    let universe = pg.config().universe_id;
    let bot_id = api::BotId::new("helper");
    let mut changed = pg
        .changes()
        .subscribe(universe, ChangeKind::Bot, bot_id.as_str());
    pg.create_bot(
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
    .await
    .unwrap();
    assert!(changed.has_changed().unwrap());
    changed.borrow_and_update();
    let ready = BotStatusProjection {
        setup: api::BotSetupStatus::Ready,
        ..Default::default()
    };
    pg.record_bot_status(&bot_id, &ready, 10).await.unwrap();
    let id = SessionId::new("bot:v1:helper:main");
    pg.create_session(create(id.as_str())).await.unwrap();
    let access = store_pg::PgAccessStore::new(pg.pool().clone()).with_changes(pg.changes().clone());
    access
        .stamp_session(
            universe,
            id.as_str(),
            &api::Attribution::Internal {
                component: "bot".into(),
                cause: "test".into(),
            },
            None,
            Some(bot_id.as_str()),
        )
        .await
        .unwrap();
    append(
        &pg,
        &id,
        vec![event(
            2,
            CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
        )],
    )
    .await;
    let roster = pg
        .bot_roster_filtered(None, Some(bot_id.as_str()))
        .await
        .unwrap();
    assert_eq!(roster[0].0.open_session_count, 1);
    assert_eq!(roster[0].0.active_session_count, 0);
    assert_eq!(roster[0].0.status, api::BotListStatus::Idle);
    changed.borrow_and_update();
    append(
        &pg,
        &id,
        vec![event(
            3,
            CoreAgentEvent::Run(RunEvent::Started {
                run_id: RunId::new(1),
            }),
        )],
    )
    .await;
    assert!(changed.has_changed().unwrap());
    let roster = pg
        .bot_roster_filtered(None, Some(bot_id.as_str()))
        .await
        .unwrap();
    assert_eq!(roster[0].0.active_session_count, 1);
    assert_eq!(roster[0].0.status, api::BotListStatus::Working);
    append(
        &pg,
        &id,
        vec![event(
            4,
            CoreAgentEvent::Approval(ApprovalEvent::RunParked {
                run_id: RunId::new(1),
            }),
        )],
    )
    .await;
    let degraded = BotStatusProjection {
        setup: api::BotSetupStatus::Degraded,
        ..ready.clone()
    };
    pg.record_bot_status(&bot_id, &degraded, 11).await.unwrap();
    assert_eq!(
        pg.bot_roster_filtered(None, Some(bot_id.as_str()))
            .await
            .unwrap()[0]
            .0
            .status,
        api::BotListStatus::Waiting
    );
    append(
        &pg,
        &id,
        vec![event(
            5,
            CoreAgentEvent::Run(RunEvent::Completed {
                run_id: RunId::new(1),
                output: None,
            }),
        )],
    )
    .await;
    let roster = pg
        .bot_roster_filtered(None, Some(bot_id.as_str()))
        .await
        .unwrap();
    assert_eq!(roster[0].0.active_session_count, 0);
    assert_eq!(roster[0].0.status, api::BotListStatus::Attention);
    pg.record_bot_status(&bot_id, &ready, 20).await.unwrap();
    pg.record_bot_status(&bot_id, &ready, 30).await.unwrap();
    pg.record_bot_status(&bot_id, &degraded, 25).await.unwrap();
    assert_eq!(
        pg.bot_roster_filtered(None, Some(bot_id.as_str()))
            .await
            .unwrap()[0]
            .0
            .status,
        api::BotListStatus::Idle
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM session_summary WHERE universe_id=$1")
            .bind(universe)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(count, 1, "idle summaries remain available");
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn catalog_changes_and_inherited_access_publish_visible_revisions() {
    let pg = store().await;
    let universe = pg.config().universe_id;
    let root = pg.create_session(create("root")).await.unwrap();
    let mut child = create("child");
    child.origin = Some(SessionOrigin {
        kind: SessionOriginKind::Subagent,
        parent_session_id: root.session_id.clone(),
        parent_run_id: 1,
        root_session_id: root.session_id.clone(),
        depth: 1,
        invocation_id: "call".into(),
        profile_id: "child".into(),
        profile_revision: 1,
        limits: SubagentLimits::default(),
    });
    let child = pg.create_session(child).await.unwrap();
    let mut root_changes = pg
        .changes()
        .subscribe(universe, ChangeKind::Session, "root");
    let mut child_changes = pg
        .changes()
        .subscribe(universe, ChangeKind::Session, "child");
    let access = store_pg::PgAccessStore::new(pg.pool().clone()).with_changes(pg.changes().clone());
    access
        .stamp_session(
            universe,
            "root",
            &api::Attribution::Actor { id: "alice".into() },
            Some(api::Visibility::Restricted),
            None,
        )
        .await
        .unwrap();
    assert!(root_changes.has_changed().unwrap());
    assert!(child_changes.has_changed().unwrap());
    root_changes.borrow_and_update();
    child_changes.borrow_and_update();
    assert!(access.share_session(universe, "root").await.unwrap());
    assert!(root_changes.has_changed().unwrap());
    assert!(child_changes.has_changed().unwrap());
    let after = pg.load_session(&child.session_id).await.unwrap().unwrap();
    assert_eq!(after.revision, child.revision + 2);
    root_changes.borrow_and_update();
    child_changes.borrow_and_update();
    assert!(!access.share_session(universe, "root").await.unwrap());
    assert!(!root_changes.has_changed().unwrap());
    assert!(!child_changes.has_changed().unwrap());
    pg.set_session_display_name(&root.session_id, Some("renamed".into()))
        .await
        .unwrap();
    assert!(root_changes.has_changed().unwrap());
    root_changes.borrow_and_update();
    pg.set_session_metadata(&root.session_id, [("key".into(), "value".into())].into())
        .await
        .unwrap();
    assert!(root_changes.has_changed().unwrap());
    root_changes.borrow_and_update();
    let before = pg.load_session(&root.session_id).await.unwrap().unwrap();
    let version: String = sqlx::query_scalar(
        "SELECT xmin::text FROM sessions WHERE universe_id=$1 AND session_id=$2",
    )
    .bind(universe)
    .bind(root.session_id.as_str())
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let renamed = pg
        .set_session_display_name(&root.session_id, Some("renamed".into()))
        .await
        .unwrap();
    let same = pg
        .set_session_metadata(&root.session_id, [("key".into(), "value".into())].into())
        .await
        .unwrap();
    assert_eq!(renamed, before);
    assert_eq!(same, before);
    assert!(
        !root_changes.has_changed().unwrap(),
        "no-op catalog updates must not invalidate lists"
    );
    let after_version: String = sqlx::query_scalar(
        "SELECT xmin::text FROM sessions WHERE universe_id=$1 AND session_id=$2",
    )
    .bind(universe)
    .bind(root.session_id.as_str())
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(
        version, after_version,
        "no-op catalog updates must not rewrite the row"
    );
    pg.create_session(create("unused")).await.unwrap();
    let discarded = pg
        .changes()
        .subscribe(universe, ChangeKind::Session, "unused");
    assert!(
        pg.discard_unopened_session(&SessionId::new("unused"))
            .await
            .unwrap()
    );
    assert!(discarded.has_changed().unwrap());
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn mutations_preserve_summary_committed_while_waiting_for_catalog_lock() {
    use harness::storage::summary::ExecutionFailure;
    let pg = store().await;
    for operation in ["append", "rename", "metadata", "recovery"] {
        let id = SessionId::new(format!("blocked-{operation}"));
        pg.create_session(create(id.as_str())).await.unwrap();
        let opened = append(
            &pg,
            &id,
            vec![event(
                2,
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
            )],
        )
        .await;
        let initial_revision = opened.revision;
        let application = format!("summary-lock-{}", Uuid::new_v4());
        let options = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
            .unwrap()
            .parse::<sqlx::postgres::PgConnectOptions>()
            .unwrap()
            .application_name(&application);
        let writer_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let writer = PgStore::new(writer_pool.clone(), pg.config().clone());
        let mut tx = pg.pool().begin().await.unwrap();
        sqlx::query(
            "UPDATE sessions SET revision=revision+1 WHERE universe_id=$1 AND session_id=$2",
        )
        .bind(pg.config().universe_id)
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("UPDATE session_summary SET summary_json=summary_json || $3 WHERE universe_id=$1 AND session_id=$2")
            .bind(pg.config().universe_id).bind(id.as_str())
            .bind(serde_json::json!({"execution_failure":"failed","active_environment_id":"concurrent-machine"}))
            .execute(&mut *tx).await.unwrap();
        let target = id.clone();
        let pending = tokio::spawn(async move {
            match operation {
                "append" => {
                    writer
                        .append(AppendSessionEvents {
                            session_id: target.clone(),
                            expected_head: opened.head,
                            events: vec![event(
                                3,
                                CoreAgentEvent::Environment(
                                    EnvironmentEvent::ActiveEnvironmentSet {
                                        environment_id: EnvironmentId::new("new-machine"),
                                    },
                                ),
                            )],
                        })
                        .await?;
                }
                "rename" => {
                    return writer
                        .set_session_display_name(&target, Some("renamed".into()))
                        .await;
                }
                "metadata" => {
                    return writer
                        .set_session_metadata(&target, [("key".into(), "value".into())].into())
                        .await;
                }
                "recovery" => {
                    writer
                        .record_execution_failure(&target, None, None, 3)
                        .await?;
                }
                _ => unreachable!(),
            }
            Ok(writer.load_session(&target).await?.unwrap())
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock')")
                    .bind(&application).fetch_one(pg.pool()).await.unwrap();
                if blocked { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("mutation must wait behind the existing transaction");
        tx.commit().await.unwrap();
        let returned = pending.await.unwrap().unwrap();
        let stored = pg.load_session(&id).await.unwrap().unwrap();
        assert_eq!(
            returned, stored,
            "{operation} must return a coherent projection"
        );
        assert_eq!(stored.revision, initial_revision + 2);
        assert_eq!(
            stored.summary.execution_failure,
            (operation != "recovery").then_some(ExecutionFailure::Failed)
        );
        assert_eq!(
            stored.summary.active_environment_id.as_deref(),
            Some(if operation == "append" {
                "new-machine"
            } else {
                "concurrent-machine"
            })
        );
        writer_pool.close().await;
    }
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn unopened_cleanup_preserves_children_and_invalidates_inherited_bot_counts() {
    let pg = store().await;
    let memory = InMemorySessionStore::new();
    let root = SessionId::new("unopened-root");
    let child = SessionId::new("unopened-child");
    let fork = SessionId::new("empty-fork");
    for store in [&pg as &dyn SessionStore, &memory] {
        store.create_session(create(root.as_str())).await.unwrap();
        let mut request = create(child.as_str());
        request.origin = Some(SessionOrigin {
            kind: SessionOriginKind::Subagent,
            parent_session_id: root.clone(),
            parent_run_id: 1,
            root_session_id: root.clone(),
            depth: 1,
            invocation_id: "call".into(),
            profile_id: "child".into(),
            profile_revision: 1,
            limits: SubagentLimits::default(),
        });
        store.create_session(request).await.unwrap();
        assert!(!store.discard_unopened_session(&root).await.unwrap());
        assert!(store.load_session(&child).await.unwrap().is_some());
        store
            .create_forked_session(CreateForkedSession {
                session_id: fork.clone(),
                source_session_id: root.clone(),
                source_seq: EventSeq::new(0),
                created_at_ms: 2,
            })
            .await
            .unwrap();
    }
    let access = store_pg::PgAccessStore::new(pg.pool().clone()).with_changes(pg.changes().clone());
    access
        .stamp_session(
            pg.config().universe_id,
            root.as_str(),
            &api::Attribution::Internal {
                component: "bot".into(),
                cause: "test".into(),
            },
            None,
            Some("helper"),
        )
        .await
        .unwrap();
    let mut bot_changes =
        pg.changes()
            .subscribe(pg.config().universe_id, ChangeKind::Bot, "helper");
    assert!(pg.discard_unopened_session(&child).await.unwrap());
    assert!(bot_changes.has_changed().unwrap());
    bot_changes.borrow_and_update();
    assert!(
        !pg.discard_unopened_session(&root).await.unwrap(),
        "an empty history fork is still a dependent child"
    );
    assert!(!bot_changes.has_changed().unwrap());
    assert!(pg.discard_unopened_session(&fork).await.unwrap());
    assert!(pg.discard_unopened_session(&root).await.unwrap());
    assert!(bot_changes.has_changed().unwrap());
    assert!(memory.discard_unopened_session(&child).await.unwrap());
    assert!(!memory.discard_unopened_session(&root).await.unwrap());
    assert!(memory.discard_unopened_session(&fork).await.unwrap());
    assert!(memory.discard_unopened_session(&root).await.unwrap());
    cleanup(&pg).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local PostgreSQL"]
async fn branch_validation_rejects_a_gap_inside_the_inherited_prefix() {
    let pg = store().await;
    let id = SessionId::new("gapped-history");
    pg.create_session(create(id.as_str())).await.unwrap();
    append(
        &pg,
        &id,
        vec![
            event(
                2,
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config: config() }),
            ),
            event(
                3,
                CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentSet {
                    environment_id: EnvironmentId::new("machine"),
                }),
            ),
            event(
                4,
                CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentCleared),
            ),
        ],
    )
    .await;
    sqlx::query("DELETE FROM session_events WHERE universe_id=$1 AND session_id=$2 AND seq=2")
        .bind(pg.config().universe_id)
        .bind(id.as_str())
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(matches!(
        pg.safe_fork_seq(&id).await,
        Err(SessionStoreError::Store { .. })
    ));
    let result = pg
        .create_forked_session(CreateForkedSession {
            session_id: SessionId::new("rejected-fork"),
            source_session_id: id,
            source_seq: EventSeq::new(3),
            created_at_ms: 5,
        })
        .await;
    assert!(matches!(result, Err(SessionStoreError::Store { .. })));
    assert!(
        pg.load_session(&SessionId::new("rejected-fork"))
            .await
            .unwrap()
            .is_none()
    );
    cleanup(&pg).await;
}
