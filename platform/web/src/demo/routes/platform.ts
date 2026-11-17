/// Platform-owned records: universes, memberships, the user directory, and
/// configuration. The demo user is a platform admin, so every gate the
/// real server applies passes.
import { Hono } from "hono";
import { effectiveFeatures, featureOverridesSchema, mergeFeatureOverrides, organizationUniverseAdoptSchema, universeHandleFromName, universeHandleSchema, universeUpdateSchema } from "@lightspeed-ai/platform-shared";
import { z } from "zod";
import type { EngineUniverse, Universe } from "@/api";
import { type DemoStore, type UniverseState } from "../store";
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
    const state = store.addUniverse({ handle, name, role: "owner" });
    return c.json(state.universe, 201);
  });

  /// Admin reconciliation: every platform row against the engine inventory,
  /// plus engine universes nothing links to.
  app.get("/universes/reconcile", (c) => {
    if (store.currentUser.role !== "admin") return c.json({ error: "platform admin required" }, 403);
    const organizationId = c.req.query("organizationId");
    if (organizationId === "") return badRequest(c, "organizationId must not be empty");
    return c.json({
      platform: [...store.universes.values()].filter(state => organizationId === undefined || state.universe.organizationId === organizationId).map((state) => ({
        id: state.universe.id,
        organizationId: state.universe.organizationId,
        lightspeedUniverseId: state.universe.lightspeedUniverseId,
        engine: "ok",
      })),
      orphans: organizationId === undefined ? store.orphanEngineUniverses : [],
    });
  });

  // Demo runtime and Platform share one in-memory inventory, so its cache
  // is already current; the explicit action still mirrors the real API.
  app.post("/universes/sync-handles", async c => {
    if (store.currentUser.role !== "admin") return c.json({ error: "platform admin required" }, 403);
    const body = await readBody<{ organizationId?: unknown }>(c);
    if (body.organizationId !== undefined && (typeof body.organizationId !== "string" || !body.organizationId.length)) return badRequest(c, "invalid organizationId");
    return c.json({ updated: 0, skipped: 0 });
  });

  app.post("/universes/adopt", async (c) => {
    if (store.currentUser.role !== "admin") return c.json({ error: "platform admin required" }, 403);
    const parsed = organizationUniverseAdoptSchema.extend({ organizationId: z.string().min(1) }).safeParse(await readBody(c));
    if (!parsed.success) return badRequest(c, "Invalid universe adoption");
    const { organizationId, lightspeedUniverseId: engineId, name, initialOwner } = parsed.data;
    const organization = store.organizations.get(organizationId);
    if (!organization) return notFound(c, "Organization not found");
    const ownerUser = store.users.get(initialOwner.id);
    const ownerServiceUser = store.serviceUsers.get(initialOwner.id);
    if (initialOwner.kind === "user"
      ? !ownerUser || ownerUser.banned || !store.organizationMemberships.get(organizationId)?.has(initialOwner.id)
      : !ownerServiceUser?.enabled || ownerServiceUser.organizationId !== organizationId) return badRequest(c, "Owner must be an active organization principal");
    if ([...store.universes.values()].some((s) => s.universe.lightspeedUniverseId === engineId)) {
      return conflict(c, "already linked to a universe");
    }
    const orphanIndex = store.orphanEngineUniverses.findIndex((o) => o.universeId === engineId);
    if (orphanIndex < 0) return notFound(c, "engine universe not found");
    const orphan = store.orphanEngineUniverses[orphanIndex] as EngineUniverse;
    const handle = orphan.handle;
    if (store.universeByHandle(handle)) return conflict(c, "universe handle is already in use");
    if ([...store.universes.values()].filter(state => state.universe.organizationId === organizationId).length >= organization.universeQuota) return conflict(c, "Organization universe quota reached");
    store.orphanEngineUniverses.splice(orphanIndex, 1);
    const state = store.addUniverse({
      handle,
      name,
      lightspeedUniverseId: engineId,
      role: "owner",
    });
    const protectedValue = orphan.protected || organization.requireProtected;
    Object.assign(state.universe, { organizationId, createdBy: store.currentUser.id, protected: protectedValue, runtimeConfiguration: { ...orphan, protected: protectedValue } });
    const grant = { id: store.nextId("grant"), universeId: state.universe.id, principal: initialOwner, name: initialOwner.kind === "user" ? ownerUser!.name : ownerServiceUser!.name, role: "owner" as const, createdAt: nowIso() };
    state.members = initialOwner.kind === "user" ? [{ id: grant.id, userId: initialOwner.id, name: grant.name, role: grant.role, createdAt: grant.createdAt }] : [];
    if (initialOwner.kind === "serviceUser") store.serviceUserGrants.set(state.universe.id, [grant]);
    store.auditEvents.push({ id: crypto.randomUUID(), createdAt: nowIso(), actorId: store.currentUser.id, targetId: engineId, universeId: state.universe.id, action: "universe.adopt", outcome: "success", details: { organizationId } });
    return c.json(state.universe, 201);
  });

  app.delete("/universes/engine/:lightspeedUniverseId", async (c) => {
    const engineId = c.req.param("lightspeedUniverseId");
    if ([...store.universes.values()].some((s) => s.universe.lightspeedUniverseId === engineId)) {
      return conflict(c, "linked to a universe — delete it through its Platform entry");
    }
    const index = store.orphanEngineUniverses.findIndex((o) => o.universeId === engineId);
    if (index < 0) return notFound(c);
    const body = await readBody<{ handle?: string }>(c);
    if (body.handle !== store.orphanEngineUniverses[index]!.handle) return badRequest(c, "type the universe handle to confirm deletion");
    store.auditEvents.push({ id: crypto.randomUUID(), createdAt: nowIso(), actorId: store.currentUser.id, targetId: engineId, universeId: engineId, action: "universe.delete", outcome: "success", details: {} });
    store.orphanEngineUniverses.splice(index, 1);
    return c.json({
      ok: true,
      purge: { universeId: engineId, state: "deleted" },
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
    const body = await readBody<Partial<Pick<Universe, "name" | "status"> & { features: unknown }>>(c);
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
    return c.json(state.universe);
  });

  app.delete("/universes/:id", async (c) => {
    const state = universeFor(store, c);
    if (!state) return notFound(c);
    const body = await readBody<{ handle?: string }>(c);
    if (body.handle !== state.universe.handle) return badRequest(c, "type the universe handle to confirm deletion");
    store.auditEvents.push({ id: crypto.randomUUID(), createdAt: nowIso(), actorId: store.currentUser.id, targetId: state.universe.lightspeedUniverseId, universeId: state.universe.id, action: "universe.delete", outcome: "success", details: {} });
    if (state.universe.protected) return conflict(c, "Universe is protected");
    store.universes.delete(state.universe.id);
    return c.json({ ok: true, purge: { universeId: state.universe.lightspeedUniverseId, state: "deleted" } });
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
