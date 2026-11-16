import { Hono, type Context } from "hono";
import { HTTPException } from "hono/http-exception";
import { and, eq, ne, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { PLATFORM_OPERATIONS, type AccountRole, type PlatformOperationId, type PlatformOperationParams, type PrincipalRef } from "@lightspeed-ai/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { enforceRestrictions, getAuth, type AuthContext } from "../auth-context.js";
import { accountForPrincipal, listAccountsForPrincipal, declaredPrincipalRole, requireAccountAdmin } from "../account-access.js";
import { auditAsSubject } from "../identity-audit.js";
import { deploymentClient, GateRefusal } from "../runtime-client.js";
import { LightspeedApiError } from "@lightspeed-ai/sdk";
import { policyBody, problem } from "../runtime-http.js";
import { adoptManagedUniverse, createManagedUniverse, requireAccountPrincipal, listUniverseAccesses, universeForPrincipal, updateUniverseLifecycle, type UniverseAccess } from "./universes.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
function accountView(account: typeof schema.organization.$inferSelect, role: AccountRole) { return { id: account.id, name: account.name, slug: account.slug, kind: account.kind, enabled: account.enabled, role, universeQuota: account.universeQuota, defaultProtected: account.defaultProtected, requireProtected: account.requireProtected, createdAt: account.createdAt, ...(role === "admin" ? { membershipPolicy: account.membershipPolicy } : {}) }; }
function applicationView(application: typeof schema.applications.$inferSelect) { return { id: application.id, accountId: application.organizationId, name: application.name, enabled: application.enabled, role: application.role, createdAt: application.createdAt }; }

export function platformRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.onError((error, c) => {
    if (error instanceof HTTPException || error instanceof GateRefusal) return problem(c, error.status, error.status === 404 ? "notFound" : error.status === 409 ? "conflict" : error.status === 400 || error.status === 413 ? "invalidRequest" : "forbidden", error.message);
    if (error instanceof LightspeedApiError) return new Response(JSON.stringify(error.payload), { status: error.status, headers: { "content-type": "application/problem+json" } });
    if ((error as { cause?: { code?: string } }).cause?.code === "23505") return problem(c, 409, "conflict", "resource already exists");
    console.error("Platform API failed", error); return problem(c, 500, "internal", "Platform request failed");
  });
  for (const [operation, spec] of Object.entries(PLATFORM_OPERATIONS)) {
    if (operation.startsWith("platform/session/") || operation.startsWith("platform/credentials/") || operation.includes("/credentials/")) continue;
    const path = spec.path.replace(/^\/api\/v1\/platform/, "").replace(/\{([^}]+)\}/g, ":$1");
    app.on(spec.method, path, async c => {
      const auth = getAuth(c);
      let body: Record<string, unknown> = {};
      if (!["GET", "DELETE"].includes(spec.method)) {
        try { body = await policyBody(c.req.raw); } catch (error) { if (error instanceof HTTPException) throw error; return problem(c, 400, "invalidRequest", "JSON object required"); }
        if (!body || typeof body !== "object" || Array.isArray(body)) return problem(c, 400, "invalidRequest", "JSON object required");
      }
      const parameters: Record<string, unknown> = { ...body };
      for (const match of spec.path.matchAll(/\{([^}]+)\}/g)) {
        if (Object.hasOwn(body, match[1]!)) return problem(c, 400, "invalidRequest", "path-owned field must not appear in body");
        parameters[match[1]!] = c.req.param(match[1]!);
      }
      const parsed = spec.params.safeParse(parameters);
      if (!parsed.success) return problem(c, 400, "invalidRequest", parsed.error.issues.map(issue => `${issue.path.join(".")}: ${issue.message}`).join("; "));
      if (auth.credential.restrictions.universeIds && operation.startsWith("platform/accounts/") && spec.method !== "GET") throw new GateRefusal(403, "universe-restricted credential cannot mutate account-wide resources");
      enforceRestrictions(auth, { operation, ...(typeof parameters.accountId === "string" ? { accountId: parameters.accountId } : {}), write: spec.method !== "GET" && typeof parameters.universe !== "string" });
      return c.json(await dispatch(ctx, auth, operation as PlatformOperationId, parsed.data as Record<string, unknown>));
    });
  }
  return app;
}

async function dispatch(ctx: AppContext, auth: AuthContext, operation: PlatformOperationId, params: Record<string, unknown>): Promise<unknown> {
  const accountId = params.accountId as string;
  switch (operation) {
    case "platform/accounts/list": {
      const accessible = await listAccountsForPrincipal(ctx, auth, true);
      const limits = auth.credential.restrictions;
      const canCreate = auth.principal.kind === "user" && (auth.principal.platformAdmin || ctx.env.allowSharedAccountCreation === true)
        && !limits.readOnly && !limits.accountIds && !limits.universeIds && (!limits.operations || limits.operations.includes("platform/accounts/create"));
      return { accounts: accessible.map(access => accountView(access.account, access.role)), canCreate };
    }
    case "platform/accounts/create": {
      if (!auth.principal.platformAdmin && !(ctx.env.allowSharedAccountCreation && auth.principal.kind === "user")) throw new GateRefusal(403, "shared-account creation is disabled for this principal");
      // A scoped credential cannot create a resource outside its declared account set.
      if (auth.credential.restrictions.accountIds || auth.credential.restrictions.universeIds) throw new GateRefusal(403, "credential cannot create accounts outside its scope");
      const input = params as PlatformOperationParams<"platform/accounts/create">;
      if (!auth.principal.platformAdmin && input.initialAdminUserId && input.initialAdminUserId !== auth.principal.id) throw new GateRefusal(403, "self-service account creators must remain the initial admin");
      if (!auth.principal.platformAdmin && input.kind === "development") throw new GateRefusal(403, "only a platform admin can provision development accounts");
      const adminId = input.initialAdminUserId ?? (auth.principal.kind === "user" ? auth.principal.id : undefined);
      if (!adminId) throw new GateRefusal(400, "initial account admin required");
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, adminId)).limit(1);
      if (!user || user.banned) throw new GateRefusal(400, "initial admin must be an active user");
      const account = await ctx.db.transaction(async tx => {
        const [created] = await tx.insert(schema.organization).values({ id: crypto.randomUUID(), name: input.name, slug: input.slug, kind: input.kind, enabled: true, createdAt: new Date(), universeQuota: input.universeQuota ?? ctx.env.sharedAccountUniverseQuota ?? 100, defaultProtected: input.defaultProtected ?? input.kind !== "development", requireProtected: input.requireProtected ?? false }).returning();
        await tx.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: created!.id, userId: adminId, role: "admin", createdAt: new Date() });
        await auditAsSubject(tx, auth, { action: "account.create", targetId: created!.id });
        return created!;
      });
      return { account: accountView(account, "admin") };
    }
    case "platform/accounts/read": {
      const access = await accountForPrincipal(ctx, auth, accountId, true); if (!access) throw new GateRefusal(404, "account not found");
      return { account: accountView(access.account, access.role) };
    }
    case "platform/accounts/update": {
      await requireAccountAdmin(ctx, auth, accountId);
      const { accountId: _, ...input } = params as PlatformOperationParams<"platform/accounts/update">;
      if (input.enabled !== undefined && !auth.principal.platformAdmin) throw new GateRefusal(403, "platform admin required to suspend an account");
      const account = await ctx.db.transaction(async tx => {
        const [current] = await tx.select().from(schema.organization).where(eq(schema.organization.id, accountId)).for("update");
        await requireAccountAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, accountId);
        if (input.requireProtected === true && !current?.requireProtected) {
          const universes = await tx.select().from(schema.universes).where(eq(schema.universes.organizationId, accountId));
          for (const universe of universes) {
            if (universe.status === "deleting") throw new GateRefusal(409, "wait for universe deletion before requiring protection");
            const state = universe.status === "provisioning" ? universe.runtimeConfiguration : (await deploymentClient(ctx.env, universe.gatewayUrl).call("deployment/universes/read", { universe: universe.lightspeedUniverseId }, { signal: AbortSignal.timeout(30000) })).universe;
            if (state.protected !== true || state.expiresAtMs != null) throw new GateRefusal(409, "protect every universe and clear expiry before requiring account protection");
          }
        }
        const [updated] = await tx.update(schema.organization).set(input).where(eq(schema.organization.id, accountId)).returning();
        await auditAsSubject(tx, auth, { action: "account.update", targetId: accountId });
        return updated!;
      });
      return { account: accountView(account, "admin") };
    }
    case "platform/accounts/members/list": {
      await requireAccountDirectory(ctx, auth, accountId);
      const account = (await accountForPrincipal(ctx, auth, accountId))!;
      const rows = await ctx.db.select({ user: schema.user, membership: schema.member }).from(schema.user).leftJoin(schema.member, and(eq(schema.member.userId, schema.user.id), eq(schema.member.organizationId, accountId)));
      const members = rows.flatMap(({ user, membership }) => {
        const manual = ["member", "creator", "admin"].includes(membership?.role ?? "") ? membership!.role as AccountRole : undefined;
        const role = declaredPrincipalRole(account.account.membershipPolicy, user, ["member", "creator", "admin"] as const, manual);
        return role ? [{ id: membership?.id ?? `declared:${user.id}`, userId: user.id, name: user.name, email: user.email, role, createdAt: membership?.createdAt ?? user.createdAt }] : [];
      });
      return { members };
    }
    case "platform/accounts/members/put": {
      await requireAccountAdmin(ctx, auth, accountId);
      const input = params as PlatformOperationParams<"platform/accounts/members/put">;
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, input.userId)).limit(1);
      if (!user || user.banned) throw new GateRefusal(404, "active user not found");
      const member = await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${accountId} FOR UPDATE`);
        await requireAccountAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, accountId);
        const [current] = await tx.select().from(schema.member).where(and(eq(schema.member.organizationId, accountId), eq(schema.member.userId, input.userId))).limit(1);
        if (current?.role === "admin" && input.role !== "admin") {
          const [other] = await tx.select({ id: schema.member.id }).from(schema.member).where(and(eq(schema.member.organizationId, accountId), eq(schema.member.role, "admin"), ne(schema.member.userId, input.userId))).limit(1);
          if (!other) throw new GateRefusal(409, "account must retain an explicit admin");
        }
        const [updated] = current ? await tx.update(schema.member).set({ role: input.role }).where(eq(schema.member.id, current.id)).returning() : await tx.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: accountId, userId: input.userId, role: input.role, createdAt: new Date() }).returning();
        await auditAsSubject(tx, auth, { action: "account.member.put", targetId: input.userId, details: { accountId, role: input.role ?? "member" } });
        return updated!;
      });
      return { member: { id: member.id, userId: member.userId, role: member.role, createdAt: member.createdAt, name: user.name, email: user.email } };
    }
    case "platform/accounts/members/delete": {
      await requireAccountAdmin(ctx, auth, accountId);
      await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${accountId} FOR UPDATE`);
        await requireAccountAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, accountId);
        const [current] = await tx.select().from(schema.member).where(and(eq(schema.member.organizationId, accountId), eq(schema.member.userId, params.userId as string))).limit(1);
        if (current?.role === "admin") {
          const [other] = await tx.select({ id: schema.member.id }).from(schema.member).where(and(eq(schema.member.organizationId, accountId), eq(schema.member.role, "admin"), ne(schema.member.id, current.id))).limit(1);
          if (!other) throw new GateRefusal(409, "account must retain an explicit admin");
        }
        if (current) await tx.delete(schema.member).where(eq(schema.member.id, current.id));
        await auditAsSubject(tx, auth, { action: "account.member.delete", targetId: params.userId as string, details: { accountId } });
      });
      return { ok: true };
    }
    case "platform/accounts/applications/list": {
      await requireAccountDirectory(ctx, auth, accountId);
      const applications = await ctx.db.select().from(schema.applications).where(eq(schema.applications.organizationId, accountId));
      return { applications: applications.map(applicationView) };
    }
    case "platform/accounts/applications/create": {
      await requireAccountAdmin(ctx, auth, accountId);
      const input = params as PlatformOperationParams<"platform/accounts/applications/create">;
      const [application] = await ctx.db.insert(schema.applications).values({ organizationId: accountId, name: input.name, role: input.role }).returning();
      await auditAsSubject(ctx.db, auth, { action: "application.create", targetId: application!.id, details: { accountId } });
      return { application: applicationView(application!) };
    }
    case "platform/accounts/applications/update": {
      await requireAccountAdmin(ctx, auth, accountId);
      const { accountId: _, applicationId, ...input } = params as PlatformOperationParams<"platform/accounts/applications/update">;
      const application = await ctx.db.transaction(async tx => {
        await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, accountId)).for("update");
        await requireAccountAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, accountId);
        const [updated] = await tx.update(schema.applications).set(input).where(and(eq(schema.applications.organizationId, accountId), eq(schema.applications.id, applicationId))).returning();
        return updated;
      });
      if (!application) throw new GateRefusal(404, "application not found");
      await auditAsSubject(ctx.db, auth, { action: "application.update", targetId: application.id, details: { accountId } });
      return { application: applicationView(application) };
    }
    case "platform/accounts/applications/delete": {
      await requireAccountAdmin(ctx, auth, accountId);
      // Suspension preserves attribution and Owner grants while immediately disabling all keys.
      await ctx.db.transaction(async tx => {
        await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, accountId)).for("update");
        await requireAccountAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, accountId);
        await tx.update(schema.applications).set({ enabled: false }).where(and(eq(schema.applications.organizationId, accountId), eq(schema.applications.id, params.applicationId as string)));
      });
      await auditAsSubject(ctx.db, auth, { action: "application.suspend", targetId: params.applicationId as string, details: { accountId } });
      return { ok: true };
    }
    case "platform/universes/adopt": return { universe: await adoptManagedUniverse(ctx, auth, accountId, params as PlatformOperationParams<"platform/universes/adopt">) };
    case "platform/universes/create": return { universe: await createManagedUniverse(ctx, auth, accountId, params as PlatformOperationParams<"platform/universes/create">) };
    case "platform/universes/update": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      return { universe: await updateUniverseLifecycle(ctx, access, params as PlatformOperationParams<"platform/universes/update">) };
    }
    case "platform/universes/grants/list": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      return { grants: await listGrants(ctx, access) };
    }
    case "platform/universes/grants/put": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      const input = params as PlatformOperationParams<"platform/universes/grants/put">;
      const target = await requireAccountPrincipal(ctx, access.account.id, input.principal);
      const grant = await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${access.account.id} FOR UPDATE`);
        await tx.execute(sql`SELECT id FROM universes WHERE id = ${access.universe.id} FOR UPDATE`);
        await ownerAccess({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, access.universe.id, operation);
        await requireAccountPrincipal({ ...ctx, db: tx as unknown as AppContext["db"] }, access.account.id, input.principal);
        const where = and(eq(schema.universeGrants.universeId, access.universe.id), input.principal.kind === "user" ? eq(schema.universeGrants.userId, input.principal.id) : eq(schema.universeGrants.applicationId, input.principal.id));
        const [current] = await tx.select().from(schema.universeGrants).where(where).limit(1);
        if (current?.role === "owner" && input.role !== "owner") await requireOtherOwner(tx, access.universe.id, current.id);
        const [updated] = current ? await tx.update(schema.universeGrants).set({ role: input.role }).where(eq(schema.universeGrants.id, current.id)).returning() : await tx.insert(schema.universeGrants).values({ universeId: access.universe.id, role: input.role, ...(input.principal.kind === "user" ? { userId: input.principal.id } : { applicationId: input.principal.id }) }).returning();
        await auditAsSubject(tx, auth, { action: "universe.grant.put", targetId: input.principal.id, universeId: access.universe.id, details: { role: input.role } });
        return updated!;
      });
      return { grant: { id: grant.id, universeId: grant.universeId, principal: input.principal, role: grant.role, name: target.name, createdAt: grant.createdAt } };
    }
    case "platform/universes/grants/delete": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${access.account.id} FOR UPDATE`);
        await tx.execute(sql`SELECT id FROM universes WHERE id = ${access.universe.id} FOR UPDATE`);
        await ownerAccess({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, access.universe.id, operation);
        const [grant] = await tx.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, access.universe.id), eq(schema.universeGrants.id, params.grantId as string))).limit(1);
        if (grant?.role === "owner") await requireOtherOwner(tx, access.universe.id, grant.id);
        if (grant) await tx.delete(schema.universeGrants).where(eq(schema.universeGrants.id, grant.id));
        await auditAsSubject(tx, auth, { action: "universe.grant.delete", targetId: params.grantId as string, universeId: access.universe.id });
      });
      return { ok: true };
    }
    case "platform/universes/access-policy/put": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      const { policy } = params as PlatformOperationParams<"platform/universes/access-policy/put">;
      await ctx.db.update(schema.universes).set({ accessPolicy: policy }).where(eq(schema.universes.id, access.universe.id));
      await auditAsSubject(ctx.db, auth, { action: "universe.access-policy.put", universeId: access.universe.id, targetId: access.universe.lightspeedUniverseId });
      return { policy };
    }
    default: throw new GateRefusal(404, "unknown Platform operation");
  }
}

async function ownerAccess(ctx: AppContext, auth: AuthContext, reference: string, operation: string) {
  const access = await universeForPrincipal(ctx, auth, reference);
  if (!access) throw new GateRefusal(404, "universe not found");
  enforceRestrictions(auth, { operation, accountId: access.account.id, universeId: access.universe.lightspeedUniverseId, write: !operation.endsWith("/list") });
  if (access.role !== "owner") throw new GateRefusal(403, "universe Owner required");
  return access;
}
async function requireAccountDirectory(ctx: AppContext, auth: AuthContext, accountId: string) {
  const account = await accountForPrincipal(ctx, auth, accountId); if (!account) throw new GateRefusal(404, "account not found");
  if (account.role === "admin") return;
  if ((await listUniverseAccesses(ctx, auth)).some(access => access.account.id === accountId && access.role === "owner")) return;
  throw new GateRefusal(403, "account admin or universe Owner required to browse account principals");
}
async function requireOtherOwner(db: Pick<AppContext["db"], "select">, universeId: string, grantId: string) {
  const [other] = await db.select({ id: schema.universeGrants.id }).from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, universeId), eq(schema.universeGrants.role, "owner"), ne(schema.universeGrants.id, grantId))).limit(1);
  if (!other) throw new GateRefusal(409, "universe must retain at least one explicit Owner");
}
async function listGrants(ctx: AppContext, access: UniverseAccess) {
  const rows = await ctx.db.select({ grant: schema.universeGrants, userName: schema.user.name, applicationName: schema.applications.name }).from(schema.universeGrants)
    .leftJoin(schema.user, eq(schema.user.id, schema.universeGrants.userId)).leftJoin(schema.applications, eq(schema.applications.id, schema.universeGrants.applicationId)).where(eq(schema.universeGrants.universeId, access.universe.id));
  return rows.map(({ grant, userName, applicationName }) => ({ id: grant.id, universeId: grant.universeId, principal: { kind: grant.userId ? "user" : "application", id: grant.userId ?? grant.applicationId! } as PrincipalRef, role: grant.role, name: userName ?? applicationName ?? "Removed principal", createdAt: grant.createdAt }));
}
