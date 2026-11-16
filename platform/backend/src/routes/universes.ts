import { Hono, type Context } from "hono";
import { and, count, eq, inArray, or, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { LightspeedApiError } from "@lightspeed-ai/sdk";
import { accountUniverseAdoptSchema, accountUniverseCreateSchema, effectiveFeatures, mergeFeatureOverrides, universeHandleFromName, universeHandleSchema, universeUpdateSchema, type AccountRole, type PrincipalRef, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { z } from "zod";
import type { AppContext, ApiVariables } from "../context.js";
import { enforceRestrictions, getAuth, type AuthContext } from "../auth-context.js";
import { accountForPrincipal, listAccountsForPrincipal, declaredPrincipalRole, declaredUniverseRole, resourceScopeAllowed } from "../account-access.js";
import { auditAsSubject } from "../identity-audit.js";
import { deploymentClient, GateRefusal, type Member } from "../runtime-client.js";
import { requireAvailableCachedHandle, syncUniverseHandles } from "../universe-handles.js";
import { parseBody } from "../http.js";
import { withGateway } from "./gateway.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
type UniverseRow = typeof schema.universes.$inferSelect;
export interface UniverseAccess {
  universe: UniverseRow;
  handle: string;
  role: UniverseRole;
  accountRole: AccountRole;
  account: typeof schema.organization.$inferSelect;
  auth: AuthContext;
  member: Member;
}

export function universeView(access: UniverseAccess) {
  const { universe, role, accountRole } = access;
  const configuration = universe.runtimeConfiguration;
  return { id: universe.id, accountId: universe.organizationId, lightspeedUniverseId: universe.lightspeedUniverseId,
    name: universe.name, handle: access.handle, role, accountRole, status: universe.status,
    protected: configuration.protected === true, expiresAtMs: typeof configuration.expiresAtMs === "number" ? configuration.expiresAtMs : null,
    createdBy: universe.createdBy, deletion: configuration.deletion, features: effectiveFeatures(universe.features),
    icon: universe.icon, iconColor: universe.iconColor, gatewayUrl: universe.gatewayUrl, createdAt: universe.createdAt, updatedAt: universe.updatedAt,
    runtimeConfiguration: role === "owner" ? configuration : { displayName: configuration.displayName, protected: configuration.protected, expiresAtMs: configuration.expiresAtMs },
  };
}

export async function universeForPrincipal(ctx: AppContext, auth: AuthContext, reference: string, allowDeleting = false): Promise<UniverseAccess | null> {
  const uuid = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(reference);
  const [universe] = await ctx.db.select().from(schema.universes).where(uuid
    ? or(eq(schema.universes.id, reference), eq(schema.universes.lightspeedUniverseId, reference))
    : eq(schema.universes.handle, reference)).limit(1);
  if (!universe || (universe.status !== "active" && universe.status !== "archived" && !allowDeleting)
    || !resourceScopeAllowed(auth, universe.organizationId, universe.lightspeedUniverseId)) return null;
  const account = await accountForPrincipal(ctx, auth, universe.organizationId);
  if (!account) return null;
  let role: UniverseRole | undefined;
  if (account.role === "admin") role = "owner";
  else {
    const [grant] = await ctx.db.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, universe.id), auth.principal.kind === "user" ? eq(schema.universeGrants.userId, auth.principal.id) : eq(schema.universeGrants.applicationId, auth.principal.id))).limit(1);
    role = grant?.role;
    if (auth.principal.kind === "user") role = declaredUniverseRole(universe.accessPolicy, auth.principal.user, role);
  }
  if (!role) return null;
  return { universe, handle: typeof universe.runtimeConfiguration.deletionHandle === "string" ? universe.runtimeConfiguration.deletionHandle : universe.handle, role, accountRole: account.role, account: account.account, auth, member: { userId: auth.actorId, role } };
}
export async function universeForSession(ctx: AppContext, c: ApiContext, reference: string, allowDeleting = false) { return universeForPrincipal(ctx, getAuth(c), reference, allowDeleting); }
export const universeForRuntimeReference = universeForSession;

/** Accounts, candidates and explicit grants are each loaded once, regardless of
 * deployment size. A Creator's account membership alone never exposes rows. */
export async function listUniverseAccesses(ctx: AppContext, auth: AuthContext, allowInactive = false): Promise<UniverseAccess[]> {
  const accounts = await listAccountsForPrincipal(ctx, auth);
  if (!accounts.length) return [];
  const accountMap = new Map(accounts.map(access => [access.account.id, access]));
  const allowedIds = auth.credential.restrictions.universeIds;
  if (allowedIds && !allowedIds.length) return [];
  const rows = await ctx.db.select().from(schema.universes).where(and(inArray(schema.universes.organizationId, [...accountMap.keys()]), ...(allowedIds ? [inArray(schema.universes.lightspeedUniverseId, allowedIds)] : [])));
  const grants = await ctx.db.select().from(schema.universeGrants).where(auth.principal.kind === "user" ? eq(schema.universeGrants.userId, auth.principal.id) : eq(schema.universeGrants.applicationId, auth.principal.id));
  const grantRoles = new Map(grants.map(grant => [grant.universeId, grant.role]));
  return rows.flatMap(universe => {
    if (!allowInactive && universe.status !== "active" && universe.status !== "archived") return [];
    const account = accountMap.get(universe.organizationId)!;
    let role: UniverseRole | undefined = account.role === "admin" ? "owner" : grantRoles.get(universe.id);
    if (account.role !== "admin" && auth.principal.kind === "user") role = declaredUniverseRole(universe.accessPolicy, auth.principal.user, role);
    if (!role) return [];
    return [{ universe, handle: typeof universe.runtimeConfiguration.deletionHandle === "string" ? universe.runtimeConfiguration.deletionHandle : universe.handle, role, accountRole: account.role, account: account.account, auth, member: { userId: auth.actorId, role } }];
  });
}
export async function listAccessibleUniverses(ctx: AppContext, auth: AuthContext) { return (await listUniverseAccesses(ctx, auth, true)).map(universeView); }

/** Explicit grants can name only active principals in the containing account. */
export async function requireAccountPrincipal(ctx: AppContext, accountId: string, principal: PrincipalRef) {
  const [account] = await ctx.db.select().from(schema.organization).where(eq(schema.organization.id, accountId)).limit(1);
  if (!account?.enabled) throw new GateRefusal(404, "account not found");
  if (principal.kind === "application") {
    const [application] = await ctx.db.select().from(schema.applications).where(and(eq(schema.applications.id, principal.id), eq(schema.applications.organizationId, accountId))).limit(1);
    if (!application?.enabled) throw new GateRefusal(400, "application must be enabled in the containing account");
    return { name: application.name };
  }
  const [user] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, principal.id)).limit(1);
  if (!user || user.banned) throw new GateRefusal(400, "user must be active in the containing account");
  const [membership] = await ctx.db.select().from(schema.member).where(and(eq(schema.member.organizationId, accountId), eq(schema.member.userId, user.id))).limit(1);
  // Account declarations count as membership, just like manually assigned roles.
  if (!membership && !declaredPrincipalRole(account.membershipPolicy, user, ["member", "creator", "admin"] as const)) throw new GateRefusal(400, "principal must first join the containing account");
  return { name: user.name };
}

export async function createManagedUniverse(ctx: AppContext, auth: AuthContext, accountId: string, input: z.infer<typeof accountUniverseCreateSchema>) {
  enforceRestrictions(auth, { operation: "platform/universes/create", accountId, write: true, createUniverse: true });
  const access = await accountForPrincipal(ctx, auth, accountId);
  if (!access) throw new GateRefusal(404, "account not found");
  if (access.role === "member") throw new GateRefusal(403, "account creator role required");
  if (auth.principal.platformAdmin && !input.initialOwner) throw new GateRefusal(400, "provisioning requires an explicit initial Owner");
  const owner = input.initialOwner ?? { kind: auth.principal.kind, id: auth.principal.id };
  if ((owner.kind !== auth.principal.kind || owner.id !== auth.principal.id) && access.role !== "admin") throw new GateRefusal(403, "account admin required to appoint another initial Owner");
  await requireAccountPrincipal(ctx, accountId, owner);
  const protectedValue = input.protected ?? (access.account.requireProtected || access.account.defaultProtected);
  if (access.account.requireProtected && !protectedValue) throw new GateRefusal(403, "account policy requires deletion protection");
  if (protectedValue && input.expiresAtMs != null) throw new GateRefusal(409, "protected universes cannot expire");
  const runtimeId = input.universeId ?? crypto.randomUUID();
  const client = deploymentClient(ctx.env);
  const [prior] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtimeId)).limit(1);
  if (prior && (prior.organizationId !== accountId || prior.createdBy !== auth.actorId)) throw new GateRefusal(409, "universe creation identifier already belongs to another request");
  if (!prior) {
    try { await client.call("deployment/universes/read", { universe: runtimeId }); throw new GateRefusal(409, "runtime universe already exists; use administrative adoption"); }
    catch (error) { if (!(error instanceof LightspeedApiError && error.kind === "notFound")) throw error; }
  }
  const handle = prior?.handle ?? input.handle ?? universeHandleFromName(input.name);
  if (!prior) await requireAvailableCachedHandle(ctx, handle);
  const row = await ctx.db.transaction(async tx => {
    const [lockedAccount] = await tx.select().from(schema.organization).where(eq(schema.organization.id, accountId)).for("update");
    if (!lockedAccount?.enabled) throw new GateRefusal(404, "account not found");
    if (lockedAccount.requireProtected && input.protected === false) throw new GateRefusal(403, "account policy requires deletion protection");
    const reservationProtected = input.protected ?? (lockedAccount.requireProtected || lockedAccount.defaultProtected);
    if (reservationProtected && input.expiresAtMs != null) throw new GateRefusal(409, "protected universes cannot expire");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const currentAccess = await accountForPrincipal(scopedContext, auth, accountId);
    if (!currentAccess || currentAccess.role === "member") throw new GateRefusal(403, "account creation authority was revoked");
    await requireAccountPrincipal(scopedContext, accountId, owner);
    const [reserved] = await tx.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtimeId)).limit(1);
    if (reserved) {
      if (reserved.organizationId !== accountId || reserved.createdBy !== auth.actorId) throw new GateRefusal(409, "universe creation identifier already belongs to another request");
      return reserved;
    }
    const [allocation] = await tx.select({ total: count() }).from(schema.universes).where(eq(schema.universes.organizationId, accountId));
    if ((allocation?.total ?? 0) >= lockedAccount.universeQuota) throw new GateRefusal(409, "account universe quota reached");
    const [universe] = await tx.insert(schema.universes).values({ organizationId: accountId, lightspeedUniverseId: runtimeId, name: input.name, handle, createdBy: auth.actorId, status: "provisioning", runtimeConfiguration: { metadata: input.metadata ?? {}, protected: reservationProtected, expiresAtMs: input.expiresAtMs ?? null } }).returning();
    await tx.insert(schema.universeGrants).values({ universeId: universe!.id, role: "owner", ...(owner.kind === "user" ? { userId: owner.id } : { applicationId: owner.id }) });
    await auditAsSubject(tx, auth, { action: "universe.create", targetId: runtimeId, universeId: universe!.id, details: { accountId } });
    return universe!;
  });
  return ctx.db.transaction(async tx => {
    const [account] = await tx.select().from(schema.organization).where(eq(schema.organization.id, accountId)).for("update");
    if (!account?.enabled) throw new GateRefusal(404, "account not found");
    const [reservation] = await tx.select().from(schema.universes).where(eq(schema.universes.id, row.id)).for("update");
    if (!reservation) throw new GateRefusal(409, "creation reservation no longer exists");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const currentAccess = await accountForPrincipal(scopedContext, auth, accountId);
    if (!currentAccess || currentAccess.role === "member") throw new GateRefusal(403, "account creation authority was revoked");
    const owners = await tx.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, row.id), eq(schema.universeGrants.role, "owner")));
    let activeOwner = false;
    for (const grant of owners) {
      try { await requireAccountPrincipal(scopedContext, accountId, grant.userId ? { kind: "user", id: grant.userId } : { kind: "application", id: grant.applicationId! }); activeOwner = true; break; }
      catch (error) { if (!(error instanceof GateRefusal)) throw error; }
    }
    if (!activeOwner) throw new GateRefusal(409, "creation requires an active explicit Owner; account administration must recover the grant");
    if (reservation.status === "provisioning") {
      const protection = account.requireProtected || reservation.runtimeConfiguration.protected === true;
      const expiresAtMs = protection ? null : typeof reservation.runtimeConfiguration.expiresAtMs === "number" ? reservation.runtimeConfiguration.expiresAtMs : null;
      const options = { headers: { "x-lightspeed-actor": auth.actorId }, signal: AbortSignal.timeout(30000) };
      const created = await client.call("deployment/universes/create", { universeId: runtimeId, handle: reservation.handle, metadata: reservation.runtimeConfiguration.metadata as Record<string, string> | undefined, protected: protection, expiresAtMs }, options);
      const configured = await client.call("deployment/universes/configuration/put", { universe: runtimeId, displayName: reservation.name, ...(account.requireProtected ? { protected: true } : {}) }, options);
      await tx.update(schema.universes).set({ status: "active", handle: created.universe.handle, runtimeConfiguration: { ...configured.universe } }).where(eq(schema.universes.id, row.id));
    }
    const linked = await universeForPrincipal(scopedContext, auth, row.id, true);
    if (!linked) throw new GateRefusal(403, "created universe is outside this credential's scope");
    return universeView(linked);
  });
}

export async function adoptManagedUniverse(ctx: AppContext, auth: AuthContext, accountId: string, input: z.infer<typeof accountUniverseAdoptSchema>) {
  enforceRestrictions(auth, { operation: "platform/universes/adopt", accountId, universeId: input.lightspeedUniverseId, write: true, createUniverse: true });
  if (!auth.principal.platformAdmin) throw new GateRefusal(403, "platform admin required to adopt an unlinked runtime universe");
  const account = await accountForPrincipal(ctx, auth, accountId);
  if (!account) throw new GateRefusal(404, "account not found");
  await requireAccountPrincipal(ctx, accountId, input.initialOwner);
  const client = deploymentClient(ctx.env);
  const { universe: runtime } = await client.call("deployment/universes/read", { universe: input.lightspeedUniverseId });
  if (runtime.deletion) throw new GateRefusal(409, "universe is being deleted");
  const [linked] = await ctx.db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtime.universeId)).limit(1);
  if (linked) throw new GateRefusal(409, "runtime universe already belongs to an account");
  await requireAvailableCachedHandle(ctx, runtime.handle);
    const row = await ctx.db.transaction(async tx => {
    const [locked] = await tx.select().from(schema.organization).where(eq(schema.organization.id, accountId)).for("update");
    if (!locked?.enabled) throw new GateRefusal(404, "account not found");
    const [allocation] = await tx.select({ total: count() }).from(schema.universes).where(eq(schema.universes.organizationId, accountId));
    if ((allocation?.total ?? 0) >= locked.universeQuota) throw new GateRefusal(409, "account universe quota reached");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    await requireAccountPrincipal(scopedContext, accountId, input.initialOwner);
    if (locked.requireProtected && !runtime.protected) await client.call("deployment/universes/configuration/put", { universe: runtime.universeId, protected: true }, { headers: { "x-lightspeed-actor": auth.actorId }, signal: AbortSignal.timeout(30000) });
    const [created] = await tx.insert(schema.universes).values({ organizationId: accountId, lightspeedUniverseId: runtime.universeId, name: input.name, handle: runtime.handle, createdBy: auth.actorId, runtimeConfiguration: { ...runtime, protected: runtime.protected || locked.requireProtected } }).returning();
    await tx.insert(schema.universeGrants).values({ universeId: created!.id, role: "owner", ...(input.initialOwner.kind === "user" ? { userId: input.initialOwner.id } : { applicationId: input.initialOwner.id }) });
    await auditAsSubject(tx, auth, { action: "universe.adopt", targetId: runtime.universeId, universeId: created!.id, details: { accountId } });
    return created!;
  });
  return universeView((await universeForPrincipal(ctx, auth, row.id))!);
}

export async function updateUniverseLifecycle(ctx: AppContext, access: UniverseAccess, input: { name?: string; status?: "active" | "archived"; metadata?: Record<string, string>; protected?: boolean; expiresAtMs?: number | null }, operation = "platform/universes/update") {
  enforceRestrictions(access.auth, { operation, accountId: access.account.id, universeId: access.universe.lightspeedUniverseId, write: true });
  return ctx.db.transaction(async tx => {
    const [account] = await tx.select().from(schema.organization).where(eq(schema.organization.id, access.account.id)).for("update");
    await tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.id, access.universe.id)).for("update");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const currentAccess = await universeForPrincipal(scopedContext, access.auth, access.universe.id);
    if (!account?.enabled || !currentAccess) throw new GateRefusal(404, "universe not found");
    if (currentAccess.role !== "owner") throw new GateRefusal(403, "universe Owner required");
    if (input.protected === false && (currentAccess.accountRole !== "admin" || account.requireProtected)) throw new GateRefusal(403, account.requireProtected ? "account policy requires deletion protection" : "only an account admin can clear deletion protection");
    const client = deploymentClient(ctx.env, access.universe.gatewayUrl);
    const options = { headers: { "x-lightspeed-actor": access.auth.actorId }, signal: AbortSignal.timeout(30000) };
    const current = await client.call("deployment/universes/read", { universe: access.universe.lightspeedUniverseId }, options);
    const protectedValue = input.protected ?? current.universe.protected;
    if (input.expiresAtMs != null && (protectedValue || account.requireProtected)) throw new GateRefusal(409, "protected universes cannot expire");
    const configuration = await client.call("deployment/universes/configuration/put", { universe: access.universe.lightspeedUniverseId, ...(input.name !== undefined ? { displayName: input.name } : {}), ...(input.protected !== undefined ? { protected: input.protected } : {}), ...(input.metadata !== undefined ? { metadata: input.metadata } : {}) }, options);
    if (input.expiresAtMs !== undefined) await client.call("deployment/universes/expiry/put", { universe: access.universe.lightspeedUniverseId, expiresAtMs: input.expiresAtMs }, options);
    const runtimeConfiguration = { ...configuration.universe, ...(input.expiresAtMs !== undefined ? { expiresAtMs: input.expiresAtMs } : {}) };
    const [updated] = await tx.update(schema.universes).set({ ...(input.name !== undefined ? { name: input.name } : {}), ...(input.status !== undefined ? { status: input.status } : {}), runtimeConfiguration }).where(eq(schema.universes.id, access.universe.id)).returning();
    await auditAsSubject(tx, access.auth, { action: "universe.update", targetId: access.universe.lightspeedUniverseId, universeId: access.universe.id });
    return universeView({ ...currentAccess, universe: updated! });
  });
}

export async function deleteManagedUniverse(ctx: AppContext, access: UniverseAccess, _actorId?: string) {
  enforceRestrictions(access.auth, { operation: "deployment/universes/delete", accountId: access.account.id, universeId: access.universe.lightspeedUniverseId, write: true });
  return ctx.db.transaction(async tx => {
    await tx.execute(sql`SELECT pg_advisory_xact_lock(731845920114)`);
    const [account] = await tx.select().from(schema.organization).where(eq(schema.organization.id, access.account.id)).for("update");
    await tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.id, access.universe.id)).for("update");
    const scopedContext = { ...ctx, db: tx as unknown as AppContext["db"] };
    const current = await universeForPrincipal(scopedContext, access.auth, access.universe.id, true);
    if (!current || !account?.enabled) throw new GateRefusal(404, "universe not found");
    if (current.role !== "owner") throw new GateRefusal(403, "universe Owner required");
    const client = deploymentClient(ctx.env, current.universe.gatewayUrl);
    if (current.universe.status === "provisioning") {
      try { await client.call("deployment/universes/read", { universe: current.universe.lightspeedUniverseId }, { signal: AbortSignal.timeout(30000) }); }
      catch (error) {
        if (!(error instanceof LightspeedApiError && (error.kind === "notFound" || error.status === 410))) throw error;
        await tx.delete(schema.universes).where(eq(schema.universes.id, current.universe.id));
        await auditAsSubject(tx, access.auth, { action: "universe.provisioning.cancel", targetId: current.universe.lightspeedUniverseId, universeId: current.universe.id });
        return { universeId: current.universe.lightspeedUniverseId, state: "deleted" as const };
      }
    }
    if (account.requireProtected) throw new GateRefusal(409, "account policy requires deletion protection");
    const result = await deploymentClient(ctx.env, current.universe.gatewayUrl).call("deployment/universes/delete", { universe: current.universe.lightspeedUniverseId }, { headers: { "x-lightspeed-actor": access.auth.actorId }, signal: AbortSignal.timeout(30000) });
    await auditAsSubject(tx, access.auth, { action: "universe.delete", targetId: current.universe.lightspeedUniverseId, universeId: current.universe.id });
    if (result.state === "deleted") await tx.delete(schema.universes).where(eq(schema.universes.id, current.universe.id));
    else await tx.update(schema.universes).set({ status: "deleting", handle: `_deleting-${current.universe.lightspeedUniverseId}`, runtimeConfiguration: { ...current.universe.runtimeConfiguration, deletionHandle: current.handle } }).where(eq(schema.universes.id, current.universe.id));
    return result;
  });
}

export async function renameManagedUniverse(ctx: AppContext, access: UniverseAccess, handle: string) {
  enforceRestrictions(access.auth, { operation: "deployment/universes/handle/put", accountId: access.account.id, universeId: access.universe.lightspeedUniverseId, write: true });
  if (access.role !== "owner") throw new GateRefusal(403, "universe Owner required");
  await requireAvailableCachedHandle(ctx, handle, access.universe.id);
  const result = await deploymentClient(ctx.env, access.universe.gatewayUrl).call("deployment/universes/handle/put", { universe: access.universe.lightspeedUniverseId, handle }, { headers: { "x-lightspeed-actor": access.auth.actorId }, signal: AbortSignal.timeout(30000) });
  const [updated] = await ctx.db.update(schema.universes).set({ handle: result.universe.handle }).where(eq(schema.universes.id, access.universe.id)).returning();
  await auditAsSubject(ctx.db, access.auth, { action: "universe.handle.put", targetId: access.universe.lightspeedUniverseId, universeId: access.universe.id });
  return { result, view: universeView({ ...access, universe: updated!, handle: result.universe.handle }) };
}

/** Browser composition uses the same account, Owner and lifecycle services. */
export function universeRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.get("/", c => withGateway(c, async () => c.json(await listAccessibleUniverses(ctx, getAuth(c)))));
  app.post("/sync-handles", c => withGateway(c, async () => { if (!getAuth(c).principal.platformAdmin) throw new GateRefusal(403, "platform admin required"); return c.json(await syncUniverseHandles(ctx)); }));
  app.post("/", c => withGateway(c, async () => {
    const body = await parseBody(c, accountUniverseCreateSchema.extend({ accountId: z.string().min(1) }));
    if (!body.ok) return body.response;
    return c.json(await createManagedUniverse(ctx, getAuth(c), body.data.accountId, body.data), 201);
  }));
  app.get("/reconcile", c => withGateway(c, async () => {
    if (!getAuth(c).principal.platformAdmin) throw new GateRefusal(403, "platform admin required");
    const rows = await ctx.db.select().from(schema.universes);
    const listed = (await deploymentClient(ctx.env).call("deployment/universes/list", {})).universes ?? [];
    const index = new Map(listed.map(universe => [universe.universeId, universe]));
    const linked = new Set(rows.map(row => row.lightspeedUniverseId));
    return c.json({ platform: rows.map(row => ({ id: row.id, accountId: row.organizationId, lightspeedUniverseId: row.lightspeedUniverseId, engine: row.gatewayUrl && row.gatewayUrl !== ctx.env.lightspeedApiUrl ? "unchecked" : index.has(row.lightspeedUniverseId) ? index.get(row.lightspeedUniverseId)?.deletion ? "deleting" : "ok" : "missing", deletion: index.get(row.lightspeedUniverseId)?.deletion })), orphans: listed.filter(universe => !universe.deletion && !linked.has(universe.universeId)) });
  }));
  app.post("/adopt", c => withGateway(c, async () => {
    const body = await parseBody(c, accountUniverseAdoptSchema.extend({ accountId: z.string().min(1) })); if (!body.ok) return body.response;
    return c.json(await adoptManagedUniverse(ctx, getAuth(c), body.data.accountId, body.data), 201);
  }));
  app.post("/:id/engine", c => withGateway(c, async () => {
    const auth = getAuth(c); if (!auth.principal.platformAdmin) throw new GateRefusal(403, "platform admin required");
    const access = await universeForSession(ctx, c, c.req.param("id"), true); if (!access) throw new GateRefusal(404, "universe not found");
    return ctx.db.transaction(async tx => {
      await tx.select({ id: schema.organization.id }).from(schema.organization).where(eq(schema.organization.id, access.account.id)).for("update");
      await tx.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.id, access.universe.id)).for("update");
      const current = await universeForPrincipal({ ...ctx, db: tx as unknown as AppContext["db"] }, auth, access.universe.id, true);
      if (!current) throw new GateRefusal(404, "universe not found");
      const client = deploymentClient(ctx.env, current.universe.gatewayUrl);
      const configuration = current.universe.runtimeConfiguration;
      const protectedValue = current.account.requireProtected || configuration.protected === true;
      const options = { headers: { "x-lightspeed-actor": auth.actorId }, signal: AbortSignal.timeout(30000) };
      const created = await client.call("deployment/universes/create", { universeId: current.universe.lightspeedUniverseId, handle: current.handle, protected: protectedValue, expiresAtMs: protectedValue ? null : typeof configuration.expiresAtMs === "number" ? configuration.expiresAtMs : null, metadata: configuration.metadata as Record<string, string> | undefined }, options);
      if (created.created) await client.call("deployment/universes/configuration/put", { universe: current.universe.lightspeedUniverseId, displayName: current.universe.name }, options);
      await tx.update(schema.universes).set({ handle: created.universe.handle, status: "active", runtimeConfiguration: { ...created.universe } }).where(eq(schema.universes.id, current.universe.id));
      await auditAsSubject(tx, auth, { action: "universe.repair", targetId: current.universe.lightspeedUniverseId, universeId: current.universe.id });
      return c.json({ created: created.created });
    });
  }));
  app.delete("/engine/:lightspeedUniverseId", c => withGateway(c, async () => {
    const auth = getAuth(c); if (!auth.principal.platformAdmin) throw new GateRefusal(403, "platform admin required");
    const runtimeId = c.req.param("lightspeedUniverseId");
    const [linked] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, runtimeId)).limit(1);
    if (linked) throw new GateRefusal(409, "delete linked universes through their account");
    const body = await parseBody(c, z.object({ handle: z.string() }).strict()); if (!body.ok) return body.response;
    const client = deploymentClient(ctx.env); const { universe } = await client.call("deployment/universes/read", { universe: runtimeId });
    if (body.data.handle !== universe.handle) throw new GateRefusal(400, "type the universe handle to confirm deletion");
    const result = await client.call("deployment/universes/delete", { universe: runtimeId }, { headers: { "x-lightspeed-actor": auth.actorId } });
    await auditAsSubject(ctx.db, auth, { action: "universe.orphan.delete", targetId: runtimeId });
    return c.json({ ok: true, purge: result });
  }));
  app.get("/:id", c => withGateway(c, async () => { const access = await universeForSession(ctx, c, c.req.param("id"), true); if (!access) throw new GateRefusal(404, "universe not found"); return c.json(universeView(access)); }));
  app.delete("/:id", c => withGateway(c, async () => { const access = await universeForSession(ctx, c, c.req.param("id"), true); if (!access) throw new GateRefusal(404, "universe not found"); return c.json({ ok: true, purge: await deleteManagedUniverse(ctx, access) }); }));
  app.put("/:id/handle", c => withGateway(c, async () => { const access = await universeForSession(ctx, c, c.req.param("id")); if (!access) throw new GateRefusal(404, "universe not found"); const body = await c.req.json() as { handle?: unknown }; const handle = universeHandleSchema.parse(body.handle); return c.json((await renameManagedUniverse(ctx, access, handle)).view); }));
  app.patch("/:id", c => withGateway(c, async () => {
    const access = await universeForSession(ctx, c, c.req.param("id")); if (!access) throw new GateRefusal(404, "universe not found");
    if (access.role !== "owner") throw new GateRefusal(403, "universe Owner required");
    const body = await parseBody(c, universeUpdateSchema); if (!body.ok) return body.response;
    const { name, gatewayUrl, ...fields } = body.data;
    if (gatewayUrl !== undefined && gatewayUrl !== null && gatewayUrl !== ctx.env.lightspeedApiUrl) throw new GateRefusal(400, "universe must use the configured runtime");
    if (name !== undefined) await updateUniverseLifecycle(ctx, access, { name });
    const [updated] = await ctx.db.update(schema.universes).set({ ...fields, ...(gatewayUrl !== undefined ? { gatewayUrl } : {}), ...(fields.features ? { features: mergeFeatureOverrides(access.universe.features, fields.features) } : {}) }).where(eq(schema.universes.id, access.universe.id)).returning();
    return c.json(universeView({ ...access, universe: updated! }));
  }));
  return app;
}
