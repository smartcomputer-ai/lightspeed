import { organizationAllows, useDeploymentPermissions } from "@/lib/permissions";
import { useOrganizations } from "@/lib/platform-api";
import { ReadError } from "@/components/read-error";
import { useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { AddIcon, UniversesIcon } from "@/icons";
import {
  api,
  type Universe,
  type UniverseReconcile,
  type UniverseHandleSyncResult,
} from "@/api";
import { Button } from "@/components/ui/button";
import { TableCell } from "@/components/ui/table";
import { NewUniverseDialog } from "@/components/universe-switcher";
import { EmptyState, LoadingNote, PageHeader } from "@/components/page";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceIdentityCell, ResourceRow, ResourceStatus, ResourceStatusBadge, ResourceTable, ResourceValue } from "@/components/resource-table";
import type { ResourceStatusInfo } from "@/lib/resource-status";
import { universeHome, useUniverses } from "@/lib/universes";

type EngineState = "ok" | "missing" | "deleting" | undefined;

function universeStatus(universe: Universe, engine: EngineState): ResourceStatusInfo {
  if (universe.status === "deleting") {
    return { label: "Deleting", tone: "pending", description: universe.deletion ? `${universe.deletion.step}${universe.deletion.detail ? `: ${universe.deletion.detail}` : ""}` : undefined };
  }
  if (engine === "missing") return { label: "Missing on engine", tone: "danger", description: "The runtime no longer has this universe." };
  if (universe.status === "archived") return { label: "Archived", tone: "neutral" };
  return { label: "Active", tone: "positive" };
}

const ROLE_LABELS: Record<string, string> = { viewer: "Viewer", contributor: "Contributor", operator: "Operator", owner: "Owner" };

/// The universes of one organization that the caller administers. A row
/// opens the universe's details; its settings, members and lifecycle live in
/// the universe itself.
export function AdminUniversesPage() {
  const universes = useUniverses();
  // Runtime inventory and repair are deployment administration.
  const runtimeRepair = useDeploymentPermissions().can("manageUnclaimedUniverses");
  const { organizationId } = useParams();
  const organizations = useOrganizations();
  const canCreate = organizations.data?.organizations.some(
    (organization) => organization.id === organizationId && organizationAllows(organization, "createUniverse"),
  );
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);

  const reconcile = useQuery({
    queryKey: ["universes-reconcile", organizationId],
    enabled: runtimeRepair && !!organizationId,
    queryFn: () => api<UniverseReconcile>("GET", `/api/ui/universes/reconcile?organizationId=${encodeURIComponent(organizationId!)}`),
    refetchInterval: 5000,
  });
  const engineStatus = new Map((reconcile.data?.platform ?? []).map((row) => [row.id, row.engine]));
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["universes"] });
    void queryClient.invalidateQueries({ queryKey: ["universes-reconcile"] });
  };
  const syncHandles = useMutation({
    mutationFn: () => api<UniverseHandleSyncResult>("POST", "/api/ui/universes/sync-handles", { organizationId }),
    onSuccess: refresh,
  });
  const rows = (universes.data ?? [])
    .filter((universe) => universe.organizationId === organizationId && universe.actions?.includes("administer"))
    .map((universe) => {
      const deletion = reconcile.data?.platform.find((row) => row.id === universe.id)?.deletion ?? universe.deletion;
      return deletion ? { ...universe, deletion, status: "deleting" as const } : universe;
    })
    .sort((a, b) => a.name.localeCompare(b.name));
  const selected = rows.find((universe) => universe.id === selectedId) ?? null;

  return (
    <>
      <PageHeader
        title="Universes"
        description="Universes you administer in this organization, including archived ones and deletions in progress."
        actions={
          <div className="flex flex-wrap items-center gap-2">
            {runtimeRepair && (
              <Button variant="outline" disabled={syncHandles.isPending} onClick={() => syncHandles.mutate()}>
                {syncHandles.isPending ? "Syncing…" : "Sync from runtime"}
              </Button>
            )}
            {canCreate && (
              <Button onClick={() => setCreateOpen(true)}>
                <AddIcon data-icon="inline-start" />
                New universe
              </Button>
            )}
          </div>
        }
      />
      <NewUniverseDialog organizationId={organizationId} open={createOpen} onOpenChange={setCreateOpen} />
      {syncHandles.error && <p role="alert" className="mb-4 text-sm text-destructive">{syncHandles.error.message}</p>}
      {syncHandles.data && !syncHandles.isPending && !syncHandles.error && (
        <p role="status" className="mb-4 text-sm text-muted-foreground">
          Synced runtime handles: {syncHandles.data.updated} updated.
          {syncHandles.data.skipped > 0 && ` ${syncHandles.data.skipped} missing from the runtime and skipped; these require review.`}
        </p>
      )}
      {universes.isLoading && <LoadingNote />}
      {universes.error && <ReadError error={universes.error} loading={!universes.data} />}
      {runtimeRepair && reconcile.error && (
        <ReadError error={reconcile.error} loading={!reconcile.data} prefix="Engine reconciliation unavailable" className="mb-4 text-muted-foreground" />
      )}
      {universes.data && rows.length === 0 && (
        <EmptyState icon={UniversesIcon} title="No universes to administer">
          Universes you own, or every universe of an organization you administer, appear here.
        </EmptyState>
      )}
      {rows.length > 0 && (
        <ResourceTable label="Universes" columns={["Universe", "Your role", "Created", "Status"]}>
          {rows.map((universe) => (
            <ResourceRow key={universe.id} name={universe.name} onOpen={(trigger) => { triggerRef.current = trigger; setSelectedId(universe.id); }}>
              <ResourceIdentityCell name={universe.name} subtitle={universe.handle} mono />
              <TableCell className="hidden sm:table-cell"><ResourceValue>{universe.role ? ROLE_LABELS[universe.role] ?? universe.role : "—"}</ResourceValue></TableCell>
              <TableCell className="hidden sm:table-cell"><ResourceValue>{new Date(universe.createdAt).toLocaleDateString()}</ResourceValue></TableCell>
              <TableCell><ResourceStatusBadge status={universeStatus(universe, engineStatus.get(universe.id))} /></TableCell>
            </ResourceRow>
          ))}
        </ResourceTable>
      )}
      <ResourceDetailsDialog open={selected !== null} onOpenChange={(open) => { if (!open) setSelectedId(null); }} finalFocus={triggerRef}
        title={selected?.name ?? "Universe"} description={selected ? `/u/${selected.handle}` : ""}>
        {selected && <UniverseDetails universe={selected} engine={engineStatus.get(selected.id)} runtimeRepair={runtimeRepair} onRepaired={refresh} />}
      </ResourceDetailsDialog>
    </>
  );
}

/// The universe at a glance, with the way into it and into its settings.
/// Deployment administrators can also recreate a universe the runtime lost.
function UniverseDetails({ universe, engine, runtimeRepair, onRepaired }: {
  universe: Universe;
  engine: EngineState;
  runtimeRepair: boolean;
  onRepaired: () => void;
}) {
  const recreate = useMutation({
    mutationFn: () => api("POST", `/api/ui/universes/${universe.id}/engine`, {}),
    onSuccess: onRepaired,
  });
  const deleting = universe.status === "deleting";
  return (
    <>
      <ResourceDetailsGrid>
        <ResourceDetail label="Handle" mono>{universe.handle}</ResourceDetail>
        <ResourceDetail label="Status"><ResourceStatus status={universeStatus(universe, engine)} /></ResourceDetail>
        <ResourceDetail label="Your role">{universe.role ? ROLE_LABELS[universe.role] ?? universe.role : "—"}</ResourceDetail>
        <ResourceDetail label="Created">{new Date(universe.createdAt).toLocaleDateString()}</ResourceDetail>
        <ResourceDetail label="Deletion protection">{universe.protected ? "Protected" : "Not protected"}</ResourceDetail>
        {runtimeRepair && <ResourceDetail label="Runtime">{engine === "missing" ? "Missing" : engine === "ok" ? "Present" : engine === "deleting" ? "Deleting" : "—"}</ResourceDetail>}
      </ResourceDetailsGrid>
      {!deleting && (
        <div className="flex flex-wrap gap-2">
          <Button nativeButton={false} render={<Link to={universeHome(universe)} />}>Open universe</Button>
          <Button nativeButton={false} variant="outline" render={<Link to={`/u/${encodeURIComponent(universe.handle)}/settings/general`} />}>Settings</Button>
          <Button nativeButton={false} variant="outline" render={<Link to={`/u/${encodeURIComponent(universe.handle)}/settings/members`} />}>Members</Button>
        </div>
      )}
      {runtimeRepair && engine === "missing" && !deleting && (
        <ResourceActions>
          <Button variant="outline" size="sm" disabled={recreate.isPending} onClick={() => recreate.mutate()}>
            {recreate.isPending ? "Recreating…" : "Recreate on engine"}
          </Button>
          <p className="text-sm text-muted-foreground">The runtime lost this universe. Recreating it starts empty.</p>
          {recreate.error && <p role="alert" className="text-sm text-destructive">{recreate.error.message}</p>}
        </ResourceActions>
      )}
    </>
  );
}
