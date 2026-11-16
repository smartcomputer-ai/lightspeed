import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { credentialRestrictionsSchema } from "@lightspeed-ai/platform-shared";
import type { AppContext } from "./context.js";
import type { Session } from "./auth.js";
import type { AuthContext } from "./auth-context.js";
import { principalAllowed, sessionAllowed } from "./auth-access.js";
import { APPLICATION_KEY_CONFIG, APPLICATION_KEY_PREFIX, PERSONAL_KEY_CONFIG, PERSONAL_KEY_PREFIX } from "./api-key-config.js";
import { ensurePersonalAccount } from "./personal-accounts.js";

/** A valid credential exceeding its budget is retryable, not unauthenticated. */
export class CredentialRateLimit extends Error {
  constructor(readonly retryAfterSeconds?: number) { super("API credential rate limit exceeded"); }
  response(): Response {
    return new Response(JSON.stringify({ type: "about:blank", title: this.message, detail: this.message, status: 429, kind: "rejected" }), {
      status: 429, headers: { "content-type": "application/problem+json", ...(this.retryAfterSeconds === undefined ? {} : { "retry-after": String(this.retryAfterSeconds) }) },
    });
  }
}

/** Authentication resolves once; account/universe services apply current authority afterward. */
export async function authenticateRequest(ctx: AppContext, request: Request): Promise<{ auth: AuthContext; session?: Session; renewedToken?: string } | null> {
  const authorization = request.headers.get("authorization");
  if (authorization !== null) {
    // An explicitly supplied invalid credential must never fall back to a broader cookie.
    const match = /^Bearer ([^\s,]+)$/i.exec(authorization);
    if (!match || match[1]!.length > 4096) return null;
    const secret = match[1]!;
    const configId = secret.startsWith(PERSONAL_KEY_PREFIX) ? PERSONAL_KEY_CONFIG : secret.startsWith(APPLICATION_KEY_PREFIX) ? APPLICATION_KEY_CONFIG : null;
    if (!configId) {
      // Ignore cookies completely when a bearer is explicit. Better Auth owns
      // token decoding/signatures; only device-issued CLI sessions are accepted.
      const headers = new Headers({ authorization });
      const initial = await ctx.auth.api.getSession({ headers, query: { disableRefresh: true } });
      if (!initial || initial.session.sessionKind !== "cli" || !sessionAllowed(initial, ctx.env) || !principalAllowed(initial.user, ctx.env)) return null;
      const renewed = await ctx.auth.api.getSession({ headers, returnHeaders: true });
      const session = renewed.response;
      if (!session || session.session.sessionKind !== "cli" || !sessionAllowed(session, ctx.env)) return null;
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, session.user.id));
      if (!user || !principalAllowed(user, ctx.env) || !sessionAllowed({ user, session: session.session }, ctx.env)) return null;
      return { auth: { principal: { kind: "user", id: user.id, user, platformAdmin: user.role?.split(",").includes("admin") === true },
        actorId: user.id, credential: { kind: "cliSession", id: session.session.id, restrictions: {} },
        session: { id: session.session.id, createdAt: session.session.createdAt, expiresAt: effectiveSessionExpiry(session, ctx.env) } },
        ...(renewed.headers.get("set-auth-token") ? { renewedToken: renewed.headers.get("set-auth-token")! } : {}) };
    }
    const verified = await ctx.auth.api.verifyApiKey({ body: { key: secret, configId } });
    if (!verified.valid && ["RATE_LIMITED", "RATE_LIMIT_EXCEEDED", "USAGE_EXCEEDED"].includes(verified.error?.code ?? "")) {
      const details = verified.error && "details" in verified.error ? verified.error.details : undefined;
      const retryMs = details && typeof details === "object" && "tryAgainIn" in details ? details.tryAgainIn : undefined;
      throw new CredentialRateLimit(typeof retryMs === "number" && Number.isFinite(retryMs) ? Math.max(1, Math.ceil(retryMs / 1000)) : undefined);
    }
    if (!verified.valid || !verified.key || verified.key.configId !== configId || !verified.key.expiresAt) return null;
    const key = verified.key;
    const [binding] = await ctx.db.select().from(schema.credentialBindings).where(eq(schema.credentialBindings.keyId, key.id));
    if (!binding) return null;
    const limits = credentialRestrictionsSchema.safeParse(binding.restrictions);
    if (!limits.success) return null;
    if (configId === PERSONAL_KEY_CONFIG) {
      if (!binding.userId || binding.applicationId || binding.userId !== key.referenceId) return null;
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, binding.userId));
      if (!user || !principalAllowed(user, ctx.env)) return null;
      return { auth: {
        principal: { kind: "user", id: user.id, user, platformAdmin: user.role?.split(",").includes("admin") === true },
        credential: { kind: "personalKey", id: key.id, restrictions: limits.data }, actorId: user.id,
      } };
    }
    if (!binding.applicationId || binding.userId) return null;
    const [row] = await ctx.db.select({ application: schema.applications, account: schema.organization }).from(schema.applications)
      .innerJoin(schema.organization, eq(schema.organization.id, schema.applications.organizationId))
      .where(eq(schema.applications.id, binding.applicationId));
    if (!row?.application.enabled || !row.account.enabled || row.account.id !== key.referenceId) return null;
    return { auth: {
      principal: { kind: "application", id: row.application.id, application: row.application, accountId: row.account.id, platformAdmin: false },
      credential: { kind: "applicationKey", id: key.id, restrictions: limits.data }, actorId: `application:${row.application.id}`,
    } };
  }
  const session = await ctx.auth.api.getSession({ headers: request.headers });
  if (!session || session.session.sessionKind !== "browser" || !sessionAllowed(session, ctx.env)) return null;
  const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, session.user.id));
  if (!user || !principalAllowed(user, ctx.env) || !sessionAllowed({ user, session: session.session }, ctx.env)) return null;
  await ensurePersonalAccount(ctx.db, ctx.env, user.id);
  return { session, auth: {
    principal: { kind: "user", id: user.id, user, platformAdmin: user.role?.split(",").includes("admin") === true },
    credential: { kind: "session", id: session.session.id, restrictions: {} }, actorId: user.id,
    session: { id: session.session.id, createdAt: session.session.createdAt, expiresAt: effectiveSessionExpiry(session, ctx.env) },
  } };
}

function effectiveSessionExpiry(session: Session, env: AppContext["env"]): Date {
  const absolute = session.session.createdAt.getTime() + (env.oidc ? env.oidc.sessionMaxAgeSeconds * 1000 : (env.browserSessionMaxDays ?? 90) * 86400000);
  return new Date(Math.min(session.session.expiresAt.getTime(), absolute));
}
