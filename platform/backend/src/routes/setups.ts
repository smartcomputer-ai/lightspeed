import { Hono } from "hono";
import type { AppContext, ApiVariables } from "../context.js";
import { universeForSession } from "./universes.js";

export function setupRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.get("/:id/setups", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || (access.role !== "admin" && access.role !== "operator")) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json([{
      id: "configurator", name: "Configurator", version: 5,
      description: "Configurator setup is temporarily unavailable while scoped Platform tokens are introduced.",
      available: false, status: "unavailable",
    }]);
  });
  app.post("/:id/setups/configurator/install", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    if (access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    // A runtime token has full authority and must never be installed for an agent.
    return c.json({ error: "Configurator setup is temporarily unavailable" }, 503);
  });
  return app;
}
