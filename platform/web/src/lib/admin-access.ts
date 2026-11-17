import { useParams } from "react-router-dom";
import { useOrganizations } from "@/lib/platform-api";
import type { DeploymentAction, OrganizationAction } from "@lightspeed-ai/platform-shared";
import { organizationAllows, useDeploymentPermissions } from "@/lib/permissions";
import { useUniverses } from "@/lib/universes";

export const organizationAdminPath = (id: string) =>
  `/admin/organizations/${encodeURIComponent(id)}`;
export const organizationLabel = (
  organization: { name: string; personal?: boolean },
  distinguishPersonal = false,
) =>
  organization.personal
    ? distinguishPersonal
      ? `Personal · ${organization.name}`
      : "Personal"
    : organization.name;

/// The organization actions that make an organization's admin area worth
/// showing. Creating universes alone does not.
const ADMINISTRATIVE_ACTIONS: readonly OrganizationAction[] = ["administerUniverses", "manageMembers", "manageServiceUsers", "managePolicy"];

/** Navigation hints only; the server authorizes every organization and universe operation. */
export function useAdminAccess() {
  const deployment = useDeploymentPermissions();
  const organizations = useOrganizations();
  const universes = useUniverses();
  const { organizationId } = useParams();
  const administersUniverseIn = (id: string) =>
    universes.data?.some((universe) => universe.organizationId === id && universe.actions?.includes("administer")) === true;
  const manageable = (organizations.data?.organizations ?? []).filter(
    (organization) =>
      ADMINISTRATIVE_ACTIONS.some((action) => organizationAllows(organization, action)) ||
      administersUniverseIn(organization.id),
  );
  const selected = manageable.find((organization) => organization.id === organizationId);
  const deploymentArea = PLATFORM_AREA_ACTIONS.some((action) => deployment.can(action));
  return {
    selected,
    manageable,
    canCreate: organizations.data?.canCreate ?? false,
    /** Whether the caller holds `action` in the organization on screen. */
    organization: (action: OrganizationAction) => organizationAllows(selected, action),
    /** The organization's universe list: for its administrators and for Owners of any of its universes. */
    universes: !!selected && (organizationAllows(selected, "administerUniverses") || administersUniverseIn(selected.id)),
    deployment: deployment.can,
    any: deploymentArea || manageable.length > 0 || organizations.data?.canCreate === true,
    isLoading: organizations.isLoading || universes.isLoading,
    error: organizations.error ?? universes.error,
  };
}

/// The deployment actions behind the Platform admin pages.
export const PLATFORM_AREA_ACTIONS: readonly DeploymentAction[] = [
  "administerOrganizations", "manageUsers", "configureDeployment", "readAudit", "manageUnclaimedUniverses",
];
