import { userClient, requestIdentity } from "./runtime-client.js";
import { identityRoutes } from "./routes/identity.js";
import { withGateway } from "./routes/gateway.js";
import { Hono } from "hono";
import { schema } from "@lightspeed/platform-db";
import type { AppContext, ApiVariables } from "./context.js";
import { botRoutes } from "./routes/bots.js";
import { channelAccountAdminRoutes, channelUniverseRoutes } from "./routes/channel-accounts.js";
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

  // Better Auth owns sign-in, login sessions, profile/password self-service and bearer tokens.
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
    const principalId = session.user.corePrincipalId;
    if (!principalId) return c.json({ error: "account has no canonical identity" }, 403);
    const self = await userClient(ctx.env, principalId).call("deployment/identity/self", { scope: { kind: "deployment" } });
    c.set("session", session);
    c.set("principalId", principalId);
    await requestIdentity.run(self.result.access, next);
  });

  api.get("/me", async (c) => {
    const session = c.get("session");
    return c.json({ user: { ...session.user, role: isPlatformAdmin() ? "admin" : "user" }, access: requestIdentity.getStore() });
  });

  /// Platform user directory (id, name, email) for member pickers.
  /// Restricted to administrators who can assign access.
  api.get("/users", async (c) => {
    if (!isPlatformAdmin()) {
      const self = await userClient(ctx.env, c.get("principalId")).call("deployment/identity/self", { scope: { kind: "deployment" } });
      if (!self.result.universes.some((r) => r.roles.includes("admin"))) return c.json({ error: "universe admin required" }, 403);
    }
    const rows = await ctx.db
      .select({
        id: schema.user.id,
        name: schema.user.name,
        email: schema.user.email,
        principalId: schema.user.corePrincipalId,
      })
      .from(schema.user);
    return c.json(rows);
  });

  api.get("/status/channels", async (c) => {
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    return c.json({ connectors: await readChannelsStatus(ctx.env.channelsHealthUrls) });
  });

  api.route("/admin", identityRoutes(ctx));
  api.route("/universes", universeRoutes(ctx));
  api.route("/universes", setupRoutes(ctx));
  api.route("/universes", gatewayRoutes(ctx));
  api.route("/universes", botRoutes(ctx));
  api.route("/universes", channelUniverseRoutes(ctx));
  api.route("/admin", environmentDeploymentRoutes(ctx));
  api.route("/channel-accounts", channelAccountAdminRoutes(ctx));

  app.route("/api/v1", api);

  registerWebApp(app);

  return app;
}
