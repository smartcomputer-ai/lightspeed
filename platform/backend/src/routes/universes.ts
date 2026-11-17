import { Hono, type Context } from "hono";
import { and, count, eq, inArray, or, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { LightspeedApiError } from "@lightspeed-ai/sdk";
import { UNIVERSE_ROLES, ORGANIZATION_ROLES, organizationRoleSchema, highestRole, universeActions, organizationUniverseAdoptSchema, organizationUniverseCreateSchema, effectiveFeatures, mergeFeatureOverrides, universeHandleFromName, universeHandleSchema, universeUpdateSchema, type OrganizationAction, type OrganizationRole, type PrincipalRef, type UniverseAction, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { z } from "zod";
import type { AppContext, ApiVariables } from "../context.js";
import { deploymentAllows, enforceRestrictions, getAuth, type AuthContext } from "../auth-context.js";
import { organizationForPrincipal, listOrganizationsForPrincipal, declaredPrincipalRole, declaredUniverseRole, resourceScopeAllowed, type OrganizationAccess } from "../organization-access.js";
import { auditAsSubject } from "../identity-audit.js";
import { deploymentClient, GateRefusal, type UniverseCaller } from "../runtime-client.js";
import { requireAvailableCachedHandle, syncUniverseHandles } from "../universe-handles.js";
import { parseBody, withApiErrors } from "../http.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
type UniverseRow = typeof schema.universes.$inferSelect;
export interface UniverseAccess {
  universe: UniverseRow;
  handle: string;
  /** The highest universe role held, for display. Decisions use `actions`. */
  role: UniverseRole;
  actions: ReadonlySet<UniverseAction>;
  organizationRole: OrganizationRole;
  organizationActions: ReadonlySet<OrganizationAction>;
  organization: typeof schema.organization.$inferSelect;
  auth: AuthContext;
  caller: UniverseCaller;
}

export function universeView(access: UniverseAccess) {
  const { universe, role, organizationRole } = access;
  const configuration = universe.runtimeConfiguration;
  return { id: universe.id, organizationId: universe.organizationId, lightspeedUniverseId: universe.lightspeedUniverseId,
    name: universe.name, handle: access.handle, role, organizationRole, status: universe.status,
    actions: [...access.actions], organizationActions: [...access.organizationActions],
    protected: configuration.protected === true, expiresAtMs: typeof configuration.expiresAtMs === "number" ? configuration.expiresAtMs : null,
    createdBy: universe.createdBy, deletion: configuration.deletion, features: effectiveFeatures(universe.features),
    icon: universe.icon, iconColor: universe.iconColor, createdAt: universe.createdAt, updatedAt: universe.updatedAt,
    runtimeConfiguration: access.actions.has("administer") ? configuration : { displayName: configuration.displayName, protected: configuration.protected, expiresAtMs: configuration.expiresAtMs },
  };
}

export async function universeForPrincipal(ctx: AppContext, auth: AuthContext, reference: string, allowDeleting = false): Promise<UniverseAccess | null> {
  const uuid = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(reference);
  const [universe] = await ctx.db.select().from(schema.universes).where(uuid
    ? or(eq(schema.universes.id, reference), eq(schema.universes.lightspeedUniverseId, reference))
    : eq(schema.universes.handle, reference)).limit(1);
  if (!universe || (universe.status !== "active" && universe.status !== "archived" && !allowDeleting)
    || !resourceScopeAllowed(auth, universe.organizationId, universe.lightspeedUniverseId)) return null;
  const organization = await organizationForPrincipal(ctx, auth, universe.organizationId);
  if (!organization) return null;
  const [grant] = await ctx.db.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, universe.id), auth.principal.kind === "user" ? eq(schema.universeGrants.userId, auth.principal.id) : eq(schema.universeGrants.serviceUserId, auth.principal.id))).limit(1);
  return universeAccess(auth, universe, organization, grant?.role);
}

/** A principal's universe authority is the union of every role it holds
 * there: its explicit grant, the universe's declared access policy, the
 * organization's base role for a member person, and Owner through
 * organization administration. */
function universeAccess(auth: AuthContext, universe: UniverseRow, organization: OrganizationAccess, granted: UniverseRole | undefined): UniverseAccess | null {
  const roles: UniverseRole[] = granted ? [granted] : [];
  if (auth.principal.kind === "user") {
    const declared = declaredUniverseRole(universe.accessPolicy, auth.principal.user);
    if (declared) roles.push(declared);
    const base = organization.organization.baseUniverseRole;
    if (organization.member && base) roles.push(base);
  }
  if (organization.actions.has("administerUniverses")) roles.push("owner");
  const role = highestRole(UNIVERSE_ROLES, roles);
  if (!role) return null;
  const actions = universeActions(roles);
  return { universe, handle: typeof universe.runtimeConfiguration.deletionHandle === "string" ? universe.runtimeConfiguration.deletionHandle : universe.handle, role, actions, organizationRole: organization.role, organizationActions: organization.actions, organization: organization.organization, auth, caller: { actorId: auth.actorId, actions } };
}
export async function universeForRequest(ctx: AppContext, c: ApiContext, reference: string, allowDeleting = false) { return universeForPrincipal(ctx, getAuth(c), reference, allowDeleting); }

/** Organizations, candidates and explicit grants are each loaded once, regardless of
 * deployment size. Membership exposes a universe only through the organization's
 * base role; a Creator's authority to create never does. */
export async function listUniverseAccesses(ctx: AppContext, auth: AuthContext, allowInactive = false): Promise<UniverseAccess[]> {
  const organizations = await listOrganizationsForPrincipal(ctx, auth);
  if (!organizations.length) return [];
  const organizationMap = new Map(organizations.map(access => [access.organization.id, access]));
  const allowedIds = auth.credential.restrictions.universeIds;
  if (allowedIds && !allowedIds.length) return [];
  const rows = await ctx.db.select().from(schema.universes).where(and(inArray(schema.universes.organizationId, [...organizationMap.keys()]), ...(allowedIds ? [inArray(schema.universes.lightspeedUniverseId, allowedIds)] : [])));
  const grants = await ctx.db.select().from(schema.universeGrants).where(auth.principal.kind === "user" ? eq(schema.universeGrants.userId, auth.principal.id) : eq(schema.universeGrants.serviceUserId, auth.principal.id));
  const grantRoles = new Map(grants.map(grant => [grant.universeId, grant.role]));
  return rows.flatMap(universe => {
    if (!allowInactive && universe.status !== "active" && universe.status !== "archived") return [];
    const access = universeAccess(auth, universe, organizationMap.get(universe.organizationId)!, grantRoles.get(universe.id));
    return access ? [access] : [];
  });
}
export async function listAccessibleUniverses(ctx: AppContext, auth: AuthContext) { return (await listUniverseAccesses(ctx, auth, true)).map(universeView); }

/** Explicit grants can name only active principals in the containing organization. */
export async function requireOrganizationPrincipal(ctx: AppContext, organizationId: string, principal: PrincipalRef) {
  const [organization] = await ctx.db.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).limit(1);
  if (!organization) throw new GateRefusal(404, "organization not found");
  if (principal.kind === "serviceUser") {
    const [serviceUser] = await ctx.db.select().from(schema.serviceUsers).where(and(eq(schema.serviceUsers.id, principal.id), eq(schema.serviceUsers.organizationId, organizationId))).limit(1);
    if (!serviceUser?.enabled) throw new GateRefusal(400, "service user must be enabled in the containing organization");
    return { name: serviceUser.name };
  }
  const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, principal.id)).limit(1);
  if (!user || user.banned) throw new GateRefusal(400, "user must be active in the containing organization");
  const [membership] = await ctx.db.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, user.id))).limit(1);
  // Organization declarations count as membership, just like manually assigned roles.
  if (!membership && !declaredPrincipalRole(organization.membershipPolicy, user, ORGANIZATION_ROLES)) throw new GateRefusal(400, "principal must first join the containing organization");
  return { name: user.name };
}

type GrantRow = typeof schema.universeGrants.$inferSelect;

/** The grants whose principal is still active in the containing organization.
 * A person can leave through a directory change, with no removal to clean up
 * after, so an explicit grant outlives membership. Such a grant gives nothing
 * (universe access starts from organization access) and never counts as an
 * Owner. */
export async function activeGrantIds(db: Pick<AppContext["db"], "select">, organization: typeof schema.organization.$inferSelect, grants: GrantRow[]): Promise<Set<string>> {
  const userIds = grants.flatMap(grant => grant.userId ? [grant.userId] : []);
  const serviceUserIds = grants.flatMap(grant => grant.serviceUserId ? [grant.serviceUserId] : []);
  const [users, members, serviceUsers] = await Promise.all([
    userIds.length ? db.select().from(schema.user).where(inArray(schema.user.id, userIds)) : [],
    userIds.length ? db.select({ userId: schema.member.userId, role: schema.member.role }).from(schema.member).where(and(eq(schema.member.organizationId, organization.id), inArray(schema.member.userId, userIds))) : [],
    serviceUserIds.length ? db.select({ id: schema.serviceUsers.id }).from(schema.serviceUsers).where(and(eq(schema.serviceUsers.organizationId, organization.id), eq(schema.serviceUsers.enabled, true), inArray(schema.serviceUsers.id, serviceUserIds))) : [],
  ]);
  const manual = new Map(members.map(member => [member.userId, organizationRoleSchema.safeParse(member.role).data]));
  const active = new Set([
    ...users.filter(user => declaredPrincipalRole(organization.membershipPolicy, user, ORGANIZATION_ROLES, manual.get(user.id)) !== undefined).map(user => user.id),
    ...serviceUsers.map(serviceUser => serviceUser.id),
  ]);
  return new Set(grants.filter(grant => active.has(grant.userId ?? grant.serviceUserId!)).map(grant => grant.id));
}

export async function createManagedUniverse(ctx: AppContext, auth: AuthContext, organizationId: string, input: z.infer<typeof organizationUniverseCreateSchema>) {
  enforceRestrictions(auth, { operation: "platform/universes/create", organizationId, write: true, createUniverse: true });
  const access = await organizationForPrincipal(ctx, auth, organizationId);
  if (!access) throw new GateRefusal(404, "organization not found");
  if (!access.actions.has("createUniverse")) throw new GateRefusal(403, "organization permission required: createUniverse");
  // An administrator acting from outside the organization cannot be its Owner.
  if (!access.member && !input.initialOwner) throw new GateRefusal(400, "provisioning requires an explicit initial Owner");
  const owner = input.initialOwner ?? { kind: auth.principal.kind, id: auth.principal.id };
  if ((owner.kind !== auth.principal.kind || owner.id !== auth.principal.id) && !access.actions.has("administerUniverses")) throw new GateRefusal(403, "organization permission required to appoint another initial Owner: administerUniverses");
  await requireOrganizationPrincipal(ctx, organizationId, owner);
  const protectedValue = input.protected ?? (access.organization.requireProtected || access.organization.defaultProtected);
  if (access.organization.requireProtected && !protectedValue) throw new GateRefusal(403, "organization policy requires deletion protection");
  if (protectedValue && input.expiresAtMs != null) throw new GateRefusal(409, "protected universes cannot expire");
  const runtimeId = input.universeId ?? crypto.randomUUID();
  const client = deploymentClient(ctx.env);
  const [prior] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtimeId)).limit(1);
  if (prior && (prior.organizationId !== organizationId || prior.createdBy !== auth.actorId)) throw new GateRefusal(409, "universe creation identifier already belongs to another request");
  if (!prior) {
    try { await client.call("deployment/universes/read", { universe: runtimeId }); throw new GateRefusal(409, "runtime universe already exists; use administrative adoption"); }
    catch (error) { if (!(error instanceof LightspeedApiError && error.kind === "notFound")) throw error; }
  }
  const handle = prior?.handle ?? input.handle ?? universeHandleFromName(input.name);
  if (!prior) await requireAvailableCachedHandle(ctx, handle);
  const row = await ctx.db.transaction(async tx => {
    const [lockedOrganization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
    if (!lockedOrganization) throw new GateRefusal(404, "organization not found");
    if (lockedOrganization.requireProtected && input.protected === false) throw new GateRefusal(403, "organization policy requires deletion protection");
    const reservationProtected = input.protected ?? (lockedOrganization.requireProtected || lockedOrganization.defaultProtected);
    if (reservationProtected && input.expiresAtMs != null) throw new GateRefusal(409, "protected universes cannot expire");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const currentAccess = await organizationForPrincipal(scopedContext, auth, organizationId);
    if (!currentAccess?.actions.has("createUniverse")) throw new GateRefusal(403, "organization creation authority was revoked");
    await requireOrganizationPrincipal(scopedContext, organizationId, owner);
    const [reserved] = await tx.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtimeId)).limit(1);
    if (reserved) {
      if (reserved.organizationId !== organizationId || reserved.createdBy !== auth.actorId) throw new GateRefusal(409, "universe creation identifier already belongs to another request");
      return reserved;
    }
    const [allocation] = await tx.select({ total: count() }).from(schema.universes).where(eq(schema.universes.organizationId, organizationId));
    if ((allocation?.total ?? 0) >= lockedOrganization.universeQuota) throw new GateRefusal(409, "organization universe quota reached");
    const [universe] = await tx.insert(schema.universes).values({ organizationId: organizationId, lightspeedUniverseId: runtimeId, name: input.name, handle, createdBy: auth.actorId, status: "provisioning", runtimeConfiguration: { metadata: input.metadata ?? {}, protected: reservationProtected, expiresAtMs: input.expiresAtMs ?? null } }).returning();
    await tx.insert(schema.universeGrants).values({ universeId: universe!.id, role: "owner", ...(owner.kind === "user" ? { userId: owner.id } : { serviceUserId: owner.id }) });
    await auditAsSubject(tx, auth, { action: "universe.create", targetId: runtimeId, universeId: universe!.id, details: { organizationId } });
    return universe!;
  });
  return ctx.db.transaction(async tx => {
    const [organization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
    if (!organization) throw new GateRefusal(404, "organization not found");
    const [reservation] = await tx.select().from(schema.universes).where(eq(schema.universes.id, row.id)).for("update");
    if (!reservation) throw new GateRefusal(409, "creation reservation no longer exists");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const currentAccess = await organizationForPrincipal(scopedContext, auth, organizationId);
    if (!currentAccess?.actions.has("createUniverse")) throw new GateRefusal(403, "organization creation authority was revoked");
    const owners = await tx.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, row.id), eq(schema.universeGrants.role, "owner")));
    if (!(await activeGrantIds(tx, organization, owners)).size) throw new GateRefusal(409, "creation requires an active explicit Owner; organization administration must recover the grant");
    if (reservation.status === "provisioning") {
      const protection = organization.requireProtected || reservation.runtimeConfiguration.protected === true;
      const expiresAtMs = protection ? null : typeof reservation.runtimeConfiguration.expiresAtMs === "number" ? reservation.runtimeConfiguration.expiresAtMs : null;
      const options = { headers: { "x-lightspeed-actor": auth.actorId }, signal: AbortSignal.timeout(30000) };
      const created = await client.call("deployment/universes/create", { universeId: runtimeId, handle: reservation.handle, metadata: reservation.runtimeConfiguration.metadata as Record<string, string> | undefined, protected: protection, expiresAtMs }, options);
      const configured = await client.call("deployment/universes/configuration/put", { universe: runtimeId, displayName: reservation.name, ...(organization.requireProtected ? { protected: true } : {}) }, options);
      await tx.update(schema.universes).set({ status: "active", handle: created.universe.handle, runtimeConfiguration: { ...configured.universe } }).where(eq(schema.universes.id, row.id));
    }
    const linked = await universeForPrincipal(scopedContext, auth, row.id, true);
    if (!linked) throw new GateRefusal(403, "created universe is outside this credential's scope");
    return universeView(linked);
  });
}

export async function adoptManagedUniverse(ctx: AppContext, auth: AuthContext, organizationId: string, input: z.infer<typeof organizationUniverseAdoptSchema>) {
  enforceRestrictions(auth, { operation: "platform/universes/adopt", organizationId, universeId: input.lightspeedUniverseId, write: true, createUniverse: true });
  if (!deploymentAllows(auth, "manageUnclaimedUniverses")) throw new GateRefusal(403, "deployment permission required: manageUnclaimedUniverses");
  const organization = await organizationForPrincipal(ctx, auth, organizationId);
  if (!organization) throw new GateRefusal(404, "organization not found");
  await requireOrganizationPrincipal(ctx, organizationId, input.initialOwner);
  const client = deploymentClient(ctx.env);
  const { universe: runtime } = await client.call("deployment/universes/read", { universe: input.lightspeedUniverseId });
  if (runtime.deletion) throw new GateRefusal(409, "universe is being deleted");
  const [linked] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtime.universeId)).limit(1);
  if (linked) throw new GateRefusal(409, "runtime universe already belongs to an organization");
  await requireAvailableCachedHandle(ctx, runtime.handle);
    const row = await ctx.db.transaction(async tx => {
    const [locked] = await tx.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).for("update");
    if (!locked) throw new GateRefusal(404, "organization not found");
    const [allocation] = await tx.select({ total: count() }).from(schema.universes).where(eq(schema.universes.organizationId, organizationId));
    if ((allocation?.total ?? 0) >= locked.universeQuota) throw new GateRefusal(409, "organization universe quota reached");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    await requireOrganizationPrincipal(scopedContext, organizationId, input.initialOwner);
    if (locked.requireProtected && !runtime.protected) await client.call("deployment/universes/configuration/put", { universe: runtime.universeId, protected: true }, { headers: { "x-lightspeed-actor": auth.actorId }, signal: AbortSignal.timeout(30000) });
    const [created] = await tx.insert(schema.universes).values({ organizationId: organizationId, lightspeedUniverseId: runtime.universeId, name: input.name, handle: runtime.handle, createdBy: auth.actorId, runtimeConfiguration: { ...runtime, protected: runtime.protected || locked.requireProtected } }).returning();
    await tx.insert(schema.universeGrants).values({ universeId: created!.id, role: "owner", ...(input.initialOwner.kind === "user" ? { userId: input.initialOwner.id } : { serviceUserId: input.initialOwner.id }) });
    await auditAsSubject(tx, auth, { action: "universe.adopt", targetId: runtime.universeId, universeId: created!.id, details: { organizationId } });
    return created!;
  });
  return universeView((await universeForPrincipal(ctx, auth, row.id))!);
}

export async function updateUniverseLifecycle(ctx: AppContext, access: UniverseAccess, input: { name?: string; status?: "active" | "archived"; metadata?: Record<string, string>; protected?: boolean; expiresAtMs?: number | null }, operation = "platform/universes/update") {
  enforceRestrictions(access.auth, { operation, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: true });
  return ctx.db.transaction(async tx => {
    const [organization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, access.organization.id)).for("update");
    await tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.id, access.universe.id)).for("update");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const currentAccess = await universeForPrincipal(scopedContext, access.auth, access.universe.id);
    if (!organization || !currentAccess) throw new GateRefusal(404, "universe not found");
    if (!currentAccess.actions.has("administer")) throw new GateRefusal(403, "universe permission required: administer");
    if (input.protected === false && (!currentAccess.organizationActions.has("administerUniverses") || organization.requireProtected)) throw new GateRefusal(403, organization.requireProtected ? "organization policy requires deletion protection" : "only an organization admin can clear deletion protection");
    const client = deploymentClient(ctx.env);
    const options = { headers: { "x-lightspeed-actor": access.auth.actorId }, signal: AbortSignal.timeout(30000) };
    const current = await client.call("deployment/universes/read", { universe: access.universe.lightspeedUniverseId }, options);
    const protectedValue = input.protected ?? current.universe.protected;
    if (input.expiresAtMs != null && (protectedValue || organization.requireProtected)) throw new GateRefusal(409, "protected universes cannot expire");
    const configuration = await client.call("deployment/universes/configuration/put", { universe: access.universe.lightspeedUniverseId, ...(input.name !== undefined ? { displayName: input.name } : {}), ...(input.protected !== undefined ? { protected: input.protected } : {}), ...(input.metadata !== undefined ? { metadata: input.metadata } : {}) }, options);
    if (input.expiresAtMs !== undefined) await client.call("deployment/universes/expiry/put", { universe: access.universe.lightspeedUniverseId, expiresAtMs: input.expiresAtMs }, options);
    const runtimeConfiguration = { ...configuration.universe, ...(input.expiresAtMs !== undefined ? { expiresAtMs: input.expiresAtMs } : {}) };
    const [updated] = await tx.update(schema.universes).set({ ...(input.name !== undefined ? { name: input.name } : {}), ...(input.status !== undefined ? { status: input.status } : {}), runtimeConfiguration }).where(eq(schema.universes.id, access.universe.id)).returning();
    await auditAsSubject(tx, access.auth, { action: "universe.update", targetId: access.universe.lightspeedUniverseId, universeId: access.universe.id });
    return universeView({ ...currentAccess, universe: updated! });
  });
}

export async function deleteManagedUniverse(ctx: AppContext, access: UniverseAccess) {
  enforceRestrictions(access.auth, { operation: "deployment/universes/delete", organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: true });
  return ctx.db.transaction(async tx => {
    await tx.execute(sql`SELECT pg_advisory_xact_lock(731845920114)`);
    const [organization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, access.organization.id)).for("update");
    await tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.id, access.universe.id)).for("update");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const current = await universeForPrincipal(scopedContext, access.auth, access.universe.id, true);
    if (!current || !organization) throw new GateRefusal(404, "universe not found");
    if (!current.actions.has("administer")) throw new GateRefusal(403, "universe permission required: administer");
    const client = deploymentClient(ctx.env);
    if (current.universe.status === "provisioning") {
      try { await client.call("deployment/universes/read", { universe: current.universe.lightspeedUniverseId }, { signal: AbortSignal.timeout(30000) }); }
      catch (error) {
        if (!(error instanceof LightspeedApiError && (error.kind === "notFound" || error.status === 410))) throw error;
        await tx.delete(schema.universes).where(eq(schema.universes.id, current.universe.id));
        await auditAsSubject(tx, access.auth, { action: "universe.provisioning.cancel", targetId: current.universe.lightspeedUniverseId, universeId: current.universe.id });
        return { universeId: current.universe.lightspeedUniverseId, state: "deleted" as const };
      }
    }
    if (organization.requireProtected) throw new GateRefusal(409, "organization policy requires deletion protection");
    const result = await deploymentClient(ctx.env).call("deployment/universes/delete", { universe: current.universe.lightspeedUniverseId }, { headers: { "x-lightspeed-actor": access.auth.actorId }, signal: AbortSignal.timeout(30000) });
    await auditAsSubject(tx, access.auth, { action: "universe.delete", targetId: current.universe.lightspeedUniverseId, universeId: current.universe.id });
    if (result.state === "deleted") await tx.delete(schema.universes).where(eq(schema.universes.id, current.universe.id));
    else await tx.update(schema.universes).set({ status: "deleting", handle: `_deleting-${current.universe.lightspeedUniverseId}`, runtimeConfiguration: { ...current.universe.runtimeConfiguration, deletionHandle: current.handle } }).where(eq(schema.universes.id, current.universe.id));
    return result;
  });
}

export async function renameManagedUniverse(ctx: AppContext, access: UniverseAccess, handle: string) {
  enforceRestrictions(access.auth, { operation: "deployment/universes/handle/put", organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: true });
  if (!access.actions.has("administer")) throw new GateRefusal(403, "universe permission required: administer");
  await requireAvailableCachedHandle(ctx, handle, access.universe.id);
  const result = await deploymentClient(ctx.env).call("deployment/universes/handle/put", { universe: access.universe.lightspeedUniverseId, handle }, { headers: { "x-lightspeed-actor": access.auth.actorId }, signal: AbortSignal.timeout(30000) });
  const [updated] = await ctx.db.update(schema.universes).set({ handle: result.universe.handle }).where(eq(schema.universes.id, access.universe.id)).returning();
  await auditAsSubject(ctx.db, access.auth, { action: "universe.handle.put", targetId: access.universe.lightspeedUniverseId, universeId: access.universe.id });
  return { result, view: universeView({ ...access, universe: updated!, handle: result.universe.handle }) };
}

/** Browser composition uses the same organization, Owner and lifecycle services. */
export function universeRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.get("/", c => withApiErrors(c, async () => c.json(await listAccessibleUniverses(ctx, getAuth(c)))));
  app.post("/sync-handles", c => withApiErrors(c, async () => {
    if (!deploymentAllows(getAuth(c), "configureDeployment")) throw new GateRefusal(403, "deployment permission required: configureDeployment");
    let raw: unknown;
    try { const text = await c.req.text(); raw = text ? JSON.parse(text) : {}; }
    catch { return c.json({ error: "invalid JSON body" }, 400); }
    const body = z.object({ organizationId: z.string().min(1).optional() }).strict().safeParse(raw);
    if (!body.success) return c.json({ error: "validation failed", issues: body.error.issues }, 400);
    return c.json(await syncUniverseHandles(ctx, body.data.organizationId));
  }));
  app.post("/", c => withApiErrors(c, async () => {
    const body = await parseBody(c, organizationUniverseCreateSchema.extend({ organizationId: z.string().min(1) }));
    if (!body.ok) return body.response;
    return c.json(await createManagedUniverse(ctx, getAuth(c), body.data.organizationId, body.data), 201);
  }));
  app.get("/reconcile", c => withApiErrors(c, async () => {
    if (!deploymentAllows(getAuth(c), "manageUnclaimedUniverses")) throw new GateRefusal(403, "deployment permission required: manageUnclaimedUniverses");
    const organizationId = c.req.query("organizationId");
    if (organizationId === "") return c.json({ error: "organizationId must not be empty" }, 400);
    const rows = await ctx.db.select().from(schema.universes);
    const listed = (await deploymentClient(ctx.env).call("deployment/universes/list", {})).universes ?? [];
    const index = new Map(listed.map(universe => [universe.universeId, universe]));
    const linked = new Set(rows.map(row => row.lightspeedUniverseId));
    const selected = organizationId === undefined ? rows : rows.filter(row => row.organizationId === organizationId);
    return c.json({ platform: selected.map(row => ({ id: row.id, organizationId: row.organizationId, lightspeedUniverseId: row.lightspeedUniverseId, engine: index.has(row.lightspeedUniverseId) ? index.get(row.lightspeedUniverseId)?.deletion ? "deleting" : "ok" : "missing", deletion: index.get(row.lightspeedUniverseId)?.deletion })), orphans: organizationId === undefined ? listed.filter(universe => !universe.deletion && !linked.has(universe.universeId)) : [] });
  }));
  app.post("/adopt", c => withApiErrors(c, async () => {
    const body = await parseBody(c, organizationUniverseAdoptSchema.extend({ organizationId: z.string().min(1) })); if (!body.ok) return body.response;
    return c.json(await adoptManagedUniverse(ctx, getAuth(c), body.data.organizationId, body.data), 201);
  }));
  app.post("/:id/engine", c => withApiErrors(c, async () => {
    const auth = getAuth(c); if (!deploymentAllows(auth, "manageUnclaimedUniverses")) throw new GateRefusal(403, "deployment permission required: manageUnclaimedUniverses");
    const access = await universeForRequest(ctx, c, c.req.param("id"), true); if (!access) throw new GateRefusal(404, "universe not found");
    return ctx.db.transaction(async tx => {
      await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, access.organization.id)).for("update");
      await tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.id, access.universe.id)).for("update");
      const current = await universeForPrincipal({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, access.universe.id, true);
      if (!current) throw new GateRefusal(404, "universe not found");
      const client = deploymentClient(ctx.env);
      const configuration = current.universe.runtimeConfiguration;
      const protectedValue = current.organization.requireProtected || configuration.protected === true;
      const options = { headers: { "x-lightspeed-actor": auth.actorId }, signal: AbortSignal.timeout(30000) };
      const created = await client.call("deployment/universes/create", { universeId: current.universe.lightspeedUniverseId, handle: current.handle, protected: protectedValue, expiresAtMs: protectedValue ? null : typeof configuration.expiresAtMs === "number" ? configuration.expiresAtMs : null, metadata: configuration.metadata as Record<string, string> | undefined }, options);
      if (created.created) await client.call("deployment/universes/configuration/put", { universe: current.universe.lightspeedUniverseId, displayName: current.universe.name }, options);
      await tx.update(schema.universes).set({ handle: created.universe.handle, status: "active", runtimeConfiguration: { ...created.universe } }).where(eq(schema.universes.id, current.universe.id));
      await auditAsSubject(tx, auth, { action: "universe.repair", targetId: current.universe.lightspeedUniverseId, universeId: current.universe.id });
      return c.json({ created: created.created });
    });
  }));
  app.delete("/engine/:lightspeedUniverseId", c => withApiErrors(c, async () => {
    const auth = getAuth(c); if (!deploymentAllows(auth, "manageUnclaimedUniverses")) throw new GateRefusal(403, "deployment permission required: manageUnclaimedUniverses");
    const runtimeId = c.req.param("lightspeedUniverseId");
    const [linked] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtimeId)).limit(1);
    if (linked) throw new GateRefusal(409, "delete linked universes through their organization");
    const body = await parseBody(c, z.object({ handle: z.string() }).strict()); if (!body.ok) return body.response;
    const client = deploymentClient(ctx.env); const { universe } = await client.call("deployment/universes/read", { universe: runtimeId });
    if (body.data.handle !== universe.handle) throw new GateRefusal(400, "type the universe handle to confirm deletion");
    const result = await client.call("deployment/universes/delete", { universe: runtimeId }, { headers: { "x-lightspeed-actor": auth.actorId } });
    await auditAsSubject(ctx.db, auth, { action: "universe.orphan.delete", targetId: runtimeId });
    return c.json({ ok: true, purge: result });
  }));
  app.get("/:id", c => withApiErrors(c, async () => { const access = await universeForRequest(ctx, c, c.req.param("id"), true); if (!access) throw new GateRefusal(404, "universe not found"); return c.json(universeView(access)); }));
  app.delete("/:id", c => withApiErrors(c, async () => { const access = await universeForRequest(ctx, c, c.req.param("id"), true); if (!access) throw new GateRefusal(404, "universe not found"); return c.json({ ok: true, purge: await deleteManagedUniverse(ctx, access) }); }));
  app.put("/:id/handle", c => withApiErrors(c, async () => { const access = await universeForRequest(ctx, c, c.req.param("id")); if (!access) throw new GateRefusal(404, "universe not found"); const body = await c.req.json() as { handle?: unknown }; const handle = universeHandleSchema.parse(body.handle); return c.json((await renameManagedUniverse(ctx, access, handle)).view); }));
  app.patch("/:id", c => withApiErrors(c, async () => {
    const access = await universeForRequest(ctx, c, c.req.param("id")); if (!access) throw new GateRefusal(404, "universe not found");
    if (!access.actions.has("administer")) throw new GateRefusal(403, "universe permission required: administer");
    const body = await parseBody(c, universeUpdateSchema); if (!body.ok) return body.response;
    const { name, ...fields } = body.data;
    if (name !== undefined) await updateUniverseLifecycle(ctx, access, { name });
    const [updated] = await ctx.db.update(schema.universes).set({ ...fields, ...(fields.features ? { features: mergeFeatureOverrides(access.universe.features, fields.features) } : {}) }).where(eq(schema.universes.id, access.universe.id)).returning();
    return c.json(universeView({ ...access, universe: updated! }));
  }));
  return app;
}
