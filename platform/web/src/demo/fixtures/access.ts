import { accessState, demoCreateAccess, demoPolicy, resourceKey } from "../access-state";
import type { DemoStore } from "../store";

export function seedAccess(store: DemoStore) {
  const universe = store.universeBySlug("software-factory");
  if (!universe) return;
  accessState(universe).personal = true;
  if (universe.sessions.has("session-flaky-scheduler"))
    demoCreateAccess(
      store,
      universe,
      { kind: "session", id: "session-flaky-scheduler" },
      { visibility: "restricted" },
      { kind: "personal" },
    );
  // Priya's own machine: restricted to her, and shared for use with the demo
  // person. The default agent identity cannot use it, so the session that
  // explains code on it runs as the demo person.
  const laptop = { kind: "environment" as const, id: "env-priya-laptop" };
  if (universe.environments.has(laptop.id)) {
    demoCreateAccess(store, universe, laptop, {
      visibility: "restricted",
      grants: [
        { subject: { kind: "principal", id: store.currentUser.id }, permission: "use" },
      ],
    });
    const policy = demoPolicy(store, universe, laptop);
    accessState(universe).policies.set(resourceKey(laptop), {
      ...policy,
      owner: "user-priya",
      grants: policy.grants.map((grant) => ({ ...grant, grantedBy: "user-priya" })),
    });
  }
  if (universe.sessions.has("session-auth-middleware"))
    demoCreateAccess(
      store,
      universe,
      { kind: "session", id: "session-auth-middleware" },
      { visibility: "universe" },
      { kind: "personal" },
    );
}
