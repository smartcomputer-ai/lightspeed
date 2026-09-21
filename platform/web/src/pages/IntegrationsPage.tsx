import { ReadError } from "@/components/read-error";
import { useActionPermissions } from "@/lib/permissions";
import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { AddIntegrationDialog } from "@/components/integrations/add-integration-dialog";
import { IntegrationDetailsDialog } from "@/components/integrations/integration-details-dialog";
import { IntegrationList } from "@/components/integrations/integration-list";
import {
  useIntegrations,
  useInvalidateIntegrations,
  type ConnectedIntegration,
} from "@/components/integrations/use-integrations";
import { INTEGRATION_CATALOG, type IntegrationKind } from "@/components/integrations/catalog";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { useActiveUniverse } from "@/lib/universes";

export function IntegrationsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading || permissions.isLoading) return <LoadingNote />;
  if (permissions.error) {
    return <ReadError error={permissions.error} loading prefix="Permissions unavailable" />;
  }
  if (!universe || !permissions.can("read")) return <UniverseNotFound slug={slug} />;
  return <Integrations universeId={universe.id} slug={universe.slug} />;
}

function Integrations({ universeId, slug }: { universeId: string; slug: string }) {
  const [searchParams, setSearchParams] = useSearchParams();
  const writable = useActionPermissions(universeId).can("configure_resource");
  const requestedKind = parseIntegrationKind(searchParams.get("add"));
  const [addOpen, setAddOpen] = useState(requestedKind !== null);
  const [initialKind, setInitialKind] = useState<IntegrationKind | null>(requestedKind);
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
  const [selected, setSelected] = useState<ConnectedIntegration | null>(null);
  const { connected, isLoading, error } = useIntegrations(universeId);
  const invalidate = useInvalidateIntegrations(universeId);

  // Keep the details dialog on the freshest copy of its integration.
  const current = selected ? connected.find((i) => i.id === selected.id) ?? selected : null;

  return (
    <>
      <PageHeader
        title="Integrations"
        description="Third-party services connected to this universe — credentials stay encrypted and are never returned."
        actions={writable && (
          <Button onClick={() => setAddOpen(true)}>
            <Plus data-icon="inline-start" />
            Add integration
          </Button>
        )}
      />
      <ProviderReadinessBanner
        universeId={universeId}
        slug={slug}
        className="mb-4 rounded-lg border"
      />
      {isLoading && <LoadingNote />}
      {error && <p className="text-sm text-destructive">{error.message}</p>}
      {!isLoading && <IntegrationList integrations={connected} onSelect={setSelected} />}
      {writable && (
      <AddIntegrationDialog
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
      <IntegrationDetailsDialog
        universeId={universeId}
        integration={current}
        onOpenChange={(open) => {
          if (!open) setSelected(null);
        }}
        onChanged={() => void invalidate()}
      />
    </>
  );
}

function parseIntegrationKind(value: string | null): IntegrationKind | null {
  return INTEGRATION_CATALOG.find((entry) => entry.kind === value)?.kind ?? null;
}
