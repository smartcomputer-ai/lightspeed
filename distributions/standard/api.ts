import type { ModuleRouteFactory } from "@lightspeed-ai/platform-backend/modules";
import { fileURLToPath } from "node:url";
import { withModuleRoutes, type ModuleRouteEntries } from "@lightspeed-ai/platform-shared";
import { buildApp, type AppContext } from "@lightspeed-ai/platform-backend/server";
import { standardModules } from "./catalog.js";

async function loadFactory(id: string, entry?: string): Promise<ModuleRouteFactory | undefined> {
  if (entry === undefined) return;
  const { default: createRoutes } = await import(entry);
  if (typeof createRoutes !== "function") throw new Error(`Module ${id} must export a route factory from ${entry}`);
  return createRoutes;
}

const factories: Record<string, ModuleRouteEntries<ModuleRouteFactory>> = Object.create(null);
for (const { id, api } of standardModules) {
  if (!api) continue;
  factories[id] = {
    browserRoutes: await loadFactory(id, api.browserRoutes),
    publicOperations: await loadFactory(id, api.publicOperations),
  };
}

const modules = standardModules.map(({ migrations, ...module }) => ({
  ...module,
  migrationsFolder: migrations && fileURLToPath(new URL(migrations, import.meta.url)),
}));
export const standardApiModules = withModuleRoutes(modules, factories);

export function buildStandardApi(context: AppContext) {
  return buildApp(context, standardApiModules);
}
