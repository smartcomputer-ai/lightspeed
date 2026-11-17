import { Hono, type Context } from "hono";
import { HTTPException } from "hono/http-exception";
import { and, count, eq, inArray, ne, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { ORGANIZATION_ROLES, PLATFORM_OPERATIONS, type OrganizationRole, type PlatformSettings, type PlatformOperationId, type PlatformOperationParams, type PrincipalRef } from "@lightspeed-ai/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { deploymentAllows, enforceRestrictions, getAuth, type AuthContext } from "../auth-context.js";
import { organizationForPrincipal, listOrganizationsForPrincipal, declaredPrincipalRole, refusePersonalOrganization, requireOrganizationAction, withOwnerNames, type OrganizationAccess } from "../organization-access.js";
import { auditAsSubject } from "../identity-audit.js";
import { effectiveUniverseQuota, listPlatformSettings, platformSettings, putPlatformSetting } from "../platform-settings.js";
import { deploymentClient, GateRefusal } from "../runtime-client.js";
import { LightspeedApiError } from "@lightspeed-ai/sdk";
import { policyBody, problem } from "../runtime-http.js";
import { activeGrantIds, universeView, adoptManagedUniverse, createManagedUniverse, requireOrganizationPrincipal, listUniverseAccesses, universeForPrincipal, updateUniverseLifecycle, type UniverseAccess, type UniverseReference } from "./universes.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
/** Usage and policy are administration details: only a caller who may manage the organization's policy sees them. */
function organizationView(organization: typeof schema.organization.$inferSelect, access: Pick<OrganizationAccess, "role" | "actions">, universeCount: number, settings: PlatformSettings) { return { id: organization.id, name: organization.name, handle: organization.slug, personal: organization.personalUserId !== null, role: access.role, actions: [...access.actions], universeQuota: effectiveUniverseQuota(organization, settings), universeQuotaSource: organization.universeQuota === null ? "deployment" as const : "organization" as const, canCreateUniverse: access.actions.has("createUniverse") && universeCount < effectiveUniverseQuota(organization, settings), baseUniverseRole: organization.baseUniverseRole, defaultProtected: organization.defaultProtected, requireProtected: organization.requireProtected, createdAt: organization.createdAt, ...(access.actions.has("managePolicy") ? { universeCount, membershipPolicy: organization.membershipPolicy } : {}) }; }
/** The same allocation the universe quota is enforced against: every row, including provisioning and deleting universes. */
async function universeCounts(db: AppContext["db"], organizationIds: string[]) {
  if (!organizationIds.length) return new Map<string, number>();
  const rows = await db.select({ organizationId: schema.universes.organizationId, total: count() }).from(schema.universes)
    .where(inArray(schema.universes.organizationId, organizationIds)).groupBy(schema.universes.organizationId);
  return new Map(rows.map(row => [row.organizationId, row.total]));
}
/** Usage is counted for those who see it (policy managers) and those it limits (creators). */
function countsUniverses(access: Pick<OrganizationAccess, "actions">) { return access.actions.has("managePolicy") || access.actions.has("createUniverse"); }
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

/** Deployment-wide settings: a Platform Admin with a credential not narrowed to some organizations or universes. */
function requireSettingsAuthority(auth: AuthContext) {
  if (!deploymentAllows(auth, "configureDeployment")) throw new GateRefusal(403, "deployment permission required: configureDeployment");
  if (auth.credential.restrictions.organizationIds || auth.credential.restrictions.universeIds) throw new GateRefusal(403, "credential is limited to some organizations or universes");
}

async function dispatch(ctx: AppContext, auth: AuthContext, operation: PlatformOperationId, params: Record<string, unknown>): Promise<unknown> {
  const organizationId = params.organizationId as string;
  switch (operation) {
    case "platform/settings/list": {
      requireSettingsAuthority(auth);
      return { settings: await listPlatformSettings(ctx.db, ctx.env) };
    }
    case "platform/settings/put": {
      requireSettingsAuthority(auth);
      const { key, value } = params as PlatformOperationParams<"platform/settings/put">;
      const setting = await putPlatformSetting(ctx.db, ctx.env, auth.actorId, key, value);
      await auditAsSubject(ctx.db, auth, { action: "platform.setting.put", targetId: key, details: { value: value === null ? null : String(value) } });
      return { setting };
    }
    case "platform/organizations/list": {
      const accessible = await listOrganizationsForPrincipal(ctx, auth);
      const limits = auth.credential.restrictions;
      const canCreate = deploymentAllows(auth, "createOrganization")
        && !limits.readOnly && !limits.organizationIds && !limits.universeIds && (!limits.operations || limits.operations.includes("platform/organizations/create"));
      const counts = await universeCounts(ctx.db, accessible.filter(access => countsUniverses(access)).map(access => access.organization.id));
      const named = await withOwnerNames(ctx.db, accessible.map(access => access.organization));
      const settings = await platformSettings(ctx.db, ctx.env);
      return { organizations: accessible.map((access, index) => organizationView(named[index]!, access, counts.get(access.organization.id) ?? 0, settings)), canCreate };
    }
    case "platform/organizations/create": {
      if (!deploymentAllows(auth, "createOrganization")) throw new GateRefusal(403, "deployment permission required: createOrganization");
      // A scoped credential cannot create a resource outside its declared organization set.
      if (auth.credential.restrictions.organizationIds || auth.credential.restrictions.universeIds) throw new GateRefusal(403, "credential cannot create organizations outside its scope");
      const input = params as PlatformOperationParams<"platform/organizations/create">;
      const adminId = input.initialAdminUserId ?? (auth.principal.kind === "user" ? auth.principal.id : undefined);
      if (!adminId) throw new GateRefusal(400, "initial organization admin required");
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, adminId)).limit(1);
      if (!user || user.banned) throw new GateRefusal(400, "initial admin must be an active user");
      const organization = await ctx.db.transaction(async tx => {
        const [created] = await tx.insert(schema.organization).values({ id: crypto.randomUUID(), name: input.name, slug: input.handle, createdAt: new Date(), universeQuota: input.universeQuota ?? null, baseUniverseRole: input.baseUniverseRole === undefined ? "viewer" : input.baseUniverseRole, defaultProtected: input.defaultProtected ?? true, requireProtected: input.requireProtected ?? false }).returning();
        await tx.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: created!.id, userId: adminId, role: "admin", createdAt: new Date() });
        await auditAsSubject(tx, auth, { action: "organization.create", targetId: created!.id });
        return created!;
      });
      const access = await organizationForPrincipal(ctx, auth, organization.id);
      if (!access) throw new GateRefusal(403, "created organization is outside this credential's scope");
      return { organization: organizationView(organization, access, 0, await platformSettings(ctx.db, ctx.env)) };
    }
    case "platform/organizations/read": {
      const access = await organizationForPrincipal(ctx, auth, organizationId); if (!access) throw new GateRefusal(404, "organization not found");
      const [named] = await withOwnerNames(ctx.db, [access.organization]);
      return { organization: organizationView(named!, access, countsUniverses(access) ? await universeCount(ctx.db, organizationId) : 0, await platformSettings(ctx.db, ctx.env)) };
    }
    case "platform/organizations/update": {
      const access = await requireOrganizationAction(ctx, auth, organizationId, "managePolicy");
      const { organizationId: _, ...input } = params as PlatformOperationParams<"platform/organizations/update">;
      if (input.universeQuota !== undefined && !deploymentAllows(auth, "setUniverseQuota")) throw new GateRefusal(403, "deployment permission required: setUniverseQuota");
      if (input.name !== undefined) await refusePersonalOrganization(ctx.db, organizationId, "name");
      if (input.membershipPolicy !== undefined) await refusePersonalOrganization(ctx.db, organizationId, "membership policy");
      const organization = await ctx.db.transaction(async tx => {
        const [current] = await tx.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
        await requireOrganizationAction({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId, "managePolicy");
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
      const [named] = await withOwnerNames(ctx.db, [organization]);
      return { organization: organizationView(named!, access, await universeCount(ctx.db, organizationId), await platformSettings(ctx.db, ctx.env)) };
    }
    case "platform/organizations/members/list": {
      await requireOrganizationDirectory(ctx, auth, organizationId);
      const organization = (await organizationForPrincipal(ctx, auth, organizationId))!;
      const rows = await ctx.db.select({ user: schema.user, membership: schema.member }).from(schema.user).leftJoin(schema.member, and(eq(schema.member.userId, schema.user.id), eq(schema.member.organizationId, organizationId)));
      const members = rows.flatMap(({ user, membership }) => {
        const manual = ["member", "creator", "admin"].includes(membership?.role ?? "") ? membership!.role as OrganizationRole : undefined;
        const declared = declaredPrincipalRole(organization.organization.membershipPolicy, user, ["member", "creator", "admin"] as const);
        const role = declaredPrincipalRole(organization.organization.membershipPolicy, user, ["member", "creator", "admin"] as const, manual);
        return role ? [{ id: membership?.id ?? `declared:${user.id}`, userId: user.id, name: user.name, email: user.email, role, added: manual !== undefined, declared: declared !== undefined, createdAt: membership?.createdAt ?? user.createdAt }] : [];
      });
      return { members };
    }
    case "platform/organizations/members/put": {
      const access = await requireOrganizationAction(ctx, auth, organizationId, "manageMembers");
      await refusePersonalOrganization(ctx.db, organizationId, "members");
      const input = params as PlatformOperationParams<"platform/organizations/members/put">;
      const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, input.userId)).limit(1);
      if (!user || user.banned) throw new GateRefusal(404, "active user not found");
      const member = await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${organizationId} FOR UPDATE`);
        await requireOrganizationAction({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId, "manageMembers");
        const [current] = await tx.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, input.userId))).limit(1);
        if (current?.role === "admin" && input.role !== "admin") {
          const [other] = await tx.select({ id: schema.member.id }).from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.role, "admin"), ne(schema.member.userId, input.userId))).limit(1);
          if (!other) throw new GateRefusal(409, "organization must retain an explicit admin");
        }
        const [updated] = current ? await tx.update(schema.member).set({ role: input.role }).where(eq(schema.member.id, current.id)).returning() : await tx.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: organizationId, userId: input.userId, role: input.role, createdAt: new Date() }).returning();
        await auditAsSubject(tx, auth, { action: "organization.member.put", targetId: input.userId, details: { organizationId, role: input.role ?? "member" } });
        return updated!;
      });
      return { member: { id: member.id, userId: member.userId, role: member.role, added: true, declared: declaredPrincipalRole(access.organization.membershipPolicy, user, ["member", "creator", "admin"] as const) !== undefined, createdAt: member.createdAt, name: user.name, email: user.email } };
    }
    case "platform/organizations/members/delete": {
      await requireOrganizationAction(ctx, auth, organizationId, "manageMembers");
      await refusePersonalOrganization(ctx.db, organizationId, "members");
      await ctx.db.transaction(async tx => {
        await tx.execute(sql`SELECT id FROM organization WHERE id = ${organizationId} FOR UPDATE`);
        await requireOrganizationAction({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId, "manageMembers");
        const [current] = await tx.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, params.userId as string))).limit(1);
        if (current?.role === "admin") {
          const [other] = await tx.select({ id: schema.member.id }).from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.role, "admin"), ne(schema.member.id, current.id))).limit(1);
          if (!other) throw new GateRefusal(409, "organization must retain an explicit admin");
        }
        if (current) await tx.delete(schema.member).where(eq(schema.member.id, current.id));
        // Roles given in this organization's universes go with the membership,
        // unless a directory declaration keeps the person a member.
        const [organization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).limit(1);
        const [user] = await tx.select().from(schema.user).where(eq(schema.user.id, params.userId as string)).limit(1);
        const declared = user && declaredPrincipalRole(organization!.membershipPolicy, user, ORGANIZATION_ROLES) !== undefined;
        const removedGrants = declared ? [] : await tx.delete(schema.universeGrants).where(and(eq(schema.universeGrants.userId, params.userId as string),
          inArray(schema.universeGrants.universeId, tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.organizationId, organizationId))))).returning({ id: schema.universeGrants.id });
        await auditAsSubject(tx, auth, { action: "organization.member.delete", targetId: params.userId as string, details: { organizationId, removedUniverseGrants: String(removedGrants.length) } });
      });
      return { ok: true };
    }
    case "platform/organizations/service-users/list": {
      await requireOrganizationDirectory(ctx, auth, organizationId);
      const serviceUsers = await ctx.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.organizationId, organizationId));
      return { serviceUsers: serviceUsers.map(serviceUserView) };
    }
    case "platform/organizations/service-users/create": {
      await requireOrganizationAction(ctx, auth, organizationId, "manageServiceUsers");
      const input = params as PlatformOperationParams<"platform/organizations/service-users/create">;
      const [serviceUser] = await ctx.db.insert(schema.serviceUsers).values({ organizationId: organizationId, name: input.name, role: input.role }).returning();
      await auditAsSubject(ctx.db, auth, { action: "serviceUser.create", targetId: serviceUser!.id, details: { organizationId } });
      return { serviceUser: serviceUserView(serviceUser!) };
    }
    case "platform/organizations/service-users/update": {
      await requireOrganizationAction(ctx, auth, organizationId, "manageServiceUsers");
      const { organizationId: _, serviceUserId, ...input } = params as PlatformOperationParams<"platform/organizations/service-users/update">;
      const serviceUser = await ctx.db.transaction(async tx => {
        await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
        await requireOrganizationAction({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, organizationId, "manageServiceUsers");
        const [updated] = await tx.update(schema.serviceUsers).set(input).where(and(eq(schema.serviceUsers.organizationId, organizationId), eq(schema.serviceUsers.id, serviceUserId))).returning();
        return updated;
      });
      if (!serviceUser) throw new GateRefusal(404, "service user not found");
      await auditAsSubject(ctx.db, auth, { action: "serviceUser.update", targetId: serviceUser.id, details: { organizationId } });
      return { serviceUser: serviceUserView(serviceUser) };
    }
    case "platform/universes/adopt": return { universe: await adoptManagedUniverse(ctx, auth, organizationId, params as PlatformOperationParams<"platform/universes/adopt">) };
    case "platform/universes/create": return { universe: await createManagedUniverse(ctx, auth, organizationId, params as PlatformOperationParams<"platform/universes/create">) };
    case "platform/universes/read": {
      const access = await universeForPrincipal(ctx, auth, params.universe as string);
      if (!access) throw new GateRefusal(404, "universe not found");
      enforceRestrictions(auth, { operation, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: false });
      return { universe: universeView(access) };
    }
    case "platform/universes/access-policy/read": {
      const access = await ownerAccess(ctx, auth, params.universe as string, operation);
      return { policy: access.universe.accessPolicy ?? {} };
    }
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
        await ownerAccess({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, { platformId: access.universe.id }, operation);
        await requireOrganizationPrincipal({ ...ctx, db: tx as unknown as AppContext["db"] }, access.organization.id, input.principal);
        const where = and(eq(schema.universeGrants.universeId, access.universe.id), input.principal.kind === "user" ? eq(schema.universeGrants.userId, input.principal.id) : eq(schema.universeGrants.serviceUserId, input.principal.id));
        const [current] = await tx.select().from(schema.universeGrants).where(where).limit(1);
        if (current?.role === "owner" && input.role !== "owner") await requireOtherOwner(tx, access, current.id);
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
        await ownerAccess({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, { platformId: access.universe.id }, operation);
        const [grant] = await tx.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, access.universe.id), eq(schema.universeGrants.id, params.grantId as string))).limit(1);
        if (grant?.role === "owner") await requireOtherOwner(tx, access, grant.id);
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

async function ownerAccess(ctx: AppContext, auth: AuthContext, reference: UniverseReference, operation: string) {
  const access = await universeForPrincipal(ctx, auth, reference);
  if (!access) throw new GateRefusal(404, "universe not found");
  enforceRestrictions(auth, { operation, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: !/\/(list|read)$/.test(operation) });
  if (!access.actions.has("administer")) throw new GateRefusal(403, "universe permission required: administer");
  return access;
}
async function requireOrganizationDirectory(ctx: AppContext, auth: AuthContext, organizationId: string) {
  const organization = await organizationForPrincipal(ctx, auth, organizationId); if (!organization) throw new GateRefusal(404, "organization not found");
  if (organization.actions.has("manageMembers")) return;
  // Choosing whom to grant a universe needs the organization's principals.
  if ((await listUniverseAccesses(ctx, auth)).some(access => access.organization.id === organizationId && access.actions.has("administer"))) return;
  throw new GateRefusal(403, "browsing organization principals requires manageMembers, or administer on one of its universes");
}
/** Only an Owner who is still in the organization keeps a universe owned. */
async function requireOtherOwner(db: Pick<AppContext["db"], "select">, access: UniverseAccess, grantId: string) {
  const others = await db.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, access.universe.id), eq(schema.universeGrants.role, "owner"), ne(schema.universeGrants.id, grantId)));
  if (!(await activeGrantIds(db, access.organization, others)).size) throw new GateRefusal(409, "universe must retain at least one explicit Owner who is an organization member");
}
async function listGrants(ctx: AppContext, access: UniverseAccess) {
  const rows = await ctx.db.select({ grant: schema.universeGrants, userName: schema.user.name, serviceUserName: schema.serviceUsers.name }).from(schema.universeGrants)
    .leftJoin(schema.user, eq(schema.user.id, schema.universeGrants.userId)).leftJoin(schema.serviceUsers, eq(schema.serviceUsers.id, schema.universeGrants.serviceUserId)).where(eq(schema.universeGrants.universeId, access.universe.id));
  const active = await activeGrantIds(ctx.db, access.organization, rows.map(row => row.grant));
  return rows.map(({ grant, userName, serviceUserName }) => ({ id: grant.id, universeId: grant.universeId, principal: { kind: grant.userId ? "user" : "serviceUser", id: grant.userId ?? grant.serviceUserId! } as PrincipalRef, role: grant.role, name: userName ?? serviceUserName ?? "Removed principal", active: active.has(grant.id), createdAt: grant.createdAt }));
}
