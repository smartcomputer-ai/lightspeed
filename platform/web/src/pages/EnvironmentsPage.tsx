import { useActionPermissions } from "@/lib/permissions";
import { ResourceIdentityCell, ResourceRow, ResourceStatus, ResourceTable, ResourceValue } from "@/components/resource-table";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { environmentStatus, resourceTimestamp } from "@/lib/resource-status";
import { ReadError } from "@/components/read-error";
import { useRef, useState } from "react";
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Boxes, Check, ChevronDown, Copy, KeyRound, Pencil, Plus, RotateCw, Trash2 } from "lucide-react";
import {
  api,
  type Environment,
  type EnvironmentCredential,
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
import { TableCell } from "@/components/ui/table";
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
import { Switch } from "@/components/ui/switch";
import { EmptyState, LoadingNote, PageHeader, SectionHeader, ShowHiddenToggle, UniverseNotFound } from "@/components/page";
import type { EnvironmentIdlePolicyView } from "@lightspeed-ai/sdk";
import { EnvironmentPowerControls } from "@/components/environment/power-controls";
import { IdlePolicyFields, idlePolicyProblem, idleStagesFor } from "@/components/environment/idle-policy-fields";
import { MetadataMapEditor } from "@/components/session/metadata-editor";
import {
  environmentCredentialAvailable,
  environmentCredentialOptions,
  environmentCredentialSourceFromValue,
  environmentCredentialSourceLabel,
} from "@/lib/environment-credentials";
import {
  closeReasonLabel,
  describeIdlePolicy,
  environmentName,
  environmentPagePath,
  environmentTemplate,
  environmentTemplatesQuery,
  formatDuration,
  lifecycleMessage,
  type EnvironmentPage,
} from "@/lib/environments";
import { useActiveUniverse } from "@/lib/universes";

/// Universe environments are provisioned from the templates offered by the
/// universe's enabled providers. Physical provider registration stays admin-only.
export function EnvironmentsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !permissions.can("read")) {
    return <UniverseNotFound slug={slug} />;
  }

  return <EnvironmentList key={universe.id} universeId={universe.id} />;
}

const REFRESH_MS = 10_000;

function EnvironmentList({ universeId }: { universeId: string }) {
  const writable = useActionPermissions(universeId).can("configure_resource");
  const [showClosed, setShowClosed] = useState(false);
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState<string | null>(null);
  const detailsTriggerRef = useRef<HTMLElement | null>(null);
  const bindings = useQuery({
    queryKey: ["environment-provider-bindings", universeId],
    queryFn: () =>
      api<EnvironmentProviderBinding[]>(
        "GET",
        `/api/v1/universes/${universeId}/environment-provider-bindings`,
      ),
  });
  const templates = useQuery(environmentTemplatesQuery(universeId));
  // Pages of `environments/list`; closed rows only when asked for. Only the
  // lifecycle Lightspeed drives (create until accepted, close until gone)
  // changes without a user action, so polling is limited to those rows.
  const environments = useInfiniteQuery({
    queryKey: ["environments", universeId, "pages", showClosed],
    queryFn: ({ pageParam }) =>
      api<EnvironmentPage>("GET", environmentPagePath(universeId, { includeClosed: showClosed, after: pageParam })),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.nextAfter ?? undefined,
    refetchInterval: (query) =>
      query.state.data?.pages.some((page) =>
        page.environments.some((environment) => ["provisioning", "closing"].includes(environment.state)))
        ? REFRESH_MS
        : false,
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
    enabled: writable,
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
  const loadedRows = environments.data?.pages.flatMap((page) => page.environments) ?? [];
  const environmentRows = loadedRows
    .slice()
    .sort((a, b) => environmentName(a).localeCompare(environmentName(b))
      || a.environmentId.localeCompare(b.environmentId));
  const selectedEnvironment = environmentRows.find((environment) => environment.environmentId === selectedEnvironmentId);
  const enabledProviders = new Set(
    bindingRows.filter((binding) => binding.enabled).map((binding) => binding.providerId),
  );
  const creatableTemplates = templateRows.filter(
    (template) => enabledProviders.has(template.providerId) && !template.deprecated,
  );
  const keyRows = (registrationKeys.data ?? [])
    .slice()
    .sort((a, b) => a.displayName.localeCompare(b.displayName));
  const keysById = new Map(keyRows.map((key) => [key.registrationKeyId, key]));
  const registrationKeyOf = (environment: Environment) => environment.source.type === "registered"
    ? keysById.get(environment.source.registrationKeyId)
    : undefined;

  return (
    <>
      <PageHeader
        title="Environments"
        description="Computers and sandboxes agents can use in this universe."
        actions={writable && (
          <div className="flex flex-wrap items-center gap-2">
            <RegisterExternalEnvironmentDialog
              universeId={universeId}
              suggestedEndpoint={hints.data?.devEnvdEndpoint ?? null}
              alreadyRegistered={loadedRows.some(
                (environment) =>
                  environment.source.type === "external"
                  && hints.data?.devEnvdEndpoint
                  && environment.source.endpoint === hints.data.devEnvdEndpoint,
              )}
            />
            {creatableTemplates.length > 0 && (
              <CreateEnvironmentDialog universeId={universeId} templates={creatableTemplates} />
            )}
          </div>
        )}
      />
      {(bindings.isLoading || templates.isLoading || environments.isLoading || secrets.isLoading) && <LoadingNote />}
      {bindings.error && (
        <ReadError error={bindings.error} loading={!bindings.data} prefix="Provider bindings unavailable" />
      )}
      {templates.error && (
        <ReadError error={templates.error} loading={!templates.data} prefix="Environment templates unavailable" />
      )}
      {environments.error && (
        <ReadError error={environments.error} loading={!environments.data} prefix="Environments unavailable" />
      )}
      {secrets.error && (
        <ReadError error={secrets.error} loading={!secrets.data} prefix="Access credentials unavailable" />
      )}
      {environments.data && environmentRows.length === 0 && (
        <EmptyState icon={Boxes} title={showClosed ? "No environments yet" : "No open environments"}>
          Machines agents run commands on: created from a provider template, or registered by
          running <span className="font-mono">lightspeed-envd</span> on a machine of your own.
        </EmptyState>
      )}
      {environmentRows.length > 0 && (
        <ResourceTable label="Environments" columns={["Environment", "Source", "Status"]}>
          {environmentRows.map((environment) => (
            <EnvironmentRow
              key={environment.environmentId}
              environment={environment}
              onSelect={(trigger) => {
                detailsTriggerRef.current = trigger;
                setSelectedEnvironmentId(environment.environmentId);
              }}
              template={environmentTemplate(environment, templateRows)}
              registrationKey={registrationKeyOf(environment)}
            />
          ))}
        </ResourceTable>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-4">
        {environments.hasNextPage && (
          <Button
            variant="outline"
            size="sm"
            disabled={environments.isFetchingNextPage}
            onClick={() => void environments.fetchNextPage()}
          >
            {environments.isFetchingNextPage ? "Loading…" : "Load more"}
          </Button>
        )}
        <label className="flex w-fit cursor-pointer items-center gap-2 text-sm text-muted-foreground">
          <Switch checked={showClosed} onCheckedChange={setShowClosed} />
          Show closed environments
        </label>
      </div>
      <ResourceDetailsDialog open={selectedEnvironment !== undefined}
        onOpenChange={(open) => { if (!open) setSelectedEnvironmentId(null); }}
        title={selectedEnvironment ? environmentName(selectedEnvironment) : "Environment"}
        description="Machine details, access credentials, and power settings." finalFocus={detailsTriggerRef}>
        {selectedEnvironment && (
          <EnvironmentDetails
            key={selectedEnvironment.environmentId}
            universeId={universeId}
            environment={selectedEnvironment}
            configurable={writable}
            template={environmentTemplate(selectedEnvironment, templateRows)}
            registrationKey={registrationKeyOf(selectedEnvironment)}
            secrets={secrets.data}
          />
        )}
      </ResourceDetailsDialog>
      {writable && (
        <section className="mt-8">
          <SectionHeader
            title="Enrollment"
            description="Registration keys let daemons on your machines enroll as environments."
            actions={<CreateRegistrationKeyDialog universeId={universeId} />}
          />
          {registrationKeys.isLoading && <LoadingNote />}
          {registrationKeys.error && (
            <ReadError error={registrationKeys.error} loading={!registrationKeys.data} prefix="Registration keys unavailable" />
          )}
          {registrationKeys.data && keyRows.length === 0 && (
            <p className="text-sm text-muted-foreground">No registration keys yet.</p>
          )}
          {keyRows.length > 0 && <RegistrationKeys universeId={universeId} keys={keyRows} />}
        </section>
      )}
      {bindingRows.length > 0 && <ProviderBindings bindings={bindingRows} />}
    </>
  );
}

function sourceLabel(environment: Environment): string {
  const source = environment.source;
  return source.type === "provisioned" ? "Provisioned" : source.type === "registered" ? "Registered" : "External";
}

function EnvironmentRow({
  environment,
  onSelect,
  template,
  registrationKey,
}: {
  environment: Environment;
  onSelect: (trigger: HTMLButtonElement | null) => void;
  template: EnvironmentTemplate | undefined;
  registrationKey: EnvironmentRegistrationKey | undefined;
}) {
  const source = environment.source;
  const sourceName = sourceLabel(environment);
  const sourceDescription = source.type === "provisioned"
    ? `${source.providerId} · ${template?.displayName ?? source.templateId}`
    : source.type === "registered" ? registrationKey?.displayName ?? source.registrationKeyId : source.endpoint;
  return (
    <ResourceRow name={environmentName(environment)} onOpen={onSelect}>
      <ResourceIdentityCell name={environmentName(environment)} subtitle={environment.environmentId} mono compactDescription={`${sourceName} · ${sourceDescription}`} />
      <TableCell className="hidden max-w-64 sm:table-cell"><ResourceValue description={sourceDescription}>{sourceName}</ResourceValue></TableCell>
      <TableCell><ResourceStatus status={environmentStatus(environment)} /></TableCell>
    </ResourceRow>
  );
}

function EnvironmentDetails({
  universeId,
  environment,
  configurable,
  template,
  registrationKey,
  secrets,
}: {
  universeId: string;
  environment: Environment;
  /** Configuring this environment: rename, power, ingress, close. */
  configurable: boolean;
  template: EnvironmentTemplate | undefined;
  registrationKey: EnvironmentRegistrationKey | undefined;
  secrets: SecretsInventory | undefined;
}) {
  const [editOpen, setEditOpen] = useState(false);
  const source = environment.source;
  const closed = environment.state === "closed";
  const closeReason = closeReasonLabel(environment.closeReason);

  return (
    <div className="grid gap-6">
      <ResourceDetailsGrid>
        <Detail label="Environment ID" value={environment.environmentId} mono />
        <ResourceDetail label="Status"><ResourceStatus status={environmentStatus(environment)} /></ResourceDetail>
        <Detail label="State" value={`${environment.state} · since ${relativeTime(environment.stateChangedAtMs)}`} />
        {environment.stateMessage && (
          <ResourceDetail label={environment.state === "failed" ? "Failure" : "State message"}>
            <span className={environment.state === "failed" ? "text-destructive" : undefined}>{environment.stateMessage}</span>
          </ResourceDetail>
        )}
        {closeReason && <Detail label="Close reason" value={closeReason} />}
        <Detail label="Source" value={sourceLabel(environment)} />
        {source.type === "provisioned" && (
          <ResourceDetail label="Public endpoint" mono>
            {source.publicEndpoint
              ? <a className="text-primary underline-offset-4 hover:underline" href={source.publicEndpoint} target="_blank" rel="noreferrer">{source.publicEndpoint}</a>
              : "Private environment"}
          </ResourceDetail>
        )}
        {source.type === "external" && <Detail label="Connection endpoint" value={source.endpoint} mono />}
        {source.type === "registered" && (
          <>
            <Detail label="Registration key" value={registrationKey?.displayName ?? source.registrationKeyId} />
            <Detail label="Daemon" value={source.daemonId} mono />
            <Detail
              label="Identity"
              value={source.identityMode === "ephemeral"
                ? "Ephemeral · closes after its key's disconnect grace"
                : "Persistent · waits for its daemon to reconnect"}
            />
            <Detail
              label="Connection"
              value={`${source.connected ? "Connected" : "Disconnected"} since ${relativeTime(source.connectionChangedAtMs)}`}
            />
            <Detail label="Last seen" value={relativeTime(source.lastSeenAtMs)} />
          </>
        )}
        {source.type === "provisioned" && (
          <>
            <Detail label="Provider" value={source.providerId} mono />
            <Detail label="Template" value={template?.displayName ?? source.templateId} />
            {source.targetId && <Detail label="Target" value={source.targetId} mono />}
            <Detail
              label="Provider reports"
              value={source.observed
                ? `${source.observed.phase}${source.observed.power ? ` · ${source.observed.power}` : ""} · ${relativeTime(source.observed.observedAtMs)}${source.observed.message ? ` · ${source.observed.message}` : ""}`
                : "nothing yet"}
            />
            <Detail label="Idle policy" value={describeIdlePolicy(source.idle ?? template?.idle)} />
          </>
        )}
        {environment.daemon && (
          <Detail
            label="Daemon build"
            value={[
              environment.daemon.version,
              environment.daemon.gitSha?.slice(0, 12),
              `protocol ${environment.daemon.protocolVersion}`,
            ].filter(Boolean).join(" · ")}
          />
        )}
        {Object.entries(environment.metadata ?? {}).map(([key, value]) => (
          <Detail key={key} label={key} value={value} mono />
        ))}
        <Detail label="Created" value={resourceTimestamp(environment.createdAtMs)} />
        <Detail label="Updated" value={resourceTimestamp(environment.updatedAtMs)} />
      </ResourceDetailsGrid>
      <EnvironmentCredentials
        universeId={universeId}
        environment={environment}
        secrets={secrets}
        writable={configurable}
      />
      {configurable && !closed && (
        <ResourceActions>
          <CloseEnvironmentButton universeId={universeId} environment={environment} />
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => setEditOpen(true)}>
              <Pencil data-icon="inline-start" />
              Rename…
            </Button>
            {source.type === "provisioned" && (
              <EnvironmentPowerControls
                universeId={universeId}
                environment={environment}
                powerStates={template?.powerStates ?? []}
                size="sm"
              />
            )}
            {source.type === "provisioned" && template?.publicIngress && environment.state === "open" && (
              <EnvironmentIngressButton universeId={universeId} environment={environment} />
            )}
          </div>
        </ResourceActions>
      )}
      {configurable && !closed && (
        <EditEnvironmentDialog
          key={`${environment.environmentId}:${editOpen ? "open" : "closed"}`}
          universeId={universeId}
          environment={environment}
          open={editOpen}
          onOpenChange={setEditOpen}
        />
      )}
    </div>
  );
}

function EnvironmentCredentials({
  universeId,
  environment,
  secrets,
  writable,
}: {
  universeId: string;
  environment: Environment;
  secrets: SecretsInventory | undefined;
  writable: boolean;
}) {
  const queryClient = useQueryClient();
  const credentials = useQuery({
    queryKey: ["environment-credentials", universeId, environment.environmentId],
    queryFn: () =>
      api<EnvironmentCredential[]>(
        "GET",
        `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}/credentials`,
      ),
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
  const canAssign = environment.state !== "closed" && environment.state !== "closing";

  return (
    <div className="border-t pt-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 className="text-sm font-medium">Secret environment variables</h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Resolved when a process or job starts. Secret values are never shown here.
          </p>
        </div>
        {writable && (
          <AssignCredentialDialog
            universeId={universeId}
            environmentId={environment.environmentId}
            secrets={secrets}
            disabled={!canAssign}
          />
        )}
      </div>
      {credentials.isLoading && <p className="mt-3 text-xs text-muted-foreground">Loading…</p>}
      {credentials.error && (
        <p className="mt-3 text-sm text-destructive">{credentials.error.message}</p>
      )}
      {unbind.error && <p className="mt-3 text-sm text-destructive">{unbind.error.message}</p>}
      {secrets && environmentCredentialOptions(secrets).length === 0 && (
        <p className="mt-3 text-xs text-muted-foreground">
          Add an environment secret or access credential on the Credentials page, or a model
          provider API key on the Models page, first.
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
                {writable && (
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
                )}
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

function Detail({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <ResourceDetail label={label} mono={mono}>{value}</ResourceDetail>;
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
  const [open, setOpen] = useState(false);
  const [showInactive, setShowInactive] = useState(false);
  const [editing, setEditing] = useState<EnvironmentRegistrationKey | null>(null);
  const inactiveCount = keys.filter((key) => key.status !== "active").length;
  const rows = showInactive ? keys : keys.filter((key) => key.status === "active");
  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <section className="rounded-xl border p-4">
        <CollapsibleTrigger className="flex w-full items-center gap-2 text-left text-sm font-medium outline-none focus-visible:ring-3 focus-visible:ring-ring/50">
          Registration keys ({keys.length})
          <ChevronDown aria-hidden="true" className={`ml-auto size-4 transition-transform ${open ? "rotate-180" : ""}`} />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <p className="mt-1 text-sm text-muted-foreground">
            A key lets <span className="font-mono">lightspeed-envd</span> daemons dial in and register as
            environments; each key is the group its environments belong to. Revoking or rotating stops
            new daemons while registered ones keep reconnecting.
          </p>
          <div className="mt-3 grid gap-2">
            {rows.map((key) => (
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
                    ? ` · grace ${formatDuration(key.ephemeralDisconnectGraceMs)}`
                    : key.offlineCloseAfterMs
                      ? ` · closes offline after ${formatDuration(key.offlineCloseAfterMs)}`
                      : ""}
                  {key.expiresAtMs ? ` · expires ${relativeTime(key.expiresAtMs).replace(" ago", "")}` : ""}
                </span>
                {key.status === "active" ? (
                  <Badge variant="secondary">active</Badge>
                ) : (
                  <Badge variant="outline">{key.status}</Badge>
                )}
                {key.status !== "revoked" && (
                  <Button variant="outline" size="xs" onClick={() => setEditing(key)}>
                    <Pencil data-icon="inline-start" />
                    Edit
                  </Button>
                )}
                {key.status === "active" && (
                  <RotateRegistrationKeyButton universeId={universeId} registrationKey={key} />
                )}
                {key.status === "active" && (
                  <RevokeRegistrationKeyButton universeId={universeId} registrationKey={key} />
                )}
              </div>
            ))}
            {rows.length === 0 && (
              <p className="text-sm text-muted-foreground">Every registration key here is revoked or expired.</p>
            )}
          </div>
          <div className="mt-3">
            <ShowHiddenToggle
              label="Show revoked and expired keys"
              count={inactiveCount}
              checked={showInactive}
              onCheckedChange={setShowInactive}
            />
          </div>
        </CollapsibleContent>
      </section>
      <EditRegistrationKeyDialog
        key={editing?.registrationKeyId ?? "closed"}
        universeId={universeId}
        registrationKey={editing}
        onOpenChange={(next) => { if (!next) setEditing(null); }}
      />
    </Collapsible>
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

/// Retire the key's live secret and show its replacement once. The group,
/// its policy, and the daemons it already admitted are unaffected.
function RotateRegistrationKeyButton({
  universeId,
  registrationKey,
}: {
  universeId: string;
  registrationKey: EnvironmentRegistrationKey;
}) {
  const queryClient = useQueryClient();
  const [confirming, setConfirming] = useState(false);
  const rotate = useMutation({
    mutationFn: () =>
      api<EnvironmentRegistrationKeyCreated>(
        "POST",
        `/api/v1/universes/${universeId}/environment-registration-keys/${registrationKey.registrationKeyId}/rotate`,
      ),
    onSuccess: async () => {
      setConfirming(false);
      await queryClient.invalidateQueries({ queryKey: ["environment-registration-keys", universeId] });
    },
  });
  return (
    <>
      <Button variant="outline" size="xs" onClick={() => { rotate.reset(); setConfirming(true); }}>
        <RotateCw data-icon="inline-start" />
        Rotate
      </Button>
      <AlertDialog open={confirming} onOpenChange={setConfirming}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Rotate “{registrationKey.displayName}”?</AlertDialogTitle>
            <AlertDialogDescription>
              The current secret stops admitting new daemons at once; give the new one to the
              machines that should still register. Daemons that already registered keep
              reconnecting by their own identity.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {rotate.error && <p className="text-sm text-destructive">{rotate.error.message}</p>}
          <AlertDialogFooter>
            <AlertDialogCancel>Keep</AlertDialogCancel>
            <AlertDialogAction onClick={() => rotate.mutate()} disabled={rotate.isPending}>
              {rotate.isPending ? "Rotating…" : "Rotate key"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Dialog open={rotate.data !== undefined} onOpenChange={(next) => { if (!next) rotate.reset(); }}>
        <DialogContent showCloseButton={false}>
          {rotate.data && <RegistrationSecret created={rotate.data} onDone={() => rotate.reset()} />}
        </DialogContent>
      </Dialog>
    </>
  );
}

/// The engine's grace for an ephemeral key that sets none. The key view only
/// carries the effective grace, so the edit form leaves the field empty when
/// it equals this default and an unchanged save keeps following it.
const DEFAULT_EPHEMERAL_GRACE_MS = 5 * 60_000;

/// The editable policy of a registration key, in the units the form shows.
type RegistrationKeyForm = {
  displayName: string;
  maxActive: string;
  graceMinutes: string;
  offlineCloseMinutes: string;
};

function registrationKeyForm(key: EnvironmentRegistrationKey | null): RegistrationKeyForm {
  return {
    displayName: key?.displayName ?? "",
    maxActive: key?.maxActiveEnvironments ? String(key.maxActiveEnvironments) : "",
    graceMinutes:
      key && key.identityMode === "ephemeral" && key.ephemeralDisconnectGraceMs !== DEFAULT_EPHEMERAL_GRACE_MS
        ? String(key.ephemeralDisconnectGraceMs / 60_000)
        : "",
    offlineCloseMinutes: key?.offlineCloseAfterMs ? String(key.offlineCloseAfterMs / 60_000) : "",
  };
}

/// Request fields for `registration-keys/create` and `/update`: absent
/// fields are unset, so an edit sends the complete policy.
function registrationKeySettings(
  form: RegistrationKeyForm,
  identityMode: "persistent" | "ephemeral",
  expiresAtMs: number | null | undefined,
) {
  const minutes = (value: string) => Math.round(Number(value) * 60_000);
  return {
    displayName: form.displayName.trim(),
    ...(form.maxActive.trim() ? { maxActiveEnvironments: Number(form.maxActive) } : {}),
    ...(identityMode === "ephemeral" && form.graceMinutes.trim()
      ? { ephemeralDisconnectGraceMs: minutes(form.graceMinutes) }
      : {}),
    ...(identityMode === "persistent" && form.offlineCloseMinutes.trim()
      ? { offlineCloseAfterMs: minutes(form.offlineCloseMinutes) }
      : {}),
    ...(expiresAtMs ? { expiresAtMs } : {}),
  };
}

function registrationKeyFormError(form: RegistrationKeyForm): string | null {
  if (!form.displayName.trim()) return "Name the group.";
  if (form.maxActive.trim() && !(Number.isInteger(Number(form.maxActive)) && Number(form.maxActive) >= 1)) {
    return "The active environment limit must be a whole number of at least 1.";
  }
  for (const value of [form.graceMinutes, form.offlineCloseMinutes]) {
    if (value.trim() && !(Number(value) > 0)) return "Durations must be positive minutes.";
  }
  return null;
}

function RegistrationKeyFields({
  form,
  identityMode,
  onChange,
}: {
  form: RegistrationKeyForm;
  identityMode: "persistent" | "ephemeral";
  onChange: (form: RegistrationKeyForm) => void;
}) {
  return (
    <>
      <Field>
        <FieldLabel htmlFor="registration-key-name">Group name</FieldLabel>
        <Input
          id="registration-key-name"
          value={form.displayName}
          onChange={(event) => onChange({ ...form, displayName: event.target.value })}
          placeholder="CI runners"
          maxLength={128}
          autoFocus
        />
      </Field>
      {identityMode === "ephemeral" ? (
        <Field>
          <FieldLabel htmlFor="registration-key-grace">Disconnect grace (minutes)</FieldLabel>
          <Input
            id="registration-key-grace"
            value={form.graceMinutes}
            onChange={(event) => onChange({ ...form, graceMinutes: event.target.value })}
            placeholder={String(DEFAULT_EPHEMERAL_GRACE_MS / 60_000)}
            inputMode="decimal"
          />
          <FieldDescription>An environment whose daemon stays away this long is closed. Empty uses the default.</FieldDescription>
        </Field>
      ) : (
        <Field>
          <FieldLabel htmlFor="registration-key-offline">Close when offline for (minutes)</FieldLabel>
          <Input
            id="registration-key-offline"
            value={form.offlineCloseMinutes}
            onChange={(event) => onChange({ ...form, offlineCloseMinutes: event.target.value })}
            placeholder="never"
            inputMode="decimal"
          />
          <FieldDescription>
            Empty keeps a disconnected environment open until it is closed, for machines that
            come back; set it when scaled-down machines do not.
          </FieldDescription>
        </Field>
      )}
      <Field>
        <FieldLabel htmlFor="registration-key-max">Active environment limit</FieldLabel>
        <Input
          id="registration-key-max"
          value={form.maxActive}
          onChange={(event) => onChange({ ...form, maxActive: event.target.value })}
          placeholder="unlimited"
          inputMode="numeric"
        />
        <FieldDescription>Bounds what a leaked key can add to this universe.</FieldDescription>
      </Field>
    </>
  );
}

/// The one-time view of a registration secret, with the two variables a
/// daemon needs to dial in.
function RegistrationSecret({
  created,
  onDone,
}: {
  created: EnvironmentRegistrationKeyCreated;
  onDone: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const bootstrap = [
    "LIGHTSPEED_ENVD_GATEWAY_URL=wss://<lightspeed host>/environment-gateway/connect",
    `LIGHTSPEED_ENVD_REGISTRATION_KEY=${created.secret}`,
    "lightspeed-envd",
  ].join("\n");
  return (
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
        <Button type="button" onClick={onDone}>I saved the key</Button>
      </DialogFooter>
    </>
  );
}

/// Mint a registration key. The secret is shown once.
function CreateRegistrationKeyDialog({ universeId }: { universeId: string }) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState(() => registrationKeyForm(null));
  const [identityMode, setIdentityMode] = useState<"persistent" | "ephemeral">("persistent");
  const [created, setCreated] = useState<EnvironmentRegistrationKeyCreated | null>(null);
  const create = useMutation({
    mutationFn: () =>
      api<EnvironmentRegistrationKeyCreated>(
        "POST",
        `/api/v1/universes/${universeId}/environment-registration-keys`,
        { identityMode, ...registrationKeySettings(form, identityMode, null) },
      ),
    onSuccess: async (result) => {
      setCreated(result);
      await queryClient.invalidateQueries({ queryKey: ["environment-registration-keys", universeId] });
    },
  });
  const reset = () => {
    setOpen(false);
    setForm(registrationKeyForm(null));
    setIdentityMode("persistent");
    setCreated(null);
    create.reset();
  };
  const formError = registrationKeyFormError(form);

  return (
    <>
      <Button variant="outline" onClick={() => { reset(); setOpen(true); }}>
        <KeyRound data-icon="inline-start" />
        Registration key
      </Button>
      <Dialog open={open} onOpenChange={(next) => { if (!next) reset(); }}>
        <DialogContent showCloseButton={!created}>
          {created ? (
            <RegistrationSecret created={created} onDone={reset} />
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
                  <FieldLabel>Identity</FieldLabel>
                  <Select value={identityMode} onValueChange={(next) => setIdentityMode(next as "persistent" | "ephemeral")}>
                    <SelectTrigger className="w-full">
                      <SelectValue>
                        {(value: string) => value === "ephemeral"
                          ? "Ephemeral · close after the daemon stays away"
                          : "Persistent · wait for the daemon to reconnect"}
                      </SelectValue>
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="persistent">Persistent · wait for the daemon to reconnect</SelectItem>
                      <SelectItem value="ephemeral">Ephemeral · close after the daemon stays away</SelectItem>
                    </SelectContent>
                  </Select>
                  <FieldDescription>
                    Persistent suits VMs and stateful pods whose daemon state survives restarts.
                    Ephemeral suits replaceable workers and sandboxes. It cannot be changed later.
                  </FieldDescription>
                </Field>
                <RegistrationKeyFields form={form} identityMode={identityMode} onChange={setForm} />
                {create.error && <p className="text-sm text-destructive">{create.error.message}</p>}
                <DialogFooter>
                  <Button type="button" variant="outline" onClick={reset}>Cancel</Button>
                  <Button
                    type="button"
                    disabled={create.isPending || formError !== null}
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

/// Replace a key's editable policy (`registration-keys/update`). The
/// identity mode is fixed when the key is minted.
function EditRegistrationKeyDialog({
  universeId,
  registrationKey,
  onOpenChange,
}: {
  universeId: string;
  registrationKey: EnvironmentRegistrationKey | null;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [form, setForm] = useState(() => registrationKeyForm(registrationKey));
  const save = useMutation({
    mutationFn: (key: EnvironmentRegistrationKey) =>
      api<EnvironmentRegistrationKey>(
        "PUT",
        `/api/v1/universes/${universeId}/environment-registration-keys/${key.registrationKeyId}`,
        registrationKeySettings(form, key.identityMode, key.expiresAtMs),
      ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["environment-registration-keys", universeId] });
      onOpenChange(false);
    },
  });
  const formError = registrationKeyFormError(form);
  return (
    <Dialog open={registrationKey !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Edit “{registrationKey?.displayName}”</DialogTitle>
          <DialogDescription>
            {registrationKey?.identityMode === "ephemeral" ? "Ephemeral" : "Persistent"} key. The active limit
            and expiry apply to later admissions; a changed disconnect grace or offline close-after applies to
            admitted environments whose daemon is away.
          </DialogDescription>
        </DialogHeader>
        {registrationKey && (
          <div className="grid gap-4">
            <RegistrationKeyFields form={form} identityMode={registrationKey.identityMode} onChange={setForm} />
            {save.error && <p className="text-sm text-destructive">{save.error.message}</p>}
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
              <Button
                type="button"
                disabled={save.isPending || formError !== null}
                onClick={() => save.mutate(registrationKey)}
              >
                {save.isPending ? "Saving…" : "Save"}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function ProviderBindings({ bindings }: { bindings: EnvironmentProviderBinding[] }) {
  const [open, setOpen] = useState(false);
  const enabled = bindings.filter((binding) => binding.enabled).length;

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <section className="mt-6 rounded-xl border">
        <CollapsibleTrigger className="flex w-full items-center gap-3 px-4 py-3 text-left outline-none hover:bg-muted/30 focus-visible:ring-3 focus-visible:ring-inset focus-visible:ring-ring/50 md:px-5">
          <span className="text-sm font-medium">Provider configuration</span>
          <span className="text-xs text-muted-foreground">
            {enabled} enabled · {bindings.length} total
          </span>
          <ChevronDown className={`ml-auto size-4 text-muted-foreground transition-transform ${open ? "rotate-180" : ""}`} />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="divide-y border-t">
            {bindings.map((binding) => (
              <div key={binding.providerId} className="grid gap-3 px-4 py-4 md:grid-cols-[minmax(0,1fr)_auto] md:px-5">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="text-sm font-medium">{humanizeIdentifier(binding.providerId)}</p>
                    {binding.enabled ? (
                      <Badge variant="secondary">enabled</Badge>
                    ) : (
                      <Badge variant="outline" className="border-destructive/50 text-destructive">disabled</Badge>
                    )}
                  </div>
                  <p className="mt-2 font-mono text-xs text-muted-foreground">
                    {binding.providerId}
                  </p>
                </div>
                <div className="text-xs text-muted-foreground md:text-right">
                  <p>Updated {relativeTime(binding.updatedAtMs)}</p>
                </div>
              </div>
            ))}
          </div>
        </CollapsibleContent>
      </section>
    </Collapsible>
  );
}

/// One-shot `environments/ingress/put`: the provider adds or removes the
/// public endpoint during the call, and the details show what it returned.
function EnvironmentIngressButton({
  universeId,
  environment,
}: {
  universeId: string;
  environment: Environment;
}) {
  const queryClient = useQueryClient();
  const publicEndpoint = environment.source.type === "provisioned" ? environment.source.publicEndpoint : null;
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
        size="sm"
        disabled={ingress.isPending}
        onClick={() => ingress.mutate(!publicEndpoint)}
      >
        {ingress.isPending
          ? "Updating ingress…"
          : publicEndpoint ? "Disable public ingress" : "Enable public ingress"}
      </Button>
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
  if (environment.state === "closed" || environment.state === "closing") {
    return null;
  }
  const source = environment.source;
  const label = environmentName(environment);
  return (
    <div className="flex flex-col items-start gap-1">
      <ConfirmDangerButton
        label={close.isPending ? "Closing…" : "Close environment"}
        title={`Close ${label}?`}
        description={<>
          {source.type === "provisioned" ? (
            <>
              This permanently destroys the provider target
              {source.targetId
                ? <> <span className="font-mono text-xs">{source.targetId}</span></>
                : null}
              {" "}and everything stored on it, and its public endpoint stops working.
            </>
          ) : source.type === "registered" ? (
            <>
              The daemon is told to stop registering and its identity is spent; the machine
              itself and its files are untouched.
            </>
          ) : (
            <>
              Lightspeed stops using the daemon at{" "}
              <span className="font-mono text-xs">{source.endpoint}</span>; the daemon itself
              keeps running.
            </>
          )}
          {" "}A closed environment cannot be reopened; sessions that select it will lose access.
        </>}
        pending={close.isPending}
        onConfirm={() => close.mutate()}
      />
      {close.error && <span className="text-xs text-destructive">{close.error.message}</span>}
    </div>
  );
}

/// Replace the environment's display name and caller metadata
/// (`environments/update`).
function EditEnvironmentDialog({
  universeId,
  environment,
  open,
  onOpenChange,
}: {
  universeId: string;
  environment: Environment;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [displayName, setDisplayName] = useState(environment.displayName ?? "");
  const [metadata, setMetadata] = useState<Record<string, string> | undefined>(
    Object.keys(environment.metadata ?? {}).length ? environment.metadata : undefined,
  );
  const save = useMutation({
    mutationFn: () => api<Environment>(
      "PATCH",
      `/api/v1/universes/${universeId}/environments/${encodeURIComponent(environment.environmentId)}`,
      {
        displayName: displayName.trim() || null,
        metadata: metadata ?? {},
      },
    ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      onOpenChange(false);
    },
  });
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Rename {environmentName(environment)}</DialogTitle>
          <DialogDescription>
            The name shown throughout Lightspeed, and labels for finding the environment.
            Neither changes how it runs.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="environment-rename">Display name</FieldLabel>
            <Input
              id="environment-rename"
              value={displayName}
              onChange={(event) => setDisplayName(event.target.value)}
              placeholder={environment.environmentId}
              maxLength={200}
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel>Metadata</FieldLabel>
            <MetadataMapEditor value={metadata} onChange={setMetadata} />
          </Field>
          {save.error && <p className="text-sm text-destructive">{save.error.message}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button disabled={save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? "Saving…" : "Save"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
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
  const [customIdle, setCustomIdle] = useState(false);
  const [idle, setIdle] = useState<EnvironmentIdlePolicyView | undefined>(undefined);
  const [requestId, setRequestId] = useState(() => crypto.randomUUID());
  const [error, setError] = useState<string | null>(null);
  const selectedTemplate = templates.find((template) => templateKey === templateValue(template)) ?? templates[0];
  const idleProblem = customIdle ? idlePolicyProblem(idle) : null;
  /// The override starts from the template's default, limited to the stages
  /// the template can carry out.
  const resetIdle = (template: EnvironmentTemplate | undefined) => {
    const stages = idleStagesFor(template?.powerStates ?? []);
    const defaults = Object.fromEntries(
      stages
        .map((stage) => [stage.key, template?.idle?.[stage.key]] as const)
        .filter(([, ms]) => typeof ms === "number"),
    ) as EnvironmentIdlePolicyView;
    setIdle(Object.keys(defaults).length ? defaults : undefined);
  };
  const create = useMutation({
    mutationFn: () => {
      if (!selectedTemplate) throw new Error("Choose an environment template.");
      return api<Environment>("POST", `/api/v1/universes/${universeId}/environments`, {
        requestId,
        providerId: selectedTemplate.providerId,
        templateId: selectedTemplate.templateId,
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        ...(customIdle && idle ? { idle } : {}),
      });
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      setOpen(false);
      setDisplayName("");
      setCustomIdle(false);
      setIdle(undefined);
      setRequestId(crypto.randomUUID());
      setError(null);
    },
    onError: (cause) => setError(cause.message),
  });
  const selectedTemplateKey = selectedTemplate ? templateValue(selectedTemplate) : "";

  return (
    <>
      <Button
        disabled={templates.length === 0}
        onClick={() => {
          setTemplateKey(selectedTemplateKey);
          setOpen(true);
        }}
      >
        <Plus data-icon="inline-start" />
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
                onValueChange={(value) => {
                  setTemplateKey(value as string);
                  resetIdle(templates.find((template) => templateValue(template) === value));
                }}
              >
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {templates.map((template) => (
                    <SelectItem key={templateValue(template)} value={templateValue(template)}>
                      {template.displayName}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {selectedTemplate && (
                <FieldDescription>
                  {humanizeIdentifier(selectedTemplate.providerId)} · idle policy{" "}
                  {describeIdlePolicy(selectedTemplate.idle)}
                  {selectedTemplate.powerStates.length
                    ? ` · power ${selectedTemplate.powerStates.join(", ")}`
                    : " · no power control"}
                  . The provider runs the idle policy; a powered-down environment wakes when a
                  tool uses it.
                </FieldDescription>
              )}
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
            {selectedTemplate && (
              <Field orientation="horizontal">
                <Switch
                  id="environment-custom-idle"
                  checked={customIdle}
                  onCheckedChange={(checked) => {
                    setCustomIdle(checked);
                    if (checked) resetIdle(selectedTemplate);
                  }}
                />
                <FieldLabel htmlFor="environment-custom-idle">
                  Custom idle policy instead of the template's
                </FieldLabel>
              </Field>
            )}
            {selectedTemplate && customIdle && (
              <IdlePolicyFields
                value={idle}
                powerStates={selectedTemplate.powerStates}
                onChange={setIdle}
              />
            )}
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>Cancel</Button>
            <Button
              disabled={!selectedTemplate || create.isPending || idleProblem !== null}
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

function templateValue(template: EnvironmentTemplate): string {
  return JSON.stringify([template.providerId, template.templateId]);
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
