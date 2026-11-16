import { platformApi, useAccounts } from "@/lib/platform-api";
import { DeletedSessionsDialog } from "@/components/admin/deleted-sessions";
import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { AddIcon } from "@/icons";
import {
  api,
  type EngineUniverse,
  type Universe,
  type UniverseReconcile,
  type UniverseHandleSyncResult,
} from "@/api";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
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

/// The full inventory (platform admins get every universe from the list
/// endpoint, incl. archived ones) — as opposed to the memberships-only
/// switcher. Reconciliation against the engine rides along: each row gets
/// an engine status, and engine universes with no platform row (orphans)
/// are listed below with adopt/delete actions. Reconciliation checks links;
/// updating cached runtime handles requires the explicit sync action.
export function AdminUniversesPage() {
  const universes = useUniverses();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [deletedUniverse, setDeletedUniverse] = useState<Universe | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [adopting, setAdopting] = useState<EngineUniverse | null>(null);

  const reconcile = useQuery({
    queryKey: ["universes-reconcile"],
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
    .map(universe => {
      const deletion = reconcile.data?.platform.find(row => row.id === universe.id)?.deletion ?? universe.deletion;
      return deletion ? { ...universe, deletion, status: "deleting" as const } : universe;
    })
    .sort((a, b) => a.name.localeCompare(b.name));
  const orphans = reconcile.data?.orphans ?? [];

  return (
    <>
      <PageHeader
        title="Universes"
        description="Every universe on this deployment, including archived universes and deletions in progress."
        actions={
          <div className="flex items-center gap-2">
            <Button variant="outline" disabled={syncHandles.isPending} onClick={() => syncHandles.mutate()}>
              {syncHandles.isPending ? "Syncing…" : "Sync from runtime"}
            </Button>
            <Button onClick={() => setCreateOpen(true)}>
              <AddIcon data-icon="inline-start" />
              New universe
            </Button>
          </div>
        }
      />
      <DeletedSessionsDialog universe={deletedUniverse} onClose={() => setDeletedUniverse(null)} />
      <NewUniverseDialog open={createOpen} onOpenChange={setCreateOpen} />
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
      {reconcile.error && (
        <ReadError error={reconcile.error} loading={!reconcile.data} prefix="Engine reconciliation unavailable" className="mb-4 text-muted-foreground" />
      )}
      {universes.data && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Universe</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Engine</TableHead>
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
                  <TableCell>
                    <EngineBadge status={engineStatus.get(universe.id)} />
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {universe.role ?? "—"}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {new Date(universe.createdAt).toLocaleDateString()}
                  </TableCell>
                  <TableActionsCell>
                    {universe.status !== "deleting" && <>
                      <Button variant="ghost" size="sm" onClick={() => setDeletedUniverse(universe)}>Deleted sessions</Button>
                      {engineStatus.get(universe.id) === "missing" && (
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
      <AdoptDialog
        key={adopting?.universeId ?? "closed"}
        orphan={adopting}
        onOpenChange={(open) => {
          if (!open) {
            setAdopting(null);
          }
        }}
        onAdopted={refresh}
      />
    </>
  );
}

function DeleteUniverseDialog({ name, handle, protected: protectedValue, onDelete }: { name: string; handle: string; protected?: boolean; onDelete: () => void }) {
  const [confirmation, setConfirmation] = useState("");
  return <AlertDialog onOpenChange={() => setConfirmation("")}>
    <AlertDialogTrigger render={<Button variant="ghost" size="sm" className="text-destructive" disabled={protectedValue} title={protectedValue ? "Clear deletion protection in universe settings first" : undefined} />}>Delete</AlertDialogTrigger>
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogTitle>Permanently delete {name}?</AlertDialogTitle>
        <AlertDialogDescription>Stops this universe immediately and removes its resources, members and stored content in the background. This cannot be undone.</AlertDialogDescription>
      </AlertDialogHeader>
      <Field>
        <FieldLabel htmlFor={`delete-${handle}`}>Type {handle} to confirm</FieldLabel>
        <Input id={`delete-${handle}`} value={confirmation} onChange={event => setConfirmation(event.target.value)} autoComplete="off" />
      </Field>
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction className="bg-destructive text-white hover:bg-destructive/90" disabled={confirmation !== handle} onClick={onDelete}>Delete permanently</AlertDialogAction>
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>;
}

function EngineBadge({ status }: { status: "ok" | "missing" | "unchecked" | "deleting" | undefined }) {
  if (!status) {
    return <span className="text-muted-foreground">—</span>;
  }
  if (status === "deleting") return <Badge variant="outline">deleting</Badge>;
  if (status === "ok") {
    return <Badge variant="secondary">ok</Badge>;
  }
  if (status === "missing") {
    return (
      <Badge variant="outline" className="border-destructive/50 text-destructive">
        missing
      </Badge>
    );
  }
  return (
    <Badge variant="outline" title="Custom gateway URL — not checked here">
      unchecked
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
  const accounts = useAccounts();
  const [selectedAccount, setSelectedAccount] = useState("");
  const accountId = selectedAccount || accounts.data?.accounts.find(a => a.enabled)?.id || "";
  const members = useQuery({ queryKey: ["account-members", accountId], enabled: !!orphan && !!accountId, queryFn: () => platformApi("platform/accounts/members/list", { accountId }) });
  const [selectedOwner, setSelectedOwner] = useState("");
  const ownerId = members.data?.members.some(m => m.userId === selectedOwner) ? selectedOwner : members.data?.members[0]?.userId ?? "";
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const adopt = useMutation({
    mutationFn: () =>
      api<Universe>("POST", "/api/ui/universes/adopt", {
        accountId,
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
    if (!accountId || !ownerId || adopt.isPending) return;
    adopt.mutate();
  };

  return (
    <Dialog open={orphan !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Adopt engine universe</DialogTitle>
          <DialogDescription>
            Creates the Platform display entry linked to{" "}
            <span className="font-mono text-xs">{orphan?.universeId}</span>. Engine
            access is managed within the selected account.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="adopt-account">Account</FieldLabel><select id="adopt-account" className="rounded-md border bg-background p-2" value={accountId} onChange={e => setSelectedAccount(e.target.value)} required>{accounts.data?.accounts.filter(a => a.enabled).map(a => <option key={a.id} value={a.id}>{a.name}</option>)}</select>
            <FieldLabel htmlFor="adopt-owner">Initial Owner</FieldLabel><select id="adopt-owner" className="rounded-md border bg-background p-2" value={ownerId} onChange={e => setSelectedOwner(e.target.value)} required>{members.data?.members.map(m => <option key={m.userId} value={m.userId}>{m.name}</option>)}</select>
            {(accounts.error || members.error) && <p role="alert">{(accounts.error || members.error)?.message}</p>}
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
            <Button type="submit" disabled={adopt.isPending || !accountId || !ownerId}>
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
