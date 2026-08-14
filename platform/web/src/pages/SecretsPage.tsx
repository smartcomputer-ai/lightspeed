import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, KeyRound, Plus, ShieldOff, Trash2 } from "lucide-react";
import {
  api,
  type SecretGrant,
  type SecretProvider,
  type SecretsInventory,
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
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
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
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  LoadingNote,
  PageHeader,
  SectionHeader,
  UniverseNotFound,
} from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";

type SecretKind = "provider" | "bearer" | "environment";

export function SecretsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return <UniverseNotFound slug={slug} />;
  }

  return <SecretsList universeId={universe.id} />;
}

function SecretsList({ universeId }: { universeId: string }) {
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const inventory = useQuery({
    queryKey: ["secrets", universeId],
    queryFn: () =>
      api<SecretsInventory>("GET", `/api/v1/universes/${universeId}/secrets`),
  });
  const invalidate = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ["secrets", universeId] }),
      queryClient.invalidateQueries({ queryKey: ["auth-grants", universeId] }),
      queryClient.invalidateQueries({ queryKey: ["models", universeId] }),
    ]);

  const removeProvider = useMutation({
    mutationFn: (credentialId: string) =>
      api<SecretProvider>(
        "DELETE",
        `/api/v1/universes/${universeId}/secrets/providers/${encodeURIComponent(credentialId)}`,
      ),
    onSuccess: () => void invalidate(),
  });
  const revokeGrant = useMutation({
    mutationFn: (grantId: string) =>
      api<SecretGrant>(
        "DELETE",
        `/api/v1/universes/${universeId}/secrets/grants/${encodeURIComponent(grantId)}`,
      ),
    onSuccess: () => void invalidate(),
  });

  const providers = (inventory.data?.providers ?? [])
    .slice()
    .sort((a, b) =>
      a.providerId.localeCompare(b.providerId) || a.credentialId.localeCompare(b.credentialId)
    );
  const grants = (inventory.data?.grants ?? [])
    .slice()
    .sort((a, b) => b.createdAtMs - a.createdAtMs);
  const mutationError = removeProvider.error ?? revokeGrant.error;

  return (
    <>
      <PageHeader
        title="Secrets"
        description="Encrypted credentials owned by this universe. Secret values can be set or replaced, but never read back."
        actions={
          <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            Add secret
          </Button>
        }
      />
      {inventory.isLoading && <LoadingNote />}
      {inventory.error && (
        <p className="text-sm text-destructive">{inventory.error.message}</p>
      )}
      {mutationError && (
        <p className="mb-4 text-sm text-destructive">{mutationError.message}</p>
      )}
      {inventory.data && providers.length === 0 && grants.length === 0 && (
        <div className="mb-8 flex min-h-40 flex-col items-center justify-center gap-2 rounded-xl border border-dashed p-8 text-center">
          <KeyRound className="size-7 text-muted-foreground" />
          <p className="text-sm font-medium">No secrets configured</p>
          <p className="max-w-lg text-sm text-muted-foreground">
            Add a model provider API key, environment secret, or bearer token. The resulting
            handle can later be bound to an environment without exposing its value.
          </p>
        </div>
      )}

      {inventory.data && (providers.length > 0 || grants.length > 0) && (
        <div className="grid gap-8">
          <section>
            <SectionHeader
              title="Model provider credentials"
              description="API keys and OAuth connections used by session models for discovery and inference. These are not used for MCP servers or general service authentication."
            />
            {providers.some((provider) => !provider.usableForModels) && (
              <p className="mb-3 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                A legacy credential below has an incorrect internal ID and is not used for model
                calls. Remove it and add the provider again.
              </p>
            )}
            {providers.length === 0 ? (
              <p className="rounded-xl border border-dashed p-5 text-sm text-muted-foreground">
                No model provider credentials.
              </p>
            ) : (
              <div className="overflow-x-auto rounded-xl border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Name</TableHead>
                      <TableHead>Model provider ID</TableHead>
                      <TableHead>Type</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead>Updated</TableHead>
                      <TableHead className="w-0" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {providers.map((provider) => (
                      <TableRow key={provider.credentialId}>
                        <TableCell className="font-medium">
                          {provider.displayName ?? provider.providerId}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {provider.providerId}
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {providerTypeLabel(provider)}
                        </TableCell>
                        <TableCell>
                          <ProviderStatusBadge provider={provider} />
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {formatTimestamp(provider.updatedAtMs)}
                        </TableCell>
                        <TableCell>
                          <AlertDialog>
                            <AlertDialogTrigger
                              render={
                                <Button
                                  variant="ghost"
                                  size="icon-sm"
                                  className="text-destructive"
                                  aria-label={`Remove ${provider.displayName ?? provider.providerId}`}
                                />
                              }
                            >
                              <Trash2 />
                            </AlertDialogTrigger>
                            <AlertDialogContent>
                              <AlertDialogHeader>
                                <AlertDialogTitle>
                                  Remove this model provider credential?
                                </AlertDialogTitle>
                                <AlertDialogDescription>
                                  Session model configurations and future environment bindings that
                                  reference{" "}
                                  <span className="font-mono text-xs">{provider.providerId}</span>{" "}
                                  will stop resolving its credential.
                                </AlertDialogDescription>
                              </AlertDialogHeader>
                              <AlertDialogFooter>
                                <AlertDialogCancel>Cancel</AlertDialogCancel>
                                <AlertDialogAction
                                  className="bg-destructive text-white hover:bg-destructive/90"
                                  onClick={() => removeProvider.mutate(provider.credentialId)}
                                >
                                  Remove model credential
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
            )}
          </section>

          <section>
            <SectionHeader
              title="Access credentials"
              description="Environment secrets, bearer tokens, and OAuth connections that workloads can reference without exposing their values."
            />
            {grants.length === 0 ? (
              <p className="rounded-xl border border-dashed p-5 text-sm text-muted-foreground">
                No access credentials.
              </p>
            ) : (
              <div className="overflow-x-auto rounded-xl border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Name</TableHead>
                      <TableHead>Credential ID</TableHead>
                      <TableHead>Provider</TableHead>
                      <TableHead>Type</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="w-0" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {grants.map((grant) => (
                      <TableRow key={grant.grantId}>
                        <TableCell className="font-medium">
                          {grant.displayName ?? grant.subjectHint ?? "Unnamed credential"}
                        </TableCell>
                        <TableCell className="font-mono text-xs">{grant.grantId}</TableCell>
                        <TableCell className="font-mono text-xs text-muted-foreground">
                          {grant.providerId}
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {grantTypeLabel(grant)}
                        </TableCell>
                        <TableCell>
                          <GrantStatusBadge status={grant.status} />
                        </TableCell>
                        <TableCell>
                          {grant.status !== "revoked" && (
                            <AlertDialog>
                              <AlertDialogTrigger
                                render={
                                  <Button
                                    variant="ghost"
                                    size="icon-sm"
                                    className="text-destructive"
                                    aria-label={`Revoke ${grant.displayName ?? grant.grantId}`}
                                  />
                                }
                              >
                                <ShieldOff />
                              </AlertDialogTrigger>
                              <AlertDialogContent>
                                <AlertDialogHeader>
                                  <AlertDialogTitle>Revoke this access credential?</AlertDialogTitle>
                                  <AlertDialogDescription>
                                    Anything referencing{" "}
                                    <span className="font-mono text-xs">{grant.grantId}</span>{" "}
                                    will stop receiving its token. This cannot be undone.
                                  </AlertDialogDescription>
                                </AlertDialogHeader>
                                <AlertDialogFooter>
                                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                                  <AlertDialogAction
                                    className="bg-destructive text-white hover:bg-destructive/90"
                                    onClick={() => revokeGrant.mutate(grant.grantId)}
                                  >
                                    Revoke credential
                                  </AlertDialogAction>
                                </AlertDialogFooter>
                              </AlertDialogContent>
                            </AlertDialog>
                          )}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            )}
          </section>
        </div>
      )}

      <p className="mt-6 text-sm text-muted-foreground">
        OAuth client setup and browser authorization flows are not exposed here yet. Access
        credentials created through those flows will appear automatically in this inventory.
      </p>
      <CreateSecretDialog
        universeId={universeId}
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={invalidate}
        existingGrants={grants}
      />
    </>
  );
}

function CreateSecretDialog({
  universeId,
  open,
  onOpenChange,
  onCreated,
  existingGrants,
}: {
  universeId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: () => void;
  existingGrants: SecretGrant[];
}) {
  const [kind, setKind] = useState<SecretKind>("provider");
  const [displayName, setDisplayName] = useState("");
  const [modelProviderId, setModelProviderId] = useState("openai");
  const [grantId, setGrantId] = useState("");
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [secret, setSecret] = useState("");
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setKind("provider");
    setDisplayName("");
    setModelProviderId("openai");
    setGrantId("");
    setAdvancedOpen(false);
    setSecret("");
    setError(null);
    create.reset();
  };

  const create = useMutation<SecretProvider | SecretGrant, Error, void>({
    mutationFn: () => {
      if (kind === "provider") {
        return api<SecretProvider>(
          "POST",
          `/api/v1/universes/${universeId}/secrets/providers`,
          {
            providerId: modelProviderId.trim(),
            credential: secret,
            ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
          },
        );
      }
      if (kind === "environment") {
        return api<SecretGrant>(
          "POST",
          `/api/v1/universes/${universeId}/secrets/environment`,
          {
            value: secret,
            ...(grantId.trim() ? { grantId: grantId.trim() } : {}),
            ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
          },
        );
      }
      return api<SecretGrant>(
        "POST",
        `/api/v1/universes/${universeId}/secrets/grants`,
        {
          token: secret,
          ...(grantId.trim() ? { grantId: grantId.trim() } : {}),
          ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        },
      );
    },
    onSuccess: () => {
      onCreated();
      onOpenChange(false);
      reset();
    },
    onError: (reason) => setError(reason.message),
  });

  const changeKind = (nextKind: SecretKind) => {
    setKind(nextKind);
    setModelProviderId("openai");
    setGrantId("");
    setAdvancedOpen(false);
    setSecret("");
    setError(null);
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!secret || (kind === "provider" && !modelProviderId.trim())) {
      setError(
        kind === "provider"
          ? "a model provider ID and secret value are required"
          : "a secret value is required",
      );
      return;
    }
    const existingGrant = existingGrants.find(
      (candidate) => kind !== "provider" && candidate.grantId === grantId.trim(),
    );
    if (existingGrant) {
      setError(credentialIdConflictMessage(existingGrant));
      return;
    }
    create.mutate();
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        onOpenChange(nextOpen);
        if (!nextOpen) {
          reset();
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add secret</DialogTitle>
          <DialogDescription>
            The value is sent once to Lightspeed, encrypted, and never returned by an API.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel>Secret type</FieldLabel>
            <Select value={kind} onValueChange={(value) => changeKind(value as SecretKind)}>
              <SelectTrigger className="w-full" aria-label="Secret type">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="provider">Model provider API key</SelectItem>
                <SelectItem value="environment">Environment secret</SelectItem>
                <SelectItem value="bearer">Bearer token</SelectItem>
              </SelectContent>
            </Select>
            <FieldDescription>
              {kind === "provider"
                ? "Used by sessions to discover models and make inference calls. It is not used for MCP servers or general service authentication."
                : kind === "environment"
                  ? "Stores an opaque value for injection into an environment variable. Multiline values such as SSH private keys are preserved."
                  : "Creates a revocable access credential that MCP servers or environments can reference."}
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="secret-name">Display name</FieldLabel>
            <Input
              id="secret-name"
              value={displayName}
              onChange={(event) => setDisplayName(event.target.value)}
              placeholder={
                kind === "provider"
                  ? "OpenAI production"
                  : kind === "environment"
                    ? "px-dev SSH private key"
                    : "GitHub deploy token"
              }
              autoFocus
            />
          </Field>
          {kind === "provider" ? (
            <Field>
              <FieldLabel htmlFor="secret-resource-id">Model provider ID</FieldLabel>
              <Select
                value={modelProviderId}
                onValueChange={(value) => setModelProviderId(value as string)}
              >
                <SelectTrigger id="secret-resource-id" className="w-full" aria-label="Model provider">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="openai">OpenAI</SelectItem>
                  <SelectItem value="anthropic">Anthropic</SelectItem>
                </SelectContent>
              </Select>
              <FieldDescription>
                Sessions using this <span className="font-mono">model.providerId</span> will use
                the stored key instead of the deployment-wide fallback key.
              </FieldDescription>
            </Field>
          ) : (
            <Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
              <CollapsibleTrigger className="inline-flex h-8 items-center gap-1.5 rounded-md px-2.5 text-sm font-medium text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50">
                Advanced options
                <ChevronDown
                  className={`size-3.5 transition-transform ${advancedOpen ? "rotate-180" : ""}`}
                />
              </CollapsibleTrigger>
              <CollapsibleContent>
                <Field className="mt-2 rounded-lg border bg-muted/15 p-3">
                  <FieldLabel htmlFor="secret-grant-id">Custom credential ID</FieldLabel>
                  <Input
                    id="secret-grant-id"
                    value={grantId}
                    onChange={(event) => {
                      setGrantId(event.target.value);
                      setError(null);
                    }}
                    placeholder="Generated automatically when blank"
                    className="font-mono"
                  />
                  <FieldDescription>
                    Useful for automation and stable references. It must be unique and cannot be
                    changed or reused after this credential is revoked.
                  </FieldDescription>
                </Field>
              </CollapsibleContent>
            </Collapsible>
          )}
          <Field>
            <FieldLabel htmlFor="secret-value">Secret value</FieldLabel>
            {kind === "environment" ? (
              <Textarea
                id="secret-value"
                value={secret}
                onChange={(event) => setSecret(event.target.value)}
                autoComplete="off"
                spellCheck={false}
                className="min-h-36 resize-y font-mono"
                placeholder="Paste the value exactly, including any line breaks"
              />
            ) : (
              <Input
                id="secret-value"
                type="password"
                value={secret}
                onChange={(event) => setSecret(event.target.value)}
                autoComplete="new-password"
                spellCheck={false}
                className="font-mono"
              />
            )}
            <FieldDescription>
              {kind === "environment"
                ? "Line breaks and surrounding whitespace are preserved exactly. The value is cleared from this form after creation."
                : "Whitespace is preserved. Paste the value exactly as issued."}
            </FieldDescription>
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                onOpenChange(false);
                reset();
              }}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? "Encrypting…" : "Add secret"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function credentialIdConflictMessage(grant: SecretGrant): string {
  return grant.status === "revoked"
    ? `Credential ID "${grant.grantId}" belongs to a revoked access credential and cannot be reused. Leave it blank to generate a new ID or choose another.`
    : `Credential ID "${grant.grantId}" already belongs to a ${grant.status} access credential. Leave it blank to generate a new ID or choose another.`;
}

function ProviderStatusBadge({ provider }: { provider: SecretProvider }) {
  if (!provider.usableForModels) {
    return (
      <Badge
        variant="outline"
        className="border-destructive/50 text-destructive"
        title={`Stored as ${provider.credentialId}; expected model:${provider.providerId}`}
      >
        not connected
      </Badge>
    );
  }
  if (provider.status === "active" && provider.hasCredential) {
    return <Badge variant="secondary">configured</Badge>;
  }
  if (provider.status === "needsConfiguration" || !provider.hasCredential) {
    return (
      <Badge variant="outline" className="border-destructive/50 text-destructive">
        needs secret
      </Badge>
    );
  }
  return <Badge variant="outline">{provider.status}</Badge>;
}

function GrantStatusBadge({ status }: { status: SecretGrant["status"] }) {
  if (status === "active") {
    return <Badge variant="secondary">active</Badge>;
  }
  if (status === "needsReauth" || status === "failed") {
    return (
      <Badge variant="outline" className="border-destructive/50 text-destructive">
        {status === "needsReauth" ? "needs reauth" : status}
      </Badge>
    );
  }
  return <Badge variant="outline">revoked</Badge>;
}

function providerTypeLabel(provider: SecretProvider): string {
  if (provider.config.type === "modelApiKey") return "API key";
  if (provider.config.type === "modelOAuth") return "OAuth connection";
  if (provider.config.type === "githubApp") return "GitHub App";
  return provider.providerKind;
}

function grantTypeLabel(grant: SecretGrant): string {
  if (grant.providerId === "environment-secret") return "Environment secret";
  if (grant.providerKind === "staticBearer") return "Bearer token";
  if (grant.hasRefreshToken) return "OAuth (refreshable)";
  return grant.providerKind;
}

function formatTimestamp(timestampMs: number): string {
  if (!timestampMs) return "—";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(timestampMs));
}
