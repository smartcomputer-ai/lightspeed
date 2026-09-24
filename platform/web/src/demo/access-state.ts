import type {
  AccessInput,
  AccessPolicyView,
  ExecutionInput,
  ResourceAccessSummary,
  ResourceRef,
} from "@lightspeed-ai/agent-client";
import type { DemoStore, UniverseState } from "./store";

const states = new WeakMap<
  UniverseState,
  {
    policies: Map<string, AccessPolicyView>;
    personal: boolean;
  }
>();
export const resourceKey = (r: ResourceRef) => `${r.kind}:${r.id}`;
/// Workspaces, environments and MCP servers: roots that run nothing and
/// carry one grant, `use`.
export const isOperational = (kind: ResourceRef["kind"]) =>
  kind === "workspace" || kind === "environment" || kind === "mcp_server";
/// The universe's execution service, which people see as the default agent
/// identity. It holds the system `executor` role.
export const executionPrincipal = (universe: UniverseState) =>
  `execution-${universe.universe.id}`;
export const DEFAULT_AGENT_IDENTITY = "Default agent identity";
export function accessState(universe: UniverseState) {
  let state = states.get(universe);
  if (!state) {
    state = {
      policies: new Map(),
      personal: false,
    };
    states.set(universe, state);
  }
  return state;
}
export function demoPolicy(
  store: DemoStore,
  universe: UniverseState,
  resource: ResourceRef,
): AccessPolicyView {
  const state = accessState(universe);
  let root: ResourceRef | undefined;
  if (resource.kind === "session") {
    const session = universe.sessions.get(resource.id);
    const parent = session?.view.origin?.parentSessionId;
    if (parent)
      root = demoPolicy(store, universe, { kind: "session", id: parent }).root;
    else {
      const bot = [...universe.bots.keys()].find(
        (id) =>
          resource.id === `bot:v1:${id}` ||
          resource.id.startsWith(`bot:v1:${id}:`),
      );
      if (bot)
        root = demoPolicy(store, universe, { kind: "bot", id: bot }).root;
    }
  }
  root ??= resource;
  const key = resourceKey(root);
  let policy = state.policies.get(key);
  if (!policy) {
    policy = {
      resource: root,
      root,
      owner: store.currentUser.id,
      visibility: "universe",
      grants: [],
      revision: 1,
      ...(root.kind === "session" || root.kind === "bot"
        ? {
            execution: {
              kind: "service" as const,
              runAs: executionPrincipal(universe),
            },
          }
        : {}),
      updatedBy: { kind: "principal", id: store.currentUser.id },
      updatedAtMs: Date.now(),
    };
    state.policies.set(key, policy);
  }
  return { ...policy, resource };
}
export function demoSummary(
  store: DemoStore,
  universe: UniverseState,
  resource: ResourceRef,
): ResourceAccessSummary {
  const { root, owner, visibility, execution } = demoPolicy(
    store,
    universe,
    resource,
  );
  return { root, owner, visibility, execution };
}
export function demoCreateAccess(
  store: DemoStore,
  universe: UniverseState,
  resource: ResourceRef,
  access?: AccessInput,
  execution?: ExecutionInput,
) {
  const policy = demoPolicy(store, universe, resource);
  policy.visibility =
    access?.visibility ??
    (execution?.kind === "personal" ? "restricted" : "universe");
  if (!isOperational(resource.kind))
    policy.execution = {
      kind: execution?.kind ?? "service",
      runAs:
        execution?.kind === "personal"
          ? store.currentUser.id
          : executionPrincipal(universe),
    };
  policy.grants = (access?.grants ?? []).map((g) => ({
    ...g,
    grantedAtMs: Date.now(),
    grantedBy: store.currentUser.id,
  }));
  accessState(universe).policies.set(resourceKey(policy.root), policy);
}
const isAdmin = (universe: UniverseState) => universe.universe.role === "admin";

export function demoOrdinaryRead(
  store: DemoStore,
  universe: UniverseState,
  policy: AccessPolicyView,
) {
  return (
    policy.visibility === "universe" ||
    policy.owner === store.currentUser.id ||
    policy.grants.some(
      (g) =>
        g.subject.kind === "principal" && g.subject.id === store.currentUser.id,
    ) ||
    // Admins see every workspace, environment and MCP server.
    (isOperational(policy.root.kind) && isAdmin(universe))
  );
}
export function demoPrivilegedRead(
  store: DemoStore,
  universe: UniverseState,
  policy: AccessPolicyView,
) {
  return (
    !isOperational(policy.root.kind) &&
    !demoOrdinaryRead(store, universe, policy) &&
    universe.members.some(
      (member) =>
        member.userId === store.currentUser.id &&
        member.readPrivateContent === true,
    )
  );
}
export function demoCanRead(
  store: DemoStore,
  universe: UniverseState,
  policy: AccessPolicyView,
) {
  return (
    demoOrdinaryRead(store, universe, policy) ||
    demoPrivilegedRead(store, universe, policy)
  );
}
/// On sessions and bots the owner and writers share; on workspaces,
/// environments and MCP servers only the owner or an Admin.
export function demoCanShare(
  store: DemoStore,
  universe: UniverseState,
  policy: AccessPolicyView,
) {
  if (isOperational(policy.root.kind))
    return policy.owner === store.currentUser.id || isAdmin(universe);
  return (
    policy.owner === store.currentUser.id ||
    policy.grants.some(
      (g) =>
        g.subject.kind === "principal" &&
        g.subject.id === store.currentUser.id &&
        g.permission === "write",
    )
  );
}
/// Whether `principal`, holding `role` in the universe, may use an
/// operational resource: an eligible role, and universe visibility,
/// ownership, a `use` grant or Admin.
export function demoCanUse(
  policy: AccessPolicyView,
  principal: string,
  role: string | null | undefined,
) {
  if (!role || !["contributor", "operator", "admin", "executor"].includes(role))
    return false;
  return (
    policy.visibility === "universe" ||
    policy.owner === principal ||
    role === "admin" ||
    policy.grants.some(
      (g) =>
        g.subject.kind === "principal" &&
        g.subject.id === principal &&
        g.permission === "use",
    )
  );
}
