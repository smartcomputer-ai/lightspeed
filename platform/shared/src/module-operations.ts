import type { ModuleOperation } from "./modules.js";

/** Reserved in both the runtime and the native Platform credential catalogs. */
export const MODULE_OPERATION_PREFIX = "modules/";

export function moduleCredentialOperationId(moduleId: string, operationId: string): string {
  return `${MODULE_OPERATION_PREFIX}${moduleId}/${operationId}`;
}

/** Pair each public operation with its own handler chain before mounting it.
 * An unlisted handler or missing implementation is a distribution error. */
export function bindModuleOperations<T extends { method: string; path: string }>(
  moduleId: string, operations: readonly ModuleOperation[], routes: readonly T[],
) {
  for (const route of routes) {
    if (route.method === "ALL" && route.path === "/*") continue;
    if (!operations.some(op => op.path === route.path && (op.method === route.method || route.method === "ALL"))) {
      throw new Error(`Module ${moduleId} implements undeclared route ${route.method} ${route.path}`);
    }
  }
  return operations.map(operation => {
    if (!routes.some(route => route.method === operation.method && route.path === operation.path)) {
      throw new Error(`Module ${moduleId} is missing the handler for ${operation.method} ${operation.path}`);
    }
    const handlers = routes.filter(route => {
      if (route.method === "ALL") return route.path === "/*" || route.path === operation.path;
      return route.method === operation.method && route.path === operation.path;
    });
    return { operation, routes: handlers };
  });
}
