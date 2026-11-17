import { useParams } from "react-router-dom";
import { useOrganizations } from "@/lib/platform-api";
import { usePlatformAdmin } from "@/lib/permissions";
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

/** Navigation hints only; the server authorizes every organization and universe operation. */
export function useAdminAccess() {
  const platform = usePlatformAdmin();
  const organizations = useOrganizations();
  const universes = useUniverses();
  const { organizationId } = useParams();
  const manageable = (organizations.data?.organizations ?? []).filter(
    (organization) =>
      platform ||
      organization.role === "admin" ||
      universes.data?.some(
        (universe) =>
          universe.organizationId === organization.id &&
          universe.role === "owner",
      ),
  );
  const selected = manageable.find(
    (organization) => organization.id === organizationId,
  );
  const organizationAdmin =
    !!selected && (platform || selected.role === "admin");
  return {
    platform,
    selected,
    manageable,
    canCreate: organizations.data?.canCreate ?? false,
    organizations: organizationAdmin,
    universes:
      !!selected &&
      (organizationAdmin ||
        universes.data?.some(
          (universe) =>
            universe.organizationId === selected.id &&
            universe.role === "owner",
        ) === true),
    keys: organizationAdmin,
    any:
      platform ||
      manageable.length > 0 ||
      organizations.data?.canCreate === true,
    isLoading: organizations.isLoading || universes.isLoading,
    error: organizations.error ?? universes.error,
  };
}
