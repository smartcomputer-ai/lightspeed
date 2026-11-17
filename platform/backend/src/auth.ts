import { AsyncLocalStorage } from "node:async_hooks";
import { apiKey } from "@better-auth/api-key";
import { apiKeyConfigurations } from "./api-key-config.js";
import { organizationKeyAuthority } from "./organization-key-authority.js";
import { ensurePersonalOrganization } from "./personal-organizations.js";
import { betterAuth, type BetterAuthOptions } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { admin } from "better-auth/plugins/admin";
import { bearer } from "better-auth/plugins/bearer";
import { genericOAuth } from "better-auth/plugins/generic-oauth";
import { organization } from "better-auth/plugins/organization";
import { and, eq, sql } from "drizzle-orm";
import { schema, identityUserFields, identitySessionFields, organizationFields, type Db } from "@lightspeed-ai/platform-db";
import type { ServerEnv } from "./env.js";
import { admitCompanyIdentity, companyClaimsReader, companyProviderId, type LoginIdentity } from "./company-identity.js";
import { canUsePassword, passwordMode, principalAllowed, sessionAllowed } from "./auth-access.js";
import { auditIdentity } from "./identity-audit.js";
import { hasPlatformAdminRole } from "./context.js";
import { deploymentActions } from "@lightspeed-ai/platform-shared";
import { CLI_CLIENT_ID, DEVICE_AUTH_PATHS, deviceAuthorizationPlugin, deviceProtocolError, readDeviceBody } from "./device-authorization.js";

interface AuthRequest {
  identity?: LoginIdentity;
  actorId?: string;
  createEmergency?: boolean;
  cliSession?: boolean;
}

const servedPaths = new Set([
  "/get-session", "/sign-in/email", "/sign-in/social", "/sign-out", "/update-user", "/change-password",
  "/list-sessions", "/revoke-session", "/revoke-sessions", "/revoke-other-sessions",
  "/admin/list-users", "/admin/create-user", "/admin/update-user", "/admin/set-user-password",
  "/admin/ban-user", "/admin/unban-user", "/admin/revoke-user-sessions",
]);
const errorResponse = (message: string, status = 403) => Response.json({ message }, { status });

export function publicAuthConfig(env: ServerEnv) {
  return {
    sso: !!env.oidc,
    providerId: env.oidc ? companyProviderId(env.oidc) : null,
    password: passwordMode(env),
    autoSignIn: env.oidc?.autoSignIn ?? false,
  };
}

export function createAuth(db: Db, env: ServerEnv) {
  const requests = new AsyncLocalStorage<AuthRequest>();
  const keyAuthority = organizationKeyAuthority();
  const database = drizzleAdapter(db, { provider: "pg", schema, transaction: true });
  const oidc = env.oidc;
  const providerId = oidc ? companyProviderId(oidc) : undefined;
  const readClaims = oidc ? companyClaimsReader(oidc) : undefined;
  const auth = betterAuth({
    baseURL: env.baseUrl,
    secret: env.authSecret,
    basePath: "/api/auth",
    trustedOrigins: [...new Set([env.baseUrl, ...env.trustedOrigins])],
    database: (options: BetterAuthOptions) => keyAuthority.adapter(database(options)),
    // Provider errors can contain token responses; durable events below contain
    // only our action, outcome and identifiers.
    logger: { disabled: true },
    telemetry: { enabled: false },
    emailAndPassword: { enabled: passwordMode(env) !== "off", disableSignUp: true },
    session: {
      additionalFields: identitySessionFields,
      cookieCache: { enabled: false },
      expiresIn: Math.min(env.browserSessionIdleDays ?? 30, env.browserSessionMaxDays ?? 90) * 86400,
      updateAge: 86400,
      ...(oidc ? { expiresIn: oidc.sessionMaxAgeSeconds, disableSessionRefresh: true } : {}),
    },
    user: {
      additionalFields: identityUserFields,
      validateUserInfo: ({ source, user }) => {
        if (source.method === "oauth" && source.oauth?.providerId === providerId) {
          const identity = requests.getStore()?.identity;
          if (!identity || identity.source !== "company" || identity.userId !== user.id) return { error: "company_access_denied" };
        } else if (oidc && source.method !== "admin") return { error: "company_sign_in_required" };
      },
    },
    account: { accountLinking: { enabled: false }, storeAccountCookie: false },
    databaseHooks: {
      user: { create: { before: async (user) => ({ data: { ...user, emergencyAdmin: requests.getStore()?.createEmergency === true } }) } },
      account: {
        create: { before: async (account) => ({ data: account.providerId.startsWith("company-") ? { ...account, accessToken: null, refreshToken: null, idToken: null, accessTokenExpiresAt: null, refreshTokenExpiresAt: null } : account }) },
        update: { before: async (account) => ({ data: requests.getStore()?.identity?.source === "company" ? { ...account, accessToken: null, refreshToken: null, idToken: null, accessTokenExpiresAt: null, refreshTokenExpiresAt: null } : account }) },
      },
      session: { create: {
        before: async (session) => {
          const [user] = await db.select().from(schema.user).where(eq(schema.user.id, session.userId));
          const identity = requests.getStore()?.identity;
          if (!user || user.banned || (identity && (identity.userId !== user.id || identity.accessVersion !== user.accessVersion))) return false;
          if (user.identitySource === "company" && (!oidc || identity?.source !== "company" || !user.companyAdmitted || user.oidcIssuer !== oidc.issuer)) return false;
          if (oidc && user.identitySource !== "company" && (!canUsePassword(user, env) || identity?.source !== "password")) return false;
          const expiresAt = oidc ? new Date(Math.min(session.expiresAt.getTime(), (identity?.checkedAt.getTime() ?? Date.now()) + oidc.sessionMaxAgeSeconds * 1000)) : session.expiresAt;
          return { data: { ...session, expiresAt, accessVersion: identity?.accessVersion ?? user.accessVersion,
            sessionKind: requests.getStore()?.cliSession === true ? "cli" : "browser" } };
        },
        after: async (session) => {
          if (requests.getStore()?.cliSession) {
            await auditIdentity(db, { actorId: session.userId, action: "session.cli_sign_in", targetId: session.id });
            return;
          }
          const [user] = await db.select().from(schema.user).where(eq(schema.user.id, session.userId));
          if (user && requests.getStore()?.identity?.source === "password") {
            await auditIdentity(db, { actorId: user.id, action: oidc && user.emergencyAdmin ? "emergency.sign_in" : "password.sign_in", targetId: user.id });
          }
        },
      } },
    },
    ...(!oidc && env.github ? { socialProviders: { github: { clientId: env.github.clientId, clientSecret: env.github.clientSecret } } } : {}),
    plugins: [
      organization({ allowUserToCreateOrganization: false, creatorRole: "admin", schema: { organization: { additionalFields: organizationFields } } }), admin(), bearer(),
      apiKey(apiKeyConfigurations(env)),
      deviceAuthorizationPlugin(db, env, identity => {
        const state = requests.getStore();
        if (!state) throw new Error("Device session redemption requires the vetted HTTP entry point");
        state.identity = identity;
        state.cliSession = true;
      }),
      ...(oidc ? [genericOAuth({ config: [{
        providerId: providerId!, discoveryUrl: oidc.discoveryUrl,
        clientId: oidc.clientId, clientSecret: oidc.clientSecret,
        scopes: oidc.scopes, pkce: true, requireIdTokenVerification: true,
        disableProviderLogout: true,
        authorizationUrlParams: oidc.resource ? { resource: oidc.resource } : undefined,
        getUserInfo: async (tokens) => {
          const state = requests.getStore();
          if (!state) return null;
          try {
            const { identity, access } = await readClaims!(tokens);
            const admitted = await admitCompanyIdentity(db, oidc, identity, access);
            if (!admitted) return null;
            state.identity = admitted;
            return { ...identity, email: identity.email as string, name: typeof identity.name === "string" ? identity.name : identity.email as string, emailVerified: identity.email_verified === true };
          } catch {
            return null;
          }
        },
      }] })] : []),
    ],
  });

  const handler = async (request: Request): Promise<Response> => requests.run({}, async () => {
    const path = new URL(request.url).pathname.replace(/^\/api\/auth/, "");
    if (DEVICE_AUTH_PATHS.has(path)) {
      const publicDevice = path === "/device/code" || path === "/device/token";
      let approvalActor: string | undefined;
      if (request.headers.has("authorization")) return deviceProtocolError("invalid_request", "Device approval requires a browser session; code and token requests carry no authorization header");
      let body: Record<string, unknown> | null = null;
      if (request.method === "POST") body = await readDeviceBody(request);
      if (path === "/device" ? request.method !== "GET" : request.method !== "POST" || !body) return deviceProtocolError("invalid_request", "Invalid device request");
      const fields = path === "/device/code" ? ["client_id"] : path === "/device/token" ? ["client_id", "device_code", "grant_type"] : ["userCode"];
      if (body && (Object.keys(body).some(key => !fields.includes(key)) || fields.some(key => typeof body![key] !== "string" || !(body![key] as string).length))) return deviceProtocolError("invalid_request", "Invalid device request fields");
      if (publicDevice && body?.client_id !== CLI_CLIENT_ID) return deviceProtocolError("invalid_client", "Unknown client");
      if (body) {
        const headers = new Headers(request.headers);
        headers.delete("content-length");
        request = new Request(request.url, { method: "POST", headers, body: JSON.stringify(body) });
      }
      if (!publicDevice) {
        const current = await auth.api.getSession({ headers: request.headers });
        if (!current || current.session.sessionKind !== "browser" || !sessionAllowed(current, env) || !principalAllowed(current.user, env)) return deviceProtocolError("unauthorized", "Sign in before reviewing this request", 401);
        approvalActor = current.user.id;
        const origin = request.headers.get("origin");
        const site = request.headers.get("sec-fetch-site");
        if (site === "cross-site" || (origin && ![env.baseUrl, ...env.trustedOrigins].includes(origin)) || (request.method === "POST" && !origin && site !== "same-origin" && site !== "same-site")) return deviceProtocolError("access_denied", "Untrusted origin", 403);
        if (path === "/device/approve") {
          // GET /device must first claim the request for this exact browser user.
          const normalized = (body!.userCode as string).replace(/[^a-zA-Z0-9]/g, "").toUpperCase();
          const [claimed] = await db.update(schema.deviceCode).set({ approvalAccessVersion: current.user.accessVersion ?? 0 })
            .where(and(eq(schema.deviceCode.userCode, normalized), eq(schema.deviceCode.userId, current.user.id), eq(schema.deviceCode.status, "pending"))).returning({ id: schema.deviceCode.id });
          if (!claimed) return deviceProtocolError("access_denied", "Review and claim this code before approval", 403);
        }
      }
      const response = await auth.handler(request);
      if (response.ok && path === "/device/token" && requests.getStore()?.identity) await ensurePersonalOrganization(db, env, requests.getStore()!.identity!.userId);
      if (response.ok && (path === "/device/approve" || path === "/device/deny")) await auditIdentity(db, {
        actorId: approvalActor, action: path === "/device/approve" ? "session.cli_approve" : "session.cli_deny", details: { clientId: CLI_CLIENT_ID },
      });
      const headers = new Headers(response.headers);
      headers.set("cache-control", "no-store"); headers.set("pragma", "no-cache");
      return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
    }
    const callback = path === `/callback/${providerId}` || (!oidc && env.github && path === "/callback/github");
    if (!servedPaths.has(path) && !callback) return errorResponse("not found", 404);
    // Browser auth endpoints never accept bearer sessions. CLI sessions enter
    // only through the shared API, where provenance is checked explicitly.
    if (request.headers.has("authorization")) return errorResponse("Browser session required", 401);
    if (path === "/get-session") {
      const current = await auth.api.getSession({ headers: request.headers });
      if (!current || current.session.sessionKind !== "browser" || !sessionAllowed(current, env) || !principalAllowed(current.user, env)) return Response.json(null);
      return auth.handler(request);
    }
    let body: Record<string, any> = {};
    if (request.method === "POST" && !callback) {
      try { body = await request.clone().json() as Record<string, any>; } catch { return errorResponse("Invalid request", 400); }
      if (!body || typeof body !== "object" || Array.isArray(body)) return errorResponse("Invalid request", 400);
    }
    const publicPath = path === "/sign-in/email" || path === "/sign-in/social" || callback;
    const current = publicPath ? null : await auth.api.getSession({ headers: request.headers });
    if (!publicPath && (!current || current.session.sessionKind !== "browser" || !sessionAllowed(current, env) || !principalAllowed(current.user, env))) return errorResponse("Session expired or access unavailable", 401);
    const state = requests.getStore()!;
    state.actorId = current?.user.id;
    if (request.method !== "GET") {
      const origin = request.headers.get("origin");
      if (origin && ![env.baseUrl, ...env.trustedOrigins].includes(origin)) return errorResponse("Untrusted origin");
    }
    if (path.startsWith("/admin/") && !(current && deploymentActions(hasPlatformAdminRole(current.user)).has("manageUsers"))) return errorResponse("Deployment permission required: manageUsers");
    if (path === "/sign-in/email") {
      if (typeof body.email !== "string") return errorResponse("Invalid sign-in", 401);
      const [user] = await db.select().from(schema.user).where(eq(schema.user.email, body.email.toLowerCase()));
      if (!user || !canUsePassword(user, env)) return errorResponse("Invalid sign-in", 401);
      state.identity = { userId: user.id, accessVersion: user.accessVersion, checkedAt: new Date(), source: "password" };
    }
    if (path === "/sign-in/social") {
      if (body.idToken || body.provider !== (oidc ? providerId : "github") || (!oidc && !env.github)) return errorResponse("Unsupported sign-in");
      // Resource, scopes and protocol parameters belong to deployment config.
      body = { provider: body.provider, callbackURL: body.callbackURL, errorCallbackURL: body.errorCallbackURL };
      request = new Request(request.url, { method: "POST", headers: request.headers, body: JSON.stringify(body) });
    }
    if (path === "/admin/create-user") {
      if (passwordMode(env) === "off" && body.password) return errorResponse("Password login is disabled");
      const extra = body.data ?? {};
      if (Object.keys(extra).some((key) => key !== "emergencyAdmin")) return errorResponse("Unsupported account fields");
      state.createEmergency = extra.emergencyAdmin === true;
      if ((oidc && !state.createEmergency) || (state.createEmergency && (body.role !== "admin" || !body.password))) return errorResponse("Create a local emergency admin, or let company users sign in with SSO");
    }
    const targetId = path.startsWith("/admin/") ? body.userId : current?.user.id;
    const mutatingUser = ["/admin/update-user", "/admin/set-user-password", "/admin/ban-user", "/admin/unban-user", "/admin/revoke-user-sessions", "/change-password", "/update-user"].includes(path);
    if (mutatingUser) {
      if (typeof targetId !== "string") return errorResponse("User required", 400);
      const [target] = await db.select().from(schema.user).where(eq(schema.user.id, targetId));
      if (!target) return errorResponse("User not found", 404);
      if (["/admin/update-user", "/update-user"].includes(path)) {
        const data = path === "/admin/update-user" ? body.data ?? {} : body;
        const allowed = path === "/admin/update-user" ? ["name", "email", "emailVerified", "role"] : ["name", "image"];
        if (target.identitySource === "company" || Object.keys(data).some((key) => !allowed.includes(key))) return errorResponse("Company identity and access fields are managed by the provider");
        if (data.role && !["admin", "user"].includes(data.role)) return errorResponse("Invalid platform role", 400);
        if (data.role && (target.id === current?.user.id || target.emergencyAdmin && data.role !== "admin")) return errorResponse("Cannot demote this administrator");
      }
      if (["/change-password", "/admin/set-user-password"].includes(path) && !canUsePassword(target, env)) return errorResponse("Password access is unavailable for this account");
      if (path === "/change-password") state.identity = { userId: target.id, accessVersion: target.accessVersion, checkedAt: new Date(), source: "password" };
      if (["/admin/ban-user", "/admin/unban-user", "/admin/revoke-user-sessions"].includes(path)) {
        if (path === "/admin/ban-user" && target.id === current?.user.id) return errorResponse("Cannot suspend yourself");
        const action = path === "/admin/ban-user" ? "user.suspend" : path === "/admin/unban-user" ? "user.reinstate" : "session.revoke_all";
        const updated = await db.transaction(async (tx) => {
          const [row] = await tx.update(schema.user).set({
            accessVersion: sql`${schema.user.accessVersion} + 1`,
            ...(action === "user.suspend" ? { banned: true, banReason: "Suspended by a platform administrator", banExpires: null } : {}),
            ...(action === "user.reinstate" ? { banned: false, banReason: null, banExpires: null } : {}),
          }).where(eq(schema.user.id, target.id)).returning();
          await tx.delete(schema.session).where(eq(schema.session.userId, target.id));
          await auditIdentity(tx, { actorId: current!.user.id, action, targetId: target.id });
          return row;
        });
        return Response.json({ success: true, user: updated });
      }
    }
    const response = await auth.handler(request);
    if (callback) {
      const location = response.headers.get("location");
      if (location) {
        const redirect = new URL(location, env.baseUrl);
        if (redirect.searchParams.has("error")) {
          redirect.searchParams.delete("error_description");
          redirect.searchParams.set("error", "company_sign_in_failed");
          response.headers.set("location", redirect.href);
          await auditIdentity(db, { action: "company.sign_in", targetId: state.identity?.userId, outcome: "denied" });
        }
      }
    }
    if (response.ok && ["/admin/create-user", "/admin/update-user", "/admin/set-user-password"].includes(path)) {
      const created = path === "/admin/create-user" ? await response.clone().json() as { user?: { id?: string } } : undefined;
      await auditIdentity(db, { actorId: current!.user.id, action: path.slice(7).replaceAll("-", "_"), targetId: typeof targetId === "string" ? targetId : created?.user?.id });
      if (path === "/admin/set-user-password" && typeof targetId === "string") {
        await db.transaction(async (tx) => {
          await tx.update(schema.user).set({ accessVersion: sql`${schema.user.accessVersion} + 1` }).where(eq(schema.user.id, targetId));
          await tx.delete(schema.session).where(eq(schema.session.userId, targetId));
          await auditIdentity(tx, { actorId: current!.user.id, action: "session.revoke_all", targetId });
        });
      }
    }
    if (path === "/sign-in/email" && !response.ok && state.identity) {
      await auditIdentity(db, { action: "password.sign_in", targetId: state.identity.userId, outcome: "denied" });
    }
    if (response.ok && state.identity) await ensurePersonalOrganization(db, env, state.identity.userId);
    return response;
  });
  return { ...auth, handler, withOrganizationKeyAuthority: keyAuthority.run };
}

export type Auth = ReturnType<typeof createAuth>;
export type Session = Auth["$Infer"]["Session"];
