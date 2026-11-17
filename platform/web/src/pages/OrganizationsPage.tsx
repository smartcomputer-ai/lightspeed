import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  OrganizationRole,
  PlatformOperationResult,
} from "@lightspeed-ai/platform-shared";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { usePlatformAdmin } from "@/lib/permissions";
import { AddIcon, EditIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  MemberDialog,
  ServiceUserDialog,
  PolicyDialog,
  ConfirmDialog,
} from "@/components/admin/organization-dialogs";
import {
  Table,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader } from "@/components/page";

type Organization =
  PlatformOperationResult<"platform/organizations/read">["organization"];
type Member =
  PlatformOperationResult<"platform/organizations/members/list">["members"][number];
type ServiceUser =
  PlatformOperationResult<"platform/organizations/service-users/list">["serviceUsers"][number];
const roleLabels: Record<OrganizationRole, string> = {
  member: "Member",
  creator: "Creator",
  admin: "Admin",
};

type Section = "people" | "service-users" | "settings";
const sectionTitles: Record<Section, string> = {
  people: "People",
  "service-users": "Service users",
  settings: "Organization settings",
};

/// One organization's administration sections. The Platform inventory of
/// every organization is PlatformOrganizationsPage.
export function OrganizationsPage({ section }: { section: Section }) {
  const organizations = useOrganizations();
  const admin = usePlatformAdmin();
  const { organizationId } = useParams();
  const organization = organizations.data?.organizations.find(
    (organization) =>
      organization.id === organizationId &&
      (admin || organization.role === "admin"),
  );
  if (organizations.isLoading) return <LoadingNote />;
  if (organizations.error)
    return (
      <p role="alert" className="text-destructive">
        {organizations.error.message}
      </p>
    );
  if (!organization)
    return (
      <>
        <PageHeader title={sectionTitles[section]} />
        <p role="alert">
          This organization is unavailable or you do not have permission to
          administer it. Choose another organization in the sidebar.
        </p>
      </>
    );
  return (
    <OrganizationDetails
      key={organization.id}
      organization={organization}
      section={section}
    />
  );
}

function OrganizationDetails({
  organization,
  section,
}: {
  organization: Organization;
  section: Section;
}) {
  const client = useQueryClient();
  const organizationId = organization.id;
  const [member, setMember] = useState<Member | "new" | null>(null);
  const [serviceUser, setServiceUser] = useState<ServiceUser | "new" | null>(
    null,
  );
  const [policyOpen, setPolicyOpen] = useState(false);
  const [remove, setRemove] = useState<Member | null>(null);
  const [toggleApp, setToggleApp] = useState<ServiceUser | null>(null);
  const members = useQuery({
    queryKey: ["organization-members", organizationId],
    enabled: section === "people",
    queryFn: () =>
      platformApi("platform/organizations/members/list", { organizationId }),
  });
  const apps = useQuery({
    queryKey: ["service-users", organizationId],
    enabled: section === "service-users",
    queryFn: () =>
      platformApi("platform/organizations/service-users/list", {
        organizationId,
      }),
  });
  const refresh = async () => {
    await Promise.all([
      client.invalidateQueries({ queryKey: ["organizations"] }),
      client.invalidateQueries({
        queryKey: ["organization-members", organizationId],
      }),
      client.invalidateQueries({ queryKey: ["service-users", organizationId] }),
      client.invalidateQueries({ queryKey: ["universes"] }),
    ]);
  };
  const hasPolicy =
    !!organization.membershipPolicy?.members?.length ||
    !!organization.membershipPolicy?.directoryGroupMappings?.length;
  return (
    <div className="grid min-w-0 gap-8">
      {section === "settings" && (
        <section>
          <PageHeader
            title="Organization settings"
            description={`${organization.name} · Organization Admins manage every universe here.`}
            actions={
              <Button variant="outline" onClick={() => setPolicyOpen(true)}>
                <EditIcon data-icon="inline-start" />
                Edit policy
              </Button>
            }
          />
          <Card>
            <CardContent className="grid gap-2 text-sm">
              <p>
                Universes:{" "}
                <strong className="tabular-nums">
                  {organization.universeCount ?? 0} of{" "}
                  {organization.universeQuota}
                </strong>
              </p>
              <p>
                New universes:{" "}
                <strong>
                  {organization.defaultProtected
                    ? "Protected by default"
                    : "Not protected by default"}
                </strong>
              </p>
              <p>
                Protection requirement:{" "}
                <strong>
                  {organization.requireProtected
                    ? "Required for all universes"
                    : "Optional"}
                </strong>
              </p>
            </CardContent>
          </Card>
        </section>
      )}
      <>
        {section === "people" && (
          <section className="min-w-0">
            <PageHeader
              title="People"
              description={`${organization.name} · Organization roles apply across its universes. Assign universe roles under Universes → Access.`}
              actions={
                <Button onClick={() => setMember("new")}>
                  <AddIcon data-icon="inline-start" />
                  Add member
                </Button>
              }
            />
            {hasPolicy && (
              <p className="mb-3 text-sm text-muted-foreground">
                Organization policy also grants membership. Removing a member or
                lowering their role here does not remove access granted by that
                policy.
              </p>
            )}
            {members.isLoading && <LoadingNote />}
            {members.error && (
              <p role="alert" className="text-destructive">
                {members.error.message}
              </p>
            )}
            {members.data?.members.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No organization members yet. Add an existing user.
              </p>
            )}
            {!!members.data?.members.length && (
              <TableCard>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Person</TableHead>
                      <TableHead>Organization role</TableHead>
                      <TableHead />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {members.data.members.map((item) => (
                      <TableRow key={item.id}>
                        <TableCell>
                          <p className="font-medium">{item.name}</p>
                          <p className="text-xs text-muted-foreground">
                            {item.email}
                          </p>
                        </TableCell>
                        <TableCell>{roleLabels[item.role]}</TableCell>
                        <TableCell>
                          <div className="flex justify-end gap-2">
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Edit organization role for ${item.name}`}
                              onClick={() => setMember(item)}
                            >
                              <EditIcon />
                            </Button>
                            <Button
                              variant="ghost"
                              aria-label={`Remove ${item.name}`}
                              onClick={() => setRemove(item)}
                            >
                              Remove
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TableCard>
            )}
          </section>
        )}
        {section === "service-users" && (
          <section className="min-w-0">
            <PageHeader
              title="Service users"
              description={`${organization.name} · Service users have their own permissions and API keys. Grant access to specific universes under Universes → Access.`}
              actions={
                <Button onClick={() => setServiceUser("new")}>
                  <AddIcon data-icon="inline-start" />
                  New service user
                </Button>
              }
            />
            {apps.isLoading && <LoadingNote />}
            {apps.error && (
              <p role="alert" className="text-destructive">
                {apps.error.message}
              </p>
            )}
            {apps.data?.serviceUsers.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No service users yet. Create one for an integration that needs
                its own identity.
              </p>
            )}
            {!!apps.data?.serviceUsers.length && (
              <TableCard>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Service user</TableHead>
                      <TableHead>Organization role</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {apps.data.serviceUsers.map((item) => (
                      <TableRow key={item.id}>
                        <TableCell className="font-medium">
                          {item.name}
                        </TableCell>
                        <TableCell>{roleLabels[item.role]}</TableCell>
                        <TableCell>
                          {item.enabled ? "Active" : "Disabled"}
                        </TableCell>
                        <TableCell>
                          <div className="flex justify-end gap-2">
                            <Button
                              nativeButton={false}
                              variant="outline"
                              render={
                                <Link
                                  to={`/admin/organizations/${encodeURIComponent(organizationId)}/api-keys?serviceUser=${encodeURIComponent(item.id)}`}
                                />
                              }
                              aria-label={`API keys for ${item.name}`}
                            >
                              API keys
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label={`Edit ${item.name}`}
                              onClick={() => setServiceUser(item)}
                            >
                              <EditIcon />
                            </Button>
                            <Button
                              variant="ghost"
                              aria-label={`${item.enabled ? "Disable" : "Enable"} ${item.name}`}
                              onClick={() => setToggleApp(item)}
                            >
                              {item.enabled ? "Disable" : "Enable"}
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TableCard>
            )}
          </section>
        )}
      </>
      <p className="text-sm text-muted-foreground">
        For API access acting as you, manage{" "}
        <Link className="underline" to="/account#api-keys">
          your API keys
        </Link>
        .
      </p>
      {policyOpen && (
        <PolicyDialog
          organization={organization}
          close={() => setPolicyOpen(false)}
          saved={refresh}
        />
      )}
      {member && (
        <MemberDialog
          organizationId={organizationId}
          member={member === "new" ? undefined : member}
          close={() => setMember(null)}
          saved={refresh}
        />
      )}
      {serviceUser && (
        <ServiceUserDialog
          organizationId={organizationId}
          serviceUser={serviceUser === "new" ? undefined : serviceUser}
          close={() => setServiceUser(null)}
          saved={refresh}
        />
      )}
      {remove && (
        <ConfirmDialog
          title={`Remove ${remove.name}?`}
          description="Remove this person's explicit organization membership. Access granted by organization policy or a Platform Admin role will remain."
          label="Remove member"
          close={() => setRemove(null)}
          action={async () => {
            await platformApi("platform/organizations/members/delete", {
              organizationId,
              userId: remove.userId,
            });
            await refresh();
          }}
        />
      )}
      {toggleApp && (
        <ConfirmDialog
          title={`${toggleApp.enabled ? "Disable" : "Enable"} ${toggleApp.name}?`}
          description={
            toggleApp.enabled
              ? "All of its API keys will stop working. Its universes will remain, and you can enable it again later."
              : "Existing, unexpired API keys will work again with this service user's current permissions."
          }
          label={
            toggleApp.enabled ? "Disable service user" : "Enable service user"
          }
          close={() => setToggleApp(null)}
          action={async () => {
            await platformApi("platform/organizations/service-users/update", {
              organizationId,
              serviceUserId: toggleApp.id,
              enabled: !toggleApp.enabled,
            });
            await refresh();
          }}
        />
      )}
    </div>
  );
}
