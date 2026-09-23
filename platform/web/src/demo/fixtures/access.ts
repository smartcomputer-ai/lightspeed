import { accessState, demoCreateAccess } from "../access-state";
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
}
