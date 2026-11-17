import { useRef, useState } from "react";
import { useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { organizationAllows } from "@/lib/permissions";
import { AddIcon, UserAdminIcon, UserIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { TableCell } from "@/components/ui/table";
import { EmptyState, LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { ResourceIdentityCell, ResourceRow, ResourceStatusBadge, ResourceTable, ResourceValue } from "@/components/resource-table";
import { AddPersonDialog, NewServiceUserDialog } from "@/components/admin/organization-dialogs";
import { PersonDetailsDialog, ROLE_LABELS, ServiceUserDetailsDialog, serviceUserStatus } from "@/components/admin/member-details";

type Organization = PlatformOperationResult<"platform/organizations/list">["organizations"][number];

/// Everyone with a role in one organization: people and service users, each
/// row opening its details.
export function OrganizationMembersPage() {
  const organizations = useOrganizations();
  const { organizationId } = useParams();
  const organization = organizations.data?.organizations.find((candidate) =>
    candidate.id === organizationId && (organizationAllows(candidate, "manageMembers") || organizationAllows(candidate, "manageServiceUsers")));
  if (organizations.isLoading) return <LoadingNote />;
  if (organizations.error) return <ReadError error={organizations.error} loading={false} prefix="Organizations unavailable" />;
  if (!organization) return (
    <>
      <PageHeader title="Members" />
      <p role="alert">This organization is unavailable or you do not have permission to manage its members.</p>
    </>
  );
  return <Members key={organization.id} organization={organization} />;
}

function Members({ organization }: { organization: Organization }) {
  const organizationId = organization.id;
  const client = useQueryClient();
  const managesPeople = organizationAllows(organization, "manageMembers");
  const managesServiceUsers = organizationAllows(organization, "manageServiceUsers");
  const people = useQuery({
    queryKey: ["organization-members", organizationId],
    enabled: managesPeople,
    queryFn: () => platformApi("platform/organizations/members/list", { organizationId }),
  });
  const serviceUsers = useQuery({
    queryKey: ["service-users", organizationId],
    enabled: managesServiceUsers,
    queryFn: () => platformApi("platform/organizations/service-users/list", { organizationId }),
  });
  const [adding, setAdding] = useState<"person" | "serviceUser" | null>(null);
  const [personId, setPersonId] = useState<string | null>(null);
  const [serviceUserId, setServiceUserId] = useState<string | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const person = people.data?.members.find((member) => member.id === personId) ?? null;
  const serviceUser = serviceUsers.data?.serviceUsers.find((candidate) => candidate.id === serviceUserId) ?? null;
  const refresh = async () => {
    await Promise.all([
      client.invalidateQueries({ queryKey: ["organization-members", organizationId] }),
      client.invalidateQueries({ queryKey: ["service-users", organizationId] }),
    ]);
  };
  const canAddPeople = managesPeople && !organization.personal;
  return (
    <>
      <PageHeader
        title="Members"
        description={organization.personal
          ? `${organization.name} · A personal organization has only its owner as a member. It can still have service users.`
          : `${organization.name} · Organization roles apply across its universes. Give universe roles under a universe's Settings → Members.`}
        actions={(canAddPeople || managesServiceUsers) && (
          <div className="flex flex-wrap gap-2">
            {managesServiceUsers && (
              <Button variant={canAddPeople ? "outline" : "default"} onClick={() => setAdding("serviceUser")}>
                <AddIcon data-icon="inline-start" />
                New service user
              </Button>
            )}
            {canAddPeople && (
              <Button onClick={() => setAdding("person")}>
                <AddIcon data-icon="inline-start" />
                Add person
              </Button>
            )}
          </div>
        )}
      />
      {managesPeople && (
        <section aria-label="People" className="mb-8 min-w-0">
          <h2 className="mb-3 text-sm font-semibold">People</h2>
          {people.isPending && <LoadingNote />}
          {people.error && <ReadError error={people.error} loading={!people.data} prefix="People unavailable" />}
          {people.data?.members.length === 0 && (
            <EmptyState icon={UserIcon} title="No people yet">Add existing users by their user ID.</EmptyState>
          )}
          {!!people.data?.members.length && (
            <ResourceTable label="People" columns={["Person", "Member since", "Role"]}>
              {people.data.members.map((member) => (
                <ResourceRow key={member.id} name={member.name} onOpen={(trigger) => { triggerRef.current = trigger; setPersonId(member.id); }}>
                  <ResourceIdentityCell name={member.name} subtitle={member.email} />
                  <TableCell className="hidden sm:table-cell"><ResourceValue>{new Date(member.createdAt).toLocaleDateString()}</ResourceValue></TableCell>
                  <TableCell><ResourceValue>{organization.personal ? "Owner" : ROLE_LABELS[member.role]}</ResourceValue></TableCell>
                </ResourceRow>
              ))}
            </ResourceTable>
          )}
        </section>
      )}
      {managesServiceUsers && (
        <section aria-label="Service users" className="min-w-0">
          <h2 className="mb-3 text-sm font-semibold">Service users</h2>
          {serviceUsers.isPending && <LoadingNote />}
          {serviceUsers.error && <ReadError error={serviceUsers.error} loading={!serviceUsers.data} prefix="Service users unavailable" />}
          {serviceUsers.data?.serviceUsers.length === 0 && (
            <EmptyState icon={UserAdminIcon} title="No service users yet">
              A service user is an identity for an integration, with its own role and API keys.
            </EmptyState>
          )}
          {!!serviceUsers.data?.serviceUsers.length && (
            <ResourceTable label="Service users" columns={["Service user", "Role", "Status"]}>
              {serviceUsers.data.serviceUsers.map((candidate) => (
                <ResourceRow key={candidate.id} name={candidate.name} onOpen={(trigger) => { triggerRef.current = trigger; setServiceUserId(candidate.id); }}>
                  <ResourceIdentityCell name={candidate.name} subtitle={candidate.id} mono compactDescription={ROLE_LABELS[candidate.role]} />
                  <TableCell className="hidden sm:table-cell"><ResourceValue>{ROLE_LABELS[candidate.role]}</ResourceValue></TableCell>
                  <TableCell><ResourceStatusBadge status={serviceUserStatus(candidate)} /></TableCell>
                </ResourceRow>
              ))}
            </ResourceTable>
          )}
        </section>
      )}
      {adding === "person" && <AddPersonDialog organizationId={organizationId} close={() => setAdding(null)} saved={refresh} />}
      {adding === "serviceUser" && <NewServiceUserDialog organizationId={organizationId} close={() => setAdding(null)} saved={refresh} />}
      <PersonDetailsDialog organizationId={organizationId} personal={organization.personal} member={person} finalFocus={triggerRef}
        onOpenChange={(open) => { if (!open) setPersonId(null); }} />
      <ServiceUserDetailsDialog organizationId={organizationId} serviceUser={serviceUser} finalFocus={triggerRef}
        onOpenChange={(open) => { if (!open) setServiceUserId(null); }} />
    </>
  );
}
