import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, ChevronDown, Copy, KeyRound, Plus, Trash2 } from "lucide-react";
import {
  api,
  type Environment,
  type EnvironmentCredential,
  type EnvironmentCredentialSource,
  type EnvironmentProviderBinding,
  type EnvironmentRegistrationKey,
  type EnvironmentRegistrationKeyCreated,
  type EnvironmentTemplate,
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { EnvironmentIdlePolicyDialog } from "@/components/environment/idle-policy-dialog";
import {
  IdlePolicyFields,
  idlePolicyIsMonotone,
  type IdlePolicy,
} from "@/components/environment/idle-policy-fields";
import {
  EnvironmentPowerControls,
  describeIdlePolicy,
  observedPower,
  powerDiverges,
} from "@/components/environment/power-controls";
import {
  environmentCredentialAvailable,
  environmentCredentialOptions,
  environmentCredentialSourceFromValue,
  environmentCredentialSourceLabel,
  environmentCredentialSourceValue,
} from "@/lib/environment-credentials";
import { canManage, useActiveUniverse } from "@/lib/universes";

/// Universe environments are provisioned through operator-enabled bindings and
/// immutable provider templates. Physical provider registration stays admin-only.
export function EnvironmentsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return <UniverseNotFound slug={slug} />;
  }

  return <ProviderList universeId={universe.id} />;
}

const REFRESH_MS = 10_000;

function ProviderList({ universeId }: { universeId: string }) {
  const bindings = useQuery({
    queryKey: ["environment-provider-bindings", universeId],
    queryFn: () =>
      api<EnvironmentProviderBinding[]>(
        "GET",
        `/api/v1/universes/${universeId}/environment-provider-bindings`,
      ),
  });
  const templates = useQuery({
    queryKey: ["environment-templates", universeId],
    queryFn: () =>
      api<EnvironmentTemplate[]>(
        "GET",
        `/api/v1/universes/${universeId}/environment-templates`,
      ),
  });
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () =>
      api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    refetchInterval: (query) => {
      const rows = query.state.data as Environment[] | undefined;
      return rows?.some((environment) =>
        ["provisioning", "booting", "closing", "unknown"].includes(environment.status)
          || powerDiverges(environment)
      ) ? REFRESH_MS : false;
    },
  });
  const secrets = useQuery({
    queryKey: ["secrets", universeId],
    queryFn: () =>
      api<SecretsInventory>("GET", `/api/v1/universes/${universeId}/secrets`),
  });
  const hints = useQuery({
    queryKey: ["environment-hints", universeId],
    queryFn: () =>
      api<{ devEnvdEndpoint: string | null }>(
        "GET",
        `/api/v1/universes/${universeId}/environments/hints`,
      ),
    staleTime: 300_000,
  });
  const registrationKeys = useQuery({
    queryKey: ["environment-registration-keys", universeId],
    queryFn: () =>
      api<EnvironmentRegistrationKey[]>(
        "GET",
        `/api/v1/universes/${universeId}/environment-registration-keys`,
      ),
  });

  const bindingRows = (bindings.data ?? [])
    .slice()
    .sort((a, b) => a.providerId.localeCompare(b.providerId));
  const templateRows = (templates.data ?? [])
    .slice()
    .sort((a, b) => a.displayName.localeCompare(b.displayName));
  const environmentRows = (environments.data ?? [])
    .slice()
    .sort((a, b) => environmentName(a).localeCompare(environmentName(b)));
  const enabledBindings = new Set(
    bindingRows.filter((binding) => binding.status === "enabled").map((binding) => binding.bindingId),
  );
  const creatableTemplates = templateRows.filter(
    (template) => enabledBindings.has(template.bindingId) && !template.deprecated,
  );
  const keyRows = (registrationKeys.data ?? [])
    .slice()
    .sort((a, b) => a.displayName.localeCompare(b.displayName));
  const keysById = new Map(keyRows.map((key) => [key.registrationKeyId, key]));
  // Registered environments are grouped under the key that admitted them;
  // provisioned and external ones keep their flat list.
  const groupedRows = keyRows
    .map((key) => ({
      key,
      environments: environmentRows.filter((environment) =>
        environment.source.type === "registered"
        && environment.source.registrationKeyId === key.registrationKeyId),
    }))
    .filter((group) => group.environments.length > 0);
  const orphanRegistered = environmentRows.filter((environment) =>
    environment.source.type === "registered"
    && !keysById.has(environment.source.registrationKeyId));
  const ungroupedRows = environmentRows.filter((environment) => environment.source.type !== "registered");

  return (
    <>
      <PageHeader
        title="Environments"
        description="Computers and sandboxes agents can use in this universe."
        actions={
          <div className="flex items-center gap-2">
            <RegisterExternalEnvironmentDialog
              universeId={universeId}
              suggestedEndpoint={hints.data?.devEnvdEndpoint ?? null}
              alreadyRegistered={(environments.data ?? []).some(
                (environment) =>
                  environment.source.type === "external"
                  && hints.data?.devEnvdEndpoint
                  && environment.source.connection.endpoint === hints.data.devEnvdEndpoint,
              )}
            />
            {creatableTemplates.length > 0 && (
              <CreateEnvironmentDialog universeId={universeId} templates={creatableTemplates} />
            )}
            <CreateRegistrationKeyDialog universeId={universeId} />
          </div>
        }
      />
      {(bindings.isLoading || templates.isLoading || environments.isLoading || secrets.isLoading) && <LoadingNote />}
      {bindings.error && (
        <p className="text-sm text-destructive">
          Provider bindings unavailable: {bindings.error.message}
        </p>
      )}
      {templates.error && (
        <p className="text-sm text-destructive">
          Environment templates unavailable: {templates.error.message}
        </p>
      )}
      {environments.error && (
        <p className="text-sm text-destructive">
          Environments unavailable: {environments.error.message}
        </p>
      )}
      {secrets.error && (
        <p className="text-sm text-destructive">
          Access credentials unavailable: {secrets.error.message}
        </p>
      )}
      {registrationKeys.error && (
        <p className="text-sm text-destructive">
          Registration keys unavailable: {registrationKeys.error.message}
        </p>
      )}
      {bindings.data && environments.data && environmentRows.length === 0 && (
        <div className="rounded-xl border border-dashed px-5 py-8 text-center">
          <p className="text-sm font-medium">No environments yet</p>
          <p className="mt-1 text-sm text-muted-foreground">
            Create one from an enabled provider template, or mint a registration key and start
            <span className="font-mono"> lightspeed-envd</span> on a machine of your own.
          </p>
        </div>
      )}
      {groupedRows.map((group) => (
        <section key={group.key.registrationKeyId} className="grid gap-3">
          <div className="flex flex-wrap items-center gap-2 pt-2">
            <KeyRound className="size-4 text-muted-foreground" />
            <h2 className="text-sm font-semibold">{group.key.displayName}</h2>
            <Badge variant="outline">{group.key.identityMode}</Badge>
            <span className="text-xs text-muted-foreground">
              {group.environments.length} registered
              {group.key.maxActiveEnvironments ? ` · limit ${group.key.maxActiveEnvironments}` : ""}
            </span>
          </div>
          {group.environments.map((environment) => (
            <EnvironmentCard
              key={environment.environmentId}
              universeId={universeId}
              environment={environment}
              template={undefined}
              registrationKey={group.key}
              secrets={secrets.data}
            />
          ))}
        </section>
      ))}
      <div className="grid gap-3">
        {[...ungroupedRows, ...orphanRegistered].map((environment) => (
          <EnvironmentCard
            key={environment.environmentId}
            universeId={universeId}
            environment={environment}
            template={templateRows.find((candidate) =>
              candidate.bindingId === provisionedBindingId(environment)
              && candidate.templateId === environment.incarnation.templateId
            )}
            registrationKey={undefined}
            secrets={secrets.data}
          />
        ))}
      </div>
      {keyRows.length > 0 && <RegistrationKeys universeId={universeId} keys={keyRows} />}
      {bindingRows.length > 0 && <ProviderBindings bindings={bindingRows} />}
    </>
  );
}

function EnvironmentCard({
  universeId,
  environment,
  template,
  registrationKey,
  secrets,
}: {
  universeId: string;
  environment: Environment;
  template: EnvironmentTemplate | undefined;
  registrationKey: EnvironmentRegistrationKey | undefined;
  secrets: SecretsInventory | undefined;
}) {
  const [open, setOpen] = useState(false);
  const [policyOpen, setPolicyOpen] = useState(false);
  const source = environment.source;
  const gone = ["closing", "closed", "failed"].includes(environment.status);
  const registered = source.type === "registered";

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <section className="rounded-xl border">
        <div className="flex flex-wrap items-center gap-4 px-4 py-4 md:px-5">
          <div className="min-w-0 flex-1">
            <h2 className="truncate text-sm font-semibold">
              {environmentName(environment)}
            </h2>
            <p className="mt-1 truncate text-sm text-muted-foreground">
              {source.type === "provisioned"
                ? template?.displayName ?? environment.incarnation.templateId ?? "Provisioned environment"
                : source.type === "registered"
                  ? `Registered daemon · ${registrationKey?.displayName ?? source.registrationKeyId}`
                  : "External environment"}
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              {registered
                ? environment.lastSeenAtMs
                  ? `Last seen ${relativeTime(environment.lastSeenAtMs)}`
                  : "Never seen"
                : environment.publicEndpoint ?? "Private environment"}
            </p>
          </div>
          <EnvironmentStatusBadge environment={environment} />
          <CollapsibleTrigger className="inline-flex h-8 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50">
            Details
            <ChevronDown className={`size-3.5 transition-transform ${open ? "rotate-180" : ""}`} />
          </CollapsibleTrigger>
        </div>
        <CollapsibleContent>
          <div className="border-t bg-muted/15 px-4 py-4 md:px-5">
            <dl className="grid gap-x-8 gap-y-4 sm:grid-cols-2 lg:grid-cols-3">
              <Detail label="Environment ID" value={environment.environmentId} mono />
              <Detail
                label="Source"
                value={source.type === "provisioned" ? "Provisioned" : source.type === "registered" ? "Registered" : "External"}
              />
              {source.type === "registered" && (
                <Detail label="Registration key" value={registrationKey?.displayName ?? source.registrationKeyId} />
              )}
              {source.type === "registered" && <Detail label="Daemon" value={source.daemonId} mono />}
              {source.type === "registered" && (
                <Detail
                  label="Identity"
                  value={source.identityMode === "ephemeral"
                    ? "Ephemeral · closes after its key's disconnect grace"
                    : "Persistent · stays offline until closed"}
                />
              )}
              {source.type === "registered" && environment.lastSeenAtMs && (
                <Detail label="Last seen" value={relativeTime(environment.lastSeenAtMs)} />
              )}
              {source.type === "provisioned" && <Detail label="Binding" value={source.bindingId} mono />}
              {source.type === "provisioned" && <Detail label="Provider" value={source.providerId} mono />}
              {environment.incarnation.templateId && <Detail label="Template" value={environment.incarnation.templateId} mono />}
              {environment.incarnation.providerTargetId && <Detail label="Target" value={environment.incarnation.providerTargetId} mono />}

              {source.type === "provisioned" && (
                <Detail
                  label="Power"
                  value={powerDiverges(environment)
                    ? `${observedPower(environment) ?? environment.status} → ${environment.desiredPower}`
                    : environment.desiredPower}
                />
              )}
              {source.type === "provisioned" && (
                <Detail label="Idle policy" value={describeIdlePolicy(environment.idlePolicy)} />
              )}
              <Detail label="Updated" value={relativeTime(environment.updatedAtMs)} />
            </dl>
            <EnvironmentCredentials
              universeId={universeId}
              environment={environment}
              secrets={secrets}
              enabled={open}
            />
            {source.type === "provisioned" && (
              <div className="mt-4 flex flex-wrap items-center gap-2 border-t pt-4">
                <EnvironmentPowerControls universeId={universeId} environment={environment} />
                {!gone && (
                  <Button variant="outline" size="xs" onClick={() => setPolicyOpen(true)}>
                    Idle policy…
                  </Button>
                )}
                {template?.publicIngress && (
                  <EnvironmentIngressButton universeId={universeId} environment={environment} />
                )}
                <CloseEnvironmentButton universeId={universeId} environment={environment} />
              </div>
            )}
            {registered && !gone && (
              <div className="mt-4 flex flex-wrap items-center gap-2 border-t pt-4">
                <CloseEnvironmentButton universeId={universeId} environment={environment} />
                <span className="text-xs text-muted-foreground">
                  Closing spends the daemon identity; the machine itself is untouched.
                </span>
              </div>
            )}
            {source.type === "provisioned" && !gone && (
              <EnvironmentIdlePolicyDialog
                key={`${environment.environmentId}:${policyOpen ? "open" : "closed"}`}
                universeId={universeId}
                environment={environment}
                open={policyOpen}
                onOpenChange={setPolicyOpen}
              />
            )}
          </div>
        </CollapsibleContent>
      </section>
    </Collapsible>
  );
}

function EnvironmentCredentials({
  universeId,
  environment,
  secrets,
  enabled,
}: {
  universeId: string;
  environment: Environment;
  secrets: SecretsInventory | undefined;
  enabled: boolean;
}) {
  const queryClient = useQueryClient();
  const credentials = useQuery({
    queryKey: ["environment-credentials", universeId, environment.environmentId],
    queryFn: () =>
      api<EnvironmentCredential[]>(
        "GET",
        `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}/credentials`,
      ),
    enabled,
  });
  const unbind = useMutation({
    mutationFn: (envName: string) =>
      api<EnvironmentCredential>(
        "DELETE",
        `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}/credentials/${encodeURIComponent(envName)}`,
      ),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: ["environment-credentials", universeId, environment.environmentId],
      }),
  });
  const rows = (credentials.data ?? []).slice().sort((a, b) => a.envName.localeCompare(b.envName));
  const canAssign = environment.status !== "closed" && environment.status !== "closing";

  return (
    <div className="mt-4 border-t pt-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 className="text-sm font-medium">Secret environment variables</h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Resolved when a process or job starts. Secret values are never shown here.
          </p>
        </div>
        <AssignCredentialDialog
          universeId={universeId}
          environmentId={environment.environmentId}
          secrets={secrets}
          disabled={!canAssign}
        />
      </div>
      {credentials.isLoading && <p className="mt-3 text-xs text-muted-foreground">Loading…</p>}
      {credentials.error && (
        <p className="mt-3 text-sm text-destructive">{credentials.error.message}</p>
      )}
      {unbind.error && <p className="mt-3 text-sm text-destructive">{unbind.error.message}</p>}
      {secrets && environmentCredentialOptions(secrets).length === 0 && (
        <p className="mt-3 text-xs text-muted-foreground">
          Add an environment secret, active access credential, or model provider API key on the
          Secrets page first.
        </p>
      )}
      {credentials.data && rows.length === 0 && (
        <p className="mt-3 rounded-lg border border-dashed px-3 py-4 text-sm text-muted-foreground">
          No secret environment variables assigned.
        </p>
      )}
      {rows.length > 0 && (
        <div className="mt-3 divide-y rounded-lg border">
          {rows.map((credential) => {
            const available = environmentCredentialAvailable(credential.source, secrets);
            return (
              <div
                key={credential.envName}
                className="flex flex-wrap items-center gap-3 px-3 py-2.5"
              >
                <code className="min-w-0 flex-1 break-all text-xs font-medium">
                  {credential.envName}
                </code>
                <span className="max-w-full truncate text-xs text-muted-foreground">
                  {environmentCredentialSourceLabel(credential.source, secrets)}
                </span>
                {!available && (
                  <Badge variant="outline" className="border-destructive/50 text-destructive">
                    unavailable
                  </Badge>
                )}
                <AlertDialog>
                  <AlertDialogTrigger
                    render={
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="text-destructive"
                        aria-label={`Unassign ${credential.envName}`}
                      />
                    }
                  >
                    <Trash2 />
                  </AlertDialogTrigger>
                  <AlertDialogContent>
                    <AlertDialogHeader>
                      <AlertDialogTitle>Unassign this environment variable?</AlertDialogTitle>
                      <AlertDialogDescription>
                        New processes and jobs in this environment will no longer receive{" "}
                        <span className="font-mono text-xs">{credential.envName}</span>. The stored
                        access credential itself will not be deleted.
                      </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                      <AlertDialogCancel>Cancel</AlertDialogCancel>
                      <AlertDialogAction
                        className="bg-destructive text-white hover:bg-destructive/90"
                        onClick={() => unbind.mutate(credential.envName)}
                      >
                        Unassign variable
                      </AlertDialogAction>
                    </AlertDialogFooter>
                  </AlertDialogContent>
                </AlertDialog>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function AssignCredentialDialog({
  universeId,
  environmentId,
  secrets,
  disabled,
}: {
  universeId: string;
  environmentId: string;
  secrets: SecretsInventory | undefined;
  disabled: boolean;
}) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [envName, setEnvName] = useState("");
  const [sourceValue, setSourceValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const sources = environmentCredentialOptions(secrets);
  const selectedSource = sourceValue || sources[0]?.value || "";

  const bind = useMutation({
    mutationFn: () => {
      const source = environmentCredentialSourceFromValue(selectedSource);
      return api<EnvironmentCredential>(
        "POST",
        `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environmentId)}/credentials`,
        { envName: envName.trim(), source },
      );
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: ["environment-credentials", universeId, environmentId],
      });
      setOpen(false);
      setEnvName("");
      setSourceValue("");
      setError(null);
    },
    onError: (cause) => setError(cause.message),
  });

  const submit = () => {
    if (!/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(envName.trim())) {
      setError("Use a valid environment variable name, such as GITHUB_TOKEN.");
      return;
    }
    if (!selectedSource) {
      setError("Choose an access credential.");
      return;
    }
    bind.mutate();
  };

  return (
    <>
      <Button
        variant="outline"
        size="xs"
        disabled={disabled || sources.length === 0}
        onClick={() => {
          setSourceValue(selectedSource);
          setOpen(true);
        }}
      >
        <Plus data-icon="inline-start" />
        Assign credential
      </Button>
      <Dialog
        open={open}
        onOpenChange={(nextOpen) => {
          setOpen(nextOpen);
          if (!nextOpen) setError(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Assign secret</DialogTitle>
            <DialogDescription>
              Map a stored secret to an environment variable. Assigning an existing variable
              name replaces its current source.
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-4">
            <Field>
              <FieldLabel htmlFor="credential-env-name">Environment variable</FieldLabel>
              <input
                id="credential-env-name"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 font-mono text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
                value={envName}
                onChange={(event) => {
                  setEnvName(event.target.value);
                  setError(null);
                }}
                placeholder="GITHUB_TOKEN"
                autoFocus
                spellCheck={false}
              />
              <FieldDescription>
                If a process explicitly sets the same variable, Lightspeed rejects the request
                instead of choosing one value silently.
              </FieldDescription>
            </Field>
            <Field>
              <FieldLabel>Secret source</FieldLabel>
              <Select
                value={selectedSource}
                onValueChange={(value) => {
                  setSourceValue(value as string);
                  setError(null);
                  const suggested = sources.find((s) => s.value === value)?.suggestedEnvName;
                  if (suggested && !envName.trim()) setEnvName(suggested);
                }}
              >
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {sources.map((source) => (
                    <SelectItem key={source.value} value={source.value}>
                      {source.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <FieldDescription>
                Environment secrets and access grants are resolved at runtime; provider API keys
                use their stored value directly.
              </FieldDescription>
            </Field>
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>Cancel</Button>
            <Button disabled={bind.isPending} onClick={submit}>
              {bind.isPending ? "Assigning…" : "Assign"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {!secrets && (
        <span className="sr-only">Access credentials are still loading</span>
      )}
    </>
  );
}

function EnvironmentStatusBadge({ environment }: { environment: Environment }) {
  if (environment.status === "ready") {
    return <Badge variant="secondary">ready</Badge>;
  }
  if (environment.status === "paused" || environment.status === "suspended") {
    return (
      <Badge variant="outline">
        {environment.status}
        {powerDiverges(environment) ? " · waking" : ""}
      </Badge>
    );
  }
  if (environment.status === "failed") {
    return (
      <Badge variant="outline" className="border-destructive/50 text-destructive">
        failed
      </Badge>
    );
  }
  return <Badge variant="outline">{environment.status}</Badge>;
}

function Detail({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className={`mt-1 break-all text-sm ${mono ? "font-mono text-xs" : ""}`}>{value}</dd>
    </div>
  );
}

/// Every registration key of the universe: the policy each pool of
/// registered daemons runs under, and how many environments it admitted.
function RegistrationKeys({
  universeId,
  keys,
}: {
  universeId: string;
  keys: EnvironmentRegistrationKey[];
}) {
  return (
    <section className="mt-6">
      <h2 className="text-sm font-semibold">Registration keys</h2>
      <p className="mt-1 text-sm text-muted-foreground">
        A key lets <span className="font-mono">lightspeed-envd</span> daemons dial in and register as
        environments; each key is the group its environments belong to. Revoking stops new daemons
        while registered ones keep reconnecting.
      </p>
      <div className="mt-3 grid gap-2">
        {keys.map((key) => (
          <div key={key.registrationKeyId} className="flex flex-wrap items-center gap-3 rounded-xl border px-4 py-3">
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium">{key.displayName}</p>
              <p className="mt-0.5 font-mono text-xs text-muted-foreground">
                {key.keyPrefix}… · {key.registrationKeyId}
              </p>
            </div>
            <Badge variant="outline">{key.identityMode}</Badge>
            <span className="text-xs text-muted-foreground">
              {key.activeEnvironmentCount} active / {key.registeredEnvironmentCount} total
              {key.maxActiveEnvironments ? ` · limit ${key.maxActiveEnvironments}` : ""}
              {key.identityMode === "ephemeral"
                ? ` · grace ${Math.round(key.ephemeralDisconnectGraceMs / 60_000)} min`
                : ""}
              {key.expiresAtMs ? ` · expires ${relativeTime(key.expiresAtMs).replace(" ago", "")}` : ""}
            </span>
            {key.status === "active" ? (
              <Badge variant="secondary">active</Badge>
            ) : (
              <Badge variant="outline">{key.status}</Badge>
            )}
            {key.status === "active" && (
              <RevokeRegistrationKeyButton universeId={universeId} registrationKey={key} />
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

function RevokeRegistrationKeyButton({
  universeId,
  registrationKey,
}: {
  universeId: string;
  registrationKey: EnvironmentRegistrationKey;
}) {
  const queryClient = useQueryClient();
  const [closeEnvironments, setCloseEnvironments] = useState(false);
  const revoke = useMutation({
    mutationFn: () =>
      api<{ registrationKey: EnvironmentRegistrationKey; closedEnvironmentIds?: string[] }>(
        "POST",
        `/api/v1/universes/${universeId}/environment-registration-keys/${registrationKey.registrationKeyId}/revoke`,
        { closeEnvironments },
      ),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["environment-registration-keys", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["environments", universeId] }),
      ]);
    },
  });
  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button variant="outline" size="xs" />}>
        Revoke
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Revoke “{registrationKey.displayName}”?</AlertDialogTitle>
          <AlertDialogDescription>
            New daemons presenting this key are refused from now on. Daemons that already registered
            keep reconnecting by their own identity unless you also close their environments.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={closeEnvironments}
            onChange={(event) => setCloseEnvironments(event.target.checked)}
          />
          Also close its {registrationKey.activeEnvironmentCount} open environment
          {registrationKey.activeEnvironmentCount === 1 ? "" : "s"}
        </label>
        {revoke.error && <p className="text-sm text-destructive">{revoke.error.message}</p>}
        <AlertDialogFooter>
          <AlertDialogCancel>Keep</AlertDialogCancel>
          <AlertDialogAction onClick={() => revoke.mutate()} disabled={revoke.isPending}>
            {revoke.isPending ? "Revoking…" : "Revoke key"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

/// Mint a registration key. The secret is shown once, with the two
/// variables a daemon needs to dial in.
function CreateRegistrationKeyDialog({ universeId }: { universeId: string }) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [displayName, setDisplayName] = useState("");
  const [identityMode, setIdentityMode] = useState<"persistent" | "ephemeral">("persistent");
  const [maxActive, setMaxActive] = useState("");
  const [graceMinutes, setGraceMinutes] = useState("");
  const [created, setCreated] = useState<EnvironmentRegistrationKeyCreated | null>(null);
  const [copied, setCopied] = useState(false);
  const create = useMutation({
    mutationFn: () =>
      api<EnvironmentRegistrationKeyCreated>(
        "POST",
        `/api/v1/universes/${universeId}/environment-registration-keys`,
        {
          displayName: displayName.trim(),
          identityMode,
          ...(maxActive.trim() ? { maxActiveEnvironments: Number(maxActive) } : {}),
          ...(identityMode === "ephemeral" && graceMinutes.trim()
            ? { ephemeralDisconnectGraceMs: Math.round(Number(graceMinutes) * 60_000) }
            : {}),
        },
      ),
    onSuccess: async (result) => {
      setCreated(result);
      await queryClient.invalidateQueries({ queryKey: ["environment-registration-keys", universeId] });
    },
  });
  const reset = () => {
    setOpen(false);
    setDisplayName("");
    setIdentityMode("persistent");
    setMaxActive("");
    setGraceMinutes("");
    setCreated(null);
    setCopied(false);
    create.reset();
  };
  const bootstrap = created
    ? [
        "LIGHTSPEED_ENVD_GATEWAY_URL=wss://<lightspeed host>/environment-gateway/connect",
        `LIGHTSPEED_ENVD_REGISTRATION_KEY=${created.secret}`,
        "lightspeed-envd",
      ].join("\n")
    : "";

  return (
    <>
      <Button variant="outline" onClick={() => { reset(); setOpen(true); }}>
        <KeyRound data-icon="inline-start" />
        Registration key
      </Button>
      <Dialog open={open} onOpenChange={(next) => { if (!next) reset(); }}>
        <DialogContent showCloseButton={!created}>
          {created ? (
            <>
              <DialogHeader>
                <DialogTitle>Copy the registration key</DialogTitle>
                <DialogDescription>
                  Shown once. Give it to the machines that should register under
                  “{created.registrationKey.displayName}”; in a sandbox, mount it as a file and delete
                  it after the daemon's receipt appears.
                </DialogDescription>
              </DialogHeader>
              <div className="grid gap-2">
                <FieldLabel htmlFor="registration-key-secret">Registration key</FieldLabel>
                <div className="flex gap-2">
                  <Input
                    id="registration-key-secret"
                    value={created.secret}
                    readOnly
                    className="font-mono text-xs"
                    onFocus={(event) => event.currentTarget.select()}
                  />
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => {
                      void navigator.clipboard.writeText(created.secret).then(() => setCopied(true));
                    }}
                  >
                    {copied ? <Check data-icon="inline-start" /> : <Copy data-icon="inline-start" />}
                    {copied ? "Copied" : "Copy"}
                  </Button>
                </div>
                <FieldLabel htmlFor="registration-key-bootstrap">Bootstrap</FieldLabel>
                <textarea
                  id="registration-key-bootstrap"
                  className="min-h-20 w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs outline-none"
                  value={bootstrap}
                  readOnly
                  onFocus={(event) => event.currentTarget.select()}
                />
              </div>
              <DialogFooter>
                <Button type="button" onClick={reset}>I saved the key</Button>
              </DialogFooter>
            </>
          ) : (
            <>
              <DialogHeader>
                <DialogTitle>Mint a registration key</DialogTitle>
                <DialogDescription>
                  Machines, VMs, containers, or pods start <span className="font-mono">lightspeed-envd</span> with
                  this key and become environments of this universe. The key's name is their group.
                </DialogDescription>
              </DialogHeader>
              <div className="grid gap-4">
                <Field>
                  <FieldLabel htmlFor="registration-key-name">Group name</FieldLabel>
                  <Input
                    id="registration-key-name"
                    value={displayName}
                    onChange={(event) => setDisplayName(event.target.value)}
                    placeholder="CI runners"
                    maxLength={128}
                    autoFocus
                  />
                </Field>
                <Field>
                  <FieldLabel>Identity</FieldLabel>
                  <Select value={identityMode} onValueChange={(next) => setIdentityMode(next as "persistent" | "ephemeral")}>
                    <SelectTrigger className="w-full">
                      <SelectValue>
                        {(value: string) => value === "ephemeral"
                          ? "Ephemeral · close after the daemon stays away"
                          : "Persistent · stay offline until closed"}
                      </SelectValue>
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="persistent">Persistent · stay offline until closed</SelectItem>
                      <SelectItem value="ephemeral">Ephemeral · close after the daemon stays away</SelectItem>
                    </SelectContent>
                  </Select>
                  <FieldDescription>
                    Persistent suits VMs and stateful pods whose daemon state survives restarts.
                    Ephemeral suits replaceable workers and sandboxes.
                  </FieldDescription>
                </Field>
                {identityMode === "ephemeral" && (
                  <Field>
                    <FieldLabel htmlFor="registration-key-grace">Disconnect grace (minutes)</FieldLabel>
                    <Input
                      id="registration-key-grace"
                      value={graceMinutes}
                      onChange={(event) => setGraceMinutes(event.target.value)}
                      placeholder="5"
                      inputMode="decimal"
                    />
                  </Field>
                )}
                <Field>
                  <FieldLabel htmlFor="registration-key-max">Active environment limit</FieldLabel>
                  <Input
                    id="registration-key-max"
                    value={maxActive}
                    onChange={(event) => setMaxActive(event.target.value)}
                    placeholder="unlimited"
                    inputMode="numeric"
                  />
                  <FieldDescription>Bounds what a leaked key can add to this universe.</FieldDescription>
                </Field>
                {create.error && <p className="text-sm text-destructive">{create.error.message}</p>}
                <DialogFooter>
                  <Button type="button" variant="outline" onClick={reset}>Cancel</Button>
                  <Button
                    type="button"
                    disabled={create.isPending || !displayName.trim()}
                    onClick={() => create.mutate()}
                  >
                    {create.isPending ? "Minting…" : "Mint key"}
                  </Button>
                </DialogFooter>
              </div>
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}

function ProviderBindings({ bindings }: { bindings: EnvironmentProviderBinding[] }) {
  const [open, setOpen] = useState(false);
  const enabled = bindings.filter((binding) => binding.status === "enabled").length;

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <section className="mt-6 rounded-xl border">
        <CollapsibleTrigger className="flex w-full items-center gap-3 px-4 py-3 text-left outline-none hover:bg-muted/30 focus-visible:ring-3 focus-visible:ring-inset focus-visible:ring-ring/50 md:px-5">
          <span className="text-sm font-medium">Provider bindings</span>
          <span className="text-xs text-muted-foreground">
            {enabled} enabled · {bindings.length} total
          </span>
          <ChevronDown className={`ml-auto size-4 text-muted-foreground transition-transform ${open ? "rotate-180" : ""}`} />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="divide-y border-t">
            {bindings.map((binding) => (
              <div key={binding.bindingId} className="grid gap-3 px-4 py-4 md:grid-cols-[minmax(0,1fr)_auto] md:px-5">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="text-sm font-medium">{humanizeIdentifier(binding.providerId)}</p>
                    <ProviderStatusBadge status={binding.status} />
                  </div>
                  <p className="mt-2 font-mono text-xs text-muted-foreground">
                    {binding.bindingId}
                  </p>
                </div>
                <div className="text-xs text-muted-foreground md:text-right">
                  <p>Revision {binding.revision}</p>
                  <p className="mt-1">Updated {relativeTime(binding.updatedAtMs)}</p>
                </div>
              </div>
            ))}
          </div>
        </CollapsibleContent>
      </section>
    </Collapsible>
  );
}

function ProviderStatusBadge({ status }: { status: EnvironmentProviderBinding["status"] }) {
  if (status === "enabled") return <Badge variant="secondary">enabled</Badge>;
  return (
    <Badge variant="outline" className="border-destructive/50 text-destructive">
      {status}
    </Badge>
  );
}

function EnvironmentIngressButton({
  universeId,
  environment,
}: {
  universeId: string;
  environment: Environment;
}) {
  const queryClient = useQueryClient();
  const ingress = useMutation({
    mutationFn: (enabled: boolean) => api<Environment>(
      "PUT",
      `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}/ingress`,
      { enabled },
    ),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["environments", universeId] }),
  });
  return (
    <>
      <Button
        variant="outline"
        size="xs"
        disabled={ingress.isPending || environment.status !== "ready"}
        onClick={() => ingress.mutate(!environment.publicIngressEnabled)}
      >
        {ingress.isPending
          ? "Updating ingress…"
          : environment.publicIngressEnabled ? "Disable public ingress" : "Enable public ingress"}
      </Button>
      {environment.publicEndpoint && (
        <a
          className="break-all text-xs text-primary underline-offset-4 hover:underline"
          href={environment.publicEndpoint}
          target="_blank"
          rel="noreferrer"
        >
          {environment.publicEndpoint}
        </a>
      )}
      {ingress.error && <span className="text-xs text-destructive">{ingress.error.message}</span>}
    </>
  );
}

function CloseEnvironmentButton({
  universeId,
  environment,
}: {
  universeId: string;
  environment: Environment;
}) {
  const queryClient = useQueryClient();
  const close = useMutation({
    mutationFn: () => api<Environment>(
      "DELETE",
      `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}`,
    ),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["environments", universeId] }),
  });
  if (environment.source.type !== "provisioned"
    || environment.status === "closed"
    || environment.status === "closing") {
    return null;
  }
  const label = environment.displayName ?? environment.environmentId;
  return (
    <div className="ml-auto flex flex-col items-end gap-1">
      <AlertDialog>
        <AlertDialogTrigger
          render={
            <Button variant="destructive" size="sm" disabled={close.isPending} />
          }
        >
          <Trash2 />
          {close.isPending ? "Closing…" : "Close environment"}
        </AlertDialogTrigger>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Close {label}?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently destroys the provider target
              {environment.incarnation.providerTargetId
                ? <> <span className="font-mono text-xs">{environment.incarnation.providerTargetId}</span></>
                : null}
              {" "}and everything stored on it. A closed environment cannot be
              reopened; sessions that still select it will see it as
              unavailable and its public endpoint stops working.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep environment</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => close.mutate()}
            >
              Close permanently
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {close.error && <span className="text-xs text-destructive">{close.error.message}</span>}
    </div>
  );
}

function CreateEnvironmentDialog({
  universeId,
  templates,
}: {
  universeId: string;
  templates: EnvironmentTemplate[];
}) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [templateKey, setTemplateKey] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [idlePolicy, setIdlePolicy] = useState<IdlePolicy | undefined>(undefined);
  const [requestId, setRequestId] = useState(() => crypto.randomUUID());
  const [error, setError] = useState<string | null>(null);
  const selectedTemplate = templates.find((template) =>
    templateKey === `${template.bindingId}:${template.templateId}`
  ) ?? templates[0];
  const create = useMutation({
    mutationFn: () => {
      if (!selectedTemplate) throw new Error("Choose an environment template.");
      return api<Environment>("POST", `/api/v1/universes/${universeId}/environments`, {
        requestId,
        bindingId: selectedTemplate.bindingId,
        templateId: selectedTemplate.templateId,
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        ...(idlePolicy ? { idlePolicy } : {}),
      });
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      setOpen(false);
      setDisplayName("");
      setIdlePolicy(undefined);
      setRequestId(crypto.randomUUID());
      setError(null);
    },
    onError: (cause) => setError(cause.message),
  });
  const selectedTemplateKey = selectedTemplate
    ? `${selectedTemplate.bindingId}:${selectedTemplate.templateId}`
    : "";

  return (
    <>
      <Button
        disabled={templates.length === 0}
        onClick={() => {
          setTemplateKey(selectedTemplateKey);
          setOpen(true);
        }}
      >
        New environment
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New universe environment</DialogTitle>
            <DialogDescription>
              Provision a universe-owned resource. Sessions may select it independently.
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-4">
            <Field>
              <FieldLabel>Template</FieldLabel>
              <Select
                value={selectedTemplateKey}
                onValueChange={(value) => setTemplateKey(value as string)}
              >
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {templates.map((template) => (
                    <SelectItem
                      key={`${template.bindingId}:${template.templateId}`}
                      value={`${template.bindingId}:${template.templateId}`}
                    >
                      {template.displayName}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel htmlFor="environment-display-name">Display name</FieldLabel>
              <input
                id="environment-display-name"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
                value={displayName}
                onChange={(event) => setDisplayName(event.target.value)}
                placeholder={selectedTemplate?.displayName ?? "Development environment"}
              />
              <FieldDescription>
                {selectedTemplate?.description ??
                  "Optional name shown throughout Lightspeed."}
              </FieldDescription>
            </Field>
            <IdlePolicyFields
              value={idlePolicy}
              warning={
                idlePolicy === undefined
                  ? "No stages: the environment never sleeps while idle. A box a bot or several sessions share should at least pause. You can change this later from the environment's details."
                  : undefined
              }
              onChange={setIdlePolicy}
            />
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>Cancel</Button>
            <Button
              disabled={!selectedTemplate || create.isPending || !idlePolicyIsMonotone(idlePolicy)}
              onClick={() => create.mutate()}
            >
              {create.isPending ? "Provisioning…" : "Provision"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function environmentName(environment: Environment): string {
  return environment.displayName
    ?? environment.incarnation.templateId
    ?? environment.incarnation.providerTargetId
    ?? humanizeIdentifier(environment.environmentId);
}

function humanizeIdentifier(value: string): string {
  return value
    .replace(/[-_]+/g, " ")
    .split(" ")
    .filter(Boolean)
    .map((word) => {
      if (word.toLowerCase() === "macbook") return "MacBook";
      if (word.toLowerCase() === "mac") return "Mac";
      return `${word.charAt(0).toUpperCase()}${word.slice(1)}`;
    })
    .join(" ");
}

function provisionedBindingId(environment: Environment): string | null {
  return environment.source.type === "provisioned" ? environment.source.bindingId : null;
}

function relativeTime(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 10_000) return "just now";
  if (delta < 60_000) return `${Math.floor(delta / 1000)}s ago`;
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h ago`;
  return `${Math.floor(delta / 86_400_000)}d ago`;
}

/// Attach a reachable `lightspeed-envd` directly (no provider). In development
/// `./dev.sh` starts one and Platform offers its endpoint as the default.
function RegisterExternalEnvironmentDialog({
  universeId,
  suggestedEndpoint,
  alreadyRegistered,
}: {
  universeId: string;
  suggestedEndpoint: string | null;
  alreadyRegistered: boolean;
}) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const register = useMutation({
    mutationFn: () =>
      api<Environment>("POST", `/api/v1/universes/${universeId}/environments/external`, {
        endpoint: endpoint.trim(),
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
      }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      setOpen(false);
      setError(null);
    },
    onError: (cause) => setError(cause.message),
  });
  const isDevSuggestion = Boolean(suggestedEndpoint) && endpoint.trim() === suggestedEndpoint;

  return (
    <>
      <Button
        variant={suggestedEndpoint && !alreadyRegistered ? "default" : "outline"}
        onClick={() => {
          setEndpoint(suggestedEndpoint && !alreadyRegistered ? suggestedEndpoint : "");
          setDisplayName(suggestedEndpoint && !alreadyRegistered ? "Local daemon" : "");
          setError(null);
          setOpen(true);
        }}
      >
        {suggestedEndpoint && !alreadyRegistered ? "Attach local daemon" : "Register external"}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Register external environment</DialogTitle>
            <DialogDescription>
              Attach a running <span className="font-mono">lightspeed-envd</span> that Lightspeed can
              reach directly, without a provider. Reachability is checked when it is used.
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-4">
            <Field>
              <FieldLabel htmlFor="external-endpoint">Daemon endpoint</FieldLabel>
              <input
                id="external-endpoint"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 font-mono text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
                value={endpoint}
                onChange={(event) => {
                  setEndpoint(event.target.value);
                  setError(null);
                }}
                placeholder="ws://127.0.0.1:19091/"
                spellCheck={false}
              />
              <FieldDescription>
                {isDevSuggestion
                  ? "The daemon started by ./dev.sh on this machine; its workspace is .lightspeed-dev/envd/workspace."
                  : "WebSocket URL of the daemon (ws:// or wss://)."}
              </FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="external-display-name">Display name</FieldLabel>
              <input
                id="external-display-name"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
                value={displayName}
                onChange={(event) => setDisplayName(event.target.value)}
                placeholder="Local daemon"
              />
            </Field>
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>Cancel</Button>
            <Button
              disabled={!/^wss?:\/\/\S+$/.test(endpoint.trim()) || register.isPending}
              onClick={() => register.mutate()}
            >
              {register.isPending ? "Registering…" : "Register"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
