import { z } from "zod";

/// Authorization asks whether a principal may take an action at a scope:
/// the deployment, an organization, or a universe. A role is a fixed, named
/// bundle of actions at one scope, defined here and nowhere else. A principal
/// may hold several roles at a scope at once (a grant, a declared policy, an
/// organization's base role, administration inherited from above); its
/// authority there is the union of their bundles. Grants only add: no rule
/// subtracts an action. Code decides on actions, never on role names; role
/// names are what people assign and what screens display.

// ----- Universe -----

/// The actions core declares on universe methods (`UniverseAction` in the
/// core manifest). The generated method table is typed against this list,
/// so a new core action fails the Platform build until it is placed in a
/// bundle below.
export const CORE_UNIVERSE_ACTIONS = [
  "read",
  "createSession",
  "controlSession",
  "configureSession",
  "stopSession",
  "closeSession",
  "deleteSession",
  "setSessionRetention",
  "shareSession",
  "createProfile",
  "manageProfile",
  "createBot",
  "manageBot",
  "invokeBot",
  "useResource",
  "configureResource",
  "createWorkspace",
] as const;
export type CoreUniverseAction = (typeof CORE_UNIVERSE_ACTIONS)[number];

/// Actions the Platform gate adds inside universe methods.
export const PLATFORM_UNIVERSE_ACTIONS = [
  /// Read and act on sessions their creators have not shared.
  "accessPrivateSessions",
  /// Close a shared session, including one somebody else started.
  "closeSharedSessions",
  /// Manage the universe's grants, settings and lifecycle.
  "administer",
] as const;
export const UNIVERSE_ACTIONS = [...CORE_UNIVERSE_ACTIONS, ...PLATFORM_UNIVERSE_ACTIONS] as const;
export type UniverseAction = (typeof UNIVERSE_ACTIONS)[number];

/// Universe roles, least to most; the order is for display and for picking
/// the highest role to show.
export const UNIVERSE_ROLES = ["viewer", "contributor", "operator", "owner"] as const;
export const universeRoleSchema = z.enum(UNIVERSE_ROLES);
export type UniverseRole = z.infer<typeof universeRoleSchema>;

const VIEWER: readonly UniverseAction[] = ["read"];
const CONTRIBUTOR: readonly UniverseAction[] = [
  ...VIEWER, "createSession", "controlSession", "stopSession", "closeSession", "shareSession", "invokeBot", "useResource",
];
const OPERATOR: readonly UniverseAction[] = [
  ...CONTRIBUTOR, "configureSession", "deleteSession", "closeSharedSessions", "createProfile", "manageProfile",
  "createBot", "manageBot", "configureResource", "createWorkspace",
];
const OWNER: readonly UniverseAction[] = [...OPERATOR, "setSessionRetention", "accessPrivateSessions", "administer"];

export const UNIVERSE_ROLE_ACTIONS: Readonly<Record<UniverseRole, readonly UniverseAction[]>> = {
  viewer: VIEWER,
  contributor: CONTRIBUTOR,
  operator: OPERATOR,
  owner: OWNER,
};

/// An organization's default universe access: the universe role every member
/// person holds on every one of its universes. Any universe role can be the
/// default; Owner gives every member full control of every universe,
/// including others' unshared sessions.
export const BASE_UNIVERSE_ROLES = UNIVERSE_ROLES;
export const baseUniverseRoleSchema = z.enum(BASE_UNIVERSE_ROLES);
export type BaseUniverseRole = z.infer<typeof baseUniverseRoleSchema>;

// ----- Organization -----

export const ORGANIZATION_ACTIONS = [
  /// Create a universe in the organization and become its first Owner.
  "createUniverse",
  /// Hold every universe action on every universe in the organization,
  /// clear deletion protection, and appoint another principal as a new
  /// universe's first Owner.
  "administerUniverses",
  /// Add, remove and change the roles of members.
  "manageMembers",
  "manageServiceUsers",
  /// Change protection defaults, default universe access and the membership policy.
  "managePolicy",
] as const;
export const organizationActionSchema = z.enum(ORGANIZATION_ACTIONS);
export type OrganizationAction = (typeof ORGANIZATION_ACTIONS)[number];

export const ORGANIZATION_ROLES = ["member", "creator", "admin"] as const;
export const organizationRoleSchema = z.enum(ORGANIZATION_ROLES);
export type OrganizationRole = z.infer<typeof organizationRoleSchema>;

export const ORGANIZATION_ROLE_ACTIONS: Readonly<Record<OrganizationRole, readonly OrganizationAction[]>> = {
  /// A member's universe access comes from the organization's base role and
  /// from grants, not from an organization action.
  member: [],
  creator: ["createUniverse"],
  admin: [...ORGANIZATION_ACTIONS],
};

// ----- Deployment -----

export const DEPLOYMENT_ACTIONS = [
  /// Hold every organization action in every organization.
  "administerOrganizations",
  "createOrganization",
  "setUniverseQuota",
  /// Create, suspend and assign the Platform role of users.
  "manageUsers",
  "readAudit",
  /// Channel connectors, environment providers and handle reconciliation.
  "configureDeployment",
  /// List, adopt, relink and delete runtime universes Platform does not hold.
  "manageUnclaimedUniverses",
  /// Inspect and permanently remove deleted sessions.
  "purgeSessions",
] as const;
export type DeploymentAction = (typeof DEPLOYMENT_ACTIONS)[number];

/// The one deployment role. Everyone else holds no deployment action.
export const DEPLOYMENT_ROLE_ACTIONS: Readonly<Record<"platformAdmin", readonly DeploymentAction[]>> = {
  platformAdmin: [...DEPLOYMENT_ACTIONS],
};

// ----- Resolution -----

function union<Role extends string, Action>(bundles: Readonly<Record<Role, readonly Action[]>>, roles: Iterable<Role>): ReadonlySet<Action> {
  const actions = new Set<Action>();
  for (const role of roles) for (const action of bundles[role]) actions.add(action);
  return actions;
}

export function universeActions(roles: Iterable<UniverseRole>): ReadonlySet<UniverseAction> {
  return union(UNIVERSE_ROLE_ACTIONS, roles);
}

export function organizationActions(roles: Iterable<OrganizationRole>): ReadonlySet<OrganizationAction> {
  return union(ORGANIZATION_ROLE_ACTIONS, roles);
}

export function deploymentActions(platformAdmin: boolean): ReadonlySet<DeploymentAction> {
  return union(DEPLOYMENT_ROLE_ACTIONS, platformAdmin ? ["platformAdmin" as const] : []);
}

/// Whether one universe role's bundle includes `action`.
export function universeRoleAllows(role: UniverseRole | null | undefined, action: UniverseAction): boolean {
  return !!role && UNIVERSE_ROLE_ACTIONS[role].includes(action);
}

/// Whether one organization role's bundle includes `action`.
export function organizationRoleAllows(role: OrganizationRole | null | undefined, action: OrganizationAction): boolean {
  return !!role && ORGANIZATION_ROLE_ACTIONS[role].includes(action);
}

/// The highest of several held roles, for display.
export function highestRole<Role extends string>(order: readonly Role[], roles: Iterable<Role>): Role | undefined {
  let highest: Role | undefined;
  for (const role of roles) if (highest === undefined || order.indexOf(role) > order.indexOf(highest)) highest = role;
  return highest;
}

// ----- Session lifecycle rules -----

/// Closing a shared session is moderation, even for its creator; an
/// unshared one is its creator's, or someone's who may access private work.
export function canCloseSession(actions: ReadonlySet<UniverseAction>, creator: boolean, shared: boolean): boolean {
  if (!actions.has("closeSession")) return false;
  return shared ? actions.has("closeSharedSessions") : creator || actions.has("accessPrivateSessions");
}

/// Deleting an unshared session requires access to private work.
export function canDeleteSession(actions: ReadonlySet<UniverseAction>, shared: boolean): boolean {
  return actions.has("deleteSession") && (shared || actions.has("accessPrivateSessions"));
}
