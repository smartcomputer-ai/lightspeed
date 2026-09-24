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
import type { DemoStore, UniverseState } from "../store";
import {
  accessState,
  DEFAULT_AGENT_IDENTITY,
  demoCanRead,
  demoCanUse,
  demoPrivilegedRead,
  demoCanShare,
  demoCreateAccess,
  demoPolicy,
  demoSummary,
  executionPrincipal,
  isOperational,
  resourceKey,
} from "../access-state";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

/// Path segments of the operational lists, and the record field naming each item.
const OPERATIONAL = {
  workspaces: { kind: "workspace", field: "workspaceId" },
  environments: { kind: "environment", field: "environmentId" },
  "mcp-servers": { kind: "mcp_server", field: "serverId" },
} as const;
type OperationalSegment = keyof typeof OPERATIONAL;
/// Sub-paths of the lists that are not record ids.
const NOT_IDS = new Set(["external", "hints", "discover-auth"]);
/// Record responses that carry the record itself (or `{ workspace }`).
const RECORD_SUFFIXES = new Set([undefined, "/tree", "/power", "/idle-policy", "/ingress"]);

function exists(universe: UniverseState, resource: ResourceRef) {
  switch (resource.kind) {
    case "session":
      return universe.sessions.has(resource.id);
    case "bot":
      return universe.bots.has(resource.id);
    case "profile":
      return universe.profiles.has(resource.id);
    case "workspace":
      return universe.workspaces.has(resource.id);
    case "environment":
      return universe.environments.has(resource.id);
    case "mcp_server":
      return universe.mcpServers.has(resource.id);
  }
}

export function accessRoutes(store: DemoStore): Hono {
  const app = new Hono();
  // Add the same summaries as the runtime to existing demo read surfaces.
  app.use("/:id/*", async (c, next) => {
    const universe = universeFor(store, c);
    if (!universe) return next();
    const operationalPath = c.req.path.match(
      /\/universes\/[^/]+\/(workspaces|environments|mcp-servers)(?:\/([^/]+))?(\/.*)?$/,
    );
    const segment = operationalPath?.[1] as OperationalSegment | undefined;
    const operational = segment ? OPERATIONAL[segment] : undefined;
    const operationalId =
      operationalPath?.[2] && !NOT_IDS.has(operationalPath[2])
        ? decodeURIComponent(operationalPath[2])
        : undefined;
    const operationalRecord =
      operational && RECORD_SUFFIXES.has(operationalPath?.[3]);
    const creation =
      c.req.method === "POST" &&
      /\/(sessions|bots|workspaces|environments|environments\/external|mcp-servers)$/.test(
        c.req.path,
      );
    const body = creation
      ? ((await c.req.raw
          .clone()
          .json()
          .catch(() => undefined)) as {
          access?: AccessInput;
          execution?: ExecutionInput;
        })
      : undefined;
    if (body?.execution?.kind === "personal" && !accessState(universe).personal)
      return c.json({ error: "Personal execution is disabled" }, 403);
    const target = c.req.path.match(/\/(sessions|bots)\/([^/]+)/);
    if (c.req.method === "GET" && target) {
      const kind = target[1] === "sessions" ? "session" : "bot";
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
    if (c.req.method === "GET" && operational && operationalId) {
      const resource = { kind: operational.kind, id: operationalId };
      if (
        exists(universe, resource) &&
        !demoCanRead(store, universe, demoPolicy(store, universe, resource))
      )
        return notFound(c);
    }
    await next();
    if (
      !c.res.ok ||
      !c.res.headers.get("content-type")?.includes("application/json")
    )
      return;
    let value = await c.res.clone().json();
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
    if (operational && operationalRecord) {
      const { kind, field } = operational;
      const decorate = (record: Record<string, unknown>) => {
        const id = record[field];
        if (typeof id !== "string") return record;
        return { ...record, access: demoSummary(store, universe, { kind, id }) };
      };
      if (Array.isArray(value)) {
        // Lists carry only what the caller may see.
        value = value
          .filter((record: Record<string, unknown>) => {
            const id = record[field];
            return (
              typeof id !== "string" ||
              demoCanRead(store, universe, demoPolicy(store, universe, { kind, id }))
            );
          })
          .map(decorate);
      } else {
        if (creation && typeof value[field] === "string")
          demoCreateAccess(
            store,
            universe,
            { kind, id: value[field] },
            body?.access,
          );
        value = value.workspace
          ? { ...value, workspace: decorate(value.workspace) }
          : decorate(value);
      }
    }
    if (c.req.method === "GET" || c.req.path.endsWith("/access/policy/read")) {
      const resources: ResourceRef[] = [];
      if (target)
        resources.push({
          kind: target[1] === "sessions" ? "session" : "bot",
          id: decodeURIComponent(target[2]!),
        });
      if (value.policy?.resource) resources.push(value.policy.resource);
      for (const session of value.sessions ?? [])
        resources.push({ kind: "session", id: session.id });
      for (const bot of value.bots ?? [])
        resources.push({ kind: "bot", id: bot.botId });
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
        "create_workspace",
        "use_resource",
      );
    if (configure) actions.push("configure_resource");
    if (role === "admin") actions.push("manage_access");
    // `as: execution_service` decides the caller's visible resources for the
    // default agent identity instead.
    const service = body.as === "execution_service";
    const response: AccessReadResponse = {
      actions,
      resources: (body.resources ?? []).map((resource) => {
        const policy = demoPolicy(store, universe, resource);
        if (!exists(universe, resource) || !demoCanRead(store, universe, policy))
          return { resource, actions: [] };
        if (isOperational(resource.kind)) {
          const allowed: UniverseAction[] = ["read"];
          const [principal, principalRole] = service
            ? [executionPrincipal(universe), "executor"]
            : [store.currentUser.id, role];
          if (demoCanUse(policy, principal, principalRole))
            allowed.push("use_resource");
          if (service) return { resource, actions: allowed };
          const owner = policy.owner === store.currentUser.id;
          if (owner || role === "admin" || (configure && policy.visibility === "universe"))
            allowed.push("configure_resource");
          if (demoCanShare(store, universe, policy)) allowed.push("share_resource");
          return { resource, actions: allowed };
        }
        if (service) return { resource, actions: [] };
        const allowed: UniverseAction[] = ["read"];
        if (
          resource.kind !== "profile" &&
          contribute &&
          demoCanShare(store, universe, policy)
        )
          allowed.push("share_resource");
        if (contribute && demoCanShare(store, universe, policy)) {
          if (resource.kind === "session")
            allowed.push("control_session", "stop_session", "delete_session");
          if (resource.kind === "bot") allowed.push("manage_bot", "invoke_bot");
          if (resource.kind === "profile") allowed.push("manage_profile");
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
    const agent = executionPrincipal(universe);
    return c.json({
      principalId: store.currentUser.id,
      subjects: [
        ...universe.members.map((m) => ({ id: m.userId, name: m.name ?? m.userId })),
        { id: agent, name: DEFAULT_AGENT_IDENTITY },
      ]
        .filter(
          (m) => m.name.toLowerCase().includes(query) || m.id === query,
        )
        .map((m) => ({
          subject: { kind: "principal", id: m.id },
          displayName: m.name,
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
    if (!demoCanShare(store, universe, policy))
      return c.json({ error: "Forbidden" }, 403);
    if (body.expectedRevision !== policy.revision)
      return conflict(c, "Access revision conflict");
    const operational = isOperational(policy.root.kind);
    if (
      (body.grants ?? []).some((g) =>
        operational ? g.permission !== "use" : g.permission === "use",
      )
    )
      return badRequest(
        c,
        operational
          ? "Workspaces, environments and MCP servers take only `use` grants"
          : "Sessions and bots take `read` or `write` grants",
      );
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
        executionPrincipalId: executionPrincipal(universe),
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
        executionPrincipalId: executionPrincipal(universe),
        personalExecutionEnabled: accessState(universe).personal,
      },
    });
  });
  return app;
}
