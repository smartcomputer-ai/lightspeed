import { runtimeHttpRoutes, problem } from "./runtime-http.js";
import type { MiddlewareHandler } from "hono";
import { authenticateRequest, CredentialRateLimit } from "./authentication.js";
import { enrollmentRoutes } from "./routes/enrollment.js";
import { platformRoutes } from "./routes/platform.js";
import { credentialRoutes } from "./credential-routes.js";
import { sessionRoutes } from "./session-routes.js";
import { HTTPException } from "hono/http-exception";
import { sessionAdminRoutes } from "./routes/session-admin.js";
import { withApiErrors } from "./http.js";
import { Hono } from "hono";
import { desc, eq, sql } from "drizzle-orm";
import { auditQuerySchema, type AuditPage } from "@lightspeed-ai/platform-shared";
import { schema } from "@lightspeed-ai/platform-db";
import type { AppContext, ApiVariables } from "./context.js";
import { moduleRoutes } from "./module-routes.js";
import type { ApiModule } from "./module-api.js";
import { conversationRoutes } from "./routes/conversations.js";
import { environmentDeploymentRoutes } from "./routes/environment-deployment.js";
import { gatewayRoutes } from "./routes/gateway.js";
import { universeRoutes } from "./routes/universes.js";
import { registerWebApp } from "./static.js";
import { sessionAllows } from "./context.js";
import { publicAuthConfig } from "./auth.js";


export function buildApp(ctx: AppContext, modules: readonly ApiModule[] = []) {
  const app = new Hono();
  app.onError((error, c) => {
    if (error instanceof CredentialRateLimit) return error.response();
    if (error instanceof HTTPException) return c.req.path.startsWith("/api/v1/") ? problem(c, error.status, error.status === 401 ? "unauthenticated" : "forbidden", error.message) : c.json({ error: error.message }, error.status);
    return withApiErrors(c, async () => { throw error; });
  });

  app.get("/health", (c) => c.json({ ok: true }));
  app.get("/device", c => c.redirect(`/app/device${new URL(c.req.url).search}`));
  app.get("/api/login-config", (c) => c.json(publicAuthConfig(ctx.env)));

  // Organization membership, service users and credential association go through
  // reviewed Platform services; raw plugin management endpoints are not served.
  app.all("/api/auth/api-key/*", c => c.json({ error: "not found" }, 404));
  app.all("/api/auth/organization/*", (c) => c.json({ error: "not found" }, 404));
  // better-auth owns the rest of /api/auth (sign-in, admin user management,
  // bearer tokens).
  app.on(["GET", "POST"], "/api/auth/*", (c) => ctx.auth.handler(c.req.raw));

  if (ctx.env.federation) app.route("/", enrollmentRoutes(ctx.db, ctx.env.federation));

  const authenticated: MiddlewareHandler<{ Variables: ApiVariables }> = async (c, next) => {
    const admission = await authenticateRequest(ctx, c.req.raw);
    const browserOnly = c.req.path.startsWith("/api/ui/");
    if (!admission || (browserOnly && (!admission.session || admission.auth.credential.kind !== "session"))) {
      return c.req.path.startsWith("/api/v1/") ? problem(c, 401, "unauthenticated", "authentication required") : c.json({ error: "unauthorized" }, 401);
    }
    if (admission.auth.credential.kind === "session" && !["GET", "HEAD", "OPTIONS"].includes(c.req.method)) {
      const origin = c.req.header("origin");
      const site = c.req.header("sec-fetch-site");
      if ((origin && ![ctx.env.baseUrl, ...ctx.env.trustedOrigins].includes(origin)) || site === "cross-site" || (!origin && c.req.header("cookie") && site !== "same-origin" && site !== "same-site")) return c.req.path.startsWith("/api/v1/") ? problem(c, 403, "forbidden", "untrusted origin") : c.json({ error: "untrusted origin" }, 403);
    }
    c.set("auth", admission.auth);
    if (admission.session) c.set("session", admission.session);
    await next();
    if (admission.renewedToken) c.header("set-auth-token", admission.renewedToken);
  };
  const shared = new Hono<{ Variables: ApiVariables }>();
  shared.use("*", authenticated);
  shared.route("/platform", sessionRoutes(ctx));
  shared.route("/platform", credentialRoutes(ctx));
  shared.route("/platform", platformRoutes(ctx, modules));
  shared.route("/universes", moduleRoutes(ctx, modules, "public"));
  shared.route("/", runtimeHttpRoutes(ctx));
  app.route("/api/v1", shared);

  const api = new Hono<{ Variables: ApiVariables }>();
  api.use("*", authenticated);

  api.get("/me", async (c) => {
    const session = c.get("session");
    return c.json({ user: session.user });
  });


  api.get("/admin/audit", async (c) => {
    if (!sessionAllows(c.get("session"), "readAudit")) return c.json({ error: "deployment permission required: readAudit" }, 403);
    const query = auditQuerySchema.safeParse(c.req.query());
    if (!query.success) return c.json({ error: "invalid audit pagination" }, 400);
    const { limit, cursor } = query.data;
    const audit = schema.identityAudit;
    if (cursor) {
      const [anchor] = await ctx.db.select({ id: audit.id }).from(audit).where(eq(audit.id, cursor));
      if (!anchor) return c.json({ error: "audit cursor not found" }, 400);
    }
    // Keep the cursor timestamp in PostgreSQL to preserve sub-millisecond precision.
    const rows = await ctx.db.select().from(audit)
      .where(cursor ? sql`(${audit.createdAt}, ${audit.id}) < (select created_at, id from identity_audit where id = ${cursor}::uuid)` : undefined)
      .orderBy(desc(audit.createdAt), desc(audit.id)).limit(limit + 1);
    const events = rows.slice(0, limit).map((event) => ({ ...event, createdAt: event.createdAt.toISOString() }));
    const page: AuditPage = { events, nextCursor: rows.length > limit ? events.at(-1)!.id : null };
    return c.json(page);
  });

  api.route("/universes", universeRoutes(ctx));
  api.route("/universes", gatewayRoutes(ctx));
  api.route("/universes", conversationRoutes(ctx));
  api.route("/universes", moduleRoutes(ctx, modules, "browser"));
  api.route("/admin", sessionAdminRoutes(ctx));
  api.route("/admin", environmentDeploymentRoutes(ctx));

  app.route("/api/ui", api);

  registerWebApp(app);

  return app;
}
