import type { FeatureKey } from "./index.js";
import { UNIVERSE_ACTIONS, type UniverseAction } from "./access.js";
import { z } from "zod";

export const organizationModuleSchema = z.object({
  id: z.string(),
  name: z.string(),
  description: z.string(),
  enabled: z.boolean(),
});

const moduleMethods = ["GET", "POST", "PUT", "PATCH", "DELETE"] as const;

/** An operation exposed to browser sessions and scoped API credentials.
 * Its credential operation ID is modules/<module>/<operation>. */
export interface ModuleOperation {
  id: string;
  method: typeof moduleMethods[number];
  path: string;
  permission: string;
}

/** A distribution selects module definitions. Organizations override their defaults. */
export interface PlatformModule {
  id: string;
  name: string;
  description: string;
  defaultEnabled: boolean;
  /** Contracts for the module's public API, independent of any private browser routes. */
  operations?: readonly ModuleOperation[];
  /** Opt into the gateway's reviewed runtime operations for this module ID. */
  exposeRuntimeOperations?: boolean;
  /** Optional capability the universe must also have enabled. */
  requiresFeature?: FeatureKey;
  /** Local permission names and the universe authority each requires. */
  permissions?: Readonly<Record<string, UniverseAction>>;
}

/** Resolve this module's permissions from the caller's effective universe grants. */
export function modulePermissions(module: PlatformModule, actions: ReadonlySet<UniverseAction>): ReadonlySet<string> {
  return new Set(Object.entries(module.permissions ?? {})
    .filter(([, required]) => actions.has(required))
    .map(([permission]) => permission));
}

export function organizationModuleView(module: PlatformModule, enabled = module.defaultEnabled): z.infer<typeof organizationModuleSchema> {
  return { id: module.id, name: module.name, description: module.description, enabled };
}

/** A distribution can select either or both route factories for a trusted module. */
export interface ModuleRouteEntries<R> {
  /** Private UI endpoints served only to authenticated browser sessions. */
  browserRoutes?: R;
  /** Implementation of every published operation in the module's contract. */
  publicOperations?: R;
}

/** Attach host-specific route factories to the selected modules. */
export function withModuleRoutes<M extends PlatformModule, R>(
  modules: readonly M[],
  routes: Readonly<Partial<Record<string, ModuleRouteEntries<R>>>>,
) {
  validateModuleDefinitions(modules);
  return modules.map(module => {
    const entries = Object.hasOwn(routes, module.id) ? routes[module.id] : undefined;
    return { ...module, browserRoutes: entries?.browserRoutes, publicOperations: entries?.publicOperations };
  });
}

export function validateModuleDefinitions(modules: readonly PlatformModule[]) {
  const ids = new Set<string>();
  for (const module of modules) {
    if (!/^[a-z][a-z0-9-]*$/.test(module.id) || ids.has(module.id)) {
      throw new Error(`Invalid or duplicate module ID: ${module.id}`);
    }
    ids.add(module.id);
    validateModuleOperations(module);
  }
}

/** An authenticated request must identify exactly one credential operation. */
function validateModuleOperations(module: PlatformModule) {
  for (const [name, action] of Object.entries(module.permissions ?? {})) {
    if (!name || !UNIVERSE_ACTIONS.includes(action)) throw new Error(`Invalid permission ${module.id}/${name}: ${action}`);
  }
  const ids = new Set<string>();
  const routes = new Set<string>();
  for (const op of module.operations ?? []) {
    if (!moduleMethods.includes(op.method)) {
      throw new Error(`Invalid module operation method: ${module.id}/${op.id} ${op.method}`);
    }
    if (!/^[a-z][a-zA-Z0-9-]*(?:\/[a-z][a-zA-Z0-9-]*)*$/.test(op.id) || ids.has(op.id)) {
      throw new Error(`Invalid or duplicate operation ID: ${module.id}/${op.id}`);
    }
    ids.add(op.id);
    if (!Object.hasOwn(module.permissions ?? {}, op.permission)) {
      throw new Error(`Operation ${module.id}/${op.id} requires an undeclared permission: ${op.permission}`);
    }
    const segments = op.path === "/" ? [] : op.path.slice(1).split("/");
    const params = segments.filter(part => part.startsWith(":"));
    // Reserve the host's universe parameter. Wildcards and optional segments could admit undeclared URLs.
    if (!op.path.startsWith("/") || !segments.every(part => /^(?:[a-zA-Z0-9][a-zA-Z0-9_.~-]*|:[a-zA-Z][a-zA-Z0-9_]*)$/.test(part))
      || params.includes(":id") || new Set(params).size !== params.length) {
      throw new Error(`Invalid module operation path: ${module.id}/${op.id} ${op.path}`);
    }
    const shape = op.method + "/" + segments.map(part => part.startsWith(":") ? ":" : part).join("/");
    if (routes.has(shape)) throw new Error(`Ambiguous module operation path: ${module.id} ${op.method} ${op.path}`);
    routes.add(shape);
  }
}
