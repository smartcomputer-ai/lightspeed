import { Hono, type MiddlewareHandler } from "hono";
import { bindModuleOperations, moduleCredentialOperationId, modulePermissions, type ModuleOperation } from "@lightspeed-ai/platform-shared";
import { credentialRefusal, enforceRestrictions } from "./auth-context.js";
import type { AppContext, ApiVariables } from "./context.js";
import type { ApiModule, UniverseModuleEnv } from "./module-api.js";
import { organizationModuleEnabled } from "./module-installation.js";
import { universeForPrincipal } from "./routes/universes.js";
import type { RuntimeOperationOwners } from "./runtime-admission.js";
import { featureRefusal, GateRefusal, universeClient } from "./runtime-client.js";

type ModuleEnv = { Variables: ApiVariables & UniverseModuleEnv["Variables"] };

/** Only declared operation handlers can enter the public router. */
export function moduleRoutes(ctx: AppContext, modules: readonly ApiModule[], operationOwners: RuntimeOperationOwners) {
  const browserRoutes = new Hono<{ Variables: ApiVariables }>();
  const publicOperations = new Hono<{ Variables: ApiVariables }>();
  for (const module of modules) {
    const operations = module.operations ?? [];
    if (operations.length && !module.publicOperations) {
      throw new Error(`Module ${module.id} declares public operations without an API implementation`);
    }
    if (module.publicOperations && !operations.length) {
      throw new Error(`Module ${module.id} provides a public API implementation without declared operations`);
    }
    const base = `/:id/modules/${module.id}`;
    const admit = (operation?: ModuleOperation): MiddlewareHandler<ModuleEnv> => async (c, next) => {
      const access = await universeForPrincipal(ctx, c.get("auth"), c.req.param("id")!);
      if (!access) return c.json({ error: "not found" }, 404);
      if (!await organizationModuleEnabled(ctx.db, access.organization.id, module)) return c.json({ error: "module unavailable" }, 404);
      const off = module.requiresFeature && featureRefusal(access.universe.features, module.requiresFeature);
      if (off) return c.json({ error: off.message }, off.status);
      const grants = modulePermissions(module, access.actions);
      let permissions = grants;
      if (operation) {
        const target = (op: ModuleOperation) => ({
          organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId,
          operation: moduleCredentialOperationId(module.id, op.id), write: op.method !== "GET",
        });
        enforceRestrictions(access.auth, target(operation));
        if (!grants.has(operation.permission)) return c.json({ error: `Permission required: ${module.id}/${operation.permission}` }, 403);
        // A credential allowed to list records may still be unable to publish or retract one.
        permissions = new Set(operations
          .filter(op => grants.has(op.permission) && !credentialRefusal(access.auth, target(op)))
          .map(op => op.permission));
      }
      c.set("permissions", permissions);
      c.set("access", {
        organizationId: access.organization.id,
        universeId: access.universe.id,
        actorId: access.caller.actorId,
        actions: access.actions,
      });
      c.set("client", {
        call: async (method, params, options) => {
          const owner = operationOwners.get(method);
          // The calling module already passed its own installation check above.
          if (operationOwners.has(method) && owner !== module && !await organizationModuleEnabled(ctx.db, access.organization.id, owner)) {
            throw new GateRefusal(404, "module unavailable");
          }
          return universeClient(ctx.env, access.universe, access.caller).call(method, params, options);
        },
        openDownload: (method, params, options) => universeClient(ctx.env, access.universe, access.caller).openDownload(method, params, options),
      });
      await next();
    };
    if (module.browserRoutes) {
      const scoped = new Hono<ModuleEnv>();
      scoped.use("*", admit());
      scoped.route("/", module.browserRoutes({ db: ctx.db }));
      browserRoutes.route(base, scoped);
    }
    if (module.publicOperations) {
      const scoped = new Hono<ModuleEnv>();
      // Mount through Hono once to retain any module-specific error handler.
      const router = new Hono<ModuleEnv>().route("/", module.publicOperations({ db: ctx.db }));
      for (const { operation, routes } of bindModuleOperations(module.id, operations, router.routes)) {
        scoped.on(operation.method, operation.path, admit(operation), ...routes.map(route => route.handler));
      }
      publicOperations.route(base, scoped);
    }
  }
  return { browserRoutes, publicOperations };
}
