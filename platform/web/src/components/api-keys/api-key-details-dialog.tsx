import { useState, type RefObject } from "react";
import { useMutation } from "@tanstack/react-query";
import { useOrganizations } from "@/lib/platform-api";
import { useUniverses } from "@/lib/universes";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceStatus } from "@/components/resource-table";
import { Button } from "@/components/ui/button";
import { apiKeyExpiry, apiKeyStatus, replaceApiKey, revokeApiKey, useInvalidateApiKeys, type ApiKey, type ApiKeyOwner, type ManagedApiKeyOwner } from "./api-keys";

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
        <p className="text-sm text-muted-foreground">Only {owner.name} can revoke this key, in their Settings.</p>
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
  // Only this component's state holds the one-time secret.
  const [secret, setSecret] = useState<string | null>(null);
  const replace = useMutation({
    mutationFn: () => replaceApiKey(owner, apiKey.id),
    onSuccess: (result) => { setSecret(result.secret); void invalidate(); },
  });
  if (secret) return <NewSecret secret={secret} />;
  return (
    <ResourceActions>
      <ConfirmDangerButton label="Replace secret" title={`Replace the secret of ${apiKey.name}?`} pending={replace.isPending}
        description="The key keeps its name, access and expiry, and gets a new secret, shown once. The current secret stops working at once."
        onConfirm={() => replace.mutate()} />
      <ConfirmDangerButton label="Revoke key" title={`Revoke ${apiKey.name}?`} pending={revoke.isPending}
        description="Anything using this key stops working at once. This cannot be undone; create a new key to restore access."
        onConfirm={() => revoke.mutate()} />
      {[replace.error, revoke.error].filter(Boolean).map((error, index) => (
        <p key={index} role="alert" className="text-sm text-destructive">{error?.message}</p>
      ))}
    </ResourceActions>
  );
}

/// The replaced key's new secret, shown once.
function NewSecret({ secret }: { secret: string }) {
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);
  return (
    <div className="grid gap-3 border-t pt-4">
      <p className="text-sm font-medium">New secret</p>
      <p className="text-sm text-muted-foreground">Copy it now. It is shown only once; the old secret no longer works.</p>
      <code className="block break-all rounded-lg border bg-muted/40 p-3 text-sm select-all" role="status">{secret}</code>
      {copied === "failed" && <p role="alert" className="text-sm text-destructive">Could not copy the secret. Select and copy it manually.</p>}
      <div>
        <Button variant="outline" onClick={async () => {
          try { await navigator.clipboard.writeText(secret); setCopied("yes"); } catch { setCopied("failed"); }
        }}>{copied === "yes" ? "Copied" : "Copy secret"}</Button>
      </div>
    </div>
  );
}
