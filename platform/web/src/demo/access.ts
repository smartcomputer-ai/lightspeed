import {
  deploymentActions,
  organizationActions,
  organizationRoleSchema,
  universeActions,
  universeRoleSchema,
  type DeploymentAction,
  type OrganizationAction,
  type UniverseAction,
  type UniverseRole,
} from "@lightspeed-ai/platform-shared";
import type { DemoStore, UniverseState } from "./store";

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

/** A universe as the Platform presents it to the demo user, with the actions it holds. */
export function demoUniverseView(store: DemoStore, state: UniverseState) {
  return {
    ...state.universe,
    actions: [...demoUniverseActions(store, state)],
    organizationActions: [...demoOrganizationActions(store, state.universe.organizationRole)],
  };
}
