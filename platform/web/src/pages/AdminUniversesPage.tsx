import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { Plus } from "lucide-react";
import {
  api,
  type EngineUniverse,
  type Universe,
  type UniverseReconcile,
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
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { NewUniverseDialog } from "@/components/universe-switcher";
import { LoadingNote, PageHeader } from "@/components/page";
import { universeHome, useUniverses } from "@/lib/universes";

/// The full inventory (platform admins get every universe from the list
/// endpoint, incl. archived ones) — as opposed to the memberships-only
/// switcher. Reconciliation against the engine rides along: each row gets
/// an engine status, and engine universes with no platform row (orphans)
/// are listed below with adopt/delete actions. Not a sync — the platform
/// row is a link plus platform-owned state, so the only invariant worth
/// checking is referential.
export function AdminUniversesPage() {
  const universes = useUniverses();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [createOpen, setCreateOpen] = useState(false);
  const [adopting, setAdopting] = useState<EngineUniverse | null>(null);

  const reconcile = useQuery({
    queryKey: ["universes-reconcile"],
    queryFn: () => api<UniverseReconcile>("GET", "/api/v1/universes/reconcile"),
  });
  const engineStatus = new Map(
    (reconcile.data?.platform ?? []).map((row) => [row.id, row.engine]),
  );

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["universes"] });
    void queryClient.invalidateQueries({ queryKey: ["universes-reconcile"] });
  };

  const setStatus = useMutation({
    mutationFn: ({ id, status }: { id: string; status: "active" | "archived" }) =>
      api<Universe>("PATCH", `/api/v1/universes/${id}`, { status }),
    onSuccess: refresh,
  });
  const purge = useMutation({
    mutationFn: (id: string) => api("DELETE", `/api/v1/universes/${id}`),
    onSuccess: refresh,
  });
  const recreate = useMutation({
    mutationFn: (id: string) => api("POST", `/api/v1/universes/${id}/engine`, {}),
    onSuccess: refresh,
  });
  const deleteOrphan = useMutation({
    mutationFn: (lightspeedUniverseId: string) =>
      api("DELETE", `/api/v1/universes/engine/${lightspeedUniverseId}`),
    onSuccess: refresh,
  });

  const rows = (universes.data ?? [])
    .slice()
    .sort((a, b) => a.name.localeCompare(b.name));
  const orphans = reconcile.data?.orphans ?? [];

  return (
    <>
      <PageHeader
        title="Universes"
        description="Every universe on this deployment, including archived ones."
        actions={
          <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            New universe
          </Button>
        }
      />
      <NewUniverseDialog open={createOpen} onOpenChange={setCreateOpen} />
      {universes.isLoading && <LoadingNote />}
      {universes.error && (
        <p className="text-sm text-destructive">{universes.error.message}</p>
      )}
      {reconcile.error && (
        <p className="mb-4 text-sm text-muted-foreground">
          Engine reconciliation unavailable: {reconcile.error.message}
        </p>
      )}
      {universes.data && (
        <div className="overflow-x-auto rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Slug</TableHead>
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
                  <TableCell className="font-medium">{universe.name}</TableCell>
                  <TableCell className="font-mono text-xs">{universe.slug}</TableCell>
                  <TableCell>
                    <Badge
                      variant={universe.status === "active" ? "secondary" : "outline"}
                    >
                      {universe.status}
                    </Badge>
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
                  <TableCell className="whitespace-nowrap">
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
                      onClick={() => navigate(universeHome(universe.slug, true))}
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
                        <AlertDialog>
                          <AlertDialogTrigger
                            render={
                              <Button
                                variant="ghost"
                                size="sm"
                                className="text-destructive"
                              />
                            }
                          >
                            Delete
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>
                                Permanently delete {universe.name}?
                              </AlertDialogTitle>
                              <AlertDialogDescription>
                                Purges everything engine-side (sessions, profiles,
                                workspaces, blobs) and removes the universe, its
                                members, and routing rules from the platform. This
                                cannot be undone.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Cancel</AlertDialogCancel>
                              <AlertDialogAction
                                className="bg-destructive text-white hover:bg-destructive/90"
                                onClick={() => purge.mutate(universe.id)}
                              >
                                Delete permanently
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      </>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      {orphans.length > 0 && (
        <section className="mt-8">
          <h2 className="text-sm font-semibold">On the engine only</h2>
          <p className="mt-1 mb-3 text-sm text-muted-foreground">
            Engine universes no platform universe links to — CLI/test tenants, or
            survivors of a platform reset. Adopt to manage one here, or delete to
            purge it engine-side.
          </p>
          <div className="overflow-x-auto rounded-xl border">
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
                    <TableCell className="font-mono text-xs">
                      {orphan.universeId}
                    </TableCell>
                    <TableCell>{orphan.sessions ?? 0}</TableCell>
                    <TableCell>{orphan.workspaces ?? 0}</TableCell>
                    <TableCell>{orphan.profiles ?? 0}</TableCell>
                    <TableCell>{formatBytes(orphan.blobBytes ?? 0)}</TableCell>
                    <TableCell className="whitespace-nowrap">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setAdopting(orphan)}
                      >
                        Adopt
                      </Button>
                      <AlertDialog>
                        <AlertDialogTrigger
                          render={
                            <Button
                              variant="ghost"
                              size="sm"
                              className="text-destructive"
                            />
                          }
                        >
                          Delete
                        </AlertDialogTrigger>
                        <AlertDialogContent>
                          <AlertDialogHeader>
                            <AlertDialogTitle>
                              Delete this engine universe?
                            </AlertDialogTitle>
                            <AlertDialogDescription>
                              Purges {orphan.sessions ?? 0} session(s) and{" "}
                              {formatBytes(orphan.blobBytes ?? 0)} of blobs from the
                              engine. No platform state is involved. This cannot be
                              undone.
                            </AlertDialogDescription>
                          </AlertDialogHeader>
                          <AlertDialogFooter>
                            <AlertDialogCancel>Cancel</AlertDialogCancel>
                            <AlertDialogAction
                              className="bg-destructive text-white hover:bg-destructive/90"
                              onClick={() => deleteOrphan.mutate(orphan.universeId)}
                            >
                              Delete permanently
                            </AlertDialogAction>
                          </AlertDialogFooter>
                        </AlertDialogContent>
                      </AlertDialog>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </section>
      )}
      <AdoptDialog
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

function EngineBadge({ status }: { status: "ok" | "missing" | "unchecked" | undefined }) {
  if (!status) {
    return <span className="text-muted-foreground">—</span>;
  }
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

function AdoptDialog({
  orphan,
  onOpenChange,
  onAdopted,
}: {
  orphan: EngineUniverse | null;
  onOpenChange: (open: boolean) => void;
  onAdopted: () => void;
}) {
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const adopt = useMutation({
    mutationFn: () =>
      api<Universe>("POST", "/api/v1/universes/adopt", {
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
    adopt.mutate();
  };

  return (
    <Dialog open={orphan !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Adopt engine universe</DialogTitle>
          <DialogDescription>
            Creates the platform half — organization, your owner membership, and
            the universe row linked to{" "}
            <span className="font-mono text-xs">{orphan?.universeId}</span>. Engine
            data is untouched.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="adopt-name">Name</FieldLabel>
            <Input
              id="adopt-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Adopted universe"
              autoFocus
            />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={adopt.isPending}>
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
