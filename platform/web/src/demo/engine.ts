/// Engine simulation: sessions, runs, and the event log the transcript
/// tail reads. Shapes follow the generated `@lightspeed-ai/agent-client`
/// contract so the real transcript reducer renders them unchanged.
import type {
  ContextEntryView,
  ContextMessageRoleView,
  ContextEntrySourceView,
  EventJoinsView,
  ModelConfig,
  RunAcceptedSourceView,
  RunSummaryView,
  RunView,
  SessionEventKindView,
  SessionEventView,
  SessionEventsReadResponse,
  ToolCallEventView,
} from "@lightspeed-ai/agent-client";
import type { SessionManagement, SessionOrigin, SessionView } from "@/api";
import type {
  DemoResponder,
  DemoStore,
  DemoToolCall,
  DemoTurn,
  SessionRecord,
  UniverseState,
} from "./store";

export const DEFAULT_MODEL: ModelConfig = {
  providerId: "anthropic",
  apiKind: "anthropic:messages",
  model: "claude-opus-5",
};

export const DEFAULT_INSTRUCTIONS_KEY = "instructions.000.default";
export const PROFILE_INSTRUCTIONS_KEY = "instructions.050.profile";

// ---------------------------------------------------------------------------
// Events and context entries
// ---------------------------------------------------------------------------

export function pushEvent(
  session: SessionRecord,
  kind: SessionEventKindView,
  joins: EventJoinsView = {},
  observedAtMs = Date.now(),
): SessionEventView {
  const seq = (session.events.at(-1)?.cursor.seq ?? 0) + 1;
  const event: SessionEventView = {
    cursor: { seq },
    joins,
    kind,
    observedAtMs,
    sessionId: session.view.id,
  };
  session.events.push(event);
  if (observedAtMs > session.view.updatedAtMs) session.view.updatedAtMs = observedAtMs;
  for (const wake of [...session.waiters]) wake();
  return event;
}

/// The origin the bot controller stamps on a delivered event's message, so
/// the transcript shows it as an event band rather than a typed message.
export const EVENT_ORIGIN = "event";

/// Scripted histories hand in the delivered text of an event as the run's
/// input; the hosted controller would have stamped its origin, so the demo
/// derives it from the delivered header line.
export function inputOrigin(text: string): string | undefined {
  return text.startsWith("── event #") ? EVENT_ORIGIN : undefined;
}

export function contextMessage(
  id: string,
  role: ContextMessageRoleView,
  text: string,
  source?: ContextEntrySourceView,
  origin?: string,
): ContextEntryView {
  return {
    id,
    content: { contentRef: `blob:${id}`, mediaType: "text/plain", providerKind: null },
    kind: { type: "message", role },
    text,
    ...(source ? { source } : {}),
    ...(origin ? { origin } : {}),
  } as ContextEntryView;
}

export function contextToolCall(id: string, callId: string, name: string): ContextEntryView {
  return { id, content: { contentRef: `blob:${id}`, mediaType: "text/plain", providerKind: null }, kind: { type: "toolCall", callId, name } } as ContextEntryView;
}

export function contextToolResult(
  id: string,
  callId: string,
  output: string,
  isError = false,
): ContextEntryView {
  return {
    id,
    content: { contentRef: `blob:${id}`, mediaType: "text/plain", providerKind: null },
    kind: { type: "toolResult", callId, isError },
    text: output,
  } as ContextEntryView;
}

export function contextReasoning(id: string, text: string): ContextEntryView {
  return { id, content: { contentRef: `blob:${id}`, mediaType: "text/plain", providerKind: null }, kind: { type: "reasoningState" }, text };
}

/// Applies entries to the active context and records the event.
export function applyEntries(
  session: SessionRecord,
  entries: ContextEntryView[],
  joins: EventJoinsView = {},
  at = Date.now(),
): SessionEventView {
  const baseRevision = session.activeContext.revision;
  const revision = baseRevision + 1;
  session.activeContext.revision = revision;
  session.activeContext.entries.push(...entries);
  const run = joins.runId ? session.runs.get(joins.runId) : undefined;
  if (run) run.entries = [...(run.entries ?? []), ...entries];
  return pushEvent(session, { type: "contextEntriesApplied", baseRevision, revision, entries }, joins, at);
}

export function readEvents(
  session: SessionRecord,
  after: number | null,
  limit = 200,
): SessionEventsReadResponse {
  const from = after ?? 0;
  const pending = session.events.filter((event) => event.cursor.seq > from);
  const events = pending.slice(0, limit);
  return {
    events,
    nextCursor: events.at(-1)?.cursor ?? null,
    headCursor: session.events.at(-1)?.cursor ?? null,
    complete: pending.length <= limit,
    gap: null,
  };
}

/// Long-poll: resolves as soon as an event lands after `after`, or when
/// `waitMs` elapses, or when the client aborts.
export async function waitForEvents(
  session: SessionRecord,
  after: number | null,
  limit: number,
  waitMs: number,
  signal?: AbortSignal | null,
): Promise<SessionEventsReadResponse> {
  const first = readEvents(session, after, limit);
  if (first.events?.length || waitMs <= 0 || signal?.aborted) return first;
  await new Promise<void>((resolve) => {
    const done = () => {
      session.waiters.delete(done);
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, Math.min(waitMs, 25_000));
    session.waiters.add(done);
    signal?.addEventListener("abort", done);
  });
  return readEvents(session, after, limit);
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export interface NewSessionInit {
  /// Descriptive key/value metadata stamped at creation.
  metadata?: Record<string, string>;
  id?: string;
  displayName?: string | null;
  config?: Record<string, unknown>;
  managed?: boolean;
  management?: SessionManagement | null;
  origin?: SessionOrigin | null;
  activeEnvironmentId?: string | null;
  instructions?: string | null;
  createdAtMs?: number;
  deleteAfterCloseMs?: number | null;
  responder?: DemoResponder;
}

export function newSession(
  store: DemoStore,
  universe: UniverseState,
  init: NewSessionInit = {},
): SessionRecord {
  const at = init.createdAtMs ?? Date.now();
  const id = init.id ?? store.nextId("session");
  const config = init.config ?? { model: { ...DEFAULT_MODEL } };
  const retentionRootSessionId = init.origin
    ? universe.sessions.get(init.origin.parentSessionId)?.view.retention.rootSessionId ?? id
    : id;
  const view: SessionView = {
    id,
    displayName: init.displayName ?? null,
    metadata: init.metadata ?? {},
    createdAtMs: at,
    updatedAtMs: at,
    closedAtMs: null,
    status: "idle",
    retention: {
      rootSessionId: retentionRootSessionId,
      deleteAfterCloseMs: retentionRootSessionId === id
        ? init.deleteAfterCloseMs ?? null
        : universe.sessions.get(retentionRootSessionId)?.view.retention.deleteAfterCloseMs ?? null,
      deleteAtMs: null,
    },
    managed: init.managed ?? false,
    activeEnvironmentId: init.activeEnvironmentId ?? null,
    config,
    configRevision: 0,
    management: init.management ?? null,
    origin: init.origin ?? null,
    runs: [],
  };
  const record: SessionRecord = {
    view,
    events: [],
    activeContext: { revision: 0, entries: [] },
    instructions: null,
    submissions: new Map(),
    runs: new Map(),
    queue: [],
    steering: [],
    timers: new Set(),
    waiters: new Set(),
    turns: 0,
    responder: init.responder,
  };
  setInstructions(store, record, init.instructions ?? null);
  universe.sessions.set(id, record);
  pushEvent(record, { type: "sessionOpened", model: modelOf(config) }, {}, at);
  return record;
}

export function modelOf(config: Record<string, unknown> | null | undefined): ModelConfig | null {
  const model = config?.model;
  if (!model || typeof model !== "object") return null;
  const { providerId, apiKind, model: name } = model as Partial<ModelConfig>;
  return providerId && apiKind && name ? { providerId, apiKind, model: name } : null;
}

/// Rebuilds the instruction entries: the default one always, plus the
/// profile-keyed one when custom text is set.
export function setInstructions(store: DemoStore, session: SessionRecord, text: string | null): void {
  const retained = session.activeContext.entries.filter(
    (entry) => entry.key !== DEFAULT_INSTRUCTIONS_KEY && entry.key !== PROFILE_INSTRUCTIONS_KEY,
  );
  const instructions: ContextEntryView[] = [
    {
      id: "default-instructions",
      key: DEFAULT_INSTRUCTIONS_KEY,
      kind: { type: "instructions" },
      content: { contentRef: store.defaultInstructionsRef, mediaType: "text/plain", providerKind: null },
      preview: "Default instructions",
    },
  ];
  if (text) {
    instructions.push({
      id: store.nextId("instructions"),
      key: PROFILE_INSTRUCTIONS_KEY,
      kind: { type: "instructions" },
      content: { contentRef: store.putText(text), mediaType: "text/plain", providerKind: null },
      preview: "Profile instructions",
    });
  }
  session.instructions = text;
  session.activeContext.entries = [...instructions, ...retained];
}

/// What `runAccepted` carries for a text submission.
export function inputSource(store: DemoStore, text: string, origin?: string): RunAcceptedSourceView {
  return {
    type: "input",
    entries: [{ origin, content: { contentRef: store.putText(text), mediaType: "text/plain", providerKind: null }, kind: { type: "message", role: "user" }, preview: text }],
  };
}

export function findRun(session: SessionRecord, runId: string): RunView | null {
  return session.runs.get(runId) ?? null;
}

export function activeRun(session: SessionRecord): RunView | null {
  return [...session.runs.values()].find(
    (run) => run.status === "running" || run.status === "cancelling",
  ) ?? null;
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

export interface RunInput {
  text: string;
  origin?: string;
  submissionId?: string | null;
  /// Scripted turn; defaults to the session's or universe's responder.
  turn?: DemoTurn;
  source?: RunView["source"];
  /// Called once the run reaches a terminal state.
  onFinished?: (run: RunView) => void;
}

type DemoRun = RunView & RunSummaryView;

/// Accepts a run: `running` at once when the session is idle, else `queued`
/// behind the active run (mirrors the hosted acceptance boundary).
export function startRun(
  store: DemoStore,
  universe: UniverseState,
  session: SessionRecord,
  input: RunInput,
): RunView {
  if (input.submissionId) {
    const existing = session.submissions.get(input.submissionId);
    if (existing) return existing;
  }
  const source = {
    type: "input" as const,
    items: input.source?.items ?? [{ type: "text" as const, text: input.text, origin: input.origin }],
    preview: input.text,
    previewTruncated: false,
  };
  const run: DemoRun = {
    id: store.nextId("run"),
    status: "queued",
    acceptedAtMs: Date.now(),
    source,
  };
  session.view.runs = [...(session.view.runs ?? []), run];
  session.runs.set(run.id, run);
  if (input.submissionId) session.submissions.set(input.submissionId, run);
  const joins: EventJoinsView = { runId: run.id, submissionId: input.submissionId ?? null };
  pushEvent(
    session,
    {
      type: "runAccepted",
      runId: run.id,
      submissionId: input.submissionId ?? null,
      source: inputSource(store, input.text, input.origin),
    },
    joins,
  );

  const begin = () => {
    run.status = "running";
    run.startedAtMs = Date.now();
    session.view.status = "active";
    pushEvent(session, { type: "runStarted", runId: run.id }, joins);
    session.turns += 1;
    applyEntries(
      session,
      [
        contextMessage(store.nextId("entry"), "user", input.text, {
          type: "runInput",
          inputIndex: 0,
          runId: run.id,
        }, input.origin),
      ],
      joins,
    );
    const respond = session.responder ?? universe.responder;
    const turn =
      input.turn ?? respond(input.text, { store, universe, session, turn: session.turns });
    schedule(session, run, turnSteps(store, session, run, turn, 1), () =>
      afterTurns(store, universe, session, run, 2, input.onFinished),
    );
  };
  if (activeRun(session)) {
    session.queue.push({ runId: run.id, begin });
  } else {
    begin();
  }
  return run;
}

/// Consumes admitted steering as extra turns, then completes the run.
function afterTurns(
  store: DemoStore,
  universe: UniverseState,
  session: SessionRecord,
  run: RunView,
  turnIndex: number,
  onFinished?: (run: RunView) => void,
): void {
  const steer = session.steering.shift();
  if (steer === undefined) {
    finishRun(session, run, "completed");
    onFinished?.(run);
    return;
  }
  applyEntries(
    session,
    [
      contextMessage(store.nextId("entry"), "user", steer.text, {
        type: "steering",
        inputIndex: 0,
        runId: run.id,
        steeringId: steer.steeringId,
      }, steer.origin),
    ],
    { runId: run.id },
  );
  const respond = session.responder ?? universe.responder;
  const turn = respond(steer.text, { store, universe, session, turn: session.turns });
  schedule(session, run, turnSteps(store, session, run, turn, turnIndex), () =>
    afterTurns(store, universe, session, run, turnIndex + 1, onFinished),
  );
}

export function finishRun(
  session: SessionRecord,
  run: RunView,
  status: "completed" | "cancelled" | "failed",
  at = Date.now(),
): void {
  for (const timer of session.timers) clearTimeout(timer);
  session.timers.clear();
  run.status = status;
  run.completedAtMs = at;
  const joins = { runId: run.id };
  if (status === "completed") {
    completeRunOutput(session, run, at);
  } else if (status === "cancelled") {
    pushEvent(session, { type: "runCancelled", runId: run.id }, joins, at);
  } else {
    pushEvent(
      session,
      { type: "runFailed", runId: run.id, kind: "internal", message: "demo run failed" },
      joins,
      at,
    );
  }
  if (session.view.status !== "closed") session.view.status = "idle";
  const next = session.queue.shift();
  if (next) next.begin();
}

function completeRunOutput(session: SessionRecord, run: RunView, at: number): void {
  const message = run.entries?.slice().reverse().find(
    (entry) => entry.kind.type === "message" && entry.kind.role === "assistant",
  );
  run.output = message?.content;
  run.outputText = message?.text;
  pushEvent(session, { type: "runCompleted", runId: run.id, output: run.output ?? null }, { runId: run.id }, at);
}

/// `session/runs/cancel`: a queued run is cancelled outright; an active one
/// becomes `cancelling` and reaches `cancelled` on the tail shortly after.
export function cancelRun(session: SessionRecord, runId: string): RunView | null {
  const run = findRun(session, runId);
  if (!run) return null;
  if (run.status === "queued") {
    session.queue = session.queue.filter((queued) => queued.runId !== runId);
    run.status = "cancelled";
    run.completedAtMs = Date.now();
    pushEvent(session, { type: "runCancelled", runId }, { runId });
    return run;
  }
  if (run.status !== "running") return run;
  for (const timer of session.timers) clearTimeout(timer);
  session.timers.clear();
  run.status = "cancelling";
  pushEvent(session, { type: "runCancellationRequested", runId }, { runId });
  const timer = setTimeout(() => {
    session.timers.delete(timer);
    finishRun(session, run, "cancelled");
  }, 400);
  session.timers.add(timer);
  return run;
}

/// `session/runs/steer`: admitted now, materializes at the next turn boundary.
export function steerRun(
  store: DemoStore,
  session: SessionRecord,
  runId: string,
  text: string,
  origin?: string,
): { steeringId: string; run: RunView } | null {
  const run = findRun(session, runId);
  if (!run || run.status !== "running") return null;
  const steeringId = store.nextId("steer");
  session.steering.push({ text, steeringId, origin });
  pushEvent(
    session,
    {
      type: "runSteeringAccepted",
      runId,
      steeringId,
      input: [{ origin, content: { contentRef: store.putText(text), mediaType: "text/plain", providerKind: null }, kind: { type: "message", role: "user" }, preview: text }],
    },
    { runId },
  );
  return { steeringId, run };
}

/// `at` backdates the close for fixture history; live closes use the clock.
export function closeSession(session: SessionRecord, force: boolean, at = Date.now()): boolean {
  const active = activeRun(session);
  if (active && !force) return false;
  if (session.view.status === "closed") return true;
  for (const queued of session.queue) {
    const run = findRun(session, queued.runId);
    if (run) {
      run.status = "cancelled";
      run.completedAtMs = at;
    }
  }
  session.queue = [];
  if (active) finishRun(session, active, "cancelled", at);
  session.view.status = "closed";
  session.view.closedAtMs = at;
  if (session.view.retention.rootSessionId === session.view.id) {
    const duration = session.view.retention.deleteAfterCloseMs;
    session.view.retention.deleteAtMs = duration == null ? null : at + duration;
  }
  pushEvent(session, { type: "sessionClosed" } as SessionEventKindView, {}, at);
  return true;
}

// ---------------------------------------------------------------------------
// Turn choreography (shared by live runs and fixture history)
// ---------------------------------------------------------------------------

interface Step {
  delayMs: number;
  apply: (at: number) => void;
}

function toolCallEvents(store: DemoStore, tools: DemoToolCall[]): ToolCallEventView[] {
  return tools.map((tool) => {
    const callId = store.nextId("call");
    return {
      callId,
      toolId: tool.toolId,
      toolName: tool.name,
      argumentsRef: `blob:${callId}-args`,
      arguments: JSON.stringify(tool.arguments),
      display: tool.display,
    } as ToolCallEventView;
  });
}

/// One assistant turn as timed steps: (thinking + tool calls) → tool results
/// → reply. Tool-less turns collapse to a single generation.
function turnSteps(
  store: DemoStore,
  session: SessionRecord,
  run: RunView,
  turn: DemoTurn,
  index: number,
): Step[] {
  const runId = run.id;
  const tools = turn.tools ?? [];
  const steps: Step[] = [];
  let turnId = `${runId}-turn-${index}`;
  const joins = (extra: EventJoinsView = {}): EventJoinsView => ({ runId, turnId, ...extra });

  const openTurn = (at: number) => {
    pushEvent(session, { type: "turnStarted", runId, turnId }, joins(), at);
    pushEvent(session, { type: "turnPlanned", runId, turnId }, joins(), at);
    pushEvent(session, { type: "turnGenerationRequested", runId, turnId }, joins(), at);
  };
  const closeGeneration = (at: number) => {
    pushEvent(
      session,
      { type: "turnGenerationCompleted", runId, turnId, status: "succeeded" },
      joins(),
      at,
    );
    pushEvent(session, { type: "turnCompleted", turnId }, joins(), at);
  };

  steps.push({ delayMs: 0, apply: openTurn });

  if (tools.length > 0) {
    const calls = toolCallEvents(store, tools);
    const batchId = store.nextId("batch");
    steps.push({
      delayMs: 700,
      apply: (at) => {
        const entries: ContextEntryView[] = [];
        if (turn.thinking) entries.push(contextReasoning(store.nextId("entry"), turn.thinking));
        for (const call of calls) {
          entries.push(contextToolCall(store.nextId("entry"), call.callId, call.toolName));
        }
        applyEntries(session, entries, joins(), at);
        closeGeneration(at);
        pushEvent(
          session,
          { type: "toolBatchStarted", runId, turnId, batchId, calls },
          joins({ toolBatchId: batchId }),
          at,
        );
        for (const call of calls) {
          pushEvent(
            session,
            { type: "toolCallStarted", runId, turnId, batchId, callId: call.callId },
            joins({ toolBatchId: batchId, toolCallId: call.callId }),
            at,
          );
        }
      },
    });
    steps.push({
      delayMs: 1_100,
      apply: (at) => {
        const results: ContextEntryView[] = [];
        calls.forEach((call, i) => {
          const tool = tools[i]!;
          pushEvent(
            session,
            {
              type: "toolCallCompleted",
              runId,
              turnId,
              batchId,
              callId: call.callId,
              status: tool.isError ? "failed" : "succeeded",
            },
            joins({ toolBatchId: batchId, toolCallId: call.callId }),
            at,
          );
          results.push(
            contextToolResult(store.nextId("entry"), call.callId, tool.output, tool.isError ?? false),
          );
        });
        applyEntries(session, results, joins({ toolBatchId: batchId }), at);
        pushEvent(
          session,
          { type: "toolBatchCompleted", runId, turnId, batchId },
          joins({ toolBatchId: batchId }),
          at,
        );
        turnId = `${runId}-turn-${index}b`;
        openTurn(at);
      },
    });
  }

  steps.push({
    delayMs: tools.length > 0 ? 900 : turn.thinking ? 1_200 : 800,
    apply: (at) => {
      const entries: ContextEntryView[] = [];
      if (turn.thinking && tools.length === 0) {
        entries.push(contextReasoning(store.nextId("entry"), turn.thinking));
      }
      entries.push(contextMessage(store.nextId("entry"), "assistant", turn.text));
      applyEntries(session, entries, joins(), at);
      closeGeneration(at);
    },
  });
  return steps;
}

function schedule(session: SessionRecord, run: RunView, steps: Step[], done: () => void): void {
  let index = 0;
  const next = () => {
    if (run.status !== "running") return;
    const step = steps[index++];
    if (!step) {
      done();
      return;
    }
    const timer = setTimeout(() => {
      session.timers.delete(timer);
      if (run.status !== "running") return;
      step.apply(Date.now());
      next();
    }, step.delayMs);
    session.timers.add(timer);
  };
  next();
}

// ---------------------------------------------------------------------------
// Fixture history
// ---------------------------------------------------------------------------

export interface Exchange {
  /// When the user message arrived (ms since epoch).
  at: number;
  user: string;
  turn: DemoTurn;
}

/// Appends a completed run to a session's history without timers, spacing
/// the events a few seconds apart so timestamps look lived-in.
export function appendExchange(
  store: DemoStore,
  universe: UniverseState,
  session: SessionRecord,
  exchange: Exchange,
): RunView {
  const run: DemoRun = {
    id: store.nextId("run"),
    status: "running",
    acceptedAtMs: exchange.at,
    source: {
      type: "input",
      items: [{ type: "text", text: exchange.user }],
      preview: exchange.user,
      previewTruncated: false,
    },
    startedAtMs: exchange.at,
  };
  session.view.runs = [...(session.view.runs ?? []), run];
  session.runs.set(run.id, run);
  const joins = { runId: run.id };
  let at = exchange.at;
  const origin = inputOrigin(exchange.user);
  pushEvent(session, { type: "runAccepted", runId: run.id, source: inputSource(store, exchange.user, origin) }, joins, at);
  pushEvent(session, { type: "runStarted", runId: run.id }, joins, at);
  session.turns += 1;
  applyEntries(
    session,
    [
      contextMessage(store.nextId("entry"), "user", exchange.user, {
        type: "runInput",
        inputIndex: 0,
        runId: run.id,
      }, origin),
    ],
    joins,
    at,
  );
  for (const step of turnSteps(store, session, run, exchange.turn, 1)) {
    at += step.delayMs * 4;
    step.apply(at);
  }
  finishRun(session, run, "completed", at + 500);
  void universe;
  return run;
}

/// One generation of a scripted run: optional thinking, optional tool
/// batch, and the reply — which an intermediate step leaves out so the run
/// continues into the next generation.
export interface ScriptedStep {
  thinking?: string;
  tools?: DemoToolCall[];
  text?: string;
}

export interface ScriptedRun {
  /// When the user message arrived (ms since epoch).
  at: number;
  user: string;
  steps: ScriptedStep[];
  /// Steering admitted after the 1-based step; it lands before the next one.
  steer?: { afterStep: number; text: string };
  /// The run fails after its last step with this message.
  failure?: string;
}

function entryChars(entries: ContextEntryView[]): number {
  return entries.reduce((sum, entry) => sum + (entry.text?.length ?? entry.preview?.length ?? 0), 0);
}

/// Appends a finished run written step by step, where `appendExchange`'s
/// single turn is not enough: several generations, a tool batch per
/// generation, steering admitted mid-run, a failed run, and token usage on
/// every `turnGenerationCompleted`. Timestamps advance a few seconds per
/// step from `script.at`.
export function appendScriptedRun(store: DemoStore, session: SessionRecord, script: ScriptedRun): RunView {
  const run: DemoRun = {
    id: store.nextId("run"),
    status: "running",
    acceptedAtMs: script.at,
    source: {
      type: "input",
      items: [{ type: "text", text: script.user }],
      preview: script.user,
      previewTruncated: false,
    },
    startedAtMs: script.at,
  };
  session.view.runs = [...(session.view.runs ?? []), run];
  session.runs.set(run.id, run);
  const runJoins: EventJoinsView = { runId: run.id };
  let clock = script.at;
  const origin = inputOrigin(script.user);
  pushEvent(session, { type: "runAccepted", runId: run.id, source: inputSource(store, script.user, origin) }, runJoins, clock);
  pushEvent(session, { type: "runStarted", runId: run.id }, runJoins, clock);
  session.turns += 1;
  applyEntries(
    session,
    [
      contextMessage(store.nextId("entry"), "user", script.user, {
        type: "runInput",
        inputIndex: 0,
        runId: run.id,
      }, origin),
    ],
    runJoins,
    clock,
  );

  let generation = 0;
  const generate = (turnId: string, joins: EventJoinsView, entries: ContextEntryView[], thinkMs: number) => {
    generation += 1;
    pushEvent(session, { type: "turnStarted", runId: run.id, turnId }, joins, clock);
    pushEvent(session, { type: "turnPlanned", runId: run.id, turnId }, joins, clock);
    pushEvent(session, { type: "turnGenerationRequested", runId: run.id, turnId }, joins, clock);
    clock += thinkMs;
    applyEntries(session, entries, joins, clock);
    const inputTokens = 5_200 + 640 * generation + 900 * session.turns;
    const cachedInputTokens = Math.round(inputTokens * (generation === 1 ? 0.71 : 0.94));
    const outputTokens = Math.max(40, Math.round(entryChars(entries) / 4));
    pushEvent(
      session,
      {
        type: "turnGenerationCompleted",
        runId: run.id,
        turnId,
        status: "succeeded",
        usage: { inputTokens, cachedInputTokens, outputTokens },
      },
      joins,
      clock,
    );
    pushEvent(session, { type: "turnCompleted", turnId }, joins, clock);
  };

  let turn = 0;
  script.steps.forEach((step, index) => {
    const tools = step.tools ?? [];
    turn += 1;
    let turnId = `${run.id}-turn-${turn}`;
    const joins = (extra: EventJoinsView = {}): EventJoinsView => ({ runId: run.id, turnId, ...extra });
    clock += 1_500;
    if (tools.length > 0) {
      const calls = toolCallEvents(store, tools);
      const requested: ContextEntryView[] = [];
      if (step.thinking) requested.push(contextReasoning(store.nextId("entry"), step.thinking));
      for (const call of calls) {
        requested.push(contextToolCall(store.nextId("entry"), call.callId, call.toolName));
      }
      generate(turnId, joins(), requested, step.thinking ? 4_000 : 2_200);
      const batchId = store.nextId("batch");
      pushEvent(
        session,
        { type: "toolBatchStarted", runId: run.id, turnId, batchId, calls },
        joins({ toolBatchId: batchId }),
        clock,
      );
      for (const call of calls) {
        pushEvent(
          session,
          { type: "toolCallStarted", runId: run.id, turnId, batchId, callId: call.callId },
          joins({ toolBatchId: batchId, toolCallId: call.callId }),
          clock,
        );
      }
      clock += 900 + 600 * calls.length;
      const results: ContextEntryView[] = [];
      calls.forEach((call, i) => {
        const tool = tools[i];
        if (!tool) return;
        pushEvent(
          session,
          {
            type: "toolCallCompleted",
            runId: run.id,
            turnId,
            batchId,
            callId: call.callId,
            status: tool.isError ? "failed" : "succeeded",
          },
          joins({ toolBatchId: batchId, toolCallId: call.callId }),
          clock,
        );
        results.push(contextToolResult(store.nextId("entry"), call.callId, tool.output, tool.isError ?? false));
      });
      applyEntries(session, results, joins({ toolBatchId: batchId }), clock);
      pushEvent(
        session,
        { type: "toolBatchCompleted", runId: run.id, turnId, batchId },
        joins({ toolBatchId: batchId }),
        clock,
      );
      if (step.text) {
        turn += 1;
        turnId = `${run.id}-turn-${turn}`;
        clock += 1_200;
        generate(turnId, joins(), [contextMessage(store.nextId("entry"), "assistant", step.text)], 2_600);
      }
    } else {
      const entries: ContextEntryView[] = [];
      if (step.thinking) entries.push(contextReasoning(store.nextId("entry"), step.thinking));
      entries.push(contextMessage(store.nextId("entry"), "assistant", step.text ?? ""));
      generate(turnId, joins(), entries, step.thinking ? 5_000 : 2_400);
    }
    if (script.steer && script.steer.afterStep === index + 1) {
      const steeringId = store.nextId("steer");
      const text = script.steer.text;
      const steerOrigin = inputOrigin(text);
      clock += 700;
      pushEvent(
        session,
        {
          type: "runSteeringAccepted",
          runId: run.id,
          steeringId,
          input: [{ origin: steerOrigin, content: { contentRef: store.putText(text), mediaType: "text/plain", providerKind: null }, kind: { type: "message", role: "user" }, preview: text }],
        },
        runJoins,
        clock,
      );
      clock += 300;
      applyEntries(
        session,
        [
          {
            ...contextMessage(store.nextId("entry"), "user", text, undefined, steerOrigin),
            source: { type: "steering", runId: run.id, steeringId, inputIndex: 0 },
          },
        ],
        runJoins,
        clock,
      );
    }
  });

  clock += 400;
  run.completedAtMs = clock;
  if (script.failure) {
    run.status = "failed";
    pushEvent(
      session,
      { type: "runFailed", runId: run.id, kind: "internal", message: script.failure },
      runJoins,
      clock,
    );
  } else {
    run.status = "completed";
    completeRunOutput(session, run, clock);
  }
  return run;
}

/// Plain back-and-forth without runs (chat-style history where the
/// transport, not the engine, is the interesting part).
export function appendMessages(
  store: DemoStore,
  session: SessionRecord,
  messages: Array<{ at: number; role: ContextMessageRoleView; text: string }>,
): void {
  for (const message of messages) {
    applyEntries(session, [contextMessage(store.nextId("entry"), message.role, message.text)], {}, message.at);
  }
}
