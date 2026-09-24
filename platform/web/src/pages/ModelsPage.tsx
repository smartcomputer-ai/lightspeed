import { ReadError } from "@/components/read-error";
import { useActionPermissions } from "@/lib/permissions";
import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { AddModelProviderDialog } from "@/components/models/add-model-provider-dialog";
import { ModelProviderDetailsDialog } from "@/components/models/model-provider-details-dialog";
import { ModelProviderList } from "@/components/models/model-provider-list";
import {
  useInvalidateModelProviders,
  useModelProviders,
  type ConnectedModelProvider,
} from "@/components/models/use-model-providers";
import { MODEL_PROVIDER_CATALOG, type ModelProviderKind } from "@/components/models/catalog";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { useActiveUniverse } from "@/lib/universes";

export function ModelsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading || permissions.isLoading) return <LoadingNote />;
  if (permissions.error) {
    return <ReadError error={permissions.error} loading prefix="Permissions unavailable" />;
  }
  if (!universe || !permissions.can("read")) return <UniverseNotFound slug={slug} />;
  return <Models universeId={universe.id} slug={universe.slug} />;
}

function Models({ universeId, slug }: { universeId: string; slug: string }) {
  const [searchParams, setSearchParams] = useSearchParams();
  const writable = useActionPermissions(universeId).can("configure_resource");
  const requestedKind = parseModelProviderKind(searchParams.get("add"));
  const [addOpen, setAddOpen] = useState(requestedKind !== null);
  const [initialKind, setInitialKind] = useState<ModelProviderKind | null>(requestedKind);
  // `?add=<kind>` (from the readiness banner) opens the dialog pre-selected,
  // then drops the parameter so a refresh does not reopen it.
  useEffect(() => {
    if (requestedKind) {
      setInitialKind(requestedKind);
      setAddOpen(true);
      const next = new URLSearchParams(searchParams);
      next.delete("add");
      setSearchParams(next, { replace: true });
    }
  }, [requestedKind]);
  const [selected, setSelected] = useState<ConnectedModelProvider | null>(null);
  const { connected, isLoading, error } = useModelProviders(universeId);
  const invalidate = useInvalidateModelProviders(universeId);

  // Keep the details dialog on the freshest copy of its provider.
  const current = selected ? connected.find((i) => i.id === selected.id) ?? selected : null;
  const legacy = connected.some((entry) => "provider" in entry && !entry.provider.usableForModels);

  return (
    <>
      <PageHeader
        title="Models"
        description="Model providers that sessions in this universe can use. Operators add them; keys and logins are never shown again."
        actions={writable && (
          <Button onClick={() => setAddOpen(true)}>
            <Plus data-icon="inline-start" />
            Add provider
          </Button>
        )}
      />
      <ProviderReadinessBanner
        universeId={universeId}
        slug={slug}
        className="mb-4 rounded-lg border"
      />
      {legacy && (
        <p className="mb-4 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          A legacy credential below has an incorrect internal ID and is not used for model calls.
          Remove it and add the provider again.
        </p>
      )}
      {isLoading && <LoadingNote />}
      {error && <p className="text-sm text-destructive">{error.message}</p>}
      {!isLoading && <ModelProviderList providers={connected} onSelect={setSelected} />}
      {writable && (
        <AddModelProviderDialog
          universeId={universeId}
          connected={connected}
          open={addOpen}
          initialKind={initialKind}
          onOpenChange={(open) => {
            setAddOpen(open);
            if (!open) setInitialKind(null);
          }}
          onAdded={() => void invalidate()}
        />
      )}
      <ModelProviderDetailsDialog
        universeId={universeId}
        provider={current}
        onOpenChange={(open) => {
          if (!open) setSelected(null);
        }}
        onChanged={() => void invalidate()}
      />
    </>
  );
}

function parseModelProviderKind(value: string | null): ModelProviderKind | null {
  return MODEL_PROVIDER_CATALOG.find((entry) => entry.kind === value)?.kind ?? null;
}
