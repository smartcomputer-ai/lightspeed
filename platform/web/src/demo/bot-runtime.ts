/// In-page simulation of the core Bots controller. Manual and webhook events
/// use the same admission, routing and delivery logic.
import type { Context } from "hono";
import type {
  BotActiveDeliverySnapshot,
  BotControllerSnapshot,
  BotEventDocument,
  BotEventOutcome,
  BotEventView,
  BotListItem,
  BotSessionSnapshot,
  BotTriggerRoute,
  BotTriggerView,
  BotView,
  LlmUsageView,
  RunView,
  SessionSummaryView,
} from "@lightspeed-ai/sdk";
import type { ProfileDocument } from "@/api";
import {
  EVENT_ORIGIN,
  activeRun,
  applyEntries,
  closeSession,
  contextMessage,
  newSession,
  modelOf,
  startRun,
  steerRun,
  demoAccess,
} from "./engine";
import type { BotRecord, DemoStore, SessionRecord, StoredBotTrigger, UniverseState } from "./store";
import { sessionSummaryOf } from "./fixtures/builders";
import { foldActivity } from "@/components/activity-dot";
import { asRecord, isRecord } from "./input";

const NAME_PATTERN = /^[a-z0-9][a-z0-9-]*$/;
const TRIGGER_KINDS: ReadonlySet<string> = new Set(["schedule", "webhook", "poll", "bot"]);
/// The controller's "picking it up" pause before a delivery becomes a run.
const DELIVERY_DELAY_MS = 800;
const ROTATE_DELAY_MS = 600;
const MAX_RECENT_DELIVERIES = 20;
const MAX_PROMPT_PAYLOAD_CHARS = 2_000;

type TriggerKind = "schedule" | "webhook" | "poll" | "bot";
type WhenBusy = "queue" | "steer" | "append";
type PerKeyRoute = Extract<BotTriggerRoute, { policy: "perKey" }>;

/// Typed refusal so one handler maps config and admission failures to the
/// status the server would answer with.
export class BotConfigError extends Error {
  constructor(
    message: string,
    readonly status: 400 | 404 | 409 | 410,
    readonly modelDefaultSlot?: "agentRun",
  ) {
    super(message);
    this.name = "BotConfigError";
  }
}

export function botErrorResponse(c: Context, error: unknown): Response {
  if (error instanceof BotConfigError) return c.json({ error: error.message, ...(error.modelDefaultSlot ? { kind: "modelDefaultUnset", modelDefaultSlot: error.modelDefaultSlot } : {}) }, error.status);
  throw error;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function optionalNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function randomHex(bytes: number): string {
  const buffer = crypto.getRandomValues(new Uint8Array(bytes));
  return [...buffer].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function eventSeqOf(record: BotRecord): number {
  let max = 0;
  for (const event of record.events) if (event.seq > max) max = event.seq;
  return max;
}

/// The bot document as `bots/read` returns it; `eventSeq` is derived from
/// the log so it never drifts.
export function botViewOf(record: BotRecord): BotView {
  return { ...record.bot, eventSeq: eventSeqOf(record) };
}

/// The bot's sessions and their sub-agents as its state read lists them,
/// each with what it is doing now.
export function botSessionsOf(universe: UniverseState, record: BotRecord): SessionSummaryView[] {
  const own = (record.state?.sessions ?? [])
    .map((session) => universe.sessions.get(session.sessionId))
    .filter((session): session is SessionRecord => session !== undefined)
    .map(sessionSummaryOf);
  const descendants = record.descendants.map((child) => {
    const live = universe.sessions.get(child.id);
    return live ? sessionSummaryOf(live) : child;
  });
  return [...own, ...descendants];
}

export function listItemOf(universe: UniverseState, record: BotRecord): BotListItem {
  // Its sessions and their sub-agents: how many are open, and how many busy.
  const sessions = botSessionsOf(universe, record);
  return {
    ...botViewOf(record),
    openSessionCount: sessions.filter((session) => session.lifecycleStatus !== "closed").length,
    activeSessionCount: sessions.filter((session) => session.activity !== "idle").length,
    access: demoAccess(),
    triggerCount: record.triggers.size,
    pendingCount: record.events.filter((event) => event.outcome === null || event.outcome === undefined)
      .length,
    lastEvent: record.events.at(-1) ?? null,
    activity: foldActivity(sessions.map((session) => session.activity)),
  };
}

// ---------------------------------------------------------------------------
// Controlled sessions
// ---------------------------------------------------------------------------

function profileInstructions(profile: ProfileDocument | undefined): string | null {
  const instructions = asRecord(profile?.instructions);
  const text = instructions?.text;
  return instructions?.type === "text" && typeof text === "string" ? text : null;
}

/// A session the bot's controller owns: controlled, lifecycle-bound to the
/// controller, configured from the bot's profile.
function botSessionConfig(universe: UniverseState, bot: BotView): Record<string, unknown> {
  const config = asRecord(universe.profiles.get(bot.profileId)?.config) ?? {};
  const model = modelOf(config) ?? universe.modelDefaults.agentRun;
  if (!model) throw new BotConfigError("No default agent model is selected. Choose a model in Models.", 400, "agentRun");
  return { ...structuredClone(config), model: { ...model } };
}

function botSession(store: DemoStore, universe: UniverseState, bot: BotView, label: string, config = botSessionConfig(universe, bot)): SessionRecord {
  const profile = universe.profiles.get(bot.profileId);
  return newSession(store, universe, {
    displayName: `${bot.botId} · ${label}`,
    lifecycleControlled: true,
    integration: {
      version: 1,
      lifecycleController: { workflowId: `bot:v1:${bot.botId}`, workflowKind: "bot_controller_v1" },
      tools: [],
    },
    config,
    instructions: profileInstructions(profile),
  });
}

function initialState(
  bot: BotView,
  profile: ProfileDocument | undefined,
  mainSessionId: string,
): BotControllerSnapshot {
  return {
    mainSessionId,
    sessions: [
      { sessionId: mainSessionId, label: "main", kind: "main", busy: false, generation: 1 },
    ],
    controllerStatus: "idle",
    setupStatus: "ready",
    enabled: bot.enabled ?? true,
    closed: false,
    activeDeliveries: [],
    buffers: [],
    pendingDeliveries: 0,
    recentDeliveries: [],
    eventsProcessed: 0,
    duplicateEvents: 0,
    appliedProfileRevision: profile?.revision ?? null,
    runDay: new Date().toISOString().slice(0, 10),
    runsToday: 0,
    descendantsToday: 0,
    lastError: null,
  };
}

interface Target {
  session: SessionRecord;
  controlled: BotSessionSnapshot;
}

/// Routing key → session id per bot; a keyed session survives rotation
/// under the same key.
const keyedSessions = new WeakMap<BotRecord, Map<string, string>>();

function keyedMap(record: BotRecord): Map<string, string> {
  let map = keyedSessions.get(record);
  if (!map) {
    map = new Map();
    keyedSessions.set(record, map);
  }
  return map;
}

function sessionsOf(state: BotControllerSnapshot): BotSessionSnapshot[] {
  return (state.sessions ??= []);
}

function openSession(universe: UniverseState, sessionId: string | null | undefined): SessionRecord | null {
  const session = sessionId ? universe.sessions.get(sessionId) : undefined;
  return session && session.view.lifecycleStatus !== "closed" ? session : null;
}

function addControlled(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  label: string,
  kind: BotSessionSnapshot["kind"],
): Target {
  const session = botSession(store, universe, record.bot, label);
  const controlled: BotSessionSnapshot = {
    sessionId: session.view.id,
    label,
    kind,
    busy: false,
    generation: 1,
  };
  sessionsOf(record.state).push(controlled);
  return { session, controlled };
}

/// The main session, re-created when it is gone (a closed fixture session).
function mainSession(store: DemoStore, universe: UniverseState, record: BotRecord): Target {
  const state = record.state;
  const existing = openSession(universe, state.mainSessionId);
  const controlled = sessionsOf(state).find((entry) => entry.kind === "main");
  if (existing && controlled) return { session: existing, controlled };
  const session = botSession(store, universe, record.bot, "main");
  const entry: BotSessionSnapshot = {
    sessionId: session.view.id,
    label: "main",
    kind: "main",
    busy: false,
    generation: (controlled?.generation ?? 0) + 1,
  };
  state.mainSessionId = session.view.id;
  state.sessions = [entry, ...sessionsOf(state).filter((item) => item.kind !== "main")];
  return { session, controlled: entry };
}

function routeKey(
  trigger: BotTriggerView | null,
  route: PerKeyRoute,
  payload: unknown,
): { key: string; label: string } {
  const body = asRecord(payload);
  const keyed = (key: string) => ({ key, label: key });
  if (route.key) {
    const value = body?.[route.key];
    if ((typeof value === "string" && value) || typeof value === "number") return keyed(String(value).slice(0, 200));
  }
  if (trigger?.kind === "webhook" && trigger.preset === "github") {
    const pullRequest = asRecord(body?.pull_request)?.number;
    if (typeof pullRequest === "number") return keyed(`pr-${pullRequest}`);
    const issue = asRecord(body?.issue)?.number;
    if (typeof issue === "number") return keyed(`issue-${issue}`);
    const repository = asRecord(body?.repository)?.full_name;
    if (typeof repository === "string") return keyed(repository);
  }
  return keyed(trigger?.triggerId ?? "default");
}

/// Where an event lands: the main session, one session per key, or a fresh
/// session per event — decided at admission, where the payload is at hand.
function resolveTarget(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  input: {
    trigger: BotTriggerView | null;
    payload: unknown;
    eventId: string;
    session: { sessionId: string; label: string } | null;
  },
): Target {
  const state = record.state;
  const reused = openSession(universe, input.session?.sessionId);
  if (reused && input.session) {
    let controlled = sessionsOf(state).find((entry) => entry.sessionId === reused.view.id);
    if (!controlled) {
      controlled = {
        sessionId: reused.view.id,
        label: input.session.label,
        kind: "perKey",
        busy: false,
        generation: 1,
      };
      sessionsOf(state).push(controlled);
    }
    return { session: reused, controlled };
  }
  const trigger = input.trigger;
  let route: BotTriggerRoute | null = trigger?.route ?? null;
  if (route === null || route.policy === "bot") return mainSession(store, universe, record);
  if (route.policy === "perEvent") {
    return addControlled(store, universe, record, `event ${input.eventId.slice(0, 24)}`, "perEvent");
  }
  const { key, label } = routeKey(trigger, route, input.payload);
  const map = keyedMap(record);
  const existing = openSession(universe, map.get(key));
  const controlled = existing
    ? sessionsOf(state).find((entry) => entry.sessionId === existing.view.id)
    : undefined;
  if (existing && controlled) return { session: existing, controlled };
  const target = addControlled(store, universe, record, label, "perKey");
  map.set(key, target.session.view.id);
  return target;
}

// ---------------------------------------------------------------------------
// Admission and delivery
// ---------------------------------------------------------------------------

export interface DemoBotEventInput {
  kind: string;
  /// Document source string; the view carries only the trigger id.
  source: string;
  /// One line about the event; derived from kind and source when absent.
  summary?: string;
  payload?: unknown;
  /// Rendered in place of `payload` (a preset's projection); the stored
  /// document keeps the full payload.
  promptData?: unknown;
  /// Caller-supplied id, deduped per bot; a fresh uuid when absent.
  eventId?: string;
  occurredAtMs?: number;
  headers?: Record<string, string>;
  trigger?: BotTriggerView | null;
  /// Routed target of an earlier admission (replays reuse it).
  session?: { sessionId: string; label: string } | null;
  senderBotId?: string | null;
  hops?: number;
  inReplyTo?: { bot: string; seq: number } | null;
  /// An already-stored document ref (replays).
  documentRef?: string;
  correlationId?: string | null;
  links?: string[];
}

function admissionRefusal(record: BotRecord, trigger: BotTriggerView | null): BotConfigError | null {
  if (record.bot.closedAtMs != null) return new BotConfigError("bot is closed", 410);
  if (record.bot.enabled === false) return new BotConfigError("bot is disabled", 409);
  if (trigger && trigger.enabled === false) return new BotConfigError("trigger is disabled", 409);
  return null;
}

/// The model-facing text of one event: a header the model can quote back
/// by #N, the summary, and the payload (cut when large).
function renderEvent(
  event: Pick<BotEventView, "seq" | "kind" | "occurredAtMs" | "inReplyTo">,
  source: string,
  summary: string,
  payload: unknown,
): string {
  const handle = `event #${event.seq}`;
  const time = `${new Date(event.occurredAtMs).toISOString().slice(0, 16).replace("T", " ")} UTC`;
  const parts = [`── ${handle} · ${event.kind} · ${source} · ${time}`, summary];
  if (payload !== undefined && payload !== null) {
    const json = JSON.stringify(payload, null, 2);
    parts.push(
      json.length > MAX_PROMPT_PAYLOAD_CHARS
        ? `${json.slice(0, MAX_PROMPT_PAYLOAD_CHARS)}\n(… truncated — full payload: bot_event_read #${event.seq})`
        : json,
    );
  }
  if (event.inReplyTo) parts.push(`reply to your #${event.inReplyTo.seq} at ${event.inReplyTo.bot}`);
  return parts.join("\n");
}

/// Store, then deliver: numbers the event, routes it, and hands it to the
/// controller simulation. Throws a `BotConfigError` when the bot or trigger
/// refuses (closed, disabled); routes check first and answer 410/409.
export function admitBotEvent(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  input: DemoBotEventInput,
): { event: BotEventView; document: BotEventDocument; duplicate: boolean } {
  const trigger = input.trigger ?? null;
  const refusal = admissionRefusal(record, trigger);
  if (refusal) throw refusal;
  const eventId = input.eventId ?? crypto.randomUUID();
  const occurredAtMs = input.occurredAtMs ?? Date.now();
  const summary = input.summary ?? `${input.kind} from ${input.source}`;
  const document: BotEventDocument = {
    version: 1,
    kind: input.kind,
    source: input.source,
    occurredAtMs,
    summary,
    ...(input.payload === undefined ? {} : { data: input.payload }),
    ...(input.headers === undefined ? {} : { headers: input.headers }),
    ...(input.correlationId ? { correlationId: input.correlationId } : {}),
    ...(input.links === undefined ? {} : { links: input.links }),
    ...(input.senderBotId ? { sender: { bot: input.senderBotId } } : {}),
    ...(input.hops ? { hops: input.hops } : {}),
    ...(input.inReplyTo ? { inReplyTo: input.inReplyTo } : {}),
  };
  const existing = record.events.find((event) => event.eventId === eventId);
  if (existing) {
    record.state.duplicateEvents = (record.state.duplicateEvents ?? 0) + 1;
    return { event: existing, document, duplicate: true };
  }
  const target = resolveTarget(store, universe, record, {
    trigger,
    payload: input.payload,
    eventId,
    session: input.session ?? null,
  });
  const seq = eventSeqOf(record) + 1;
  record.bot.eventSeq = seq;
  const event: BotEventView = {
    seq,
    eventId,
    ...(trigger ? { triggerId: trigger.triggerId } : {}),
    kind: input.kind,
    summary,
    occurredAtMs,
    receivedAtMs: Date.now(),
    documentRef: input.documentRef ?? store.putText(JSON.stringify(document, null, 2)),
    session:
      target.controlled.kind === "main"
        ? null
        : { sessionId: target.session.view.id, label: target.controlled.label },
    senderBotId: input.senderBotId ?? null,
    hops: input.hops ?? 0,
    inReplyTo: input.inReplyTo ?? null,
    outcome: null,
    outcomeDetail: null,
    runId: null,
    resolvedAtMs: null,
  };
  const prompt = renderEvent(event, input.source, summary, input.promptData ?? input.payload);
  event.promptRef = store.putText(prompt);
  record.events.push(event);
  const whenBusy = (trigger?.deliver?.whenBusy ?? "queue") as WhenBusy;
  deliver(store, universe, record, event, target, whenBusy, prompt);
  return { event, document, duplicate: false };
}

function removeDelivery(state: BotControllerSnapshot, deliveryId: string): void {
  state.activeDeliveries = (state.activeDeliveries ?? []).filter(
    (delivery) => delivery.deliveryId !== deliveryId,
  );
}

/// Write-once outcome plus the controller's bookkeeping.
function resolveEvent(
  record: BotRecord,
  event: BotEventView,
  outcome: BotEventOutcome,
  detail: string | null,
  runId: string | null,
  delivery?: { deliveryId: string; sessionId: string },
  usage?: LlmUsageView,
): void {
  if (event.outcome !== null && event.outcome !== undefined) return;
  const state = record.state;
  event.outcome = outcome;
  event.outcomeDetail = detail;
  event.runId = runId;
  event.resolvedAtMs = Date.now();
  state.eventsProcessed = (state.eventsProcessed ?? 0) + 1;
  if (runId !== null) state.runsToday = (state.runsToday ?? 0) + 1;
  const recent = {
    deliveryId: delivery?.deliveryId ?? `dlv-${record.bot.botId}-${event.seq}`,
    seqs: [event.seq],
    sessionId: delivery?.sessionId ?? event.session?.sessionId ?? state.mainSessionId,
    finishedAtMs: Date.now(),
    outcome,
    ...(runId === null ? {} : { runId }),
    ...(detail === null ? {} : { summary: detail }),
    ...(usage === undefined ? {} : { usage }),
  };
  state.recentDeliveries = [recent, ...(state.recentDeliveries ?? [])].slice(0, MAX_RECENT_DELIVERIES);
  if (
    (state.activeDeliveries ?? []).length === 0 &&
    state.controllerStatus !== "closed" &&
    state.controllerStatus !== "closing"
  ) {
    state.controllerStatus = "idle";
  }
}

/// Settles once the run is terminal — including cancellation, which the
/// engine never reports through `onFinished`.
function watchRun(session: SessionRecord, run: RunView, settle: (run: RunView) => void): void {
  const check = () => {
    if (run.status !== "completed" && run.status !== "cancelled" && run.status !== "failed") return;
    session.waiters.delete(check);
    settle(run);
  };
  session.waiters.add(check);
  check();
}

function lastAssistantLine(session: SessionRecord): string | null {
  const entries = session.activeContext.entries;
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index];
    if (!entry || entry.kind.type !== "message") continue;
    if (!("role" in entry.kind) || entry.kind.role !== "assistant") continue;
    if (!("text" in entry) || typeof entry.text !== "string") continue;
    const line = entry.text.split("\n").find((candidate) => candidate.trim()) ?? "";
    return line.trim().slice(0, 200) || null;
  }
  return null;
}

/// Plausible prompt accounting: the prefix grows per turn and is served
/// from cache once the session has one.
function usageFor(session: SessionRecord, prompt: string): LlmUsageView {
  const inputTokens = 1_400 + Math.round(prompt.length / 4) + Math.max(0, session.turns - 1) * 850;
  const cachedInputTokens = session.turns > 1 ? Math.round(inputTokens * 0.82) : 0;
  return { inputTokens, cachedInputTokens };
}

/// The controller's delivery policy for one event: steer or append into a
/// busy session when the trigger says so, otherwise a run (queued behind an
/// active one by the engine).
function deliver(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  event: BotEventView,
  target: Target,
  whenBusy: WhenBusy,
  prompt: string,
): void {
  const state = record.state;
  const session = target.session;
  const deliveryId = store.nextId("delivery");
  target.controlled.lastActiveAtMs = Date.now();
  const active = activeRun(session);
  if (active && whenBusy === "steer") {
    const steered = steerRun(store, session, active.id, prompt, EVENT_ORIGIN);
    if (steered) {
      resolveEvent(record, event, "steered", `steered run ${active.id}`, null, {
        deliveryId,
        sessionId: session.view.id,
      });
      return;
    }
  }
  if (active && whenBusy === "append") {
    applyEntries(
      session,
      [
        contextMessage(store.nextId("entry"), "user", prompt, {
          type: "runInput",
          inputIndex: 0,
          runId: active.id,
        }, EVENT_ORIGIN),
      ],
      { runId: active.id },
    );
    resolveEvent(record, event, "appended", `appended to run ${active.id}`, null, {
      deliveryId,
      sessionId: session.view.id,
    });
    return;
  }
  const delivery: BotActiveDeliverySnapshot = {
    deliveryId,
    seqs: [event.seq],
    sessionId: session.view.id,
    runId: null,
    startedAtMs: Date.now(),
  };
  state.activeDeliveries = [...(state.activeDeliveries ?? []), delivery];
  state.pendingDeliveries = (state.pendingDeliveries ?? 0) + 1;
  state.controllerStatus = "deliveringEvent";
  setTimeout(() => {
    state.pendingDeliveries = Math.max(0, (state.pendingDeliveries ?? 0) - 1);
    // Archived while waiting (bot closed, session rotated).
    if (event.outcome !== null && event.outcome !== undefined) return;
    if (session.view.lifecycleStatus === "closed") {
      removeDelivery(state, deliveryId);
      resolveEvent(record, event, "archived", "session closed before delivery", null, {
        deliveryId,
        sessionId: session.view.id,
      });
      return;
    }
    const run = startRun(store, universe, session, { text: prompt, origin: EVENT_ORIGIN });
    delivery.runId = run.id;
    state.controllerStatus = "deliveringEvent";
    watchRun(session, run, (finished) => {
      removeDelivery(state, deliveryId);
      if (finished.status === "completed") {
        resolveEvent(
          record,
          event,
          "handled",
          lastAssistantLine(session) ?? "handled",
          finished.id,
          { deliveryId, sessionId: session.view.id },
          usageFor(session, prompt),
        );
      } else {
        resolveEvent(record, event, "runFailed", `run ${finished.status}`, finished.id, {
          deliveryId,
          sessionId: session.view.id,
        });
      }
    });
  }, DELIVERY_DELAY_MS);
}

function abandonDeliveries(record: BotRecord, sessionId: string, detail: string): void {
  const state = record.state;
  for (const delivery of (state.activeDeliveries ?? []).filter((entry) => entry.sessionId === sessionId)) {
    for (const seq of delivery.seqs) {
      const event = record.events.find((entry) => entry.seq === seq);
      if (event) {
        resolveEvent(record, event, "archived", detail, null, {
          deliveryId: delivery.deliveryId,
          sessionId,
        });
      }
    }
    removeDelivery(state, delivery.deliveryId);
  }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Terminal close: the row first (admission refuses from here on), every
/// trigger paused as `botClosed`, pending events archived, every session
/// force-closed and recorded.
export function closeBot(universe: UniverseState, record: BotRecord): void {
  const { bot, state } = record;
  const at = Date.now();
  if (bot.closedAtMs == null) {
    bot.closedAtMs = at;
    bot.enabled = false;
    bot.updatedAtMs = at;
  }
  for (const trigger of record.triggers.values()) {
    if (trigger.enabled === false) continue;
    trigger.enabled = false;
    trigger.disabledReason = "botClosed";
    trigger.disabledAtMs = at;
    trigger.updatedAtMs = at;
  }
  state.controllerStatus = "closing";
  for (const event of record.events) {
    if (event.outcome === null || event.outcome === undefined) {
      resolveEvent(record, event, "archived", "bot closed", null);
    }
  }
  state.activeDeliveries = [];
  state.pendingDeliveries = 0;
  state.buffers = [];
  const closed: string[] = [];
  for (const controlled of sessionsOf(state)) {
    const session = universe.sessions.get(controlled.sessionId);
    if (!session) continue;
    closeSession(session, true);
    closed.push(session.view.id);
  }
  bot.closedSessions = closed;
  state.enabled = false;
  state.closed = true;
  state.controllerStatus = "closed";
}

/// Operator reset of one controlled session: the old one closes, a fresh one
/// takes its place under the same label (and routing key), a generation up.
function rotateSession(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  controlled: BotSessionSnapshot,
  config: Record<string, unknown>,
): void {
  const state = record.state;
  const old = universe.sessions.get(controlled.sessionId);
  if (old) {
    abandonDeliveries(record, old.view.id, "session rotated");
    closeSession(old, true);
  }
  const fresh = botSession(store, universe, record.bot, controlled.label, config);
  const entry: BotSessionSnapshot = {
    sessionId: fresh.view.id,
    label: controlled.label,
    kind: controlled.kind,
    busy: false,
    generation: controlled.generation + 1,
  };
  const sessions = sessionsOf(state);
  const index = sessions.indexOf(controlled);
  if (index >= 0) sessions[index] = entry;
  else sessions.push(entry);
  if (state.mainSessionId === controlled.sessionId) state.mainSessionId = fresh.view.id;
  const map = keyedMap(record);
  for (const [key, sessionId] of map) if (sessionId === controlled.sessionId) map.set(key, fresh.view.id);
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

function isTriggerKind(value: unknown): value is TriggerKind {
  return typeof value === "string" && TRIGGER_KINDS.has(value);
}

function normalizeRoute(route: unknown): BotTriggerRoute | null {
  if (route === null || route === undefined) return null;
  const policy = asRecord(route)?.policy;
  const key = asRecord(route)?.key;
  if (policy === "perKey") return { policy: "perKey", key: typeof key === "string" && key ? key : null };
  if (policy === "perEvent") return { policy: "perEvent" };
  if (policy === "bot") return { policy: "bot" };
  throw new BotConfigError("validation failed", 400);
}

function normalizeCoalesce(value: unknown): { debounceMs: number; maxWaitMs: number; maxCount: number } | null {
  const input = asRecord(value);
  if (!input) return null;
  const { debounceMs, maxWaitMs, maxCount } = input;
  if (typeof debounceMs !== "number" || typeof maxWaitMs !== "number" || typeof maxCount !== "number") {
    throw new BotConfigError("validation failed", 400);
  }
  return { debounceMs, maxWaitMs, maxCount };
}

function normalizeDeliver(value: unknown): { whenBusy: WhenBusy } | null {
  if (value === null || value === undefined) return null;
  const whenBusy = asRecord(value)?.whenBusy;
  if (whenBusy === "queue" || whenBusy === "steer" || whenBusy === "append") return { whenBusy };
  throw new BotConfigError("validation failed", 400);
}

function normalizeSessionCloseAfter(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new BotConfigError("validation failed", 400);
  }
  return value;
}

export function webhookTokenOf(trigger: StoredBotTrigger | undefined): string | null {
  const path = trigger?.ingestPath;
  if (!path) return null;
  return path.slice(path.lastIndexOf("/") + 1) || null;
}

/// The trigger view omits the webhook ingest path.
export function triggerViewOf(stored: StoredBotTrigger): BotTriggerView {
  const { ingestPath: _ingestPath, ...view } = stored;
  return view as BotTriggerView;
}

/// What only bot managers read, and what creating or replacing a trigger
/// answers with beside the view (`bots/triggers/secrets/read`).
export function triggerSecretsOf(stored: StoredBotTrigger): { ingestPath?: string } {
  return {
    ...(stored.ingestPath ? { ingestPath: stored.ingestPath } : {}),
  };
}

/// Reads one `BotTriggerInput` (the flattened core shape) into a stored
/// trigger view. Webhook URL tokens survive edits; a poll spec edit
/// re-baselines the cursor.
function normalizeTrigger(
  universe: UniverseState,
  record: BotRecord,
  triggerId: string,
  body: Record<string, unknown>,
  existing: StoredBotTrigger | undefined,
): StoredBotTrigger {
  if (!NAME_PATTERN.test(triggerId) || triggerId.length > 64) {
    throw new BotConfigError("validation failed: trigger ids are lowercase alphanumerics and dashes", 400);
  }
  const kind = body.kind;
  if (!isTriggerKind(kind)) throw new BotConfigError("validation failed", 400);
  if (kind === "bot") {
    // One inbox per bot: `to: "b"` must mean exactly one event in B.
    const inbox = [...record.triggers.values()].find(
      (trigger) => trigger.kind === "bot" && trigger.triggerId !== triggerId,
    );
    if (inbox) {
      throw new BotConfigError(
        `this bot already has an inbox (trigger ${inbox.triggerId}); a bot has at most one trigger of kind bot`,
        409,
      );
    }
  }
  const routed = kind !== "schedule";
  const enabled = body.enabled !== false;
  const base = {
    botId: record.bot.botId,
    triggerId,
    revision: (existing?.revision ?? 0) + 1,
    filter: routed ? optionalString(body.filter) : null,
    route: routed ? normalizeRoute(body.route) : null,
    coalesce: routed ? normalizeCoalesce(body.coalesce) : null,
    deliver: routed ? normalizeDeliver(body.deliver) : null,
    sessionCloseAfterMs: routed ? normalizeSessionCloseAfter(body.sessionCloseAfterMs) : null,
    enabled,
    disabledReason: enabled ? null : ("operator" as const),
    disabledAtMs: enabled ? null : Date.now(),
    lastFilterError: null,
    lastFilterErrorAtMs: null,
    createdAtMs: existing?.createdAtMs ?? Date.now(),
    updatedAtMs: Date.now(),
  };
  switch (kind) {
    case "schedule": {
      const cron = optionalString(body.cron);
      const atMs = optionalNumber(body.atMs);
      const summary = typeof body.summary === "string" ? body.summary : "";
      if (!cron && atMs === null) {
        throw new BotConfigError("a schedule needs a cron expression or a one-shot time", 400);
      }
      return {
        ...base,
        kind: "schedule",
        cron,
        atMs,
        timezone: optionalString(body.timezone) ?? "UTC",
        summary,
      } as BotTriggerView;
    }
    case "webhook": {
      const verification = asRecord(body.verification);
      const hmac =
        verification?.scheme === "hmac-sha256" &&
        typeof verification.grantId === "string" &&
        typeof verification.header === "string";
      // The URL token survives spec edits; rotation means a new trigger.
      const token = webhookTokenOf(existing) ?? randomHex(24);
      return {
        ...base,
        kind: "webhook",
        verification: hmac
          ? (verification as unknown as Extract<BotTriggerView, { kind: "webhook" }>["verification"])
          : { scheme: "token" },
        preset: body.preset === "github" ? "github" : null,
        ingestPath: `/ingress/webhooks/bots/${universe.universe.lightspeedUniverseId}/${record.bot.botId}/${triggerId}/${token}`,
      } as StoredBotTrigger;
    }
    case "poll": {
      const source = asRecord(body.source);
      const sourceKind = source?.kind;
      const intervalMs = body.intervalMs;
      if ((sourceKind !== "http" && sourceKind !== "exec") || typeof intervalMs !== "number") {
        throw new BotConfigError("validation failed", 400);
      }
      const cursor = asRecord(body.cursor);
      const cursorKind = cursor?.kind;
      return {
        ...base,
        kind: "poll",
        source,
        intervalMs,
        items: optionalString(body.items),
        cursor: cursorKind === "idSet" || cursorKind === "watermark" ? cursor : { kind: "idSet", id: "id" },
        // A document PUT re-baselines the poll: the advancing state resets.
        cursorState: null,
      } as unknown as BotTriggerView;
    }
    case "bot": {
      const from = Array.isArray(body.from)
        ? [...new Set(body.from.filter((value): value is string => typeof value === "string"))]
        : null;
      return { ...base, kind: "bot", from: from && from.length > 0 ? from : null } as BotTriggerView;
    }
  }
}

// ---------------------------------------------------------------------------
// Bot creation (shared by POST and an upserting PUT)
// ---------------------------------------------------------------------------

function botFromInput(body: Record<string, unknown>, existing: BotView | undefined): BotView | BotConfigError {
  const botId = optionalString(body.botId) ?? existing?.botId ?? "";
  if (!NAME_PATTERN.test(botId) || botId.length > 64) {
    return new BotConfigError("validation failed: bot ids are lowercase alphanumerics and dashes", 400);
  }
  const profileId = optionalString(body.profileId);
  if (!profileId) return new BotConfigError("validation failed: profileId is required", 400);
  const breaker = asRecord(body.breaker);
  const fires = breaker?.fires;
  const windowMs = breaker?.windowMs;
  const now = Date.now();
  return {
    botId,
    displayName: optionalString(body.displayName),
    description: optionalString(body.description),
    profileId,
    brief: optionalString(body.brief),
    runsPerDay: optionalNumber(body.runsPerDay),
    breaker: typeof fires === "number" && typeof windowMs === "number" ? { fires, windowMs } : null,
    routedSessionCloseAfterMs: optionalNumber(body.routedSessionCloseAfterMs),
    selfConfig: body.selfConfig === true,
    emit: body.emit === true,
    enabled: body.enabled !== false,
    eventSeq: existing?.eventSeq ?? 0,
    revision: (existing?.revision ?? 0) + 1,
    createdAtMs: existing?.createdAtMs ?? now,
    updatedAtMs: now,
  };
}

export function createBot(
  store: DemoStore,
  universe: UniverseState,
  body: Record<string, unknown>,
  triggerInputs: unknown,
): { record: BotRecord } | BotConfigError {
  const bot = botFromInput(body, undefined);
  if (bot instanceof BotConfigError) return bot;
  if (!universe.profiles.has(bot.profileId)) {
    return new BotConfigError(`profile ${bot.profileId} does not exist`, 400);
  }
  if (universe.bots.has(bot.botId)) {
    return new BotConfigError("a bot with that id already exists", 409);
  }
  const main = botSession(store, universe, bot, "main");
  const record: BotRecord = {
    bot,
    triggers: new Map(),
    events: [],
    state: initialState(bot, universe.profiles.get(bot.profileId), main.view.id),
    descendants: [],
  };
  universe.bots.set(bot.botId, record);
  // One refused trigger fails the whole create and leaves no half-made bot.
  try {
    const triggers = Array.isArray(triggerInputs) ? triggerInputs.filter(isRecord) : [];
    for (const input of triggers) {
      const triggerId = optionalString(input.triggerId);
      if (!triggerId) throw new BotConfigError("validation failed: triggerId is required", 400);
      if (record.triggers.has(triggerId)) {
        throw new BotConfigError("a trigger with that id already exists", 409);
      }
      record.triggers.set(triggerId, normalizeTrigger(universe, record, triggerId, input, undefined));
    }
  } catch (error) {
    universe.bots.delete(bot.botId);
    universe.sessions.delete(main.view.id);
    if (error instanceof BotConfigError) return error;
    throw error;
  }
  return { record };
}

export function putBot(store: DemoStore, universe: UniverseState, botId: string, input: Record<string, unknown>, expectedRevision: unknown): BotView {
  const record = universe.bots.get(botId);
  if (!record) {
    const created = createBot(store, universe, { ...input, botId }, undefined);
    if (created instanceof BotConfigError) throw created;
    return botViewOf(created.record);
  }
  if (record.bot.closedAtMs != null) throw new BotConfigError("bot is closed", 409);
  if (typeof expectedRevision === "number" && expectedRevision !== record.bot.revision) {
    throw new BotConfigError("bot revision conflict", 409);
  }
  const bot = botFromInput({ ...input, botId }, record.bot);
  if (bot instanceof BotConfigError) throw bot;
  const profile = universe.profiles.get(bot.profileId);
  if (!profile) throw new BotConfigError(`profile ${bot.profileId} does not exist`, 400);
  record.bot = bot;
  record.state.enabled = bot.enabled ?? true;
  record.state.appliedProfileRevision = profile.revision ?? null;
  return botViewOf(record);
}

export function putTrigger(universe: UniverseState, record: BotRecord, triggerId: string, input: Record<string, unknown>, expectedRevision: unknown): StoredBotTrigger {
  const existing = record.triggers.get(triggerId);
  if (existing && typeof expectedRevision === "number" && expectedRevision !== existing.revision) {
    throw new BotConfigError("trigger revision conflict", 409);
  }
  const trigger = normalizeTrigger(universe, record, triggerId, input, existing);
  record.triggers.set(triggerId, trigger);
  return trigger;
}

export function requestBotSessionRotation(store: DemoStore, universe: UniverseState, record: BotRecord, sessionId: string): void {
  const controlled = sessionsOf(record.state).find(entry => entry.sessionId === sessionId);
  if (!controlled) throw new BotConfigError("session is not controlled by this bot", 404);
  if (record.bot.closedAtMs != null) throw new BotConfigError("bot is closed", 409);
  const config = botSessionConfig(universe, record.bot);
  setTimeout(() => rotateSession(store, universe, record, controlled, config), ROTATE_DELAY_MS);
}
