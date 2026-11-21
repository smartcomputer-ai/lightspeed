import type { ApiModule } from "@lightspeed-ai/platform-backend/modules";
import { fileURLToPath } from "node:url";
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

const modules = standardModules.map(({ migrations, ...module }) => ({
  ...module,
  migrationsFolder: migrations && fileURLToPath(new URL(migrations, import.meta.url)),
}));
export const standardApiModules = withModuleRoutes(modules, factories);

export function buildStandardApi(context: AppContext) {
  return buildApp(context, standardApiModules);
}
