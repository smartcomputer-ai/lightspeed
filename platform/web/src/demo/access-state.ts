import type {
  AccessInput,
  AccessPolicyView,
  CollectionView,
  ExecutionInput,
  ResourceAccessSummary,
  ResourceRef,
} from "@lightspeed-ai/agent-client";
import type { DemoStore, UniverseState } from "./store";

const states = new WeakMap<
  UniverseState,
  {
    policies: Map<string, AccessPolicyView>;
    roots: Map<string, ResourceRef>;
    collections: Map<string, CollectionView>;
    personal: boolean;
  }
>();
export const resourceKey = (r: ResourceRef) => `${r.kind}:${r.id}`;
export function accessState(universe: UniverseState) {
  let state = states.get(universe);
  if (!state) {
    state = {
      policies: new Map(),
      roots: new Map(),
      collections: new Map(),
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
  let root = state.roots.get(resourceKey(resource));
  if (!root && resource.kind === "session") {
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
      execution: {
        kind: "service",
        runAs: `execution-${universe.universe.id}`,
      },
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
  if (access?.root) {
    accessState(universe).roots.set(resourceKey(resource), access.root);
    return;
  }
  const policy = demoPolicy(store, universe, resource);
  policy.visibility =
    access?.visibility ??
    (execution?.kind === "personal" ? "restricted" : "universe");
  policy.execution = {
    kind: execution?.kind ?? "service",
    runAs:
      execution?.kind === "personal"
        ? store.currentUser.id
        : `execution-${universe.universe.id}`,
  };
  policy.grants = (access?.grants ?? []).map((g) => ({
    ...g,
    grantedAtMs: Date.now(),
    grantedBy: store.currentUser.id,
  }));
  accessState(universe).policies.set(resourceKey(policy.root), policy);
}
export function demoOrdinaryRead(store: DemoStore, policy: AccessPolicyView) {
  return (
    policy.visibility === "universe" ||
    policy.owner === store.currentUser.id ||
    policy.grants.some(
      (g) =>
        g.subject.kind === "principal" && g.subject.id === store.currentUser.id,
    )
  );
}
export function demoPrivilegedRead(
  store: DemoStore,
  universe: UniverseState,
  policy: AccessPolicyView,
) {
  return (
    !demoOrdinaryRead(store, policy) &&
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
    demoOrdinaryRead(store, policy) ||
    demoPrivilegedRead(store, universe, policy)
  );
}
export function demoCanShare(store: DemoStore, policy: AccessPolicyView) {
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
export function demoMembers(
  store: DemoStore,
  universe: UniverseState,
  id: string,
): ResourceRef[] {
  return [
    ...[...universe.sessions.keys()].map((id): ResourceRef => ({
      kind: "session",
      id,
    })),
    ...[...universe.bots.keys()].map((id): ResourceRef => ({
      kind: "bot",
      id,
    })),
  ].filter((resource) => {
    const root = demoPolicy(store, universe, resource).root;
    return root.kind === "collection" && root.id === id;
  });
}
