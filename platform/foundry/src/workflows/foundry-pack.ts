import {
  condition,
  continueAsNew,
  defineQuery,
  defineSignal,
  proxyActivities,
  setHandler,
  sleep,
  workflowInfo,
} from "@temporalio/workflow";
import type { FoundryActivities } from "../activities/index.js";
import { DELIVER_EMISSION_SIGNAL, parseEmissionEnvelope } from "../contracts/emissions.js";
import {
  FOUNDRY_ACTIVITY_TASK_QUEUE,
  FOUNDRY_CONFIG_SIGNAL,
  FOUNDRY_EVENT_RESOLVE_TOOL_ID,
  FOUNDRY_EVENT_SIGNAL,
  FOUNDRY_PACK_WORKFLOW,
  FOUNDRY_RELEASE_RECORD_TOOL_ID,
  FOUNDRY_STATE_QUERY,
  foundryEventSubmissionId,
  foundryEventTerminalToken,
  foundrySessionId,
  parseEventResolveArgs,
  parseReleaseRecordArgs,
  validateFoundryEvent,
  type FoundryEvent,
  type FoundryEventOutcome,
  type FoundryPackStartV1,
} from "../contracts/foundry.js";

const EVENT_TERMINAL_TIMEOUT = "24 hours";
const BUSY_RETRY_DELAY = "5 seconds";
const CONTINUE_AS_NEW_AFTER_RUNS = 100;
const SEEN_EVENT_CAP = 2_000;
const SEEN_EMISSION_CAP = 2_000;
const RECENT_EVENT_CAP = 50;

export interface FoundryRecentEventSnapshot {
  id: string;
  ref: string;
  status: FoundryEventOutcome | "unresolved" | "run_failed";
  runId?: string;
  summary?: string;
  failure?: string;
}

export interface FoundryPackSnapshot {
  sessionId: string;
  controllerStatus: "initializing" | "idle" | "manager_busy" | "delivering_event" | "degraded";
  activeEvent: FoundryEvent | null;
  activeRunId: string | null;
  sessionReady: boolean;
  pendingEvents: FoundryEvent[];
  pendingEventCount: number;
  recentEvents: FoundryRecentEventSnapshot[];
  eventsProcessed: number;
  duplicateEventCount: number;
  duplicateEmissionCount: number;
  managerProfileId: string;
  appliedProfileRevision: number | null;
  environmentId: string | null;
  lastError: string | null;
}

export interface FoundryCarryV1 {
  version: 1;
  config: FoundryPackStartV1;
  pendingEvents: FoundryEvent[];
  recentEvents: FoundryRecentEventSnapshot[];
  seenEventIds: string[];
  seenEmissionIds: string[];
  eventCursorSeq: number;
  appliedProfileId: string | null;
  appliedProfileRevision: number | null;
  environmentId: string | null;
  sessionReady: boolean;
  eventsProcessed: number;
  duplicateEventCount: number;
  duplicateEmissionCount: number;
}

const activities = proxyActivities<FoundryActivities>({
  taskQueue: FOUNDRY_ACTIVITY_TASK_QUEUE,
  startToCloseTimeout: "60 seconds",
  retry: { maximumAttempts: 5 },
});

export const foundryEventSignal = defineSignal<[FoundryEvent]>(FOUNDRY_EVENT_SIGNAL);
export const foundryConfigSignal = defineSignal<[FoundryPackStartV1]>(FOUNDRY_CONFIG_SIGNAL);
export const deliverEmissionSignal = defineSignal<[unknown]>(DELIVER_EMISSION_SIGNAL);
export const foundryStateQuery = defineQuery<FoundryPackSnapshot>(FOUNDRY_STATE_QUERY);

/** One durable inbox and lifecycle controller per Foundry pack. */
export async function foundryPackWorkflowV1(
  initial: FoundryPackStartV1,
  carry?: FoundryCarryV1,
): Promise<never> {
  validateConfig(initial);
  let config = carry?.config ?? initial;
  const sessionId = foundrySessionId(config.packName);
  const pendingEvents = [...(carry?.pendingEvents ?? [])];
  const recentEvents = [...(carry?.recentEvents ?? [])];
  const seenEventIds = new Set(carry?.seenEventIds ?? pendingEvents.map((event) => event.id));
  const seenEmissionIds = new Set(carry?.seenEmissionIds ?? []);
  const emissionInbox: unknown[] = [];
  let eventCursorSeq = carry?.eventCursorSeq ?? 0;
  let appliedProfileId = carry?.appliedProfileId ?? null;
  let appliedProfileRevision = carry?.appliedProfileRevision ?? null;
  let environmentId = carry?.environmentId ?? null;
  let managerReady =
    (carry?.appliedProfileId ?? null) === config.managerProfileId && environmentId !== null;
  let sessionReady = carry?.sessionReady ?? managerReady;
  let eventsProcessed = carry?.eventsProcessed ?? 0;
  let duplicateEventCount = carry?.duplicateEventCount ?? 0;
  let duplicateEmissionCount = carry?.duplicateEmissionCount ?? 0;
  let controllerStatus: FoundryPackSnapshot["controllerStatus"] = "initializing";
  let activeEvent: FoundryEvent | null = null;
  let activeRunId: string | null = null;
  let activeResolution: { outcome: FoundryEventOutcome; summary: string | null } | null = null;
  let activeTerminal:
    | { token: string; status: string; runId: string; failureRef: string | null }
    | null = null;
  let configDirty = true;
  let lastError: string | null = null;

  setHandler(foundryEventSignal, (event) => {
    validateFoundryEvent(event);
    if (seenEventIds.has(event.id)) {
      duplicateEventCount += 1;
      return;
    }
    seenEventIds.add(event.id);
    pendingEvents.push(event);
    if (!managerReady) configDirty = true;
  });
  setHandler(foundryConfigSignal, (next) => {
    validateConfig(next);
    if (
      next.universeId !== initial.universeId ||
      next.packId !== initial.packId ||
      next.packName !== initial.packName
    ) {
      throw new TypeError("foundry pack identity cannot change");
    }
    config = next;
    configDirty = true;
    managerReady = false;
  });
  setHandler(deliverEmissionSignal, (emission) => {
    emissionInbox.push(emission);
  });
  setHandler(foundryStateQuery, () => ({
    sessionId,
    controllerStatus,
    activeEvent,
    activeRunId,
    sessionReady,
    pendingEvents: pendingEvents.slice(0, 100),
    pendingEventCount: pendingEvents.length,
    recentEvents: [...recentEvents],
    eventsProcessed,
    duplicateEventCount,
    duplicateEmissionCount,
    managerProfileId: config.managerProfileId,
    appliedProfileRevision,
    environmentId,
    lastError,
  }));

  async function reconcileManager(): Promise<boolean> {
    try {
      const ensured = await activities.ensureFoundrySession({
        universeId: config.universeId,
        sessionId,
        displayName: `foundry ${config.packName}`,
        managerProfileId: config.managerProfileId,
        packName: config.packName,
        repoUrl: config.repoUrl,
        runtimeTarget: config.runtimeTarget ?? null,
        appliedProfileRevision:
          appliedProfileId === config.managerProfileId ? appliedProfileRevision : null,
        controller: {
          workflowId: workflowInfo().workflowId,
          workflowKind: FOUNDRY_PACK_WORKFLOW,
        },
      });
      sessionReady = true;
      const environment = await activities.resolveFoundryEnvironment({
        universeId: config.universeId,
        packName: config.packName,
        environmentId: config.environmentId ?? null,
      });
      await activities.activateEnvironment({
        universeId: config.universeId,
        sessionId,
        environmentId: environment.environmentId,
      });
      appliedProfileId = config.managerProfileId;
      appliedProfileRevision = ensured.profileRevision;
      environmentId = environment.environmentId;
      configDirty = false;
      managerReady = true;
      lastError = null;
      controllerStatus = "idle";
      return true;
    } catch (error) {
      lastError = errorMessage(error);
      controllerStatus = "degraded";
      configDirty = false;
      managerReady = false;
      return false;
    }
  }

  async function reconcileRun(runId: string): Promise<void> {
    const pulled = await activities.readWorkflowToolInvocations({
      universeId: config.universeId,
      sessionId,
      afterSeq: eventCursorSeq,
    });
    eventCursorSeq = pulled.nextSeq;
    for (const invocation of pulled.invocations) {
      if (invocation.runId !== runId) continue;
      const args = await activities.readJsonBlob({
        universeId: config.universeId,
        blobRef: invocation.argumentsRef,
      });
      if (invocation.toolId === FOUNDRY_EVENT_RESOLVE_TOOL_ID) {
        const resolution = parseEventResolveArgs(args);
        if (activeEvent !== null && resolution.eventId === activeEvent.id) {
          activeResolution = { outcome: resolution.outcome, summary: resolution.summary };
        }
      } else if (invocation.toolId === FOUNDRY_RELEASE_RECORD_TOOL_ID) {
        await activities.recordRelease({
          packId: config.packId,
          invocationId: invocation.invocationId,
          release: parseReleaseRecordArgs(args),
        });
      }
    }
  }

  async function processEmissions(): Promise<void> {
    while (emissionInbox.length > 0) {
      const raw = emissionInbox.shift();
      if (raw === undefined) continue;
      const envelope = parseEmissionEnvelope(raw);
      if (seenEmissionIds.has(envelope.emission_id)) {
        duplicateEmissionCount += 1;
        continue;
      }
      seenEmissionIds.add(envelope.emission_id);
      if (
        envelope.producer.kind === "session" &&
        (envelope.producer.universe_id !== config.universeId ||
          envelope.producer.session_id !== sessionId)
      ) {
        throw new TypeError("emission does not belong to this Foundry manager session");
      }
      if (envelope.body.kind !== "run_terminal") continue;
      const terminalRunId = `run_${envelope.body.run_id}`;
      await reconcileRun(terminalRunId);
      if (
        activeEvent !== null &&
        envelope.body.token === foundryEventTerminalToken(activeEvent.id)
      ) {
        activeTerminal = {
          token: envelope.body.token,
          status: envelope.body.status,
          runId: terminalRunId,
          failureRef: envelope.body.failure_message_ref,
        };
      }
    }
  }

  async function waitUntilManagerIdle(): Promise<void> {
    for (;;) {
      await processEmissions();
      const state = await activities.readFoundrySessionStatus({
        universeId: config.universeId,
        sessionId,
      });
      if (state.status === "idle") {
        controllerStatus = "idle";
        return;
      }
      controllerStatus = "manager_busy";
      await sleep(BUSY_RETRY_DELAY);
    }
  }

  async function deliverEvent(event: FoundryEvent): Promise<void> {
    activeEvent = event;
    activeResolution = null;
    activeTerminal = null;
    controllerStatus = "delivering_event";
    await waitUntilManagerIdle();
    try {
      const run = await activities.startFoundryRun({
        universeId: config.universeId,
        sessionId,
        event,
        submissionId: foundryEventSubmissionId(event.id),
        terminalToken: foundryEventTerminalToken(event.id),
      });
      activeRunId = run.runId;
    } catch (error) {
      // A direct run can win the narrow read/start race. Preserve the event
      // and retry after the manager becomes idle again.
      lastError = errorMessage(error);
      pendingEvents.unshift(event);
      activeEvent = null;
      activeRunId = null;
      controllerStatus = "manager_busy";
      await sleep(BUSY_RETRY_DELAY);
      return;
    }

    const signaled = await condition(() => emissionInbox.length > 0, EVENT_TERMINAL_TIMEOUT);
    while (signaled && activeTerminal === null) {
      await processEmissions();
      if (activeTerminal === null) {
        const more = await condition(() => emissionInbox.length > 0, EVENT_TERMINAL_TIMEOUT);
        if (!more) break;
      }
    }

    const terminal = activeTerminal as {
      token: string;
      status: string;
      runId: string;
      failureRef: string | null;
    } | null;
    const resolution = activeResolution as {
      outcome: FoundryEventOutcome;
      summary: string | null;
    } | null;
    let recent: FoundryRecentEventSnapshot;
    if (terminal === null) {
      recent = {
        id: event.id,
        ref: event.ref,
        status: "run_failed",
        ...(activeRunId === null ? {} : { runId: activeRunId }),
        failure: "timed out waiting for the manager run terminal",
      };
    } else if (terminal.status !== "completed") {
      recent = {
        id: event.id,
        ref: event.ref,
        status: "run_failed",
        runId: activeRunId ?? terminal.runId,
        failure: `manager run ended ${terminal.status}`,
      };
    } else if (resolution !== null) {
      recent = {
        id: event.id,
        ref: event.ref,
        status: resolution.outcome,
        runId: activeRunId ?? terminal.runId,
        ...(resolution.summary === null ? {} : { summary: resolution.summary }),
      };
    } else {
      recent = {
        id: event.id,
        ref: event.ref,
        status: "unresolved",
        runId: activeRunId ?? terminal.runId,
      };
    }
    recentEvents.push(recent);
    if (recentEvents.length > RECENT_EVENT_CAP) {
      recentEvents.splice(0, recentEvents.length - RECENT_EVENT_CAP);
    }
    eventsProcessed += 1;
    activeEvent = null;
    activeRunId = null;
    activeResolution = null;
    activeTerminal = null;
    lastError = null;
    controllerStatus = "idle";
  }

  await reconcileManager();

  for (;;) {
    await processEmissions();
    if (configDirty && activeEvent === null) {
      await waitUntilManagerIdle().catch(() => undefined);
      await reconcileManager();
    }
    if (config.enabled && managerReady && !configDirty && pendingEvents.length > 0) {
      const event = pendingEvents.shift();
      if (event !== undefined) await deliverEvent(event);
      continue;
    }
    if (
      emissionInbox.length === 0 &&
      activeEvent === null &&
      eventsProcessed >= CONTINUE_AS_NEW_AFTER_RUNS
    ) {
      await continueAsNew<typeof foundryPackWorkflowV1>(config, {
        version: 1,
        config,
        pendingEvents,
        recentEvents,
        seenEventIds: [...seenEventIds].slice(-SEEN_EVENT_CAP),
        seenEmissionIds: [...seenEmissionIds].slice(-SEEN_EMISSION_CAP),
        eventCursorSeq,
        appliedProfileId,
        appliedProfileRevision,
        environmentId,
        sessionReady,
        eventsProcessed: 0,
        duplicateEventCount,
        duplicateEmissionCount,
      } satisfies FoundryCarryV1);
    }
    await condition(
      () =>
        emissionInbox.length > 0 ||
        configDirty ||
        (config.enabled && managerReady && pendingEvents.length > 0),
    );
  }
}

function validateConfig(config: FoundryPackStartV1): void {
  if (config.version !== 1) throw new TypeError("unsupported foundry pack version");
  if (!config.managerProfileId) throw new TypeError("managerProfileId is required");
}

function errorMessage(error: unknown): string {
  if (!(error instanceof Error)) return String(error);
  let current: Error = error;
  while (current.cause instanceof Error) current = current.cause;
  return current.message;
}
