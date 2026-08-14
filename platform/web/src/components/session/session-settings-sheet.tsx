import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type Environment,
  type SessionInstructionState,
  type SessionView,
} from "@/api";
import { SetupEditorSection } from "@/components/session/setup-editor-section";
import {
  normalizeSessionConfig,
  SessionConfigEditor,
  workspaceLinksError,
  workspaceLinksFromConfig,
  type SessionConfig,
} from "@/components/session/session-config-editor";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import {
  hasSessionFeature,
  resourceFeatureDisableReasons,
} from "@/lib/sessions/resource-features";
import { managedSessionOwnerLabel } from "@/lib/sessions/management";

export function SessionSettingsDialog({
  universeId,
  sessionId,
  session,
  runActive,
  open,
  onOpenChange,
}: {
  universeId: string;
  sessionId: string;
  session: SessionView | undefined;
  runActive: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const management = session?.management;
  const managed = session?.managed === true;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="h-[min(92dvh,900px)] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 p-0 sm:max-w-5xl">
        <DialogHeader className="border-b p-6 pr-14">
          <DialogTitle>Session settings</DialogTitle>
          <DialogDescription>
            {managed
              ? `Edit configuration directly. Lifecycle and chat delivery remain managed by ${managedSessionOwnerLabel(management)}.`
              : "Edit this live session directly, then apply the setup in one operation."}
          </DialogDescription>
        </DialogHeader>
        <LiveSessionSetup
          universeId={universeId}
          sessionId={sessionId}
          session={session}
          runActive={runActive}
          enabled={open}
        />
      </DialogContent>
    </Dialog>
  );
}

function LiveSessionSetup({
  universeId,
  sessionId,
  session,
  runActive,
  enabled,
}: {
  universeId: string;
  sessionId: string;
  session: SessionView | undefined;
  runActive: boolean;
  enabled: boolean;
}) {
  const queryClient = useQueryClient();
  const options = useSessionConfigEditorOptions(universeId, enabled);
  const instructions = useQuery({
    queryKey: ["session-instructions", universeId, sessionId],
    queryFn: () => api<SessionInstructionState>(
      "GET",
      `/api/v1/universes/${universeId}/sessions/${sessionId}/instructions`,
    ),
    enabled,
  });
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled,
  });
  const [configDraft, setConfigDraft] = useState<SessionConfig | undefined>();
  const [originalConfig, setOriginalConfig] = useState<SessionConfig | undefined>();
  const [instructionsDraft, setInstructionsDraft] = useState<string | undefined>();
  const [originalInstructions, setOriginalInstructions] = useState<string | undefined>();
  const [activeEnvironmentDraft, setActiveEnvironmentDraft] = useState<string | null>(null);
  const [originalActiveEnvironmentId, setOriginalActiveEnvironmentId] = useState<string | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const configBase = useRef({ revision: 0, config: undefined as SessionConfig | undefined });

  useEffect(() => {
    if (!session) return;
    const config = normalizeSessionConfig(session.config);
    setConfigDraft(config);
    setOriginalConfig(config);
    configBase.current = { revision: session.configRevision, config };
    setConfigError(null);
    setError(null);
  }, [session?.configRevision]);

  useEffect(() => {
    if (!session) return;
    const environmentId = session.activeEnvironmentId ?? null;
    setActiveEnvironmentDraft(environmentId);
    setOriginalActiveEnvironmentId(environmentId);
  }, [session?.activeEnvironmentId]);

  useEffect(() => {
    if (!instructions.data) return;
    const text = instructions.data.text ?? "";
    setInstructionsDraft(text);
    setOriginalInstructions(text);
    setError(null);
  }, [instructions.data]);

  const nextWorkspaceLinks = workspaceLinksFromConfig(configDraft);
  const configDirty = !sameConfig(configDraft, originalConfig);
  const instructionsDirty = instructionsDraft !== undefined
    && originalInstructions !== undefined
    && instructionsDraft !== originalInstructions;
  const activeEnvironmentDirty = activeEnvironmentDraft !== originalActiveEnvironmentId;
  const dirty = configDirty || instructionsDirty || activeEnvironmentDirty;
  const pinnedApiKind = stringField(record(session?.config).model, "apiKind");
  const environmentError = activeEnvironmentSelectionError(
    configDraft,
    activeEnvironmentDraft,
    originalActiveEnvironmentId,
    environments.data ?? [],
  );

  const save = useMutation({
    mutationFn: async () => {
      if (!session) throw new Error("Session is still loading.");
      const linkError = workspaceLinksError(nextWorkspaceLinks);
      if (linkError) throw new Error(linkError);
      const desiredConfig = normalizeSessionConfig(configDraft) ?? {};
      const selectionError = activeEnvironmentSelectionError(
        desiredConfig,
        activeEnvironmentDraft,
        originalActiveEnvironmentId,
        environments.data ?? [],
      );
      if (selectionError) throw new Error(selectionError);

      const putConfig = async (config: SessionConfig) => {
        if (sameConfig(config, configBase.current.config)) return;
        const updated = await api<SessionView>(
          "PUT",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/config`,
          {
            config,
            expectedConfigRevision: configBase.current.revision,
          },
        );
        configBase.current = {
          revision: updated.configRevision,
          config: normalizeSessionConfig(updated.config),
        };
      };

      if (activeEnvironmentDirty && originalActiveEnvironmentId) {
        await api<SessionView>(
          "POST",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/environments/deactivate`,
          {},
        );
      }

      await putConfig(desiredConfig);

      if (activeEnvironmentDirty && activeEnvironmentDraft) {
        await api<SessionView>(
          "POST",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/environments/${encodeURIComponent(activeEnvironmentDraft)}/activate`,
          {},
        );
      }

      if (instructionsDirty) {
        await api<SessionView>(
          "PUT",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/instructions`,
          { text: instructionsDraft?.trim() ? instructionsDraft : null },
        );
      }

    },
    onSuccess: async () => {
      setError(null);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["session-instructions", universeId, sessionId] }),
        queryClient.invalidateQueries({ queryKey: ["environments", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["session", universeId, sessionId] }),
      ]);
    },
    onError: async (err) => {
      setError(err.message);
      await queryClient.invalidateQueries({ queryKey: ["session", universeId, sessionId] });
    },
  });

  return (
    <>
      <div className="min-h-0 overflow-y-auto p-6">
        <div className="grid gap-8">
          <section className="grid gap-3">
            <div className="grid gap-0.5">
              <h2 className="text-sm font-semibold">Custom instructions</h2>
              <p className="text-xs text-muted-foreground">
                Session-local instructions reconciled with the product default and workspace-linked VFS prompts.
              </p>
            </div>
            {instructions.isLoading ? (
              <p className="text-sm text-muted-foreground">Loading instructions…</p>
            ) : (
              <textarea
                className="min-h-32 w-full resize-y rounded-lg border border-input bg-transparent p-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
                value={instructionsDraft ?? ""}
                onChange={(event) => setInstructionsDraft(event.target.value)}
                placeholder="No custom instructions — the managed fallback applies."
                spellCheck={false}
              />
            )}
            <ActiveInstructionSources value={instructions.data} />
          </section>
          <section className="grid gap-3">
            <div className="grid gap-0.5">
              <h2 className="text-sm font-semibold">Session config</h2>
              <p className="text-xs text-muted-foreground">
                Sparse behavior and capability grants. Enable a resource capability before editing its setup.
              </p>
            </div>
            <SessionConfigEditor
              value={configDraft}
              onChange={(config) => {
                setConfigDraft(config);
                if (!hasSessionFeature(config, "environments")) setActiveEnvironmentDraft(null);
              }}
              onValidityChange={setConfigError}
              mcpServers={options.mcpServers}
              workspaces={options.workspaces}
              workspacesLoading={options.workspacesLoading}
              models={options.models}
              profiles={options.profiles}
              environmentProviders={options.environmentProviders}
              featureDisableReasons={resourceFeatureDisableReasons({
                config: configDraft,
              })}
              pinnedApiKind={pinnedApiKind || undefined}
            />
          </section>
          <ActiveEnvironmentEditor
            value={activeEnvironmentDraft}
            environments={environments.data ?? []}
            loading={environments.isLoading}
            disabled={!hasSessionFeature(configDraft, "environments")}
            onChange={setActiveEnvironmentDraft}
          />
          {environments.error && <p className="text-sm text-destructive">{environments.error.message}</p>}
          {instructions.error && <p className="text-sm text-destructive">{instructions.error.message}</p>}
        </div>
      </div>
      <div className="grid gap-2 border-t p-4">
        {runActive && (
          <p className="text-sm text-muted-foreground">
            Session setup can be applied after the active run finishes.
          </p>
        )}
        {environmentError && <p className="text-sm text-destructive">{environmentError}</p>}
        {error && <p className="text-sm text-destructive">{error}</p>}
        <div className="flex justify-end">
          <Button
            disabled={
              !session
              || !dirty
              || Boolean(configError || environmentError)
              || runActive
              || save.isPending
            }
            onClick={() => save.mutate()}
          >
            {save.isPending ? "Applying setup…" : "Apply setup"}
          </Button>
        </div>
      </div>
    </>
  );
}

function ActiveInstructionSources({ value }: { value: SessionInstructionState | undefined }) {
  const sources = (value?.active ?? []).filter(
    (entry) => entry.key !== "instructions.050.profile",
  );
  if (!sources.length) return null;
  return (
    <div className="rounded-lg border bg-muted/30 p-3">
      <p className="text-xs font-medium">Other active instruction sources</p>
      <ul className="mt-2 grid gap-1.5 text-xs text-muted-foreground">
        {sources.map((source) => (
          <li key={`${source.key}:${source.contentRef}`} className="flex min-w-0 gap-2">
            <span className="truncate">
              {instructionSourceLabel(source.key, source.preview)}
            </span>
            {source.key && (
              <code className="ml-auto shrink-0 font-mono text-[11px]">{source.key}</code>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function instructionSourceLabel(key: string | null, preview: string | null): string {
  if (key === "instructions.000.default") return "Product default";
  if (key?.startsWith("instructions.100.prompts")) return preview ?? "Workspace-linked VFS prompt";
  return preview ?? key ?? "Managed instructions";
}

function ActiveEnvironmentEditor({
  value,
  environments,
  loading,
  disabled,
  onChange,
}: {
  value: string | null;
  environments: Environment[];
  loading: boolean;
  disabled: boolean;
  onChange: (environmentId: string | null) => void;
}) {
  const none = "__no_active_environment__";
  const ids = [...new Set([
    ...environments.map((environment) => environment.environmentId),
    ...(value ? [value] : []),
  ])];
  return (
    <SetupEditorSection
      title="Active environment"
      description="The one universe environment used by process and environment-targeted tools. Selection does not transfer ownership."
    >
      {disabled && (
        <p className="rounded-lg border border-dashed p-3 text-sm text-muted-foreground">
          Enable the Environments feature to select an active environment.
        </p>
      )}
      <Select
        value={value ?? none}
        disabled={disabled || loading}
        onValueChange={(environmentId) => onChange(environmentId === none ? null : environmentId as string)}
      >
        <SelectTrigger className="w-full">
          <SelectValue>
            {(environmentId: string) => {
              if (environmentId === none) return "No active environment";
              const environment = environments.find((candidate) => candidate.environmentId === environmentId);
              return environment ? environmentLabel(environment) : `${environmentId} (unavailable)`;
            }}
          </SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={none}>No active environment</SelectItem>
          {ids.map((environmentId) => {
            const environment = environments.find((candidate) => candidate.environmentId === environmentId);
            return (
              <SelectItem
                key={environmentId}
                value={environmentId}
                disabled={!environment || (environment.status !== "ready" && environmentId !== value)}
              >
                {environment
                  ? `${environmentLabel(environment)}${environment.status === "ready" ? "" : ` — ${environment.status}`}`
                  : `${environmentId} (unavailable)`}
              </SelectItem>
            );
          })}
        </SelectContent>
      </Select>
    </SetupEditorSection>
  );
}

function activeEnvironmentSelectionError(
  config: SessionConfig | undefined,
  environmentId: string | null,
  originalEnvironmentId: string | null,
  environments: Environment[],
): string | null {
  if (!environmentId) return null;
  if (!hasSessionFeature(config, "environments")) {
    return "An active environment requires the Environments feature.";
  }
  const environment = environments.find((candidate) => candidate.environmentId === environmentId);
  if (!environment) {
    return environmentId === originalEnvironmentId
      ? null
      : `Environment is unavailable: ${environmentId}`;
  }
  if (environmentId !== originalEnvironmentId && environment.status !== "ready") {
    return `Environment is not currently selectable: ${environmentId} (${environment.status})`;
  }
  const allowedProviders = record(record(record(config).features).environments).providers;
  const providerId = environment.source.type === "provisioned"
    ? environment.source.providerId
    : null;
  if (Array.isArray(allowedProviders) && allowedProviders.length
    && (!providerId || !allowedProviders.includes(providerId))) {
    return providerId
      ? `Environment provider is not allowed by the session config: ${providerId}`
      : "External environments are not allowed by this provider-restricted session config.";
  }
  return null;
}

function environmentLabel(environment: Environment): string {
  return `${environment.displayName
    ?? environment.incarnation.templateId
    ?? environment.incarnation.providerTargetId
    ?? environment.environmentId} (${environment.environmentId})`;
}

function sameConfig(left: SessionConfig | undefined, right: SessionConfig | undefined): boolean {
  return JSON.stringify(normalizeSessionConfig(left) ?? {})
    === JSON.stringify(normalizeSessionConfig(right) ?? {});
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function stringField(value: unknown, key: string): string {
  const field = record(value)[key];
  return typeof field === "string" ? field : "";
}
