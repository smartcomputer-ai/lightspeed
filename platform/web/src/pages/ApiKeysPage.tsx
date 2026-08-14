import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Copy, KeyRound, Plus, ShieldOff } from "lucide-react";
import {
  api,
  type UniverseApiKey,
  type UniverseApiKeyCreated,
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
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";

export function ApiKeysPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
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
      api<UniverseApiKey[]>("GET", `/api/v1/universes/${universeId}/api-keys`),
  });
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["api-keys", universeId] });

  const revoke = useMutation({
    mutationFn: (keyPrefix: string) =>
      api<UniverseApiKey>(
        "DELETE",
        `/api/v1/universes/${universeId}/api-keys/${encodeURIComponent(keyPrefix)}`,
      ),
    onSuccess: () => void invalidate(),
  });

  const rows = (keys.data ?? []).slice().sort((a, b) => b.createdAtMs - a.createdAtMs);

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
      {keys.error && <p className="text-sm text-destructive">{keys.error.message}</p>}
      {revoke.error && (
        <p className="mb-3 text-sm text-destructive">{revoke.error.message}</p>
      )}
      {keys.data && rows.length === 0 && (
        <div className="flex min-h-40 flex-col items-center justify-center gap-2 rounded-xl border border-dashed p-8 text-center">
          <KeyRound className="size-7 text-muted-foreground" />
          <p className="text-sm font-medium">No API keys</p>
          <p className="max-w-md text-sm text-muted-foreground">
            Create one when an external agent needs access to this universe. The secret is
            shown only once.
          </p>
        </div>
      )}
      {rows.length > 0 && (
        <div className="overflow-x-auto rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Prefix</TableHead>
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
                    <TableCell className="font-medium">
                      {key.displayName ?? "Unnamed key"}
                    </TableCell>
                    <TableCell className="font-mono text-xs">{key.keyPrefix}</TableCell>
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
                    <TableCell>
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
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
      <p className="mt-4 text-sm text-muted-foreground">
        Use a key as <code className="font-mono text-xs">Authorization: Bearer lsk_…</code>.
        Keys grant access to this universe only and can be revoked at any time.
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
  const [created, setCreated] = useState<UniverseApiKeyCreated | null>(null);
  const [copied, setCopied] = useState(false);

  const create = useMutation({
    mutationFn: () =>
      api<UniverseApiKeyCreated>("POST", `/api/v1/universes/${universeId}/api-keys`, {
        displayName,
      }),
    onSuccess: (result) => {
      setCreated(result);
      onCreated();
    },
  });

  const close = () => {
    onOpenChange(false);
    setDisplayName("");
    setCreated(null);
    setCopied(false);
    create.reset();
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (displayName.trim()) {
      create.mutate();
    }
  };

  const copySecret = async () => {
    if (!created) {
      return;
    }
    await navigator.clipboard.writeText(created.secret);
    setCopied(true);
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
      <DialogContent showCloseButton={!created}>
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>Copy your API key</DialogTitle>
              <DialogDescription>
                This secret is shown once. Store it in the agent's secret manager before
                closing this dialog.
              </DialogDescription>
            </DialogHeader>
            <div className="grid gap-2">
              <FieldLabel htmlFor="api-key-secret">API key</FieldLabel>
              <div className="flex gap-2">
                <Input
                  id="api-key-secret"
                  value={created.secret}
                  readOnly
                  className="font-mono text-xs"
                  onFocus={(event) => event.currentTarget.select()}
                />
                <Button type="button" variant="outline" onClick={() => void copySecret()}>
                  {copied ? <Check data-icon="inline-start" /> : <Copy data-icon="inline-start" />}
                  {copied ? "Copied" : "Copy"}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                Identifier: <span className="font-mono">{created.apiKey.keyPrefix}</span>
              </p>
            </div>
            <DialogFooter>
              <Button type="button" onClick={close}>I saved the key</Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Create API key</DialogTitle>
              <DialogDescription>
                Name the client or agent that will use this universe credential.
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
              {create.error && (
                <p className="text-sm text-destructive">{create.error.message}</p>
              )}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={close}>Cancel</Button>
                <Button type="submit" disabled={create.isPending || !displayName.trim()}>
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
