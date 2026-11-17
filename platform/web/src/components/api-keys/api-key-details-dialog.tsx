import type { RefObject } from "react";
import { useMutation } from "@tanstack/react-query";
import { useOrganizations } from "@/lib/platform-api";
import { useUniverses } from "@/lib/universes";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceStatus } from "@/components/resource-table";
import { apiKeyExpiry, apiKeyStatus, revokeApiKey, useInvalidateApiKeys, type ApiKey, type ApiKeyOwner, type ManagedApiKeyOwner } from "./api-keys";

/// One key: what it may reach and when it was used, with Revoke at the end.
export function ApiKeyDetailsDialog({ owner, apiKey, onOpenChange, finalFocus }: {
  owner: ApiKeyOwner;
  apiKey: ApiKey | null;
  onOpenChange: (open: boolean) => void;
  finalFocus?: RefObject<HTMLElement | null>;
}) {
  return (
    <ResourceDetailsDialog open={apiKey !== null} onOpenChange={onOpenChange} finalFocus={finalFocus}
      title={apiKey?.name ?? "API key"}
      description={owner.kind === "personal" ? "Acts as you." : `Acts as ${owner.name}.`}>
      {apiKey && <ApiKeyDetails key={apiKey.id} owner={owner} apiKey={apiKey} onRevoked={() => onOpenChange(false)} />}
    </ResourceDetailsDialog>
  );
}

function ApiKeyDetails({ owner, apiKey, onRevoked }: { owner: ApiKeyOwner; apiKey: ApiKey; onRevoked: () => void }) {
  const organizations = useOrganizations();
  const universes = useUniverses();
  const organizationName = (id: string) => organizations.data?.organizations.find((organization) => organization.id === id)?.name ?? id;
  const universeName = (id: string) => {
    const universe = universes.data?.find((candidate) => candidate.lightspeedUniverseId === id);
    return universe ? `${universe.name} (${universe.handle})` : id;
  };
  const { organizationIds, universeIds, operations, readOnly, allowUniverseCreation } = apiKey.restrictions;
  const list = (values: string[], name: (id: string) => string) => values.length ? values.map(name).join(", ") : "None";
  return (
    <>
      <ResourceDetailsGrid>
        <ResourceDetail label="Key" mono>{apiKey.start ? `${apiKey.start}…` : "—"}</ResourceDetail>
        <ResourceDetail label="Status"><ResourceStatus status={apiKeyStatus(apiKey, owner)} /></ResourceDetail>
        <ResourceDetail label="Organizations">
          {owner.kind === "serviceUser" ? organizationName(owner.organizationId) : organizationIds ? list(organizationIds, organizationName) : owner.kind === "person" ? "All of theirs" : "All of yours"}
        </ResourceDetail>
        <ResourceDetail label="Universes">{universeIds ? list(universeIds, universeName) : "All it can access there"}</ResourceDetail>
        <ResourceDetail label="Access">{readOnly ? "Read only" : "Reads and changes its roles allow"}</ResourceDetail>
        <ResourceDetail label="Universe creation">{allowUniverseCreation ? "Allowed by its roles" : "Not allowed"}</ResourceDetail>
        {operations && <ResourceDetail label="Operations" mono>{operations.length ? operations.join(", ") : "None"}</ResourceDetail>}
        <ResourceDetail label="Expires">{apiKeyExpiry(apiKey)}</ResourceDetail>
        <ResourceDetail label="Created">{new Date(apiKey.createdAt).toLocaleString()}</ResourceDetail>
        <ResourceDetail label="Last used">{apiKey.lastUsedAt ? new Date(apiKey.lastUsedAt).toLocaleString() : "Never"}</ResourceDetail>
      </ResourceDetailsGrid>
      {apiKey.enabled && owner.kind !== "person" && <RevokeAction owner={owner} apiKey={apiKey} onRevoked={onRevoked} />}
      {owner.kind === "person" && (
        <p className="text-sm text-muted-foreground">Only {owner.name} can revoke this key, from their profile.</p>
      )}
    </>
  );
}

function RevokeAction({ owner, apiKey, onRevoked }: { owner: ManagedApiKeyOwner; apiKey: ApiKey; onRevoked: () => void }) {
  const invalidate = useInvalidateApiKeys(owner);
  const revoke = useMutation({
    mutationFn: () => revokeApiKey(owner, apiKey.id),
    onSuccess: async () => { await invalidate(); onRevoked(); },
  });
  return (
    <ResourceActions>
      <ConfirmDangerButton label="Revoke key" title={`Revoke ${apiKey.name}?`} pending={revoke.isPending}
        description="Anything using this key stops working at once. This cannot be undone; create a new key to restore access."
        onConfirm={() => revoke.mutate()} />
      {revoke.error && <p role="alert" className="text-sm text-destructive">{revoke.error.message}</p>}
    </ResourceActions>
  );
}
