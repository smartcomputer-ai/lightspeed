import { withModuleRoutes } from "@lightspeed-ai/platform-shared";
import { sessionCollectionRoutes } from "../../modules/sessions/demo";
import { createDemoRouter } from "@/demo/router";
import { botRoutes } from "@/demo/routes/bots";
import type { DemoStore } from "@/demo/store";
import { standardModules } from "./catalog";

export function createStandardDemoRouter(store: DemoStore) {
  return createDemoRouter(store, withModuleRoutes(standardModules, { bots: botRoutes, sessions: sessionCollectionRoutes }));
}
