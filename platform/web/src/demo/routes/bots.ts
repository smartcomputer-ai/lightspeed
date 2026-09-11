/// Bots: durable event routers over managed sessions, with a small in-page
/// controller standing in for the Temporal one. The routes mirror the
/// platform server's passthroughs to the core `bots/*` API: the
/// same paths, envelopes, and status codes, over the core wire shapes.
/// Every event — manual, webhook, replay, fixture — goes through
/// `admitBotEvent`: numbered, routed by its trigger's policy to one of the
/// bot's sessions, and delivered as a run whose end writes the event's
/// outcome.
import { Hono } from "hono";
import type { Context } from "hono";
import type {
  BotActiveDeliverySnapshot,
  BotControllerSnapshot,
  BotEventDocument,
  BotEventOutcome,
  BotEventView,
  BotFilterTestResult,
  BotListItem,
  BotSessionSnapshot,
  BotTriggerRoute,
  BotTriggerView,
  BotView,
  LlmUsageView,
  RunView,
  WebhookPreset,
} from "@lightspeed-ai/agent-client";
import type { ProfileDocument } from "@/api";
import {
  DEFAULT_MODEL,
  EVENT_ORIGIN,
  activeRun,
  applyEntries,
  closeSession,
  contextMessage,
  newSession,
  startRun,
  steerRun,
} from "../engine";
import type { BotRecord, DemoStore, SessionRecord, UniverseState } from "../store";
import { badRequest, conflict, intQuery, notFound, readBody, universeFor } from "./common";

const NAME_PATTERN = /^[a-z0-9][a-z0-9-]*$/;
const TRIGGER_KINDS: ReadonlySet<string> = new Set(["schedule", "webhook", "poll", "bot", "chat"]);
const PAIRING_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
/// The controller's "picking it up" pause before a delivery becomes a run.
const DELIVERY_DELAY_MS = 800;
const ROTATE_DELAY_MS = 600;
const MAX_RECENT_DELIVERIES = 20;
const MAX_HISTORY_LIMIT = 100;
const MAX_BODY_BYTES = 1024 * 1024;
const MAX_PROMPT_PAYLOAD_CHARS = 2_000;
const COLLATOR = new Intl.Collator("en", { sensitivity: "base", numeric: true });

type TriggerKind = "schedule" | "webhook" | "poll" | "bot" | "chat";
type WhenBusy = "queue" | "steer" | "append";
type PerKeyRoute = Extract<BotTriggerRoute, { policy: "perKey" }>;

/// Typed refusal so one handler maps config and admission failures to the
/// status the server would answer with.
class BotConfigError extends Error {
  constructor(
    message: string,
    readonly status: 400 | 404 | 409 | 410,
  ) {
    super(message);
    this.name = "BotConfigError";
  }
}

function configErrorResponse(c: Context, error: unknown): Response {
  if (error instanceof BotConfigError) return c.json({ error: error.message }, error.status);
  throw error;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return isRecord(value) ? value : undefined;
}

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

function mintPairingCode(length = 12): string {
  const bytes = crypto.getRandomValues(new Uint8Array(length));
  let code = "";
  for (const byte of bytes) code += PAIRING_ALPHABET[byte % PAIRING_ALPHABET.length];
  return code;
}

interface Found {
  universe: UniverseState;
  record: BotRecord;
}

function botFor(store: DemoStore, c: Context): Found | null {
  const universe = universeFor(store, c);
  const record = universe?.bots.get(c.req.param("botId") ?? "");
  return universe && record ? { universe, record } : null;
}

function eventSeqOf(record: BotRecord): number {
  let max = 0;
  for (const event of record.events) if (event.seq > max) max = event.seq;
  return max;
}

/// The bot document as `bots/read` returns it; `eventSeq` is derived from
/// the log so it never drifts.
function botViewOf(record: BotRecord): BotView {
  return { ...record.bot, eventSeq: eventSeqOf(record) };
}

function listItemOf(record: BotRecord): BotListItem {
  return {
    ...botViewOf(record),
    triggerCount: record.triggers.size,
    pendingCount: record.events.filter((event) => event.outcome === null || event.outcome === undefined)
      .length,
    lastEvent: record.events.at(-1) ?? null,
  };
}

// ---------------------------------------------------------------------------
// Managed sessions
// ---------------------------------------------------------------------------

function profileInstructions(profile: ProfileDocument | undefined): string | null {
  const instructions = asRecord(profile?.instructions);
  const text = instructions?.text;
  return instructions?.type === "text" && typeof text === "string" ? text : null;
}

/// A session the bot's controller owns: managed, lifecycle-bound to the
/// controller, configured from the bot's profile.
function botSession(store: DemoStore, universe: UniverseState, bot: BotView, label: string): SessionRecord {
  const profile = universe.profiles.get(bot.profileId);
  const config = asRecord(profile?.config);
  return newSession(store, universe, {
    displayName: `${bot.botId} · ${label}`,
    managed: true,
    management: {
      version: 1,
      lifecycleController: { workflowId: `bot:v1:${bot.botId}`, workflowKind: "bot_controller_v1" },
      tools: [],
    },
    config: { model: { ...DEFAULT_MODEL }, ...config },
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
  managed: BotSessionSnapshot;
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
  return session && session.view.status !== "closed" ? session : null;
}

function addManaged(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  label: string,
  kind: BotSessionSnapshot["kind"],
): Target {
  const session = botSession(store, universe, record.bot, label);
  const managed: BotSessionSnapshot = {
    sessionId: session.view.id,
    label,
    kind,
    busy: false,
    generation: 1,
  };
  sessionsOf(record.state).push(managed);
  return { session, managed };
}

/// The main session, re-created when it is gone (a closed fixture session).
function mainSession(store: DemoStore, universe: UniverseState, record: BotRecord): Target {
  const state = record.state;
  const existing = openSession(universe, state.mainSessionId);
  const managed = sessionsOf(state).find((entry) => entry.kind === "main");
  if (existing && managed) return { session: existing, managed };
  const session = botSession(store, universe, record.bot, "main");
  const entry: BotSessionSnapshot = {
    sessionId: session.view.id,
    label: "main",
    kind: "main",
    busy: false,
    generation: (managed?.generation ?? 0) + 1,
  };
  state.mainSessionId = session.view.id;
  state.sessions = [entry, ...sessionsOf(state).filter((item) => item.kind !== "main")];
  return { session, managed: entry };
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
  if (trigger?.kind === "chat") {
    const conversation = asRecord(body?.conversation);
    const key = conversation?.key;
    const label = conversation?.label;
    if (typeof key === "string" && key) {
      return { key, label: typeof label === "string" && label ? label.slice(0, 200) : key };
    }
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
    let managed = sessionsOf(state).find((entry) => entry.sessionId === reused.view.id);
    if (!managed) {
      managed = {
        sessionId: reused.view.id,
        label: input.session.label,
        kind: "perKey",
        busy: false,
        generation: 1,
      };
      sessionsOf(state).push(managed);
    }
    return { session: reused, managed };
  }
  const trigger = input.trigger;
  let route: BotTriggerRoute | null = trigger?.route ?? null;
  // A chat conversation always gets its own session.
  if (trigger?.kind === "chat" && (route === null || route.policy === "bot")) route = { policy: "perKey", key: null };
  if (route === null || route.policy === "bot") return mainSession(store, universe, record);
  if (route.policy === "perEvent") {
    return addManaged(store, universe, record, `event ${input.eventId.slice(0, 24)}`, "perEvent");
  }
  const { key, label } = routeKey(trigger, route, input.payload);
  const map = keyedMap(record);
  const existing = openSession(universe, map.get(key));
  const managed = existing
    ? sessionsOf(state).find((entry) => entry.sessionId === existing.view.id)
    : undefined;
  if (existing && managed) return { session: existing, managed };
  const target = addManaged(store, universe, record, label, "perKey");
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
      target.managed.kind === "main"
        ? null
        : { sessionId: target.session.view.id, label: target.managed.label },
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
  target.managed.lastActiveAtMs = Date.now();
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
  state.controllerStatus = "delivering_event";
  setTimeout(() => {
    state.pendingDeliveries = Math.max(0, (state.pendingDeliveries ?? 0) - 1);
    // Archived while waiting (bot closed, session rotated).
    if (event.outcome !== null && event.outcome !== undefined) return;
    if (session.view.status === "closed") {
      removeDelivery(state, deliveryId);
      resolveEvent(record, event, "archived", "session closed before delivery", null, {
        deliveryId,
        sessionId: session.view.id,
      });
      return;
    }
    const run = startRun(store, universe, session, { text: prompt, origin: EVENT_ORIGIN });
    delivery.runId = run.id;
    state.controllerStatus = "delivering_event";
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
        resolveEvent(record, event, "run_failed", `run ${finished.status}`, finished.id, {
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
/// trigger paused as `bot_closed`, pending events archived, every session
/// force-closed and recorded.
function closeBot(universe: UniverseState, record: BotRecord): void {
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
    trigger.disabledReason = "bot_closed";
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
  for (const managed of sessionsOf(state)) {
    const session = universe.sessions.get(managed.sessionId);
    if (!session) continue;
    closeSession(session, true);
    closed.push(session.view.id);
  }
  bot.closedSessions = closed;
  state.enabled = false;
  state.closed = true;
  state.controllerStatus = "closed";
}

/// Operator reset of one managed session: the old one closes, a fresh one
/// takes its place under the same label (and routing key), a generation up.
function rotateSession(
  store: DemoStore,
  universe: UniverseState,
  record: BotRecord,
  managed: BotSessionSnapshot,
): void {
  const state = record.state;
  const old = universe.sessions.get(managed.sessionId);
  if (old) {
    abandonDeliveries(record, old.view.id, "session rotated");
    closeSession(old, true);
  }
  const fresh = botSession(store, universe, record.bot, managed.label);
  const entry: BotSessionSnapshot = {
    sessionId: fresh.view.id,
    label: managed.label,
    kind: managed.kind,
    busy: false,
    generation: managed.generation + 1,
  };
  const sessions = sessionsOf(state);
  const index = sessions.indexOf(managed);
  if (index >= 0) sessions[index] = entry;
  else sessions.push(entry);
  if (state.mainSessionId === managed.sessionId) state.mainSessionId = fresh.view.id;
  const map = keyedMap(record);
  for (const [key, sessionId] of map) if (sessionId === managed.sessionId) map.set(key, fresh.view.id);
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

/// Chat triggers route per conversation; the main session cannot carry a
/// conversation's reply tools.
function chatRoute(route: unknown): BotTriggerRoute {
  const normalized = normalizeRoute(route);
  if (normalized === null) return { policy: "perKey", key: null };
  if (normalized.policy === "bot") {
    throw new BotConfigError(
      "chat triggers route per conversation (perKey or perEvent); the main session cannot take a chat",
      400,
    );
  }
  return normalized;
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

function webhookTokenOf(trigger: BotTriggerView | undefined): string | null {
  const path = trigger?.ingestPath;
  if (!path) return null;
  return path.slice(path.lastIndexOf("/") + 1) || null;
}

/// Reads one `BotTriggerInput` (the flattened core shape) into a stored
/// trigger view. Webhook URL tokens survive edits; a poll spec edit
/// re-baselines the cursor.
function normalizeTrigger(
  universe: UniverseState,
  record: BotRecord,
  triggerId: string,
  body: Record<string, unknown>,
  existing: BotTriggerView | undefined,
): BotTriggerView {
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
    route: kind === "chat" ? chatRoute(body.route) : routed ? normalizeRoute(body.route) : null,
    coalesce: routed ? normalizeCoalesce(body.coalesce) : null,
    deliver: routed ? normalizeDeliver(body.deliver) : null,
    sessionCloseAfterMs: routed ? (normalizeSessionCloseAfter(body.sessionCloseAfterMs) ?? (kind === "chat" ? 0 : null)) : null,
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
        ingestPath: `/hooks/bots/${universe.universe.lightspeedUniverseId}/${record.bot.botId}/${triggerId}/${token}`,
      } as BotTriggerView;
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
    case "chat": {
      const accountId = optionalString(body.accountId);
      if (!accountId) throw new BotConfigError("validation failed", 400);
      if (!universe.channelAccounts.has(accountId)) {
        throw new BotConfigError("unknown channel account", 400);
      }
      const matchScope = body.matchScope === "direct" || body.matchScope === "group" ? body.matchScope : null;
      const pairing =
        body.pairing === "open" || body.pairing === "code"
          ? body.pairing
          : existing?.kind === "chat"
            ? (existing.pairing ?? "code")
            : "code";
      const pairingCode =
        pairing === "code"
          ? (optionalString(body.pairingCode) ?? existing?.pairingCode ?? mintPairingCode())
          : null;
      return {
        ...base,
        kind: "chat",
        accountId,
        matchScope,
        ...(isRecord(body.activation) ? { activation: body.activation } : {}),
        ...(isRecord(body.access) ? { access: body.access } : {}),
        pairing,
        priority:
          typeof body.priority === "number"
            ? body.priority
            : existing?.kind === "chat"
              ? (existing.priority ?? 100)
              : 100,
        pairingCode,
      } as BotTriggerView;
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

function createBot(
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

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

export function botRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/bots", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const bots: BotListItem[] = [...universe.bots.values()]
      .sort((left, right) => {
        const byLabel = COLLATOR.compare(
          left.bot.displayName ?? left.bot.botId,
          right.bot.displayName ?? right.bot.botId,
        );
        return byLabel !== 0 ? byLabel : COLLATOR.compare(left.bot.botId, right.bot.botId);
      })
      .map(listItemOf);
    return c.json({ bots });
  });

  app.post("/:id/bots", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody(c);
    const bot = asRecord(body.bot);
    if (!bot) return badRequest(c, "invalid body");
    const created = createBot(store, universe, bot, body.triggers);
    if (created instanceof BotConfigError) return configErrorResponse(c, created);
    return c.json(
      { bot: botViewOf(created.record), triggers: [...created.record.triggers.values()] },
      201,
    );
  });

  app.get("/:id/bots/:botId", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    return c.json({ bot: botViewOf(found.record) });
  });

  /// Whole-document replace with an expected revision; an unknown id
  /// creates the bot, like the core's `bots/put`.
  app.put("/:id/bots/:botId", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const botId = c.req.param("botId") ?? "";
    const body = await readBody(c);
    const input = asRecord(body.bot);
    if (!input) return badRequest(c, "invalid body");
    const record = universe.bots.get(botId);
    if (!record) {
      const created = createBot(store, universe, { ...input, botId }, undefined);
      if (created instanceof BotConfigError) return configErrorResponse(c, created);
      return c.json({ bot: botViewOf(created.record) });
    }
    if (record.bot.closedAtMs != null) return conflict(c, "bot is closed");
    if (typeof body.expectedRevision === "number" && body.expectedRevision !== record.bot.revision) {
      return conflict(c, "bot revision conflict");
    }
    const bot = botFromInput({ ...input, botId }, record.bot);
    if (bot instanceof BotConfigError) return configErrorResponse(c, bot);
    if (!universe.profiles.has(bot.profileId)) {
      return badRequest(c, `profile ${bot.profileId} does not exist`);
    }
    record.bot = bot;
    record.state.enabled = bot.enabled ?? true;
    record.state.appliedProfileRevision = universe.profiles.get(bot.profileId)?.revision ?? null;
    return c.json({ bot: botViewOf(record) });
  });

  app.post("/:id/bots/:botId/close", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    closeBot(found.universe, found.record);
    return c.json({ bot: botViewOf(found.record) });
  });

  app.delete("/:id/bots/:botId", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    closeBot(universe, record);
    const deletedSessions: string[] = [];
    for (const sessionId of record.bot.closedSessions ?? []) {
      if (universe.sessions.delete(sessionId)) deletedSessions.push(sessionId);
    }
    universe.channelPairings = universe.channelPairings.filter(
      (pairing) => pairing.botId !== record.bot.botId,
    );
    universe.bots.delete(record.bot.botId);
    return c.json({ bot: botViewOf(record), deletedSessions });
  });

  app.get("/:id/bots/:botId/state", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    return c.json({ state: { controller: found.record.state, descendants: found.record.descendants } });
  });

  app.post("/:id/bots/:botId/sessions/:sessionId/rotate", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    const sessionId = c.req.param("sessionId") ?? "";
    const managed = sessionsOf(record.state).find((entry) => entry.sessionId === sessionId);
    if (!managed) return c.json({ error: "session is not managed by this bot" }, 404);
    if (record.bot.closedAtMs != null) return conflict(c, "bot is closed");
    setTimeout(() => rotateSession(store, universe, record, managed), ROTATE_DELAY_MS);
    return c.json({ accepted: true });
  });

  app.get("/:id/bots/:botId/triggers", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const triggers = [...found.record.triggers.values()].sort((left, right) =>
      left.triggerId.localeCompare(right.triggerId),
    );
    return c.json({ triggers });
  });

  app.get("/:id/bots/:botId/triggers/:triggerId", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const trigger = found.record.triggers.get(c.req.param("triggerId") ?? "");
    if (!trigger) return notFound(c);
    return c.json({ trigger });
  });

  /// Whole-document replace with an expected revision; an unknown id
  /// creates the trigger, like the core's `bots/triggers/put`.
  app.put("/:id/bots/:botId/triggers/:triggerId", async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    if (record.bot.closedAtMs != null) return conflict(c, "bot is closed");
    const triggerId = c.req.param("triggerId") ?? "";
    const body = await readBody(c);
    const input = asRecord(body.trigger);
    if (!input) return badRequest(c, "invalid body");
    const existing = record.triggers.get(triggerId);
    if (
      existing &&
      typeof body.expectedRevision === "number" &&
      body.expectedRevision !== existing.revision
    ) {
      return conflict(c, "trigger revision conflict");
    }
    try {
      const trigger = normalizeTrigger(universe, record, triggerId, input, existing);
      record.triggers.set(triggerId, trigger);
      return c.json({ trigger });
    } catch (error) {
      return configErrorResponse(c, error);
    }
  });

  app.delete("/:id/bots/:botId/triggers/:triggerId", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const triggerId = c.req.param("triggerId") ?? "";
    const trigger = found.record.triggers.get(triggerId);
    if (!trigger) return notFound(c);
    found.record.triggers.delete(triggerId);
    return c.json({ trigger });
  });

  /// Operator admit: `{ event: BotEventInput }`, answered 202.
  app.post("/:id/bots/:botId/events", async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    if (record.bot.closedAtMs != null) return c.json({ error: "bot is closed" }, 410);
    if (record.bot.enabled === false) return conflict(c, "bot is disabled");
    const body = await readBody(c);
    const input = asRecord(body.event);
    if (!input) return badRequest(c, "invalid body");
    const kind = optionalString(input.kind);
    const summary = optionalString(input.summary);
    if (!kind || !summary) return badRequest(c, "validation failed: kind and summary are required");
    const links = Array.isArray(input.links)
      ? input.links.filter((link): link is string => typeof link === "string")
      : undefined;
    const headers = isRecord(input.headers)
      ? Object.fromEntries(
          Object.entries(input.headers).filter((entry): entry is [string, string] => typeof entry[1] === "string"),
        )
      : undefined;
    const { event, duplicate } = admitBotEvent(store, universe, record, {
      kind,
      source: "manual",
      summary,
      payload: input.data,
      ...(typeof input.eventId === "string" && input.eventId ? { eventId: input.eventId } : {}),
      ...(typeof input.occurredAtMs === "number" ? { occurredAtMs: input.occurredAtMs } : {}),
      ...(headers === undefined ? {} : { headers }),
      correlationId: optionalString(input.correlationId),
      ...(links === undefined ? {} : { links }),
    });
    return c.json({ event, duplicate }, 202);
  });

  /// A replay is a fresh event reusing the stored document and routing.
  app.post("/:id/bots/:botId/events/replay", async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    const body = await readBody(c);
    if (typeof body.seq !== "number") return badRequest(c, "invalid body");
    const stored = record.events.find((event) => event.seq === body.seq);
    if (!stored) return notFound(c);
    if (record.bot.closedAtMs != null) return c.json({ error: "bot is closed" }, 410);
    if (record.bot.enabled === false) return conflict(c, "bot is disabled");
    let summary = `replay of #${stored.seq}`;
    let source = "replay";
    let payload: unknown;
    const raw = store.readText(stored.documentRef);
    if (raw) {
      try {
        const document = asRecord(JSON.parse(raw));
        if (typeof document?.summary === "string") summary = document.summary;
        if (typeof document?.source === "string") source = document.source;
        payload = document?.data;
      } catch {
        // Unreadable document: the envelope stub above stands in.
      }
    }
    try {
      const { event } = admitBotEvent(store, universe, record, {
        eventId: `replay-${crypto.randomUUID()}`,
        kind: stored.kind,
        source,
        summary,
        payload,
        documentRef: stored.documentRef,
        trigger: stored.triggerId ? (record.triggers.get(stored.triggerId) ?? null) : null,
        session: stored.session,
      });
      return c.json({ event }, 202);
    } catch (error) {
      return configErrorResponse(c, error);
    }
  });

  app.get("/:id/bots/:botId/events", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const limit = Math.min(intQuery(c, "limit", 50), MAX_HISTORY_LIMIT);
    const cursor = c.req.query("cursor");
    const newestFirst = [...found.record.events].sort((left, right) => right.seq - left.seq);
    let start = 0;
    if (cursor) {
      const index = newestFirst.findIndex((event) => String(event.seq) === cursor);
      if (index < 0) return badRequest(c, "invalid cursor");
      start = index + 1;
    }
    const events = newestFirst.slice(start, start + limit);
    const last = events.at(-1);
    return c.json({
      events,
      nextCursor: last && start + limit < newestFirst.length ? String(last.seq) : null,
    });
  });

  app.get("/:id/bots/:botId/events/:seq", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const seq = Number(c.req.param("seq"));
    if (!Number.isSafeInteger(seq)) return badRequest(c, "invalid seq");
    const event = found.record.events.find((entry) => entry.seq === seq);
    if (!event) return notFound(c);
    let document: BotEventDocument = {
      version: 1,
      kind: event.kind,
      source: event.triggerId ?? "manual",
      occurredAtMs: event.occurredAtMs,
      summary: event.summary,
    };
    const raw = store.readText(event.documentRef);
    if (raw) {
      try {
        document = JSON.parse(raw) as BotEventDocument;
      } catch {
        // The reconstructed stub above stands in.
      }
    }
    return c.json({ document, event });
  });

  /// The demo has no CEL evaluator: every sampled event matches, which is
  /// enough for the page to exercise the request/response shapes.
  app.post("/:id/bots/:botId/filters/test", async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const body = await readBody(c);
    if (typeof body.filter !== "string" || !body.filter.trim()) {
      return badRequest(c, "validation failed: filter is required");
    }
    if (body.payload !== undefined) {
      const results: BotFilterTestResult[] = [{ matched: true, seq: null }];
      return c.json({ sampled: 1, matched: 1, errors: 0, results });
    }
    const limit = Math.min(typeof body.limit === "number" ? body.limit : 20, MAX_HISTORY_LIMIT);
    const sample = [...found.record.events].sort((left, right) => right.seq - left.seq).slice(0, limit);
    const results: BotFilterTestResult[] = sample.map((event) => ({ matched: true, seq: event.seq }));
    return c.json({ sampled: sample.length, matched: sample.length, errors: 0, results });
  });

  return app;
}

// ---------------------------------------------------------------------------
// Webhook ingest
// ---------------------------------------------------------------------------

interface WebhookExtraction {
  eventId: string;
  kind: string;
  summary: string;
  promptData?: unknown;
}

/// The GitHub preset projects the envelope (action, repository, sender, the
/// subject named after the event); plain webhooks take `kind` from the body.
function extractWebhookEvent(
  triggerId: string,
  preset: WebhookPreset | null,
  data: unknown,
  header: (name: string) => string | undefined,
): WebhookExtraction {
  const body = asRecord(data);
  if (preset === "github") {
    const ghEvent = header("x-github-event") ?? "unknown";
    const action = typeof body?.action === "string" ? body.action : null;
    const kind = action ? `${ghEvent}.${action}` : ghEvent;
    const repoName = asRecord(body?.repository)?.full_name;
    const repository = typeof repoName === "string" ? repoName : null;
    const subject = asRecord(body?.[ghEvent]);
    const promptData =
      subject === undefined
        ? data
        : {
            ...(action === null ? {} : { action }),
            ...(repository === null ? {} : { repository }),
            ...(body?.sender === undefined ? {} : { sender: body.sender }),
            [ghEvent]: subject,
          };
    return {
      eventId: header("x-github-delivery") ?? crypto.randomUUID(),
      kind,
      summary: `GitHub ${kind}${repository ? ` in ${repository}` : ""}`,
      promptData,
    };
  }
  const kind = typeof body?.kind === "string" && body.kind ? body.kind.slice(0, 200) : "webhook";
  return {
    // A digest id would dedupe the page's identical sample bodies; a uuid
    // lets every test-fire show up.
    eventId: crypto.randomUUID(),
    kind,
    summary: `Webhook ${kind} received on trigger ${triggerId}`,
  };
}

/// The `:universe` path segment resolves by the core universe id first
/// (what ingest paths carry), then by the platform id.
function universeByAnyId(store: DemoStore, id: string): UniverseState | null {
  for (const state of store.universes.values()) {
    if (state.universe.lightspeedUniverseId === id) return state;
  }
  return store.universe(id);
}

/// Public ingress: `POST /hooks/bots/{universe}/{bot}/{trigger}/{token}`,
/// authenticated by the per-trigger URL token alone. A wrong token is
/// indistinguishable from an unknown endpoint, like the core's route.
export function hookRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.post("/bots/:universeId/:botId/:triggerId/:token", async (c) => {
    const universe = universeByAnyId(store, c.req.param("universeId") ?? "");
    const record = universe?.bots.get(c.req.param("botId") ?? "");
    const trigger = record?.triggers.get(c.req.param("triggerId") ?? "");
    if (!universe || !record || !trigger || trigger.kind !== "webhook") return notFound(c);
    const token = webhookTokenOf(trigger);
    if (!token || token !== c.req.param("token")) return notFound(c);
    const raw = await c.req.text();
    if (raw.length > MAX_BODY_BYTES) return c.json({ error: "payload too large" }, 413);
    // A closed bot is gone for good: tell the sender to stop, not to retry.
    if (record.bot.closedAtMs != null) return c.json({ error: "bot is closed" }, 410);
    if (record.bot.enabled === false || trigger.enabled === false) {
      return conflict(c, "trigger is disabled");
    }
    let data: unknown;
    try {
      data = raw ? JSON.parse(raw) : undefined;
    } catch {
      data = undefined;
    }
    const extraction = extractWebhookEvent(trigger.triggerId, trigger.preset ?? null, data, (name) =>
      c.req.header(name),
    );
    const { event, duplicate } = admitBotEvent(store, universe, record, {
      eventId: extraction.eventId,
      kind: extraction.kind,
      source: `webhook:${trigger.triggerId}`,
      summary: extraction.summary,
      payload: data,
      ...(extraction.promptData === undefined ? {} : { promptData: extraction.promptData }),
      trigger,
    });
    return c.json({ eventId: event.eventId, duplicate }, 202);
  });

  return app;
}
