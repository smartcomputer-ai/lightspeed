import type { ApiModule } from "@lightspeed-ai/platform-backend/modules";
import { withModuleRoutes } from "@lightspeed-ai/platform-shared";
import { buildApp, type AppContext } from "@lightspeed-ai/platform-backend/server";
import { standardModules } from "./catalog.js";

type RouteFactory = NonNullable<ApiModule["routes"]>;
const factories: Record<string, RouteFactory> = Object.create(null);
for (const { id, api } of standardModules) {
  if (!api) continue;
  const { default: createRoutes }: { default: RouteFactory } = await import(api);
  if (typeof createRoutes !== "function") throw new Error(`Module ${id} must export a route factory from ${api}`);
  factories[id] = createRoutes;
}

export const standardApiModules = withModuleRoutes(standardModules, factories);

export function buildStandardApi(context: AppContext) {
  return buildApp(context, standardApiModules);
}
