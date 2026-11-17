import { Hono } from "hono";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { sessionAllows } from "../context.js";
import { auditIdentity } from "../identity-audit.js";
import { withApiErrors } from "../http.js";
import { deploymentClient } from "../runtime-client.js";

/** Deleted sessions are available only through Platform administration. */
export function sessionAdminRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    if (!sessionAllows(c.get("session"), "purgeSessions")) return c.json({ error: "deployment permission required: purgeSessions" }, 403);
    await next();
  });
  app.get("/universes/:id/deleted-sessions", (c) => withApiErrors(c, async () => {
    const [universe] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.id, c.req.param("id"))).limit(1);
    if (!universe) return c.json({ error: "not found" }, 404);
    const result = await deploymentClient(ctx.env).call("deployment/sessions/deleted/list", {
      universe: universe.lightspeedUniverseId, after: c.req.query("after"),
    });
    return c.json(result);
  }));
  app.post("/universes/:id/sessions/:sessionId/purge", (c) => withApiErrors(c, async () => {
    const [universe] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.id, c.req.param("id"))).limit(1);
    if (!universe) return c.json({ error: "not found" }, 404);
    const result = await deploymentClient(ctx.env).call("deployment/sessions/purge", {
      universe: universe.lightspeedUniverseId, sessionId: c.req.param("sessionId"),
    });
    if (result.deletedSessionIds.length) await auditIdentity(ctx.db, {
      actorId: c.get("session").user.id, universeId: universe.id,
      targetId: c.req.param("sessionId"), action: "session.purge",
      details: { deletedSessionIds: JSON.stringify(result.deletedSessionIds) },
    });
    return c.json(result);
  }));
  return app;
}
