import { and, eq, getTableColumns, inArray } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { ORGANIZATION_ROLES, highestRole, organizationActions, organizationRoleSchema, type OrganizationAction, type OrganizationRole, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { deploymentAllows, type AuthContext } from "./auth-context.js";
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

type Enrollment = Pick<typeof schema.organizationEnrollments.$inferSelect, "issuer" | "subject" | "subjectPattern">;

type OrganizationRow = typeof schema.organization.$inferSelect & { enrollment?: Enrollment | null };

function selectOrganizations(ctx: AppContext, auth: AuthContext, includeEnrollment: boolean) {
  const { organization, organizationEnrollments } = schema;
  if (includeEnrollment && auth.principal.platformAdmin) {
    const { issuer, subject, subjectPattern } = organizationEnrollments;
    return ctx.db.select({ ...getTableColumns(organization), enrollment: { issuer, subject, subjectPattern } }).from(organization)
      .leftJoin(organizationEnrollments, eq(organizationEnrollments.organizationId, organization.id));
  }
  return ctx.db.select().from(organization);
}

export interface OrganizationAccess {
  enrollment?: Enrollment;
  organization: typeof schema.organization.$inferSelect;
  /** The highest role held, for display. Decisions use `actions`. */
  role: OrganizationRole;
  actions: ReadonlySet<OrganizationAction>;
  /** Whether the principal holds a role here itself, rather than only
   * through deployment administration. Only a member can be granted
   * universe roles or receive the organization's base universe role. */
  member: boolean;
}

/** A principal's organization authority: the union of the role it holds
 * there and, for a deployment administrator, the Admin bundle. */
function organizationAccess(auth: AuthContext, { enrollment, ...organization }: OrganizationRow, held: OrganizationRole | undefined): OrganizationAccess | null {
  const roles: OrganizationRole[] = held ? [held] : [];
  if (deploymentAllows(auth, "administerOrganizations")) roles.push("admin");
  const role = highestRole(ORGANIZATION_ROLES, roles);
  return role ? { organization, enrollment: enrollment ?? undefined, role, actions: organizationActions(roles), member: held !== undefined } : null;
}

export async function organizationForPrincipal(ctx: AppContext, auth: AuthContext, organizationId: string, { includeEnrollment = false } = {}): Promise<OrganizationAccess | null> {
  if (!resourceScopeAllowed(auth, organizationId)) return null;
  if (auth.credential.restrictions.universeIds) {
    if (!auth.credential.restrictions.universeIds.length) return null;
    const [scoped] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes).where(and(eq(schema.universes.organizationId, organizationId), inArray(schema.universes.lightspeedUniverseId, auth.credential.restrictions.universeIds))).limit(1);
    if (!scoped) return null;
  }
  const [organization] = await selectOrganizations(ctx, auth, includeEnrollment).where(eq(schema.organization.id, organizationId)).limit(1);
  if (!organization) return null;
  if (auth.principal.kind === "serviceUser") {
    const [serviceUser] = await ctx.db.select().from(schema.serviceUsers).where(and(eq(schema.serviceUsers.id, auth.principal.id), eq(schema.serviceUsers.organizationId, organizationId))).limit(1);
    return organizationAccess(auth, organization, serviceUser?.enabled ? serviceUser.role : undefined);
  }
  const [membership] = await ctx.db.select().from(schema.member).where(and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, auth.principal.id))).limit(1);
  const manual = organizationRoleSchema.safeParse(membership?.role).data;
  return organizationAccess(auth, organization, declaredPrincipalRole(organization.membershipPolicy, auth.principal.user, ORGANIZATION_ROLES, manual));
}

/** The principal's organization access, refused unless it holds `action` there. */
export async function requireOrganizationAction(ctx: AppContext, auth: AuthContext, organizationId: string, action: OrganizationAction): Promise<OrganizationAccess> {
  const access = await organizationForPrincipal(ctx, auth, organizationId);
  if (!access) throw new HTTPException(404, { message: "organization not found" });
  if (!access.actions.has(action)) throw new HTTPException(403, { message: `organization permission required: ${action}` });
  return access;
}

export function declaredUniverseRole(policy: Policy<UniverseRole>, identity: Identity, manual?: UniverseRole) {
  return declaredPrincipalRole(policy, identity, ["viewer", "contributor", "operator", "owner"] as const, manual);
}

/** A personal organization is presented under its owner's current name, so
 * renaming the user renames it; its stored name is only the creation-time value. */
export async function withOwnerNames<T extends typeof schema.organization.$inferSelect>(db: AppContext["db"], organizations: T[]): Promise<T[]> {
  const owners = [...new Set(organizations.flatMap(organization => organization.personalUserId ? [organization.personalUserId] : []))];
  if (!owners.length) return organizations;
  const names = new Map((await db.select({ id: schema.user.id, name: schema.user.name }).from(schema.user).where(inArray(schema.user.id, owners))).map(user => [user.id, user.name]));
  return organizations.map(organization => {
    const owner = organization.personalUserId ? names.get(organization.personalUserId) : undefined;
    return owner ? { ...organization, name: owner } : organization;
  });
}

/** A personal organization belongs to one user: its membership, membership
 * policy and name are fixed. Its owner may still create service users. */
export async function refusePersonalOrganization(db: AppContext["db"], organizationId: string, change: string) {
  const [organization] = await db.select({ personalUserId: schema.organization.personalUserId }).from(schema.organization).where(eq(schema.organization.id, organizationId)).limit(1);
  if (organization?.personalUserId) throw new HTTPException(409, { message: `a personal organization cannot change its ${change}` });
}

/** Resolve organization inventory with a bounded query count, including declarations.
 * Directory policies are evaluated locally; no query is issued per organization. */
export async function listOrganizationsForPrincipal(ctx: AppContext, auth: AuthContext, { includeEnrollment = false } = {}): Promise<OrganizationAccess[]> {
  const organizations = await selectOrganizations(ctx, auth, includeEnrollment);
  const memberships = auth.principal.kind === "user"
    ? await ctx.db.select().from(schema.member).where(eq(schema.member.userId, auth.principal.id)) : [];
  const membershipRoles = new Map(memberships.map(member => [member.organizationId, organizationRoleSchema.safeParse(member.role).data]));
  const serviceUser = auth.principal.kind === "serviceUser" ? (await ctx.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.id, auth.principal.id)).limit(1))[0] : null;
  const limitedUniverses = auth.credential.restrictions.universeIds;
  const scopedOrganizations = limitedUniverses ? new Set((limitedUniverses.length ? await ctx.db.select({ organizationId: schema.universes.organizationId }).from(schema.universes).where(inArray(schema.universes.lightspeedUniverseId, limitedUniverses)) : []).map(universe => universe.organizationId)) : null;
  return organizations.flatMap(organization => {
    if (!resourceScopeAllowed(auth, organization.id) || (scopedOrganizations && !scopedOrganizations.has(organization.id))) return [];
    const held = auth.principal.kind === "serviceUser"
      ? (serviceUser?.enabled && serviceUser.organizationId === organization.id ? serviceUser.role : undefined)
      : declaredPrincipalRole(organization.membershipPolicy, auth.principal.user, ORGANIZATION_ROLES, membershipRoles.get(organization.id));
    const access = organizationAccess(auth, organization, held);
    return access ? [access] : [];
  });
}
