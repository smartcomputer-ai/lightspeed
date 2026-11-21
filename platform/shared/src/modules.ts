import type { FeatureKey } from "./index.js";
import { z } from "zod";

export const organizationModuleSchema = z.object({
  id: z.string(),
  name: z.string(),
  description: z.string(),
  enabled: z.boolean(),
});

/** A distribution selects module definitions. Organizations override their defaults. */
export interface PlatformModule {
  id: string;
  name: string;
  description: string;
  defaultEnabled: boolean;
  /** Optional capability the universe must also have enabled. */
  requiresFeature?: FeatureKey;
}

export function organizationModuleView(module: PlatformModule, enabled = module.defaultEnabled): z.infer<typeof organizationModuleSchema> {
  return { id: module.id, name: module.name, description: module.description, enabled };
}

/** Attach host-specific routes to the modules selected by a distribution. */
export function withModuleRoutes<R>(modules: readonly PlatformModule[], routes: Readonly<Partial<Record<string, R>>>) {
  const ids = new Set<string>();
  return modules.map(module => {
    if (!/^[a-z][a-z0-9-]*$/.test(module.id) || ids.has(module.id)) {
      throw new Error(`Invalid or duplicate module ID: ${module.id}`);
    }
    ids.add(module.id);
    return { ...module, routes: Object.hasOwn(routes, module.id) ? routes[module.id] : undefined };
  });
}
