/// Platform-owned records: universes, memberships, the user directory, and
/// universe API keys. The demo user is a platform admin, so every gate the
/// real server applies passes.
import { Hono } from "hono";
import { effectiveFeatures, featureOverridesSchema, keyIdConflictMessage, memberUpdateSchema, mergeFeatureOverrides, universeHandleFromName, universeRoleSchema, universeHandleSchema, universeUpdateSchema } from "@lightspeed-ai/platform-shared";
import type { MethodGroup } from "@lightspeed-ai/sdk";
import type { EngineUniverse, Member, Universe } from "@/api";
import { universeApiKey, type DemoStore, type UniverseState } from "../store";
import { conflict, badRequest, notFound, nowIso, readBody, universeFor } from "./common";

export function platformRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/me", (c) => c.json({ user: store.currentUser }));

  app.get("/users", (c) =>
    c.json([...store.users.values()].map(({ id, name, email }) => ({ id, name, email }))),
  );

  app.get("/universes", (c) => c.json([...store.universes.values()].map((state) => state.universe)));

  app.post("/universes", async (c) => {
    const body = await readBody<{ name?: string; handle?: string }>(c);
    const name = body.name?.trim();
    if (!name) return badRequest(c, "validation failed — name: required");
    const handle = body.handle?.trim() || universeHandleFromName(name);
    if (!universeHandleSchema.safeParse(handle).success) return badRequest(c, "invalid universe handle");
    if (store.universeByHandle(handle) || store.orphanEngineUniverses.some(o => o.handle === handle)) return conflict(c, "universe handle is already in use");
    const state = store.addUniverse({ handle, name, role: "admin" });
    return c.json(state.universe, 201);
  });

  /// Admin reconciliation: every platform row against the engine inventory,
  /// plus engine universes nothing links to.
  app.get("/universes/reconcile", (c) =>
    c.json({
      platform: [...store.universes.values()].map((state) => ({
        id: state.universe.id,
        lightspeedUniverseId: state.universe.lightspeedUniverseId,
        engine: state.universe.gatewayUrl ? "unchecked" : "ok",
      })),
      orphans: store.orphanEngineUniverses,
    }),
  );

  // Demo runtime and Platform share one in-memory inventory, so its cache
  // is already current; the explicit action still mirrors the real API.
  app.post("/universes/sync-handles", c => c.json({ updated: 0, skipped: 0 }));

  app.post("/universes/adopt", async (c) => {
    const body = await readBody<{ lightspeedUniverseId?: string; name?: string; handle?: string }>(c);
    // As in the real API, adoption keeps the universe's handle and never names it.
    if (body.handle !== undefined) return badRequest(c, "validation failed — handle: not accepted");
    const engineId = body.lightspeedUniverseId?.trim();
    const name = body.name?.trim();
    if (!engineId || !name) return badRequest(c, "validation failed — lightspeedUniverseId and name are required");
    if ([...store.universes.values()].some((s) => s.universe.lightspeedUniverseId === engineId)) {
      return conflict(c, "already linked to a universe");
    }
    const orphanIndex = store.orphanEngineUniverses.findIndex((o) => o.universeId === engineId);
    if (orphanIndex < 0) return notFound(c, "engine universe not found");
    const orphan = store.orphanEngineUniverses[orphanIndex] as EngineUniverse;
    const handle = orphan.handle;
    if (store.universeByHandle(handle)) return conflict(c, "universe handle is already in use");
    store.orphanEngineUniverses.splice(orphanIndex, 1);
    const state = store.addUniverse({
      handle,
      name,
      lightspeedUniverseId: engineId,
      role: "admin",
      createdAt: new Date(orphan.createdAtMs).toISOString(),
    });
    return c.json(state.universe, 201);
  });

  app.delete("/universes/engine/:lightspeedUniverseId", (c) => {
    const engineId = c.req.param("lightspeedUniverseId");
    if ([...store.universes.values()].some((s) => s.universe.lightspeedUniverseId === engineId)) {
      return conflict(c, "linked to a universe — archive and delete it instead");
    }
    const index = store.orphanEngineUniverses.findIndex((o) => o.universeId === engineId);
    if (index < 0) return notFound(c);
    const [orphan] = store.orphanEngineUniverses.splice(index, 1);
    return c.json({
      ok: true,
      purge: { universeId: engineId, sessions: orphan?.sessions ?? 0, blobBytes: orphan?.blobBytes ?? 0 },
    });
  });

  app.post("/universes/:id/engine", (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    return c.json({ created: false });
  });

  app.get("/universes/:id", (c) => {
    const state = universeFor(store, c);
    return state ? c.json(state.universe) : notFound(c);
  });

  app.put("/universes/:id/handle", async c => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = await readBody<{ handle?: string }>(c);
    const parsed = universeHandleSchema.safeParse(body.handle);
    if (!parsed.success) return badRequest(c, "invalid universe handle");
    const handle = parsed.data;
    const existing = store.universeByHandle(handle);
    if ((existing && existing !== state) || store.orphanEngineUniverses.some(orphan => orphan.handle === handle)) return conflict(c, "universe handle is already in use");
    state.universe.handle = handle;
    return c.json(state.universe);
  });

  app.patch("/universes/:id", async (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = await readBody<Partial<Pick<Universe, "name" | "status" | "gatewayUrl"> & { features: unknown }>>(c);
    const parsed = universeUpdateSchema.safeParse(body);
    if (!parsed.success) return badRequest(c, "invalid universe settings");
    if (parsed.data.icon !== undefined) state.universe.icon = parsed.data.icon;
    if (parsed.data.iconColor !== undefined) state.universe.iconColor = parsed.data.iconColor;
    if (body.features !== undefined) {
      const features = featureOverridesSchema.safeParse(body.features);
      if (!features.success) return c.json({ error: "unknown feature" }, 400);
      state.featureOverrides = mergeFeatureOverrides(state.featureOverrides, features.data);
      state.universe.features = effectiveFeatures(state.featureOverrides);
    }
    if (typeof body.name === "string" && body.name.trim()) state.universe.name = body.name.trim();
    if (body.status === "active" || body.status === "archived") state.universe.status = body.status;
    if (body.gatewayUrl !== undefined) state.universe.gatewayUrl = body.gatewayUrl || null;
    return c.json(state.universe);
  });

  app.delete("/universes/:id", (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    if (state.universe.status !== "archived") {
      return conflict(c, "archive the universe before deleting it");
    }
    store.universes.delete(state.universe.id);
    return c.json({ ok: true, purge: { universeId: state.universe.lightspeedUniverseId } });
  });

  // --- membership ---------------------------------------------------------

  /// Every member may list members; emails are for admins only.
  app.get("/universes/:id/members", (c) => {
    const state = universeFor(store, c);
    if (!state || !state.universe.role) return notFound(c);
    if (state.universe.role === "admin") return c.json(state.members);
    return c.json(state.members.map(({ email: _email, ...member }) => member));
  });

  app.post("/universes/:id/members", async (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = await readBody<{ userId?: string; email?: string; role?: string }>(c);
    const role = universeRoleSchema.safeParse(body.role ?? "contributor");
    if (!role.success) return badRequest(c, "invalid role");
    const target = body.userId
      ? store.users.get(body.userId)
      : [...store.users.values()].find((u) => u.email === body.email?.trim());
    if (!target) return notFound(c, "user not found");
    if (state.members.some((m) => m.userId === target.id)) return conflict(c, "already a member");
    const created: Member = {
      id: store.nextId("member"),
      userId: target.id,
      role: role.data,
      email: target.email,
      name: target.name,
      createdAt: nowIso(),
    };
    state.members.push(created);
    return c.json(created, 201);
  });

  app.patch("/universes/:id/members/:memberId", async (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = memberUpdateSchema.safeParse(await readBody(c));
    if (!body.success) return badRequest(c, "invalid role");
    const member = state.members.find((m) => m.id === c.req.param("memberId"));
    if (!member) return notFound(c);
    if (member.role === "admin" && body.data.role !== "admin" && !state.members.some((m) => m.id !== member.id && m.role === "admin")) {
      return conflict(c, "a universe keeps at least one admin");
    }
    member.role = body.data.role;
    if (member.userId === store.currentUser.id) state.universe.role = member.role;
    return c.json(member);
  });

  app.delete("/universes/:id/members/:memberId", (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const member = state.members.find((m) => m.id === c.req.param("memberId"));
    if (member?.role === "admin" && !state.members.some((m) => m.id !== member.id && m.role === "admin")) {
      return conflict(c, "a universe keeps at least one admin");
    }
    const before = state.members.length;
    state.members = state.members.filter((m) => m.id !== c.req.param("memberId"));
    return state.members.length === before ? notFound(c) : c.json({ ok: true });
  });

  // --- API keys -----------------------------------------------------------

  app.get("/universes/:id/api-keys", (c) => {
    const state = universeFor(store, c);
    return state ? c.json(state.apiKeys) : notFound(c);
  });

  app.post("/universes/:id/api-keys", async (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = await readBody<{ displayName?: string; groups?: MethodGroup[]; keyId?: string }>(c);
    const displayName = body.displayName?.trim();
    if (!displayName) return badRequest(c, "validation failed — displayName: required");
    if (!body.groups?.length) return badRequest(c, "validation failed — groups: required");
    // A chosen id is taken while any key of the universe holds it, revoked or not.
    const keyId = body.keyId?.trim() || undefined;
    const holder = state.apiKeys.find((key) => key.keyId === keyId);
    if (keyId && holder) return conflict(c, keyIdConflictMessage(keyId, holder.revokedAtMs != null));
    const secret = `lsk_${randomHex(24)}`;
    const apiKey = universeApiKey(state, {
      keyId,
      keyPrefix: secret.slice(0, 12),
      displayName,
      groups: body.groups,
      createdAtMs: Date.now(),
      createdBy: store.currentUser.id,
    });
    state.apiKeys.push(apiKey);
    return c.json({ apiKey, secret }, 201);
  });

  app.post("/universes/:id/api-keys/:keyId/rotate", (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const apiKey = state.apiKeys.find((key) => key.keyId === c.req.param("keyId") && key.revokedAtMs == null);
    if (!apiKey) return notFound(c);
    if (apiKey.assertActor) return c.json({ error: "Rotate this key from Platform admin" }, 403);
    const secret = `lsk_${randomHex(24)}`;
    apiKey.keyPrefix = secret.slice(0, 12);
    apiKey.lastUsedAtMs = null;
    return c.json({ apiKey, secret });
  });

  app.delete("/universes/:id/api-keys/:keyId", (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const apiKey = state.apiKeys.find((k) => k.keyId === c.req.param("keyId"));
    if (!apiKey) return notFound(c);
    apiKey.revokedAtMs ??= Date.now();
    return c.json(apiKey);
  });

  return app;
}

function randomHex(bytes: number): string {
  const buffer = new Uint8Array(bytes);
  crypto.getRandomValues(buffer);
  return [...buffer].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function universeOrNull(store: DemoStore, id: string): UniverseState | null {
  return store.universe(id);
}
