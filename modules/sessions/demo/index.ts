import { Hono } from "hono";
import type { ProfileInstructions, ProfileSessionRetention } from "@lightspeed-ai/sdk";
import type { ProfileSource } from "@lightspeed-ai/platform-web/modules/client";
import {
  badRequest, conflict, demoCanReadSession, demoUniverseActions, intQuery, isRecord, newSession,
  notFound, readBody, resolveSessionEnvironment, sessionConfig, sessionSummary, universeFor,
  type DemoStore, type UniverseState,
} from "@lightspeed-ai/platform-web/demo";
import { sessionCreateSchema, sessionListQuery } from "../api/collection";

/// What a session start consumes from a profile, whichever source it came
/// from. `profileId` is null for inline profiles.
interface ResolvedProfile {
  profileId: string | null;
  metadata: Record<string, string>;
  retention: ProfileSessionRetention | null;
  config: Record<string, unknown>;
  instructions: ProfileInstructions | null;
}

export default function sessionCollectionRoutes(store: DemoStore): Hono {
  const app = new Hono();

  /// Newest activity first; the cursor is a plain offset because the demo
  /// list is small and never changes underneath a page.
  app.get("/", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const { limit, closed, lifecycleControlled, subagent, parent, metadata } = sessionListQuery(c.req.url);
    const offset = intQuery(c, "cursor", 0);
    // The Sessions page's list (not a lineage lookup) counts as a look.
    if (!parent && universe.onSessionsViewed) {
      const viewed = universe.onSessionsViewed;
      universe.onSessionsViewed = undefined;
      viewed();
    }
    const actions = demoUniverseActions(store, universe);
    const all = [...universe.sessions.values()]
      .filter(record => demoCanReadSession(store, universe, record, actions))
      .filter((record) => !parent || record.view.origin?.parentSessionId === parent)
      .filter((record) => closed === undefined || (record.view.lifecycleStatus === "closed") === closed)
      .filter((record) => subagent === undefined || Boolean(record.view.origin) === subagent)
      // Lifecycle-controlled work follows its root, as the core list does.
      .filter((record) => {
        if (lifecycleControlled === undefined) return true;
        const root = universe.sessions.get(record.view.origin?.rootSessionId ?? record.view.id) ?? record;
        return root.view.lifecycleControlled === lifecycleControlled;
      })
      .filter((record) =>
        Object.entries(metadata ?? {}).every(([key, value]) => value
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

  /// Resolve the default attachment before creating the session so a refused
  /// profile leaves no session behind.
  app.post("/", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const parsed = sessionCreateSchema.safeParse(await readBody(c));
    if (!parsed.success) return badRequest(c, "invalid session creation request");
    const body = parsed.data;
    const actions = demoUniverseActions(store, universe);
    if (!actions.has("createSession")) return c.json({ error: "universe permission required: createSession" }, 403);
    if (!actions.has("configureSession") && body.access?.visibility === "universe") {
      return c.json({ error: "universe permission required to start shared work: configureSession" }, 403);
    }
    if (!actions.has("setSessionRetention") && body.deleteAfterCloseMs != null) return c.json({ error: "universe permission required for deletion retention: setSessionRetention" }, 403);
    const profile = resolveProfile(universe, body.profile);
    if (!profile) return notFound(c, "not found in engine");
    const config = sessionConfig(profile.config, universe.modelDefaults.agentRun);
    if (!config) return c.json({ error: "No default agent model is selected. Choose a model in Models.", kind: "modelDefaultUnset", modelDefaultSlot: "agentRun" }, 400);
    const sessionId = store.nextId("session");
    const resolved = resolveSessionEnvironment(universe, profile.config);
    if ("error" in resolved) return conflict(c, `engine conflict: ${resolved.error}`);
    const session = newSession(store, universe, {
      id: sessionId,
      displayName: body.displayName?.trim() || null,
      metadata: { ...profile.metadata, ...(body.metadata ?? {}) },
      deleteAfterCloseMs: !actions.has("setSessionRetention") ? null : Object.hasOwn(body, "deleteAfterCloseMs")
        ? body.deleteAfterCloseMs
        : profile.retention?.deleteAfterCloseMs,
      config,
      activeEnvironmentId: resolved.environmentId,
      instructions: instructionText(store, profile.instructions),
      access: { visibility: body.access?.visibility ?? "restricted", createdBy: { kind: "actor", id: store.currentUser.id } },
    });
    return c.json(session.view);
  });

  return app;
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
  };
}

function instructionText(store: DemoStore, instructions: ProfileInstructions | null): string | null {
  if (!instructions) return null;
  return instructions.type === "text" ? instructions.text : store.readText(instructions.blobRef);
}

function isStringMap(value: unknown): value is Record<string, string> {
  return isRecord(value) && Object.values(value).every((entry) => typeof entry === "string");
}
