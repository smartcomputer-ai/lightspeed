import { isEnvironmentAttached } from "@session-ui/model/resource-features";
import { demoCanReadSession, demoUniverseActions } from "../access";
import { modelFromConfig } from "@/lib/model-defaults";
import { Hono, type Context } from "hono";
import {
  MAX_ATTACHMENT_BYTES,
  attachmentUploadSchema,
  canCloseSession,
  canDeleteSession,
  sessionMessageSchema,
  sessionSteerSchema,
  type MessageAttachment,
} from "@lightspeed-ai/platform-shared";
import type { SessionView } from "@/api";
import { isRecord, sessionConfig, usableEnvironment } from "../session-config";
import {
  refreshDemoCompactionView,
  PROFILE_INSTRUCTIONS_KEY,
  cancelRun,
  closeSession,
  decideApprovals,
  findRun,
  modelOf,
  pushEvent,
  setInstructions,
  startRun,
  steerRun,
  waitForEvents,
} from "../engine";
import { sessionSummary, type DemoStore, type SessionRecord, type UniverseState } from "../store";
import { badRequest, conflict, intQuery, notFound, readBody, universeFor } from "./common";

export function conversationRoutes(store: DemoStore): Hono {
  const app = new Hono();

  const lookup = (c: Context): { universe: UniverseState; session: SessionRecord } | null => {
    const universe = universeFor(store, c);
    const session = universe?.sessions.get(c.req.param("sessionId") ?? "");
    return universe && session && demoCanReadSession(store, universe, session) ? { universe, session } : null;
  };

  app.get("/:id/sessions/:sessionId/children", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c);
    const limit = Math.min(intQuery(c, "limit", 50), 200);
    const offset = intQuery(c, "cursor", 0);
    const actions = demoUniverseActions(store, found.universe);
    const children = [...found.universe.sessions.values()]
      .filter(child => child.view.origin?.parentSessionId === found.session.view.id && demoCanReadSession(store, found.universe, child, actions))
      .sort((a, b) => b.view.updatedAtMs - a.view.updatedAtMs);
    return c.json({
      sessions: children.slice(offset, offset + limit).map(sessionSummary),
      nextCursor: offset + limit < children.length ? String(offset + limit) : null,
    });
  });

  /// Shares an unshared root session and its sub-agents with the universe,
  /// one way.
  app.post("/:id/sessions/:sessionId/share", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const view = found.session.view;
    if (view.origin) return conflict(c, "a sub-agent's session follows its root session");
    if (view.access?.visibility === "universe") return conflict(c, "the session is already shared");
    const access = { ...view.access, visibility: "universe" as const };
    for (const record of found.universe.sessions.values()) {
      if (record.view.retention.rootSessionId === view.id) record.view.access = access;
    }
    return c.json({ access });
  });

  app.get("/:id/sessions/:sessionId", (c) => {
    const found = lookup(c);
    found?.session.onOpen?.();
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
    if (!canDeleteSession(demoUniverseActions(store, found.universe), found.session.view.access.visibility === "universe")) return c.json({ error: "not permitted to delete this session" }, 403);
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

  app.post("/:id/sessions/:sessionId/context/compact", (c) => {
    if (!lookup(c)) return notFound(c, "not found in engine");
    return c.json({ error: "Manual compaction requires a connected engine and is unavailable in the demo." }, 501);
  });

  /// Closing keeps history; `force` cancels active and queued work first.
  /// Environment lifecycles are independent.
  app.post("/:id/sessions/:sessionId/close", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const access = session.view.access;
    if (!canCloseSession(demoUniverseActions(store, found.universe), access.createdBy?.kind === "actor" && access.createdBy.id === store.currentUser.id, access.visibility === "universe")) {
      return c.json({ error: "not permitted to close this session" }, 403);
    }
    const body = await readBody<{ force?: boolean }>(c);
    if (!closeSession(session, body.force === true)) {
      return conflict(c, "engine conflict: session has active work; close with force to cancel it");
    }
    return c.json(session.view);
  });

  /// Nothing in the demo has an execution to lose; a restart clears the
  /// stopped workflow a debug command can fake.
  app.post("/:id/sessions/:sessionId/restart", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    found.session.view.executionFailure = null;
    return c.json(found.session.view);
  });

  app.delete("/:id/sessions/:sessionId", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    if (!canDeleteSession(demoUniverseActions(store, found.universe), found.session.view.access.visibility === "universe")) return c.json({ error: "not permitted to delete this session" }, 403);
    const { universe, session } = found;
    if (session.view.lifecycleStatus !== "closed") {
      return conflict(c, "engine conflict: only closed sessions can be deleted");
    }
    const descendants = retentionDescendants(universe, session.view.id);
    const cascade = c.req.query("cascade") === "true";
    if (descendants.length > 0 && !cascade) {
      return conflict(c, "engine conflict: session has delegated children; enable cascade");
    }
    const selected = cascade ? [session, ...descendants] : [session];
    if (selected.some((candidate) => candidate.view.lifecycleStatus !== "closed")) {
      return conflict(c, "engine conflict: every session in the subtree must be closed");
    }
    const actions = demoUniverseActions(store, universe);
    if (selected.some((candidate) => !canDeleteSession(actions, candidate.view.access.visibility === "universe"))) {
      return c.json({ error: "not permitted to delete this session subtree" }, 403);
    }
    const deletedAtMs = Date.now();
    for (const candidate of selected.reverse()) {
      for (const timer of candidate.timers) clearTimeout(timer);
      candidate.timers.clear();
      // A parked tail returns now instead of waiting out its poll.
      for (const wake of [...candidate.waiters]) wake();
      store.deletedSessions.set(`${universe.universe.id}:${candidate.view.id}`, { record: candidate, deletedAtMs });
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
    const config = sessionConfig(body.config, modelOf(session.view.config));
    if (!config) return badRequest(c, "Session has no model.");
    session.view.config = config;
    refreshDemoCompactionView(session);
    if (session.view.activeEnvironmentId && !isEnvironmentAttached(config, session.view.activeEnvironmentId)) session.view.activeEnvironmentId = null;
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
        blobRef: entry.content.blobRef,
        preview: entry.preview ?? null,
      }));
    const custom = active.find((entry) => entry.key === PROFILE_INSTRUCTIONS_KEY);
    return c.json({
      text: custom ? store.readText(custom.blobRef) : null,
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
    if (session.view.lifecycleStatus === "closed" || session.view.activity !== "idle") {
      return conflict(c, "engine conflict: environment activation requires an idle session");
    }
    if (!grantsEnvironments(session.view.config ?? {})) {
      return conflict(c, "engine conflict: environment activation requires the environments feature");
    }
    const environment = universe.environments.get(c.req.param("environmentId"));
    if (!environment) return notFound(c, "not found in engine");
    if (!usableEnvironment(environment)) {
      return conflict(
        c,
        `engine conflict: environment is ${environment.status}: ${environment.environmentId}`,
      );
    }
    if (!isEnvironmentAttached(session.view.config, environment.environmentId)) return conflict(c, "engine conflict: environment is not attached to the session");
    session.view.activeEnvironmentId = environment.environmentId;
    session.view.updatedAtMs = Date.now();
    return c.json(session.view);
  });

  app.post("/:id/sessions/:sessionId/environments/deactivate", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    if (session.view.lifecycleStatus === "closed" || session.view.activity !== "idle") {
      return conflict(c, "engine conflict: environment deactivation requires an idle session");
    }
    session.view.activeEnvironmentId = null;
    session.view.updatedAtMs = Date.now();
    return c.json(session.view);
  });

  const missingAttachment = (attachments: readonly MessageAttachment[]) =>
    attachments.find((attachment) => !store.blobs.has(attachment.blobRef))?.blobRef;

  /// Composer attachments: stored like any blob, content-addressed.
  app.post("/:id/attachments", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (!demoUniverseActions(store, universe).has("useResource")) {
      return c.json({ error: "universe permission required: useResource" }, 403);
    }
    const body = attachmentUploadSchema.safeParse(await readBody(c));
    if (!body.success) return badRequest(c, "Invalid attachment upload");
    const bytes = Uint8Array.from(atob(body.data.bytesBase64), (char) => char.charCodeAt(0));
    if (bytes.length > MAX_ATTACHMENT_BYTES) return c.json({ error: "Attachment exceeds the 10 MiB limit." }, 413);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
    const blobRef = `sha256:${[...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
    store.blobs.set(blobRef, { blobRef, bytes: bytes.length, bytesBase64: body.data.bytesBase64 });
    return c.json({ blobs: [{ blobRef, bytes: bytes.length }] });
  });

  /// Acceptance boundary: the run is `running` or `queued` on return and
  /// the reply arrives on the tail. `submissionId` dedupes retries.
  app.post("/:id/sessions/:sessionId/messages", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { universe, session } = found;
    const body = sessionMessageSchema.safeParse(await readBody(c));
    if (!body.success) return badRequest(c, body.error.issues[0]?.message ?? "Invalid message");
    if (session.view.lifecycleStatus === "closed") return conflict(c, "engine conflict: session is closed");
    const missing = missingAttachment(body.data.attachments);
    if (missing) return badRequest(c, `blob ${missing} was not found`);
    // The runtime keeps provider identity and API kind fixed per session.
    const pinned = modelFromConfig(session.view.config);
    const override = body.data.options?.model;
    if (override && pinned && (override.providerId !== pinned.providerId || override.apiKind !== pinned.apiKind)) {
      return badRequest(c, "a run model override must keep the session's provider and API kind");
    }
    const run = startRun(store, universe, session, {
      text: body.data.text,
      media: body.data.attachments,
      origin: `user:${store.currentUser.id}`,
      submissionId: body.data.submissionId,
    });
    return c.json({ run: { id: run.id, status: run.status } });
  });

  app.get("/:id/sessions/:sessionId/runs/:runId", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const run = findRun(found.session, c.req.param("runId"));
    return run ? c.json(run) : notFound(c, "run not found");
  });

  app.post("/:id/sessions/:sessionId/runs/:runId/cancel", (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const run = cancelRun(found.session, c.req.param("runId"));
    if (!run) return notFound(c, "not found in engine");
    return c.json({ run: { id: run.id, status: run.status, startedAtMs: run.startedAtMs } });
  });

  /// Only a running run takes steering; queued, cancelling, and finished
  /// runs refuse it the way the engine does.
  app.post("/:id/sessions/:sessionId/runs/:runId/steer", async (c) => {
    const found = lookup(c);
    if (!found) return notFound(c, "not found in engine");
    const { session } = found;
    const runId = c.req.param("runId");
    const body = sessionSteerSchema.safeParse(await readBody(c));
    if (!body.success) return badRequest(c, body.error.issues[0]?.message ?? "Invalid steering");
    const missing = missingAttachment(body.data.attachments);
    if (missing) return badRequest(c, `blob ${missing} was not found`);
    const run = findRun(session, runId);
    if (!run) return notFound(c, "not found in engine");
    const steered = steerRun(store, session, runId, body.data.text, `user:${store.currentUser.id}`, body.data.attachments, body.data.submissionId);
    if (!steered) {
      return c.json({
        error: `run ${runId} is ${run.status}; only a running run accepts steering`,
        kind: "steering_not_submitted",
      }, 409);
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
    const results = decideApprovals(found.session, run, body.decisions.map((decision) => ({
      approvalId: typeof decision.approvalId === "string" ? decision.approvalId : "",
      decision: decision.decision === "approve" ? "approve" as const : "reject" as const,
    })));
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

function grantsEnvironments(config: NonNullable<SessionView["config"]>): boolean {
  const features = config.features;
  return isRecord(features) && Boolean(features.environments);
}
