import { Hono } from "hono";
import { schema } from "@lightspeed/platform-db";
import type { AppContext, ApiVariables } from "./context.js";
import { bindingRoutes } from "./routes/bindings.js";
import { channelAccountRoutes } from "./routes/channel-accounts.js";
import { foundryRoutes } from "./routes/foundry.js";
import { environmentOperatorRoutes } from "./routes/environment-operators.js";
import { gatewayRoutes } from "./routes/gateway.js";
import { setupRoutes } from "./routes/setups.js";
import { universeRoutes } from "./routes/universes.js";
import { registerWebApp } from "./static.js";
import { isPlatformAdmin } from "./context.js";
import { readChannelsStatus } from "./channels-status.js";

export function buildApp(ctx: AppContext) {
  const app = new Hono();

  app.get("/health", (c) => c.json({ ok: true }));

  // better-auth owns everything under /api/auth (sign-in, admin user
  // management, organization endpoints, bearer tokens).
  app.on(["GET", "POST"], "/api/auth/*", (c) => ctx.auth.handler(c.req.raw));

  const api = new Hono<{ Variables: ApiVariables }>();
  api.use("*", async (c, next) => {
    const session = await ctx.auth.api.getSession({
      headers: c.req.raw.headers,
    });
    if (!session) {
      return c.json({ error: "unauthorized" }, 401);
    }
    c.set("session", session);
    await next();
  });

  api.get("/me", async (c) => {
    const session = c.get("session");
    return c.json({ user: session.user });
  });

  /// Platform user directory (id, name, email) for member pickers.
  /// Authenticated-only, deliberately not admin-gated: this is a small
  /// private deployment where members address each other by account, and
  /// only owners/admins can act on what they see here.
  api.get("/users", async (c) => {
    const rows = await ctx.db
      .select({
        id: schema.user.id,
        name: schema.user.name,
        email: schema.user.email,
      })
      .from(schema.user);
    return c.json(rows);
  });

  api.get("/status/channels", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    return c.json({ connectors: await readChannelsStatus(ctx.env.channelsHealthUrls) });
  });

  const bindings = bindingRoutes(ctx);
  const foundry = foundryRoutes(ctx);
  api.route("/universes", universeRoutes(ctx));
  api.route("/universes", setupRoutes(ctx));
  api.route("/universes", bindings.byUniverse);
  api.route("/universes", gatewayRoutes(ctx));
  api.route("/universes", foundry.byUniverse);
  api.route("/bindings", bindings.byId);
  api.route("/foundry/packs", foundry.packs);
  api.route("/admin", environmentOperatorRoutes(ctx));
  api.route("/channel-accounts", channelAccountRoutes(ctx));

  app.route("/api/v1", api);

  registerWebApp(app);

  return app;
}
