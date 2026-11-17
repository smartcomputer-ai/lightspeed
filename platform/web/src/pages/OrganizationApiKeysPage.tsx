import { useRef, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import type { PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { organizationAllows } from "@/lib/permissions";
import { organizationAdminPath } from "@/lib/admin-access";
import { ApiKeyIcon } from "@/icons";
import { TableCell } from "@/components/ui/table";
import { EmptyState, LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { ResourceIdentityCell, ResourceRow, ResourceStatusBadge, ResourceTable, ResourceValue } from "@/components/resource-table";
import { ApiKeyDetailsDialog } from "@/components/api-keys/api-key-details-dialog";
import { apiKeyExpiry, apiKeyStatus, type ApiKeyOwner } from "@/components/api-keys/api-keys";

type OrganizationKey = PlatformOperationResult<"platform/organizations/credentials/list">["credentials"][number];

/// Every key that can sign in to this organization, and whose it is. Keys
/// are made by their owners, so this page lists and explains them only.
export function OrganizationApiKeysPage() {
  const organizations = useOrganizations();
  const { organizationId = "" } = useParams();
  const organization = organizations.data?.organizations.find((candidate) => candidate.id === organizationId && organizationAllows(candidate, "manageMembers"));
  const keys = useQuery({
    queryKey: ["organization-api-keys", organizationId],
    enabled: !!organization,
    queryFn: () => platformApi("platform/organizations/credentials/list", { organizationId }),
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  if (organizations.isLoading) return <LoadingNote />;
  if (!organization) return (
    <>
      <PageHeader title="API keys" />
      <p role="alert">This organization is unavailable or you do not have permission to administer it.</p>
    </>
  );
  const selected = keys.data?.credentials.find((key) => key.id === selectedId) ?? null;
  return (
    <>
      <PageHeader title="API keys" description={`${organization.name} · Every key that can sign in to this organization, and whose it is.`} />
      <p className="mb-6 max-w-3xl text-sm text-muted-foreground">
        Keys are created by their owners. People create keys that act as themselves under{" "}
        <Link className="underline" to="/account#api-keys">Your profile → Your API keys</Link>. Service-user keys are created from
        the service user under{" "}
        <Link className="underline" to={`${organizationAdminPath(organizationId)}/members`}>Members</Link>, where they can also be revoked.
      </p>
      {keys.isPending && <LoadingNote />}
      {keys.error && <ReadError error={keys.error} loading={!keys.data} prefix="API keys unavailable" />}
      {keys.data?.credentials.length === 0 && (
        <EmptyState icon={ApiKeyIcon} title="No API keys reach this organization" />
      )}
      {!!keys.data?.credentials.length && (
        <ResourceTable label="API keys" columns={["Key", "Owner", "Expires", "Status"]}>
          {keys.data.credentials.map((key) => {
            const owner = keyOwner(organizationId, key);
            return (
              <ResourceRow key={key.id} name={key.name} onOpen={(trigger) => { triggerRef.current = trigger; setSelectedId(key.id); }}>
                <ResourceIdentityCell name={key.name} subtitle={key.start ? `${key.start}…` : undefined} mono compactDescription={key.owner.name} />
                <TableCell className="hidden max-w-56 sm:table-cell">
                  <ResourceValue description={key.owner.kind === "user" ? key.owner.email : "Service user"}>{key.owner.name}</ResourceValue>
                </TableCell>
                <TableCell className="hidden sm:table-cell"><ResourceValue>{apiKeyExpiry(key)}</ResourceValue></TableCell>
                <TableCell><ResourceStatusBadge status={apiKeyStatus(key, owner)} /></TableCell>
              </ResourceRow>
            );
          })}
        </ResourceTable>
      )}
      <ApiKeyDetailsDialog owner={selected ? keyOwner(organizationId, selected) : { kind: "personal" }} apiKey={selected} finalFocus={triggerRef}
        onOpenChange={(open) => { if (!open) setSelectedId(null); }} />
    </>
  );
}

/// A service user's key can be revoked here; a person's key only by them.
function keyOwner(organizationId: string, key: OrganizationKey): ApiKeyOwner {
  return key.owner.kind === "serviceUser"
    ? { kind: "serviceUser", organizationId, serviceUserId: key.owner.id, name: key.owner.name, enabled: key.owner.enabled }
    : { kind: "person", name: key.owner.name, email: key.owner.email };
}
