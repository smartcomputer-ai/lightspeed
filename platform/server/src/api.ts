import { withGateway } from "./routes/gateway.js";
import { Hono } from "hono";
import { and, eq } from "drizzle-orm";
import { schema } from "@lightspeed/platform-db";
import type { AppContext, ApiVariables } from "./context.js";
import { botRoutes } from "./routes/bots.js";
import { channelAccountAdminRoutes, channelUniverseRoutes } from "./routes/channel-accounts.js";
import { apiKeyAdminRoutes } from "./routes/api-keys-admin.js";
import { environmentDeploymentRoutes } from "./routes/environment-deployment.js";
import { gatewayRoutes } from "./routes/gateway.js";
import { setupRoutes } from "./routes/setups.js";
import { universeRoutes } from "./routes/universes.js";
import { registerWebApp } from "./static.js";
import { isPlatformAdmin } from "./context.js";
import { readChannelsStatus } from "./channels-status.js";

export function buildApp(ctx: AppContext) {
  const app = new Hono();
  app.onError((error, c) => withGateway(c, async () => { throw error; }));

  app.get("/health", (c) => c.json({ ok: true }));

  // Organization membership changes only through the universe routes, which
  // keep the last admin and the four roles; the plugin's endpoints are not
  // served.
  app.all("/api/auth/organization/*", (c) => c.json({ error: "not found" }, 404));
  // better-auth owns the rest of /api/auth (sign-in, admin user management,
  // bearer tokens).
  app.on(["GET", "POST"], "/api/auth/*", (c) => ctx.auth.handler(c.req.raw));

  const api = new Hono<{ Variables: ApiVariables }>();
  api.use("*", async (c, next) => {
    const session = await ctx.auth.api.getSession({
      headers: c.req.raw.headers,
    });
    if (!session) {
      return c.json({ error: "unauthorized" }, 401);
    }
    if (!["GET", "HEAD", "OPTIONS"].includes(c.req.method)) {
      const origin = c.req.header("origin");
      if (origin && ![ctx.env.baseUrl, ...ctx.env.trustedOrigins].includes(origin)) return c.json({ error: "untrusted origin" }, 403);
    }
    c.set("session", session);
    await next();
  });

  api.get("/me", async (c) => {
    const session = c.get("session");
    return c.json({ user: session.user });
  });

  /// Platform user directory (id, name, email) for member pickers: platform
  /// admins and the admins of some universe, who add members.
  api.get("/users", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin(session)) {
      const [adminOf] = await ctx.db
        .select({ id: schema.member.id })
        .from(schema.member)
        .where(and(eq(schema.member.userId, session.user.id), eq(schema.member.role, "admin")))
        .limit(1);
      if (!adminOf) return c.json({ error: "universe admin required" }, 403);
    }
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

  api.route("/universes", universeRoutes(ctx));
  api.route("/universes", setupRoutes(ctx));
  api.route("/universes", gatewayRoutes(ctx));
  api.route("/universes", botRoutes(ctx));
  api.route("/universes", channelUniverseRoutes(ctx));
  api.route("/admin", environmentDeploymentRoutes(ctx));
  api.route("/admin", apiKeyAdminRoutes(ctx));
  api.route("/channel-accounts", channelAccountAdminRoutes(ctx));

  app.route("/api/v1", api);

  registerWebApp(app);

  return app;
}
