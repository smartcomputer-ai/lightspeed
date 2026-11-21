import {
  deploymentActions,
  PLATFORM_SETTINGS,
  type PlatformOperationResult,
  type PlatformModule,
  organizationActions,
  organizationRoleSchema,
  universeActions,
  universeRoleSchema,
  type DeploymentAction,
  type OrganizationAction,
  type UniverseAction,
  type UniverseRole,
} from "@lightspeed-ai/platform-shared";
import type { DemoStore, SessionRecord, UniverseState } from "./store";

/// The demo resolves authority the way the Platform does: the demo user's
/// roles are bundles of actions from the shared definitions, unioned, with
/// organization administration holding Owner on every universe and Platform
/// administration holding Admin in every organization.

export function demoDeploymentAllows(store: DemoStore, action: DeploymentAction): boolean {
  return deploymentActions(store.currentUser.role === "admin").has(action);
}

export function demoOrganizationActions(store: DemoStore, role: string | null | undefined): ReadonlySet<OrganizationAction> {
  const held = organizationRoleSchema.safeParse(role).data;
  const roles = held ? [held] : [];
  if (demoDeploymentAllows(store, "administerOrganizations")) roles.push("admin");
  return organizationActions(roles);
}

export function demoUniverseActions(store: DemoStore, state: UniverseState): ReadonlySet<UniverseAction> {
  const roles: UniverseRole[] = [];
  const granted = universeRoleSchema.safeParse(state.universe.role).data;
  if (granted) roles.push(granted);
  if (demoOrganizationActions(store, state.universe.organizationRole).has("administerUniverses")) roles.push("owner");
  return universeActions(roles);
}

export function demoModuleEnabled(store: DemoStore, universe: UniverseState, module: PlatformModule): boolean {
  const organizationId = universe.universe.organizationId;
  const override = organizationId ? store.organizationModules.get(organizationId)?.get(module.id) : undefined;
  return override ?? module.defaultEnabled;
}

/** All descendants inherit the visibility of their session tree's root. */
export function demoCanReadSession(
  store: DemoStore, state: UniverseState, record: SessionRecord,
  actions = demoUniverseActions(store, state),
): boolean {
  if (!actions.has("read")) return false;
  if (actions.has("accessPrivateSessions")) return true;
  const root = state.sessions.get(record.view.origin?.rootSessionId ?? record.view.id) ?? record;
  const access = root.view.access;
  return access.visibility === "universe" || (access.createdBy?.kind === "actor" && access.createdBy.id === store.currentUser.id);
}

/** A universe as the Platform presents it to the demo user, with the actions it holds. */
export function demoUniverseView(store: DemoStore, state: UniverseState) {
  return {
    ...state.universe,
    actions: [...demoUniverseActions(store, state)],
    organizationActions: [...demoOrganizationActions(store, state.universe.organizationRole)],
  };
}

/// An organization's universe limit: its own, or the deployment default for its kind.
export function demoUniverseQuota(store: DemoStore, organization: PlatformOperationResult<"platform/organizations/read">["organization"]): number {
  if (organization.universeQuotaSource === "organization") return organization.universeQuota;
  const key = organization.personal ? "personalUniverseQuota" : "sharedUniverseQuota";
  return Number(store.platformSettings.get(key) ?? PLATFORM_SETTINGS[key].defaultValue);
}
