//! Bots live proof: the bot controller, trigger fires, admission, and
//! the `bots/*` service against the local Temporal + PostgreSQL stack, with
//! a fake LLM for the mechanics and one real-model scenario for the resolve
//! round trip.
//!
//! Run serially against the local stack:
//!
//! ```bash
//! source scripts/dev/env.sh
//! cargo test -p temporal-server --test bots_live -- --ignored --test-threads=1
//! ```

mod support;

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use api::{
    AccessPolicyReadParams, AgentApiErrorKind, AgentApiService, AgentProfileInput, BotBreaker,
    BotCloseParams, BotCoalescePolicy, BotControllerStatus, BotCreateParams, BotDeleteParams,
    BotDocument, BotEventAdmitParams, BotEventInput, BotEventListParams, BotEventOutcome,
    BotEventView, BotId, BotInput, BotPutParams, BotReadParams, BotStateReadParams,
    BotTriggerDeleteParams, BotTriggerDocument, BotTriggerId, BotTriggerInput, BotTriggerPutParams,
    BotTriggerSpec, CollectionReadParams, ProfileCreateParams, ProfileDocument, ProfileId,
    ProfileInstructions, SessionReadParams, SessionStatus, WebhookVerification,
};
use bots::ids::{bot_main_session_id, bot_schedule_id};
use engine::{CoreAgentLlm, CoreAgentTools, storage::BlobStore};
use support::live::{
    LIVE_TEST_LOCK, live_universe_id, openai_live_model, require_openai_live_env,
    require_storage_live_env,
};
use temporal_server::{
    config::TaskQueues,
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{
        ActivityState, BotWorkerActivities, FakeLlm, FakeTools, WorkerActivities, bots_worker,
        core_runtime, worker_with_activities,
    },
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use temporalio_client::Client;
use temporalio_common::worker::WorkerTaskTypes;

const WAIT: Duration = Duration::from_secs(90);

enum Llm {
    Fake,
    Real,
}

/// Run the sessions worker (fake or real LLM), the bots worker, and the
/// in-process service over one random deployment queue prefix.
async fn run_bots_live<F, Fut>(llm: Llm, body: F) -> anyhow::Result<()>
where
    F: FnOnce(Arc<GatewayAgentApi>, Client) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    require_storage_live_env()?;
    let _guard = LIVE_TEST_LOCK.lock().await;
    let universe = live_universe_id()?;
    let store = pg_store_from_env().await?;
    let queues = TaskQueues::derived_from(format!(
        "lightspeed-bots-live-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;

    let mut builder = GatewayAgentApi::builder(client.clone(), store.clone())
        .with_task_queue(queues.sessions.clone())
        .with_bot_task_queue(queues.bots.clone())
        .with_channel_task_queue(queues.channels.clone());
    let activity_state = match llm {
        Llm::Fake => {
            let blobs: Arc<dyn BlobStore> = store.clone();
            let llm =
                Arc::new(FakeLlm::new(blobs.clone()).with_tool_rounds(0)) as Arc<dyn CoreAgentLlm>;
            let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
            ActivityState::from_pg_store(store.clone(), llm, tools)
        }
        Llm::Real => {
            builder = builder.with_default_model(openai_live_model());
            ActivityState::from_pg_store_with_default_runtime(store.clone())?
        }
    };
    let api = Arc::new(builder.build());

    let mut sessions_worker = worker_with_activities(
        &runtime,
        client.clone(),
        queues.sessions.clone(),
        WorkerActivities::for_universe(universe, activity_state),
    )?;
    let mut bots = bots_worker(
        &runtime,
        client.clone(),
        queues.bots.clone(),
        BotWorkerActivities::for_universe(universe, api.clone()),
        WorkerTaskTypes::all(),
    )?;
    let shutdown_sessions = sessions_worker.shutdown_handle();
    let shutdown_bots = bots.shutdown_handle();
    let workers = async { tokio::try_join!(sessions_worker.run(), bots.run()).map(|_| ()) };
    tokio::pin!(workers);
    let body = temporal_server::gateway::principal::with_request_context(
        support::live::local_request_context().await?,
        body(api.clone(), client.clone()),
    );
    tokio::pin!(body);
    let result = tokio::select! {
        workers_result = workers.as_mut() => Err(anyhow::anyhow!("workers stopped early: {workers_result:?}")),
        body_result = body.as_mut() => body_result,
    };
    shutdown_sessions();
    shutdown_bots();
    let _ = tokio::time::timeout(Duration::from_secs(10), workers.as_mut()).await;
    result
}

fn unique(prefix: &str) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &suffix[..12])
}

async fn create_profile(api: &GatewayAgentApi, instructions: &str) -> anyhow::Result<ProfileId> {
    let profile_id = ProfileId::new(unique("bots-live"));
    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: Some("bots live".to_owned()),
            description: None,
            document: ProfileDocument {
                instructions: Some(ProfileInstructions::Text {
                    text: instructions.to_owned(),
                }),
                ..ProfileDocument::default()
            },
        },
    })
    .await?;
    Ok(profile_id)
}

fn bot_document(profile_id: &ProfileId) -> BotDocument {
    BotDocument {
        display_name: Some("Live bot".to_owned()),
        description: Some("bots live test".to_owned()),
        profile_id: profile_id.clone(),
        brief: Some("Acknowledge every event briefly.".to_owned()),
        runs_per_day: None,
        breaker: None,
        routed_session_close_after_ms: None,
        self_config: false,
        emit: false,
        enabled: true,
    }
}

async fn create_bot(
    api: &GatewayAgentApi,
    profile_id: &ProfileId,
    edit: impl FnOnce(&mut BotDocument),
    triggers: Vec<BotTriggerInput>,
) -> anyhow::Result<BotId> {
    let bot_id = BotId::new(unique("live"));
    let mut document = bot_document(profile_id);
    edit(&mut document);
    api.create_bot(BotCreateParams {
        access: None,
        bot: BotInput {
            bot_id: bot_id.clone(),
            document,
        },
        triggers,
    })
    .await?;
    Ok(bot_id)
}

fn manual_event(summary: &str) -> BotEventInput {
    BotEventInput {
        kind: "test.ping".to_owned(),
        summary: summary.to_owned(),
        data: Some(serde_json::json!({ "note": summary })),
        headers: Default::default(),
        event_id: None,
        occurred_at_ms: None,
        correlation_id: None,
        links: Vec::new(),
    }
}

async fn list_events(api: &GatewayAgentApi, bot_id: &BotId) -> anyhow::Result<Vec<BotEventView>> {
    Ok(api
        .list_bot_events(BotEventListParams {
            bot_id: bot_id.clone(),
            limit: Some(50),
            cursor: None,
        })
        .await?
        .result
        .events)
}

/// Wait until every event of the bot has an outcome.
async fn wait_for_outcomes(
    api: &GatewayAgentApi,
    bot_id: &BotId,
    expected: usize,
) -> anyhow::Result<Vec<BotEventView>> {
    let started = Instant::now();
    loop {
        let events = list_events(api, bot_id).await?;
        let resolved = events
            .iter()
            .filter(|event| event.outcome.is_some())
            .count();
        if events.len() >= expected && resolved >= expected {
            return Ok(events);
        }
        if started.elapsed() > WAIT {
            anyhow::bail!(
                "timed out waiting for {expected} outcomes on bot {bot_id}; events: {events:#?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_controller<F>(
    api: &GatewayAgentApi,
    bot_id: &BotId,
    predicate: F,
) -> anyhow::Result<Option<api::BotControllerSnapshot>>
where
    F: Fn(Option<&api::BotControllerSnapshot>) -> bool,
{
    let started = Instant::now();
    loop {
        let state = api
            .read_bot_state(BotStateReadParams {
                bot_id: bot_id.clone(),
            })
            .await?
            .result
            .state;
        if predicate(state.controller.as_ref()) {
            return Ok(state.controller);
        }
        if started.elapsed() > WAIT {
            anyhow::bail!("timed out waiting for bot {bot_id} controller state; last: {state:#?}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn bots_live_manual_event_runs_and_records_outcome() -> anyhow::Result<()> {
    run_bots_live(Llm::Fake, |api, _client| async move {
        let profile_id = create_profile(&api, "You are a live-test bot.").await?;
        let bot_id = create_bot(&api, &profile_id, |_| {}, Vec::new()).await?;

        let admitted = api
            .admit_bot_event(BotEventAdmitParams {
                bot_id: bot_id.clone(),
                event: manual_event("first ping"),
            })
            .await?
            .result;
        assert_eq!(admitted.event.seq, 1);
        assert!(!admitted.duplicate);

        let events = wait_for_outcomes(&api, &bot_id, 1).await?;
        let event = &events[0];
        // The fake model never calls bot_event_resolve, so the delivery ends
        // unresolved — with the run that handled it recorded on the row.
        assert_eq!(event.outcome, Some(BotEventOutcome::Unresolved));
        assert!(event.run_id.is_some(), "{event:#?}");

        let snapshot = wait_for_controller(&api, &bot_id, |controller| {
            controller.is_some_and(|snapshot| snapshot.recent_deliveries.len() == 1)
        })
        .await?
        .expect("controller running");
        assert_eq!(snapshot.runs_today, 1);
        assert_eq!(snapshot.recent_deliveries[0].seqs, vec![1]);
        assert_eq!(snapshot.controller_status, BotControllerStatus::Idle);

        let main_session = bot_main_session_id(&bot_id, 1);
        let session = api
            .read_session(SessionReadParams {
                session_id: main_session.clone(),
                run_limit: None,
            })
            .await?
            .result
            .session;
        assert_eq!(session.status, SessionStatus::Idle);
        assert!(
            session.active_tools.tools.iter().any(|tool| {
                tool.tool_id == "bot_event_resolve"
                    || tool.tool_id == "lightspeed.bots.event.resolve.v1"
            }),
            "bot tools declared: {:?}",
            session
                .active_tools
                .tools
                .iter()
                .map(|tool| tool.tool_id.clone())
                .collect::<Vec<_>>()
        );

        // A duplicate admission keeps #N and wakes the controller again.
        let duplicate = api
            .admit_bot_event(BotEventAdmitParams {
                bot_id: bot_id.clone(),
                event: BotEventInput {
                    event_id: Some(admitted.event.event_id.clone()),
                    ..manual_event("first ping again")
                },
            })
            .await?
            .result;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.event.seq, 1);
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn bots_live_hmac_webhook_verifies_without_a_caller() -> anyhow::Result<()> {
    run_bots_live(Llm::Fake, |api, _client| async move {
        let profile_id = create_profile(&api, "You are a live-test bot.").await?;
        let secret = format!("signing-{}", uuid::Uuid::new_v4().simple());
        let grant_id = api
            .import_auth_grant(api::AuthGrantImportParams {
                grant_id: None,
                provider_id: None,
                exposure: api::AuthGrantExposure::Retrievable,
                token: secret.clone(),
                display_name: Some("Webhook signing secret".to_owned()),
                subject_hint: None,
                scopes: Vec::new(),
                audience: None,
                expires_at_ms: None,
                metadata: None,
            })
            .await?
            .result
            .grant
            .grant_id;
        let trigger_id = BotTriggerId::new("signed");
        let bot_id = create_bot(
            &api,
            &profile_id,
            |_| {},
            vec![BotTriggerInput {
                trigger_id: trigger_id.clone(),
                document: BotTriggerDocument {
                    spec: BotTriggerSpec::Webhook {
                        verification: WebhookVerification::HmacSha256 {
                            grant_id,
                            header: "x-signature".to_owned(),
                            prefix: Some("sha256=".to_owned()),
                            audience: None,
                        },
                        preset: None,
                    },
                    filter: None,
                    route: None,
                    coalesce: None,
                    deliver: None,
                    session_close_after_ms: None,
                    enabled: true,
                },
                pairing_code: None,
            }],
        )
        .await?;
        let trigger = api
            .read_bot_trigger(api::BotTriggerReadParams {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            })
            .await?
            .result
            .trigger;
        let ingest_path = trigger.ingest_path.expect("webhook ingest path");
        let token = ingest_path.rsplit('/').next().expect("token").to_owned();

        // The public ingest route has no caller: run it outside the test's
        // request context, exactly as the HTTP edge does. The stored trigger,
        // not a caller, names the signing secret.
        let deliver = |signature: String| {
            let (api, bot_id, trigger_id, token) = (
                api.clone(),
                bot_id.clone(),
                trigger_id.clone(),
                token.clone(),
            );
            tokio::spawn(async move {
                let headers = [("x-signature".to_owned(), signature)]
                    .into_iter()
                    .collect();
                api.ingest_bot_webhook(
                    bot_id.as_str(),
                    trigger_id.as_str(),
                    &token,
                    headers,
                    br#"{"kind":"deploy"}"#,
                )
                .await
            })
        };
        let signed = format!(
            "sha256={}",
            bots::webhook::hmac_sha256_hex(&secret, br#"{"kind":"deploy"}"#)
        );
        let admitted = deliver(signed).await?;
        assert!(
            matches!(
                admitted,
                temporal_server::bots::hooks::WebhookIngestOutcome::Admitted { .. }
            ),
            "{admitted:?}"
        );
        let forged = deliver(format!(
            "sha256={}",
            bots::webhook::hmac_sha256_hex("not-the-secret", br#"{"kind":"deploy"}"#)
        ))
        .await?;
        assert!(
            !matches!(
                forged,
                temporal_server::bots::hooks::WebhookIngestOutcome::Admitted { .. }
                    | temporal_server::bots::hooks::WebhookIngestOutcome::SecretUnavailable { .. }
            ),
            "{forged:?}"
        );
        wait_for_outcomes(&api, &bot_id, 1).await?;
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn bots_live_webhook_trigger_coalesces_events() -> anyhow::Result<()> {
    run_bots_live(Llm::Fake, |api, _client| async move {
        let profile_id = create_profile(&api, "You are a live-test bot.").await?;
        let trigger_id = BotTriggerId::new("hook");
        let bot_id = create_bot(
            &api,
            &profile_id,
            |_| {},
            vec![BotTriggerInput {
                trigger_id: trigger_id.clone(),
                document: BotTriggerDocument {
                    spec: BotTriggerSpec::Webhook {
                        verification: WebhookVerification::Token,
                        preset: None,
                    },
                    filter: Some("data.kind != 'noise'".to_owned()),
                    route: None,
                    coalesce: Some(BotCoalescePolicy {
                        debounce_ms: 500,
                        max_wait_ms: 3_000,
                        max_count: 10,
                    }),
                    deliver: None,
                    session_close_after_ms: None,
                    enabled: true,
                },
                pairing_code: None,
            }],
        )
        .await?;
        let trigger = api
            .read_bot_trigger(api::BotTriggerReadParams {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            })
            .await?
            .result
            .trigger;
        let ingest_path = trigger.ingest_path.expect("webhook ingest path");
        let token = ingest_path.rsplit('/').next().expect("token").to_owned();

        for index in 0..3 {
            let body = serde_json::json!({ "kind": "deploy", "n": index }).to_string();
            let outcome = api
                .ingest_bot_webhook(
                    bot_id.as_str(),
                    trigger_id.as_str(),
                    &token,
                    Default::default(),
                    body.as_bytes(),
                )
                .await;
            assert!(
                matches!(
                    outcome,
                    temporal_server::bots::hooks::WebhookIngestOutcome::Admitted { .. }
                ),
                "{outcome:?}"
            );
        }
        // A filtered delivery stores nothing.
        let filtered = api
            .ingest_bot_webhook(
                bot_id.as_str(),
                trigger_id.as_str(),
                &token,
                Default::default(),
                br#"{"kind":"noise"}"#,
            )
            .await;
        assert!(
            matches!(
                filtered,
                temporal_server::bots::hooks::WebhookIngestOutcome::Filtered { .. }
            ),
            "{filtered:?}"
        );
        // A wrong token is indistinguishable from an unknown endpoint.
        let probe = api
            .ingest_bot_webhook(
                bot_id.as_str(),
                trigger_id.as_str(),
                "nope",
                Default::default(),
                br#"{"kind":"deploy"}"#,
            )
            .await;
        assert_eq!(
            probe,
            temporal_server::bots::hooks::WebhookIngestOutcome::UnknownEndpoint
        );

        let events = wait_for_outcomes(&api, &bot_id, 3).await?;
        assert_eq!(events.len(), 3, "the filtered event was never stored");
        let run_ids: std::collections::BTreeSet<_> = events
            .iter()
            .map(|event| event.run_id.clone().expect("run id"))
            .collect();
        assert_eq!(
            run_ids.len(),
            1,
            "coalesced into one delivery and one run: {events:#?}"
        );
        let snapshot = wait_for_controller(&api, &bot_id, |controller| {
            controller.is_some_and(|snapshot| snapshot.recent_deliveries.len() == 1)
        })
        .await?
        .expect("controller");
        assert_eq!(snapshot.runs_today, 1);
        assert_eq!(snapshot.recent_deliveries[0].seqs.len(), 3);
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn bots_live_budget_parks_pending_events() -> anyhow::Result<()> {
    run_bots_live(Llm::Fake, |api, _client| async move {
        let profile_id = create_profile(&api, "You are a live-test bot.").await?;
        let bot_id = create_bot(
            &api,
            &profile_id,
            |document| document.runs_per_day = Some(1),
            Vec::new(),
        )
        .await?;
        for summary in ["one", "two"] {
            api.admit_bot_event(BotEventAdmitParams {
                bot_id: bot_id.clone(),
                event: manual_event(summary),
            })
            .await?;
        }
        let events = wait_for_outcomes(&api, &bot_id, 1).await?;
        let snapshot = wait_for_controller(&api, &bot_id, |controller| {
            controller.is_some_and(|snapshot| {
                snapshot.controller_status == BotControllerStatus::BudgetExhausted
            })
        })
        .await?
        .expect("controller");
        assert_eq!(snapshot.runs_today, 1);
        assert_eq!(snapshot.pending_deliveries, 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.outcome.is_none())
                .count(),
            1,
            "the second event stays pending: {events:#?}"
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn bots_live_schedule_trigger_reconciles_temporal_schedule() -> anyhow::Result<()> {
    run_bots_live(Llm::Fake, |api, client| async move {
        let profile_id = create_profile(&api, "You are a live-test bot.").await?;
        let bot_id = create_bot(&api, &profile_id, |_| {}, Vec::new()).await?;
        let trigger_id = BotTriggerId::new("nightly");
        let trigger = api
            .put_bot_trigger(BotTriggerPutParams {
                bot_id: bot_id.clone(),
                trigger: BotTriggerInput {
                    trigger_id: trigger_id.clone(),
                    document: BotTriggerDocument {
                        spec: BotTriggerSpec::Schedule {
                            cron: Some("*/5 * * * *".to_owned()),
                            at_ms: None,
                            timezone: "UTC".to_owned(),
                            summary: "check the queue".to_owned(),
                        },
                        filter: None,
                        route: None,
                        coalesce: None,
                        deliver: None,
                        session_close_after_ms: None,
                        enabled: true,
                    },
                    pairing_code: None,
                },
                expected_revision: None,
            })
            .await?
            .result
            .trigger;
        assert_eq!(trigger.revision, 1);

        let schedule_id = bot_schedule_id(live_universe_id()?, &bot_id, &trigger_id);
        let handle = client.get_schedule_handle(schedule_id.clone());
        let described = handle.describe().await?;
        assert!(!described.paused());
        assert!(
            !described.future_action_times().is_empty(),
            "cron schedule has upcoming fires"
        );

        // A manual trigger of the Schedule admits one schedule event.
        handle
            .trigger(temporalio_client::schedules::ScheduleOverlapPolicy::AllowAll)
            .await?;
        let events = wait_for_outcomes(&api, &bot_id, 1).await?;
        assert_eq!(events[0].kind, "schedule");
        assert_eq!(events[0].trigger_id.as_ref(), Some(&trigger_id));

        // Disabling the bot pauses its schedules; deleting the trigger drops it.
        let bot = api
            .read_bot(BotReadParams {
                bot_id: bot_id.clone(),
            })
            .await?
            .result
            .bot;
        let mut document = bot.document.clone();
        document.enabled = false;
        api.put_bot(BotPutParams {
            bot: BotInput {
                bot_id: bot_id.clone(),
                document,
            },
            expected_revision: Some(bot.revision),
        })
        .await?;
        assert!(handle.describe().await?.paused());

        api.delete_bot_trigger(BotTriggerDeleteParams {
            bot_id: bot_id.clone(),
            trigger_id: trigger_id.clone(),
        })
        .await?;
        assert!(
            handle.describe().await.is_err(),
            "schedule deleted with the trigger"
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn bots_live_close_and_delete_tear_down() -> anyhow::Result<()> {
    run_bots_live(Llm::Fake, |api, _client| async move {
        let profile_id = create_profile(&api, "You are a live-test bot.").await?;
        let bot_id = create_bot(&api, &profile_id, |_| {}, Vec::new()).await?;
        api.admit_bot_event(BotEventAdmitParams {
            bot_id: bot_id.clone(),
            event: manual_event("before close"),
        })
        .await?;
        wait_for_outcomes(&api, &bot_id, 1).await?;
        let main_session = bot_main_session_id(&bot_id, 1);

        let closed = api
            .close_bot(BotCloseParams {
                bot_id: bot_id.clone(),
            })
            .await?
            .result
            .bot;
        assert!(closed.closed_at_ms.is_some());
        assert!(!closed.document.enabled);

        // The controller tears down and completes: the query stops answering.
        wait_for_controller(&api, &bot_id, |controller| {
            controller.is_none()
                || controller.is_some_and(|snapshot| {
                    snapshot.controller_status == BotControllerStatus::Closed
                })
        })
        .await?;
        let started = Instant::now();
        loop {
            let bot = api
                .read_bot(BotReadParams {
                    bot_id: bot_id.clone(),
                })
                .await?
                .result
                .bot;
            if bot.closed_sessions.contains(&main_session) {
                break;
            }
            if started.elapsed() > WAIT {
                anyhow::bail!("closed sessions never recorded: {bot:#?}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let session = api
            .read_session(SessionReadParams {
                session_id: main_session.clone(),
                run_limit: None,
            })
            .await?
            .result
            .session;
        assert_eq!(session.status, SessionStatus::Closed);

        // Events admitted after close are refused on the row.
        let refused = api
            .admit_bot_event(BotEventAdmitParams {
                bot_id: bot_id.clone(),
                event: manual_event("after close"),
            })
            .await;
        assert!(refused.is_err(), "closed bots refuse events");

        // The bot lives in a collection of its own, which goes with it.
        let policy = api
            .read_access_policy(AccessPolicyReadParams {
                resource: access::ResourceRef::Bot(bot_id.as_str().to_owned()),
            })
            .await?
            .result
            .policy;
        let access::ResourceRef::Collection(collection_id) = policy.root.clone() else {
            panic!("a bot's audience root is its collection: {policy:?}");
        };
        api.read_collection(CollectionReadParams {
            collection_id: collection_id.clone(),
        })
        .await?;
        let deleted = api
            .delete_bot(BotDeleteParams {
                bot_id: bot_id.clone(),
            })
            .await?
            .result;
        assert!(deleted.deleted_sessions.contains(&main_session));
        assert_eq!(
            api.read_collection(CollectionReadParams { collection_id })
                .await
                .unwrap_err()
                .kind,
            AgentApiErrorKind::NotFound,
            "an emptied bot collection is removed with the bot"
        );
        assert!(
            api.read_bot(BotReadParams {
                bot_id: bot_id.clone()
            })
            .await
            .is_err()
        );
        assert!(
            api.read_session(SessionReadParams {
                session_id: main_session,
                run_limit: None,
            })
            .await
            .is_err()
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local stack and OPENAI_API_KEY (costs real money)"]
async fn bots_live_real_model_resolves_delivery() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    require_openai_live_env()?;
    run_bots_live(Llm::Real, |api, _client| async move {
        let profile_id = create_profile(
            &api,
            "You are a bot in a live integration test. For every event delivered to you, call bot_event_resolve exactly once with outcome handled and a one-line summary, then stop.",
        )
        .await?;
        let bot_id = create_bot(
            &api,
            &profile_id,
            |document| {
                document.breaker = Some(BotBreaker {
                    fires: 100,
                    window_ms: 60_000,
                });
            },
            Vec::new(),
        )
        .await?;
        api.admit_bot_event(BotEventAdmitParams {
            bot_id: bot_id.clone(),
            event: manual_event("A customer asked whether the API supports webhooks."),
        })
        .await?;
        let events = wait_for_outcomes(&api, &bot_id, 1).await?;
        let event = &events[0];
        assert_eq!(
            event.outcome,
            Some(BotEventOutcome::Handled),
            "the model resolves the delivery: {event:#?}"
        );
        assert!(event.outcome_detail.is_some(), "summary recorded: {event:#?}");
        Ok(())
    })
    .await
}
