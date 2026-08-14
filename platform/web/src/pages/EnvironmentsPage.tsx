import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, Plus, Trash2 } from "lucide-react";
import {
  api,
  type Environment,
  type EnvironmentCredential,
  type EnvironmentCredentialSource,
  type EnvironmentProviderBinding,
  type EnvironmentTemplate,
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
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
      ) ? REFRESH_MS : false;
    },
  });
  const secrets = useQuery({
    queryKey: ["secrets", universeId],
    queryFn: () =>
      api<SecretsInventory>("GET", `/api/v1/universes/${universeId}/secrets`),
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

  return (
    <>
      <PageHeader
        title="Environments"
        description="Computers and sandboxes agents can use in this universe."
        actions={creatableTemplates.length > 0
          ? <CreateEnvironmentDialog universeId={universeId} templates={creatableTemplates} />
          : undefined}
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
      {bindings.data && environments.data && environmentRows.length === 0 && (
        <div className="rounded-xl border border-dashed px-5 py-8 text-center">
          <p className="text-sm font-medium">No environments provisioned</p>
          <p className="mt-1 text-sm text-muted-foreground">
            Choose a template from an enabled provider binding to create one.
          </p>
        </div>
      )}
      <div className="grid gap-3">
        {environmentRows.map((environment) => (
          <EnvironmentCard
            key={environment.environmentId}
            universeId={universeId}
            environment={environment}
            template={templateRows.find((candidate) =>
              candidate.bindingId === provisionedBindingId(environment)
              && candidate.templateId === environment.incarnation.templateId
            )}
            secrets={secrets.data}
          />
        ))}
      </div>
      {bindingRows.length > 0 && <ProviderBindings bindings={bindingRows} />}
    </>
  );
}

function EnvironmentCard({
  universeId,
  environment,
  template,
  secrets,
}: {
  universeId: string;
  environment: Environment;
  template: EnvironmentTemplate | undefined;
  secrets: SecretsInventory | undefined;
}) {
  const [open, setOpen] = useState(false);
  const source = environment.source;

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
                : "External environment"}
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              {environment.publicEndpoint ?? "Private environment"}
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
              <Detail label="Source" value={source.type === "provisioned" ? "Provisioned" : "External"} />
              {source.type === "provisioned" && <Detail label="Binding" value={source.bindingId} mono />}
              {source.type === "provisioned" && <Detail label="Provider" value={source.providerId} mono />}
              {environment.incarnation.templateId && <Detail label="Template" value={environment.incarnation.templateId} mono />}
              {environment.incarnation.providerTargetId && <Detail label="Target" value={environment.incarnation.providerTargetId} mono />}
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
                {template?.publicIngress && (
                  <EnvironmentIngressButton universeId={universeId} environment={environment} />
                )}
                <CloseEnvironmentButton universeId={universeId} environment={environment} />
              </div>
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

function environmentCredentialOptions(secrets: SecretsInventory | undefined) {
  if (!secrets) return [];
  return [
    ...secrets.grants
      .filter((grant) => grant.status === "active")
      .map((grant) => ({
        value: `grant:${grant.grantId}`,
        label: grant.providerId === "environment-secret"
          ? `${accessGrantName(grant)} · Environment secret`
          : `${accessGrantName(grant)} · Access credential`,
      })),
    ...secrets.providers
      .filter((provider) => provider.status === "active" && provider.hasCredential)
      .map((provider) => ({
        value: `provider:${provider.credentialId}`,
        label: `${modelProviderName(provider)} · Model provider API key`,
      })),
  ];
}

function environmentCredentialSourceFromValue(value: string): EnvironmentCredentialSource {
  if (value.startsWith("grant:")) {
    return { type: "authGrant", grantId: value.slice("grant:".length) };
  }
  if (value.startsWith("provider:")) {
    return { type: "authProviderCredential", providerId: value.slice("provider:".length) };
  }
  throw new Error("invalid environment credential source");
}

function environmentCredentialSourceLabel(
  source: EnvironmentCredentialSource,
  secrets: SecretsInventory | undefined,
): string {
  if (source.type === "authGrant") {
    const grant = secrets?.grants.find((candidate) => candidate.grantId === source.grantId);
    return grant ? accessGrantName(grant) : source.grantId;
  }
  if (source.type === "authProviderCredential") {
    const provider = secrets?.providers.find(
      (candidate) => candidate.credentialId === source.providerId,
    );
    return provider ? modelProviderName(provider) : source.providerId;
  }
  return `Direct secret (${source.secretId})`;
}

function environmentCredentialAvailable(
  source: EnvironmentCredentialSource,
  secrets: SecretsInventory | undefined,
): boolean {
  if (!secrets || source.type === "directSecret") return true;
  if (source.type === "authGrant") {
    return secrets.grants.some(
      (grant) => grant.grantId === source.grantId && grant.status === "active",
    );
  }
  return secrets.providers.some(
    (provider) =>
      provider.credentialId === source.providerId
      && provider.status === "active"
      && provider.hasCredential,
  );
}

function accessGrantName(grant: SecretGrant): string {
  return grant.displayName ?? grant.subjectHint ?? grant.grantId;
}

function modelProviderName(provider: SecretProvider): string {
  return provider.displayName ?? provider.providerId;
}

function EnvironmentStatusBadge({ environment }: { environment: Environment }) {
  if (environment.status === "ready") {
    return <Badge variant="secondary">ready</Badge>;
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
  return (
    <Button
      variant="ghost"
      size="xs"
      disabled={close.isPending}
      onClick={() => close.mutate()}
    >
      {close.isPending ? "Closing…" : "Close"}
    </Button>
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
      });
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["environments", universeId] });
      setOpen(false);
      setDisplayName("");
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
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>Cancel</Button>
            <Button disabled={!selectedTemplate || create.isPending} onClick={() => create.mutate()}>
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
