/// Session routes over the engine simulation: the sessions browser, the
/// transcript's long-poll tail, run control, and the settings sheet. Shapes
/// and status codes follow the platform server's gateway so the UI cannot
/// tell the difference.
import { Hono, type Context } from "hono";
import type { Environment, ProfileSessionRetention, ProfileSource, SessionView } from "@/api";
import type { ProfileEnvironment, ProfileInstructions } from "@lightspeed-ai/agent-client";
import {
  DEFAULT_MODEL,
  PROFILE_INSTRUCTIONS_KEY,
  cancelRun,
  closeSession,
  findRun,
  modelOf,
  newSession,
  pushEvent,
  setInstructions,
  startRun,
  steerRun,
  waitForEvents,
} from "../engine";
import { sessionSummary, type DemoStore, type SessionRecord, type UniverseState } from "../store";
import { badRequest, conflict, intQuery, notFound, readBody, universeFor } from "./common";

/// What a session start consumes from a profile, whichever source it came
/// from. `profileId` is null for inline profiles.
interface ResolvedProfile {
  profileId: string | null;
  metadata: Record<string, string>;
  retention: ProfileSessionRetention | null;
  config: Record<string, unknown>;
  instructions: ProfileInstructions | null;
  environment: ProfileEnvironment | null;
}

/// `?metadata=key` or `?metadata=key=value`, repeatable. Empty values request
/// key presence; non-empty values request exact matches.
function metadataQueryFilter(values: string[] | undefined): Record<string, string> {
  const filter: Record<string, string> = {};
  for (const raw of values ?? []) {
    const at = raw.indexOf("=");
    const key = (at < 0 ? raw : raw.slice(0, at)).trim();
    if (!key) continue;
    filter[key] = at < 0 ? "" : raw.slice(at + 1).trim();
  }
  return filter;
}

export function sessionRoutes(store: DemoStore): Hono {
  const app = new Hono();

  const lookup = (c: Context): { universe: UniverseState; session: SessionRecord } | null => {
    const universe = universeFor(store, c);
    const session = universe?.sessions.get(c.req.param("sessionId") ?? "");
    return universe && session ? { universe, session } : null;
  };

  /// Newest activity first; the cursor is a plain offset because the demo
  /// list is small and never changes underneath a page.
  app.get("/:id/sessions", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const limit = Math.min(intQuery(c, "limit", 50), 200);
    const offset = intQuery(c, "cursor", 0);
    const rootSessionId = c.req.query("rootSessionId") || null;
    const parentSessionId = c.req.query("parentSessionId") || null;
    const excludeClosed = c.req.query("excludeClosed") === "true";
    const metadata = metadataQueryFilter(c.req.queries("metadata"));
    const all = [...universe.sessions.values()]
      .filter((record) => !rootSessionId || record.view.origin?.rootSessionId === rootSessionId)
      .filter(
        (record) => !parentSessionId || record.view.origin?.parentSessionId === parentSessionId,
      )
      .filter((record) => !excludeClosed || record.view.status !== "closed")
      .filter((record) =>
        Object.entries(metadata).every(([key, value]) => value
          ? record.view.metadata?.[key] === value
          : Object.hasOwn(record.view.metadata ?? {}, key)),
      )
      .sort((a, b) => b.view.updatedAtMs - a.view.updatedAtMs);
    const page = all.slice(offset, offset + limit);
    return c.json({
      sessions: page.map(sessionSummary),
      nextCursor: offset + limit < all.length ? String(offset + limit) : null,
    });
  });

  /// The environment intent is resolved before the session exists so a
  /// refused profile leaves nothing behind; the id is minted early because
  /// a provisioned environment is keyed by it.
  app.post("/:id/sessions", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{
      displayName?: string;
      metadata?: Record<string, string>;
      deleteAfterCloseMs?: number | null;
      profile?: ProfileSource;
      environment?: { type: "none" } | { type: "existing"; environmentId: string };
    }>(c);
    if (!body.profile) return badRequest(c, "profile is required");
    const profile = resolveProfile(universe, body.profile);
    if (!profile) return notFound(c, "not found in engine");
    if (body.environment) {
      profile.environment = body.environment.type === "none" ? null : body.environment;
    }
    const config = sessionConfig(profile.config);
    const sessionId = store.nextId("session");
    const resolved = resolveEnvironment(universe, profile);
    if ("error" in resolved) return conflict(c, `engine conflict: ${resolved.error}`);
    const session = newSession(store, universe, {
      id: sessionId,
      displayName: body.displayName?.trim() || null,
      metadata: { ...profile.metadata, ...(body.metadata ?? {}) },
      deleteAfterCloseMs: Object.hasOwn(body, "deleteAfterCloseMs")
        ? body.deleteAfterCloseMs
        : profile.retention?.deleteAfterCloseMs,
      config,
      activeEnvironmentId: resolved.environmentId,
      instructions: instructionText(store, profile.instructions),
    });
    return c.json(session.view);
  });

  app.get("/:id/sessions/:sessionId", (c) => {
    const found = lookup(c);
    return found ? c.json(found.session.view) : notFound(c, "not found in engine");
  });

  /// Put replaces the whole map; an empty map clears it.
  app.put("/:id/sessions/:sessionId/metadata", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const body = await readBody<{ metadata?: Record<string, string> }>(c);
    found.session.view.metadata = body.metadata ?? {};
    return c.json(found.session.view);
  });

  app.put("/:id/sessions/:sessionId/retention", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    if (session.view.retention.rootSessionId !== session.view.id) {
      return conflict(
        c,
        `engine conflict: retention is owned by ${session.view.retention.rootSessionId}`,
      );
    }
    const body = await readBody<{ deleteAfterCloseMs?: unknown }>(c);
    if (!Object.prototype.hasOwnProperty.call(body, "deleteAfterCloseMs")) {
      return badRequest(c, "deleteAfterCloseMs is required");
    }
    if (
      body.deleteAfterCloseMs !== null
      && (typeof body.deleteAfterCloseMs !== "number" || body.deleteAfterCloseMs <= 0)
    ) {
      return badRequest(c, "deleteAfterCloseMs must be positive or null");
    }
    session.view.retention.deleteAfterCloseMs = body.deleteAfterCloseMs;
    session.view.retention.deleteAtMs = session.view.closedAtMs == null
      || body.deleteAfterCloseMs == null
        ? null
        : session.view.closedAtMs + body.deleteAfterCloseMs;
    for (const child of found.universe.sessions.values()) {
      if (child.view.retention.rootSessionId === session.view.id && child !== session) {
        child.view.retention = { ...session.view.retention };
      }
    }
    return c.json(session.view);
  });

  /// Closing keeps history; `force` cancels active and queued work first.
  /// Environment lifecycles are independent.
  app.post("/:id/sessions/:sessionId/close", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const body = await readBody<{ force?: boolean }>(c);
    if (!closeSession(session, body.force === true)) {
      return conflict(c, "engine conflict: session has active work; close with force to cancel it");
    }
    return c.json(session.view);
  });

  app.delete("/:id/sessions/:sessionId", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { universe, session } = found;
    if (session.view.status !== "closed") {
      return conflict(c, "engine conflict: only closed sessions can be deleted");
    }
    const descendants = retentionDescendants(universe, session.view.id);
    const cascade = c.req.query("cascade") === "true";
    if (descendants.length > 0 && !cascade) {
      return conflict(c, "engine conflict: session has forks or delegated children; enable cascade");
    }
    const selected = cascade ? [session, ...descendants] : [session];
    if (selected.some((candidate) => candidate.view.status !== "closed")) {
      return conflict(c, "engine conflict: every session in the subtree must be closed");
    }
    for (const candidate of selected.reverse()) {
      for (const timer of candidate.timers) clearTimeout(timer);
      candidate.timers.clear();
      // A parked tail returns now instead of waiting out its poll.
      for (const wake of [...candidate.waiters]) wake();
      universe.sessions.delete(candidate.view.id);
    }
    return c.json(sessionSummary(session));
  });

  /// Both history and live following share the public event pagination contract.
  app.get("/:id/sessions/:sessionId/events", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const direction = c.req.query("direction") ?? "forward";
    const beforeRaw = c.req.query("before");
    const before = beforeRaw === undefined ? null : Number(beforeRaw);
    const afterRaw = c.req.query("after");
    const after = afterRaw === undefined ? null : Number(afterRaw);
    const limit = Number(c.req.query("limit") ?? 200);
    const waitMs = Number(c.req.query("waitMs") ?? 0);
    if (!["forward", "backward"].includes(direction) ||
        (before !== null && (!Number.isSafeInteger(before) || before <= 0)) ||
        (after !== null && (!Number.isSafeInteger(after) || after < 0)) ||
        !Number.isSafeInteger(limit) || limit <= 0 ||
        !Number.isSafeInteger(waitMs) || waitMs < 0 ||
        (direction === "backward" && (after !== null || waitMs > 0)) ||
        (direction === "forward" && before !== null)) {
      return badRequest(c, "Invalid event pagination parameters");
    }
    if (direction === "backward") {
      const events = found.session.events;
      const head = events.at(-1)?.cursor.seq ?? 0;
      const through = Math.min(head, before === null ? head : before - 1);
      const lower = Math.max(0, through - Math.min(limit, 500));
      return c.json({
        events: events.filter((event) => event.cursor.seq > lower && event.cursor.seq <= through),
        nextCursor: lower > 0 ? { seq: lower + 1 } : null,
        complete: lower === 0,
        headCursor: { seq: head },
        gap: null,
      });
    }
    return c.json(await waitForEvents(found.session, after, Math.min(limit, 500), Math.min(waitMs, 30_000), c.req.raw.signal));
  });

  /// Whole-document replace with optimistic concurrency, applied at once:
  /// the demo has no turn boundary to wait for.
  app.put("/:id/sessions/:sessionId/config", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const body = await readBody<{ config?: unknown; expectedConfigRevision?: unknown }>(c);
    if (!isRecord(body.config)) return badRequest(c, "config must be an object");
    if (typeof body.expectedConfigRevision !== "number") {
      return badRequest(c, "expectedConfigRevision is required");
    }
    if (body.expectedConfigRevision !== session.view.configRevision) {
      return conflict(
        c,
        `engine conflict: expected config revision ${body.expectedConfigRevision}, got ${session.view.configRevision}`,
      );
    }
    const config = sessionConfig(body.config);
    session.view.config = config;
    session.view.configRevision += 1;
    pushEvent(session, {
      type: "sessionConfigChanged",
      revision: session.view.configRevision,
      model: modelOf(config),
    });
    return c.json(session.view);
  });

  app.get("/:id/sessions/:sessionId/instructions", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const active = session.activeContext.entries
      .filter((entry) => entry.kind.type === "instructions")
      .map((entry) => ({
        key: entry.key ?? null,
        contentRef: entry.content.contentRef,
        preview: entry.preview ?? null,
      }));
    const custom = active.find((entry) => entry.key === PROFILE_INSTRUCTIONS_KEY);
    return c.json({
      text: custom ? store.readText(custom.contentRef) : null,
      contextRevision: session.activeContext.revision,
      active,
    });
  });

  /// Blank text clears the custom entry; the default one always stays.
  app.put("/:id/sessions/:sessionId/instructions", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const body = await readBody<{ text?: unknown }>(c);
    const text = typeof body.text === "string" && body.text.trim() ? body.text : null;
    if (text !== session.instructions) {
      setInstructions(store, session, text);
      session.activeContext.revision += 1;
      session.view.updatedAtMs = Date.now();
    }
    return c.json(session.view);
  });

  app.post("/:id/sessions/:sessionId/environments/:environmentId/activate", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { universe, session } = found;
    if (session.view.status !== "idle") {
      return conflict(c, "engine conflict: environment activation requires an idle session");
    }
    if (!grantsEnvironments(session.view.config ?? {})) {
      return conflict(c, "engine conflict: environment activation requires the environments feature");
    }
    const environment = universe.environments.get(c.req.param("environmentId"));
    if (!environment) return notFound(c, "not found in engine");
    if (!usable(environment)) {
      return conflict(
        c,
        `engine conflict: environment is ${environment.status}: ${environment.environmentId}`,
      );
    }
    session.view.activeEnvironmentId = environment.environmentId;
    session.view.updatedAtMs = Date.now();
    return c.json(session.view);
  });

  app.post("/:id/sessions/:sessionId/environments/deactivate", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    if (session.view.status !== "idle") {
      return conflict(c, "engine conflict: environment deactivation requires an idle session");
    }
    session.view.activeEnvironmentId = null;
    session.view.updatedAtMs = Date.now();
    return c.json(session.view);
  });

  /// Acceptance boundary: the run is `running` or `queued` on return and
  /// the reply arrives on the tail. `submissionId` dedupes retries.
  app.post("/:id/sessions/:sessionId/messages", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { universe, session } = found;
    const body = await readBody<{ text?: unknown; submissionId?: unknown }>(c);
    if (typeof body.text !== "string" || !body.text.trim()) return badRequest(c, "text is required");
    if (session.view.status === "closed") return conflict(c, "engine conflict: session is closed");
    const run = startRun(store, universe, session, {
      text: body.text,
      origin: `user:${store.currentUser.id}`,
      submissionId: typeof body.submissionId === "string" ? body.submissionId : null,
    });
    return c.json({ run: { id: run.id, status: run.status } });
  });

  app.post("/:id/sessions/:sessionId/runs/:runId/cancel", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const run = cancelRun(found.session, c.req.param("runId"));
    if (!run) return notFound(c, "not found in engine");
    return c.json({ run: { id: run.id, status: run.status } });
  });

  /// Only a running run takes steering; queued, cancelling, and finished
  /// runs refuse it the way the engine does.
  app.post("/:id/sessions/:sessionId/runs/:runId/steer", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const runId = c.req.param("runId");
    const body = await readBody<{ text?: unknown }>(c);
    if (typeof body.text !== "string" || !body.text.trim()) return badRequest(c, "text is required");
    const run = findRun(session, runId);
    if (!run) return notFound(c, "not found in engine");
    const steered = steerRun(store, session, runId, body.text, `user:${store.currentUser.id}`);
    if (!steered) {
      return conflict(c, `engine conflict: run ${runId} is ${run.status}; only a running run accepts steering`);
    }
    return c.json({
      steeringId: steered.steeringId,
      run: { id: steered.run.id, status: steered.run.status },
    });
  });

  app.post("/:id/sessions/:sessionId/runs/:runId/approvals", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const runId = c.req.param("runId");
    const run = findRun(found.session, runId);
    if (!run) return notFound(c, "not found in engine");
    const body = await readBody<{
      decisions?: Array<{ approvalId?: unknown; decision?: unknown; note?: unknown }>;
    }>(c);
    if (!Array.isArray(body.decisions) || body.decisions.length === 0) {
      return badRequest(c, "decisions are required");
    }
    const pending = new Map((run.pendingApprovals ?? []).map((approval) => [approval.approvalId, approval]));
    const results = body.decisions.map((decision) => {
      const approvalId = typeof decision.approvalId === "string" ? decision.approvalId : "";
      if (!pending.delete(approvalId)) {
        return {
          approvalId,
          status: "failed" as const,
          failure: { kind: "unknown", message: "approval was not found" },
        };
      }
      pushEvent(found.session, {
        type: "approvalDecided",
        runId,
        approvalId,
        decision: decision.decision === "approve" ? "approve" : "reject",
      });
      return { approvalId, status: "decided" as const };
    });
    run.pendingApprovals = [...pending.values()];
    if (run.pendingApprovals.length === 0 && run.status === "parked") run.status = "running";
    return c.json({ results, run });
  });

  return app;
}

function retentionDescendants(universe: UniverseState, parentId: string): SessionRecord[] {
  const descendants: SessionRecord[] = [];
  const pending = [parentId];
  while (pending.length > 0) {
    const parent = pending.shift()!;
    for (const candidate of universe.sessions.values()) {
      if (candidate.view.origin?.parentSessionId !== parent) continue;
      descendants.push(candidate);
      pending.push(candidate.view.id);
    }
  }
  return descendants;
}

function resolveProfile(universe: UniverseState, source: ProfileSource): ResolvedProfile | null {
  if (source.kind === "inline") {
    const profile = source.profile ?? {};
    return {
      profileId: null,
      metadata: isStringMap(profile.metadata) ? profile.metadata : {},
      retention: profile.retention ?? null,
      config: isRecord(profile.config) ? profile.config : {},
      instructions: profile.instructions ?? null,
      environment: profile.environment ?? null,
    };
  }
  const document = universe.profiles.get(source.profileId);
  if (!document) return null;
  const instructions = document.instructions;
  return {
    profileId: document.profileId,
    metadata: isStringMap(document.metadata) ? document.metadata : {},
    retention: document.retention ?? null,
    config: isRecord(document.config) ? document.config : {},
    instructions: isRecord(instructions) ? (instructions as unknown as ProfileInstructions) : null,
    environment: document.environment ?? null,
  };
}

/// The session's own copy of a profile config, with the model the demo
/// answers as when the profile leaves it open.
function sessionConfig(config: Record<string, unknown>): Record<string, unknown> {
  const copy = structuredClone(config);
  return { ...copy, model: modelOf(copy) ?? { ...DEFAULT_MODEL } };
}

function instructionText(store: DemoStore, instructions: ProfileInstructions | null): string | null {
  if (!instructions) return null;
  return instructions.type === "text" ? instructions.text : store.readText(instructions.blobRef);
}

/// Profiles select resources whose lifecycle is managed independently.
function resolveEnvironment(
  universe: UniverseState,
  profile: ResolvedProfile,
): { environmentId: string | null } | { error: string } {
  const intent = profile.environment;
  if (!intent || intent.type === "inherit") return { environmentId: null };
  const environment = universe.environments.get(intent.environmentId);
  if (!environment) return { error: `environment not found: ${intent.environmentId}` };
  if (!usable(environment)) return { error: `environment is ${environment.status}: ${intent.environmentId}` };
  return { environmentId: intent.environmentId };
}

/// Provisioning and booting are valid activation targets; a terminal or
/// terminating environment is not.
function usable(environment: Environment): boolean {
  return (
    environment.status !== "closed" &&
    environment.status !== "closing" &&
    environment.status !== "failed"
  );
}

function grantsEnvironments(config: NonNullable<SessionView["config"]>): boolean {
  const features = config.features;
  return isRecord(features) && Boolean(features.environments);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isStringMap(value: unknown): value is Record<string, string> {
  return isRecord(value) && Object.values(value).every((entry) => typeof entry === "string");
}
