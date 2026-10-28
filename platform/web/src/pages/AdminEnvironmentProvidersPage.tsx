import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";
import type {
  EnvironmentProviderBindingView,
  DeploymentEnvironmentProviderView,
} from "@lightspeed-ai/sdk";
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


/// Deployment-scoped environment provider administration: physical
/// providers are registered once by a platform admin and shared with
/// universes through bindings, one per universe and provider. Universe
/// owners consume enabled bindings from their Environments page; only admins
/// mutate them here.
export function AdminEnvironmentProvidersPage() {
  const queryClient = useQueryClient();
  const [registerOpen, setRegisterOpen] = useState(false);
  const [editing, setEditing] = useState<Provider | null>(null);
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
      <ProviderDialog
        key={editing?.providerId ?? (registerOpen ? "new" : "closed")}
        provider={editing}
        open={registerOpen || editing !== null}
        onOpenChange={(open) => {
          if (!open) {
            setRegisterOpen(false);
            setEditing(null);
          }
        }}
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
                    <Badge variant="outline">
                      {provider.hasCredential ? "credential" : "no credential"}
                    </Badge>
                  </div>
                  <p className="mt-1 truncate font-mono text-xs text-muted-foreground">
                    {provider.controllerEndpoint}
                  </p>
                  <p className="mt-1 truncate text-xs text-muted-foreground">
                    Data routes{" "}
                    <span className="font-mono">
                      {provider.effectiveDataRouteBase ?? "cannot be derived"}
                    </span>
                    {provider.dataRouteBase ? "" : " (derived)"}
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <Button variant="outline" size="sm" onClick={() => setEditing(provider)}>
                    <Pencil data-icon="inline-start" />
                    Edit
                  </Button>
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
                          refuses the delete while any universe binding or any
                          non-closed environment of this provider exists; delete
                          those bindings and close those environments first.
                          Provider-side machines are not touched.
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
                          <TableHead>Enabled</TableHead>
                          <TableHead className="w-0" />
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {providerBindings.map(({ universe, binding }) => (
                          <BindingRow
                            key={`${universe.universeId}:${binding.providerId}`}
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

function BindingRow({
  universe,
  binding,
  onChanged,
}: {
  universe: UniverseBindings;
  binding: EnvironmentProviderBindingView;
  onChanged: () => void;
}) {
  const path = `/api/v1/admin/universes/${universe.lightspeedUniverseId}/environment-provider-bindings/${encodeURIComponent(binding.providerId)}`;
  const setEnabled = useMutation({
    mutationFn: (enabled: boolean) =>
      api<EnvironmentProviderBindingView>("PUT", path, { enabled }),
    onSuccess: onChanged,
  });
  const remove = useMutation({
    mutationFn: () => api<EnvironmentProviderBindingView>("DELETE", path),
    onSuccess: onChanged,
  });
  const error = setEnabled.error ?? remove.error;
  return (
    <TableRow>
      <TableTitleCell title={universe.name} subtitle={universe.lightspeedUniverseId} />
      <TableCell>
        <Switch
          aria-label={`Enable ${binding.providerId} for ${universe.name}`}
          checked={binding.enabled}
          disabled={setEnabled.isPending}
          onCheckedChange={(enabled) => setEnabled.mutate(enabled)}
        />
      </TableCell>
      <TableActionsCell>
        <AlertDialog>
          <AlertDialogTrigger
            render={<Button variant="ghost" size="sm" className="text-destructive" disabled={remove.isPending} />}
          >
            Delete
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Unbind {universe.name}?</AlertDialogTitle>
              <AlertDialogDescription>
                {universe.name} will no longer be able to provision from{" "}
                <span className="font-mono text-xs">{binding.providerId}</span>. The engine
                rejects the delete while a non-closed environment of this provider is still
                open in the universe; disable the binding instead to stop new provisioning.
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

/// Register a provider, or edit one when `provider` is set. The credential
/// is write-only and optional: empty keeps the stored one.
function ProviderDialog({
  provider,
  open,
  onOpenChange,
  onSaved,
}: {
  provider: Provider | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
}) {
  const [providerId, setProviderId] = useState(provider?.providerId ?? "");
  const [displayName, setDisplayName] = useState(provider?.displayName ?? "");
  const [endpoint, setEndpoint] = useState(provider?.controllerEndpoint ?? "");
  const [dataRouteBase, setDataRouteBase] = useState(provider?.dataRouteBase ?? "");
  const [credential, setCredential] = useState("");
  const [clearCredential, setClearCredential] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const editing = provider !== null;

  const save = useMutation({
    mutationFn: () =>
      api<Provider>("PUT", `/api/v1/admin/environment-providers/${encodeURIComponent(providerId.trim())}`, {
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        controllerEndpoint: endpoint.trim(),
        ...(dataRouteBase.trim() ? { dataRouteBase: dataRouteBase.trim() } : {}),
        ...(credential ? { credential } : {}),
        ...(clearCredential ? { clearCredential: true } : {}),
      }),
    onSuccess: () => {
      onOpenChange(false);
      onSaved();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!providerId.trim()) return setError("a provider id is required");
    if (!endpoint.trim()) return setError("a controller endpoint is required");
    if (!/^(wss?:\/\/|unix:\/\/\/)\S+$/.test(endpoint.trim())) {
      return setError("the controller endpoint must be a ws://, wss:// or unix:// URL");
    }
    if (dataRouteBase.trim() && !/^wss?:\/\/\S+$/.test(dataRouteBase.trim())) {
      return setError("the data route base must be a ws:// or wss:// URL");
    }
    if (endpoint.trim().startsWith("unix://") && !dataRouteBase.trim()) {
      return setError("a unix:// controller needs a data route base");
    }
    save.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {editing ? `Edit ${provider.displayName ?? provider.providerId}` : "Register environment provider"}
          </DialogTitle>
          <DialogDescription>
            Records how Lightspeed reaches the provider's controller and data routes, and the
            optional bearer credential it presents there. The provider never self-registers.
            Universes get access through bindings.
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
              disabled={editing}
              autoFocus={!editing}
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
            <FieldLabel htmlFor="provider-endpoint">Controller endpoint</FieldLabel>
            <Input
              id="provider-endpoint"
              value={endpoint}
              onChange={(e) => setEndpoint(e.target.value)}
              placeholder="wss://provider.internal:19090/control"
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="provider-data-route-base">Data route base (optional)</FieldLabel>
            <Input
              id="provider-data-route-base"
              value={dataRouteBase}
              onChange={(e) => setDataRouteBase(e.target.value)}
              placeholder={derivedDataRouteBase(endpoint) ?? "wss://provider.internal:19090/routes"}
            />
            <FieldDescription className="text-xs">
              {editing && provider.effectiveDataRouteBase
                ? <>Lightspeed dials <span className="font-mono">{provider.effectiveDataRouteBase}</span>. </>
                : null}
              Empty derives it from the controller endpoint's origin plus{" "}
              <span className="font-mono">/routes</span>; a unix:// controller needs one.
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="provider-credential">Credential (optional)</FieldLabel>
            <Input
              id="provider-credential"
              type="password"
              autoComplete="off"
              value={credential}
              disabled={clearCredential}
              onChange={(e) => setCredential(e.target.value)}
              placeholder={editing && provider.hasCredential ? "Unchanged" : "None"}
            />
            <FieldDescription className="text-xs">
              The bearer secret the provider is configured with. Write-only: it is never shown
              again. Without one, Lightspeed sends no Authorization header.
            </FieldDescription>
            {editing && provider.hasCredential && (
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={clearCredential}
                  onChange={(event) => {
                    setClearCredential(event.target.checked);
                    if (event.target.checked) setCredential("");
                  }}
                />
                Remove the stored credential
              </label>
            )}
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending}>
              {save.isPending ? "Saving…" : editing ? "Save" : "Register"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/// The base Lightspeed derives from a ws:// or wss:// controller endpoint.
function derivedDataRouteBase(endpoint: string): string | null {
  const match = /^(wss?:\/\/[^/\s]+)/.exec(endpoint.trim());
  return match ? `${match[1]}/routes` : null;
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
  const [error, setError] = useState<string | null>(null);
  const candidates = universes.filter(
    (universe) =>
      universe.status === "active"
      && !universe.error
      && !universe.bindings.some((binding) => binding.providerId === provider?.providerId),
  );
  const selected = universes.find((universe) => universe.lightspeedUniverseId === universeId);

  const reset = () => {
    setUniverseId("");
    setError(null);
  };

  const save = useMutation({
    mutationFn: () =>
      api<EnvironmentProviderBindingView>(
        "PUT",
        `/api/v1/admin/universes/${universeId}/environment-provider-bindings/${encodeURIComponent(provider!.providerId)}`,
        { enabled: true },
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
