import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import type {
  EnvironmentProviderBindingView,
  DeploymentEnvironmentProviderView,
} from "@lightspeed-ai/agent-client";
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
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
import { LoadingNote, PageHeader } from "@/components/page";

type Provider = DeploymentEnvironmentProviderView;

/// One platform universe with the provider bindings the engine reports for
/// it (see `GET /api/v1/admin/environment-provider-bindings`).
type UniverseBindings = {
  universeId: string;
  lightspeedUniverseId: string;
  name: string;
  status: "active" | "archived";
  bindings: EnvironmentProviderBindingView[];
  error: string | null;
};

type TransportKind = "webSocket" | "http" | "provider";

/// Deployment-scoped environment provider administration: physical
/// providers are registered once by a platform admin and shared with
/// universes through revisioned bindings. Universe owners consume enabled
/// bindings from their Environments page; only admins mutate them here.
export function AdminEnvironmentProvidersPage() {
  const queryClient = useQueryClient();
  const [registerOpen, setRegisterOpen] = useState(false);
  const [bindingFor, setBindingFor] = useState<Provider | null>(null);

  const providers = useQuery({
    queryKey: ["admin-environment-providers"],
    queryFn: () => api<Provider[]>("GET", "/api/v1/admin/environment-providers"),
  });
  const bindings = useQuery({
    queryKey: ["admin-environment-provider-bindings"],
    queryFn: () =>
      api<UniverseBindings[]>("GET", "/api/v1/admin/environment-provider-bindings"),
  });

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["admin-environment-providers"] });
    void queryClient.invalidateQueries({ queryKey: ["admin-environment-provider-bindings"] });
    void queryClient.invalidateQueries({ queryKey: ["environment-provider-bindings"] });
  };

  const removeProvider = useMutation({
    mutationFn: (providerId: string) =>
      api<Provider>("DELETE", `/api/v1/admin/environment-providers/${encodeURIComponent(providerId)}`),
    onSuccess: refresh,
  });

  const rows = (providers.data ?? [])
    .slice()
    .sort((a, b) => a.providerId.localeCompare(b.providerId));
  const universes = bindings.data ?? [];
  const bindingsByProvider = new Map<string, Array<{ universe: UniverseBindings; binding: EnvironmentProviderBindingView }>>();
  for (const universe of universes) {
    for (const binding of universe.bindings) {
      const list = bindingsByProvider.get(binding.providerId) ?? [];
      list.push({ universe, binding });
      bindingsByProvider.set(binding.providerId, list);
    }
  }

  return (
    <>
      <PageHeader
        title="Environment providers"
        description="Physical compute providers registered on this deployment, and which universes may provision from them."
        actions={
          <Button onClick={() => setRegisterOpen(true)}>
            <Plus data-icon="inline-start" />
            Register provider
          </Button>
        }
      />
      <RegisterProviderDialog
        open={registerOpen}
        onOpenChange={setRegisterOpen}
        onSaved={refresh}
      />
      <BindUniverseDialog
        provider={bindingFor}
        universes={universes}
        onOpenChange={(open) => { if (!open) setBindingFor(null); }}
        onSaved={refresh}
      />
      {providers.isLoading && <LoadingNote />}
      {providers.error && (
        <ReadError error={providers.error} loading={!providers.data} />
      )}
      {bindings.error && (
        <ReadError error={bindings.error} loading={!bindings.data} prefix="Binding inventory unavailable" className="mb-4 text-muted-foreground" />
      )}
      {removeProvider.error && (
        <p className="mb-4 text-sm text-destructive">{removeProvider.error.message}</p>
      )}
      {providers.data && rows.length === 0 && (
        <p className="rounded-xl border border-dashed p-6 text-sm text-muted-foreground">
          No environment providers are registered. Register one to let universes
          provision environments.
        </p>
      )}
      <div className="grid gap-6">
        {rows.map((provider) => {
          const providerBindings = (bindingsByProvider.get(provider.providerId) ?? [])
            .sort((a, b) => a.universe.name.localeCompare(b.universe.name));
          return (
            <section key={provider.providerId} className="rounded-xl border">
              <div className="flex flex-wrap items-start justify-between gap-3 border-b px-4 py-3 md:px-5">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h2 className="text-sm font-semibold">
                      {provider.displayName ?? provider.providerId}
                    </h2>
                    <span className="font-mono text-xs text-muted-foreground">
                      {provider.providerId}
                    </span>
                    <Badge variant="outline">{transportLabel(provider)}</Badge>
                  </div>
                  <p className="mt-1 truncate font-mono text-xs text-muted-foreground">
                    {provider.controllerConnection.endpoint}
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <Button variant="outline" size="sm" onClick={() => setBindingFor(provider)}>
                    <Plus data-icon="inline-start" />
                    Bind universe
                  </Button>
                  <AlertDialog>
                    <AlertDialogTrigger
                      render={
                        <Button
                          variant="destructive"
                          size="sm"
                          disabled={removeProvider.isPending}
                        />
                      }
                    >
                      <Trash2 />
                      Delete
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                      <AlertDialogHeader>
                        <AlertDialogTitle>
                          Delete provider {provider.displayName ?? provider.providerId}?
                        </AlertDialogTitle>
                        <AlertDialogDescription>
                          Removes the registration from this deployment. The engine
                          rejects the delete while any universe binding still
                          references it; delete those bindings first. Provider-side
                          machines are not touched.
                        </AlertDialogDescription>
                      </AlertDialogHeader>
                      <AlertDialogFooter>
                        <AlertDialogCancel>Cancel</AlertDialogCancel>
                        <AlertDialogAction
                          className="bg-destructive text-white hover:bg-destructive/90"
                          onClick={() => removeProvider.mutate(provider.providerId)}
                        >
                          Delete provider
                        </AlertDialogAction>
                      </AlertDialogFooter>
                    </AlertDialogContent>
                  </AlertDialog>
                </div>
              </div>
              <div className="px-4 py-3 md:px-5">
                {Object.keys(provider.metadata ?? {}).length > 0 && (
                  <dl className="mb-3 flex flex-wrap gap-x-6 gap-y-1 text-xs text-muted-foreground">
                    {Object.entries(provider.metadata ?? {}).map(([key, value]) => (
                      <div key={key}>
                        <dt className="inline font-medium">{key}: </dt>
                        <dd className="inline font-mono">{value}</dd>
                      </div>
                    ))}
                  </dl>
                )}
                {providerBindings.length === 0 ? (
                  <p className="text-sm text-muted-foreground">
                    No universe is bound to this provider yet.
                  </p>
                ) : (
                  <TableCard className="rounded-lg">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Universe</TableHead>
                          <TableHead>Binding</TableHead>
                          <TableHead>Status</TableHead>
                          <TableHead>Revision</TableHead>
                          <TableHead className="w-0" />
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {providerBindings.map(({ universe, binding }) => (
                          <BindingRow
                            key={`${universe.universeId}:${binding.bindingId}`}
                            universe={universe}
                            binding={binding}
                            onChanged={refresh}
                          />
                        ))}
                      </TableBody>
                    </Table>
                  </TableCard>
                )}
              </div>
            </section>
          );
        })}
      </div>
      {universes.some((universe) => universe.error) && (
        <p className="mt-6 text-xs text-muted-foreground">
          Bindings could not be read for:{" "}
          {universes
            .filter((universe) => universe.error)
            .map((universe) => `${universe.name} (${universe.error})`)
            .join(", ")}
        </p>
      )}
    </>
  );
}

function transportLabel(provider: Provider): string {
  const transport = provider.controllerConnection.transport;
  return transport.type === "provider" ? `provider:${transport.providerType}` : transport.type;
}

function BindingRow({
  universe,
  binding,
  onChanged,
}: {
  universe: UniverseBindings;
  binding: EnvironmentProviderBindingView;
  onChanged: () => void;
}) {
  const setStatus = useMutation({
    mutationFn: (status: "enabled" | "disabled") =>
      api<EnvironmentProviderBindingView>(
        "PUT",
        `/api/v1/admin/universes/${universe.lightspeedUniverseId}/environment-provider-bindings/${encodeURIComponent(binding.bindingId)}`,
        {
          providerId: binding.providerId,
          status,
          expectedRevision: binding.revision,
          ...(binding.metadata && Object.keys(binding.metadata).length > 0 ? { metadata: binding.metadata } : {}),
        },
      ),
    onSuccess: onChanged,
  });
  const remove = useMutation({
    mutationFn: () =>
      api<EnvironmentProviderBindingView>(
        "DELETE",
        `/api/v1/admin/universes/${universe.lightspeedUniverseId}/environment-provider-bindings/${encodeURIComponent(binding.bindingId)}`,
      ),
    onSuccess: onChanged,
  });
  const error = setStatus.error ?? remove.error;
  return (
    <TableRow>
      <TableTitleCell title={universe.name} subtitle={universe.lightspeedUniverseId} />
      <TableCell className="max-w-48">
        <IdText>{binding.bindingId}</IdText>
      </TableCell>
      <TableCell>
        <Badge variant={binding.status === "enabled" ? "secondary" : "outline"}>
          {binding.status}
        </Badge>
      </TableCell>
      <TableCell className="text-muted-foreground">{binding.revision}</TableCell>
      <TableActionsCell>
        <Button
          variant="ghost"
          size="sm"
          disabled={setStatus.isPending}
          onClick={() => setStatus.mutate(binding.status === "enabled" ? "disabled" : "enabled")}
        >
          {binding.status === "enabled" ? "Disable" : "Enable"}
        </Button>
        <AlertDialog>
          <AlertDialogTrigger
            render={<Button variant="ghost" size="sm" className="text-destructive" disabled={remove.isPending} />}
          >
            Delete
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete binding {binding.bindingId}?</AlertDialogTitle>
              <AlertDialogDescription>
                {universe.name} will no longer be able to provision from{" "}
                <span className="font-mono text-xs">{binding.providerId}</span>. The engine
                rejects the delete while a non-closed environment still references this
                binding; disable it instead to stop new provisioning.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                className="bg-destructive text-white hover:bg-destructive/90"
                onClick={() => remove.mutate()}
              >
                Delete binding
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
        {error && <div className="mt-1 text-xs text-destructive">{error.message}</div>}
      </TableActionsCell>
    </TableRow>
  );
}

function RegisterProviderDialog({
  open,
  onOpenChange,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}) {
  const [providerId, setProviderId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [transport, setTransport] = useState<TransportKind>("webSocket");
  const [providerType, setProviderType] = useState("");
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setProviderId("");
    setDisplayName("");
    setEndpoint("");
    setTransport("webSocket");
    setProviderType("");
    setError(null);
  };

  const save = useMutation({
    mutationFn: () =>
      api<Provider>("PUT", `/api/v1/admin/environment-providers/${encodeURIComponent(providerId.trim())}`, {
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        controllerConnection: {
          endpoint: endpoint.trim(),
          transport: transport === "provider"
            ? { type: "provider", providerType: providerType.trim() }
            : { type: transport },
        },
      }),
    onSuccess: () => {
      onOpenChange(false);
      reset();
      onSaved();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!providerId.trim()) return setError("a provider id is required");
    if (!endpoint.trim()) return setError("a controller endpoint is required");
    if (transport === "provider" && !providerType.trim()) {
      return setError("a provider type is required for in-process providers");
    }
    save.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={(next) => { onOpenChange(next); if (!next) reset(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Register environment provider</DialogTitle>
          <DialogDescription>
            Records how Lightspeed reaches the provider's controller endpoint. The
            provider never self-registers; the deployment network is the trust
            boundary. Universes get access through bindings.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="provider-id">Provider id</FieldLabel>
            <Input
              id="provider-id"
              value={providerId}
              onChange={(e) => setProviderId(e.target.value)}
              placeholder="incus-hz02"
              autoFocus
            />
            <FieldDescription className="text-xs">
              Stable deployment-wide identifier; profiles reference providers by this id.
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="provider-name">Display name (optional)</FieldLabel>
            <Input
              id="provider-name"
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder="Incus on hz02"
            />
          </Field>
          <Field>
            <FieldLabel>Transport</FieldLabel>
            <Select value={transport} onValueChange={(next) => setTransport(next as TransportKind)}>
              <SelectTrigger className="w-full">
                <SelectValue>{(value: string) => transportName(value as TransportKind)}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="webSocket">{transportName("webSocket")}</SelectItem>
                <SelectItem value="http">{transportName("http")}</SelectItem>
                <SelectItem value="provider">{transportName("provider")}</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          {transport === "provider" && (
            <Field>
              <FieldLabel htmlFor="provider-type">Provider type</FieldLabel>
              <Input
                id="provider-type"
                value={providerType}
                onChange={(e) => setProviderType(e.target.value)}
                placeholder="fake"
              />
            </Field>
          )}
          <Field>
            <FieldLabel htmlFor="provider-endpoint">Controller endpoint</FieldLabel>
            <Input
              id="provider-endpoint"
              value={endpoint}
              onChange={(e) => setEndpoint(e.target.value)}
              placeholder={transport === "provider" ? "in-process" : "ws://provider.internal:19090"}
            />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending}>
              {save.isPending ? "Saving…" : "Register"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function transportName(kind: TransportKind): string {
  switch (kind) {
    case "webSocket":
      return "WebSocket";
    case "http":
      return "HTTP";
    case "provider":
      return "In-process provider";
  }
}

function BindUniverseDialog({
  provider,
  universes,
  onOpenChange,
  onSaved,
}: {
  provider: Provider | null;
  universes: UniverseBindings[];
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}) {
  const [universeId, setUniverseId] = useState("");
  const [bindingId, setBindingId] = useState("");
  const [error, setError] = useState<string | null>(null);
  const candidates = universes.filter(
    (universe) =>
      universe.status === "active"
      && !universe.error
      && !universe.bindings.some((binding) => binding.providerId === provider?.providerId),
  );
  const selected = universes.find((universe) => universe.lightspeedUniverseId === universeId);
  const effectiveBindingId = bindingId.trim() || provider?.providerId || "";

  const reset = () => {
    setUniverseId("");
    setBindingId("");
    setError(null);
  };

  const save = useMutation({
    mutationFn: () =>
      api<EnvironmentProviderBindingView>(
        "PUT",
        `/api/v1/admin/universes/${universeId}/environment-provider-bindings/${encodeURIComponent(effectiveBindingId)}`,
        { providerId: provider!.providerId, status: "enabled" },
      ),
    onSuccess: () => {
      onOpenChange(false);
      reset();
      onSaved();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!universeId) return setError("choose a universe");
    if (!effectiveBindingId) return setError("a binding id is required");
    save.mutate();
  };

  return (
    <Dialog open={provider !== null} onOpenChange={(next) => { onOpenChange(next); if (!next) reset(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Bind a universe to {provider?.displayName ?? provider?.providerId}</DialogTitle>
          <DialogDescription>
            An enabled binding lets the universe list this provider's templates and
            provision environments from it. There is at most one binding per
            universe and provider.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel>Universe</FieldLabel>
            <Select
              value={universeId || "__none__"}
              onValueChange={(next) => setUniverseId(next === "__none__" || next == null ? "" : next)}
            >
              <SelectTrigger className="w-full">
                <SelectValue>
                  {(value: string) =>
                    value === "__none__"
                      ? "Select a universe"
                      : universes.find((universe) => universe.lightspeedUniverseId === value)?.name ?? value}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">Select a universe</SelectItem>
                {candidates.map((universe) => (
                  <SelectItem key={universe.lightspeedUniverseId} value={universe.lightspeedUniverseId}>
                    {universe.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <FieldDescription className="text-xs">
              {candidates.length === 0
                ? "Every active universe on this deployment is already bound to this provider."
                : selected
                  ? <span className="font-mono">{selected.lightspeedUniverseId}</span>
                  : "Only active universes without an existing binding are listed."}
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="binding-id">Binding id</FieldLabel>
            <Input
              id="binding-id"
              value={bindingId}
              onChange={(e) => setBindingId(e.target.value)}
              placeholder={provider?.providerId ?? ""}
            />
            <FieldDescription className="text-xs">
              Universe-scoped identifier; defaults to the provider id.
            </FieldDescription>
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending || candidates.length === 0}>
              {save.isPending ? "Binding…" : "Bind"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
