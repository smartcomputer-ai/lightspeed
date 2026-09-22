import { seedAccess } from "./access";
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
  seedAccess(store);
  return store;
}
