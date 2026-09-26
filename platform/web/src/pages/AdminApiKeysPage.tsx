import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Plus, ShieldOff } from "lucide-react";
import type { DeploymentApiKeyCreateResponse, DeploymentApiKeyView, MethodGroup } from "@lightspeed-ai/agent-client";
import { api, type Universe } from "@/api";
import { ApiKeySecret } from "@/components/api-keys/secret-once";
import { ReadError } from "@/components/read-error";
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
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
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
import { EmptyState, LoadingNote, PageHeader } from "@/components/page";
import { groupsFor, METHOD_GROUPS } from "@/lib/method-groups";
import { useUniverses } from "@/lib/universes";

const DEPLOYMENT = "deployment";

/// Every core key of the deployment: what it reaches, the method groups it
/// may call, and whether it may assert actors. Keys are immutable; changing
/// one is revoke and mint.
export function AdminApiKeysPage() {
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const keys = useQuery({
    queryKey: ["admin-api-keys"],
    queryFn: () => api<DeploymentApiKeyView[]>("GET", "/api/v1/admin/api-keys"),
  });
  const universes = useUniverses();
  const universeName = (lightspeedUniverseId: string) =>
    universes.data?.find((universe) => universe.lightspeedUniverseId === lightspeedUniverseId)?.name ?? lightspeedUniverseId;
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["admin-api-keys"] });
  const revoke = useMutation({
    mutationFn: (keyPrefix: string) =>
      api<DeploymentApiKeyView>("DELETE", `/api/v1/admin/api-keys/${encodeURIComponent(keyPrefix)}`),
    onSuccess: () => void invalidate(),
  });
  const rows = [...(keys.data ?? [])].sort((a, b) =>
    Number(a.revokedAtMs != null) - Number(b.revokedAtMs != null) || b.createdAtMs - a.createdAtMs);

  return (
    <>
      <PageHeader
        title="API keys"
        description="Every key of this deployment: what it reaches, which methods it may call, and whether it speaks for people."
        actions={
          <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            Create key
          </Button>
        }
      />
      {keys.isLoading && <LoadingNote />}
      {keys.error && <ReadError error={keys.error} loading={!keys.data} />}
      {revoke.error && <p className="mb-3 text-sm text-destructive">{revoke.error.message}</p>}
      {keys.data && rows.length === 0 && (
        <EmptyState icon={KeyRound} title="No API keys yet">
          Keys reach the deployment or one universe and call the method groups they were minted
          with.
        </EmptyState>
      )}
      {rows.length > 0 && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Key</TableHead>
                <TableHead>Reaches</TableHead>
                <TableHead>May call</TableHead>
                <TableHead>Last used</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((key) => {
                const revoked = key.revokedAtMs != null;
                return (
                  <TableRow key={key.keyPrefix}>
                    <TableTitleCell title={key.displayName ?? "Unnamed key"} subtitle={key.keyPrefix} />
                    <TableCell>
                      {key.scope.kind === "deployment" ? "Deployment" : universeName(key.scope.universeId)}
                      {key.assertActor && (
                        <span className="mt-1 block text-xs text-muted-foreground">Asserts people</span>
                      )}
                    </TableCell>
                    <TableCell className="max-w-72 text-xs text-muted-foreground">
                      {groupSummary(key)}
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {key.lastUsedAtMs == null ? "Never" : formatTimestamp(key.lastUsedAtMs)}
                    </TableCell>
                    <TableCell>
                      {revoked ? <Badge variant="outline">revoked</Badge> : <Badge variant="secondary">active</Badge>}
                    </TableCell>
                    <TableActionsCell>
                      {!revoked && (
                        <AlertDialog>
                          <AlertDialogTrigger
                            render={
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                className="text-destructive"
                                aria-label={`Revoke ${key.displayName ?? key.keyPrefix}`}
                              />
                            }
                          >
                            <ShieldOff />
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>Revoke this API key?</AlertDialogTitle>
                              <AlertDialogDescription>
                                {key.displayName ?? key.keyPrefix} stops working immediately. This
                                cannot be undone; mint a replacement if needed.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Cancel</AlertDialogCancel>
                              <AlertDialogAction
                                className="bg-destructive text-white hover:bg-destructive/90"
                                onClick={() => revoke.mutate(key.keyPrefix)}
                              >
                                Revoke key
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      )}
                    </TableActionsCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </TableCard>
      )}
      <CreateKeyDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        universes={universes.data ?? []}
        onCreated={() => void invalidate()}
      />
    </>
  );
}

/// "All groups" when a key holds everything its scope allows.
function groupSummary(key: DeploymentApiKeyView): string {
  const allowed = groupsFor(key.scope.kind);
  if (allowed.every((group) => key.groups.includes(group))) return "All groups";
  return key.groups.map((group) => METHOD_GROUPS[group]?.label ?? group).join(", ");
}

function CreateKeyDialog({
  open,
  onOpenChange,
  universes,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  universes: Universe[];
  onCreated: () => void;
}) {
  const [displayName, setDisplayName] = useState("");
  const [scope, setScope] = useState<string>(DEPLOYMENT);
  const [groups, setGroups] = useState<Set<MethodGroup> | null>(null);
  const [assertActor, setAssertActor] = useState(false);
  const [created, setCreated] = useState<DeploymentApiKeyCreateResponse | null>(null);
  const kind = scope === DEPLOYMENT ? "deployment" : "universe";
  const allowed = groupsFor(kind);
  // Null means every group the scope allows, which a new key starts with.
  const chosen = groups ?? new Set(allowed);

  const create = useMutation({
    mutationFn: () =>
      api<DeploymentApiKeyCreateResponse>("POST", "/api/v1/admin/api-keys", {
        displayName: displayName.trim(),
        scope: kind === "deployment" ? { kind } : { kind, universeId: scope },
        ...(groups ? { groups: allowed.filter((group) => groups.has(group)) } : {}),
        assertActor,
      }),
    onSuccess: (result) => {
      setCreated(result);
      onCreated();
    },
  });

  const close = () => {
    onOpenChange(false);
    setDisplayName("");
    setScope(DEPLOYMENT);
    setGroups(null);
    setAssertActor(false);
    setCreated(null);
    create.reset();
  };
  const toggle = (group: MethodGroup, on: boolean) => {
    const next = new Set(chosen);
    if (on) next.add(group);
    else next.delete(group);
    setGroups(next);
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (displayName.trim() && chosen.size > 0) create.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) close(); }}>
      <DialogContent showCloseButton={!created} className="sm:max-w-lg">
        {created ? (
          <ApiKeySecret secret={created.secret} keyPrefix={created.apiKey.keyPrefix} onDone={close} />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Create API key</DialogTitle>
              <DialogDescription>
                Choose what the key reaches and which methods it may call. A key never changes;
                revoke it and mint another to change it.
              </DialogDescription>
            </DialogHeader>
            <form onSubmit={submit} className="grid gap-4">
              <Field>
                <FieldLabel htmlFor="admin-key-name">Name</FieldLabel>
                <Input
                  id="admin-key-name"
                  value={displayName}
                  onChange={(event) => setDisplayName(event.target.value)}
                  placeholder="Telegram connector"
                  maxLength={120}
                  autoFocus
                  required
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="admin-key-scope">Reaches</FieldLabel>
                <Select value={scope} onValueChange={(value) => { setScope(value as string); setGroups(null); }}>
                  <SelectTrigger id="admin-key-scope" className="w-full">
                    <SelectValue>
                      {(value: string) => value === DEPLOYMENT
                        ? "The deployment"
                        : universes.find((universe) => universe.id === value)?.name ?? value}
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={DEPLOYMENT}>The deployment</SelectItem>
                    {universes.map((universe) => (
                      <SelectItem key={universe.id} value={universe.id}>{universe.name}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <FieldDescription>
                  {kind === "deployment"
                    ? "A deployment key names the universe of each call and may hold deployment groups."
                    : "A universe key reaches only this universe."}
                </FieldDescription>
              </Field>
              <fieldset className="grid gap-2">
                <legend className="mb-1 text-sm font-medium">May call</legend>
                <div className="grid gap-2 sm:grid-cols-2">
                  {allowed.map((group) => (
                    <div key={group} className="flex items-center gap-2">
                      <Checkbox
                        id={`admin-key-group-${group}`}
                        checked={chosen.has(group)}
                        onCheckedChange={(checked) => toggle(group, checked === true)}
                      />
                      <Label htmlFor={`admin-key-group-${group}`} className="text-sm font-normal">
                        {METHOD_GROUPS[group].label}
                      </Label>
                    </div>
                  ))}
                </div>
              </fieldset>
              <div className="flex items-center justify-between gap-3 rounded-md border p-3">
                <Label htmlFor="admin-key-assert-actor" className="min-w-0 text-sm">
                  Speaks for people
                  <span className="block text-xs font-normal text-muted-foreground">
                    May name the person a call is for. Give it only to a gate that signs people in,
                    like this Platform.
                  </span>
                </Label>
                <Switch id="admin-key-assert-actor" checked={assertActor} onCheckedChange={setAssertActor} />
              </div>
              {create.error && <p className="text-sm text-destructive">{create.error.message}</p>}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={close}>Cancel</Button>
                <Button type="submit" disabled={create.isPending || !displayName.trim() || chosen.size === 0}>
                  {create.isPending ? "Creating…" : "Create key"}
                </Button>
              </DialogFooter>
            </form>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function formatTimestamp(value: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(value));
}
