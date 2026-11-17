import { DeleteUniverseDialog } from "@/components/admin/delete-universe-dialog";
import { usePlatformAdmin } from "@/lib/permissions";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { DeletedSessionsDialog } from "@/components/admin/deleted-sessions";
import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router-dom";
import { AddIcon } from "@/icons";
import {
  api,
  type EngineUniverse,
  type Universe,
  type UniverseReconcile,
  type UniverseHandleSyncResult,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  IdText,
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

/// Organization views contain only their administrable universes. Deployment
/// reconciliation, orphans and purge controls belong to the Platform inventory.
export function AdminUniversesPage() {
  const universes = useUniverses();
  const isPlatformAdmin = usePlatformAdmin();
  const { organizationId } = useParams();
  const platformAdmin = isPlatformAdmin && !organizationId;
  const organizations = useOrganizations();
  const canCreate = platformAdmin || organizations.data?.organizations.some(organization => organization.enabled && (!organizationId || organization.id === organizationId) && (organization.role === "admin" || organization.role === "creator"));
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [deletedUniverse, setDeletedUniverse] = useState<Universe | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [adopting, setAdopting] = useState<EngineUniverse | null>(null);

  const reconcile = useQuery({
    queryKey: ["universes-reconcile"],
    enabled: platformAdmin,
    queryFn: () => api<UniverseReconcile>("GET", "/api/ui/universes/reconcile"),
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
    mutationFn: () => api<UniverseHandleSyncResult>("POST", "/api/ui/universes/sync-handles", {}),
    onSuccess: refresh,
  });

  const setStatus = useMutation({
    mutationFn: ({ id, status }: { id: string; status: "active" | "archived" }) =>
      api<Universe>("PATCH", `/api/ui/universes/${id}`, { status }),
    onSuccess: refresh,
  });
  const purge = useMutation({
    mutationFn: (universe: Universe) => api("DELETE", `/api/ui/universes/${universe.id}`, { handle: universe.handle }),
    onSuccess: refresh,
  });
  const recreate = useMutation({
    mutationFn: (id: string) => api("POST", `/api/ui/universes/${id}/engine`, {}),
    onSuccess: refresh,
  });
  const deleteOrphan = useMutation({
    mutationFn: (universe: EngineUniverse) =>
      api("DELETE", `/api/ui/universes/engine/${universe.universeId}`, { handle: universe.handle }),
    onSuccess: refresh,
  });

  const rows = (universes.data ?? [])
    .filter(universe => (!organizationId || universe.organizationId === organizationId) && (isPlatformAdmin || universe.role === "owner"))
    .map(universe => {
      const deletion = reconcile.data?.platform.find(row => row.id === universe.id)?.deletion ?? universe.deletion;
      return deletion ? { ...universe, deletion, status: "deleting" as const } : universe;
    })
    .sort((a, b) => a.name.localeCompare(b.name));
  const orphans = platformAdmin ? reconcile.data?.orphans ?? [] : [];

  return (
    <>
      <PageHeader
        title="Universes"
        description={platformAdmin ? "Every universe on this deployment, including archived universes and deletions in progress." : "Universes you administer as an Owner or Organization Admin."}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            {platformAdmin && <Button variant="outline" disabled={syncHandles.isPending} onClick={() => syncHandles.mutate()}>
              {syncHandles.isPending ? "Syncing…" : "Sync from runtime"}
            </Button>}
            {canCreate && <Button onClick={() => setCreateOpen(true)}>
              <AddIcon data-icon="inline-start" />
              New universe
            </Button>}
          </div>
        }
      />
      {platformAdmin && <DeletedSessionsDialog universe={deletedUniverse} onClose={() => setDeletedUniverse(null)} />}
      <NewUniverseDialog organizationId={organizationId} open={createOpen} onOpenChange={setCreateOpen} />
      {syncHandles.error && <p role="alert" className="mb-4 text-sm text-destructive">{syncHandles.error.message}</p>}
      {syncHandles.data && !syncHandles.isPending && !syncHandles.error && (
        <p role="status" className="mb-4 text-sm text-muted-foreground">
          Synced runtime handles: {syncHandles.data.updated} updated.
          {syncHandles.data.skipped > 0 && ` ${syncHandles.data.skipped} missing from the runtime and skipped; these require review.`}
        </p>
      )}
      {(purge.error || deleteOrphan.error || recreate.error) && <p role="alert" className="mb-4 text-sm text-destructive">{(purge.error || deleteOrphan.error || recreate.error)?.message}</p>}
      {universes.isLoading && <LoadingNote />}
      {universes.error && (
        <ReadError error={universes.error} loading={!universes.data} />
      )}
      {platformAdmin && reconcile.error && (
        <ReadError error={reconcile.error} loading={!reconcile.data} prefix="Engine reconciliation unavailable" className="mb-4 text-muted-foreground" />
      )}
      {universes.data && rows.length === 0 && <p className="text-sm text-muted-foreground">You do not administer any universes yet.</p>}
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
                  <TableTitleCell title={universe.name} subtitle={universe.handle} />
                  <TableCell>
                    <Badge
                      variant={universe.status === "active" ? "secondary" : "outline"}
                    >
                      {universe.status}
                    </Badge>
                    {universe.deletion && <p className="mt-1 text-xs text-muted-foreground">{universe.deletion.step}{universe.deletion.detail ? `: ${universe.deletion.detail}` : ""}</p>}
                  </TableCell>
                  {platformAdmin && <TableCell>
                    <EngineBadge status={engineStatus.get(universe.id)} />
                  </TableCell>}
                  <TableCell className="text-muted-foreground">
                    {universe.role ?? "—"}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {new Date(universe.createdAt).toLocaleDateString()}
                  </TableCell>
                  <TableActionsCell>
                    {universe.status !== "deleting" && <>
                      <Button variant="outline" size="sm" onClick={() => navigate(`/admin/organizations/${encodeURIComponent(universe.organizationId!)}/universes/${encodeURIComponent(universe.handle)}/access`)}>Manage</Button>
                      {platformAdmin && <Button variant="ghost" size="sm" onClick={() => setDeletedUniverse(universe)}>Deleted sessions</Button>}
                      {platformAdmin && engineStatus.get(universe.id) === "missing" && (
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
                            setStatus.mutate({ id: universe.id, status: "archived" })
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
                              setStatus.mutate({ id: universe.id, status: "active" })
                            }
                          >
                            Restore
                          </Button>
                        </>
                      )}
                      <DeleteUniverseDialog name={universe.name} handle={universe.handle} protected={universe.protected} onDelete={() => purge.mutate(universe)} />
                    </>}
                  </TableActionsCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      {orphans.length > 0 && (
        <section className="mt-8">
          <h2 className="text-sm font-semibold">On the engine only</h2>
          <p className="mt-1 mb-3 text-sm text-muted-foreground">
            Engine universes no platform universe links to — CLI/test tenants, or
            survivors of a platform reset. Adopt to manage one here, or delete to
            purge it engine-side.
          </p>
          <TableCard>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Engine universe id</TableHead>
                  <TableHead>Sessions</TableHead>
                  <TableHead>Workspaces</TableHead>
                  <TableHead>Profiles</TableHead>
                  <TableHead>Blobs</TableHead>
                  <TableHead className="w-0" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {orphans.map((orphan) => (
                  <TableRow key={orphan.universeId}>
                    <TableCell className="max-w-64">
                      <IdText>{orphan.universeId}</IdText>
                    </TableCell>
                    <TableCell>{orphan.sessions ?? 0}</TableCell>
                    <TableCell>{orphan.workspaces ?? 0}</TableCell>
                    <TableCell>{orphan.profiles ?? 0}</TableCell>
                    <TableCell>{formatBytes(orphan.blobBytes ?? 0)}</TableCell>
                    <TableActionsCell>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setAdopting(orphan)}
                      >
                        Adopt
                      </Button>
                      <DeleteUniverseDialog name={orphan.handle} handle={orphan.handle} protected={orphan.protected} onDelete={() => deleteOrphan.mutate(orphan)} />
                    </TableActionsCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </TableCard>
        </section>
      )}
      {platformAdmin && <AdoptDialog
        key={adopting?.universeId ?? "closed"}
        orphan={adopting}
        onOpenChange={(open) => {
          if (!open) {
            setAdopting(null);
          }
        }}
        onAdopted={refresh}
      />}
    </>
  );
}


function EngineBadge({ status }: { status: "ok" | "missing" | "deleting" | undefined }) {
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

export function AdoptDialog({
  orphan,
  onOpenChange,
  onAdopted,
}: {
  orphan: EngineUniverse | null;
  onOpenChange: (open: boolean) => void;
  onAdopted: () => void;
}) {
  const organizations = useOrganizations();
  const [selectedOrganization, setSelectedOrganization] = useState("");
  const organizationId = selectedOrganization || organizations.data?.organizations.find(a => a.enabled)?.id || "";
  const members = useQuery({ queryKey: ["organization-members", organizationId], enabled: !!orphan && !!organizationId, queryFn: () => platformApi("platform/organizations/members/list", { organizationId }) });
  const [selectedOwner, setSelectedOwner] = useState("");
  const ownerId = members.data?.members.some(m => m.userId === selectedOwner) ? selectedOwner : members.data?.members[0]?.userId ?? "";
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const adopt = useMutation({
    mutationFn: () =>
      api<Universe>("POST", "/api/ui/universes/adopt", {
        organizationId,
        initialOwner: { kind: "user", id: ownerId },
        lightspeedUniverseId: orphan!.universeId,
        name: name.trim(),
      }),
    onSuccess: () => {
      onOpenChange(false);
      setName("");
      setError(null);
      onAdopted();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!name.trim()) {
      setError("a name is required");
      return;
    }
    if (!organizationId || !ownerId || adopt.isPending) return;
    adopt.mutate();
  };

  return (
    <Dialog open={orphan !== null} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Adopt engine universe</DialogTitle>
          <DialogDescription>
            Creates the Platform display entry linked to{" "}
            <span className="font-mono text-xs">{orphan?.universeId}</span>. Engine
            access is managed within the selected organization.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="adopt-organization">Organization</FieldLabel><select id="adopt-organization" className="rounded-md border bg-background p-2" value={organizationId} onChange={e => setSelectedOrganization(e.target.value)} required>{organizations.data?.organizations.filter(a => a.enabled).map(a => <option key={a.id} value={a.id}>{a.name}</option>)}</select>
            <FieldLabel htmlFor="adopt-owner">Initial Owner</FieldLabel><select id="adopt-owner" className="rounded-md border bg-background p-2" value={ownerId} onChange={e => setSelectedOwner(e.target.value)} required>{members.data?.members.map(m => <option key={m.userId} value={m.userId}>{m.name}</option>)}</select>
            {(organizations.error || members.error) && <p role="alert">{(organizations.error || members.error)?.message}</p>}
            <FieldLabel htmlFor="adopt-name">Name</FieldLabel>
            <Input
              id="adopt-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Adopted universe"
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="adopt-handle">Handle</FieldLabel>
            <Input id="adopt-handle" value={orphan?.handle ?? ""} readOnly />
            <p className="text-sm text-muted-foreground">
              Adoption keeps the universe's handle, which Platform URLs use. Rename it afterwards in
              the universe's settings if needed.
            </p>
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={adopt.isPending || !organizationId || !ownerId}>
              {adopt.isPending ? "Adopting…" : "Adopt"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
