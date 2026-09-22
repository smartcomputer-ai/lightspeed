import {
  accessState,
  demoCreateAccess,
  demoSummary,
  resourceKey,
} from "../access-state";
import type { DemoStore } from "../store";

export function seedAccess(store: DemoStore) {
  const universe = store.universeBySlug("software-factory");
  if (!universe) return;
  accessState(universe).personal = true;
  for (const [id, name, personal] of [
    ["delivery", "Delivery", false],
    ["investigations", "Investigations", true],
  ] as const) {
    const resource = { kind: "collection" as const, id };
    demoCreateAccess(
      store,
      universe,
      resource,
      { visibility: personal ? "restricted" : "universe" },
      { kind: personal ? "personal" : "service" },
    );
    accessState(universe).collections.set(id, {
      collectionId: id,
      displayName: name,
      revision: 1,
      createdAtMs: Date.now(),
      updatedAtMs: Date.now(),
      access: demoSummary(store, universe, resource),
    });
  }
  if (universe.bots.has("implementer"))
    accessState(universe).roots.set(
      resourceKey({ kind: "bot", id: "implementer" }),
      { kind: "collection", id: "delivery" },
    );
  if (universe.sessions.has("session-flaky-scheduler"))
    accessState(universe).roots.set(
      resourceKey({ kind: "session", id: "session-flaky-scheduler" }),
      { kind: "collection", id: "investigations" },
    );
}
