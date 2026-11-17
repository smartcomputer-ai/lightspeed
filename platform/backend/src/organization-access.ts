import { and, eq, inArray } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { organizationRoleSchema, type OrganizationRole, type UniverseRole } from "@lightspeed-ai/platform-shared";
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

export function resourceScopeAllowed(auth: AuthContext, organizationId: string, universeId?: string): boolean {
  const limits = auth.credential.restrictions;
  return !(auth.principal.kind === "serviceUser" && auth.principal.organizationId !== organizationId)
    && (!limits.organizationIds || limits.organizationIds.includes(organizationId))
    && (!universeId || !limits.universeIds || limits.universeIds.includes(universeId));
}

export async function organizationForPrincipal(ctx: AppContext, auth: AuthContext, organizationId: string, allowDisabled = false) {
  if (!resourceScopeAllowed(auth, organizationId)) return null;
  if (auth.credential.restrictions.universeIds) {
    if (!auth.credential.restrictions.universeIds.length) return null;
    const [scoped] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes).where(and(eq(schema.universes.organizationId, organizationId), inArray(schema.universes.lightspeedUniverseId, auth.credential.restrictions.universeIds))).limit(1);
    if (!scoped) return null;
  }
  const [organization] = await ctx.db.select().from(schema.organization).where(eq(schema.organization.id, organizationId)).limit(1);
  if (!organization || (!organization.enabled && !(allowDisabled && auth.principal.platformAdmin))) return null;
  if (auth.principal.platformAdmin) return { organization, role: "admin" as OrganizationRole };
  if (auth.principal.kind === "serviceUser") {
    const [serviceUser] = await ctx.db.select().from(schema.serviceUsers).where(and(eq(schema.serviceUsers.id, auth.principal.id), eq(schema.serviceUsers.organizationId, organizationId))).limit(1);
    return serviceUser?.enabled ? { organization, role: serviceUser.role } : null;
  }
  const [membership] = await ctx.db.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, auth.principal.id))).limit(1);
  const manual = organizationRoleSchema.safeParse(membership?.role).data;
  const role = declaredPrincipalRole(organization.membershipPolicy, auth.principal.user, ["member", "creator", "admin"] as const, manual);
  return role ? { organization, role } : null;
}

export async function requireOrganizationAdmin(ctx: AppContext, auth: AuthContext, organizationId: string) {
  const access = await organizationForPrincipal(ctx, auth, organizationId, true);
  if (!access) throw new HTTPException(404, { message: "organization not found" });
  if (access.role !== "admin") throw new HTTPException(403, { message: "organization admin required" });
  return access;
}

export function declaredUniverseRole(policy: Policy<UniverseRole>, identity: Identity, manual?: UniverseRole) {
  return declaredPrincipalRole(policy, identity, ["viewer", "contributor", "operator", "owner"] as const, manual);
}

/** Resolve organization inventory with a bounded query count, including declarations.
 * Directory policies are evaluated locally; no query is issued per organization. */
export async function listOrganizationsForPrincipal(ctx: AppContext, auth: AuthContext, allowDisabled = false) {
  const organizations = await ctx.db.select().from(schema.organization);
  const memberships = auth.principal.kind === "user" && !auth.principal.platformAdmin
    ? await ctx.db.select().from(schema.member).where(eq(schema.member.userId, auth.principal.id)) : [];
  const membershipRoles = new Map(memberships.map(member => [member.organizationId, organizationRoleSchema.safeParse(member.role).data]));
  const serviceUser = auth.principal.kind === "serviceUser" ? (await ctx.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.id, auth.principal.id)).limit(1))[0] : null;
  const limitedUniverses = auth.credential.restrictions.universeIds;
  const scopedOrganizations = limitedUniverses ? new Set((limitedUniverses.length ? await ctx.db.select({ organizationId: schema.universes.organizationId }).from(schema.universes).where(inArray(schema.universes.lightspeedUniverseId, limitedUniverses)) : []).map(universe => universe.organizationId)) : null;
  return organizations.flatMap(organization => {
    if (!resourceScopeAllowed(auth, organization.id) || (scopedOrganizations && !scopedOrganizations.has(organization.id)) || (!organization.enabled && !(allowDisabled && auth.principal.platformAdmin))) return [];
    let role: OrganizationRole | undefined;
    if (auth.principal.platformAdmin) role = "admin";
    else if (auth.principal.kind === "serviceUser") role = serviceUser?.enabled && serviceUser.organizationId === organization.id ? serviceUser.role : undefined;
    else role = declaredPrincipalRole(organization.membershipPolicy, auth.principal.user, ["member", "creator", "admin"] as const, membershipRoles.get(organization.id));
    return role ? [{ organization, role }] : [];
  });
}
