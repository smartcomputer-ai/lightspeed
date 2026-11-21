import { Hono } from "hono";
import { parseBody, type UniverseModuleEnv } from "@lightspeed-ai/platform-backend/modules";
import { sessionCreateSchema, sessionListQuery } from "./collection.js";

/** Standalone Sessions browser and launcher, mounted by the distribution. */
export default function sessionCollectionRoutes(): Hono<UniverseModuleEnv> {
  const app = new Hono<UniverseModuleEnv>();

  app.get("/", async (c) => {
    const page = await c.get("client").call("session/list", sessionListQuery(c.req.url));
    return c.json({ sessions: page.sessions ?? [], nextCursor: page.nextCursor ?? null });
  });

  app.post("/", async (c) => {
    const body = await parseBody(c, sessionCreateSchema);
    if (!body.ok) return body.response;
    const input = body.data;
    if (input.access?.visibility === "universe" && !c.get("access").actions.has("configureSession")) {
      return c.json({ error: "universe permission required to start shared work: configureSession" }, 403);
    }
    const client = c.get("client");
    const response = await client.call("session/start", {
      ...(input.displayName ? { displayName: input.displayName } : {}),
      ...(input.metadata && Object.keys(input.metadata).length ? { metadata: input.metadata } : {}),
      ...(input.deleteAfterCloseMs !== undefined ? { deleteAfterCloseMs: input.deleteAfterCloseMs } : {}),
      profile: input.profile,
      ...(input.access ? { access: input.access } : {}),
    });
    const current = await client.call("session/read", { sessionId: response.session.id });
    return c.json(current.session);
  });

  return app;
}
