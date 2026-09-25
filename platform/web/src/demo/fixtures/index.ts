/// Builds the demo world: one platform admin, a few other users, and one
/// universe per use-case. Add a universe by adding a seed module here.
import { DemoStore, type DemoUser } from "../store";
import { seedPersonalAssistant } from "./personal-assistant";
import { seedPlatform } from "./platform";
import { seedSoftwareFactory } from "./software-factory";
import { seedTechnicalSupport } from "./technical-support";

export const DEMO_USER: DemoUser = {
  id: "user-ada",
  name: "Ada Demo",
  email: "ada@lightspeed.demo",
  role: "admin",
  emailVerified: true,
  image: null,
  banned: false,
  createdAt: "2026-06-02T09:12:00.000Z",
  updatedAt: "2026-08-20T14:03:00.000Z",
};

export function createDemoStore(): DemoStore {
  const store = new DemoStore({ ...DEMO_USER });
  seedPlatform(store);
  seedSoftwareFactory(store);
  seedTechnicalSupport(store);
  seedPersonalAssistant(store);
  seedUnsharedWork(store);
  return store;
}

/// One investigation the demo user has not shared yet, to show the badge and
/// the Share action.
function seedUnsharedWork(store: DemoStore) {
  const session = store.universeBySlug("software-factory")?.sessions.get("session-flaky-scheduler");
  if (session) session.view.access = { visibility: "restricted", createdBy: { kind: "actor", id: DEMO_USER.id } };
}
