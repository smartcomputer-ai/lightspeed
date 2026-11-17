import { Hono, type Context } from "hono";
import { HTTPException } from "hono/http-exception";
import { and, count, eq, inArray, ne, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { PLATFORM_OPERATIONS, type OrganizationRole, type PlatformOperationId, type PlatformOperationParams, type PrincipalRef } from "@lightspeed-ai/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { enforceRestrictions, getAuth, type AuthContext } from "../auth-context.js";
import { organizationForPrincipal, listOrganizationsForPrincipal, declaredPrincipalRole, requireOrganizationAdmin } from "../organization-access.js";
import { auditAsSubject } from "../identity-audit.js";
import { deploymentClient, GateRefusal } from "../runtime-client.js";
import { LightspeedApiError } from "@lightspeed-ai/sdk";
import { policyBody, problem } from "../runtime-http.js";
import { adoptManagedUniverse, createManagedUniverse, requireOrganizationPrincipal, listUniverseAccesses, universeForPrincipal, updateUniverseLifecycle, type UniverseAccess } from "./universes.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
/** Usage and policy are administration details: only an Organization Admin's view carries them. */
function organizationView(organization: typeof schema.organization.$inferSelect, role: OrganizationRole, universeCount: number) { return { id: organization.id, name: organization.name, slug: organization.slug, personal: organization.personalUserId !== null, role, universeQuota: organization.universeQuota, defaultProtected: organization.defaultProtected, requireProtected: organization.requireProtected, createdAt: organization.createdAt, ...(role === "admin" ? { universeCount, membershipPolicy: organization.membershipPolicy } : {}) }; }
/** The same allocation the universe quota is enforced against: every row, including provisioning and deleting universes. */
async function universeCounts(db: AppContext["db"], organizationIds: string[]) {
  if (!organizationIds.length) return new Map<string, number>();
  const rows = await db.select({ organizationId: schema.universes.organizationId, total: count() }).from(schema.universes)
    .where(inArray(schema.universes.organizationId, organizationIds)).groupBy(schema.universes.organizationId);
  return new Map(rows.map(row => [row.organizationId, row.total]));
}
async function universeCount(db: AppContext["db"], organizationId: string) { return (await universeCounts(db, [organizationId])).get(organizationId) ?? 0; }
function serviceUserView(serviceUser: typeof schema.serviceUsers.$inferSelect) { return { id: serviceUser.id, organizationId: serviceUser.organizationId, name: serviceUser.name, enabled: serviceUser.enabled, role: serviceUser.role, createdAt: serviceUser.createdAt }; }

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
      if (auth.credential.restrictions.universeIds && operation.startsWith("platform/organizations/") && spec.method !== "GET") throw new GateRefusal(403, "universe-restricted credential cannot mutate organization-wide resources");
      enforceRestrictions(auth, { operation, ...(typeof parameters.organizationId === "string" ? { organizationId: parameters.organizationId } : {}), write: spec.method !== "GET" && typeof parameters.universe !== "string" });
      return c.json(await dispatch(ctx, auth, operation as PlatformOperationId, parsed.data as Record<string, unknown>));
    });
  }
  return app;
}

async function dispatch(ctx: AppContext, auth: AuthContext, operation: PlatformOperationId, params: Record<string, unknown>): Promise<unknown> {
  const organizationId = params.organizationId as string;
  switch (operation) {
    case "platform/organizations/list": {
      const accessible = await listOrganizationsForPrincipal(ctx, auth);
      const limits = auth.credential.restrictions;
      const canCreate = auth.principal.kind === "user" && (auth.principal.platformAdmin || ctx.env.allowSharedOrganizationCreation === true)
        && !limits.readOnly && !limits.organizationIds && !limits.universeIds && (!limits.operations || limits.operations.includes("platform/organizations/create"));
      const counts = await universeCounts(ctx.db, accessible.filter(access => access.role === "admin").map(access => access.organization.id));
      return { organizations: accessible.map(access => organizationView(access.organization, access.role, counts.get(access.organization.id) ?? 0)), canCreate };
    }
    case "platform/organizations/create": {
      if (!auth.principal.platformAdmin && !(ctx.env.allowSharedOrganizationCreation && auth.principal.kind === "user")) throw new GateRefusal(403, "shared-organization creation is disabled for this principal");
      // A scoped credential cannot create a resource outside its declared organization set.
      if (auth.credential.restrictions.organizationIds || auth.credential.restrictions.universeIds) throw new GateRefusal(403, "credential cannot create organizations outside its scope");
      const input = params as PlatformOperationParams<"platform/organizations/create">;
      if (!auth.principal.platformAdmin && input.initialAdminUserId && input.initialAdminUserId !== auth.principal.id) throw new GateRefusal(403, "self-service organization creators must remain the initial admin");
      const adminId = input.initialAdminUserId ?? (auth.principal.kind === "user" ? auth.principal.id : undefined);
      if (!adminId) throw new GateRefusal(400, "initial organization admin required");
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, adminId)).limit(1);
      if (!user || user.banned) throw new GateRefusal(400, "initial admin must be an active user");
      const organization = await ctx.db.transaction(async tx => {
        const [created] = await tx.insert(schema.organization).values({ id: crypto.randomUUID(), name: input.name, slug: input.slug, createdAt: new Date(), universeQuota: input.universeQuota ?? ctx.env.sharedOrganizationUniverseQuota ?? 100, defaultProtected: input.defaultProtected ?? true, requireProtected: input.requireProtected ?? false }).returning();
        await tx.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: created!.id, userId: adminId, role: "admin", createdAt: new Date() });
        await auditAsSubject(tx, auth, { action: "organization.create", targetId: created!.id });
        return created!;
      });
      return { organization: organizationView(organization, "admin", 0) };
    }
    case "platform/organizations/read": {
      const access = await organizationForPrincipal(ctx, auth, organizationId); if (!access) throw new GateRefusal(404, "organization not found");
      return { organization: organizationView(access.organization, access.role, access.role === "admin" ? await universeCount(ctx.db, organizationId) : 0) };
    }
    case "platform/organizations/update": {
      await requireOrganizationAdmin(ctx, auth, organizationId);
      const { organizationId: _, ...input } = params as PlatformOperationParams<"platform/organizations/update">;
      const organization = await ctx.db.transaction(async tx => {
        const [current] = await tx.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
        await requireOrganizationAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId);
        if (input.requireProtected === true && !current?.requireProtected) {
          const universes = await tx.select().from(schema.universes).where(eq(schema.universes.organizationId, organizationId));
          for (const universe of universes) {
            if (universe.status === "deleting") throw new GateRefusal(409, "wait for universe deletion before requiring protection");
            const state = universe.status === "provisioning" ? universe.runtimeConfiguration : (await deploymentClient(ctx.env).call("deployment/universes/read", { universe: universe.lightspeedUniverseId }, { signal: AbortSignal.timeout(30000) })).universe;
            if (state.protected !== true || state.expiresAtMs != null) throw new GateRefusal(409, "protect every universe and clear expiry before requiring organization protection");
          }
        }
        const [updated] = await tx.update(schema.organization).set(input).where(eq(schema.organization.id, organizationId)).returning();
        await auditAsSubject(tx, auth, { action: "organization.update", targetId: organizationId });
        return updated!;
      });
      return { organization: organizationView(organization, "admin", await universeCount(ctx.db, organizationId)) };
    }
    case "platform/organizations/members/list": {
      await requireOrganizationDirectory(ctx, auth, organizationId);
      const organization = (await organizationForPrincipal(ctx, auth, organizationId))!;
      const rows = await ctx.db.select({ user: schema.user, membership: schema.member }).from(schema.user).leftJoin(schema.member, and(eq(schema.member.userId, schema.user.id), eq(schema.member.organizationId, organizationId)));
      const members = rows.flatMap(({ user, membership }) => {
        const manual = ["member", "creator", "admin"].includes(membership?.role ?? "") ? membership!.role as OrganizationRole : undefined;
        const role = declaredPrincipalRole(organization.organization.membershipPolicy, user, ["member", "creator", "admin"] as const, manual);
        return role ? [{ id: membership?.id ?? `declared:${user.id}`, userId: user.id, name: user.name, email: user.email, role, createdAt: membership?.createdAt ?? user.createdAt }] : [];
      });
      return { members };
    }
    case "platform/organizations/members/put": {
      await requireOrganizationAdmin(ctx, auth, organizationId);
      const input = params as PlatformOperationParams<"platform/organizations/members/put">;
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, input.userId)).limit(1);
      if (!user || user.banned) throw new GateRefusal(404, "active user not found");
      const member = await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${organizationId} FOR UPDATE`);
        await requireOrganizationAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId);
        const [current] = await tx.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, input.userId))).limit(1);
        if (current?.role === "admin" && input.role !== "admin") {
          const [other] = await tx.select({ id: schema.member.id }).from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.role, "admin"), ne(schema.member.userId, input.userId))).limit(1);
          if (!other) throw new GateRefusal(409, "organization must retain an explicit admin");
        }
        const [updated] = current ? await tx.update(schema.member).set({ role: input.role }).where(eq(schema.member.id, current.id)).returning() : await tx.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: organizationId, userId: input.userId, role: input.role, createdAt: new Date() }).returning();
        await auditAsSubject(tx, auth, { action: "organization.member.put", targetId: input.userId, details: { organizationId, role: input.role ?? "member" } });
        return updated!;
      });
      return { member: { id: member.id, userId: member.userId, role: member.role, createdAt: member.createdAt, name: user.name, email: user.email } };
    }
    case "platform/organizations/members/delete": {
      await requireOrganizationAdmin(ctx, auth, organizationId);
      await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${organizationId} FOR UPDATE`);
        await requireOrganizationAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId);
        const [current] = await tx.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, params.userId as string))).limit(1);
        if (current?.role === "admin") {
          const [other] = await tx.select({ id: schema.member.id }).from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.role, "admin"), ne(schema.member.id, current.id))).limit(1);
          if (!other) throw new GateRefusal(409, "organization must retain an explicit admin");
        }
        if (current) await tx.delete(schema.member).where(eq(schema.member.id, current.id));
        await auditAsSubject(tx, auth, { action: "organization.member.delete", targetId: params.userId as string, details: { organizationId } });
      });
      return { ok: true };
    }
    case "platform/organizations/service-users/list": {
      await requireOrganizationDirectory(ctx, auth, organizationId);
      const serviceUsers = await ctx.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.organizationId, organizationId));
      return { serviceUsers: serviceUsers.map(serviceUserView) };
    }
    case "platform/organizations/service-users/create": {
      await requireOrganizationAdmin(ctx, auth, organizationId);
      const input = params as PlatformOperationParams<"platform/organizations/service-users/create">;
      const [serviceUser] = await ctx.db.insert(schema.serviceUsers).values({ organizationId: organizationId, name: input.name, role: input.role }).returning();
      await auditAsSubject(ctx.db, auth, { action: "serviceUser.create", targetId: serviceUser!.id, details: { organizationId } });
      return { serviceUser: serviceUserView(serviceUser!) };
    }
    case "platform/organizations/service-users/update": {
      await requireOrganizationAdmin(ctx, auth, organizationId);
      const { organizationId: _, serviceUserId, ...input } = params as PlatformOperationParams<"platform/organizations/service-users/update">;
      const serviceUser = await ctx.db.transaction(async tx => {
        await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
        await requireOrganizationAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId);
        const [updated] = await tx.update(schema.serviceUsers).set(input).where(and(eq(schema.serviceUsers.organizationId, organizationId), eq(schema.serviceUsers.id, serviceUserId))).returning();
        return updated;
      });
      if (!serviceUser) throw new GateRefusal(404, "service user not found");
      await auditAsSubject(ctx.db, auth, { action: "serviceUser.update", targetId: serviceUser.id, details: { organizationId } });
      return { serviceUser: serviceUserView(serviceUser) };
    }
    case "platform/organizations/service-users/delete": {
      await requireOrganizationAdmin(ctx, auth, organizationId);
      // Suspension preserves attribution and Owner grants while immediately disabling all keys.
      await ctx.db.transaction(async tx => {
        await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
        await requireOrganizationAdmin({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId);
        await tx.update(schema.serviceUsers).set({ enabled: false }).where(and(eq(schema.serviceUsers.organizationId, organizationId), eq(schema.serviceUsers.id, params.serviceUserId as string)));
      });
      await auditAsSubject(ctx.db, auth, { action: "serviceUser.suspend", targetId: params.serviceUserId as string, details: { organizationId } });
      return { ok: true };
    }
    case "platform/universes/adopt": return { universe: await adoptManagedUniverse(ctx, auth, organizationId, params as PlatformOperationParams<"platform/universes/adopt">) };
    case "platform/universes/create": return { universe: await createManagedUniverse(ctx, auth, organizationId, params as PlatformOperationParams<"platform/universes/create">) };
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
      const target = await requireOrganizationPrincipal(ctx, access.organization.id, input.principal);
      const grant = await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${access.organization.id} FOR UPDATE`);
        await tx.execute(sql`SELECT id FROM universes WHERE id = ${access.universe.id} FOR UPDATE`);
        await ownerAccess({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, access.universe.id, operation);
        await requireOrganizationPrincipal({ ...ctx, db: tx as unknown as AppContext["db"] }, access.organization.id, input.principal);
        const where = and(eq(schema.universeGrants.universeId, access.universe.id), input.principal.kind === "user" ? eq(schema.universeGrants.userId, input.principal.id) : eq(schema.universeGrants.serviceUserId, input.principal.id));
        const [current] = await tx.select().from(schema.universeGrants).where(where).limit(1);
        if (current?.role === "owner" && input.role !== "owner") await requireOtherOwner(tx, access.universe.id, current.id);
        const [updated] = current ? await tx.update(schema.universeGrants).set({ role: input.role }).where(eq(schema.universeGrants.id, current.id)).returning() : await tx.insert(schema.universeGrants).values({ universeId: access.universe.id, role: input.role, ...(input.principal.kind === "user" ? { userId: input.principal.id } : { serviceUserId: input.principal.id }) }).returning();
        await auditAsSubject(tx, auth, { action: "universe.grant.put", targetId: input.principal.id, universeId: access.universe.id, details: { role: input.role } });
        return updated!;
      });
      return { grant: { id: grant.id, universeId: grant.universeId, principal: input.principal, role: grant.role, name: target.name, createdAt: grant.createdAt } };
    }
    case "platform/universes/grants/delete": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${access.organization.id} FOR UPDATE`);
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
  enforceRestrictions(auth, { operation, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: !operation.endsWith("/list") });
  if (access.role !== "owner") throw new GateRefusal(403, "universe Owner required");
  return access;
}
async function requireOrganizationDirectory(ctx: AppContext, auth: AuthContext, organizationId: string) {
  const organization = await organizationForPrincipal(ctx, auth, organizationId); if (!organization) throw new GateRefusal(404, "organization not found");
  if (organization.role === "admin") return;
  if ((await listUniverseAccesses(ctx, auth)).some(access => access.organization.id === organizationId && access.role === "owner")) return;
  throw new GateRefusal(403, "organization admin or universe Owner required to browse organization principals");
}
async function requireOtherOwner(db: Pick<AppContext["db"], "select">, universeId: string, grantId: string) {
  const [other] = await db.select({ id: schema.universeGrants.id }).from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, universeId), eq(schema.universeGrants.role, "owner"), ne(schema.universeGrants.id, grantId))).limit(1);
  if (!other) throw new GateRefusal(409, "universe must retain at least one explicit Owner");
}
async function listGrants(ctx: AppContext, access: UniverseAccess) {
  const rows = await ctx.db.select({ grant: schema.universeGrants, userName: schema.user.name, serviceUserName: schema.serviceUsers.name }).from(schema.universeGrants)
    .leftJoin(schema.user, eq(schema.user.id, schema.universeGrants.userId)).leftJoin(schema.serviceUsers, eq(schema.serviceUsers.id, schema.universeGrants.serviceUserId)).where(eq(schema.universeGrants.universeId, access.universe.id));
  return rows.map(({ grant, userName, serviceUserName }) => ({ id: grant.id, universeId: grant.universeId, principal: { kind: grant.userId ? "user" : "serviceUser", id: grant.userId ?? grant.serviceUserId! } as PrincipalRef, role: grant.role, name: userName ?? serviceUserName ?? "Removed principal", createdAt: grant.createdAt }));
}
