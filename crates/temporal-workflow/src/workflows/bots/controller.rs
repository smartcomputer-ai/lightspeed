//! One durable inbox, router, and session lifecycle controller per bot,
//! ported from the TypeScript `botControllerWorkflowV1`.
//!
//! Signals only mutate state and wake the loop; every decision that needs
//! no I/O lives in [`state`]. The loop runs one pass per wake — teardown
//! check, emissions, dead-session sweep, rotations, ripe buffers, idle-close
//! sweep, descendant budget, rename, main-session reconcile, dispatch,
//! continue-as-new — then
//! parks on a wait condition raced against the earliest deadline. Deliveries
//! run as lanes: boxed futures polled beside whatever the loop awaits, one
//! per target session plus one steer/append sidecar, so a stalled run
//! blocks only its own session. Lanes are polled with the loop's own task
//! context (no `FuturesUnordered`, no custom wakers), the same discipline
//! the session workflow's tool batches follow.

mod lanes;
mod state;

use crate::workflows::WorkflowContextExt as _;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, UNIX_EPOCH};

use api::{BotControllerSnapshot, BotEventOutcome, BotRecentDeliverySnapshot, ProfileId};
use bots::tools::parse_event_resolve_args;
use bots::{
    BOT_CONFIG_SIGNAL, BOT_EVENT_SIGNAL, BOT_SESSION_ROTATE_SIGNAL, BOT_STATE_QUERY,
    BotControllerConfig, BotEvent, BotSessionRotate,
};
use futures::future::poll_fn;
use futures::{FutureExt, pin_mut, select_biased};
use harness::WorkflowEndpointRef;
use serde::{Deserialize, Serialize};
use temporalio_common::data_converters::RawValue;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use super::{
    BOT_BUSY_RETRY_DELAY, BotActivities, BotCloseSessionRequest, BotCountDescendantsRequest,
    BotEnsureSessionRequest, BotEnsureSessionResult, BotReadDeadSessionsRequest,
    BotReadJsonBlobRequest, BotReadToolInvocationsRequest, BotRecordClosedRequest,
    BotRecordOutcomesRequest, BotRenameSessionRequest, BotSessionStatus, bot_activity_options,
};
pub use state::{BotDelivery, CoalesceBuffer, ControlledSession, WaitingLane};
use state::{
    ControllerState, DispatchAction, EmissionDisposition, LaneResolution, LaneTerminal,
    PendingInvocation, ROTATION_RETRY_DELAY_MS, resolve_invocations_for_run, used_carried_tool,
};

/// `workflow_kind` the controller records as lifecycle controller and
/// pushed-tool receiver of its sessions.
pub const BOT_CONTROLLER_WORKFLOW_KIND: &str = "BotControllerWorkflow";

/// Start argument of the controller: the bot's configuration and what it starts
/// from, which is the previous execution's state after a continue-as-new and
/// the generations of the bot's retained sessions at any other start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotControllerArgs {
    pub config: BotControllerConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry: Option<BotControllerCarry>,
}

/// Everything a controller execution hands its successor. Every field has
/// a default so a carry written by an older execution still loads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BotControllerCarry {
    /// Lanes parked on a run that is still going.
    pub waiting_lanes: Vec<WaitingLane>,
    /// The main session still owes a reconcile.
    pub config_dirty: bool,
    pub pending_deliveries: Vec<BotDelivery>,
    pub buffers: BTreeMap<String, CoalesceBuffer>,
    pub recent_deliveries: Vec<BotRecentDeliverySnapshot>,
    /// Dedupe tails, capped at `BOT_SEEN_ID_CAP` in insertion order.
    pub seen_event_ids: Vec<String>,
    pub seen_emission_ids: Vec<String>,
    pub handled_invocation_ids: Vec<String>,
    /// Session log cursor per session id (main generations included).
    pub session_cursors: BTreeMap<String, u64>,
    pub applied_profile_id: Option<ProfileId>,
    pub applied_profile_revision: Option<u64>,
    pub session_ready: bool,
    /// Always 0: the continue-as-new threshold counts per execution.
    pub events_processed: u64,
    pub duplicate_events: u64,
    pub duplicate_emissions: u64,
    /// UTC day (`YYYY-MM-DD`) the budget counters belong to.
    pub run_day: Option<String>,
    pub runs_today: u32,
    pub descendants_today: u32,
    /// Bot sessions whose sub-agent trees are counted for `run_day`.
    pub budget_roots: Vec<String>,
    /// Routed sessions this controller created, capped at
    /// `BOT_EXTRA_SESSION_CAP`.
    pub extra_sessions: Vec<ControlledSession>,
    /// Routed base id → generation, bumped each time that session closes.
    pub session_generations: BTreeMap<String, u32>,
    /// Operator rotations that have not reached an idle boundary yet.
    pub rotation_requests: Vec<String>,
    /// Main session generation; 0 (an old carry) reads as 1.
    pub main_generation: u32,
    /// Tool declaration revision the main session was created under.
    pub tools_revision: Option<u32>,
    /// Failed main-session reconciles since the last success or config signal.
    pub reconcile_attempts: u32,
    /// That streak has already rotated the main session once.
    pub reconcile_rotated: bool,
}

#[workflow]
pub struct BotControllerWorkflow {
    state: ControllerState,
}

/// The workflow struct is the SDK's state holder; the loop, lanes, and
/// handlers all work on the pure [`ControllerState`] behind it.
impl std::ops::Deref for BotControllerWorkflow {
    type Target = ControllerState;

    fn deref(&self) -> &ControllerState {
        &self.state
    }
}

impl std::ops::DerefMut for BotControllerWorkflow {
    fn deref_mut(&mut self) -> &mut ControllerState {
        &mut self.state
    }
}

/// The workflow context every loop helper and lane works through. Helpers
/// take it by shared reference: state access and activity starts need no
/// exclusive borrow, which is what lets lanes hold their own clone.
type Ctx = WorkflowContext<BotControllerWorkflow>;

/// A detached unit of work polled beside the loop: a delivery lane, a
/// busy-session sidecar, or a pushed-tool answer. Each one finishes its
/// own bookkeeping through its context clone, so its output is `()`.
type LaneFuture = Pin<Box<dyn Future<Output = ()>>>;

#[workflow_methods]
impl BotControllerWorkflow {
    /// State exists before the first signal is dispatched, so an event
    /// riding a signal-with-start lands on the initialized controller.
    #[init]
    pub fn new(ctx: &WorkflowContextView, args: BotControllerArgs) -> Self {
        let now = ctx
            .start_time()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0);
        Self {
            state: ControllerState::new(args, now),
        }
    }

    #[run(name = "BotControllerWorkflow")]
    pub async fn run(ctx: &mut WorkflowContext<Self>) -> WorkflowResult<()> {
        run_controller(ctx).await
    }

    /// An admitted event: dedupe, then queue a delivery or fold it into
    /// its coalesce buffer. Malformed signals are recorded and dropped;
    /// the row in Postgres is authoritative either way.
    #[signal(name = BOT_EVENT_SIGNAL)]
    pub fn bot_event(&mut self, ctx: &mut SyncWorkflowContext<Self>, event: BotEvent) {
        let now = sync_now_ms(ctx);
        self.state.clock_ms = self.state.clock_ms.max(now);
        if let Err(message) = self.state.accept_event(event, now) {
            self.state
                .record_error(format!("bot_event rejected: {message}"));
        }
    }

    /// The bot record changed: identity is fixed at start (a mismatch is
    /// recorded and ignored), a display-name change is label-only, and the
    /// rest reconciles the main session at its next idle boundary.
    #[signal(name = BOT_CONFIG_SIGNAL)]
    pub fn bot_config(&mut self, ctx: &mut SyncWorkflowContext<Self>, next: BotControllerConfig) {
        self.state.clock_ms = self.state.clock_ms.max(sync_now_ms(ctx));
        if let Err(message) = self.state.accept_config(next) {
            self.state
                .record_error(format!("bot_config rejected: {message}"));
        }
    }

    #[signal(name = BOT_SESSION_ROTATE_SIGNAL)]
    pub fn bot_session_rotate(
        &mut self,
        ctx: &mut SyncWorkflowContext<Self>,
        request: BotSessionRotate,
    ) {
        self.state.clock_ms = self.state.clock_ms.max(sync_now_ms(ctx));
        self.state.request_rotation(request.session_id);
    }

    /// Pushed `bot_*` invocations and run terminals of the bot's sessions.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(&mut self, ctx: &mut SyncWorkflowContext<Self>, emission: RawValue) {
        self.state.clock_ms = self.state.clock_ms.max(sync_now_ms(ctx));
        if let Ok(envelope) = crate::workflows::receive_emission(ctx, &emission) {
            self.state.queue_emission(envelope);
        }
    }

    /// Pure: derives everything from state at the last observed workflow
    /// time, never rolling the budget day.
    #[query(name = BOT_STATE_QUERY)]
    pub fn bot_state(&self, _ctx: &WorkflowContextView) -> BotControllerSnapshot {
        self.state.snapshot(self.state.clock_ms)
    }
}

// ── The loop ────────────────────────────────────────────────────────────────

async fn run_controller(ctx: &mut Ctx) -> WorkflowResult<()> {
    let ctx: &Ctx = &*ctx;
    let mut lanes: Vec<LaneFuture> = ctx
        .state(|state| state.waiting_lanes())
        .into_iter()
        .map(|lane| Box::pin(lanes::resume_run_wait(ctx.clone(), lane)) as LaneFuture)
        .collect();

    loop {
        tick_clock(ctx);
        if ctx.state(|state| state.close_requested()) {
            with_lanes(&mut lanes, teardown(ctx)).await;
            return Ok(());
        }
        with_lanes(&mut lanes, process_emissions(ctx)).await;
        spawn_invocation_lanes(ctx, &mut lanes);
        with_lanes(&mut lanes, sweep_dead_sessions(ctx)).await;
        with_lanes(&mut lanes, rotate_requested_sessions(ctx)).await;
        let now = now_ms(ctx);
        ctx.state_mut(|state| state.flush_ripe_buffers(now));
        with_lanes(&mut lanes, sweep_routed_sessions(ctx)).await;
        with_lanes(&mut lanes, refresh_descendants_today(ctx)).await;
        if ctx.state(|state| state.rename_dirty) {
            with_lanes(&mut lanes, apply_display_name(ctx)).await;
        }
        let now = now_ms(ctx);
        if ctx.state(|state| state.reconcile_due(now)) {
            with_lanes(&mut lanes, reconcile_unless_busy(ctx)).await;
        }
        if ctx.state(|state| state.config.enabled) {
            let now = now_ms(ctx);
            for action in ctx.state_mut(|state| state.dispatch(now)) {
                lanes.push(match action {
                    DispatchAction::Lane { delivery, target } => {
                        Box::pin(lanes::run_delivery(ctx.clone(), delivery, target))
                    }
                    DispatchAction::Sidecar { delivery, target } => {
                        Box::pin(lanes::run_busy_sidecar(ctx.clone(), delivery, target))
                    }
                });
            }
        }
        let suggested = ctx.continue_as_new_suggested();
        if ctx.state(|state| state.can_continue_as_new(suggested, lanes.len())) {
            return request_continue_as_new(ctx);
        }
        park(ctx, &mut lanes).await;
    }
}

/// Wait for the earliest of: state the loop must act on, or the earliest
/// time-driven deadline (buffer flush, idle-close deadline, rotation or
/// reconcile retry, a waiting delivery, UTC day boundary while budget-parked,
/// descendant refresh, dead-session sweep). Lanes keep running underneath.
async fn park(ctx: &Ctx, lanes: &mut Vec<LaneFuture>) {
    let now = now_ms(ctx);
    let (lane_tick, event_tick, deadline) =
        ctx.state(|state| (state.lane_tick, state.event_tick, state.wake_deadline(now)));
    let wait = ctx.wait_for_state(move |state| state.wake_ready(lane_tick, event_tick));
    match deadline {
        None => with_lanes(lanes, wait).await,
        Some(deadline) => {
            let timer = ctx
                .timer_with_manual_cancellation(Duration::from_millis(
                    (deadline - now).max(1) as u64
                ))
                .fuse();
            with_lanes(lanes, async move {
                pin_mut!(wait, timer);
                select_biased! {
                    _ = wait => {}
                    _ = timer => {}
                }
            })
            .await;
        }
    }
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

fn spawn_invocation_lanes(ctx: &Ctx, lanes: &mut Vec<LaneFuture>) {
    for pending in ctx.state_mut(|state| std::mem::take(&mut state.pending_invocations)) {
        lanes.push(Box::pin(lanes::handle_invocation(ctx.clone(), pending)));
    }
}

fn request_continue_as_new(ctx: &Ctx) -> WorkflowResult<()> {
    let args = ctx.state(|state| BotControllerArgs {
        config: state.config.clone(),
        carry: Some(state.carry()),
    });
    match ctx.continue_as_new(args, ContinueAsNewOptions::default()) {
        Ok(never) => match never {},
        Err(termination) => Err(termination),
    }
}

// ── Clock ───────────────────────────────────────────────────────────────────

/// Workflow time in ms since the epoch: the SDK's deterministic clock,
/// never the wall clock.
pub(super) fn now_ms(ctx: &Ctx) -> i64 {
    ctx.workflow_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn sync_now_ms(ctx: &SyncWorkflowContext<BotControllerWorkflow>) -> i64 {
    ctx.workflow_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn tick_clock(ctx: &Ctx) {
    let now = now_ms(ctx);
    ctx.state_mut(|state| state.clock_ms = state.clock_ms.max(now));
}

// ── Activities ──────────────────────────────────────────────────────────────

/// One bot activity with the shared options; the failure text is what the
/// controller records as `last_error`.
pub(super) async fn activity<AD: temporalio_common::ActivityDefinition>(
    ctx: &Ctx,
    definition: AD,
    input: AD::Input,
) -> Result<AD::Output, String> {
    crate::workflows::fail_task_if_undecodable(
        ctx.execute_activity(definition, input, bot_activity_options())
            .await,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn ensure_request(
    state: &ControllerState,
    workflow_id: &str,
    session_id: String,
    display_name: String,
    applied_profile_revision: Option<u64>,
    tools_ref: Option<String>,
    routed_session: Option<&bots::RoutedSession>,
) -> BotEnsureSessionRequest {
    BotEnsureSessionRequest {
        universe_id: state.config.universe_id,
        bot_id: state.config.bot_id.clone(),
        session_id,
        session_key: routed_session.and_then(|session| session.session_key.clone()),
        session_label: routed_session.map(|session| session.label.clone()),
        display_name: Some(display_name),
        profile_id: state.config.profile_id.clone(),
        brief: state.config.brief.clone(),
        self_config: state.config.self_config,
        emit: state.config.emit,
        applied_profile_revision,
        controller: WorkflowEndpointRef {
            workflow_id: workflow_id.to_owned(),
            workflow_kind: BOT_CONTROLLER_WORKFLOW_KIND.to_owned(),
        },
        tools_ref,
    }
}

// ── Emissions ───────────────────────────────────────────────────────────────

/// Drain the emission inbox: pushed invocations queue a lane, run
/// terminals first pull the run's `bot_event_resolve` calls from the log
/// and then attach to the lane whose delivery token they carry.
async fn process_emissions(ctx: &Ctx) {
    while let Some(envelope) = ctx.state_mut(|state| state.emission_inbox.pop_front()) {
        let Some(disposition) = ctx.state_mut(|state| state.classify_emission(envelope)) else {
            continue;
        };
        match disposition {
            EmissionDisposition::Foreign(message) => {
                ctx.state_mut(|state| state.record_error(message));
            }
            EmissionDisposition::Invocation {
                invocation,
                holder_workflow_id,
                ..
            } => {
                ctx.state_mut(|state| {
                    state.pending_invocations.push(PendingInvocation {
                        invocation: *invocation,
                        holder_workflow_id,
                    });
                });
            }
            EmissionDisposition::Terminal {
                session_id,
                run_id,
                token,
                status,
                failure_message_ref,
            } => {
                // With no lane on that session the delivery is between attempts, has
                // moved on, or has settled. Reading the log now would move the cursor
                // past calls its next attempt, or the next delivery's lane, still
                // needs.
                if !ctx.state(|state| state.lane_awaits_terminal_from(&session_id, &token)) {
                    continue;
                }
                reconcile_run(ctx, &run_id, &session_id).await;
                ctx.state_mut(|state| {
                    state.attach_terminal(
                        &token,
                        LaneTerminal {
                            status,
                            run_id,
                            failure_message_ref,
                            dead_session: None,
                        },
                    )
                });
            }
            EmissionDisposition::Ignore => {}
        }
    }
}

/// Read the session log past its cursor and fold the run's resolve calls
/// into the lane on that session (run-scoped correlation: one delivery
/// per run, the last call wins). A run that used a carried tool without
/// resolving counts as handled. Read failures are recorded, never fatal:
/// the terminal still lands and the delivery ends `unresolved`.
async fn reconcile_run(ctx: &Ctx, run_id: &str, target: &str) {
    let (universe_id, after_seq) =
        ctx.state(|state| (state.config.universe_id, state.cursor_for(target)));
    let pulled = match activity(
        ctx,
        BotActivities::read_tool_invocations,
        BotReadToolInvocationsRequest {
            universe_id,
            session_id: target.to_owned(),
            after_seq,
        },
    )
    .await
    {
        Ok(pulled) => pulled,
        Err(message) => {
            ctx.state_mut(|state| {
                state.record_error(format!("read tool invocations of {target}: {message}"))
            });
            return;
        }
    };
    ctx.state_mut(|state| state.set_cursor(target, pulled.next_seq));
    for invocation in resolve_invocations_for_run(&pulled.invocations, run_id) {
        let args = match activity(
            ctx,
            BotActivities::read_json_blob,
            BotReadJsonBlobRequest {
                universe_id,
                blob_ref: invocation.arguments_ref.clone(),
            },
        )
        .await
        {
            Ok(args) => args,
            Err(message) => {
                ctx.state_mut(|state| {
                    state.record_error(format!(
                        "read bot_event_resolve arguments {}: {message}",
                        invocation.arguments_ref
                    ))
                });
                continue;
            }
        };
        match parse_event_resolve_args(&args) {
            Ok(parsed) => ctx.state_mut(|state| {
                state.set_lane_resolution(
                    target,
                    LaneResolution {
                        outcome: parsed.outcome,
                        summary: parsed.summary,
                    },
                )
            }),
            Err(message) => ctx.state_mut(|state| {
                state.record_error(format!("bot_event_resolve arguments invalid: {message}"))
            }),
        }
    }
    let unresolved = ctx.state(|state| {
        state
            .active_by_session
            .get(target)
            .is_some_and(|lane| lane.resolution.is_none())
    });
    if unresolved {
        let carried = ctx.state(|state| {
            state
                .extra_session(target)
                .map(|session| session.carried_tool_ids.clone())
                .unwrap_or_default()
        });
        if used_carried_tool(&pulled.invocations, run_id, &carried) {
            ctx.state_mut(|state| {
                state.set_lane_resolution(
                    target,
                    LaneResolution {
                        outcome: BotEventOutcome::Handled,
                        summary: None,
                    },
                )
            });
        }
    }
}

// ── Sessions ────────────────────────────────────────────────────────────────

/// One attempt at a dirty main session per pass. A main session running work
/// the controller does not own, such as an operator's direct run, is read once
/// and tried again later, so the loop keeps serving everything else meanwhile.
/// A dead one is left behind, and reconcile creates its successor.
async fn reconcile_unless_busy(ctx: &Ctx) {
    // A rotated generation does not exist until reconcile creates it; only an
    // already-ready session needs the check. A failed read says nothing about
    // the session, and reconcile reports its own failures.
    if ctx.state(|state| state.session_ready) {
        let main = ctx.state(|state| state.main_session_id());
        match lanes::read_session_status(ctx, &main).await {
            Ok(BotSessionStatus::Busy { .. }) => {
                let retry_at = now_ms(ctx) + BOT_BUSY_RETRY_DELAY.as_millis() as i64;
                ctx.state_mut(|state| state.reconcile_retry_at_ms = Some(retry_at));
                return;
            }
            Ok(status) if status.is_dead() => {
                let rolled_forward = roll_forward(ctx, &main, &status).await;
                if !rolled_forward {
                    let now = now_ms(ctx);
                    let message = lanes::dead_session_message(&main, &status);
                    ctx.state_mut(|state| state.mark_session_degraded(message, now));
                    return;
                }
            }
            _ => {}
        }
    }
    reconcile_session(ctx).await;
}

/// Create or bring the main session in line with the configuration. Tool
/// declarations are immutable per session and a pinned provider api kind
/// may refuse the new profile, so a mismatch rotates to a successor
/// generation and retries once, and so does a session that turns out dead;
/// any other failure degrades the controller, which tries again with a
/// backoff or at once on the next config signal.
async fn reconcile_session(ctx: &Ctx) {
    for attempt in 0..2 {
        let request = ctx.state(|state| {
            ensure_request(
                state,
                ctx.workflow_id(),
                state.main_session_id(),
                state.bot_label(),
                state.applied_revision_for_ensure(),
                None,
                None,
            )
        });
        let message = match activity(ctx, BotActivities::ensure_session, request).await {
            Ok(BotEnsureSessionResult::Ready {
                profile_revision, ..
            }) => {
                ctx.state_mut(|state| state.mark_session_ready(profile_revision));
                return;
            }
            Ok(
                BotEnsureSessionResult::DeclarationMismatch { message }
                | BotEnsureSessionResult::ProfileUnapplicable { message },
            ) => {
                if attempt == 0 && ctx.state_mut(|state| state.rotate_main_for_reconcile()) {
                    continue;
                }
                message
            }
            Ok(BotEnsureSessionResult::SessionDead { message }) => {
                let main = ctx.state(|state| state.main_session_id());
                if attempt == 0
                    && !ctx.state(|state| state.reconcile_rotated)
                    && roll_forward(ctx, &main, &BotSessionStatus::Failed).await
                {
                    ctx.state_mut(|state| state.reconcile_rotated = true);
                    continue;
                }
                message
            }
            Err(message) => message,
        };
        let now = now_ms(ctx);
        ctx.state_mut(|state| state.mark_session_degraded(message, now));
        return;
    }
}

/// Move a conversation off a dead session: close it for good, then advance its
/// generation. The next delivery opens a successor with no context, and the old
/// transcript stays readable. Coming back on the same session is an operator's
/// `session/restart`, never this. `false`, with the generation unchanged, when the
/// close failed, which is recorded: the next delivery finds the session dead again
/// and has another try.
pub(super) async fn roll_forward(ctx: &Ctx, session_id: &str, status: &BotSessionStatus) -> bool {
    if needs_close_to_roll_forward(status) && !close_session(ctx, session_id, true).await {
        return false;
    }
    ctx.state_mut(|state| state.roll_session_forward(session_id));
    true
}

/// A missing session has no row left to close.
fn needs_close_to_roll_forward(status: &BotSessionStatus) -> bool {
    *status != BotSessionStatus::Missing
}

/// Lanes waiting on a started run hear from nothing but a run terminal, which a dead
/// session never sends. On a slow clock, one read covers every such lane, and a dead
/// session settles its lane `runFailed` and moves on. A live session is left alone
/// however long its run takes, even if it is wedged: that one is an operator's to
/// force-close, and the close sends the terminal. A failed read waits for the next
/// sweep.
async fn sweep_dead_sessions(ctx: &Ctx) {
    let now = now_ms(ctx);
    let session_ids = ctx.state_mut(|state| state.take_dead_session_sweep(now));
    if session_ids.is_empty() {
        return;
    }
    let universe_id = ctx.state(|state| state.config.universe_id);
    let dead = match activity(
        ctx,
        BotActivities::read_dead_sessions,
        BotReadDeadSessionsRequest {
            universe_id,
            session_ids,
        },
    )
    .await
    {
        Ok(result) => result.dead,
        Err(message) => {
            ctx.state_mut(|state| state.record_error(format!("read dead sessions: {message}")));
            return;
        }
    };
    for session in dead {
        let message = format!(
            "{}; the run reported no end and may have partly happened",
            lanes::dead_session_message(&session.session_id, &session.status)
        );
        if ctx.state_mut(|state| state.fail_lane_on_dead_session(&session.session_id, message)) {
            roll_forward(ctx, &session.session_id, &session.status).await;
        }
    }
}

/// Close operator-selected sessions at an idle boundary, then advance
/// their generation. Pending deliveries stay queued and resolve against
/// the successor id. Busy targets are retried after a short pause.
async fn rotate_requested_sessions(ctx: &Ctx) {
    let now = now_ms(ctx);
    let targets: Vec<String> = ctx.state_mut(|state| {
        if state.rotation_requests.is_empty() {
            state.rotation_retry_at_ms = None;
            return Vec::new();
        }
        if state
            .rotation_retry_at_ms
            .is_some_and(|retry_at| now < retry_at)
        {
            return Vec::new();
        }
        state.rotation_requests.iter().cloned().collect()
    });
    if targets.is_empty() {
        return;
    }
    let mut retry = false;
    for target in targets {
        let (is_main, is_routed, busy) = ctx.state(|state| {
            (
                target == state.main_session_id(),
                state.extra_session(&target).is_some(),
                state.is_session_busy(&target),
            )
        });
        if !is_main && !is_routed {
            // A duplicate or stale request is already satisfied.
            ctx.state_mut(|state| {
                state.rotation_requests.remove(&target);
            });
            continue;
        }
        if busy {
            retry = true;
            continue;
        }
        let closed = close_session(ctx, &target, false).await;
        if !closed {
            retry = true;
            continue;
        }
        ctx.state_mut(|state| {
            state.budget_roots.insert(target.clone());
            state.rotation_requests.remove(&target);
            state.last_error = None;
            if is_main {
                state.rotate_main_session(true);
            } else {
                state.forget_routed_session(&target);
            }
        });
    }
    let retry_at = retry.then(|| now + ROTATION_RETRY_DELAY_MS);
    ctx.state_mut(|state| state.rotation_retry_at_ms = retry_at);
}

/// Close routed sessions idle past their close window. A session that
/// will not close (busy, unreachable) has its expiry pushed out.
async fn sweep_routed_sessions(ctx: &Ctx) {
    let now = now_ms(ctx);
    for session_id in ctx.state(|state| state.expired_sessions(now)) {
        if close_session(ctx, &session_id, false).await {
            ctx.state_mut(|state| state.forget_routed_session(&session_id));
        } else {
            ctx.state_mut(|state| state.touch_session(&session_id, now));
        }
    }
}

/// `true` when the session is closed now; a failure is recorded.
pub(super) async fn close_session(ctx: &Ctx, session_id: &str, force: bool) -> bool {
    let universe_id = ctx.state(|state| state.config.universe_id);
    match activity(
        ctx,
        BotActivities::close_session,
        BotCloseSessionRequest {
            universe_id,
            session_id: session_id.to_owned(),
            force,
        },
    )
    .await
    {
        Ok(result) => result.closed,
        Err(message) => {
            ctx.state_mut(|state| state.record_error(format!("close {session_id}: {message}")));
            false
        }
    }
}

/// Re-count today's sub-agent descendants: after every finished delivery
/// and, while a run is in flight, once a minute. Best effort — a core
/// outage keeps the last count rather than blocking dispatch.
async fn refresh_descendants_today(ctx: &Ctx) {
    let now = now_ms(ctx);
    let request = ctx.state_mut(|state| {
        state.roll_budget_day(now);
        if !state.descendants_refresh_due(now) {
            return None;
        }
        let session_ids = state.budget_root_ids();
        state.descendants_refreshed_at_ms = now;
        state.descendants_refreshed_processed = Some(state.events_processed);
        Some(BotCountDescendantsRequest {
            universe_id: state.config.universe_id,
            session_ids,
            since_ms: state.day_start_ms(),
        })
    });
    let Some(request) = request else {
        return;
    };
    match activity(ctx, BotActivities::count_descendants, request).await {
        Ok(counted) => ctx.state_mut(|state| state.descendants_today = counted.count),
        Err(message) => ctx.state_mut(|state| state.record_error(message)),
    }
}

/// Apply a display-name change to every controlled session. Label-only and
/// best effort: a failed rename costs a stale label, never a delivery.
async fn apply_display_name(ctx: &Ctx) {
    let targets: Vec<(String, String)> = ctx.state_mut(|state| {
        state.rename_dirty = false;
        let mut targets = vec![(state.main_session_id(), state.bot_label())];
        targets.extend(state.extra_sessions.iter().map(|session| {
            (
                session.session_id.clone(),
                state.routed_label(&session.label),
            )
        }));
        targets
    });
    let universe_id = ctx.state(|state| state.config.universe_id);
    for (session_id, display_name) in targets {
        if let Err(message) = activity(
            ctx,
            BotActivities::rename_session,
            BotRenameSessionRequest {
                universe_id,
                session_id,
                display_name: Some(display_name),
            },
        )
        .await
        {
            ctx.state_mut(|state| state.record_error(message));
        }
    }
}

// ── Teardown ────────────────────────────────────────────────────────────────

/// Terminal teardown (bot close): archive everything not yet delivered,
/// force-close every session this controller knows (main generations and
/// routed sessions), record them on the row, and mark done — the caller
/// completes the workflow right after and never continues as new. Every
/// step is an idempotent activity; a failed one is recorded, and a fresh
/// run started with `closed` repeats the whole procedure. Lanes still in
/// flight lose their session underneath and are dropped with the run.
async fn teardown(ctx: &Ctx) {
    ctx.state_mut(|state| state.closing = true);
    let (universe_id, bot_id, event_ids) = ctx.state(|state| {
        (
            state.config.universe_id,
            state.config.bot_id.clone(),
            state.teardown_event_ids(),
        )
    });
    if !event_ids.is_empty()
        && let Err(message) = activity(
            ctx,
            BotActivities::record_outcomes,
            BotRecordOutcomesRequest {
                universe_id,
                bot_id: bot_id.clone(),
                event_ids,
                outcome: BotEventOutcome::Archived,
                detail: Some("bot_closed".to_owned()),
                run_id: None,
            },
        )
        .await
    {
        ctx.state_mut(|state| state.record_error(message));
    }
    ctx.state_mut(|state| {
        state.pending_deliveries.clear();
        state.buffers.clear();
    });
    let sessions = ctx.state(|state| state.teardown_sessions());
    for session_id in &sessions {
        close_session(ctx, session_id, true).await;
    }
    if let Err(message) = activity(
        ctx,
        BotActivities::record_closed,
        BotRecordClosedRequest {
            universe_id,
            bot_id,
            sessions,
        },
    )
    .await
    {
        ctx.state_mut(|state| state.record_error(message));
    }
    ctx.state_mut(|state| state.closed_done = true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_and_query_names_match_the_bots_contract() {
        assert_eq!(BOT_EVENT_SIGNAL, "bot_event");
        assert_eq!(BOT_CONFIG_SIGNAL, "bot_config");
        assert_eq!(BOT_SESSION_ROTATE_SIGNAL, "bot_session_rotate");
        assert_eq!(BOT_STATE_QUERY, "bot_state");
    }

    #[test]
    fn only_a_missing_session_rolls_forward_without_a_close() {
        assert!(!needs_close_to_roll_forward(&BotSessionStatus::Missing));
        assert!(needs_close_to_roll_forward(&BotSessionStatus::Failed));
        assert!(needs_close_to_roll_forward(&BotSessionStatus::Closed));
    }

    #[test]
    fn controller_args_decode_from_their_identity_alone() {
        let args: BotControllerArgs = serde_json::from_value(serde_json::json!({
            "config": {
                "universe_id": "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f",
                "bot_id": "triage",
                "profile_id": "triager",
            },
            "carry": {},
        }))
        .expect("a payload already in Temporal history stopped decoding: give the new field a serde default");
        assert_eq!(args.config.bot_id.as_str(), "triage");
        assert_eq!(args.carry, Some(BotControllerCarry::default()));
    }

    #[test]
    fn controller_args_round_trip_without_a_carry() {
        let args = BotControllerArgs {
            config: BotControllerConfig {
                universe_id: uuid::Uuid::from_u128(3),
                bot_id: api::BotId::new("triage"),
                display_name: None,
                profile_id: ProfileId::new("triager"),
                brief: None,
                runs_per_day: None,
                routed_session_close_after_ms: None,
                self_config: false,
                emit: false,
                enabled: true,
                closed: false,
            },
            carry: None,
        };
        let json = serde_json::to_string(&args).unwrap();
        assert!(!json.contains("carry"));
        let decoded: BotControllerArgs = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, args);
    }
}
