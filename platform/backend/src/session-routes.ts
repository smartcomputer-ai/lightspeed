import { Hono, type Handler } from "hono";
import { HTTPException } from "hono/http-exception";
import { SESSION_OPERATIONS } from "@lightspeed-ai/platform-shared";
import type { AppContext, ApiVariables } from "./context.js";
import { getAuth } from "./auth-context.js";
import { auditAsSubject } from "./identity-audit.js";

export function sessionRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.header("cache-control", "no-store"); await next(); });
  const handlers: Record<keyof typeof SESSION_OPERATIONS, Handler<{ Variables: ApiVariables }>> = {
    "platform/session/read": c => {
      // Reporting this credential's own identity never expands its resource
      // authority. It also works for keys without a human session.
      const auth = getAuth(c); const principal = auth.principal;
      return c.json({ principal: { kind: principal.kind, id: principal.id,
        name: principal.kind === "user" ? principal.user.name : principal.application.name,
        email: principal.kind === "user" ? principal.user.email : null },
        credential: { kind: auth.credential.kind, id: auth.credential.id },
        session: auth.session ? { id: auth.session.id, createdAt: auth.session.createdAt.toISOString(), expiresAt: auth.session.expiresAt.toISOString() } : null });
    },
    "platform/session/revoke": async c => {
      const auth = getAuth(c);
      if (auth.credential.kind !== "cliSession") throw new HTTPException(403, { message: "Only the current CLI session may be revoked here" });
      const headers = new Headers({ authorization: c.req.header("authorization")! });
      await ctx.auth.api.signOut({ headers });
      await auditAsSubject(ctx.db, auth, { action: "session.cli_sign_out", targetId: auth.credential.id });
      return c.json({ revoked: true });
    },
  };
  for (const operation of Object.keys(SESSION_OPERATIONS) as (keyof typeof SESSION_OPERATIONS)[]) {
    const spec = SESSION_OPERATIONS[operation];
    app.on(spec.method, spec.path.replace(/^\/api\/v1\/platform/, ""), handlers[operation]);
  }
  return app;
}
