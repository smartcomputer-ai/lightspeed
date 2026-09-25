/// The in-browser stand-in for the platform server: one Hono app the fetch
/// shim hands every same-origin `/api/*` request to.
import { Hono } from "hono";
import type { DemoStore } from "./store";
import { adminRoutes } from "./routes/admin";
import { authRoutes } from "./routes/auth";
import { botRoutes, hookRoutes } from "./routes/bots";
import { channelRoutes } from "./routes/channels";
import { environmentRoutes } from "./routes/environments";
import { mcpRoutes } from "./routes/mcp";
import { platformRoutes } from "./routes/platform";
import { profileRoutes } from "./routes/profiles";
import { secretRoutes } from "./routes/secrets";
import { sessionRoutes } from "./routes/sessions";
import { workspaceRoutes } from "./routes/workspaces";

export function createDemoRouter(store: DemoStore): Hono {
  const app = new Hono();
  app.get("/health", (c) => c.json({ ok: true, demo: true }));
  app.route("/api/auth", authRoutes(store));
  // The public webhook ingress lives outside /api, exactly like the core's
  // POST /hooks/bots/{universe}/{bot}/{trigger}/{token} route.
  app.route("/hooks", hookRoutes(store));

  const api = new Hono();
  api.route("/", platformRoutes(store));
  api.route("/", adminRoutes(store));
  for (const routes of [
    sessionRoutes,
    profileRoutes,
    workspaceRoutes,
    environmentRoutes,
    mcpRoutes,
    secretRoutes,
    botRoutes,
    channelRoutes,
  ]) {
    api.route("/universes", routes(store));
  }
  app.route("/api/v1", api);

  // A missing stub surfaces as an ordinary API error in the UI instead of a
  // silent hang, which is how gaps get found.
  app.notFound((c) => {
    const what = `${c.req.method} ${new URL(c.req.url).pathname}`;
    console.warn(`[demo] no stub for ${what}`);
    return c.json({ error: `demo: no stub for ${what}` }, 404);
  });
  app.onError((error, c) => {
    console.error("[demo]", error);
    return c.json({ error: `demo stub failed: ${error.message}` }, 500);
  });
  return app;
}
