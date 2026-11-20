import { useRef, useState } from "react";
import { AddIcon, ApiKeyIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { TableCell } from "@/components/ui/table";
import { EmptyState, LoadingNote, SectionHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { ResourceIdentityCell, ResourceRow, ResourceStatusBadge, ResourceTable, ResourceValue } from "@/components/resource-table";
import { ApiKeyDetailsDialog } from "./api-key-details-dialog";
import { CreateApiKeyDialog } from "./create-api-key-dialog";
import { apiKeyAccessSummary, apiKeyExpiry, apiKeyStatus, useApiKeys, type ManagedApiKeyOwner } from "./api-keys";

/// An owner's keys: a list under its heading, New key on the right, each
/// row opening the key's details.
export function ApiKeysSection({ owner, title = "API keys", description }: {
  owner: ManagedApiKeyOwner;
  title?: string;
  description?: string;
}) {
  const keys = useApiKeys(owner);
  const [createOpen, setCreateOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const disabled = owner.kind === "serviceUser" && !owner.enabled;
  const selected = keys.data?.credentials.find((key) => key.id === selectedId) ?? null;
  return (
    <section aria-label={title} className="min-w-0">
      <SectionHeader title={title} description={description}
        actions={(
          <Button size="sm" disabled={disabled} onClick={() => setCreateOpen(true)}>
            <AddIcon data-icon="inline-start" />
            New key
          </Button>
        )} />
      {disabled && <p className="mb-3 text-sm text-muted-foreground">This service user is disabled: its keys cannot sign in. Enable it to create keys.</p>}
      {keys.isPending && <LoadingNote />}
      {keys.error && <ReadError error={keys.error} loading={!keys.data} prefix="API keys unavailable" />}
      {keys.data?.credentials.length === 0 && (
        <EmptyState icon={ApiKeyIcon} title="No API keys">
          {owner.kind === "serviceUser" ? "Create a key for the integration that signs in as this service user." : "Create a key for scripts and tools that act as you."}
        </EmptyState>
      )}
      {!!keys.data?.credentials.length && (
        <ResourceTable label={title} columns={["Key", "Access", "Expires", "Status"]}>
          {keys.data.credentials.map((key) => (
            <ResourceRow key={key.id} name={key.name} onOpen={(trigger) => { triggerRef.current = trigger; setSelectedId(key.id); }}>
              <ResourceIdentityCell name={key.name} subtitle={key.start ? `${key.start}…` : undefined} mono
                compactDescription={apiKeyAccessSummary(key, owner)} />
              <TableCell className="hidden max-w-56 sm:table-cell"><ResourceValue>{apiKeyAccessSummary(key, owner)}</ResourceValue></TableCell>
              <TableCell className="hidden sm:table-cell"><ResourceValue>{apiKeyExpiry(key)}</ResourceValue></TableCell>
              <TableCell><ResourceStatusBadge status={apiKeyStatus(key, owner)} /></TableCell>
            </ResourceRow>
          ))}
        </ResourceTable>
      )}
      <CreateApiKeyDialog owner={owner} open={createOpen} onOpenChange={setCreateOpen} />
      <ApiKeyDetailsDialog owner={owner} apiKey={selected} finalFocus={triggerRef}
        onOpenChange={(open) => { if (!open) setSelectedId(null); }} />
    </section>
  );
}
