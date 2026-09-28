//! Live PostgreSQL coverage of the bots and channels stores. Each test owns
//! a fresh universe and deletes it on the way out.

use std::collections::BTreeMap;

use api::{
    BotDocument, BotEventOutcome, BotId, BotTriggerDisabledReason, BotTriggerDocument,
    BotTriggerId, BotTriggerKind, BotTriggerSpec, ChannelAccountDocument, ChannelAccountId,
    ChannelPairedVia, ChannelProvider, PollCursorSpec, PollCursorState, PollSource, ProfileId,
};
use bots::{
    BotError, BotEventCursor, BotEventOutcomeWrite, BotEventRateScope, BotEventRecord,
    BotEventStore, BotStore, BotTriggerSecrets, BotTriggerStore, BotTriggerWrite,
    InsertBotEventOutcome, RoutedSession, RoutedSessionClosePolicy,
};
use channels::{
    ChannelAccountStore, ChannelError, ChannelPairingFilter, ChannelPairingRecord,
    ChannelPairingStore,
};
use engine::{BlobRef, storage::BlobStore};
use sqlx::postgres::PgPoolOptions;
use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

async fn live_store() -> PgStore {
    let database_url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect(
        "LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh",
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&database_url)
        .await
        .expect("connect to live Postgres");
    PgStore::migrate(&pool).await.expect("apply migrations");
    let store = PgStore::new(pool, PgStoreConfig::new(Uuid::new_v4()));
    store.ensure_universe().await.expect("ensure test universe");
    for bytes in [b"event document".as_slice(), b"event prompt".as_slice()] {
        store
            .put_bytes(bytes.to_vec())
            .await
            .expect("put event fixture blob");
    }
    store
}

async fn drop_universe(store: &PgStore) {
    store_pg::delete_universe(store.pool(), store.config().universe_id)
        .await
        .expect("delete test universe");
}

fn bot_document(profile: &str) -> BotDocument {
    BotDocument {
        display_name: Some("Triage".to_owned()),
        description: None,
        profile_id: ProfileId::new(profile),
        brief: None,
        runs_per_day: None,
        breaker: None,
        routed_session_close_after_ms: None,
        self_config: false,
        emit: false,
        enabled: true,
    }
}

fn trigger_document(spec: BotTriggerSpec) -> BotTriggerDocument {
    BotTriggerDocument {
        spec,
        filter: None,
        route: None,
        coalesce: None,
        deliver: None,
        session_close_after_ms: None,
        enabled: true,
    }
}

fn schedule_spec() -> BotTriggerSpec {
    BotTriggerSpec::Schedule {
        cron: Some("@hourly".to_owned()),
        at_ms: None,
        timezone: "UTC".to_owned(),
        summary: "check the queue".to_owned(),
    }
}

fn poll_spec() -> BotTriggerSpec {
    BotTriggerSpec::Poll {
        source: PollSource::Http {
            url: "https://example.com/feed".to_owned(),
            method: Default::default(),
            headers: BTreeMap::new(),
            auth: None,
            body: None,
        },
        interval_ms: 60_000,
        items: None,
        cursor: PollCursorSpec::IdSet {
            id: "id".to_owned(),
        },
    }
}

fn chat_spec(account_id: &str) -> BotTriggerSpec {
    BotTriggerSpec::Chat {
        account_id: account_id.to_owned(),
        match_scope: None,
        activation: Default::default(),
        access: Default::default(),
        pairing: Default::default(),
        priority: 100,
    }
}

fn trigger_write(trigger_id: &str, document: BotTriggerDocument) -> BotTriggerWrite {
    BotTriggerWrite {
        trigger_id: BotTriggerId::new(trigger_id),
        document,
        secrets: BotTriggerSecrets::default(),
        cursor: None,
    }
}

fn event(
    bot_id: &BotId,
    event_id: &str,
    seq: u64,
    received_at_ms: i64,
    trigger_id: Option<&str>,
    sender_bot_id: Option<&str>,
) -> BotEventRecord {
    BotEventRecord {
        bot_id: bot_id.clone(),
        event_id: event_id.to_owned(),
        seq,
        trigger_id: trigger_id.map(BotTriggerId::new),
        kind: "test.event".to_owned(),
        summary: format!("event {seq}"),
        occurred_at_ms: received_at_ms,
        received_at_ms,
        document_ref: BlobRef::from_bytes(b"event document").to_string(),
        prompt_ref: Some(BlobRef::from_bytes(b"event prompt").to_string()),
        session: Some(RoutedSession {
            session_id: format!("bot:v1:{bot_id}:main"),
            label: "main".to_owned(),
            session_key: None,
            close_policy: RoutedSessionClosePolicy::Inherit,
        }),
        sender_bot_id: sender_bot_id.map(BotId::new),
        hops: 1,
        in_reply_to: None,
        media: Vec::new(),
        receiver: None,
        outcome: None,
        outcome_detail: None,
        run_id: None,
        resolved_at_ms: None,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres"]
async fn pg_live_bot_create_put_close_delete() {
    let store = live_store().await;
    let bot_id = BotId::new("triage");

    let created = store
        .create_bot(bot_id.clone(), bot_document("triage"), 10)
        .await
        .expect("create bot");
    assert_eq!(created.revision, 1);
    assert_eq!(created.event_seq, 0);
    assert_eq!(created.closed_at_ms, None);
    assert!(created.closed_sessions.is_empty());
    assert_eq!((created.created_at_ms, created.updated_at_ms), (10, 10));
    assert!(matches!(
        store
            .create_bot(bot_id.clone(), bot_document("triage"), 11)
            .await,
        Err(BotError::BotAlreadyExists { .. })
    ));

    // Put replaces whole and bumps; the guard is enforced against the row.
    let mut edited = bot_document("triage");
    edited.brief = Some("watch the queue".to_owned());
    let put = store
        .put_bot(bot_id.clone(), edited.clone(), Some(1), 20)
        .await
        .expect("put bot");
    assert_eq!(put.revision, 2);
    assert_eq!(put.document, edited);
    assert_eq!(put.created_at_ms, 10);
    assert_eq!(put.updated_at_ms, 20);
    assert!(matches!(
        store
            .put_bot(bot_id.clone(), edited.clone(), Some(1), 21)
            .await,
        Err(BotError::BotRevisionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    let unguarded = store
        .put_bot(bot_id.clone(), edited.clone(), None, 22)
        .await
        .expect("unguarded put");
    assert_eq!(unguarded.revision, 3);

    // Put creates when absent (the expected revision is not checked then).
    let other = store
        .put_bot(BotId::new("other"), bot_document("other"), Some(7), 23)
        .await
        .expect("put creates");
    assert_eq!(other.revision, 1);

    assert_eq!(
        store
            .list_bots()
            .await
            .expect("list bots")
            .iter()
            .map(|bot| bot.bot_id.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["other", "triage"]
    );
    assert_eq!(
        store
            .list_bots_for_profile(&ProfileId::new("triage"))
            .await
            .expect("list for profile")
            .len(),
        1
    );

    // Close: set once, enabled cleared, idempotent.
    let closed = store.close_bot(&bot_id, 30).await.expect("close bot");
    assert_eq!(closed.closed_at_ms, Some(30));
    assert!(!closed.document.enabled);
    assert_eq!(closed.revision, 4);
    let again = store.close_bot(&bot_id, 31).await.expect("close again");
    assert_eq!(again, closed);
    assert!(
        store
            .list_bots_for_profile(&ProfileId::new("triage"))
            .await
            .expect("list for profile after close")
            .is_empty()
    );

    // A closed bot accepts label edits only.
    let mut relabel = closed.document.clone();
    relabel.display_name = Some("Triage (closed)".to_owned());
    relabel.description = Some("retired".to_owned());
    let relabeled = store
        .put_bot(bot_id.clone(), relabel.clone(), Some(4), 32)
        .await
        .expect("relabel closed bot");
    assert_eq!(relabeled.document, relabel);
    assert_eq!(relabeled.closed_at_ms, Some(30));
    let mut reenable = relabel.clone();
    reenable.enabled = true;
    assert!(matches!(
        store.put_bot(bot_id.clone(), reenable, None, 33).await,
        Err(BotError::BotClosed { .. })
    ));
    let mut rebrief = relabel.clone();
    rebrief.brief = Some("something else".to_owned());
    assert!(matches!(
        store.put_bot(bot_id.clone(), rebrief, None, 34).await,
        Err(BotError::BotClosed { .. })
    ));

    // Closed sessions union keeps first-occurrence order.
    let recorded = store
        .record_bot_closed_sessions(&bot_id, vec!["s-b".to_owned(), "s-a".to_owned()])
        .await
        .expect("record closed sessions");
    assert_eq!(recorded, vec!["s-b", "s-a"]);
    let recorded = store
        .record_bot_closed_sessions(
            &bot_id,
            vec!["s-a".to_owned(), "s-c".to_owned(), "s-b".to_owned()],
        )
        .await
        .expect("record closed sessions again");
    assert_eq!(recorded, vec!["s-b", "s-a", "s-c"]);
    assert_eq!(
        store.read_bot(&bot_id).await.expect("read").closed_sessions,
        vec!["s-b", "s-a", "s-c"]
    );

    // Delete cascades to triggers and events.
    store
        .put_bot_trigger(
            &BotId::new("other"),
            trigger_write("hourly", trigger_document(schedule_spec())),
            None,
            40,
        )
        .await
        .expect("put trigger");
    let seq = store
        .allocate_bot_event_seq(&BotId::new("other"))
        .await
        .expect("allocate seq");
    store
        .insert_bot_event(event(
            &BotId::new("other"),
            "evt-1",
            seq,
            41,
            Some("hourly"),
            None,
        ))
        .await
        .expect("insert event");
    let deleted = store
        .delete_bot(&BotId::new("other"))
        .await
        .expect("delete bot");
    assert_eq!(deleted.bot_id.as_str(), "other");
    assert!(matches!(
        store.read_bot(&BotId::new("other")).await,
        Err(BotError::BotNotFound { .. })
    ));
    assert!(matches!(
        store
            .read_bot_trigger(&BotId::new("other"), &BotTriggerId::new("hourly"))
            .await,
        Err(BotError::TriggerNotFound { .. })
    ));
    assert!(matches!(
        store.read_bot_event_by_seq(&BotId::new("other"), seq).await,
        Err(BotError::EventNotFound { seq: 1, .. })
    ));
    assert!(matches!(
        store.delete_bot(&BotId::new("other")).await,
        Err(BotError::BotNotFound { .. })
    ));
    assert!(matches!(
        store.allocate_bot_event_seq(&BotId::new("other")).await,
        Err(BotError::BotNotFound { .. })
    ));

    drop_universe(&store).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres"]
async fn pg_live_bot_triggers_put_disable_reenable_and_cursor() {
    let store = live_store().await;
    let bot_id = BotId::new("poller");
    store
        .create_bot(bot_id.clone(), bot_document("poller"), 10)
        .await
        .expect("create bot");

    // A trigger needs its bot.
    assert!(matches!(
        store
            .put_bot_trigger(
                &BotId::new("missing"),
                trigger_write("feed", trigger_document(poll_spec())),
                None,
                11,
            )
            .await,
        Err(BotError::BotNotFound { .. })
    ));

    let mut write = trigger_write("feed", trigger_document(poll_spec()));
    write.secrets.webhook_token = Some("tok".to_owned());
    let created = store
        .put_bot_trigger(&bot_id, write.clone(), None, 20)
        .await
        .expect("create trigger");
    assert_eq!(created.revision, 1);
    assert_eq!(created.kind(), BotTriggerKind::Poll);
    assert_eq!(created.secrets.webhook_token.as_deref(), Some("tok"));
    assert_eq!(created.cursor, None);
    assert_eq!((created.created_at_ms, created.updated_at_ms), (20, 20));

    // Cursor writes do not touch the revision; a put with `cursor: None`
    // keeps the stored cursor, `Some(None)` clears it, `Some(Some)` sets it.
    let cursor = PollCursorState {
        ids: vec!["1".to_owned(), "2".to_owned()],
        watermark: None,
        consecutive_failures: 0,
        baselined_at_ms: Some(21),
        last_polled_at_ms: Some(21),
    };
    store
        .set_bot_trigger_cursor(&bot_id, &created.trigger_id, Some(cursor.clone()))
        .await
        .expect("set cursor");
    let read = store
        .read_bot_trigger(&bot_id, &created.trigger_id)
        .await
        .expect("read trigger");
    assert_eq!(read.cursor, Some(cursor.clone()));
    assert_eq!(read.revision, 1);
    let kept = store
        .put_bot_trigger(&bot_id, write.clone(), Some(1), 22)
        .await
        .expect("put keeps cursor");
    assert_eq!(kept.revision, 2);
    assert_eq!(kept.cursor, Some(cursor.clone()));
    assert_eq!(kept.created_at_ms, 20);
    assert_eq!(kept.updated_at_ms, 22);
    assert!(matches!(
        store
            .put_bot_trigger(&bot_id, write.clone(), Some(1), 23)
            .await,
        Err(BotError::TriggerRevisionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    let mut clearing = write.clone();
    clearing.cursor = Some(None);
    let cleared = store
        .put_bot_trigger(&bot_id, clearing, Some(2), 24)
        .await
        .expect("put clears cursor");
    assert_eq!(cleared.cursor, None);
    let mut setting = write.clone();
    setting.cursor = Some(Some(cursor.clone()));
    let set = store
        .put_bot_trigger(&bot_id, setting, None, 25)
        .await
        .expect("put sets cursor");
    assert_eq!(set.cursor, Some(cursor.clone()));
    assert_eq!(set.revision, 4);

    // Filter errors are trigger state, not a document write.
    store
        .set_bot_trigger_filter_error(
            &bot_id,
            &created.trigger_id,
            Some("no such field".to_owned()),
            26,
        )
        .await
        .expect("set filter error");
    let read = store
        .read_bot_trigger(&bot_id, &created.trigger_id)
        .await
        .expect("read trigger");
    assert_eq!(read.last_filter_error.as_deref(), Some("no such field"));
    assert_eq!(read.last_filter_error_at_ms, Some(26));
    assert_eq!(read.revision, 4);
    store
        .set_bot_trigger_filter_error(&bot_id, &created.trigger_id, None, 27)
        .await
        .expect("clear filter error");
    let read = store
        .read_bot_trigger(&bot_id, &created.trigger_id)
        .await
        .expect("read trigger");
    assert_eq!(read.last_filter_error, None);
    assert_eq!(read.last_filter_error_at_ms, None);

    // Runtime disable clears `enabled`, records the reason, bumps the
    // revision; a put keeps the incident unless it enables the trigger.
    let disabled = store
        .disable_bot_trigger(
            &bot_id,
            &created.trigger_id,
            BotTriggerDisabledReason::PollFailed,
            30,
        )
        .await
        .expect("disable trigger");
    assert!(!disabled.enabled());
    assert_eq!(
        disabled.disabled_reason,
        Some(BotTriggerDisabledReason::PollFailed)
    );
    assert_eq!(disabled.disabled_at_ms, Some(30));
    assert_eq!(disabled.revision, 5);
    let mut still_off = write.clone();
    still_off.document.enabled = false;
    still_off.document.filter = Some("data.ok == true".to_owned());
    let kept_off = store
        .put_bot_trigger(&bot_id, still_off, Some(5), 31)
        .await
        .expect("put disabled trigger");
    assert_eq!(
        kept_off.disabled_reason,
        Some(BotTriggerDisabledReason::PollFailed)
    );
    assert_eq!(kept_off.disabled_at_ms, Some(30));
    let reenabled = store
        .put_bot_trigger(&bot_id, write.clone(), Some(6), 32)
        .await
        .expect("re-enable trigger");
    assert!(reenabled.enabled());
    assert_eq!(reenabled.disabled_reason, None);
    assert_eq!(reenabled.disabled_at_ms, None);
    assert_eq!(reenabled.revision, 7);

    // One inbox per bot; other kinds list by kind across the universe.
    store
        .put_bot_trigger(
            &bot_id,
            trigger_write(
                "inbox",
                trigger_document(BotTriggerSpec::Bot { from: None }),
            ),
            None,
            40,
        )
        .await
        .expect("create inbox");
    assert!(matches!(
        store
            .put_bot_trigger(
                &bot_id,
                trigger_write(
                    "inbox-2",
                    trigger_document(BotTriggerSpec::Bot { from: None })
                ),
                None,
                41,
            )
            .await,
        Err(BotError::InvalidInput { .. })
    ));
    store
        .put_bot_trigger(
            &bot_id,
            trigger_write("hourly", trigger_document(schedule_spec())),
            None,
            42,
        )
        .await
        .expect("create schedule");
    assert_eq!(
        store
            .list_bot_triggers(&bot_id)
            .await
            .expect("list triggers")
            .iter()
            .map(|trigger| trigger.trigger_id.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["feed", "hourly", "inbox"]
    );
    let by_kind = store
        .list_bot_triggers_by_kind(BotTriggerKind::Poll)
        .await
        .expect("list by kind");
    assert_eq!(by_kind.len(), 1);
    assert_eq!(by_kind[0].trigger_id.as_str(), "feed");
    assert!(
        store
            .list_bot_triggers_by_kind(BotTriggerKind::Chat)
            .await
            .expect("list chat")
            .is_empty()
    );

    // Disable-all touches only enabled triggers and returns them.
    store
        .disable_bot_trigger(
            &bot_id,
            &BotTriggerId::new("hourly"),
            BotTriggerDisabledReason::Operator,
            50,
        )
        .await
        .expect("disable hourly");
    let changed = store
        .disable_bot_triggers(&bot_id, BotTriggerDisabledReason::BotClosed, 51)
        .await
        .expect("disable all");
    assert_eq!(
        changed
            .iter()
            .map(|trigger| trigger.trigger_id.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["feed", "inbox"]
    );
    assert!(changed.iter().all(|trigger| {
        !trigger.enabled() && trigger.disabled_reason == Some(BotTriggerDisabledReason::BotClosed)
    }));
    let hourly = store
        .read_bot_trigger(&bot_id, &BotTriggerId::new("hourly"))
        .await
        .expect("read hourly");
    assert_eq!(
        hourly.disabled_reason,
        Some(BotTriggerDisabledReason::Operator)
    );
    assert!(
        store
            .disable_bot_triggers(&bot_id, BotTriggerDisabledReason::BotClosed, 52)
            .await
            .expect("disable all again")
            .is_empty()
    );

    let deleted = store
        .delete_bot_trigger(&bot_id, &BotTriggerId::new("hourly"))
        .await
        .expect("delete trigger");
    assert_eq!(deleted.trigger_id.as_str(), "hourly");
    assert!(matches!(
        store
            .delete_bot_trigger(&bot_id, &BotTriggerId::new("hourly"))
            .await,
        Err(BotError::TriggerNotFound { .. })
    ));
    assert!(matches!(
        store
            .set_bot_trigger_cursor(&bot_id, &BotTriggerId::new("hourly"), None)
            .await,
        Err(BotError::TriggerNotFound { .. })
    ));

    drop_universe(&store).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres"]
async fn pg_live_bot_events_log_rates_outcomes_and_roster() {
    let store = live_store().await;
    let bot_id = BotId::new("watcher");
    let sender = BotId::new("sender");
    store
        .create_bot(bot_id.clone(), bot_document("watcher"), 10)
        .await
        .expect("create bot");
    store
        .create_bot(sender.clone(), bot_document("sender"), 10)
        .await
        .expect("create sender");
    store
        .put_bot_trigger(
            &bot_id,
            trigger_write("hourly", trigger_document(schedule_spec())),
            None,
            11,
        )
        .await
        .expect("create trigger");

    // Seq allocation is monotonic and visible on the bot.
    let first = store
        .allocate_bot_event_seq(&bot_id)
        .await
        .expect("allocate 1");
    let second = store
        .allocate_bot_event_seq(&bot_id)
        .await
        .expect("allocate 2");
    assert_eq!((first, second), (1, 2));
    assert_eq!(store.read_bot(&bot_id).await.expect("read").event_seq, 2);

    // Insert, then the same id again: the stored row wins.
    let one = event(&bot_id, "evt-1", 1, 100, Some("hourly"), None);
    let inserted = store
        .insert_bot_event(one.clone())
        .await
        .expect("insert event 1");
    assert_eq!(inserted, InsertBotEventOutcome::Inserted(one.clone()));
    let duplicate = store
        .insert_bot_event(event(&bot_id, "evt-1", 2, 999, None, None))
        .await
        .expect("insert duplicate");
    assert!(duplicate.is_duplicate());
    assert_eq!(duplicate.record(), &one);
    // A reused seq under a fresh id is refused, not silently duplicated.
    assert!(matches!(
        store
            .insert_bot_event(event(&bot_id, "evt-x", 1, 100, None, None))
            .await,
        Err(BotError::InvalidInput { .. })
    ));
    assert!(matches!(
        store
            .insert_bot_event(event(&BotId::new("nobody"), "evt-1", 1, 100, None, None))
            .await,
        Err(BotError::BotNotFound { .. })
    ));

    // Fill the log: two events share a received time to exercise the
    // keyset tie-break, and two are sent by another bot.
    store
        .insert_bot_event(event(&bot_id, "evt-2", 2, 200, Some("hourly"), None))
        .await
        .expect("insert event 2");
    for (event_id, seq, at, sent_by) in [
        ("evt-3", 3, 200, Some("sender")),
        ("evt-4", 4, 300, Some("sender")),
        ("evt-5", 5, 400, None),
    ] {
        let seq_allocated = store
            .allocate_bot_event_seq(&bot_id)
            .await
            .expect("allocate");
        assert_eq!(seq_allocated, seq);
        store
            .insert_bot_event(event(&bot_id, event_id, seq, at, None, sent_by))
            .await
            .expect("insert event");
    }

    let by_seq = store
        .read_bot_event_by_seq(&bot_id, 3)
        .await
        .expect("read by seq");
    assert_eq!(by_seq.event_id, "evt-3");
    assert_eq!(by_seq.sender_bot_id, Some(sender.clone()));
    assert!(matches!(
        store.read_bot_event_by_seq(&bot_id, 42).await,
        Err(BotError::EventNotFound { seq: 42, .. })
    ));
    assert!(matches!(
        store.read_bot_event(&bot_id, "evt-42").await,
        Err(BotError::EventIdNotFound { .. })
    ));
    let some = store
        .read_bot_events(
            &bot_id,
            &[
                "evt-4".to_owned(),
                "evt-2".to_owned(),
                "evt-nope".to_owned(),
            ],
        )
        .await
        .expect("read events");
    assert_eq!(
        some.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2, 4]
    );

    // Newest first, keyset continuation.
    let page = store
        .list_bot_events(&bot_id, 2, None)
        .await
        .expect("first page");
    assert_eq!(
        page.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![5, 4]
    );
    let last = page.last().expect("page has rows");
    let page = store
        .list_bot_events(
            &bot_id,
            2,
            Some(BotEventCursor {
                received_at_ms: last.received_at_ms,
                seq: last.seq,
            }),
        )
        .await
        .expect("second page");
    assert_eq!(
        page.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![3, 2]
    );
    let last = page.last().expect("page has rows");
    let page = store
        .list_bot_events(
            &bot_id,
            2,
            Some(BotEventCursor {
                received_at_ms: last.received_at_ms,
                seq: last.seq,
            }),
        )
        .await
        .expect("third page");
    assert_eq!(
        page.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1]
    );
    assert!(
        store
            .list_bot_events(&bot_id, 0, None)
            .await
            .expect("empty page")
            .is_empty()
    );

    // Rate windows.
    assert_eq!(
        store
            .count_bot_events_since(
                BotEventRateScope::Trigger {
                    bot_id: &bot_id,
                    trigger_id: &BotTriggerId::new("hourly"),
                },
                0,
            )
            .await
            .expect("trigger count"),
        2
    );
    assert_eq!(
        store
            .count_bot_events_since(
                BotEventRateScope::Trigger {
                    bot_id: &bot_id,
                    trigger_id: &BotTriggerId::new("hourly"),
                },
                150,
            )
            .await
            .expect("trigger count since"),
        1
    );
    assert_eq!(
        store
            .count_bot_events_since(
                BotEventRateScope::Sender {
                    sender_bot_id: &sender,
                },
                0,
            )
            .await
            .expect("sender count"),
        2
    );
    assert_eq!(
        store
            .count_bot_events_since(
                BotEventRateScope::Sender {
                    sender_bot_id: &sender,
                },
                301,
            )
            .await
            .expect("sender count since"),
        0
    );

    // Outcomes are written once.
    let write = BotEventOutcomeWrite {
        outcome: BotEventOutcome::Handled,
        detail: Some("done".to_owned()),
        run_id: Some("run-1".to_owned()),
        resolved_at_ms: 500,
    };
    assert_eq!(
        store
            .record_bot_event_outcomes(
                &bot_id,
                &[
                    "evt-1".to_owned(),
                    "evt-2".to_owned(),
                    "evt-nope".to_owned()
                ],
                write.clone(),
            )
            .await
            .expect("record outcomes"),
        2
    );
    assert_eq!(
        store
            .record_bot_event_outcomes(
                &bot_id,
                &["evt-1".to_owned(), "evt-2".to_owned()],
                BotEventOutcomeWrite {
                    outcome: BotEventOutcome::Ignored,
                    detail: None,
                    run_id: None,
                    resolved_at_ms: 600,
                },
            )
            .await
            .expect("record outcomes again"),
        0
    );
    let resolved = store
        .read_bot_event(&bot_id, "evt-1")
        .await
        .expect("read resolved");
    assert_eq!(resolved.outcome, Some(BotEventOutcome::Handled));
    assert_eq!(resolved.outcome_detail.as_deref(), Some("done"));
    assert_eq!(resolved.run_id.as_deref(), Some("run-1"));
    assert_eq!(resolved.resolved_at_ms, Some(500));
    assert!(!resolved.is_pending());
    assert_eq!(
        store
            .record_bot_event_outcomes(&bot_id, &[], write)
            .await
            .expect("empty outcome write"),
        0
    );

    // Roster: counts and the latest event per bot, ordered by bot id.
    let roster = store.list_bot_roster().await.expect("roster");
    assert_eq!(roster.len(), 2);
    assert_eq!(roster[0].bot.bot_id, sender);
    assert_eq!(roster[0].trigger_count, 0);
    assert_eq!(roster[0].pending_count, 0);
    assert_eq!(roster[0].last_event, None);
    assert_eq!(roster[1].bot.bot_id, bot_id);
    assert_eq!(roster[1].bot.event_seq, 5);
    assert_eq!(roster[1].trigger_count, 1);
    assert_eq!(roster[1].pending_count, 3);
    let latest = roster[1].last_event.as_ref().expect("last event");
    assert_eq!(latest.seq, 5);
    assert_eq!(latest, &event(&bot_id, "evt-5", 5, 400, None, None));

    // Wake-failure compensation.
    assert!(
        store
            .delete_bot_event(&bot_id, "evt-5")
            .await
            .expect("delete event")
    );
    assert!(
        !store
            .delete_bot_event(&bot_id, "evt-5")
            .await
            .expect("delete event again")
    );
    assert_eq!(
        store.list_bot_roster().await.expect("roster")[1].pending_count,
        2
    );

    drop_universe(&store).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres"]
async fn pg_live_channel_accounts_and_pairings() {
    let store = live_store().await;
    let universe_id = store.config().universe_id;
    let account_id = ChannelAccountId::new("tg-main");
    let document = ChannelAccountDocument {
        provider: ChannelProvider::new("telegram"),
        provider_account_id: "@triage_bot".to_owned(),
        display_name: "Triage on Telegram".to_owned(),
        credential_grant_id: Some("grant-tg".to_owned()),
        settings: Default::default(),
        enabled: true,
    };

    let created = store
        .create_channel_account(account_id.clone(), document.clone(), 10)
        .await
        .expect("create account");
    assert_eq!(created.revision, 1);
    assert_eq!(created.document, document);
    assert!(matches!(
        store
            .create_channel_account(account_id.clone(), document.clone(), 11)
            .await,
        Err(ChannelError::AccountAlreadyExists { .. })
    ));
    // The provider account id is unique per universe and provider.
    assert!(matches!(
        store
            .create_channel_account(ChannelAccountId::new("tg-dup"), document.clone(), 12)
            .await,
        Err(ChannelError::InvalidInput { .. })
    ));

    let mut edited = document.clone();
    edited.display_name = "Triage".to_owned();
    edited.enabled = false;
    let put = store
        .put_channel_account(account_id.clone(), edited.clone(), Some(1), 20)
        .await
        .expect("put account");
    assert_eq!(put.revision, 2);
    assert_eq!(put.document, edited);
    assert_eq!((put.created_at_ms, put.updated_at_ms), (10, 20));
    assert!(matches!(
        store
            .put_channel_account(account_id.clone(), edited.clone(), Some(1), 21)
            .await,
        Err(ChannelError::AccountRevisionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    let whatsapp = store
        .put_channel_account(
            ChannelAccountId::new("wa-main"),
            ChannelAccountDocument {
                provider: ChannelProvider::new("whatsapp"),
                provider_account_id: "+15550000000".to_owned(),
                display_name: "Triage on WhatsApp".to_owned(),
                credential_grant_id: None,
                settings: Default::default(),
                enabled: true,
            },
            Some(9),
            22,
        )
        .await
        .expect("put creates account");
    assert_eq!(whatsapp.revision, 1);

    let listed = store
        .list_channel_accounts(None)
        .await
        .expect("list accounts");
    assert_eq!(
        listed
            .iter()
            .map(|account| account.account_id.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["tg-main", "wa-main"]
    );
    let telegram_only = store
        .list_channel_accounts(Some(ChannelProvider::new("telegram")))
        .await
        .expect("list telegram");
    assert_eq!(telegram_only.len(), 1);
    assert_eq!(telegram_only[0].account_id, account_id);

    // Deployment-wide listing skips disabled accounts unless asked.
    let mine = |rows: Vec<(Uuid, channels::ChannelAccountRecord)>| {
        rows.into_iter()
            .filter(|(universe, _)| *universe == universe_id)
            .map(|(_, account)| account.account_id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        mine(
            store_pg::list_channel_accounts_all(store.pool(), None, false)
                .await
                .expect("list all enabled")
        ),
        vec!["wa-main"]
    );
    assert_eq!(
        mine(
            store_pg::list_channel_accounts_all(store.pool(), None, true)
                .await
                .expect("list all")
        ),
        vec!["tg-main", "wa-main"]
    );
    assert_eq!(
        mine(
            store_pg::list_channel_accounts_all(
                store.pool(),
                Some(ChannelProvider::new("telegram")),
                true
            )
            .await
            .expect("list all telegram")
        ),
        vec!["tg-main"]
    );

    // Pairings hang off a chat trigger and an account.
    let bot_id = BotId::new("concierge");
    store
        .create_bot(bot_id.clone(), bot_document("concierge"), 30)
        .await
        .expect("create bot");
    let mut chat = trigger_write("telegram", trigger_document(chat_spec("tg-main")));
    chat.secrets.pairing_code = Some("ABCDEFGH2345".to_owned());
    let trigger = store
        .put_bot_trigger(&bot_id, chat, None, 31)
        .await
        .expect("create chat trigger");
    assert_eq!(trigger.kind(), BotTriggerKind::Chat);
    assert_eq!(
        trigger.secrets.pairing_code.as_deref(),
        Some("ABCDEFGH2345")
    );
    store
        .put_bot_trigger(
            &bot_id,
            trigger_write("telegram-vip", trigger_document(chat_spec("tg-main"))),
            None,
            32,
        )
        .await
        .expect("create second chat trigger");

    let pairing = ChannelPairingRecord {
        account_id: account_id.clone(),
        chat_id: "chat-1".to_owned(),
        bot_id: bot_id.clone(),
        trigger_id: BotTriggerId::new("telegram"),
        paired_via: ChannelPairedVia::Code,
        paired_at_ms: 40,
    };
    assert!(matches!(
        store
            .upsert_channel_pairing(ChannelPairingRecord {
                trigger_id: BotTriggerId::new("nope"),
                ..pairing.clone()
            })
            .await,
        Err(ChannelError::InvalidInput { .. })
    ));
    assert!(matches!(
        store
            .upsert_channel_pairing(ChannelPairingRecord {
                account_id: ChannelAccountId::new("nope"),
                ..pairing.clone()
            })
            .await,
        Err(ChannelError::InvalidInput { .. })
    ));
    assert_eq!(
        store
            .upsert_channel_pairing(pairing.clone())
            .await
            .expect("upsert pairing"),
        pairing
    );
    assert_eq!(
        store
            .read_channel_pairing(&account_id, "chat-1")
            .await
            .expect("read pairing"),
        Some(pairing.clone())
    );
    assert_eq!(
        store
            .read_channel_pairing(&account_id, "chat-missing")
            .await
            .expect("read missing pairing"),
        None
    );
    // A re-pair moves the chat to another trigger for the same chat.
    let moved = ChannelPairingRecord {
        trigger_id: BotTriggerId::new("telegram-vip"),
        paired_at_ms: 41,
        ..pairing.clone()
    };
    assert_eq!(
        store
            .upsert_channel_pairing(moved.clone())
            .await
            .expect("re-pair"),
        moved
    );
    let second = ChannelPairingRecord {
        chat_id: "chat-2".to_owned(),
        paired_at_ms: 50,
        ..pairing.clone()
    };
    store
        .upsert_channel_pairing(second.clone())
        .await
        .expect("second pairing");

    let all = store
        .list_channel_pairings(ChannelPairingFilter::default())
        .await
        .expect("list pairings");
    assert_eq!(all, vec![second.clone(), moved.clone()]);
    let by_trigger = store
        .list_channel_pairings(ChannelPairingFilter {
            trigger_id: Some(BotTriggerId::new("telegram-vip")),
            ..Default::default()
        })
        .await
        .expect("list by trigger");
    assert_eq!(by_trigger, vec![moved.clone()]);
    let by_chat = store
        .list_channel_pairings(ChannelPairingFilter {
            account_id: Some(account_id.clone()),
            chat_id: Some("chat-2".to_owned()),
            ..Default::default()
        })
        .await
        .expect("list by chat");
    assert_eq!(by_chat, vec![second.clone()]);
    assert!(
        store
            .list_channel_pairings(ChannelPairingFilter {
                bot_id: Some(BotId::new("someone-else")),
                ..Default::default()
            })
            .await
            .expect("list by other bot")
            .is_empty()
    );

    // Deleting the trigger drops its pairings; deleting the account drops
    // the rest.
    store
        .delete_bot_trigger(&bot_id, &BotTriggerId::new("telegram-vip"))
        .await
        .expect("delete vip trigger");
    assert_eq!(
        store
            .read_channel_pairing(&account_id, "chat-1")
            .await
            .expect("read moved pairing"),
        None
    );
    assert_eq!(
        store
            .delete_channel_pairing(&account_id, "chat-2")
            .await
            .expect("delete pairing"),
        second
    );
    assert!(matches!(
        store.delete_channel_pairing(&account_id, "chat-2").await,
        Err(ChannelError::PairingNotFound { .. })
    ));
    store
        .upsert_channel_pairing(pairing.clone())
        .await
        .expect("pair again");
    let deleted = store
        .delete_channel_account(&account_id)
        .await
        .expect("delete account");
    assert_eq!(deleted.account_id, account_id);
    assert!(matches!(
        store.read_channel_account(&account_id).await,
        Err(ChannelError::AccountNotFound { .. })
    ));
    assert!(
        store
            .list_channel_pairings(ChannelPairingFilter::default())
            .await
            .expect("list after account delete")
            .is_empty()
    );

    drop_universe(&store).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Postgres"]
async fn pg_live_bot_event_roots_cover_receiver_tools_and_roll_back_missing_refs() {
    let store = live_store().await;
    let bot_id = BotId::new("rooted-event");
    store
        .create_bot(bot_id.clone(), bot_document("profile"), 1)
        .await
        .expect("create bot");
    let tools = store
        .put_bytes(b"receiver tools".to_vec())
        .await
        .expect("tools");
    let media = store
        .put_bytes(b"media attachment".to_vec())
        .await
        .expect("media");
    let transcript = store
        .put_bytes(b"prepared transcript".to_vec())
        .await
        .expect("transcript");
    let mut record = event(&bot_id, "with-tools", 1, 2, None, None);
    record.receiver = Some(bots::EventReceiver::Workflow {
        workflow_id: "conversation".to_owned(),
        workflow_kind: "chat".to_owned(),
        token: "receipt".to_owned(),
        tools_ref: Some(tools.to_string()),
    });
    record.media = vec![api::BotEventMedia {
        text_ref: Some(transcript.to_string()),
        blob_ref: media.to_string(),
        kind: api::BotEventMediaKind::Audio,
        mime: "audio/ogg".to_owned(),
        name: None,
    }];
    store
        .insert_bot_event(record.clone())
        .await
        .expect("insert rooted event");
    let roots: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cas_bot_event_roots WHERE universe_id = $1 AND bot_id = $2",
    )
    .bind(store.config().universe_id)
    .bind(bot_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("roots");
    assert_eq!(
        roots, 5,
        "document, prompt, source audio, transcript, and receiver tools are retained"
    );
    sqlx::query("UPDATE cas_blobs SET created_at_ms = 1, touched_at_ms = 1 WHERE universe_id = $1")
        .bind(store.config().universe_id)
        .execute(store.pool())
        .await
        .expect("age blobs");
    let refs = [
        &record.document_ref,
        record.prompt_ref.as_ref().unwrap(),
        &media.to_string(),
        &transcript.to_string(),
        &tools.to_string(),
    ]
    .into_iter()
    .map(|r| BlobRef::parse(r.clone()).unwrap())
    .collect::<Vec<_>>();
    assert!(
        store
            .delete_dead_blobs(&refs, 2, &[])
            .await
            .expect("sweep rooted event")
            .is_empty()
    );
    let missing = BlobRef::from_bytes(b"never stored").to_string();
    let mut duplicate = record.clone();
    duplicate.document_ref = missing.clone();
    assert!(
        store
            .insert_bot_event(duplicate)
            .await
            .expect("duplicate keeps original roots")
            .is_duplicate()
    );
    let mut broken = record;
    broken.event_id = "missing".to_owned();
    broken.seq = 2;
    broken.document_ref = missing;
    assert!(matches!(
        store.insert_bot_event(broken).await,
        Err(BotError::Store { .. })
    ));
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM bot_events WHERE universe_id = $1 AND bot_id = $2",
    )
    .bind(store.config().universe_id)
    .bind(bot_id.as_str())
    .fetch_one(store.pool())
    .await
    .expect("events");
    assert_eq!(events, 1, "a failed attachment rolls back its event");
    sqlx::query("DELETE FROM bot_events WHERE universe_id = $1 AND bot_id = $2")
        .bind(store.config().universe_id)
        .bind(bot_id.as_str())
        .execute(store.pool())
        .await
        .expect("delete event");
    assert_eq!(
        store
            .delete_dead_blobs(&refs, 2, &[])
            .await
            .expect("sweep released roots")
            .len(),
        5
    );
    drop_universe(&store).await;
}
