import { Hono } from "hono";
import { and, eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { AppContext, ApiVariables } from "./context.js";
import type { ApiModule, UniverseModuleEnv } from "./module-api.js";
import { universeForPrincipal } from "./routes/universes.js";
import { featureRefusal, universeClient } from "./runtime-client.js";

/** Mounted inside the host's authenticated browser API. Each module owns one namespace. */
export function moduleRoutes(ctx: AppContext, modules: readonly ApiModule[]) {
  const host = new Hono<{ Variables: ApiVariables }>();
  for (const module of modules) {
    if (!module.routes) continue;
    const scoped = new Hono<{ Variables: ApiVariables & UniverseModuleEnv["Variables"] }>();
    scoped.use("*", async (c, next) => {
      const access = await universeForPrincipal(ctx, c.get("auth"), c.req.param("id")!);
      if (!access) return c.json({ error: "not found" }, 404);
      const table = schema.organizationModules;
      const [setting] = await ctx.db.select({ enabled: table.enabled }).from(table).where(and(
        eq(table.organizationId, access.organization.id), eq(table.moduleId, module.id),
      )).limit(1);
      if (!(setting?.enabled ?? module.defaultEnabled)) return c.json({ error: "module unavailable" }, 404);
      const off = module.requiresFeature && featureRefusal(access.universe.features, module.requiresFeature);
      if (off) return c.json({ error: off.message }, off.status);
      c.set("access", {
        organizationId: access.organization.id,
        universeId: access.universe.id,
        actorId: access.caller.actorId,
        actions: access.actions,
      });
      c.set("client", {
        call: (method, params, options) => universeClient(ctx.env, access.universe, access.caller).call(method, params, options),
      });
      await next();
    });
    scoped.route("/", module.routes());
    host.route(`/:id/modules/${module.id}`, scoped);
  }
  return host;
}
