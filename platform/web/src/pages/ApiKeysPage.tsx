import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Plus, ShieldOff } from "lucide-react";
import type { DeploymentApiKeyCreateResponse, DeploymentApiKeyView, MethodGroup } from "@lightspeed-ai/agent-client";
import { GroupSummary, KeyPresetField, MethodGroupPicker } from "@/components/api-keys/method-group-picker";
import { ApiKeySecret } from "@/components/api-keys/secret-once";
import { api } from "@/api";
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
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
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
import { EmptyState, LoadingNote, PageHeader, UniverseNotFound, ShowHiddenToggle } from "@/components/page";
import { useActiveUniverse } from "@/lib/universes";
import { useActionPermissions } from "@/lib/permissions";
import { DEFAULT_UNIVERSE_KEY_GROUPS, groupSummary, groupsFor } from "@/lib/method-groups";

export function ApiKeysPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !permissions.can("manage_access")) {
    return <UniverseNotFound slug={slug} />;
  }

  return <ApiKeyList universeId={universe.id} />;
}

function ApiKeyList({ universeId }: { universeId: string }) {
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const keys = useQuery({
    queryKey: ["api-keys", universeId],
    queryFn: () =>
      api<DeploymentApiKeyView[]>("GET", `/api/v1/universes/${universeId}/api-keys`),
  });
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["api-keys", universeId] });

  const revoke = useMutation({
    mutationFn: (keyPrefix: string) =>
      api<DeploymentApiKeyView>(
        "DELETE",
        `/api/v1/universes/${universeId}/api-keys/${encodeURIComponent(keyPrefix)}`,
      ),
    onSuccess: () => void invalidate(),
  });

  const [showRevoked, setShowRevoked] = useState(false);
  const allRows = [...(keys.data ?? [])].sort((a, b) =>
    Number(a.revokedAtMs != null) - Number(b.revokedAtMs != null) || b.createdAtMs - a.createdAtMs);
  const revokedCount = allRows.filter((key) => key.revokedAtMs != null).length;
  const rows = showRevoked ? allRows : allRows.filter((key) => key.revokedAtMs == null);

  return (
    <>
      <PageHeader
        title="API keys"
        description="Universe credentials for remote agents and the public Configurator MCP endpoint."
        actions={
          <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            Create key
          </Button>
        }
      />
      {keys.isLoading && <LoadingNote />}
      {keys.error && <ReadError error={keys.error} loading={!keys.data} />}
      {revoke.error && (
        <p className="mb-3 text-sm text-destructive">{revoke.error.message}</p>
      )}
      {keys.data && allRows.length === 0 && (
        <EmptyState icon={KeyRound} title="No API keys yet">
          Keys let external agents and clients reach this universe and call the method groups
          they were minted with. A key's secret is shown only once.
        </EmptyState>
      )}
      {rows.length > 0 && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Key</TableHead>
                <TableHead>May call</TableHead>
                <TableHead>Created</TableHead>
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
                    <TableTitleCell
                      title={key.displayName ?? "Unnamed key"}
                      subtitle={key.keyPrefix}
                    />
                    <TableCell className="text-xs text-muted-foreground">
                      <GroupSummary summary={groupSummary("universe", key.groups)} />
                      {key.assertActor && (
                        <span className="mt-1 block">Speaks for people</span>
                      )}
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {formatTimestamp(key.createdAtMs)}
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {key.lastUsedAtMs == null ? "Never" : formatTimestamp(key.lastUsedAtMs)}
                    </TableCell>
                    <TableCell>
                      {revoked ? (
                        <Badge variant="outline">revoked</Badge>
                      ) : (
                        <Badge variant="secondary">active</Badge>
                      )}
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
                                {key.displayName ?? key.keyPrefix} will stop working immediately.
                                This cannot be undone; create a replacement key if needed.
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
      {keys.data && allRows.length > 0 && rows.length === 0 && (
        <p className="text-sm text-muted-foreground">Every key here is revoked.</p>
      )}
      <div className="mt-3">
        <ShowHiddenToggle
          label="Show revoked keys"
          count={revokedCount}
          checked={showRevoked}
          onCheckedChange={setShowRevoked}
        />
      </div>
      <p className="mt-4 text-sm text-muted-foreground">
        Use a key as <code className="font-mono text-xs">Authorization: Bearer lsk_…</code>.
        Keys reach this universe only. A key never changes; revoke it and create another to
        change what it may call.
      </p>
      <CreateApiKeyDialog
        universeId={universeId}
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={invalidate}
      />
    </>
  );
}

function CreateApiKeyDialog({
  universeId,
  open,
  onOpenChange,
  onCreated,
}: {
  universeId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: () => void;
}) {
  const [displayName, setDisplayName] = useState("");
  const [groups, setGroups] = useState<Set<MethodGroup>>(() => new Set(DEFAULT_UNIVERSE_KEY_GROUPS));
  const [created, setCreated] = useState<DeploymentApiKeyCreateResponse | null>(null);
  const allowed = groupsFor("universe");

  const create = useMutation({
    mutationFn: () =>
      api<DeploymentApiKeyCreateResponse>("POST", `/api/v1/universes/${universeId}/api-keys`, {
        displayName: displayName.trim(),
        groups: allowed.filter((group) => groups.has(group)),
      }),
    onSuccess: (result) => {
      setCreated(result);
      onCreated();
    },
  });

  const close = () => {
    onOpenChange(false);
    setDisplayName("");
    setGroups(new Set(DEFAULT_UNIVERSE_KEY_GROUPS));
    setCreated(null);
    create.reset();
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (displayName.trim() && groups.size > 0) {
      create.mutate();
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) {
          close();
        }
      }}
    >
      <DialogContent showCloseButton={!created} className="sm:max-w-lg">
        {created ? (
          <ApiKeySecret secret={created.secret} keyPrefix={created.apiKey.keyPrefix} onDone={close} />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Create API key</DialogTitle>
              <DialogDescription>
                Name the client or agent that will use this key and choose what it may call. A key
                never changes; revoke it and create another to change it.
              </DialogDescription>
            </DialogHeader>
            <form onSubmit={submit} className="grid gap-4">
              <Field>
                <FieldLabel htmlFor="api-key-name">Name</FieldLabel>
                <Input
                  id="api-key-name"
                  value={displayName}
                  onChange={(event) => setDisplayName(event.target.value)}
                  placeholder="Production coding agent"
                  maxLength={120}
                  autoFocus
                  required
                />
                <FieldDescription>Shown in this list; it is not part of the secret.</FieldDescription>
              </Field>
              <KeyPresetField groups={groups} onChange={setGroups} />
              <MethodGroupPicker idPrefix="api-key" allowed={allowed} chosen={groups} onChange={setGroups} />
              {create.error && (
                <p className="text-sm text-destructive">{create.error.message}</p>
              )}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={close}>Cancel</Button>
                <Button type="submit" disabled={create.isPending || !displayName.trim() || groups.size === 0}>
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
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}
