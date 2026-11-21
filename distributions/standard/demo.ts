import { withModuleRoutes } from "@lightspeed-ai/platform-shared";
import entries from "virtual:lightspeed/demo-module-entries";
import { createDemoRouter } from "@/demo/router";
import type { DemoStore } from "@/demo/store";
import { standardModules } from "./catalog";

const modules = withModuleRoutes(standardModules, Object.fromEntries(entries.map(({ id, module }) => [id, module])));

export function createStandardDemoRouter(store: DemoStore) {
  return createDemoRouter(store, modules);
}
