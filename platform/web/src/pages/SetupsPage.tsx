import { useActionPermissions } from "@/lib/permissions";
import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, PackageOpen, RefreshCw, Sparkles } from "lucide-react";
import type { DeploymentApiKeyView, MethodGroup } from "@lightspeed-ai/agent-client";
import { api, type UniverseSetup } from "@/api";
import { KeyPresetField, MethodGroupPicker } from "@/components/api-keys/method-group-picker";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
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
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { groupSummary, groupsFor, UNIVERSE_KEY_PRESETS } from "@/lib/method-groups";
import { useActiveUniverse } from "@/lib/universes";

/// Which key the Configurator acts with, as the install route takes it.
type KeyChoice =
  | { kind: "new"; groups: MethodGroup[] }
  | { kind: "existing"; keyPrefix: string; secret: string }
  | { kind: "current" };

const CONFIGURATION_GROUPS = UNIVERSE_KEY_PRESETS.find((preset) => preset.id === "configuration")?.groups ?? [];

export function SetupsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !permissions.can("configure_resource")) {
    return <UniverseNotFound slug={slug} />;
  }

  return <SetupList universeId={universe.id} installable={permissions.can("manage_access")} />;
}

/// Operators see the templates; installing one takes an Admin.
function SetupList({ universeId, installable }: { universeId: string; installable: boolean }) {
  const queryClient = useQueryClient();
  const setups = useQuery({
    queryKey: ["setups", universeId],
    queryFn: () => api<UniverseSetup[]>("GET", `/api/v1/universes/${universeId}/setups`),
    refetchInterval: (query) =>
      query.state.data?.some((setup) => setup.status === "installing") ? 2_000 : false,
  });
  const [choosing, setChoosing] = useState<UniverseSetup | null>(null);
  const install = useMutation({
    mutationFn: ({ setupId, key }: { setupId: string; key: KeyChoice }) =>
      api<UniverseSetup>(
        "POST",
        `/api/v1/universes/${universeId}/setups/${setupId}/install`,
        { key },
      ),
    onSuccess: () => {
      setChoosing(null);
      void queryClient.invalidateQueries({ queryKey: ["setups", universeId] });
      void queryClient.invalidateQueries({ queryKey: ["api-keys", universeId] });
    },
  });

  return (
    <>
      <PageHeader
        title="Templates"
        description="Install ready-to-use profiles and resources maintained by Lightspeed."
      />
      {setups.isLoading && <LoadingNote />}
      {setups.error && <ReadError error={setups.error} loading={!setups.data} />}
      {install.error && !choosing && <p className="mb-4 text-sm text-destructive">{install.error.message}</p>}
      <div className="grid gap-4 md:grid-cols-2">
        {(setups.data ?? []).map((setup) => (
          <SetupCard
            key={setup.id}
            setup={setup}
            installable={installable}
            pending={install.isPending && install.variables?.setupId === setup.id}
            onInstall={() => { install.reset(); setChoosing(setup); }}
          />
        ))}
      </div>
      {choosing && (
        <ConfiguratorKeyDialog
          universeId={universeId}
          setup={choosing}
          pending={install.isPending}
          error={install.error?.message ?? null}
          onCancel={() => setChoosing(null)}
          onInstall={(key) => install.mutate({ setupId: choosing.id, key })}
        />
      )}
    </>
  );
}

function SetupCard({
  setup,
  installable,
  pending,
  onInstall,
}: {
  setup: UniverseSetup;
  installable: boolean;
  pending: boolean;
  onInstall: () => void;
}) {
  const ready = setup.status === "ready";
  const upgradeAvailable = ready
    && setup.installedVersion !== undefined
    && setup.installedVersion < setup.version;
  const installing = setup.status === "installing" || pending;
  const unavailable = setup.status === "unavailable";
  const action = upgradeAvailable
    ? "Upgrade"
    : ready
      ? "Repair"
      : setup.status === "failed"
        ? "Retry"
        : "Install";

  return (
    <Card className="flex flex-col">
      <CardHeader>
        <div className="mb-2 flex items-start justify-between gap-3">
          <div className="flex size-10 items-center justify-center rounded-lg bg-primary/10 text-primary">
            <PackageOpen className="size-5" />
          </div>
          <SetupStatus setup={setup} />
        </div>
        <CardTitle>{setup.name}</CardTitle>
        <CardDescription>{setup.description}</CardDescription>
      </CardHeader>
      <CardContent className="grid flex-1 content-start gap-3">
        {ready && setup.resources?.profileId && (
          <div className="rounded-lg bg-muted p-3 text-sm">
            <div className="flex items-center gap-2 font-medium">
              <CheckCircle2 className="size-4 text-primary" />
              Ready to use
            </div>
            <p className="mt-1 text-muted-foreground">
              Profile: <code className="font-mono text-xs">{setup.resources.profileId}</code>
            </p>
            {setup.resources.keyPrefix && (
              <p className="mt-1 text-muted-foreground">
                Key: <code className="font-mono text-xs">{setup.resources.keyPrefix}</code>
                {setup.resources.keySource === "existing" ? " (brought by an admin)" : ""}
                {setup.resources.keyGroups && (
                  <span className="mt-0.5 block text-xs">
                    May call: {groupSummary("universe", setup.resources.keyGroups)}
                  </span>
                )}
              </p>
            )}
          </div>
        )}
        {setup.error && (
          <p className="rounded-lg bg-destructive/10 p-3 text-sm text-destructive">
            {setup.error}
          </p>
        )}
        {unavailable && (
          <p className="text-sm text-muted-foreground">
            The deployment has not configured a Configurator MCP URL.
          </p>
        )}
        {!installable && !unavailable && (
          <p className="text-sm text-muted-foreground">
            Needs a universe Admin: it mints a key that configures the universe.
          </p>
        )}
      </CardContent>
      <CardFooter className="justify-between gap-3">
        <span className="text-xs text-muted-foreground">
          Setup version {setup.version}
          {upgradeAvailable ? ` · installed ${setup.installedVersion}` : ""}
        </span>
        <Button
          onClick={onInstall}
          disabled={installing || unavailable || !installable}
          variant={ready && !upgradeAvailable ? "outline" : "default"}
        >
          {installing ? (
            <RefreshCw className="animate-spin" data-icon="inline-start" />
          ) : ready ? (
            <RefreshCw data-icon="inline-start" />
          ) : (
            <Sparkles data-icon="inline-start" />
          )}
          {installing ? "Installing…" : action}
        </Button>
      </CardFooter>
    </Card>
  );
}

function SetupStatus({ setup }: { setup: UniverseSetup }) {
  switch (setup.status) {
    case "ready":
      return <Badge variant="secondary">installed</Badge>;
    case "installing":
      return <Badge variant="outline">installing</Badge>;
    case "failed":
      return <Badge variant="destructive">failed</Badge>;
    case "unavailable":
      return <Badge variant="outline">unavailable</Badge>;
    default:
      return <Badge variant="outline">available</Badge>;
  }
}

/// Picks the key the Configurator acts with: the one it has, a new key with
/// chosen groups, or an existing universe key whose secret the admin pastes,
/// since a key's secret cannot be read back. The MCP server offers only the
/// tools the key may call.
function ConfiguratorKeyDialog({
  universeId,
  setup,
  pending,
  error,
  onCancel,
  onInstall,
}: {
  universeId: string;
  setup: UniverseSetup;
  pending: boolean;
  error: string | null;
  onCancel: () => void;
  onInstall: (key: KeyChoice) => void;
}) {
  const hasKey = Boolean(setup.resources?.keyPrefix);
  const [kind, setKind] = useState<KeyChoice["kind"]>(hasKey ? "current" : "new");
  const [groups, setGroups] = useState<Set<MethodGroup>>(() => new Set(CONFIGURATION_GROUPS));
  const [keyPrefix, setKeyPrefix] = useState("");
  const [secret, setSecret] = useState("");
  const keys = useQuery({
    queryKey: ["api-keys", universeId],
    queryFn: () => api<DeploymentApiKeyView[]>("GET", `/api/v1/universes/${universeId}/api-keys`),
    enabled: kind === "existing",
  });
  const activeKeys = (keys.data ?? []).filter((key) => key.revokedAtMs == null);
  const chosen = activeKeys.find((key) => key.keyPrefix === keyPrefix);
  const ready = kind === "current"
    || (kind === "new" && groups.size > 0)
    || (kind === "existing" && chosen !== undefined && secret.trim().startsWith("lsk_"));

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!ready) return;
    onInstall(kind === "new"
      ? { kind, groups: groupsFor("universe").filter((group) => groups.has(group)) }
      : kind === "existing"
        ? { kind, keyPrefix, secret: secret.trim() }
        : { kind });
  };
  const options: { kind: KeyChoice["kind"]; label: string }[] = [
    ...(hasKey ? [{ kind: "current" as const, label: "Keep current key" }] : []),
    { kind: "new", label: "Create a new key" },
    { kind: "existing", label: "Use an existing key" },
  ];

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onCancel(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Configurator key</DialogTitle>
          <DialogDescription>
            The Configurator MCP server acts with this key and offers only the tools it may call.
            Anyone who can attach the server acts with it.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <div className="flex flex-wrap gap-2" role="group" aria-label="Key">
            {options.map((option) => (
              <Button
                key={option.kind}
                type="button"
                size="sm"
                variant={kind === option.kind ? "secondary" : "outline"}
                aria-pressed={kind === option.kind}
                onClick={() => setKind(option.kind)}
              >
                {option.label}
              </Button>
            ))}
          </div>
          {kind === "current" && setup.resources?.keyPrefix && (
            <p className="text-sm text-muted-foreground">
              Keeps <code className="font-mono text-xs">{setup.resources.keyPrefix}</code>
              {setup.resources.keyGroups ? `, which may call: ${groupSummary("universe", setup.resources.keyGroups)}.` : "."}
            </p>
          )}
          {kind === "new" && (
            <>
              <KeyPresetField groups={groups} onChange={setGroups} />
              <MethodGroupPicker idPrefix="configurator-key" allowed={groupsFor("universe")} chosen={groups} onChange={setGroups} />
              {hasKey && setup.resources?.keySource !== "existing" && (
                <p className="text-xs text-muted-foreground">The Configurator's current key is revoked.</p>
              )}
            </>
          )}
          {kind === "existing" && (
            <>
              <Field>
                <FieldLabel htmlFor="configurator-existing-key">Key</FieldLabel>
                <Select value={keyPrefix} onValueChange={(value) => setKeyPrefix(value as string)}>
                  <SelectTrigger id="configurator-existing-key" className="w-full">
                    <SelectValue placeholder={keys.isLoading ? "Loading keys…" : "Choose a key"}>
                      {(value: string) => {
                        const key = activeKeys.find((candidate) => candidate.keyPrefix === value);
                        return key ? `${key.displayName ?? "Unnamed key"} · ${key.keyPrefix}` : value;
                      }}
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {activeKeys.map((key) => (
                      <SelectItem key={key.keyPrefix} value={key.keyPrefix}>
                        {key.displayName ?? "Unnamed key"} · {key.keyPrefix}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <FieldDescription>
                  {chosen
                    ? `May call: ${groupSummary("universe", chosen.groups)}`
                    : "Only this universe's active keys."}
                </FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="configurator-existing-secret">Secret</FieldLabel>
                <Input
                  id="configurator-existing-secret"
                  type="password"
                  autoComplete="off"
                  value={secret}
                  onChange={(event) => setSecret(event.target.value)}
                  placeholder="lsk_…"
                />
                <FieldDescription>
                  Lightspeed keeps only a hash of each key, so paste the secret shown when the key was
                  created. It is checked against the chosen key and stored encrypted. Replacing this key
                  later never revokes it.
                </FieldDescription>
              </Field>
            </>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onCancel}>Cancel</Button>
            <Button type="submit" disabled={pending || !ready}>
              {pending ? "Installing…" : "Install"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
