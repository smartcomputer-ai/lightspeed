//! Detached lanes: one delivery on one session, a steer/append sidecar
//! beside a running lane, and the answer to a pushed `bot_*` invocation.
//! Each lane owns a context clone, finishes its own bookkeeping, and
//! records failures on the controller instead of failing the workflow.

use crate::workflows::WorkflowContextExt as _;
use api::{BotEventOutcome, BotRecentDeliverySnapshot, LlmUsageView};
use bots::{BotDeliveryPhase, RoutedSession, ids};
use harness::{BlobRef, EmissionEnvelope, PromiseResolution, REPLY_COMPLETION_KEY};

use super::super::{
    BOT_BUSY_RETRY_DELAY, BOT_EXTRA_SESSION_CAP, BotActivities, BotAppendContextRequest,
    BotEnsureSessionResult, BotExecuteToolRequest, BotExecuteToolResult,
    BotPublishDirectoryRequest, BotReadRunUsageRequest, BotRecordOutcomesRequest,
    BotSendBotReceiptsRequest, BotSendDeliveryReceiptsRequest, BotSessionRequest, BotSessionStatus,
    BotStartRunRequest, BotStartRunResult, BotSteerRunRequest, BotSteerRunResult,
    bot_start_run_activity_options,
};
use super::state::{
    BotDelivery, ControlledSession, DeliveryOutcome, LaneTerminal, PendingInvocation, WaitingLane,
    delivery_outcome,
};
use super::{Ctx, activity, close_session, ensure_request, now_ms, reconcile_run, roll_forward};
use crate::AgentSessionWorkflow;

// ── Delivery lane ───────────────────────────────────────────────────────────

/// Run one delivery on its session. A failure ends the delivery `runFailed`
/// with the failure text, apart from a run start that may still succeed, which
/// goes back to the queue for a bounded number of attempts. A session busy with
/// other work sends the delivery back too, with no limit, and a dead one sends it
/// to the session's successor, once. The lane never fails the workflow.
pub(super) async fn run_delivery(ctx: Ctx, delivery: BotDelivery, target: String) {
    let mut target = target;
    if let Err(message) = run_delivery_inner(&ctx, &delivery, &mut target).await {
        fail_delivery(&ctx, &target, &delivery, message).await;
    }
}

/// Pick up a run the previous controller execution was waiting on.
pub(super) async fn resume_run_wait(ctx: Ctx, lane: WaitingLane) {
    let WaitingLane {
        delivery,
        session_id,
        run_id,
        ..
    } = lane;
    if let Err(message) = wait_for_run_end(&ctx, &delivery, &session_id, &run_id).await {
        fail_delivery(&ctx, &session_id, &delivery, message).await;
    }
}

async fn fail_delivery(ctx: &Ctx, target: &str, delivery: &BotDelivery, message: String) {
    ctx.state_mut(|state| state.record_error(message.clone()));
    finish_delivery(
        ctx,
        target,
        delivery,
        DeliveryOutcome {
            outcome: BotEventOutcome::RunFailed,
            summary: Some(message),
            run_id: None,
        },
        None,
    )
    .await;
}

async fn run_delivery_inner(
    ctx: &Ctx,
    delivery: &BotDelivery,
    target: &mut String,
) -> Result<(), String> {
    let Some(first_event) = delivery.events.first() else {
        ctx.state_mut(|state| {
            state.active_by_session.remove(target.as_str());
            state.lane_tick += 1;
        });
        return Ok(());
    };
    let universe_id = ctx.state(|state| state.config.universe_id);

    if let Some(session) = &delivery.session {
        match ensure_routed_session(ctx, session, target, first_event.tools_ref.clone()).await {
            RoutedSessionEnsured::Dead {
                session_id,
                message,
            } => {
                ctx.state_mut(|state| state.rekey_lane(target, &session_id));
                *target = session_id;
                let dead = BotSessionStatus::Failed;
                return retry_on_successor(ctx, delivery, target, &dead, message).await;
            }
            RoutedSessionEnsured::Failed => {
                let reason = ctx
                    .state(|state| state.last_error.clone())
                    .unwrap_or_else(|| "unknown".to_owned());
                finish_delivery(
                    ctx,
                    target,
                    delivery,
                    DeliveryOutcome {
                        outcome: BotEventOutcome::RunFailed,
                        summary: Some(format!("failed to create session {target}: {reason}")),
                        run_id: None,
                    },
                    None,
                )
                .await;
                return Ok(());
            }
            RoutedSessionEnsured::Ready(ensured) if ensured != *target => {
                // The routed session rotated during ensure; move the lane
                // to the successor id so terminals and busy checks find it.
                ctx.state_mut(|state| state.rekey_lane(target, &ensured));
                *target = ensured;
            }
            RoutedSessionEnsured::Ready(_) => {}
        }
    }

    if delivery.when_busy == api::BotWhenBusy::Append {
        publish_directory(ctx, target).await;
        let appended = activity(
            ctx,
            BotActivities::append_context,
            BotAppendContextRequest {
                universe_id,
                session_id: target.clone(),
                delivery_id: delivery.id.clone(),
                events: delivery.events.clone(),
            },
        )
        .await;
        if let Err(message) = appended {
            // Appending needs no status read on the way in, so this is where a
            // dead session shows itself on this path.
            return match read_session_status(ctx, target).await {
                Ok(status) if status.is_dead() => {
                    retry_on_successor(ctx, delivery, target, &status, message).await
                }
                _ => Err(message),
            };
        }
        finish_delivery(
            ctx,
            target,
            delivery,
            DeliveryOutcome {
                outcome: BotEventOutcome::Appended,
                summary: None,
                run_id: None,
            },
            None,
        )
        .await;
        return Ok(());
    }

    let busy = match read_session_status(ctx, target).await? {
        BotSessionStatus::Idle => false,
        BotSessionStatus::Busy { .. } => true,
        dead => {
            let message = dead_session_message(target, &dead);
            return retry_on_successor(ctx, delivery, target, &dead, message).await;
        }
    };
    if busy && delivery.when_busy != api::BotWhenBusy::Steer {
        look_again_later(ctx, target, delivery);
        return Ok(());
    }
    publish_directory(ctx, target).await;
    if busy {
        // A busy session takes the events as steering.
        if let BotSteerRunResult::Steered { run_id } = steer_run(ctx, delivery, target).await? {
            finish_delivery(
                ctx,
                target,
                delivery,
                DeliveryOutcome {
                    outcome: BotEventOutcome::Steered,
                    summary: None,
                    run_id: Some(run_id),
                },
                None,
            )
            .await;
        } else {
            look_again_later(ctx, target, delivery);
        }
        return Ok(());
    }

    let terminal_token = ids::delivery_terminal_token(&delivery.id);
    let started = crate::workflows::fail_task_if_undecodable(
        ctx.execute_activity(
            BotActivities::start_run,
            BotStartRunRequest {
                universe_id,
                session_id: target.clone(),
                delivery_id: delivery.id.clone(),
                events: delivery.events.clone(),
                submission_id: ids::delivery_submission_id(&delivery.id),
                terminal_token: terminal_token.clone(),
            },
            bot_start_run_activity_options(),
        )
        .await,
    )
    .map_err(|error| error.to_string());
    let (run_id, finished) = match started {
        Ok(BotStartRunResult::Started { run_id, finished }) => (run_id, finished),
        Ok(BotStartRunResult::Refused { message }) => return Err(message),
        Ok(BotStartRunResult::SessionDead { message }) => {
            let dead = BotSessionStatus::Failed;
            return retry_on_successor(ctx, delivery, target, &dead, message).await;
        }
        // An activity that spent its own attempts is an outage, not an answer.
        Ok(BotStartRunResult::Retry { message }) | Err(message) => {
            let now = now_ms(ctx);
            if !ctx.state_mut(|state| state.retry_delivery(target, delivery, now)) {
                return Err(message);
            }
            ctx.state_mut(|state| state.record_error(message));
            return Ok(());
        }
    };
    let started_at = now_ms(ctx);
    ctx.state_mut(|state| state.mark_lane_started(target, &run_id, started_at));
    send_delivery_receipts(
        ctx,
        delivery,
        target,
        BotDeliveryPhase::Started,
        Some(run_id.clone()),
        None,
        None,
    )
    .await;

    if let Some(status) = finished {
        // An earlier attempt's run ended while no lane held this delivery, so
        // its terminal left the log unread and found nothing to attach to.
        reconcile_run(ctx, &run_id, target).await;
        ctx.state_mut(|state| {
            state.attach_terminal(
                &terminal_token,
                LaneTerminal {
                    status,
                    run_id: run_id.clone(),
                    failure_message_ref: None,
                    dead_session: None,
                },
            )
        });
    }

    wait_for_run_end(ctx, delivery, target, &run_id).await
}

/// Runs may take as long as they take. A terminal ends this wait, and so does the
/// dead-session sweep, for a session that can no longer send one. The wait holds
/// nothing, so a continue-as-new can happen under it and start it again.
async fn wait_for_run_end(
    ctx: &Ctx,
    delivery: &BotDelivery,
    target: &str,
    run_id: &str,
) -> Result<(), String> {
    ctx.state_mut(|state| state.park_lane(target));
    let session_id = target.to_owned();
    ctx.wait_for_state(move |state| state.lane_has_terminal(&session_id))
        .await;

    let Some((terminal, resolution)) = ctx.state(|state| {
        let lane = state.active_by_session.get(target)?;
        Some((lane.terminal.clone()?, lane.resolution.clone()))
    }) else {
        return Err(format!("the lane on session {target} lost its run's end"));
    };
    let outcome = delivery_outcome(&terminal, resolution.as_ref(), Some(run_id));
    let usage = match (&outcome.run_id, outcome.outcome) {
        (Some(run_id), outcome) if outcome != BotEventOutcome::RunFailed => {
            read_run_usage(ctx, target, run_id).await
        }
        _ => None,
    };
    finish_delivery(ctx, target, delivery, outcome, usage).await;
    Ok(())
}

pub(super) fn dead_session_message(session_id: &str, status: &BotSessionStatus) -> String {
    match status {
        BotSessionStatus::Failed => {
            format!("session {session_id} has failed: its workflow is no longer running")
        }
        BotSessionStatus::Closed => format!("session {session_id} is closed"),
        BotSessionStatus::Missing => format!("session {session_id} is missing"),
        BotSessionStatus::Idle | BotSessionStatus::Busy { .. } => {
            format!("session {session_id} is unavailable")
        }
    }
}

/// The session is dead and no run started on it, so nothing has happened yet that
/// the next generation would repeat. Without this every later event would fail
/// the same way, forever. A delivery that already moved once, or a session that could
/// not be left behind, ends `runFailed`, and the next event gets its own try.
async fn retry_on_successor(
    ctx: &Ctx,
    delivery: &BotDelivery,
    target: &str,
    status: &BotSessionStatus,
    message: String,
) -> Result<(), String> {
    if delivery.rolled_forward || !roll_forward(ctx, target, status).await {
        return Err(message);
    }
    let now = now_ms(ctx);
    ctx.state_mut(|state| state.requeue_rolled_forward(target, delivery, now));
    Ok(())
}

/// The session is running other work, such as an operator's direct run. The
/// delivery waits in the queue, because a lane polling here would hold the
/// rollover for as long as that run lasts, until the server terminates the
/// controller at its history limit.
fn look_again_later(ctx: &Ctx, target: &str, delivery: &BotDelivery) {
    let not_before_ms = now_ms(ctx) + BOT_BUSY_RETRY_DELAY.as_millis() as i64;
    ctx.state_mut(|state| state.requeue_lane(target, delivery, not_before_ms));
}

/// An emitting bot reads the directory before it decides. A failed put costs a
/// stale directory, never the delivery.
async fn publish_directory(ctx: &Ctx, session_id: &str) {
    let (emit, universe_id, bot_id) = ctx.state(|state| {
        (
            state.config.emit,
            state.config.universe_id,
            state.config.bot_id.clone(),
        )
    });
    if !emit {
        return;
    }
    if let Err(message) = activity(
        ctx,
        BotActivities::publish_directory,
        BotPublishDirectoryRequest {
            universe_id,
            bot_id,
            session_id: session_id.to_owned(),
        },
    )
    .await
    {
        ctx.state_mut(|state| state.record_error(message));
    }
}

/// Best effort: the cached share is observability, never a reason to fail
/// a delivery that already finished.
async fn read_run_usage(ctx: &Ctx, session_id: &str, run_id: &str) -> Option<LlmUsageView> {
    let universe_id = ctx.state(|state| state.config.universe_id);
    activity(
        ctx,
        BotActivities::read_run_usage,
        BotReadRunUsageRequest {
            universe_id,
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
        },
    )
    .await
    .ok()
    .and_then(|result| result.usage)
}

pub(super) async fn read_session_status(
    ctx: &Ctx,
    session_id: &str,
) -> Result<BotSessionStatus, String> {
    let universe_id = ctx.state(|state| state.config.universe_id);
    activity(
        ctx,
        BotActivities::read_session_status,
        BotSessionRequest {
            universe_id,
            session_id: session_id.to_owned(),
        },
    )
    .await
}

async fn steer_run(
    ctx: &Ctx,
    delivery: &BotDelivery,
    session_id: &str,
) -> Result<BotSteerRunResult, String> {
    let universe_id = ctx.state(|state| state.config.universe_id);
    activity(
        ctx,
        BotActivities::steer_run,
        BotSteerRunRequest {
            universe_id,
            session_id: session_id.to_owned(),
            delivery_id: delivery.id.clone(),
            events: delivery.events.clone(),
        },
    )
    .await
}

/// What ensuring a routed session came to.
enum RoutedSessionEnsured {
    /// The session id actually ensured.
    Ready(String),
    /// This session id exists, but its workflow execution has stopped.
    Dead { session_id: String, message: String },
    /// It could not be created; the reason is in `last_error`.
    Failed,
}

/// Create a routed (perKey / perEvent) session on first use. A declaration
/// mismatch — the session pre-exists under an older toolset, e.g. a per-event
/// id delivered again after a release — rotates the key to its next generation
/// and retries once, mirroring the main session's rotation, instead of wedging
/// the delivery. A session already known returns at once with no liveness check;
/// the status read and the run start after this are what find one dead.
async fn ensure_routed_session(
    ctx: &Ctx,
    session: &RoutedSession,
    resolved_id: &str,
    tools_ref: Option<String>,
) -> RoutedSessionEnsured {
    let mut session_id = resolved_id.to_owned();
    for attempt in 0..2 {
        let observed_at_ms = now_ms(ctx);
        let known = ctx
            .state_mut(|state| state.observe_routed_session(&session_id, session, observed_at_ms));
        if known {
            return RoutedSessionEnsured::Ready(session_id);
        }
        let request = ctx.state(|state| {
            ensure_request(
                state,
                ctx.workflow_id(),
                session_id.clone(),
                state.routed_label(&session.label),
                // Routed sessions take the profile at creation; only the
                // main session tracks profile revisions across its life.
                None,
                tools_ref.clone(),
                Some(session),
            )
        });
        match activity(ctx, BotActivities::ensure_session, request).await {
            Ok(BotEnsureSessionResult::Ready {
                carried_tool_ids, ..
            }) => {
                let now = now_ms(ctx);
                let kind = ids::bot_session_kind(&session_id);
                ctx.state_mut(|state| {
                    state.extra_sessions.push(ControlledSession {
                        session_id: session_id.clone(),
                        label: session.label.clone(),
                        session_key: session.session_key.clone(),
                        kind,
                        last_active_at_ms: Some(now),
                        close_policy: session.close_policy,
                        carried_tool_ids,
                    });
                });
                enforce_extra_session_cap(ctx, &session_id).await;
                return RoutedSessionEnsured::Ready(session_id);
            }
            Ok(
                BotEnsureSessionResult::DeclarationMismatch { message }
                | BotEnsureSessionResult::ProfileUnapplicable { message },
            ) => {
                if attempt == 0 {
                    let base = ids::routed_session_base(&session_id).to_owned();
                    session_id = ctx.state_mut(|state| {
                        state.bump_routed_generation(&base);
                        state.resolve_routed_session_id(&base)
                    });
                    continue;
                }
                ctx.state_mut(|state| state.record_error(message));
                return RoutedSessionEnsured::Failed;
            }
            Ok(BotEnsureSessionResult::SessionDead { message }) => {
                return RoutedSessionEnsured::Dead {
                    session_id,
                    message,
                };
            }
            Err(message) => {
                ctx.state_mut(|state| state.record_error(message));
                return RoutedSessionEnsured::Failed;
            }
        }
    }
    RoutedSessionEnsured::Failed
}

/// A session evicted from the tracked set would otherwise escape the
/// idle-close sweep and teardown: close the idlest free one (non-force) as
/// it leaves. A session that will not close stays tracked for the next
/// sweep.
async fn enforce_extra_session_cap(ctx: &Ctx, keep: &str) {
    while ctx.state(|state| state.extra_sessions.len() > BOT_EXTRA_SESSION_CAP) {
        let Some(victim) = ctx.state(|state| state.idlest_free_session(keep)) else {
            break;
        };
        if !close_session(ctx, &victim, false).await {
            break;
        }
        ctx.state_mut(|state| state.forget_routed_session(&victim));
    }
}

// ── Sidecar ─────────────────────────────────────────────────────────────────

/// A steer/append delivery on a session whose lane has a started run. The
/// sidecar never starts a run: if the run finished underneath it, the
/// delivery goes back to the front of the queue for an ordinary lane.
pub(super) async fn run_busy_sidecar(ctx: Ctx, delivery: BotDelivery, target: String) {
    let settled = match run_busy_sidecar_inner(&ctx, &delivery, &target).await {
        Ok(settled) => settled,
        Err(message) => {
            ctx.state_mut(|state| state.record_error(message.clone()));
            Some(DeliveryOutcome {
                outcome: BotEventOutcome::RunFailed,
                summary: Some(message),
                run_id: None,
            })
        }
    };
    let now = now_ms(&ctx);
    if let Some(outcome) = settled {
        let recent = recent_snapshot(&delivery, &target, &outcome, None, now);
        ctx.state_mut(|state| {
            state.remember_delivery(recent);
            state.release_sidecar(&target, now);
        });
        settle_delivery(&ctx, &delivery, &target, &outcome).await;
    } else {
        ctx.state_mut(|state| state.release_sidecar(&target, now));
    }
}

/// `Ok(None)` when the delivery was requeued instead of settled.
async fn run_busy_sidecar_inner(
    ctx: &Ctx,
    delivery: &BotDelivery,
    target: &str,
) -> Result<Option<DeliveryOutcome>, String> {
    if delivery.events.is_empty() {
        return Ok(None);
    }
    let universe_id = ctx.state(|state| state.config.universe_id);
    if delivery.when_busy == api::BotWhenBusy::Append {
        activity(
            ctx,
            BotActivities::append_context,
            BotAppendContextRequest {
                universe_id,
                session_id: target.to_owned(),
                delivery_id: delivery.id.clone(),
                events: delivery.events.clone(),
            },
        )
        .await?;
        return Ok(Some(DeliveryOutcome {
            outcome: BotEventOutcome::Appended,
            summary: None,
            run_id: None,
        }));
    }
    let status = read_session_status(ctx, target).await?;
    match status {
        BotSessionStatus::Failed | BotSessionStatus::Missing => {
            return Err(dead_session_message(target, &status));
        }
        BotSessionStatus::Busy { .. } => {
            if let BotSteerRunResult::Steered { run_id } = steer_run(ctx, delivery, target).await? {
                return Ok(Some(DeliveryOutcome {
                    outcome: BotEventOutcome::Steered,
                    summary: None,
                    run_id: Some(run_id),
                }));
            }
        }
        BotSessionStatus::Idle | BotSessionStatus::Closed => {}
    }
    // The tracked run finished (or takes no steering right now) under us.
    // Preserve the delivery for another try, or for an ordinary lane once the
    // session is available.
    let not_before_ms = now_ms(ctx) + BOT_BUSY_RETRY_DELAY.as_millis() as i64;
    ctx.state_mut(|state| state.requeue_front(delivery.clone(), not_before_ms));
    Ok(None)
}

// ── Finishing ───────────────────────────────────────────────────────────────

fn recent_snapshot(
    delivery: &BotDelivery,
    session_id: &str,
    outcome: &DeliveryOutcome,
    usage: Option<LlmUsageView>,
    finished_at_ms: i64,
) -> BotRecentDeliverySnapshot {
    BotRecentDeliverySnapshot {
        delivery_id: delivery.id.clone(),
        seqs: delivery.seqs(),
        session_id: session_id.to_owned(),
        run_id: outcome.run_id.clone(),
        outcome: outcome.outcome,
        summary: outcome.summary.clone(),
        finished_at_ms,
        usage,
    }
}

/// Remember the delivery, free the lane (the session is dispatchable again
/// at once), then settle the read model and receipts best-effort.
async fn finish_delivery(
    ctx: &Ctx,
    session_id: &str,
    delivery: &BotDelivery,
    outcome: DeliveryOutcome,
    usage: Option<LlmUsageView>,
) {
    let now = now_ms(ctx);
    let recent = recent_snapshot(delivery, session_id, &outcome, usage, now);
    ctx.state_mut(|state| {
        state.remember_delivery(recent);
        state.release_lane(session_id, now);
    });
    settle_delivery(ctx, delivery, session_id, &outcome).await;
}

/// The write-once outcome on every event row, `bot.reply` receipts for
/// events that asked, and the `finished` delivery receipt for events whose
/// source asked. Each is best effort — a failed write costs a pending
/// badge or a receipt, never a delivery.
async fn settle_delivery(
    ctx: &Ctx,
    delivery: &BotDelivery,
    session_id: &str,
    outcome: &DeliveryOutcome,
) {
    let (universe_id, bot_id) =
        ctx.state(|state| (state.config.universe_id, state.config.bot_id.clone()));
    if let Err(message) = activity(
        ctx,
        BotActivities::record_outcomes,
        BotRecordOutcomesRequest {
            universe_id,
            bot_id: bot_id.clone(),
            event_ids: delivery.event_ids(),
            outcome: outcome.outcome,
            detail: outcome.summary.clone(),
            run_id: outcome.run_id.clone(),
        },
    )
    .await
    {
        ctx.state_mut(|state| state.record_error(message));
    }
    ctx.state_mut(|state| state.lane_tick += 1);

    let reply_ids = delivery.reply_event_ids();
    if !reply_ids.is_empty() {
        if let Err(message) = activity(
            ctx,
            BotActivities::send_bot_receipts,
            BotSendBotReceiptsRequest {
                universe_id,
                bot_id,
                delivery_id: delivery.id.clone(),
                event_ids: reply_ids,
                outcome: outcome.outcome,
                summary: outcome.summary.clone(),
                hops: delivery.hops(),
            },
        )
        .await
        {
            ctx.state_mut(|state| state.record_error(message));
        }
        ctx.state_mut(|state| state.lane_tick += 1);
    }

    send_delivery_receipts(
        ctx,
        delivery,
        session_id,
        BotDeliveryPhase::Finished,
        outcome.run_id.clone(),
        Some(outcome.outcome),
        outcome.summary.clone(),
    )
    .await;
}

/// `started` / `finished` receipts to the admitting source of any event in
/// the delivery that asked for them (a chat conversation waiting to type
/// and to send the fallback reply). Best effort.
async fn send_delivery_receipts(
    ctx: &Ctx,
    delivery: &BotDelivery,
    session_id: &str,
    phase: BotDeliveryPhase,
    run_id: Option<String>,
    outcome: Option<BotEventOutcome>,
    summary: Option<String>,
) {
    let event_ids = delivery.notify_event_ids();
    if event_ids.is_empty() {
        return;
    }
    let (universe_id, bot_id) =
        ctx.state(|state| (state.config.universe_id, state.config.bot_id.clone()));
    if let Err(message) = activity(
        ctx,
        BotActivities::send_delivery_receipts,
        BotSendDeliveryReceiptsRequest {
            universe_id,
            bot_id,
            event_ids,
            phase,
            delivery_id: delivery.id.clone(),
            seqs: delivery.seqs(),
            session_id: session_id.to_owned(),
            run_id,
            outcome,
            summary,
        },
    )
    .await
    {
        ctx.state_mut(|state| state.record_error(message));
    }
    ctx.state_mut(|state| state.lane_tick += 1);
}

// ── Pushed tools ────────────────────────────────────────────────────────────

/// Answer a pushed `bot_*` invocation from the controller's own state and
/// the tool activity, then resolve the session's parked call by signalling
/// the holder session workflow directly. Every pushed tool is joined —
/// including `bot_emit`, whose refusals the model must read — so a failed
/// activity still resolves the promise, as failed.
pub(super) async fn handle_invocation(ctx: Ctx, pending: PendingInvocation) {
    let PendingInvocation {
        invocation,
        holder_workflow_id,
    } = pending;
    let Some(reply_promise_id) = invocation
        .completion_promises
        .as_ref()
        .and_then(|promises| promises.get(REPLY_COMPLETION_KEY))
        .cloned()
    else {
        ctx.state_mut(|state| {
            state.record_error(format!(
                "pushed invocation {} of {} carries no reply promise",
                invocation.invocation_id, invocation.workflow_tool_id
            ))
        });
        return;
    };
    let now = now_ms(&ctx);
    let (universe_id, request) = ctx.state(|state| {
        (
            state.config.universe_id,
            BotExecuteToolRequest {
                universe_id: state.config.universe_id,
                bot_id: state.config.bot_id.clone(),
                session_id: invocation.session_id.as_str().to_owned(),
                invocation_id: invocation.invocation_id.as_str().to_owned(),
                tool_id: invocation.workflow_tool_id.as_str().to_owned(),
                arguments_ref: invocation.arguments_ref.as_str().to_owned(),
                controller: state.controller_summary(invocation.session_id.as_str(), now),
            },
        )
    });
    let resolution = match activity(&ctx, BotActivities::execute_tool, request).await {
        Ok(BotExecuteToolResult::Resolved { payload_ref }) => PromiseResolution::Resolved {
            payload_ref: parse_blob_ref(&ctx, payload_ref),
        },
        Ok(BotExecuteToolResult::Failed { error_ref, .. }) => PromiseResolution::Failed {
            error_ref: parse_blob_ref(&ctx, error_ref),
        },
        Err(message) => {
            ctx.state_mut(|state| state.record_error(message));
            PromiseResolution::Failed { error_ref: None }
        }
    };
    let envelope = EmissionEnvelope::source_resolution(
        universe_id,
        ctx.workflow_id().to_owned(),
        &holder_workflow_id,
        reply_promise_id,
        resolution,
    );
    let signalled = ctx
        .external_workflow(holder_workflow_id.clone(), None)
        .signal(
            AgentSessionWorkflow::deliver_emission,
            crate::workflows::emission_signal_input(&envelope),
            crate::workflows::signal_options(),
        )
        .await;
    if let Err(failure) = signalled {
        ctx.state_mut(|state| {
            state.record_error(format!(
                "resolve pushed invocation at {holder_workflow_id}: {}",
                failure
            ))
        });
    }
    ctx.state_mut(|state| state.lane_tick += 1);
}

/// A CAS ref the tool activity stored; a malformed one is recorded and
/// resolves the promise without a payload.
fn parse_blob_ref(ctx: &Ctx, value: String) -> Option<BlobRef> {
    match BlobRef::parse(value) {
        Ok(blob_ref) => Some(blob_ref),
        Err(error) => {
            ctx.state_mut(|state| state.record_error(format!("tool result ref: {error}")));
            None
        }
    }
}
