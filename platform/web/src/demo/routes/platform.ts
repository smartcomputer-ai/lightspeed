/// Platform-owned records: universes, memberships, the user directory, and
/// universe API keys. The demo user is a platform admin, so every gate the
/// real server applies passes.
import { Hono } from "hono";
import { effectiveFeatures, featureOverridesSchema, memberUpdateSchema, mergeFeatureOverrides, slugify, universeRoleSchema } from "@lightspeed/platform-shared";
import type { EngineUniverse, Member, Universe, UniverseApiKey } from "@/api";
import type { DemoStore, UniverseState } from "../store";
import { conflict, badRequest, notFound, nowIso, readBody, universeFor } from "./common";

export function platformRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/me", (c) => c.json({ user: store.currentUser }));

  app.get("/users", (c) =>
    c.json([...store.users.values()].map(({ id, name, email }) => ({ id, name, email }))),
  );

  app.get("/universes", (c) => c.json([...store.universes.values()].map((state) => state.universe)));

  app.post("/universes", async (c) => {
    const body = await readBody<{ name?: string; slug?: string }>(c);
    const name = body.name?.trim();
    if (!name) return badRequest(c, "validation failed — name: required");
    const base = body.slug?.trim() || slugify(name);
    let slug = base;
    for (let i = 2; store.universeBySlug(slug); i++) slug = `${base}-${i}`;
    const state = store.addUniverse({ slug, name, role: "admin" });
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

  app.post("/universes/adopt", async (c) => {
    const body = await readBody<{ lightspeedUniverseId?: string; name?: string }>(c);
    const engineId = body.lightspeedUniverseId?.trim();
    const name = body.name?.trim();
    if (!engineId || !name) return badRequest(c, "validation failed — lightspeedUniverseId and name are required");
    if ([...store.universes.values()].some((s) => s.universe.lightspeedUniverseId === engineId)) {
      return conflict(c, "already linked to a universe");
    }
    const orphanIndex = store.orphanEngineUniverses.findIndex((o) => o.universeId === engineId);
    if (orphanIndex < 0) return notFound(c, "engine universe not found");
    const orphan = store.orphanEngineUniverses.splice(orphanIndex, 1)[0] as EngineUniverse;
    const state = store.addUniverse({
      slug: slugify(name),
      name,
      lightspeedUniverseId: engineId,
      role: null,
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

  app.patch("/universes/:id", async (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = await readBody<Partial<Pick<Universe, "name" | "status" | "gatewayUrl"> & { features: unknown }>>(c);
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
    const body = await readBody<{ displayName?: string }>(c);
    const displayName = body.displayName?.trim();
    if (!displayName) return badRequest(c, "validation failed — displayName: required");
    const secret = `lsk_${randomHex(24)}`;
    const apiKey: UniverseApiKey = {
      keyPrefix: secret.slice(0, 12),
      displayName,
      createdAtMs: Date.now(),
      revokedAtMs: null,
      lastUsedAtMs: null,
    };
    state.apiKeys.push(apiKey);
    return c.json({ apiKey, secret }, 201);
  });

  app.delete("/universes/:id/api-keys/:keyPrefix", (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const apiKey = state.apiKeys.find((k) => k.keyPrefix === c.req.param("keyPrefix"));
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
