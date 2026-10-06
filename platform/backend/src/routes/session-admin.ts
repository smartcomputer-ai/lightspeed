import { Hono } from "hono";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { auditIdentity } from "../identity-audit.js";
import { deploymentClientFor, withGateway } from "./gateway.js";

/** Deleted sessions are available only through Platform administration. */
export function sessionAdminRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    if (!isPlatformAdmin(c.get("session"))) return c.json({ error: "platform admin required" }, 403);
    await next();
  });
  app.get("/universes/:id/deleted-sessions", (c) => withGateway(c, async () => {
    const [universe] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.id, c.req.param("id"))).limit(1);
    if (!universe) return c.json({ error: "not found" }, 404);
    const result = await deploymentClientFor(ctx, universe.gatewayUrl).call("deployment/sessions/deleted/list", {
      universeId: universe.lightspeedUniverseId, after: c.req.query("after"),
    });
    return c.json(result.result);
  }));
  app.post("/universes/:id/sessions/:sessionId/purge", (c) => withGateway(c, async () => {
    const [universe] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.id, c.req.param("id"))).limit(1);
    if (!universe) return c.json({ error: "not found" }, 404);
    const result = await deploymentClientFor(ctx, universe.gatewayUrl).call("deployment/sessions/purge", {
      universeId: universe.lightspeedUniverseId, sessionId: c.req.param("sessionId"),
    });
    if (result.result.deletedSessionIds.length) await auditIdentity(ctx.db, {
      actorId: c.get("session").user.id, universeId: universe.id,
      targetId: c.req.param("sessionId"), action: "session.purge",
      details: { deletedSessionIds: JSON.stringify(result.result.deletedSessionIds) },
    });
    return c.json(result.result);
  }));
  return app;
}
