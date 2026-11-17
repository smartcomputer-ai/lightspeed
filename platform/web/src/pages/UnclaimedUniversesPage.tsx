import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type EngineUniverse,
  type Universe,
  type UniverseReconcile,
} from "@/api";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { DeleteUniverseDialog } from "@/components/admin/delete-universe-dialog";
import { ReadError } from "@/components/read-error";
import { LoadingNote, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
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

/** Advanced deployment administration for runtime universes with no organization. */
export function UnclaimedUniversesPage() {
  const client = useQueryClient();
  const [adopting, setAdopting] = useState<EngineUniverse | null>(null);
  const inventory = useQuery({
    queryKey: ["universes-reconcile", "unclaimed"],
    queryFn: () => api<UniverseReconcile>("GET", "/api/ui/universes/reconcile"),
    refetchInterval: 5000,
  });
  const refresh = () => {
    void client.invalidateQueries({ queryKey: ["universes"] });
    void client.invalidateQueries({ queryKey: ["universes-reconcile"] });
  };
  const remove = useMutation({
    mutationFn: (universe: EngineUniverse) =>
      api("DELETE", `/api/ui/universes/engine/${universe.universeId}`, {
        handle: universe.handle,
      }),
    onSuccess: refresh,
  });
  const universes = [...(inventory.data?.orphans ?? [])].sort((a, b) =>
    (a.displayName || a.handle).localeCompare(b.displayName || b.handle),
  );
  return (
    <>
      <PageHeader
        title="Unclaimed Universes"
        description="Advanced deployment administration: these universes exist in the runtime but do not belong to a Platform organization. Adopt one to manage it within an organization."
      />
      {inventory.isLoading && <LoadingNote />}
      {inventory.error && (
        <ReadError
          error={inventory.error}
          loading={!inventory.data}
          prefix="Runtime inventory unavailable"
        />
      )}
      {remove.error && (
        <p role="alert" className="mb-4 text-sm text-destructive">
          {remove.error.message}
        </p>
      )}
      {inventory.data && !inventory.error && !universes.length && (
        <p className="text-sm text-muted-foreground">No unclaimed universes.</p>
      )}
      {!!universes.length && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Universe</TableHead>
                <TableHead>Runtime ID</TableHead>
                <TableHead>Sessions</TableHead>
                <TableHead>Workspaces</TableHead>
                <TableHead>Profiles</TableHead>
                <TableHead>Blobs</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {universes.map((universe) => (
                <TableRow key={universe.universeId}>
                  <TableTitleCell
                    title={universe.displayName?.trim() || universe.handle}
                    subtitle={
                      universe.displayName?.trim() ? universe.handle : undefined
                    }
                  />
                  <TableCell className="max-w-64">
                    <IdText>{universe.universeId}</IdText>
                  </TableCell>
                  <TableCell>{universe.sessions ?? 0}</TableCell>
                  <TableCell>{universe.workspaces ?? 0}</TableCell>
                  <TableCell>{universe.profiles ?? 0}</TableCell>
                  <TableCell>{formatBytes(universe.blobBytes ?? 0)}</TableCell>
                  <TableActionsCell>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setAdopting(universe)}
                    >
                      Adopt
                    </Button>
                    <DeleteUniverseDialog
                      name={universe.displayName?.trim() || universe.handle}
                      handle={universe.handle}
                      protected={universe.protected}
                      onDelete={() => remove.mutate(universe)}
                    />
                  </TableActionsCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      {adopting && (
        <AdoptDialog
          key={adopting.universeId}
          orphan={adopting}
          onOpenChange={(open) => {
            if (!open) setAdopting(null);
          }}
          onAdopted={refresh}
        />
      )}
    </>
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
  const organizationId =
    selectedOrganization ||
    organizations.data?.organizations.find((a) => a.enabled)?.id ||
    "";
  const members = useQuery({
    queryKey: ["organization-members", organizationId],
    enabled: !!orphan && !!organizationId,
    queryFn: () =>
      platformApi("platform/organizations/members/list", { organizationId }),
  });
  const [selectedOwner, setSelectedOwner] = useState("");
  const ownerId = members.data?.members.some((m) => m.userId === selectedOwner)
    ? selectedOwner
    : (members.data?.members[0]?.userId ?? "");
  const [name, setName] = useState(
    orphan?.displayName?.trim() || orphan?.handle || "",
  );
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
          <DialogTitle>Adopt unclaimed universe</DialogTitle>
          <DialogDescription>
            Assigns the runtime universe to an organization and initial Owner.
            Runtime ID:{" "}
            <span className="font-mono text-xs">{orphan?.universeId}</span>.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="adopt-organization">Organization</FieldLabel>
            <Select
              value={organizationId}
              onValueChange={(value) => value !== null && setSelectedOrganization(value)}
              required
            >
              <SelectTrigger id="adopt-organization"><SelectValue /></SelectTrigger>
              <SelectContent>
                {organizations.data?.organizations
                  .filter((a) => a.enabled)
                  .map((a) => (
                    <SelectItem key={a.id} value={a.id}>
                      {a.name}
                    </SelectItem>
                  ))}
              </SelectContent>
            </Select>
            <FieldLabel htmlFor="adopt-owner">Initial Owner</FieldLabel>
            <Select
              value={ownerId}
              onValueChange={(value) => value !== null && setSelectedOwner(value)}
              required
            >
              <SelectTrigger id="adopt-owner"><SelectValue /></SelectTrigger>
              <SelectContent>
                {members.data?.members.map((m) => (
                  <SelectItem key={m.userId} value={m.userId}>
                    {m.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {(organizations.error || members.error) && (
              <p role="alert">
                {(organizations.error || members.error)?.message}
              </p>
            )}
            <FieldLabel htmlFor="adopt-name">Name</FieldLabel>
            <Input
              id="adopt-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
              maxLength={120}
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="adopt-handle">Handle</FieldLabel>
            <Input id="adopt-handle" value={orphan?.handle ?? ""} readOnly />
            <p className="text-sm text-muted-foreground">
              Adoption keeps the universe's handle, which Platform URLs use.
              Rename it afterwards in the universe's settings if needed.
            </p>
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={adopt.isPending || !organizationId || !ownerId}
            >
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
