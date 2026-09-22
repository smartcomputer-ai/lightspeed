import { Hono } from "hono";
import type {
  AccessReadParams,
  AccessReadResponse,
  AccessInput,
  AccessPolicyPutParams,
  ExecutionInput,
  ResourceRef,
  UniverseAction,
} from "@lightspeed-ai/agent-client";
import type { DemoStore } from "../store";
import {
  accessState,
  demoCanRead,
  demoPrivilegedRead,
  demoCanShare,
  demoCreateAccess,
  demoMembers,
  demoPolicy,
  demoSummary,
  resourceKey,
} from "../access-state";
import {
  badRequest,
  conflict,
  notFound,
  readBody,
  universeFor,
} from "./common";

export function accessRoutes(store: DemoStore): Hono {
  const app = new Hono();
  // Add the same summaries as the runtime to existing demo read surfaces.
  app.use("/:id/*", async (c, next) => {
    const universe = universeFor(store, c);
    if (!universe) return next();
    const creation =
      c.req.method === "POST" && /\/(sessions|bots)$/.test(c.req.path);
    const body = creation
      ? ((await c.req.raw
          .clone()
          .json()
          .catch(() => undefined)) as {
          access?: AccessInput;
          execution?: ExecutionInput;
        })
      : undefined;
    if (
      body?.access?.root &&
      (!accessState(universe).collections.has(body.access.root.id) ||
        !demoCanShare(store, demoPolicy(store, universe, body.access.root)))
    )
      return notFound(c);
    if (
      body?.access?.root &&
      (body.execution || body.access.visibility || body.access.grants?.length)
    )
      return badRequest(c, "A collection member inherits access and execution");
    if (body?.execution?.kind === "personal" && !accessState(universe).personal)
      return c.json({ error: "Personal execution is disabled" }, 403);
    const target = c.req.path.match(/\/(sessions|bots|collections)\/([^/]+)/);
    if (c.req.method === "GET" && target) {
      const kind =
        target[1] === "sessions"
          ? "session"
          : target[1] === "bots"
            ? "bot"
            : "collection";
      if (
        !demoCanRead(
          store,
          universe,
          demoPolicy(store, universe, {
            kind,
            id: decodeURIComponent(target[2]!),
          }),
        )
      )
        return notFound(c);
    }
    await next();
    if (
      !c.res.ok ||
      !c.res.headers.get("content-type")?.includes("application/json")
    )
      return;
    const value = await c.res.clone().json();
    if (!value || typeof value !== "object") return;
    if (
      value.id &&
      value.status &&
      /\/sessions(?:\/[^/]+)?$/.test(c.req.path)
    ) {
      const resource: ResourceRef = { kind: "session", id: value.id };
      if (creation)
        demoCreateAccess(
          store,
          universe,
          resource,
          body?.access,
          body?.execution,
        );
      value.access = demoSummary(store, universe, resource);
    }
    if (value.bot) {
      const resource: ResourceRef = { kind: "bot", id: value.bot.botId };
      if (creation)
        demoCreateAccess(
          store,
          universe,
          resource,
          body?.access,
          body?.execution,
        );
      value.access = demoSummary(store, universe, resource);
    }
    if (Array.isArray(value.sessions))
      value.sessions = value.sessions
        .map((s: { id: string }) => ({
          ...s,
          access: demoSummary(store, universe, { kind: "session", id: s.id }),
        }))
        .filter((s: { id: string }) =>
          demoCanRead(
            store,
            universe,
            demoPolicy(store, universe, { kind: "session", id: s.id }),
          ),
        );
    if (Array.isArray(value.bots))
      value.bots = value.bots
        .map((b: { botId: string }) => ({
          ...b,
          access: demoSummary(store, universe, { kind: "bot", id: b.botId }),
        }))
        .filter((b: { botId: string }) =>
          demoCanRead(
            store,
            universe,
            demoPolicy(store, universe, { kind: "bot", id: b.botId }),
          ),
        );
    if (c.req.method === "GET" || c.req.path.endsWith("/access/policy/read")) {
      const resources: ResourceRef[] = [];
      if (target)
        resources.push({
          kind:
            target[1] === "sessions"
              ? "session"
              : target[1] === "bots"
                ? "bot"
                : "collection",
          id: decodeURIComponent(target[2]!),
        });
      if (value.policy?.resource) resources.push(value.policy.resource);
      for (const session of value.sessions ?? [])
        resources.push({ kind: "session", id: session.id });
      for (const bot of value.bots ?? [])
        resources.push({ kind: "bot", id: bot.botId });
      for (const collection of value.collections ?? [])
        resources.push({ kind: "collection", id: collection.collectionId });
      if (
        resources.some((resource) =>
          demoPrivilegedRead(
            store,
            universe,
            demoPolicy(store, universe, resource),
          ),
        )
      )
        c.res.headers.set("x-lightspeed-privileged-read", "true");
    }
    c.res = new Response(JSON.stringify(value), {
      status: c.res.status,
      headers: c.res.headers,
    });
  });
  app.post("/:id/access", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const role = universe.universe.role;
    if (!role) return c.json({ error: "Universe access required" }, 403);
    const body = await readBody<AccessReadParams>(c);
    const contribute = ["contributor", "operator", "admin"].includes(role);
    const configure = ["operator", "admin"].includes(role);
    const actions: UniverseAction[] = ["read"];
    if (contribute)
      actions.push(
        "create_session",
        "create_profile",
        "create_bot",
        "create_collection",
        "use_resource",
      );
    if (configure) actions.push("configure_resource");
    if (role === "admin") actions.push("manage_access");
    const response: AccessReadResponse = {
      actions,
      resources: (body.resources ?? []).map((resource) => {
        const exists =
          resource.kind === "session"
            ? universe.sessions.has(resource.id)
            : resource.kind === "bot"
              ? universe.bots.has(resource.id)
              : resource.kind === "collection"
                ? accessState(universe).collections.has(resource.id)
                : universe.profiles.has(resource.id);
        const policy = demoPolicy(store, universe, resource);
        const allowed: UniverseAction[] =
          exists && demoCanRead(store, universe, policy) ? ["read"] : [];
        if (
          exists &&
          resource.kind !== "profile" &&
          contribute &&
          demoCanShare(store, policy)
        )
          allowed.push("share_resource");
        if (exists && contribute && demoCanShare(store, policy)) {
          if (resource.kind === "session")
            allowed.push("control_session", "stop_session", "delete_session");
          if (resource.kind === "bot") allowed.push("manage_bot", "invoke_bot");
          if (resource.kind === "profile") allowed.push("manage_profile");
          if (resource.kind === "collection")
            allowed.push(
              "control_session",
              "manage_collection",
              "delete_collection",
            );
        }
        return { resource, actions: allowed };
      }),
    };
    return c.json(response);
  });
  app.get("/:id/access/subjects", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const query = (c.req.query("q") ?? "").toLowerCase();
    return c.json({
      principalId: store.currentUser.id,
      subjects: universe.members
        .filter(
          (m) =>
            (m.name ?? m.userId).toLowerCase().includes(query) ||
            m.userId === query,
        )
        .map((m) => ({
          subject: { kind: "principal", id: m.userId },
          displayName: m.name ?? m.userId,
        }))
        .slice(0, 100),
    });
  });
  app.post("/:id/access/policy/read", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const { resource } = await readBody<{ resource: ResourceRef }>(c);
    const policy = demoPolicy(store, universe, resource);
    return demoCanRead(store, universe, policy)
      ? c.json({ policy })
      : notFound(c);
  });
  app.put("/:id/access/policy", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<AccessPolicyPutParams>(c);
    const policy = demoPolicy(store, universe, body.resource);
    if (!demoCanShare(store, policy))
      return c.json({ error: "Forbidden" }, 403);
    if (body.expectedRevision !== policy.revision)
      return conflict(c, "Access revision conflict");
    const owner = policy.owner === store.currentUser.id;
    const writers = (grants: typeof body.grants) =>
      JSON.stringify(
        (grants ?? [])
          .filter((g) => g.permission === "write")
          .map((g) => `${g.subject.kind}:${g.subject.id}`)
          .sort(),
      );
    if (
      !owner &&
      (body.owner || writers(body.grants) !== writers(policy.grants))
    )
      return c.json(
        { error: "Only the owner may change writers or ownership" },
        403,
      );
    if (body.owner && policy.execution?.kind === "personal")
      return c.json({ error: "Personal work cannot change owners" }, 403);
    const updated = {
      ...policy,
      resource: policy.root,
      owner: body.owner ?? policy.owner,
      visibility: body.visibility,
      grants: (body.grants ?? []).map((g) => ({
        ...g,
        grantedBy: store.currentUser.id,
        grantedAtMs: Date.now(),
      })),
      revision: policy.revision + 1,
    };
    accessState(universe).policies.set(resourceKey(policy.root), updated);
    return c.json({ policy: updated });
  });
  app.get("/:id/access/execution", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json({
      policy: {
        executionPrincipalId: `execution-${universe.universe.id}`,
        personalExecutionEnabled: accessState(universe).personal,
      },
    });
  });
  app.put("/:id/access/execution", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (universe.universe.role !== "admin")
      return c.json({ error: "Admin required" }, 403);
    accessState(universe).personal = (
      await readBody<{ personalExecutionEnabled: boolean }>(c)
    ).personalExecutionEnabled;
    return c.json({
      policy: {
        executionPrincipalId: `execution-${universe.universe.id}`,
        personalExecutionEnabled: accessState(universe).personal,
      },
    });
  });
  app.get("/:id/collections", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json({
      collections: [...accessState(universe).collections.values()]
        .filter((v) =>
          demoCanRead(
            store,
            universe,
            demoPolicy(store, universe, {
              kind: "collection",
              id: v.collectionId,
            }),
          ),
        )
        .map((v) => ({
          ...v,
          access: demoSummary(store, universe, {
            kind: "collection",
            id: v.collectionId,
          }),
        })),
    });
  });
  app.post("/:id/collections", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (
      !["contributor", "operator", "admin"].includes(
        universe.universe.role ?? "",
      )
    )
      return c.json({ error: "Forbidden" }, 403);
    const body = await readBody<{
      displayName: string;
      access?: AccessInput;
      execution?: ExecutionInput;
    }>(c);
    if (!body.displayName?.trim() || body.access?.root)
      return badRequest(c, "A collection needs a name and its own access");
    if (body.execution?.kind === "personal" && !accessState(universe).personal)
      return c.json({ error: "Personal execution is disabled" }, 403);
    const id = store.nextId("collection");
    demoCreateAccess(
      store,
      universe,
      { kind: "collection", id },
      body.access,
      body.execution,
    );
    const collection = {
      collectionId: id,
      displayName: body.displayName.trim(),
      revision: 1,
      createdAtMs: Date.now(),
      updatedAtMs: Date.now(),
      access: demoSummary(store, universe, { kind: "collection", id }),
    };
    accessState(universe).collections.set(id, collection);
    return c.json({ collection }, 201);
  });
  app.get("/:id/collections/:collectionId", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const id = c.req.param("collectionId");
    const collection = accessState(universe).collections.get(id);
    if (
      !collection ||
      !demoCanRead(
        store,
        universe,
        demoPolicy(store, universe, { kind: "collection", id }),
      )
    )
      return notFound(c);
    return c.json({
      collection: {
        ...collection,
        access: demoSummary(store, universe, { kind: "collection", id }),
      },
      members: demoMembers(store, universe, id),
    });
  });
  app.put("/:id/collections/:collectionId", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const id = c.req.param("collectionId");
    const collection = accessState(universe).collections.get(id);
    if (!collection) return notFound(c);
    if (
      demoPolicy(store, universe, { kind: "collection", id }).owner !==
        store.currentUser.id &&
      !(
        universe.universe.role === "admin" &&
        collection.access.visibility === "universe"
      )
    )
      return c.json({ error: "Forbidden" }, 403);
    const body = await readBody<{
      displayName: string;
      expectedRevision: number;
    }>(c);
    if (body.expectedRevision !== collection.revision)
      return conflict(c, "Collection revision conflict");
    if (!body.displayName?.trim()) return badRequest(c, "Name required");
    collection.displayName = body.displayName.trim();
    collection.revision += 1;
    return c.json({ collection });
  });
  app.delete("/:id/collections/:collectionId", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const id = c.req.param("collectionId");
    const collection = accessState(universe).collections.get(id);
    if (!collection) return notFound(c);
    if (
      demoPolicy(store, universe, { kind: "collection", id }).owner !==
        store.currentUser.id &&
      universe.universe.role !== "admin"
    )
      return c.json({ error: "Forbidden" }, 403);
    if (demoMembers(store, universe, id).length)
      return conflict(c, "Collection is not empty");
    accessState(universe).collections.delete(id);
    accessState(universe).policies.delete(`collection:${id}`);
    return c.json({ collection });
  });
  return app;
}
