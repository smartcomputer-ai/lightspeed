import { useActionPermissions } from "@/lib/permissions";
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ReadError } from "@/components/read-error";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { AddModelProviderDialog } from "@/components/models/add-model-provider-dialog";
import { ModelProviderDetailsDialog } from "@/components/models/model-provider-details-dialog";
import { ModelProviderList } from "@/components/models/model-provider-list";
import {
  useInvalidateModelProviders,
  useModelProviders,
} from "@/components/models/use-model-providers";
import { MODEL_PROVIDER_CATALOG, type ModelProviderKind } from "@/components/models/catalog";
import { DefaultModelDialog, ModelDefaultsSection, type DefaultSlot } from "@/components/models/model-defaults";
import { useModelDefaults } from "@/lib/model-defaults";
import type { ModelDefaults } from "@/api";
import { useActiveUniverse } from "@/lib/universes";

export function ModelsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("read")) return <UniverseNotFound slug={slug} />;
  return <Models key={universe.id} universeId={universe.id} />;
}

function Models({ universeId }: { universeId: string }) {
  const defaults = useModelDefaults(universeId);
  const [editingDefaults, setEditingDefaults] = useState<{ defaults: ModelDefaults; slot: DefaultSlot } | null>(null);
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
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const detailsTriggerRef = useRef<HTMLElement | null>(null);
  const { connected, isLoading, hasData, isComplete, error } = useModelProviders(universeId);
  const invalidate = useInvalidateModelProviders(universeId);

  // Keep the details dialog on the freshest copy of its provider.
  const current = connected.find((provider) => provider.id === selectedId) ?? null;
  const legacy = connected.some((entry) => "provider" in entry && !entry.provider.usableForModels);

  return (
    <>
      <PageHeader
        title="Models"
        description="Provider connections and default models for agents in this universe."
        actions={writable && (
          <Button onClick={() => setAddOpen(true)}>
            <Plus data-icon="inline-start" />
            Add provider
          </Button>
        )}
      />
      <h2 className="mb-3 text-sm font-semibold">Providers</h2>
      {legacy && (
        <p className="mb-4 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          A legacy credential below has an incorrect internal ID and is not used for model calls.
          Remove it and add the provider again.
        </p>
      )}
      {isLoading && connected.length === 0 && <LoadingNote />}
      {error && <ReadError error={error} loading={!hasData} prefix="Model providers unavailable" />}
      <ModelProviderList providers={connected} showEmpty={isComplete && !error} onSelect={(provider, trigger) => {
        detailsTriggerRef.current = trigger;
        setSelectedId(provider.id);
      }} />
      <ModelDefaultsSection universeId={universeId} writable={writable} onEdit={(defaults, slot) => setEditingDefaults({ defaults, slot })} />
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
          onChooseDefault={defaults.data && !defaults.error ? () => setEditingDefaults({ defaults: defaults.data!, slot: "agentRun" }) : undefined}
        />
      )}
      {writable && editingDefaults && <DefaultModelDialog universeId={universeId} initial={editingDefaults.defaults} slot={editingDefaults.slot} onClose={() => setEditingDefaults(null)} />}
      <ModelProviderDetailsDialog
        universeId={universeId}
        provider={current}
        finalFocus={detailsTriggerRef}
        onOpenChange={(open) => {
          if (!open) setSelectedId(null);
        }}
        onChanged={() => void invalidate()}
      />
    </>
  );
}

function parseModelProviderKind(value: string | null): ModelProviderKind | null {
  return MODEL_PROVIDER_CATALOG.find((entry) => entry.kind === value)?.kind ?? null;
}
