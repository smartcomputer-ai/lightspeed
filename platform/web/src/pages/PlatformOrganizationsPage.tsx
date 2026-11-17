import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import type { PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { useOrganizations } from "@/lib/platform-api";
import { organizationAdminPath } from "@/lib/admin-access";
import { AddIcon, UserIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { ButtonGroup } from "@/components/ui/button-group";
import { TableCell } from "@/components/ui/table";
import { EmptyState, LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { CreateOrganizationDialog } from "@/components/admin/organization-dialogs";
import { ResourceIdentityCell, ResourceLinkRow, ResourceTable } from "@/components/resource-table";
import { cn } from "@/lib/utils";

type Organization =
  PlatformOperationResult<"platform/organizations/list">["organizations"][number];
type Scope = "team" | "personal";

/// Every organization on the deployment, for Platform Admins. Personal
/// organizations (one per user) sit behind their own filter so they do not
/// bury the team organizations. A row opens the organization's own admin
/// area.
export function PlatformOrganizationsPage() {
  const client = useQueryClient();
  const navigate = useNavigate();
  const organizations = useOrganizations();
  const [scope, setScope] = useState<Scope>("team");
  const [createOpen, setCreateOpen] = useState(false);
  const all = organizations.data?.organizations ?? [];
  const hasPersonal = all.some((organization) => organization.personal);
  const rows = all
    .filter((organization) => organization.personal === (scope === "personal"))
    .sort((left, right) => left.name.localeCompare(right.name));
  return (
    <>
      <PageHeader
        title="Organizations"
        description="Every organization on this deployment. Open one to manage its universes, people, and settings."
        actions={organizations.data?.canCreate && (
          <Button onClick={() => setCreateOpen(true)}>
            <AddIcon data-icon="inline-start" />
            New organization
          </Button>
        )}
      />
      {organizations.isLoading && <LoadingNote />}
      {organizations.error && <ReadError error={organizations.error} loading={!organizations.data} prefix="Organizations unavailable" />}
      {organizations.data && hasPersonal && (
        <ButtonGroup aria-label="Organization type" className="mb-4">
          <Button size="sm" aria-pressed={scope === "team"} onClick={() => setScope("team")}>Team</Button>
          <Button size="sm" aria-pressed={scope === "personal"} onClick={() => setScope("personal")}>Personal</Button>
        </ButtonGroup>
      )}
      {organizations.data && rows.length === 0 && (
        <EmptyState icon={UserIcon} title={scope === "personal" ? "No personal organizations" : "No team organizations yet"}>
          {scope === "personal"
            ? "Each user receives a personal organization when they first sign in."
            : "Create an organization for a team or business unit."}
        </EmptyState>
      )}
      {rows.length > 0 && (
        <ResourceTable label="Organizations" columns={["Organization", "Created", "Universes"]}>
          {rows.map((organization) => (
            <ResourceLinkRow key={organization.id} name={organization.name} to={`${organizationAdminPath(organization.id)}/universes`}>
              <ResourceIdentityCell
                name={organization.name}
                subtitle={organization.personal ? undefined : organization.slug}
                mono
              />
              <TableCell className="hidden text-muted-foreground sm:table-cell">
                {new Date(organization.createdAt).toLocaleDateString()}
              </TableCell>
              <TableCell>
                <UniverseUsage organization={organization} />
              </TableCell>
            </ResourceLinkRow>
          ))}
        </ResourceTable>
      )}
      {createOpen && (
        <CreateOrganizationDialog
          close={() => setCreateOpen(false)}
          saved={async (organizationId) => {
            await client.invalidateQueries({ queryKey: ["organizations"] });
            navigate(`${organizationAdminPath(organizationId)}/universes`);
            setCreateOpen(false);
          }}
        />
      )}
    </>
  );
}

function UniverseUsage({ organization }: { organization: Organization }) {
  const used = organization.universeCount ?? 0;
  const full = used >= organization.universeQuota;
  return (
    <span
      className={cn("tabular-nums text-muted-foreground", full && "text-amber-700 dark:text-amber-400")}
      title={full ? "Universe quota reached" : undefined}
    >
      {used} / {organization.universeQuota}
    </span>
  );
}
