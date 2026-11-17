import { useParams } from "react-router-dom";
import { useOrganizations } from "@/lib/platform-api";
import { organizationAllows } from "@/lib/permissions";
import { LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { OrganizationSettings } from "@/components/admin/organization-settings";

/// One organization's settings, each edited where it is shown.
export function OrganizationSettingsPage() {
  const organizations = useOrganizations();
  const { organizationId } = useParams();
  const organization = organizations.data?.organizations.find((candidate) =>
    candidate.id === organizationId && organizationAllows(candidate, "managePolicy"));
  if (organizations.isLoading) return <LoadingNote />;
  if (organizations.error) return <ReadError error={organizations.error} loading={false} prefix="Organizations unavailable" />;
  if (!organization) return (
    <>
      <PageHeader title="Organization settings" />
      <p role="alert">This organization is unavailable or you do not have permission to administer it.</p>
    </>
  );
  return (
    <>
      <PageHeader
        title="Organization settings"
        description={organization.personal
          ? `${organization.name} · A personal organization: its owner is its only member.`
          : `${organization.name} · Organization Admins manage every universe here.`}
      />
      <OrganizationSettings key={organization.id} organization={organization} />
    </>
  );
}
