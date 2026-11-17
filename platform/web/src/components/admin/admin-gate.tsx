import { Navigate, Outlet, useSearchParams } from "react-router-dom";
import { LoadingNote, PageHeader } from "@/components/page";
import type { DeploymentAction, OrganizationAction } from "@lightspeed-ai/platform-shared";
import { organizationAdminPath, useAdminAccess } from "@/lib/admin-access";

/// An admin area opens when the caller holds what it needs: an action in the
/// organization on screen, a deployment action, or (for an organization's
/// universe list) administration of one of its universes.
export function AdminGate(props: { organizationAction?: OrganizationAction; deploymentAction?: DeploymentAction; universes?: boolean }) {
  const access = useAdminAccess();
  const allowed = props.organizationAction ? access.organization(props.organizationAction)
    : props.deploymentAction ? access.deployment(props.deploymentAction)
    : props.universes ? access.universes
    : access.any;
  if (allowed) return <Outlet />;
  if (access.isLoading) return <LoadingNote />;
  return (
    <>
      <PageHeader title="Admin" />
      <p role="alert">
        {access.error?.message ??
          "You do not have permission to manage this area."}
      </p>
    </>
  );
}

export function AdminIndexRedirect() {
  const access = useAdminAccess();
  const [search] = useSearchParams();
  if (access.isLoading) return <LoadingNote />;
  const organization =
    access.manageable.find((item) => item.id === search.get("organization")) ??
    access.manageable[0];
  if (organization)
    return (
      <Navigate
        to={`${organizationAdminPath(organization.id)}/universes`}
        replace
      />
    );
  if (access.deployment("administerOrganizations"))
    return <Navigate to="/admin/platform/organizations" replace />;
  return (
    <PageHeader
      title="Admin"
      description="No organizations are available to administer."
    />
  );
}
