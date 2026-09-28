//! One durable conversation: the provider side of a bot's `chat` trigger,
//! ported from the TypeScript `channelConversationWorkflowV1`.
//!
//! Every activated message becomes a bot event through admission; the
//! bot's routed session for this conversation carries this workflow as the
//! receiver of its `message_*` tools; the bot controller reports
//! `started` / `finished` receipts here for typing and the reply fallback.
//! The workflow never creates a session, starts a run, or holds a
//! lifecycle role.
//!
//! Signals only validate and enqueue. The loop drains inbound messages
//! inline (one activated message at a time, in order) and turns pushed
//! invocations and receipts into lanes: boxed futures polled beside
//! whatever the loop awaits — one per invocation, typing pulse, or reply
//! fallback — so a slow provider blocks only its own message. Lanes are
//! polled with the loop's own task context (no `FuturesUnordered`, no
//! custom wakers), the discipline the bot controller and the session
//! workflow's tool batches follow. Everything that needs no I/O lives in
//! [`channels::state`] or in the pure functions at the end of this file.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use api::{BotEventOutcome, ChannelConversationSnapshot, ChatGroupActivation, ChatScope};
use bots::signal::{BOT_DELIVERY_SIGNAL, BotDeliveryReceipt};
use channels::delivery::{
    ChannelDeliveryOperation, ChannelToolOperation, ReplyContext, parse_tool_operation,
    plan_delivery_commands, validate_delivery_result,
};
use channels::inbound::validate_inbound;
use channels::media::{MaintainChannelTypingInput, PrepareChannelMediaInput, PreparedMediaItem};
use channels::policy::{
    ActivationMode, Classification, ControlCommand, DropReason, classify_inbound,
    parse_control_command,
};
use channels::state::{
    ApplyEmissionEffect, ChatHandle, ConversationCarry, ConversationEmission, ConversationState,
    DeliveryFallback, DeliveryStatus, EmissionBody as ConversationEmissionBody, FallbackStatus,
    InvocationStatus, MessageStatus, PolicyResponse, PolicyResponseKind, PolicyResponseStatus,
    ReceiptEffect, ReceivedMessage,
};
use engine::{
    BlobRef, EmissionBody, EmissionEnvelope, EmissionProducer, PromiseId, PromiseResolution,
    REPLY_COMPLETION_KEY, WorkflowEndpointRef, WorkflowToolInvocation,
};
use futures::future::{join_all, poll_fn};
use futures::{pin_mut, select};
use serde::{Deserialize, Serialize};
use temporalio_common::ActivityDefinition;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityExecutionError, ActivityOptions, CancellableFuture, ContinueAsNewOptions,
    SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult, WorkflowTermination,
};

use super::{
    AdmittedInbound, CHANNEL_CONVERSATION_WORKFLOW_KIND, CHANNEL_INBOUND_INBOX_CAP,
    CHAT_INBOUND_SIGNAL, CHAT_STATE_QUERY, ChannelActivities, ChatAssertTriggerActiveRequest,
    ChatEmitEventRequest, ChatEmitEventResult, ChatMessage, ChatPutJsonBlobRequest,
    ChatReadJsonBlobRequest, ChatReconcileDeliveryRequest, ChatReconcileDeliveryResult,
    ChatRefusalReason, ChatResolveHandleRequest, ChatStoreSentRequest, ChatToolDeclarationsRequest,
    ChatTriggerActiveResult, ConnectorActivities, ConversationStart, channel_activity_options,
    channel_assert_active_options, connector_delivery_options, connector_media_options,
    connector_typing_options,
};
use crate::AgentSessionWorkflow;

/// Start argument: the conversation this workflow fronts and, after a
/// continue-as-new, what the previous execution carried over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelConversationArgs {
    pub start: ConversationStart,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry: Option<ConversationCarry>,
}

/// The reply a direct chat gets when its sender may not take a turn or
/// steer the conversation; groups stay silent.
const DENIED_TEXT: &str = "This channel identity is not authorized for this Lightspeed universe.";

#[workflow(name = "ChannelConversationWorkflow")]
pub struct ChannelConversationWorkflow {
    start: ConversationStart,
    state: ConversationState,
    /// Why the start could not be accepted; `run` fails with it.
    start_error: Option<String>,
    inbound_inbox: VecDeque<AdmittedInbound>,
    emission_inbox: VecDeque<EmissionEnvelope>,
    receipt_inbox: VecDeque<BotDeliveryReceipt>,
    /// Bumped by every lane that finished, so a parked loop re-evaluates
    /// its continue-as-new gate.
    lane_tick: u64,
}

/// The workflow context every loop helper and lane works through. Helpers
/// take it by shared reference: state access and activity starts need no
/// exclusive borrow, which is what lets lanes hold their own clone.
type Ctx = WorkflowContext<ChannelConversationWorkflow>;

/// A detached unit of work polled beside the loop: one pushed `message_*`
/// invocation, one typing pulse, or one reply fallback. Each finishes its
/// own bookkeeping through its context clone, so its output is `()`.
type LaneFuture = Pin<Box<dyn Future<Output = ()>>>;

#[workflow_methods]
impl ChannelConversationWorkflow {
    /// State exists before the first signal is dispatched, so the inbound
    /// riding the signal-with-start lands on the initialized conversation.
    #[init]
    pub fn new(_ctx: &WorkflowContextView, args: ChannelConversationArgs) -> Self {
        let ChannelConversationArgs { start, carry } = args;
        let (state, start_error) = match accept_start(&start, carry) {
            Ok(state) => (state, None),
            Err(message) => (ConversationState::new(&start), Some(message)),
        };
        Self {
            start,
            state,
            start_error,
            inbound_inbox: VecDeque::new(),
            emission_inbox: VecDeque::new(),
            receipt_inbox: VecDeque::new(),
            lane_tick: 0,
        }
    }

    #[run]
    pub async fn run(ctx: &mut WorkflowContext<Self>) -> WorkflowResult<()> {
        run_conversation(ctx).await
    }

    /// An authorized message of this conversation. A message for another
    /// route or scope is a protocol error; beyond the inbox ceiling the
    /// message is shed and counted (the provider redelivers on its own
    /// schedule).
    #[signal(name = CHAT_INBOUND_SIGNAL)]
    pub fn chat_inbound(&mut self, _ctx: &mut SyncWorkflowContext<Self>, inbound: AdmittedInbound) {
        if let Err(error) = validate_inbound(&inbound.inbound.inbound)
            .and_then(|()| self.start.check_inbound(&inbound.inbound))
        {
            self.state
                .protocol_errors
                .push(format!("chat_inbound rejected: {error}"));
            return;
        }
        if self.inbound_inbox.len() >= CHANNEL_INBOUND_INBOX_CAP {
            self.state.overloaded_inbound_count += 1;
            tracing::warn!(
                message_id = %inbound.inbound.inbound.message_id,
                cap = CHANNEL_INBOUND_INBOX_CAP,
                "conversation inbox overloaded; inbound shed"
            );
            return;
        }
        self.inbound_inbox.push_back(inbound);
    }

    /// Pushed `message_*` invocations and their cancellations from the
    /// bot's routed sessions.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        envelope: EmissionEnvelope,
    ) {
        self.emission_inbox.push_back(envelope);
    }

    /// The bot controller's `started` / `finished` word on a delivery this
    /// conversation's events went into.
    #[signal(name = BOT_DELIVERY_SIGNAL)]
    pub fn bot_delivery(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        receipt: BotDeliveryReceipt,
    ) {
        self.receipt_inbox.push_back(receipt);
    }

    #[query(name = CHAT_STATE_QUERY)]
    pub fn chat_state(&self, _ctx: &WorkflowContextView) -> ChannelConversationSnapshot {
        self.state.snapshot()
    }
}

impl ChannelConversationWorkflow {
    fn inboxes_empty(&self) -> bool {
        self.inbound_inbox.is_empty()
            && self.emission_inbox.is_empty()
            && self.receipt_inbox.is_empty()
    }

    /// Whether a parked loop should wake, given the lane tick it observed
    /// when it parked.
    fn wake_ready(&self, lane_tick: u64) -> bool {
        !self.inboxes_empty() || self.lane_tick != lane_tick
    }

    /// Continue as new only when the server suggests it and nothing is in
    /// flight: empty inboxes and no lane (delivery, typing, fallback).
    fn can_continue_as_new(&self, suggested: bool, lanes_active: bool) -> bool {
        suggested && !lanes_active && self.inboxes_empty()
    }
}

// ── The loop ────────────────────────────────────────────────────────────────

async fn run_conversation(ctx: &mut Ctx) -> WorkflowResult<()> {
    let ctx: &Ctx = &*ctx;
    if let Some(message) = ctx.state(|wf| wf.start_error.clone()) {
        return Err(anyhow::anyhow!("conversation start rejected: {message}").into());
    }
    if ctx.state(|wf| wf.start.workflow_id()) != ctx.workflow_id() {
        return Err(anyhow::anyhow!(
            "conversation workflow id does not match the conversation it was started for"
        )
        .into());
    }
    ensure_tool_declarations(ctx).await?;

    let mut lanes: Vec<LaneFuture> = Vec::new();
    loop {
        with_lanes(&mut lanes, process_inbound(ctx)).await;
        process_emissions(ctx, &mut lanes);
        process_receipts(ctx, &mut lanes);
        let suggested = ctx.continue_as_new_suggested();
        if ctx.state(|wf| wf.can_continue_as_new(suggested, !lanes.is_empty())) {
            return request_continue_as_new(ctx);
        }
        park(ctx, &mut lanes).await?;
    }
}

/// Content-addressed and receiver-specific: the same bytes for every event
/// of this conversation, so the routed session's declaration fingerprint
/// never drifts. Stored once per conversation lifetime; a failure fails the
/// workflow (the next message starts a fresh one).
async fn ensure_tool_declarations(ctx: &Ctx) -> Result<(), WorkflowTermination> {
    if ctx.state(|wf| wf.state.tools_ref.is_some()) {
        return Ok(());
    }
    let request = ctx.state(|wf| ChatToolDeclarationsRequest {
        universe_id: wf.start.universe_id,
        receiver: receiver(ctx),
    });
    let declared = activity(
        ctx,
        ChannelActivities::chat_tool_declarations,
        request,
        channel_activity_options(),
    )
    .await
    .map_err(|message| anyhow::anyhow!("store chat tool declarations: {message}"))?;
    ctx.state_mut(|wf| wf.state.tools_ref = Some(declared.tools_ref));
    Ok(())
}

/// Wait for work (an inbox entry or a finished lane) or workflow
/// cancellation. Lanes keep running underneath.
async fn park(ctx: &Ctx, lanes: &mut Vec<LaneFuture>) -> Result<(), WorkflowTermination> {
    let tick = ctx.state(|wf| wf.lane_tick);
    let wait = ctx.wait_condition(move |wf| wf.wake_ready(tick));
    let cancelled = ctx.cancelled();
    with_lanes(lanes, async move {
        pin_mut!(wait, cancelled);
        select! {
            _ = wait => Ok(()),
            _ = cancelled => Err(WorkflowTermination::Cancelled),
        }
    })
    .await
}

/// Drive `fut` to completion while polling every lane with the same task
/// context; finished lanes are dropped. Polling order is fixed, and no
/// combinator with its own waker machinery is involved, which keeps
/// replay deterministic (TMPRL1100).
async fn with_lanes<T>(lanes: &mut Vec<LaneFuture>, fut: impl Future<Output = T>) -> T {
    pin_mut!(fut);
    poll_fn(|cx: &mut Context<'_>| {
        if let Poll::Ready(value) = fut.as_mut().poll(cx) {
            return Poll::Ready(value);
        }
        poll_lanes(lanes, cx);
        Poll::Pending
    })
    .await
}

fn poll_lanes(lanes: &mut Vec<LaneFuture>, cx: &mut Context<'_>) {
    let mut index = 0;
    while index < lanes.len() {
        match lanes[index].as_mut().poll(cx) {
            Poll::Ready(()) => drop(lanes.remove(index)),
            Poll::Pending => index += 1,
        }
    }
}

fn request_continue_as_new(ctx: &Ctx) -> WorkflowResult<()> {
    let args = ctx.state(|wf| ChannelConversationArgs {
        start: wf.start.clone(),
        carry: Some(wf.state.compact_state()),
    });
    match ctx.continue_as_new(&args, ContinueAsNewOptions::default()) {
        Ok(never) => match never {},
        Err(termination) => Err(termination),
    }
}

fn receiver(ctx: &Ctx) -> WorkflowEndpointRef {
    WorkflowEndpointRef {
        workflow_id: ctx.workflow_id().to_owned(),
        workflow_kind: CHANNEL_CONVERSATION_WORKFLOW_KIND.to_owned(),
    }
}

// ── Activities ──────────────────────────────────────────────────────────────

/// One activity with explicit options; the failure text is what the
/// conversation records.
async fn activity<AD: ActivityDefinition>(
    ctx: &Ctx,
    definition: AD,
    input: AD::Input,
    options: ActivityOptions,
) -> Result<AD::Output, String> {
    ctx.start_activity(definition, input, options)
        .await
        .map_err(|error| error.to_string())
}

/// Why a delivery step did not complete: the activity (or a check before
/// it) failed, or the holder cancelled the invocation the step belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StepError {
    Failed(String),
    Cancelled,
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(message) => f.write_str(message),
            Self::Cancelled => f.write_str("invocation cancelled"),
        }
    }
}

/// Run one activity of a lane. With `watch` set to an invocation id the
/// step is cancellable: it never starts once the holder cancelled that
/// invocation, and an in-flight activity is abandoned (`TryCancel`) the
/// moment the cancellation lands. Without `watch` the step runs to its
/// own completion.
async fn step<T, F>(
    ctx: &Ctx,
    watch: Option<&str>,
    start: impl FnOnce() -> F,
) -> Result<T, StepError>
where
    F: CancellableFuture<Result<T, ActivityExecutionError>>,
{
    let Some(invocation_id) = watch else {
        return start()
            .await
            .map_err(|error| StepError::Failed(error.to_string()));
    };
    if ctx.state(|wf| wf.state.is_cancelled(invocation_id)) {
        return Err(StepError::Cancelled);
    }
    let activity = start();
    let cancelled = ctx.wait_condition(|wf| wf.state.is_cancelled(invocation_id));
    pin_mut!(activity, cancelled);
    select! {
        result = activity => result.map_err(|error| StepError::Failed(error.to_string())),
        _ = cancelled => {
            activity.cancel();
            let _ = activity.await;
            Err(StepError::Cancelled)
        }
    }
}

/// The trigger, bot, account, and pairing still serve this chat.
async fn assert_trigger_active(ctx: &Ctx, watch: Option<&str>) -> Result<(), StepError> {
    let request = ctx.state(|wf| ChatAssertTriggerActiveRequest {
        universe_id: wf.start.universe_id,
        bot_id: wf.start.bot_id.clone(),
        trigger_id: wf.start.trigger_id.clone(),
        account_id: wf.start.account_id.clone(),
        chat_id: wf.start.conversation.chat_id.clone(),
        scope: wf.start.scope,
    });
    match step(ctx, watch, || {
        ctx.start_activity(
            ChannelActivities::assert_trigger_active,
            request,
            channel_assert_active_options(),
        )
    })
    .await?
    {
        ChatTriggerActiveResult::Active => Ok(()),
        ChatTriggerActiveResult::Inactive { reason } => Err(StepError::Failed(format!(
            "trigger no longer serves this conversation: {reason}"
        ))),
    }
}

/// Execute one operation as its planned connector commands (a long send is
/// several chunks, each its own durable activity) and collect every
/// provider message id.
async fn deliver_planned(
    ctx: &Ctx,
    watch: Option<&str>,
    invocation_id: &str,
    operation: &ChannelDeliveryOperation,
) -> Result<Vec<String>, StepError> {
    let (provider, route, queue) = ctx.state(|wf| {
        (
            wf.start.provider.clone(),
            wf.start.route(),
            wf.start.connector_task_queue.clone(),
        )
    });
    let commands =
        plan_delivery_commands(invocation_id, &route, operation).map_err(StepError::Failed)?;
    let mut message_ids = Vec::new();
    for command in commands {
        let result = step(ctx, watch, || {
            ctx.start_activity(
                ConnectorActivities::deliver_channel_message,
                command,
                connector_delivery_options(queue.clone()),
            )
        })
        .await?;
        validate_delivery_result(&result, &provider).map_err(StepError::Failed)?;
        message_ids.extend(result.message_ids);
    }
    Ok(message_ids)
}

async fn put_json_blob(ctx: &Ctx, value: serde_json::Value) -> Result<BlobRef, String> {
    let universe_id = ctx.state(|wf| wf.start.universe_id);
    let stored = activity(
        ctx,
        ChannelActivities::put_json_blob,
        ChatPutJsonBlobRequest { universe_id, value },
        channel_activity_options(),
    )
    .await?;
    BlobRef::parse(stored.blob_ref).map_err(|error| error.to_string())
}

/// Best effort: a failed error blob resolves the promise without one.
async fn put_error_blob(ctx: &Ctx, message: &str) -> Option<BlobRef> {
    put_json_blob(ctx, serde_json::json!({ "error": message }))
        .await
        .ok()
}

/// Resolve the holder session's `reply` promise by signalling the session
/// workflow directly; the emission id when the signal went out.
async fn resolve_promise(
    ctx: &Ctx,
    holder_workflow_id: &str,
    promise_id: PromiseId,
    resolution: PromiseResolution,
) -> Vec<String> {
    let universe_id = ctx.state(|wf| wf.start.universe_id);
    let envelope = EmissionEnvelope::source_resolution(
        universe_id,
        ctx.workflow_id().to_owned(),
        holder_workflow_id,
        promise_id,
        resolution,
    );
    let emission_id = envelope.emission_id.as_str().to_owned();
    match ctx
        .external_workflow(holder_workflow_id.to_owned(), None)
        .signal(AgentSessionWorkflow::deliver_emission, envelope)
        .await
    {
        Ok(_) => vec![emission_id],
        Err(failure) => {
            ctx.state_mut(|wf| {
                wf.state.protocol_errors.push(format!(
                    "resolve invocation at {holder_workflow_id}: {}",
                    failure.message
                ))
            });
            Vec::new()
        }
    }
}

// ── Inbound ─────────────────────────────────────────────────────────────────

async fn process_inbound(ctx: &Ctx) {
    while let Some(inbound) = ctx.state_mut(|wf| wf.inbound_inbox.pop_front()) {
        handle_inbound(ctx, inbound).await;
    }
}

fn message(inbound: &AdmittedInbound) -> &api::ChannelInbound {
    &inbound.inbound.inbound
}

async fn handle_inbound(ctx: &Ctx, inbound: AdmittedInbound) {
    let Some(inbound_key) = ctx.state_mut(|wf| wf.state.apply_inbound(&inbound.inbound)) else {
        tracing::debug!(message_id = %message(&inbound).message_id, "duplicate inbound");
        return;
    };
    match ctx.state(|wf| plan_inbound(&wf.state, &inbound)) {
        InboundPlan::Control(command) => {
            let text = ctx.state_mut(|wf| control_reply(&mut wf.state, command));
            send_policy_response(
                ctx,
                &inbound_key,
                &inbound,
                PolicyResponseKind::Control,
                text,
            )
            .await;
        }
        InboundPlan::Denied { reply } => {
            ctx.state_mut(|wf| wf.state.denied_inbound_count += 1);
            tracing::info!(message_id = %message(&inbound).message_id, "inbound denied");
            if reply {
                send_policy_response(
                    ctx,
                    &inbound_key,
                    &inbound,
                    PolicyResponseKind::Denied,
                    DENIED_TEXT.to_owned(),
                )
                .await;
            }
        }
        InboundPlan::Drop(reason) => {
            ctx.state_mut(|wf| wf.state.dropped_inbound_count += 1);
            tracing::debug!(message_id = %message(&inbound).message_id, ?reason, "inbound dropped");
        }
        InboundPlan::Emit { text } => {
            let (media, transcript_refs) =
                match prepare_message_media(ctx, &inbound, &inbound_key).await {
                    Ok(media) => media,
                    Err(error) => {
                        let message_id = message(&inbound).message_id.clone();
                        ctx.state_mut(|wf| {
                            wf.state.messages.insert(
                                inbound_key,
                                ReceivedMessage {
                                    message_id: message_id.clone(),
                                    status: MessageStatus::Failed,
                                    seq: None,
                                    session_id: None,
                                    error: Some(error.clone()),
                                },
                            );
                            wf.state
                                .protocol_errors
                                .push(format!("media {message_id}: {error}"));
                        });
                        return;
                    }
                };
            emit_message(ctx, inbound_key, &inbound, text, media, transcript_refs).await;
        }
    }
}

async fn prepare_message_media(
    ctx: &Ctx,
    inbound: &AdmittedInbound,
    key: &str,
) -> Result<
    (
        Vec<PreparedMediaItem>,
        std::collections::BTreeMap<String, String>,
    ),
    String,
> {
    let media = prepare_media(ctx, inbound).await?;
    let mut transcripts = std::collections::BTreeMap::new();
    for (index, item) in media.iter().enumerate() {
        if item.kind != api::ChannelMediaKind::Audio {
            continue;
        }
        let request = ctx.state(|wf| super::ChatTranscribeMediaRequest {
            active: ChatAssertTriggerActiveRequest {
                universe_id: wf.start.universe_id,
                bot_id: wf.start.bot_id.clone(),
                trigger_id: wf.start.trigger_id.clone(),
                account_id: wf.start.account_id.clone(),
                chat_id: wf.start.conversation.chat_id.clone(),
                scope: wf.start.scope,
            },
            idempotency_key: crate::transcription_id(
                &api::Attribution::Local,
                &format!("{}:{key}:{index}", wf.start.conversation.key()),
            ),
            audio: api::TranscriptionAudio {
                blob_ref: item.blob_ref.clone(),
                mime: item.mime.clone(),
                name: item.name.clone().unwrap_or_else(|| "audio".into()),
            },
        });
        loop {
            let view = activity(
                ctx,
                ChannelActivities::transcribe_media,
                request.clone(),
                channel_activity_options(),
            )
            .await?;
            match view.status {
                api::TranscriptionStatus::Pending | api::TranscriptionStatus::Running => {
                    let cancelled = {
                        let timer = ctx.timer(std::time::Duration::from_secs(2));
                        let cancelled = ctx.cancelled();
                        pin_mut!(timer, cancelled);
                        select! { _ = timer => false, _ = cancelled => true }
                    };
                    if cancelled {
                        let _ = ctx
                            .external_workflow(
                                format!("{}/{}", request.active.universe_id, view.transcription_id),
                                None,
                            )
                            .signal(crate::TranscriptionWorkflow::cancel, ())
                            .await;
                        return Err("Audio preparation cancelled.".into());
                    }
                }
                api::TranscriptionStatus::Succeeded => {
                    transcripts.insert(
                        item.blob_ref.clone(),
                        view.transcript_ref.ok_or("missing transcript reference")?,
                    );
                    break;
                }
                _ => {
                    return Err(view.failure.map(|f| f.message).unwrap_or_else(|| {
                        "Transcription unavailable; resend the recording.".into()
                    }));
                }
            }
        }
    }
    Ok((media, transcripts))
}

/// Download every attachment through the connector and put it in the CAS;
/// the first failure fails the message.
async fn prepare_media(
    ctx: &Ctx,
    inbound: &AdmittedInbound,
) -> Result<Vec<PreparedMediaItem>, String> {
    let media = &message(inbound).media;
    if media.is_empty() {
        return Ok(Vec::new());
    }
    let (universe_id, route, queue) = ctx.state(|wf| {
        (
            wf.start.universe_id,
            wf.start.route(),
            wf.start.connector_task_queue.clone(),
        )
    });
    // At most `MAX_CHANNEL_MEDIA_PER_MESSAGE` items: `join_all` polls each
    // with this task's context (no internal wakers below its small-set
    // threshold).
    let prepared = join_all(media.iter().map(|item| {
        ctx.start_activity(
            ConnectorActivities::prepare_channel_media,
            PrepareChannelMediaInput {
                universe_id,
                route: route.clone(),
                media: item.clone(),
            },
            connector_media_options(queue.clone()),
        )
    }))
    .await;
    prepared
        .into_iter()
        .map(|result| {
            result
                .map(|result| result.item)
                .map_err(|error| error.to_string())
        })
        .collect()
}

/// One activated message → one bot event; the number it gets is its handle.
async fn emit_message(
    ctx: &Ctx,
    inbound_key: String,
    inbound: &AdmittedInbound,
    text: String,
    media: Vec<PreparedMediaItem>,
    transcript_refs: std::collections::BTreeMap<String, String>,
) {
    let chat = message(inbound);
    let request = ctx.state_mut(|wf| {
        wf.state.messages.insert(
            inbound_key.clone(),
            ReceivedMessage::emitting(&chat.message_id),
        );
        ChatEmitEventRequest {
            universe_id: wf.start.universe_id,
            bot_id: wf.start.bot_id.clone(),
            trigger_id: wf.start.trigger_id.clone(),
            account_id: wf.start.account_id.clone(),
            provider: wf.start.provider.clone(),
            conversation: wf.start.conversation.clone(),
            label: wf.start.label.clone(),
            scope: wf.start.scope,
            message: ChatMessage {
                message_id: chat.message_id.clone(),
                sender_id: chat.sender_id.clone(),
                sender_name: chat.sender_name.clone(),
                timestamp_ms: chat.timestamp_ms,
                text,
                is_direct: chat.is_direct,
                mentioned_bot: chat.mentioned_bot,
                is_reply_to_bot: chat.is_reply_to_bot,
            },
            media,
            transcript_refs,
            tools_ref: wf
                .state
                .tools_ref
                .clone()
                .expect("tool declarations are stored before the loop emits"),
            notify: receiver(ctx),
            // The token is the inbound key: receipts for a coalesced batch
            // name every message in it, and the delivery id dedupes the
            // fallback.
            notify_token: inbound_key.clone(),
        }
    });
    let outcome = activity(
        ctx,
        ChannelActivities::emit_chat_event,
        request,
        channel_activity_options(),
    )
    .await;
    let mut record = ReceivedMessage::emitting(&chat.message_id);
    let mut handle_seq = None;
    match &outcome {
        Ok(ChatEmitEventResult::Admitted {
            seq, session_id, ..
        }) => {
            record.status = MessageStatus::Emitted;
            record.seq = Some(*seq);
            record.session_id = session_id.clone();
            handle_seq = Some(*seq);
        }
        Ok(ChatEmitEventResult::Duplicate { seq, .. }) => {
            record.status = MessageStatus::Duplicate;
            record.seq = Some(*seq);
            handle_seq = Some(*seq);
        }
        Ok(ChatEmitEventResult::Filtered { .. }) => record.status = MessageStatus::Filtered,
        Ok(ChatEmitEventResult::Refused { reason }) => {
            record.status = MessageStatus::Refused;
            record.error = Some(refusal_reason_name(*reason).to_owned());
        }
        Err(error) => {
            record.status = MessageStatus::Failed;
            record.error = Some(error.clone());
        }
    }
    tracing::info!(
        message_id = %chat.message_id,
        status = ?record.status,
        seq = ?record.seq,
        "inbound emitted"
    );
    ctx.state_mut(|wf| {
        if let Some(seq) = handle_seq {
            wf.state.remember_handle(
                seq,
                ChatHandle {
                    provider_message_ids: vec![chat.message_id.clone()],
                    from_me: false,
                    sender_id: Some(chat.sender_id.clone()),
                    text: Some(chat.text.clone()),
                },
            );
        }
        if record.status == MessageStatus::Emitted {
            wf.state.emitted_count += 1;
        }
        if let Err(error) = &outcome {
            wf.state
                .protocol_errors
                .push(format!("emit {}: {error}", chat.message_id));
        }
        wf.state.messages.insert(inbound_key, record);
    });
}

/// Channels-authored replies (control, denial) go straight out; they are
/// not chat events and get no number.
async fn send_policy_response(
    ctx: &Ctx,
    inbound_key: &str,
    inbound: &AdmittedInbound,
    kind: PolicyResponseKind,
    text: String,
) {
    let chat = message(inbound);
    ctx.state_mut(|wf| {
        wf.state.policy_responses.insert(
            inbound_key,
            PolicyResponse {
                kind,
                status: PolicyResponseStatus::Delivering,
                provider_message_ids: Vec::new(),
                error: None,
            },
        );
    });
    let operation = ChannelDeliveryOperation::Send {
        text,
        reply_to: Some(chat.message_id.clone()),
        reply_context: Some(ReplyContext {
            sender_id: chat.sender_id.clone(),
            text: chat.text.clone(),
        }),
    };
    let delivered = async {
        assert_trigger_active(ctx, None).await?;
        deliver_planned(
            ctx,
            None,
            &format!("policy:{}", chat.message_id),
            &operation,
        )
        .await
    }
    .await;
    ctx.state_mut(|wf| {
        let Some(response) = wf.state.policy_responses.get_mut(inbound_key) else {
            return;
        };
        match delivered {
            Ok(message_ids) => {
                response.status = PolicyResponseStatus::Delivered;
                response.provider_message_ids = message_ids;
            }
            Err(error) => {
                let error = error.to_string();
                response.status = PolicyResponseStatus::Failed;
                response.error = Some(error.clone());
                wf.state
                    .protocol_errors
                    .push(format!("policy response {}: {error}", chat.message_id));
            }
        }
    });
}

// ── Emissions ───────────────────────────────────────────────────────────────

/// A pushed `message_*` invocation with the endpoint its reply goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PushedInvocation {
    invocation: WorkflowToolInvocation,
    holder_workflow_id: String,
}

/// Drain the emission inbox: dedupe and record through the state, then
/// answer each new invocation in its own lane. A cancellation only lands
/// in state; the lane it targets observes it between (and during) steps.
fn process_emissions(ctx: &Ctx, lanes: &mut Vec<LaneFuture>) {
    while let Some(envelope) = ctx.state_mut(|wf| wf.emission_inbox.pop_front()) {
        let (emission, pushed) = project_emission(envelope);
        match ctx.state_mut(|wf| wf.state.apply_emission(&emission)) {
            ApplyEmissionEffect::InvocationReceived { invocation_id } => {
                let Some(pushed) = pushed else {
                    ctx.state_mut(|wf| {
                        wf.state
                            .protocol_errors
                            .push(format!("missing invocation {invocation_id}"))
                    });
                    continue;
                };
                let reply_promise_id =
                    match ctx.state(|wf| reply_promise_for(&wf.start, &pushed.invocation)) {
                        Ok(promise_id) => promise_id,
                        Err(error) => {
                            ctx.state_mut(|wf| {
                                mark_invocation_invalid(&mut wf.state, &invocation_id, &error)
                            });
                            continue;
                        }
                    };
                if ctx.state(|wf| wf.state.is_cancelled(&invocation_id)) {
                    // The holder gave up before the invocation reached us:
                    // nothing to deliver, only the (already terminal)
                    // promise to acknowledge.
                    ctx.state_mut(|wf| wf.state.invocation_cancelled(&invocation_id, Vec::new()));
                    lanes.push(Box::pin(resolve_cancelled(
                        ctx.clone(),
                        invocation_id,
                        pushed.holder_workflow_id,
                        reply_promise_id,
                    )));
                    continue;
                }
                ctx.state_mut(|wf| wf.state.invocation_delivering(&invocation_id));
                lanes.push(Box::pin(run_invocation(
                    ctx.clone(),
                    pushed,
                    reply_promise_id,
                )));
            }
            ApplyEmissionEffect::InvocationCancelled { invocation_id, .. } => {
                tracing::info!(%invocation_id, "invocation cancellation received");
            }
            ApplyEmissionEffect::None => {}
        }
    }
}

/// What a delivered invocation produced: provider ids, the send's number,
/// and the receipt document the model reads.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Delivered {
    message_ids: Vec<String>,
    sent_seq: Option<u64>,
    receipt: serde_json::Value,
}

enum LaneEnd {
    Resolved {
        delivered: Delivered,
        payload_ref: BlobRef,
    },
    Failed(String),
    Cancelled,
}

/// Answer one pushed `message_*` invocation: deliver, archive a send, write
/// the receipt, resolve the holder's `reply` promise, and record the
/// outcome. The lane never fails the workflow.
async fn run_invocation(ctx: Ctx, pushed: PushedInvocation, reply_promise_id: PromiseId) {
    let invocation_id = pushed.invocation.invocation_id.as_str().to_owned();
    let end = match deliver_invocation(&ctx, &pushed.invocation).await {
        Ok(delivered) => match put_json_blob(&ctx, delivered.receipt.clone()).await {
            Ok(payload_ref) => LaneEnd::Resolved {
                delivered,
                payload_ref,
            },
            Err(error) => LaneEnd::Failed(error),
        },
        Err(StepError::Failed(error)) => LaneEnd::Failed(error),
        Err(StepError::Cancelled) => LaneEnd::Cancelled,
    };
    let resolution = match &end {
        LaneEnd::Resolved { payload_ref, .. } => PromiseResolution::Resolved {
            payload_ref: Some(payload_ref.clone()),
        },
        LaneEnd::Failed(error) => PromiseResolution::Failed {
            error_ref: put_error_blob(&ctx, error).await,
        },
        LaneEnd::Cancelled => PromiseResolution::Cancelled,
    };
    tracing::info!(
        %invocation_id,
        tool_id = %pushed.invocation.tool_id,
        outcome = match &end {
            LaneEnd::Resolved { .. } => "resolved",
            LaneEnd::Failed(_) => "failed",
            LaneEnd::Cancelled => "cancelled",
        },
        "invocation finished"
    );
    let emission_ids = resolve_promise(
        &ctx,
        &pushed.holder_workflow_id,
        reply_promise_id,
        resolution,
    )
    .await;
    ctx.state_mut(|wf| {
        match end {
            LaneEnd::Resolved { delivered, .. } => {
                wf.state.invocation_resolved(
                    &invocation_id,
                    delivered.message_ids,
                    delivered.sent_seq,
                    emission_ids,
                );
            }
            LaneEnd::Failed(error) => {
                wf.state.invocation_failed(&invocation_id, error);
            }
            LaneEnd::Cancelled => {
                wf.state.invocation_cancelled(&invocation_id, emission_ids);
            }
        }
        wf.lane_tick += 1;
    });
}

/// The steps up to and including the provider delivery are cancellable;
/// once the provider acknowledged a send, archiving it (so it has a
/// number) is not.
async fn deliver_invocation(
    ctx: &Ctx,
    invocation: &WorkflowToolInvocation,
) -> Result<Delivered, StepError> {
    let invocation_id = invocation.invocation_id.as_str();
    let watch = Some(invocation_id);
    assert_trigger_active(ctx, watch).await?;
    let universe_id = ctx.state(|wf| wf.start.universe_id);
    let arguments = step(ctx, watch, || {
        ctx.start_activity(
            ChannelActivities::read_json_blob,
            ChatReadJsonBlobRequest {
                universe_id,
                blob_ref: invocation.arguments_ref.as_str().to_owned(),
            },
            channel_activity_options(),
        )
    })
    .await?;
    let requested =
        parse_tool_operation(invocation.tool_id.as_str(), &arguments).map_err(StepError::Failed)?;
    let target = match referenced_handle(&requested) {
        Some(seq) => Some(resolve_handle(ctx, watch, seq).await?),
        None => None,
    };
    let operation =
        to_delivery_operation(&requested, target.as_ref()).map_err(StepError::Failed)?;
    let message_ids = deliver_planned(ctx, watch, invocation_id, &operation).await?;
    match requested {
        ChannelToolOperation::Send { text, reply_to } => {
            let request = ctx.state(|wf| ChatStoreSentRequest {
                universe_id,
                bot_id: wf.start.bot_id.clone(),
                trigger_id: wf.start.trigger_id.clone(),
                account_id: wf.start.account_id.clone(),
                provider: wf.start.provider.clone(),
                conversation: wf.start.conversation.clone(),
                label: wf.start.label.clone(),
                invocation_key: invocation_id.to_owned(),
                text: text.clone(),
                provider_message_ids: message_ids.clone(),
                reply_to,
            });
            let stored = step(ctx, None, || {
                ctx.start_activity(
                    ChannelActivities::store_chat_sent,
                    request,
                    channel_activity_options(),
                )
            })
            .await?;
            ctx.state_mut(|wf| {
                wf.state.remember_handle(
                    stored.seq,
                    ChatHandle {
                        provider_message_ids: message_ids.clone(),
                        from_me: true,
                        sender_id: None,
                        text: Some(text),
                    },
                )
            });
            Ok(Delivered {
                message_ids,
                sent_seq: Some(stored.seq),
                receipt: serde_json::json!({ "sent": stored.seq }),
            })
        }
        ChannelToolOperation::Edit { message, .. }
        | ChannelToolOperation::React { message, .. } => Ok(Delivered {
            message_ids,
            sent_seq: None,
            receipt: serde_json::json!({ "message": message }),
        }),
    }
}

/// A message number → provider ids and direction: the workflow's cache
/// first, the bot's event log second.
async fn resolve_handle(ctx: &Ctx, watch: Option<&str>, seq: u64) -> Result<ChatHandle, StepError> {
    if let Some(handle) = ctx.state(|wf| wf.state.handles.get(&seq).cloned()) {
        return Ok(handle);
    }
    let request = ctx.state(|wf| ChatResolveHandleRequest {
        universe_id: wf.start.universe_id,
        bot_id: wf.start.bot_id.clone(),
        conversation_key: wf.start.conversation.key(),
        seq,
    });
    let resolved = step(ctx, watch, || {
        ctx.start_activity(
            ChannelActivities::resolve_chat_handle,
            request,
            channel_activity_options(),
        )
    })
    .await?;
    match resolved.handle {
        Some(handle) => {
            ctx.state_mut(|wf| wf.state.remember_handle(seq, handle.clone()));
            Ok(handle)
        }
        None => Err(StepError::Failed(unknown_handle(seq, resolved.max_seq))),
    }
}

/// An invocation that arrived after its own cancellation: acknowledge the
/// terminal promise and record it.
async fn resolve_cancelled(
    ctx: Ctx,
    invocation_id: String,
    holder_workflow_id: String,
    reply_promise_id: PromiseId,
) {
    let emission_ids = resolve_promise(
        &ctx,
        &holder_workflow_id,
        reply_promise_id,
        PromiseResolution::Cancelled,
    )
    .await;
    ctx.state_mut(|wf| {
        wf.state.invocation_cancelled(&invocation_id, emission_ids);
        wf.lane_tick += 1;
    });
}

// ── Receipts ────────────────────────────────────────────────────────────────

/// The bot controller's word on a delivery: typing while the run is up
/// and, once it finished, the reply fallback when the model answered in
/// text without a messaging tool. One fallback per delivery, whatever the
/// batch: the state ignores a second `finished`.
fn process_receipts(ctx: &Ctx, lanes: &mut Vec<LaneFuture>) {
    while let Some(receipt) = ctx.state_mut(|wf| wf.receipt_inbox.pop_front()) {
        match ctx.state_mut(|wf| wf.state.record_delivery_receipt(&receipt)) {
            ReceiptEffect::Ignored => {}
            ReceiptEffect::Started => {
                tracing::debug!(delivery_id = %receipt.delivery_id, "delivery started");
                lanes.push(Box::pin(run_typing(ctx.clone(), receipt.delivery_id)));
            }
            ReceiptEffect::Finished => {
                tracing::debug!(
                    delivery_id = %receipt.delivery_id,
                    outcome = ?receipt.outcome,
                    "delivery finished"
                );
                match receipt_action(receipt.outcome) {
                    ReceiptAction::NoRun => {}
                    ReceiptAction::Reconcile => {
                        ctx.state_mut(|wf| {
                            wf.state.set_delivery_fallback(
                                &receipt.delivery_id,
                                DeliveryFallback::status(FallbackStatus::Reconciling),
                            )
                        });
                        lanes.push(Box::pin(run_fallback(ctx.clone(), receipt)));
                    }
                }
            }
        }
    }
}

/// Keep the provider's typing indicator on while the delivery is started;
/// the `finished` receipt (recorded in state) cancels the activity.
async fn run_typing(ctx: Ctx, delivery_id: String) {
    let (route, queue) = ctx.state(|wf| (wf.start.route(), wf.start.connector_task_queue.clone()));
    let typing = ctx.start_activity(
        ConnectorActivities::maintain_channel_typing,
        MaintainChannelTypingInput { route },
        connector_typing_options(queue),
    );
    let finished = ctx.wait_condition(|wf| !typing_wanted(&wf.state, &delivery_id));
    pin_mut!(typing, finished);
    select! {
        result = typing => match result {
            Ok(()) | Err(ActivityExecutionError::Cancelled(_)) => {}
            Err(error) => ctx.state_mut(|wf| {
                wf.state
                    .protocol_errors
                    .push(format!("typing {delivery_id}: {error}"))
            }),
        },
        _ = finished => {
            typing.cancel();
            let _ = typing.await;
        }
    }
    ctx.state_mut(|wf| wf.lane_tick += 1);
}

enum FallbackEnd {
    Suppressed,
    Delivered { message_ids: Vec<String>, seq: u64 },
}

/// The reply fallback of one finished delivery: nothing when the run
/// answered through a `message_*` tool, otherwise the assistant's final
/// text sent as the reply and archived with a number.
async fn run_fallback(ctx: Ctx, receipt: BotDeliveryReceipt) {
    let delivery_id = receipt.delivery_id.clone();
    let fallback = match reconcile_fallback(&ctx, &receipt).await {
        Ok(FallbackEnd::Suppressed) => DeliveryFallback::status(FallbackStatus::Suppressed),
        Ok(FallbackEnd::Delivered { message_ids, seq }) => DeliveryFallback {
            status: FallbackStatus::Delivered,
            provider_message_ids: message_ids,
            seq: Some(seq),
            error: None,
        },
        Err(error) => {
            ctx.state_mut(|wf| {
                wf.state
                    .protocol_errors
                    .push(format!("delivery {delivery_id}: {error}"))
            });
            DeliveryFallback {
                status: FallbackStatus::Failed,
                provider_message_ids: Vec::new(),
                seq: None,
                error: Some(error),
            }
        }
    };
    tracing::info!(%delivery_id, status = ?fallback.status, "delivery fallback settled");
    ctx.state_mut(|wf| {
        wf.state.set_delivery_fallback(&delivery_id, fallback);
        wf.lane_tick += 1;
    });
}

async fn reconcile_fallback(
    ctx: &Ctx,
    receipt: &BotDeliveryReceipt,
) -> Result<FallbackEnd, String> {
    let universe_id = ctx.state(|wf| wf.start.universe_id);
    let reconciled = activity(
        ctx,
        ChannelActivities::reconcile_delivery,
        ChatReconcileDeliveryRequest {
            universe_id,
            session_id: receipt.session_id.clone(),
            run_id: receipt.run_id.clone(),
            outcome: receipt.outcome,
        },
        channel_activity_options(),
    )
    .await?;
    let text = match reconciled {
        ChatReconcileDeliveryResult::Suppress { reason } => {
            tracing::debug!(delivery_id = %receipt.delivery_id, ?reason, "fallback suppressed");
            return Ok(FallbackEnd::Suppressed);
        }
        ChatReconcileDeliveryResult::Deliver { text } => text,
    };
    assert_trigger_active(ctx, None)
        .await
        .map_err(|error| error.to_string())?;
    let invocation_key = format!("fallback:{}", receipt.delivery_id);
    let operation = ChannelDeliveryOperation::Send {
        text: text.clone(),
        reply_to: None,
        reply_context: None,
    };
    let message_ids = deliver_planned(ctx, None, &invocation_key, &operation)
        .await
        .map_err(|error| error.to_string())?;
    let request = ctx.state(|wf| ChatStoreSentRequest {
        universe_id,
        bot_id: wf.start.bot_id.clone(),
        trigger_id: wf.start.trigger_id.clone(),
        account_id: wf.start.account_id.clone(),
        provider: wf.start.provider.clone(),
        conversation: wf.start.conversation.clone(),
        label: wf.start.label.clone(),
        invocation_key,
        text: text.clone(),
        provider_message_ids: message_ids.clone(),
        reply_to: None,
    });
    let stored = activity(
        ctx,
        ChannelActivities::store_chat_sent,
        request,
        channel_activity_options(),
    )
    .await?;
    ctx.state_mut(|wf| {
        wf.state.remember_handle(
            stored.seq,
            ChatHandle {
                provider_message_ids: message_ids.clone(),
                from_me: true,
                sender_id: None,
                text: Some(text),
            },
        )
    });
    Ok(FallbackEnd::Delivered {
        message_ids,
        seq: stored.seq,
    })
}

// ── Pure decisions ──────────────────────────────────────────────────────────

/// Validate the start and restore or initialize the conversation state.
fn accept_start(
    start: &ConversationStart,
    carry: Option<ConversationCarry>,
) -> Result<ConversationState, String> {
    validate_start(start)?;
    match carry {
        Some(carry) => ConversationState::restore(start, carry).map_err(|error| error.to_string()),
        None => Ok(ConversationState::new(start)),
    }
}

/// The secret-free start must name a real conversation of a real account.
/// (The typed ids validate themselves; `ChatActivation` carries no mode,
/// so the scope/mode consistency the TypeScript start checked has no
/// counterpart here.)
fn validate_start(start: &ConversationStart) -> Result<(), String> {
    if start.universe_id.is_nil() {
        return Err("universe id must not be nil".to_owned());
    }
    if start.label.trim().is_empty() {
        return Err("label must be a non-empty string".to_owned());
    }
    if start.connector_task_queue.trim().is_empty() {
        return Err("connector task queue must be a non-empty string".to_owned());
    }
    if start.conversation.chat_id.is_empty() {
        return Err("conversation chat id must be a non-empty string".to_owned());
    }
    if start
        .conversation
        .thread_id
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err("conversation thread id must be a non-empty string".to_owned());
    }
    if start.conversation.account_id != start.account_id {
        return Err("conversation account must be the start account".to_owned());
    }
    Ok(())
}

/// What one accepted (non-duplicate) inbound message becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum InboundPlan {
    /// A workflow-owned control command from an authorized controller.
    Control(ControlCommand),
    /// The sender may not do this (control, or a turn). A direct chat is
    /// told; a group stays silent.
    Denied { reply: bool },
    /// Not activated (empty, bare prefix, ambient group traffic).
    Drop(DropReason),
    /// A bot event with this activated text.
    Emit { text: String },
}

/// Control commands are checked before activation so `/status` works in a
/// mention-only group; authorization is admission's decision, carried on
/// the signal, and the group mode is the conversation's live one.
fn plan_inbound(state: &ConversationState, inbound: &AdmittedInbound) -> InboundPlan {
    let chat = message(inbound);
    let authorization = inbound.authorization;
    if let Some(command) = parse_control_command(&chat.text) {
        return if authorization.control_allowed {
            InboundPlan::Control(command)
        } else {
            InboundPlan::Denied {
                reply: chat.is_direct,
            }
        };
    }
    if !authorization.turn_allowed {
        return InboundPlan::Denied {
            reply: chat.is_direct,
        };
    }
    match classify_inbound(state.scope, &state.activation, state.group_activation, chat) {
        Classification::Emit { text } => InboundPlan::Emit { text },
        Classification::Drop { reason } => InboundPlan::Drop(reason),
    }
}

/// Apply a control command to the conversation and phrase the reply.
fn control_reply(state: &mut ConversationState, command: ControlCommand) -> String {
    match command {
        ControlCommand::Activation { mode } => {
            if state.scope == ChatScope::Direct {
                "Direct chats are always active; /activation applies to groups.".to_owned()
            } else {
                state.group_activation = mode;
                format!("Group activation is now {}.", group_activation_name(mode))
            }
        }
        ControlCommand::ActivationHelp => "Usage: /activation mention|always".to_owned(),
        ControlCommand::Status => [
            format!("bot: {}", state.bot_id),
            format!(
                "activation: {}",
                ActivationMode::resolve(state.scope, state.group_activation).as_str()
            ),
            format!("messages: {} delivered to the bot", state.emitted_count),
            "commands: /activation mention|always, /status".to_owned(),
        ]
        .join("\n"),
    }
}

fn group_activation_name(mode: ChatGroupActivation) -> &'static str {
    match mode {
        ChatGroupActivation::Mention => "mention",
        ChatGroupActivation::Always => "always",
    }
}

fn refusal_reason_name(reason: ChatRefusalReason) -> &'static str {
    match reason {
        ChatRefusalReason::BreakerTripped => "breaker_tripped",
        ChatRefusalReason::TriggerDisabled => "trigger_disabled",
        ChatRefusalReason::BotDisabled => "bot_disabled",
        ChatRefusalReason::BotClosed => "bot_closed",
    }
}

/// Project the engine envelope onto the facts the conversation state
/// records, keeping the full invocation (and its holder) for the lane.
fn project_emission(
    envelope: EmissionEnvelope,
) -> (ConversationEmission, Option<PushedInvocation>) {
    let emission_id = envelope.emission_id.as_str().to_owned();
    let producer_session_id = match &envelope.producer {
        EmissionProducer::Session { session_id, .. } => Some(session_id.as_str().to_owned()),
        EmissionProducer::Workflow { .. } => None,
    };
    let (body, pushed) = match envelope.body {
        EmissionBody::ToolInvocation {
            invocation,
            holder_workflow_id,
        } => (
            ConversationEmissionBody::ToolInvocation {
                holder_workflow_id: holder_workflow_id.clone(),
                invocation_id: invocation.invocation_id.as_str().to_owned(),
                tool_id: invocation.tool_id.as_str().to_owned(),
                arguments_ref: invocation.arguments_ref.as_str().to_owned(),
            },
            Some(PushedInvocation {
                invocation,
                holder_workflow_id,
            }),
        ),
        EmissionBody::RunTerminal { .. } => (ConversationEmissionBody::RunTerminal, None),
        EmissionBody::InvocationCancellation {
            invocation_id,
            completion_key,
            promise_id,
        } => (
            ConversationEmissionBody::InvocationCancellation {
                invocation_id: invocation_id.as_str().to_owned(),
                completion_key,
                promise_id: promise_id.as_str().to_owned(),
            },
            None,
        ),
        EmissionBody::SourceResolution { .. } => (ConversationEmissionBody::SourceResolution, None),
    };
    (
        ConversationEmission {
            emission_id,
            producer_session_id,
            body,
        },
        pushed,
    )
}

/// Only this bot's routed sessions may push here — the core enforces the
/// receiver, this checks the bot — and every `message_*` push is joined on
/// the single `reply` promise.
fn reply_promise_for(
    start: &ConversationStart,
    invocation: &WorkflowToolInvocation,
) -> Result<PromiseId, String> {
    if invocation.session_universe_id != start.universe_id
        || !bots::ids::is_bot_session(&start.bot_id, invocation.session_id.as_str())
    {
        return Err(
            "pushed invocation does not belong to this bot's conversation sessions".to_owned(),
        );
    }
    invocation
        .completion_promises
        .as_ref()
        .and_then(|promises| promises.get(REPLY_COMPLETION_KEY))
        .cloned()
        .ok_or_else(|| "pushed invocation carries no reply promise".to_owned())
}

/// An invocation this conversation will not answer: recorded as failed
/// without a delivery attempt (so it is not a failed delivery).
fn mark_invocation_invalid(state: &mut ConversationState, invocation_id: &str, error: &str) {
    if let Some(entry) = state.invocations.get_mut(invocation_id) {
        entry.status = InvocationStatus::Failed;
        entry.error = Some(error.to_owned());
    }
    state
        .protocol_errors
        .push(format!("invalid invocation {invocation_id}: {error}"));
}

/// The message number an operation refers to, if any.
fn referenced_handle(operation: &ChannelToolOperation) -> Option<u64> {
    match operation {
        ChannelToolOperation::Send { reply_to, .. } => *reply_to,
        ChannelToolOperation::Edit { message, .. }
        | ChannelToolOperation::React { message, .. } => Some(*message),
    }
}

/// What the model asked for, in provider terms. `target` is the resolved
/// handle of the referenced number (`None` only when nothing is
/// referenced). Quoting needs the original author and text on WhatsApp;
/// the bot's own messages quote without context. Only the bot's own sends
/// can be edited.
fn to_delivery_operation(
    operation: &ChannelToolOperation,
    target: Option<&ChatHandle>,
) -> Result<ChannelDeliveryOperation, String> {
    let anchor = |seq: u64| -> Result<(&ChatHandle, String), String> {
        let target = target.ok_or_else(|| unknown_handle(seq, 0))?;
        let anchor = target
            .provider_message_ids
            .first()
            .cloned()
            .ok_or_else(|| unknown_handle(seq, 0))?;
        Ok((target, anchor))
    };
    match operation {
        ChannelToolOperation::Send {
            text,
            reply_to: None,
        } => Ok(ChannelDeliveryOperation::Send {
            text: text.clone(),
            reply_to: None,
            reply_context: None,
        }),
        ChannelToolOperation::Send {
            text,
            reply_to: Some(seq),
        } => {
            let (target, anchor) = anchor(*seq)?;
            let reply_context = match (&target.sender_id, target.from_me) {
                (Some(sender_id), false) => Some(ReplyContext {
                    sender_id: sender_id.clone(),
                    text: target.text.clone().unwrap_or_default(),
                }),
                _ => None,
            };
            Ok(ChannelDeliveryOperation::Send {
                text: text.clone(),
                reply_to: Some(anchor),
                reply_context,
            })
        }
        ChannelToolOperation::Edit { message, text } => {
            let (target, anchor) = anchor(*message)?;
            if !target.from_me {
                return Err(format!(
                    "message #{message} is not yours to edit; only your own sends can be edited"
                ));
            }
            Ok(ChannelDeliveryOperation::Edit {
                message_id: anchor,
                text: text.clone(),
            })
        }
        ChannelToolOperation::React { message, emoji } => {
            let (target, anchor) = anchor(*message)?;
            Ok(ChannelDeliveryOperation::React {
                message_id: anchor,
                emoji: emoji.clone(),
                from_me: target.from_me,
            })
        }
    }
}

/// A message number the model used that this conversation cannot resolve.
fn unknown_handle(seq: u64, max_seq: u64) -> String {
    if max_seq > 0 {
        format!(
            "unknown message #{seq} in this conversation; this bot's messages run #1..#{max_seq}"
        )
    } else {
        format!("unknown message #{seq}; this bot has no messages yet")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiptAction {
    /// No run happened (steered, appended): nothing to reconcile.
    NoRun,
    /// Ask the core whether the run answered through a tool; send its
    /// text otherwise.
    Reconcile,
}

fn receipt_action(outcome: Option<BotEventOutcome>) -> ReceiptAction {
    match outcome {
        Some(BotEventOutcome::Steered | BotEventOutcome::Appended) => ReceiptAction::NoRun,
        _ => ReceiptAction::Reconcile,
    }
}

/// Typing shows while the delivery is recorded as started.
fn typing_wanted(state: &ConversationState, delivery_id: &str) -> bool {
    state
        .deliveries
        .get(delivery_id)
        .is_some_and(|record| record.status == DeliveryStatus::Started)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use api::{
        BotId, BotTriggerId, ChannelAccountId, ChannelInbound, ChannelProvider, ChatAccess,
        ChatActivation,
    };
    use bots::signal::BotDeliveryPhase;
    use channels::ids::ConversationRef;
    use channels::inbound::{ChannelAuthorization, NormalizedInbound};
    use channels::state::MAX_CHANNEL_INBOUND_INBOX;
    use channels::tools::{CHANNEL_EDIT_TOOL_ID, CHANNEL_SEND_TOOL_ID};
    use engine::{
        EventSeq, RunId, SessionId, ToolBatchId, ToolCallId, TurnId, WorkflowToolId,
        WorkflowToolInvocationId,
    };
    use uuid::Uuid;

    use super::*;

    const UNIVERSE: Uuid = Uuid::from_u128(0x6f3a_1a52_58c1_4f0e_9c2d_1a2b_3c4d_5e6f);
    const SESSION_ID: &str = "bot:v1:concierge:k-telegram-primary-123-0123abcd";

    fn start(scope: ChatScope) -> ConversationStart {
        ConversationStart {
            universe_id: UNIVERSE,
            bot_id: BotId::new("concierge"),
            trigger_id: BotTriggerId::new("tg"),
            account_id: ChannelAccountId::new("primary"),
            provider: ChannelProvider::new("telegram"),
            conversation: ConversationRef {
                account_id: ChannelAccountId::new("primary"),
                chat_id: "123".to_owned(),
                thread_id: None,
            },
            scope,
            activation: ChatActivation {
                group: None,
                trigger_prefixes: vec!["/ask".to_owned()],
                mention_names: vec!["lightspeed".to_owned()],
            },
            access: ChatAccess::default(),
            label: "telegram dm · Lukas".to_owned(),
            connector_task_queue: "lightspeed-connector-telegram-test".to_owned(),
        }
    }

    fn inbound(
        text: &str,
        is_direct: bool,
        authorization: ChannelAuthorization,
    ) -> AdmittedInbound {
        AdmittedInbound {
            inbound: NormalizedInbound::new(
                ChannelProvider::new("telegram"),
                ChannelAccountId::new("primary"),
                ChannelInbound {
                    message_id: "42".to_owned(),
                    chat_id: "123".to_owned(),
                    thread_id: None,
                    sender_id: "7".to_owned(),
                    sender_name: "Lukas".to_owned(),
                    timestamp_ms: 1_700_000_000_000,
                    text: text.to_owned(),
                    media: Vec::new(),
                    is_direct,
                    mentioned_bot: false,
                    is_reply_to_bot: false,
                },
            ),
            authorization,
        }
    }

    fn allowed() -> ChannelAuthorization {
        ChannelAuthorization {
            turn_allowed: true,
            control_allowed: true,
        }
    }

    fn reply_promise() -> PromiseId {
        PromiseId::from_number(3)
    }

    fn invocation(
        universe_id: Uuid,
        session_id: &str,
        promises: Option<BTreeMap<String, PromiseId>>,
    ) -> WorkflowToolInvocation {
        WorkflowToolInvocation {
            invocation_id: WorkflowToolInvocationId::new(format!("wti:sha256:{}", "a".repeat(64))),
            tool_id: WorkflowToolId::new(CHANNEL_SEND_TOOL_ID),
            semantic_type: "channels.message.send.v1".to_owned(),
            schema_revision: 3,
            binding_fingerprint: "binding".to_owned(),
            session_universe_id: universe_id,
            session_id: SessionId::new(session_id),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            tool_batch_id: ToolBatchId::new(1),
            tool_call_id: ToolCallId::new("call_1"),
            arguments_ref: BlobRef::from_bytes(b"{\"text\":\"hi\",\"replyTo\":null}"),
            execution_context_ref: None,
            completion_promises: promises,
        }
    }

    fn joined() -> Option<BTreeMap<String, PromiseId>> {
        Some(BTreeMap::from([(
            REPLY_COMPLETION_KEY.to_owned(),
            reply_promise(),
        )]))
    }

    fn handle(from_me: bool, sender_id: Option<&str>, ids: &[&str]) -> ChatHandle {
        ChatHandle {
            provider_message_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            from_me,
            sender_id: sender_id.map(str::to_owned),
            text: Some("question".to_owned()),
        }
    }

    fn workflow(start: ConversationStart) -> ChannelConversationWorkflow {
        let state = ConversationState::new(&start);
        ChannelConversationWorkflow {
            start,
            state,
            start_error: None,
            inbound_inbox: VecDeque::new(),
            emission_inbox: VecDeque::new(),
            receipt_inbox: VecDeque::new(),
            lane_tick: 0,
        }
    }

    #[test]
    fn signal_query_and_inbox_contract_match() {
        assert_eq!(CHAT_INBOUND_SIGNAL, "chat_inbound");
        assert_eq!(BOT_DELIVERY_SIGNAL, "bot_delivery");
        assert_eq!(CHAT_STATE_QUERY, "chat_state");
        assert_eq!(
            CHANNEL_CONVERSATION_WORKFLOW_KIND,
            "ChannelConversationWorkflow"
        );
        assert_eq!(CHANNEL_INBOUND_INBOX_CAP, MAX_CHANNEL_INBOUND_INBOX);
    }

    #[test]
    fn args_round_trip_and_omit_an_absent_carry() {
        let args = ChannelConversationArgs {
            start: start(ChatScope::Direct),
            carry: None,
        };
        let json = serde_json::to_value(&args).unwrap();
        assert!(json.get("carry").is_none());
        let decoded: ChannelConversationArgs = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, args);
        let carried = ChannelConversationArgs {
            carry: Some(ConversationState::new(&args.start).compact_state()),
            ..args
        };
        let json = serde_json::to_string(&carried).unwrap();
        let decoded: ChannelConversationArgs = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, carried);
    }

    #[test]
    fn accepts_a_valid_start_and_a_matching_carry_only() {
        let start = start(ChatScope::Direct);
        let state = accept_start(&start, None).unwrap();
        assert_eq!(state.conversation_key, "primary/123");

        let carry = state.compact_state();
        assert_eq!(
            accept_start(&start, Some(carry.clone())).unwrap(),
            carry.state
        );
        let mut other = start.clone();
        other.trigger_id = BotTriggerId::new("wa");
        assert!(
            accept_start(&other, Some(carry))
                .unwrap_err()
                .contains("does not match")
        );

        let mut no_label = start.clone();
        no_label.label = "  ".to_owned();
        assert!(validate_start(&no_label).unwrap_err().contains("label"));
        let mut no_queue = start.clone();
        no_queue.connector_task_queue = String::new();
        assert!(
            validate_start(&no_queue)
                .unwrap_err()
                .contains("task queue")
        );
        let mut nil = start.clone();
        nil.universe_id = Uuid::nil();
        assert!(validate_start(&nil).unwrap_err().contains("universe"));
        let mut foreign_account = start.clone();
        foreign_account.conversation.account_id = ChannelAccountId::new("other");
        assert!(
            validate_start(&foreign_account)
                .unwrap_err()
                .contains("account")
        );
        let mut empty_thread = start;
        empty_thread.conversation.thread_id = Some(String::new());
        assert!(
            validate_start(&empty_thread)
                .unwrap_err()
                .contains("thread")
        );
    }

    #[test]
    fn plans_control_denial_drop_and_emit() {
        let direct = ConversationState::new(&start(ChatScope::Direct));
        assert_eq!(
            plan_inbound(&direct, &inbound("/status", true, allowed())),
            InboundPlan::Control(ControlCommand::Status)
        );
        let no_control = ChannelAuthorization {
            turn_allowed: true,
            control_allowed: false,
        };
        assert_eq!(
            plan_inbound(&direct, &inbound("/status", true, no_control)),
            InboundPlan::Denied { reply: true }
        );
        let group = ConversationState::new(&start(ChatScope::Group));
        assert_eq!(
            plan_inbound(&group, &inbound("/activation always", false, no_control)),
            InboundPlan::Denied { reply: false },
            "a denied control command in a group is silent"
        );
        assert_eq!(
            plan_inbound(
                &direct,
                &inbound("hello", true, ChannelAuthorization::default())
            ),
            InboundPlan::Denied { reply: true }
        );
        assert_eq!(
            plan_inbound(
                &group,
                &inbound("hello", false, ChannelAuthorization::default())
            ),
            InboundPlan::Denied { reply: false }
        );
        assert_eq!(
            plan_inbound(&direct, &inbound("hello", true, allowed())),
            InboundPlan::Emit {
                text: "hello".to_owned()
            }
        );
        assert_eq!(
            plan_inbound(&group, &inbound("ambient chatter", false, allowed())),
            InboundPlan::Drop(DropReason::Ambient)
        );
        assert_eq!(
            plan_inbound(&group, &inbound("/ask look", false, allowed())),
            InboundPlan::Emit {
                text: "look".to_owned()
            }
        );
        // The live group mode wins over the trigger's configured one.
        let mut always = ConversationState::new(&start(ChatScope::Group));
        always.group_activation = ChatGroupActivation::Always;
        assert_eq!(
            plan_inbound(&always, &inbound("ambient chatter", false, allowed())),
            InboundPlan::Emit {
                text: "ambient chatter".to_owned()
            }
        );
        // Control commands are seen before activation, so `/status` works
        // in a mention-only group.
        assert_eq!(
            plan_inbound(&group, &inbound("/status@bot", false, allowed())),
            InboundPlan::Control(ControlCommand::Status)
        );
    }

    #[test]
    fn control_replies_switch_group_mode_and_describe_status() {
        let mut direct = ConversationState::new(&start(ChatScope::Direct));
        assert_eq!(
            control_reply(
                &mut direct,
                ControlCommand::Activation {
                    mode: ChatGroupActivation::Always
                }
            ),
            "Direct chats are always active; /activation applies to groups."
        );
        assert_eq!(direct.group_activation, ChatGroupActivation::Mention);

        let mut group = ConversationState::new(&start(ChatScope::Group));
        assert_eq!(
            control_reply(
                &mut group,
                ControlCommand::Activation {
                    mode: ChatGroupActivation::Always
                }
            ),
            "Group activation is now always."
        );
        assert_eq!(group.group_activation, ChatGroupActivation::Always);
        assert_eq!(
            control_reply(&mut group, ControlCommand::ActivationHelp),
            "Usage: /activation mention|always"
        );
        group.emitted_count = 4;
        assert_eq!(
            control_reply(&mut group, ControlCommand::Status),
            "bot: concierge\nactivation: always\nmessages: 4 delivered to the bot\ncommands: /activation mention|always, /status"
        );
        assert_eq!(
            control_reply(&mut direct, ControlCommand::Status),
            "bot: concierge\nactivation: dm\nmessages: 0 delivered to the bot\ncommands: /activation mention|always, /status"
        );
    }

    #[test]
    fn reply_promise_requires_this_bots_session_in_this_universe_and_a_reply_key() {
        let start = start(ChatScope::Direct);
        assert_eq!(
            reply_promise_for(&start, &invocation(UNIVERSE, SESSION_ID, joined())),
            Ok(reply_promise())
        );
        assert_eq!(
            reply_promise_for(&start, &invocation(UNIVERSE, "bot:v1:concierge", joined())),
            Ok(reply_promise()),
            "the main session is the bot's too"
        );
        assert!(
            reply_promise_for(
                &start,
                &invocation(Uuid::from_u128(9), SESSION_ID, joined())
            )
            .unwrap_err()
            .contains("does not belong")
        );
        assert!(
            reply_promise_for(&start, &invocation(UNIVERSE, "bot:v1:other:k-x", joined()))
                .unwrap_err()
                .contains("does not belong")
        );
        assert!(
            reply_promise_for(
                &start,
                &invocation(UNIVERSE, "bot:v1:concierge-evil", joined())
            )
            .unwrap_err()
            .contains("does not belong")
        );
        assert!(
            reply_promise_for(&start, &invocation(UNIVERSE, SESSION_ID, None))
                .unwrap_err()
                .contains("no reply promise")
        );
        let wrong_key = Some(BTreeMap::from([("job-0".to_owned(), reply_promise())]));
        assert!(
            reply_promise_for(&start, &invocation(UNIVERSE, SESSION_ID, wrong_key))
                .unwrap_err()
                .contains("no reply promise")
        );
    }

    #[test]
    fn projects_engine_envelopes_onto_conversation_emissions() {
        let pushed = EmissionEnvelope::tool_invocation(
            UNIVERSE,
            SessionId::new(SESSION_ID),
            EventSeq::new(7),
            invocation(UNIVERSE, SESSION_ID, joined()),
            format!("{UNIVERSE}/{SESSION_ID}"),
        );
        let (emission, invocation) = project_emission(pushed.clone());
        assert_eq!(emission.emission_id, pushed.emission_id.as_str());
        assert_eq!(emission.producer_session_id.as_deref(), Some(SESSION_ID));
        assert_eq!(
            emission.body,
            ConversationEmissionBody::ToolInvocation {
                holder_workflow_id: format!("{UNIVERSE}/{SESSION_ID}"),
                invocation_id: format!("wti:sha256:{}", "a".repeat(64)),
                tool_id: CHANNEL_SEND_TOOL_ID.to_owned(),
                arguments_ref: BlobRef::from_bytes(b"{\"text\":\"hi\",\"replyTo\":null}")
                    .as_str()
                    .to_owned(),
            }
        );
        let invocation = invocation.unwrap();
        assert_eq!(
            invocation.holder_workflow_id,
            format!("{UNIVERSE}/{SESSION_ID}")
        );
        assert_eq!(invocation.invocation.session_id.as_str(), SESSION_ID);

        let cancelled = EmissionEnvelope::invocation_cancellation(
            UNIVERSE,
            SessionId::new(SESSION_ID),
            EventSeq::new(8),
            WorkflowToolInvocationId::new(format!("wti:sha256:{}", "a".repeat(64))),
            REPLY_COMPLETION_KEY.to_owned(),
            reply_promise(),
        );
        let (emission, invocation) = project_emission(cancelled);
        assert!(invocation.is_none());
        assert_eq!(
            emission.body,
            ConversationEmissionBody::InvocationCancellation {
                invocation_id: format!("wti:sha256:{}", "a".repeat(64)),
                completion_key: "reply".to_owned(),
                promise_id: "promise_3".to_owned(),
            }
        );

        let resolution = EmissionEnvelope::source_resolution(
            UNIVERSE,
            "wte:other".to_owned(),
            &format!("{UNIVERSE}/{SESSION_ID}"),
            reply_promise(),
            PromiseResolution::Cancelled,
        );
        let (emission, invocation) = project_emission(resolution);
        assert!(invocation.is_none());
        assert_eq!(emission.producer_session_id, None);
        assert_eq!(emission.body, ConversationEmissionBody::SourceResolution);

        // Through the state: a workflow-produced invocation is a protocol
        // error, a cancellation is a fact the lane observes.
        let mut state = ConversationState::new(&start(ChatScope::Direct));
        let mut foreign = pushed;
        foreign.producer = EmissionProducer::Workflow {
            universe_id: UNIVERSE,
            workflow_id: "wte:other".to_owned(),
        };
        let (emission, _) = project_emission(foreign);
        assert_eq!(state.apply_emission(&emission), ApplyEmissionEffect::None);
        assert_eq!(state.protocol_errors.len(), 1);
    }

    #[test]
    fn invalid_invocations_are_failed_without_counting_a_delivery() {
        let mut state = ConversationState::new(&start(ChatScope::Direct));
        let pushed = EmissionEnvelope::tool_invocation(
            UNIVERSE,
            SessionId::new(SESSION_ID),
            EventSeq::new(7),
            invocation(UNIVERSE, SESSION_ID, None),
            format!("{UNIVERSE}/{SESSION_ID}"),
        );
        let (emission, _) = project_emission(pushed);
        let ApplyEmissionEffect::InvocationReceived { invocation_id } =
            state.apply_emission(&emission)
        else {
            panic!("a fresh invocation is received");
        };
        mark_invocation_invalid(&mut state, &invocation_id, "no reply promise");
        let entry = state.invocations.get(&invocation_id).unwrap();
        assert_eq!(entry.status, InvocationStatus::Failed);
        assert_eq!(entry.error.as_deref(), Some("no reply promise"));
        assert_eq!(state.failed_delivery_count, 0);
        assert_eq!(state.active_deliveries(), 0);
        assert_eq!(
            state.protocol_errors,
            vec![format!(
                "invalid invocation {invocation_id}: no reply promise"
            )]
        );
    }

    #[test]
    fn resolves_numbers_to_provider_terms_with_quote_and_ownership_rules() {
        let send = ChannelToolOperation::Send {
            text: "hi".to_owned(),
            reply_to: None,
        };
        assert_eq!(referenced_handle(&send), None);
        assert_eq!(
            to_delivery_operation(&send, None),
            Ok(ChannelDeliveryOperation::Send {
                text: "hi".to_owned(),
                reply_to: None,
                reply_context: None,
            })
        );

        let reply = ChannelToolOperation::Send {
            text: "hi".to_owned(),
            reply_to: Some(41),
        };
        assert_eq!(referenced_handle(&reply), Some(41));
        // A reply to someone else's message quotes author and text.
        assert_eq!(
            to_delivery_operation(&reply, Some(&handle(false, Some("7"), &["p41"]))),
            Ok(ChannelDeliveryOperation::Send {
                text: "hi".to_owned(),
                reply_to: Some("p41".to_owned()),
                reply_context: Some(ReplyContext {
                    sender_id: "7".to_owned(),
                    text: "question".to_owned(),
                }),
            })
        );
        // The bot's own messages quote without context; so does a message
        // with no known author. A chunked send anchors on its first id.
        assert_eq!(
            to_delivery_operation(&reply, Some(&handle(true, None, &["p1", "p2"]))),
            Ok(ChannelDeliveryOperation::Send {
                text: "hi".to_owned(),
                reply_to: Some("p1".to_owned()),
                reply_context: None,
            })
        );
        assert_eq!(
            to_delivery_operation(&reply, Some(&handle(false, None, &["p41"])))
                .unwrap()
                .clone(),
            ChannelDeliveryOperation::Send {
                text: "hi".to_owned(),
                reply_to: Some("p41".to_owned()),
                reply_context: None,
            }
        );
        assert_eq!(
            to_delivery_operation(&reply, None),
            Err(unknown_handle(41, 0))
        );
        assert_eq!(
            to_delivery_operation(&reply, Some(&handle(false, Some("7"), &[]))),
            Err(unknown_handle(41, 0))
        );

        let edit = ChannelToolOperation::Edit {
            message: 9,
            text: "fixed".to_owned(),
        };
        assert_eq!(referenced_handle(&edit), Some(9));
        assert_eq!(
            to_delivery_operation(&edit, Some(&handle(true, None, &["p9"]))),
            Ok(ChannelDeliveryOperation::Edit {
                message_id: "p9".to_owned(),
                text: "fixed".to_owned(),
            })
        );
        assert_eq!(
            to_delivery_operation(&edit, Some(&handle(false, Some("7"), &["p9"]))),
            Err("message #9 is not yours to edit; only your own sends can be edited".to_owned())
        );

        let react = ChannelToolOperation::React {
            message: 9,
            emoji: "👍".to_owned(),
        };
        assert_eq!(
            to_delivery_operation(&react, Some(&handle(false, Some("7"), &["p9"]))),
            Ok(ChannelDeliveryOperation::React {
                message_id: "p9".to_owned(),
                emoji: "👍".to_owned(),
                from_me: false,
            })
        );
        assert_eq!(
            to_delivery_operation(&react, Some(&handle(true, None, &["p9"]))),
            Ok(ChannelDeliveryOperation::React {
                message_id: "p9".to_owned(),
                emoji: "👍".to_owned(),
                from_me: true,
            })
        );
        assert_eq!(
            parse_tool_operation(
                CHANNEL_EDIT_TOOL_ID,
                &serde_json::json!({ "message": 9, "text": "x" })
            ),
            Ok(ChannelToolOperation::Edit {
                message: 9,
                text: "x".to_owned()
            })
        );
    }

    #[test]
    fn unknown_handles_name_the_range_the_model_may_use() {
        assert_eq!(
            unknown_handle(7, 12),
            "unknown message #7 in this conversation; this bot's messages run #1..#12"
        );
        assert_eq!(
            unknown_handle(7, 0),
            "unknown message #7; this bot has no messages yet"
        );
    }

    #[test]
    fn receipts_reconcile_unless_no_run_happened() {
        assert_eq!(
            receipt_action(Some(BotEventOutcome::Steered)),
            ReceiptAction::NoRun
        );
        assert_eq!(
            receipt_action(Some(BotEventOutcome::Appended)),
            ReceiptAction::NoRun
        );
        assert_eq!(
            receipt_action(Some(BotEventOutcome::Handled)),
            ReceiptAction::Reconcile
        );
        assert_eq!(
            receipt_action(Some(BotEventOutcome::RunFailed)),
            ReceiptAction::Reconcile
        );
        assert_eq!(receipt_action(None), ReceiptAction::Reconcile);

        let mut state = ConversationState::new(&start(ChatScope::Direct));
        let receipt = |phase| BotDeliveryReceipt {
            token: "token".to_owned(),
            phase,
            delivery_id: "d1".to_owned(),
            seqs: vec![1],
            session_id: SESSION_ID.to_owned(),
            run_id: Some("run_1".to_owned()),
            outcome: None,
            summary: None,
        };
        assert!(!typing_wanted(&state, "d1"));
        state.record_delivery_receipt(&receipt(BotDeliveryPhase::Started));
        assert!(typing_wanted(&state, "d1"));
        state.record_delivery_receipt(&receipt(BotDeliveryPhase::Finished));
        assert!(
            !typing_wanted(&state, "d1"),
            "a finish stops the typing lane"
        );
    }

    #[test]
    fn continue_as_new_waits_for_empty_inboxes_and_no_lanes() {
        let mut wf = workflow(start(ChatScope::Direct));
        assert!(!wf.can_continue_as_new(false, false));
        assert!(wf.can_continue_as_new(true, false));
        assert!(!wf.can_continue_as_new(true, true));
        wf.inbound_inbox
            .push_back(inbound("hello", true, allowed()));
        assert!(!wf.can_continue_as_new(true, false));
        assert!(wf.wake_ready(0));
        wf.inbound_inbox.clear();
        assert!(!wf.wake_ready(0));
        wf.lane_tick += 1;
        assert!(wf.wake_ready(0), "a finished lane wakes the loop");
        assert!(!wf.wake_ready(1));
        wf.receipt_inbox.push_back(BotDeliveryReceipt {
            token: "t".to_owned(),
            phase: BotDeliveryPhase::Started,
            delivery_id: "d".to_owned(),
            seqs: Vec::new(),
            session_id: SESSION_ID.to_owned(),
            run_id: None,
            outcome: None,
            summary: None,
        });
        assert!(wf.wake_ready(1));
        assert!(!wf.can_continue_as_new(true, false));
    }
}
