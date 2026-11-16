import { and, eq, inArray } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { accountRoleSchema, type AccountRole, type UniverseRole } from "@lightspeed-ai/platform-shared";
import type { AuthContext } from "./auth-context.js";
import type { AppContext } from "./context.js";
import { HTTPException } from "hono/http-exception";

type Identity = typeof schema.user.$inferSelect;
type Policy<Role extends string> = { members?: { email?: string; issuer?: string; subject?: string; role: Role }[]; directoryGroupMappings?: { issuer: string; group: string; role: Role }[] };

/** Directory declarations participate in the same scoped role order as manual grants. */
export function declaredPrincipalRole<Role extends string>(policy: Policy<Role> | undefined, identity: Identity, roles: readonly Role[], manual?: Role): Role | undefined {
  if (identity.banned) return undefined;
  let role = manual;
  const grant = (candidate: Role) => { if (roles.includes(candidate) && (role === undefined || roles.indexOf(candidate) > roles.indexOf(role))) role = candidate; };
  const company = identity.identitySource === "company" && identity.companyAdmitted;
  for (const entry of policy?.members ?? []) {
    const email = entry.email && !entry.issuer && !entry.subject && identity.emailVerified && entry.email.toLowerCase() === identity.email.toLowerCase();
    const subject = !entry.email && entry.issuer && entry.subject && company && entry.issuer === identity.oidcIssuer && entry.subject === identity.oidcSubject;
    if (email || subject) grant(entry.role);
  }
  for (const mapping of policy?.directoryGroupMappings ?? []) if (company && mapping.issuer === identity.oidcIssuer && identity.directoryGroups.includes(mapping.group)) grant(mapping.role);
  return role;
}

export function resourceScopeAllowed(auth: AuthContext, accountId: string, universeId?: string): boolean {
  const limits = auth.credential.restrictions;
  return !(auth.principal.kind === "application" && auth.principal.accountId !== accountId)
    && (!limits.accountIds || limits.accountIds.includes(accountId))
    && (!universeId || !limits.universeIds || limits.universeIds.includes(universeId));
}

export async function accountForPrincipal(ctx: AppContext, auth: AuthContext, accountId: string, allowDisabled = false) {
  if (!resourceScopeAllowed(auth, accountId)) return null;
  if (auth.credential.restrictions.universeIds) {
    if (!auth.credential.restrictions.universeIds.length) return null;
    const [scoped] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes).where(and(eq(schema.universes.organizationId, accountId), inArray(schema.universes.lightspeedUniverseId, auth.credential.restrictions.universeIds))).limit(1);
    if (!scoped) return null;
  }
  const [account] = await ctx.db.select().from(schema.organization).where(eq(schema.organization.id, accountId)).limit(1);
  if (!account || (!account.enabled && !(allowDisabled && auth.principal.platformAdmin))) return null;
  if (auth.principal.platformAdmin) return { account, role: "admin" as AccountRole };
  if (auth.principal.kind === "application") {
    const [application] = await ctx.db.select().from(schema.applications).where(and(eq(schema.applications.id, auth.principal.id), eq(schema.applications.organizationId, accountId))).limit(1);
    return application?.enabled ? { account, role: application.role } : null;
  }
  const [membership] = await ctx.db.select().from(schema.member).where(and(eq(schema.member.organizationId, accountId), eq(schema.member.userId, auth.principal.id))).limit(1);
  const manual = accountRoleSchema.safeParse(membership?.role).data;
  const role = declaredPrincipalRole(account.membershipPolicy, auth.principal.user, ["member", "creator", "admin"] as const, manual);
  return role ? { account, role } : null;
}

export async function requireAccountAdmin(ctx: AppContext, auth: AuthContext, accountId: string) {
  const access = await accountForPrincipal(ctx, auth, accountId, true);
  if (!access) throw new HTTPException(404, { message: "account not found" });
  if (access.role !== "admin") throw new HTTPException(403, { message: "account admin required" });
  return access;
}

export function declaredUniverseRole(policy: Policy<UniverseRole>, identity: Identity, manual?: UniverseRole) {
  return declaredPrincipalRole(policy, identity, ["viewer", "contributor", "operator", "owner"] as const, manual);
}

/** Resolve account inventory with a bounded query count, including declarations.
 * Directory policies are evaluated locally; no query is issued per account. */
export async function listAccountsForPrincipal(ctx: AppContext, auth: AuthContext, allowDisabled = false) {
  const accounts = await ctx.db.select().from(schema.organization);
  const memberships = auth.principal.kind === "user" && !auth.principal.platformAdmin
    ? await ctx.db.select().from(schema.member).where(eq(schema.member.userId, auth.principal.id)) : [];
  const membershipRoles = new Map(memberships.map(member => [member.organizationId, accountRoleSchema.safeParse(member.role).data]));
  const application = auth.principal.kind === "application" ? (await ctx.db.select().from(schema.applications).where(eq(schema.applications.id, auth.principal.id)).limit(1))[0] : null;
  const limitedUniverses = auth.credential.restrictions.universeIds;
  const scopedAccounts = limitedUniverses ? new Set((limitedUniverses.length ? await ctx.db.select({ accountId: schema.universes.organizationId }).from(schema.universes).where(inArray(schema.universes.lightspeedUniverseId, limitedUniverses)) : []).map(universe => universe.accountId)) : null;
  return accounts.flatMap(account => {
    if (!resourceScopeAllowed(auth, account.id) || (scopedAccounts && !scopedAccounts.has(account.id)) || (!account.enabled && !(allowDisabled && auth.principal.platformAdmin))) return [];
    let role: AccountRole | undefined;
    if (auth.principal.platformAdmin) role = "admin";
    else if (auth.principal.kind === "application") role = application?.enabled && application.organizationId === account.id ? application.role : undefined;
    else role = declaredPrincipalRole(account.membershipPolicy, auth.principal.user, ["member", "creator", "admin"] as const, membershipRoles.get(account.id));
    return role ? [{ account, role }] : [];
  });
}
