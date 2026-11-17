import { Navigate, Outlet, useSearchParams } from "react-router-dom";
import { LoadingNote, PageHeader } from "@/components/page";
import { organizationAdminPath, useAdminAccess } from "@/lib/admin-access";

export function AdminGate({
  scope = "any",
}: {
  scope?: "any" | "organizations" | "universes" | "keys" | "platform";
}) {
  const access = useAdminAccess();
  if (access[scope]) return <Outlet />;
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
  if (access.platform)
    return <Navigate to="/admin/platform/organizations" replace />;
  return (
    <PageHeader
      title="Admin"
      description="No organizations are available to administer."
    />
  );
}
