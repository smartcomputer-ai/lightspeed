import { DeleteUniverseDialog } from "@/components/admin/delete-universe-dialog";
import { usePlatformAdmin } from "@/lib/permissions";
import { useOrganizations } from "@/lib/platform-api";
import { DeletedSessionsDialog } from "@/components/admin/deleted-sessions";
import { ReadError } from "@/components/read-error";
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router-dom";
import { AddIcon } from "@/icons";
import {
  api,
  type Universe,
  type UniverseReconcile,
  type UniverseHandleSyncResult,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableActionsCell,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableTitleCell,
} from "@/components/ui/table";
import { NewUniverseDialog } from "@/components/universe-switcher";
import { LoadingNote, PageHeader } from "@/components/page";
import { universeHome, useUniverses } from "@/lib/universes";

/// Organization-scoped inventory, with runtime repair controls for deployment admins.
export function AdminUniversesPage() {
  const universes = useUniverses();
  const platformAdmin = usePlatformAdmin();
  const { organizationId } = useParams();
  const organizations = useOrganizations();
  const canCreate = organizations.data?.organizations.some(
    (organization) =>
      organization.id === organizationId &&
      (organization.role === "admin" || organization.role === "creator"),
  );
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [deletedUniverse, setDeletedUniverse] = useState<Universe | null>(null);
  const [createOpen, setCreateOpen] = useState(false);

  const reconcile = useQuery({
    queryKey: ["universes-reconcile", organizationId],
    enabled: platformAdmin && !!organizationId,
    queryFn: () =>
      api<UniverseReconcile>(
        "GET",
        `/api/ui/universes/reconcile?organizationId=${encodeURIComponent(organizationId!)}`,
      ),
    refetchInterval: 5000,
  });
  const engineStatus = new Map(
    (reconcile.data?.platform ?? []).map((row) => [row.id, row.engine]),
  );

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["universes"] });
    void queryClient.invalidateQueries({ queryKey: ["universes-reconcile"] });
  };

  const syncHandles = useMutation({
    mutationFn: () =>
      api<UniverseHandleSyncResult>("POST", "/api/ui/universes/sync-handles", {
        organizationId,
      }),
    onSuccess: refresh,
  });

  const setStatus = useMutation({
    mutationFn: ({
      id,
      status,
    }: {
      id: string;
      status: "active" | "archived";
    }) => api<Universe>("PATCH", `/api/ui/universes/${id}`, { status }),
    onSuccess: refresh,
  });
  const purge = useMutation({
    mutationFn: (universe: Universe) =>
      api("DELETE", `/api/ui/universes/${universe.id}`, {
        handle: universe.handle,
      }),
    onSuccess: refresh,
  });
  const recreate = useMutation({
    mutationFn: (id: string) =>
      api("POST", `/api/ui/universes/${id}/engine`, {}),
    onSuccess: refresh,
  });
  const rows = (universes.data ?? [])
    .filter(
      (universe) =>
        universe.organizationId === organizationId &&
        (platformAdmin || universe.role === "owner"),
    )
    .map((universe) => {
      const deletion =
        reconcile.data?.platform.find((row) => row.id === universe.id)
          ?.deletion ?? universe.deletion;
      return deletion
        ? { ...universe, deletion, status: "deleting" as const }
        : universe;
    })
    .sort((a, b) => a.name.localeCompare(b.name));

  return (
    <>
      <PageHeader
        title="Universes"
        description="Universes you administer in the selected organization, including archived universes and deletions in progress."
        actions={
          <div className="flex flex-wrap items-center gap-2">
            {platformAdmin && (
              <Button
                variant="outline"
                disabled={syncHandles.isPending}
                onClick={() => syncHandles.mutate()}
              >
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
      {platformAdmin && (
        <DeletedSessionsDialog
          universe={deletedUniverse}
          onClose={() => setDeletedUniverse(null)}
        />
      )}
      <NewUniverseDialog
        organizationId={organizationId}
        open={createOpen}
        onOpenChange={setCreateOpen}
      />
      {syncHandles.error && (
        <p role="alert" className="mb-4 text-sm text-destructive">
          {syncHandles.error.message}
        </p>
      )}
      {syncHandles.data && !syncHandles.isPending && !syncHandles.error && (
        <p role="status" className="mb-4 text-sm text-muted-foreground">
          Synced runtime handles: {syncHandles.data.updated} updated.
          {syncHandles.data.skipped > 0 &&
            ` ${syncHandles.data.skipped} missing from the runtime and skipped; these require review.`}
        </p>
      )}
      {(purge.error || recreate.error) && (
        <p role="alert" className="mb-4 text-sm text-destructive">
          {(purge.error || recreate.error)?.message}
        </p>
      )}
      {universes.isLoading && <LoadingNote />}
      {universes.error && (
        <ReadError error={universes.error} loading={!universes.data} />
      )}
      {platformAdmin && reconcile.error && (
        <ReadError
          error={reconcile.error}
          loading={!reconcile.data}
          prefix="Engine reconciliation unavailable"
          className="mb-4 text-muted-foreground"
        />
      )}
      {universes.data && rows.length === 0 && (
        <p className="text-sm text-muted-foreground">
          You do not administer any universes yet.
        </p>
      )}
      {universes.data && rows.length > 0 && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Universe</TableHead>
                <TableHead>Status</TableHead>
                {platformAdmin && <TableHead>Engine</TableHead>}
                <TableHead>Your role</TableHead>
                <TableHead>Created</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((universe) => (
                <TableRow key={universe.id}>
                  <TableTitleCell
                    title={universe.name}
                    subtitle={universe.handle}
                  />
                  <TableCell>
                    <Badge
                      variant={
                        universe.status === "active" ? "secondary" : "outline"
                      }
                    >
                      {universe.status}
                    </Badge>
                    {universe.deletion && (
                      <p className="mt-1 text-xs text-muted-foreground">
                        {universe.deletion.step}
                        {universe.deletion.detail
                          ? `: ${universe.deletion.detail}`
                          : ""}
                      </p>
                    )}
                  </TableCell>
                  {platformAdmin && (
                    <TableCell>
                      <EngineBadge status={engineStatus.get(universe.id)} />
                    </TableCell>
                  )}
                  <TableCell className="text-muted-foreground">
                    {universe.role ?? "—"}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {new Date(universe.createdAt).toLocaleDateString()}
                  </TableCell>
                  <TableActionsCell>
                    {universe.status !== "deleting" && (
                      <>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() =>
                            navigate(
                              `/admin/organizations/${encodeURIComponent(universe.organizationId!)}/universes/${encodeURIComponent(universe.handle)}/access`,
                            )
                          }
                        >
                          Manage
                        </Button>
                        {platformAdmin && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setDeletedUniverse(universe)}
                          >
                            Deleted sessions
                          </Button>
                        )}
                        {platformAdmin &&
                          engineStatus.get(universe.id) === "missing" && (
                            <Button
                              variant="ghost"
                              size="sm"
                              disabled={recreate.isPending}
                              onClick={() => recreate.mutate(universe.id)}
                              title="Re-create the tenant on the engine (it starts empty)"
                            >
                              Recreate on engine
                            </Button>
                          )}
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => navigate(universeHome(universe))}
                        >
                          Open
                        </Button>
                        {universe.status === "active" ? (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive"
                            onClick={() =>
                              setStatus.mutate({
                                id: universe.id,
                                status: "archived",
                              })
                            }
                          >
                            Archive
                          </Button>
                        ) : (
                          <>
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() =>
                                setStatus.mutate({
                                  id: universe.id,
                                  status: "active",
                                })
                              }
                            >
                              Restore
                            </Button>
                          </>
                        )}
                        <DeleteUniverseDialog
                          name={universe.name}
                          handle={universe.handle}
                          protected={universe.protected}
                          onDelete={() => purge.mutate(universe)}
                        />
                      </>
                    )}
                  </TableActionsCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
    </>
  );
}

function EngineBadge({
  status,
}: {
  status: "ok" | "missing" | "deleting" | undefined;
}) {
  if (!status) {
    return <span className="text-muted-foreground">—</span>;
  }
  if (status === "deleting") return <Badge variant="outline">deleting</Badge>;
  if (status === "ok") {
    return <Badge variant="secondary">ok</Badge>;
  }
  return (
    <Badge variant="outline" className="border-destructive/50 text-destructive">
      missing
    </Badge>
  );
}
