//! Channels live proof: a chat message admitted through
//! `channels/inbound/admit` becomes a bot event, the bot's routed session
//! runs, the delivery receipt reaches the conversation workflow, and the
//! reply goes out through the connector's task queue — served here by a
//! fake in-process connector. Pairing is exercised end to end.
//!
//! Run serially against the local stack:
//!
//! ```bash
//! source scripts/dev/env.sh
//! cargo test -p temporal-server --test channels_live -- --ignored --test-threads=1
//! ```

mod support;

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use api::{
    AgentApiService, AgentProfileInput, BotCreateParams, BotDocument, BotEventListParams,
    BotEventOutcome, BotId, BotInput, BotTriggerDocument, BotTriggerId, BotTriggerInput,
    BotTriggerSpec, ChannelAccountCreateParams, ChannelAccountDocument, ChannelAccountId,
    ChannelAccountInput, ChannelAccountSettings, ChannelInbound, ChannelInboundAdmitParams,
    ChannelInboundDecision, ChannelProvider, ChatAccess, ChatActivation, ChatPairing,
    ProfileCreateParams, ProfileDocument, ProfileId, ProfileInstructions,
};
use channels::{
    connector_task_queue,
    delivery::{ChannelDeliveryCommand, ChannelDeliveryOperation, ChannelDeliveryResult},
    media::{MaintainChannelTypingInput, PrepareChannelMediaInput, PrepareChannelMediaResult},
};
use engine::{CoreAgentLlm, CoreAgentTools, storage::BlobStore};
use support::live::{LIVE_TEST_LOCK, live_universe_id, require_storage_live_env};
use temporal_server::{
    config::TaskQueues,
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{
        ActivityState, BotWorkerActivities, ChannelWorkerActivities, FakeLlm, FakeTools,
        WorkerActivities, bots_worker, channels_worker, core_runtime, worker_with_activities,
    },
};
use temporal_workflow::{
    DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET,
    channels::{
        ACTIVITY_CONNECTOR_DELIVER_MESSAGE, ACTIVITY_CONNECTOR_MAINTAIN_TYPING,
        ACTIVITY_CONNECTOR_PREPARE_MEDIA,
    },
    connect_temporal,
};
use temporalio_client::Client;
use temporalio_common::worker::WorkerTaskTypes;
use temporalio_macros::activities;
use temporalio_sdk::{
    Worker, WorkerOptions,
    activities::{ActivityContext, ActivityError},
};

const WAIT: Duration = Duration::from_secs(90);

/// The connector host, in-process: records every delivery and answers with
/// synthetic provider message ids. Cloneable so the test keeps a handle to
/// the shared counters after the worker takes the activities by value.
#[derive(Clone, Default)]
pub struct FakeConnector {
    deliveries: Arc<Mutex<Vec<ChannelDeliveryCommand>>>,
    typing_started: Arc<Mutex<u32>>,
}

#[activities]
impl FakeConnector {
    #[activity(name = ACTIVITY_CONNECTOR_DELIVER_MESSAGE)]
    pub async fn deliver_channel_message(
        self: Arc<Self>,
        _ctx: ActivityContext,
        command: ChannelDeliveryCommand,
    ) -> Result<ChannelDeliveryResult, ActivityError> {
        let count = {
            let mut deliveries = self.deliveries.lock().expect("deliveries lock");
            deliveries.push(command.clone());
            deliveries.len()
        };
        Ok(ChannelDeliveryResult {
            version: 1,
            provider: command.route.provider,
            message_ids: vec![format!("fake-msg-{count}")],
        })
    }

    #[activity(name = ACTIVITY_CONNECTOR_PREPARE_MEDIA)]
    pub async fn prepare_channel_media(
        self: Arc<Self>,
        _ctx: ActivityContext,
        _input: PrepareChannelMediaInput,
    ) -> Result<PrepareChannelMediaResult, ActivityError> {
        Err(ActivityError::application(
            temporalio_common::error::ApplicationFailure::non_retryable(anyhow::anyhow!(
                "the fake connector serves no media"
            )),
        ))
    }

    #[activity(name = ACTIVITY_CONNECTOR_MAINTAIN_TYPING)]
    pub async fn maintain_channel_typing(
        self: Arc<Self>,
        ctx: ActivityContext,
        _input: MaintainChannelTypingInput,
    ) -> Result<(), ActivityError> {
        *self.typing_started.lock().expect("typing lock") += 1;
        loop {
            ctx.record_heartbeat(Vec::new());
            tokio::select! {
                _ = ctx.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    }
}

pub struct Live {
    api: Arc<GatewayAgentApi>,
    client: Client,
    connector: FakeConnector,
    account_id: ChannelAccountId,
}

async fn run_channels_live<F, Fut>(body: F) -> anyhow::Result<()>
where
    F: FnOnce(Live) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    require_storage_live_env()?;
    let _guard = LIVE_TEST_LOCK.lock().await;
    let universe = live_universe_id()?;
    let store = pg_store_from_env().await?;
    let queues = TaskQueues::derived_from(format!(
        "lightspeed-channels-live-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let runtime = core_runtime()?;
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let api = Arc::new(
        GatewayAgentApi::builder(client.clone(), store.clone())
            .with_task_queue(queues.sessions.clone())
            .with_bot_task_queue(queues.bots.clone())
            .with_channel_task_queue(queues.channels.clone())
            .build(),
    );
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(FakeLlm::new(blobs.clone()).with_tool_rounds(0)) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
    let mut sessions_worker = worker_with_activities(
        &runtime,
        client.clone(),
        queues.sessions.clone(),
        WorkerActivities::for_universe(
            universe,
            ActivityState::from_pg_store(store.clone(), llm, tools),
        ),
    )?;
    let mut bots = bots_worker(
        &runtime,
        client.clone(),
        queues.bots.clone(),
        BotWorkerActivities::for_universe(universe, api.clone()),
        WorkerTaskTypes::all(),
    )?;
    let mut channels = channels_worker(
        &runtime,
        client.clone(),
        queues.channels.clone(),
        ChannelWorkerActivities::for_universe(universe, api.clone()),
        WorkerTaskTypes::all(),
    )?;
    // The account is created before the connector worker so the queue name
    // is known; every test uses one fresh account.
    let account_id = ChannelAccountId::new(unique("tg"));
    let context = support::live::local_request_context().await?;
    temporal_server::gateway::principal::with_request_context(
        context.clone(),
        api.create_channel_account(ChannelAccountCreateParams {
            account: ChannelAccountInput {
                account_id: account_id.clone(),
                document: ChannelAccountDocument {
                    provider: ChannelProvider::new("telegram"),
                    provider_account_id: unique("bot"),
                    display_name: "Live Telegram".to_owned(),
                    credential_grant_id: None,
                    settings: ChannelAccountSettings::default(),
                    enabled: true,
                },
            },
        }),
    )
    .await?;
    let connector = FakeConnector::default();
    let connector_queue =
        connector_task_queue(universe, &ChannelProvider::new("telegram"), &account_id);
    let mut connector_worker = Worker::new(
        &runtime,
        client.clone(),
        WorkerOptions::new(connector_queue)
            .register_activities(connector.clone())
            .task_types(WorkerTaskTypes {
                enable_workflows: false,
                enable_local_activities: false,
                enable_remote_activities: true,
                enable_nexus: false,
            })
            .build(),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;

    let shutdowns = [
        sessions_worker.shutdown_handle(),
        bots.shutdown_handle(),
        channels.shutdown_handle(),
        connector_worker.shutdown_handle(),
    ];
    let workers = async {
        tokio::try_join!(
            sessions_worker.run(),
            bots.run(),
            channels.run(),
            connector_worker.run()
        )
        .map(|_| ())
    };
    tokio::pin!(workers);
    let body = temporal_server::gateway::principal::with_request_context(
        context,
        body(Live {
            api: api.clone(),
            client: client.clone(),
            connector: connector.clone(),
            account_id: account_id.clone(),
        }),
    );
    tokio::pin!(body);
    let result = tokio::select! {
        workers_result = workers.as_mut() => Err(anyhow::anyhow!("workers stopped early: {workers_result:?}")),
        body_result = body.as_mut() => body_result,
    };
    for shutdown in shutdowns {
        shutdown();
    }
    let _ = tokio::time::timeout(Duration::from_secs(10), workers.as_mut()).await;
    result
}

fn unique(prefix: &str) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &suffix[..12])
}

async fn create_bot_with_chat(
    api: &GatewayAgentApi,
    account_id: &ChannelAccountId,
    pairing: ChatPairing,
) -> anyhow::Result<(BotId, BotTriggerId, Option<String>)> {
    let profile_id = ProfileId::new(unique("chat-live"));
    api.create_profile(ProfileCreateParams {
        profile: AgentProfileInput {
            profile_id: profile_id.clone(),
            display_name: None,
            description: None,
            document: ProfileDocument {
                instructions: Some(ProfileInstructions::Text {
                    text: "You are a chat bot in a live test.".to_owned(),
                }),
                ..ProfileDocument::default()
            },
        },
    })
    .await?;
    let bot_id = BotId::new(unique("chatbot"));
    let trigger_id = BotTriggerId::new("telegram");
    let created = api
        .create_bot(BotCreateParams {
            access: None,
            execution: None,
            bot: BotInput {
                bot_id: bot_id.clone(),
                document: BotDocument {
                    display_name: Some("Chat bot".to_owned()),
                    description: None,
                    profile_id,
                    brief: None,
                    runs_per_day: None,
                    breaker: None,
                    routed_session_close_after_ms: None,
                    self_config: false,
                    emit: false,
                    enabled: true,
                },
            },
            triggers: vec![BotTriggerInput {
                trigger_id: trigger_id.clone(),
                document: BotTriggerDocument {
                    spec: BotTriggerSpec::Chat {
                        account_id: account_id.as_str().to_owned(),
                        match_scope: None,
                        activation: ChatActivation::default(),
                        access: ChatAccess::default(),
                        pairing,
                        priority: 100,
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
        })
        .await?
        .result;
    let pairing_code = created.triggers[0].pairing_code.clone();
    Ok((bot_id, trigger_id, pairing_code))
}

fn inbound(chat_id: &str, message_id: &str, text: &str) -> ChannelInbound {
    ChannelInbound {
        message_id: message_id.to_owned(),
        chat_id: chat_id.to_owned(),
        thread_id: None,
        sender_id: "user-1".to_owned(),
        sender_name: "Ada".to_owned(),
        timestamp_ms: 1_788_000_000_000,
        text: text.to_owned(),
        media: Vec::new(),
        is_direct: true,
        mentioned_bot: false,
        is_reply_to_bot: false,
    }
}

async fn admit(
    api: &GatewayAgentApi,
    account_id: &ChannelAccountId,
    message: ChannelInbound,
) -> anyhow::Result<ChannelInboundDecision> {
    Ok(api
        .admit_channel_inbound(ChannelInboundAdmitParams {
            account_id: account_id.clone(),
            inbound: message,
        })
        .await?
        .result
        .decision)
}

async fn wait_for_deliveries(
    connector: &FakeConnector,
    expected: usize,
) -> anyhow::Result<Vec<ChannelDeliveryCommand>> {
    let started = Instant::now();
    loop {
        let deliveries = connector
            .deliveries
            .lock()
            .expect("deliveries lock")
            .clone();
        if deliveries.len() >= expected {
            return Ok(deliveries);
        }
        if started.elapsed() > WAIT {
            anyhow::bail!("timed out waiting for {expected} deliveries; got {deliveries:#?}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn channels_live_open_chat_message_round_trips_to_a_reply() -> anyhow::Result<()> {
    run_channels_live(|live| async move {
        let (bot_id, trigger_id, _) =
            create_bot_with_chat(&live.api, &live.account_id, ChatPairing::Open).await?;
        let decision = admit(
            &live.api,
            &live.account_id,
            inbound("chat-1", "m1", "hello bot, are you there?"),
        )
        .await?;
        assert_eq!(decision, ChannelInboundDecision::Bound);

        // The fake model never calls message_send, so the conversation
        // workflow delivers the assistant's final text as the reply.
        let deliveries = wait_for_deliveries(&live.connector, 1).await?;
        let command = &deliveries[0];
        assert_eq!(command.route.chat_id, "chat-1");
        assert_eq!(command.route.provider, ChannelProvider::new("telegram"));
        let ChannelDeliveryOperation::Send { text, .. } = &command.operation else {
            anyhow::bail!("expected a send, got {command:#?}");
        };
        assert!(!text.is_empty());
        assert!(
            command.idempotency_key.starts_with("fallback:"),
            "{command:#?}"
        );

        // The bot's event log holds the inbound message (#1, handled by the
        // run) and the archived reply (#2, chat.sent).
        let started = Instant::now();
        let events = loop {
            let events = live
                .api
                .list_bot_events(BotEventListParams {
                    bot_id: bot_id.clone(),
                    limit: Some(10),
                    cursor: None,
                })
                .await?
                .result
                .events;
            if events.len() >= 2 && events.iter().all(|event| event.outcome.is_some()) {
                break events;
            }
            if started.elapsed() > WAIT {
                anyhow::bail!("timed out waiting for chat events: {events:#?}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let message = events
            .iter()
            .find(|event| event.kind == "chat.message")
            .expect("inbound chat event");
        assert_eq!(message.trigger_id.as_ref(), Some(&trigger_id));
        assert!(
            message.session.is_some(),
            "routed per conversation: {message:#?}"
        );
        assert_eq!(message.outcome, Some(BotEventOutcome::Unresolved));
        let sent = events
            .iter()
            .find(|event| event.kind == "chat.sent")
            .expect("archived reply");
        assert_eq!(sent.outcome, Some(BotEventOutcome::Archived));
        assert!(*live.connector.typing_started.lock().expect("typing") >= 1);

        // A second message in the same chat lands on the same routed session.
        let decision = admit(
            &live.api,
            &live.account_id,
            inbound("chat-1", "m2", "and again"),
        )
        .await?;
        assert_eq!(decision, ChannelInboundDecision::Bound);
        let deliveries = wait_for_deliveries(&live.connector, 2).await?;
        assert_eq!(deliveries.len(), 2);

        let snapshot = live
            .api
            .read_channel_conversation(api::ChannelConversationReadParams {
                account_id: live.account_id.clone(),
                chat_id: "chat-1".to_owned(),
                thread_id: None,
            })
            .await?
            .result
            .conversation
            .expect("conversation workflow running");
        assert_eq!(snapshot.inbound_count, 2);
        assert_eq!(snapshot.emitted_count, 2);
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the local Temporal + PostgreSQL stack (source scripts/dev/env.sh)"]
async fn channels_live_pairing_gates_a_conversation() -> anyhow::Result<()> {
    run_channels_live(|live| async move {
        let (_bot_id, _trigger_id, pairing_code) =
            create_bot_with_chat(&live.api, &live.account_id, ChatPairing::Code).await?;
        let code = pairing_code.expect("minted pairing code");

        // A direct message before pairing prompts for the code; ambient
        // group traffic stays silent.
        let decision = admit(&live.api, &live.account_id, inbound("chat-2", "m1", "hi")).await?;
        assert_eq!(decision, ChannelInboundDecision::PairingRequired);
        let mut ambient = inbound("chat-2", "m2", "just chatting");
        ambient.is_direct = false;
        let decision = admit(&live.api, &live.account_id, ambient).await?;
        assert_eq!(decision, ChannelInboundDecision::PairingPending);

        // The code pairs the chat and is consumed; the next message binds.
        let decision = admit(&live.api, &live.account_id, inbound("chat-2", "m3", &code)).await?;
        assert_eq!(decision, ChannelInboundDecision::Paired);
        let pairings = live
            .api
            .list_channel_pairings(api::ChannelPairingListParams {
                account_id: Some(live.account_id.clone()),
                bot_id: None,
            })
            .await?
            .result
            .pairings;
        assert_eq!(pairings.len(), 1);
        assert_eq!(pairings[0].chat_id, "chat-2");

        let decision = admit(
            &live.api,
            &live.account_id,
            inbound("chat-2", "m4", "now paired"),
        )
        .await?;
        assert_eq!(decision, ChannelInboundDecision::Bound);
        let deliveries = wait_for_deliveries(&live.connector, 1).await?;
        assert_eq!(deliveries[0].route.chat_id, "chat-2");

        // Unpairing gates the chat again.
        live.api
            .delete_channel_pairing(api::ChannelPairingDeleteParams {
                account_id: live.account_id.clone(),
                chat_id: pairings[0].chat_id.clone(),
            })
            .await?;
        let decision = admit(
            &live.api,
            &live.account_id,
            inbound("chat-2", "m5", "hello?"),
        )
        .await?;
        assert_eq!(decision, ChannelInboundDecision::PairingRequired);

        // An unknown account is unbound.
        let stray = live
            .api
            .admit_channel_inbound(ChannelInboundAdmitParams {
                account_id: ChannelAccountId::new("no-such-account"),
                inbound: inbound("chat-3", "m1", "hi"),
            })
            .await;
        assert!(stray.is_err());
        let _ = &live.client;
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local Temporal, Postgres, and MinIO"]
async fn temporal_live_chat_rebuilds_collected_declarations_and_retains_assets()
-> anyhow::Result<()> {
    use bots::BotEventStore;
    use engine::{BlobRef, WorkflowEndpointRef, storage::collect_blob_refs};
    use temporal_server::channels::activities::{chat_tool_declarations, emit_chat_event};
    use temporal_workflow::channels::{
        CHANNEL_CONVERSATION_WORKFLOW_KIND, ChatEmitEventRequest, ChatEmitEventResult, ChatMessage,
        ChatToolDeclarationsRequest,
    };
    run_channels_live(|live| async move {
        let (bot_id, trigger_id, _) = create_bot_with_chat(&live.api, &live.account_id, ChatPairing::Open).await?;
        let store = pg_store_from_env().await?;
        let receiver = WorkflowEndpointRef {
            workflow_id: unique("gc-conversation"),
            workflow_kind: CHANNEL_CONVERSATION_WORKFLOW_KIND.to_owned(),
        };
        let declaration = chat_tool_declarations(&live.api, ChatToolDeclarationsRequest {
            universe_id: store.config().universe_id, receiver: receiver.clone(),
        }).await.map_err(|e| anyhow::anyhow!("declaration: {e:?}"))?;
        let tools_ref = BlobRef::parse(declaration.tools_ref.clone())?;
        let bytes = store.read_bytes(&tools_ref).await?;
        let children = collect_blob_refs(&serde_json::from_slice(&bytes)?);
        assert!(!children.is_empty());
        let edges: i64 = sqlx::query_scalar("SELECT count(*) FROM cas_blob_edges WHERE universe_id = $1 AND parent_digest = $2")
            .bind(store.config().universe_id).bind(tools_ref.as_str().trim_start_matches("sha256:"))
            .fetch_one(store.pool()).await?;
        assert_eq!(edges as usize, children.len(), "declarations retain all schemas and descriptions");
        sqlx::query("UPDATE cas_blobs SET created_at_ms = 1, touched_at_ms = 1 WHERE universe_id = $1 AND digest = $2")
            .bind(store.config().universe_id).bind(tools_ref.as_str().trim_start_matches("sha256:"))
            .execute(store.pool()).await?;
        let deleted = store.delete_dead_blobs(std::slice::from_ref(&tools_ref), 2, &[]).await?;
        assert_eq!(deleted.len(), 1, "a workflow's cached ref alone does not retain its declaration");
        let result = emit_chat_event(&live.api, ChatEmitEventRequest {
            universe_id: store.config().universe_id,
            bot_id: bot_id.clone(), trigger_id, account_id: live.account_id.clone(),
            provider: ChannelProvider::new("telegram"),
            conversation: channels::ConversationRef {
                account_id: live.account_id.clone(), chat_id: unique("gc-chat"), thread_id: None,
            },
            label: "GC recovery".to_owned(), scope: api::ChatScope::Direct,
            message: ChatMessage {
                message_id: unique("gc-message"), sender_id: "sender".to_owned(),
                sender_name: "Sender".to_owned(), timestamp_ms: 1_788_000_000_000,
                text: "hello after collection".to_owned(), is_direct: true,
                mentioned_bot: false, is_reply_to_bot: false,
            },
            media: vec![], tools_ref: declaration.tools_ref, notify: receiver,
            notify_token: unique("receipt"),
        }).await.map_err(|e| anyhow::anyhow!("emit after collection: {e:?}"))?;
        let ChatEmitEventResult::Admitted { event_id, .. } = result else {
            anyhow::bail!("expected admitted event, got {result:?}");
        };
        let event = store.read_bot_event(&bot_id, &event_id).await?;
        assert_eq!(event.tools_ref(), Some(tools_ref.as_str()));
        assert!(store.has_blob(&tools_ref).await?, "declaration catalog row was reconstructed");
        let root: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cas_bot_event_roots WHERE universe_id = $1 AND bot_id = $2 AND event_id = $3 AND digest = $4)")
            .bind(store.config().universe_id).bind(bot_id.as_str()).bind(&event_id)
            .bind(tools_ref.as_str().trim_start_matches("sha256:")).fetch_one(store.pool()).await?;
        assert!(root, "bot event retains its receiver declaration");
        let mut held = children.into_iter().collect::<Vec<_>>();
        held.push(tools_ref);
        sqlx::query("UPDATE cas_blobs SET created_at_ms = 1, touched_at_ms = 1 WHERE universe_id = $1 AND digest = ANY($2::text[])")
            .bind(store.config().universe_id)
            .bind(held.iter().map(|r| r.as_str().trim_start_matches("sha256:")).collect::<Vec<_>>())
            .execute(store.pool()).await?;
        assert!(store.delete_dead_blobs(&held, 2, &[]).await?.is_empty());
        Ok(())
    }).await
}
