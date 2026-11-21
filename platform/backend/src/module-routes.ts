import { Hono, type MiddlewareHandler } from "hono";
import { and, eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { bindModuleOperations, modulePermissions, type ModuleOperation } from "@lightspeed-ai/platform-shared";
import { credentialRefusal, enforceRestrictions } from "./auth-context.js";
import type { AppContext, ApiVariables } from "./context.js";
import type { ApiModule, UniverseModuleEnv } from "./module-api.js";
import { universeForPrincipal } from "./routes/universes.js";
import { featureRefusal, universeClient } from "./runtime-client.js";

/** Each module has one canonical surface. Public routes must name an operation before any handler can run. */
export function moduleRoutes(ctx: AppContext, modules: readonly ApiModule[], surface: "public" | "browser") {
  const host = new Hono<{ Variables: ApiVariables }>();
  for (const module of modules) {
    const moduleSurface = module.operations ? "public" : "browser";
    if (moduleSurface !== surface) continue;
    if (!module.routes) {
      if (module.operations?.length) throw new Error(`Module ${module.id} declares public operations without an API implementation`);
      continue;
    }
    type ModuleEnv = { Variables: ApiVariables & UniverseModuleEnv["Variables"] };
    const scoped = new Hono<ModuleEnv>();
    const admit = (operation?: ModuleOperation): MiddlewareHandler<ModuleEnv> => async (c, next) => {
      const access = await universeForPrincipal(ctx, c.get("auth"), c.req.param("id")!);
      if (!access) return c.json({ error: "not found" }, 404);
      const table = schema.organizationModules;
      const [setting] = await ctx.db.select({ enabled: table.enabled }).from(table).where(and(
        eq(table.organizationId, access.organization.id), eq(table.moduleId, module.id),
      )).limit(1);
      if (!(setting?.enabled ?? module.defaultEnabled)) return c.json({ error: "module unavailable" }, 404);
      const off = module.requiresFeature && featureRefusal(access.universe.features, module.requiresFeature);
      if (off) return c.json({ error: off.message }, off.status);
      const grants = modulePermissions(module, access.actions);
      let permissions = grants;
      if (module.operations && operation) {
        const target = (op: ModuleOperation) => ({
          organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId,
          operation: `${module.id}/${op.id}`, write: op.method !== "GET",
        });
        enforceRestrictions(access.auth, target(operation));
        if (!grants.has(operation.permission)) return c.json({ error: `Permission required: ${module.id}/${operation.permission}` }, 403);
        // A credential allowed to list records may still be unable to publish or retract one.
        permissions = new Set(module.operations
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
        call: (method, params, options) => universeClient(ctx.env, access.universe, access.caller).call(method, params, options),
        openDownload: (method, params, options) => universeClient(ctx.env, access.universe, access.caller).openDownload(method, params, options),
      });
      await next();
    };
    // Mount through Hono once to retain any module-specific error handler.
    const router = new Hono<ModuleEnv>().route("/", module.routes({ db: ctx.db }));
    if (module.operations) {
      for (const { operation, routes } of bindModuleOperations(module.id, module.operations, router.routes)) {
        scoped.on(operation.method, operation.path, admit(operation), ...routes.map(route => route.handler));
      }
    } else {
      scoped.use("*", admit());
      scoped.route("/", router);
    }
    host.route(`/:id/modules/${module.id}`, scoped);
  }
  return host;
}
