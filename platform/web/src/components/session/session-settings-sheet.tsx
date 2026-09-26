import { attachedEnvironments, isEnvironmentAttached } from "@/lib/sessions/resource-features";
import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type Environment,
  type SessionInstructionState,
  type SessionView,
} from "@/api";
import {
  MetadataEditor,
  metadataToRows,
  rowsToMetadata,
  sameMetadata,
  type MetadataRow,
} from "@/components/session/metadata-editor";
import { SetupEditorSection } from "@/components/session/setup-editor-section";
import {
  normalizeSessionConfig,
  SessionConfigEditor,
  workspaceAttachmentsError,
  workspaceAttachmentsFromConfig,
  type SessionConfig,
} from "@/components/session/session-config-editor";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
  isActivatableEnvironmentStatus,
  resourceFeatureDisableReasons,
  selectableEnvironments,
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
          onApplied={() => onOpenChange(false)}
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
  onApplied,
}: {
  universeId: string;
  sessionId: string;
  session: SessionView | undefined;
  runActive: boolean;
  enabled: boolean;
  /** Called once the whole setup has applied; the dialog closes then. */
  onApplied: () => void;
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
  const [metadataRows, setMetadataRows] = useState<MetadataRow[]>([]);
  const [originalMetadata, setOriginalMetadata] = useState<Record<string, string>>({});
  const [retentionDaysDraft, setRetentionDaysDraft] = useState("");
  const [originalRetentionDays, setOriginalRetentionDays] = useState("");
  const [configError, setConfigError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const configBase = useRef({ revision: 0, config: undefined as SessionConfig | undefined });
  const metadataKey = JSON.stringify(session?.metadata ?? {});
  const retentionKey = JSON.stringify(session?.retention ?? {});

  useEffect(() => {
    if (!session) return;
    const metadata = session.metadata ?? {};
    setMetadataRows(metadataToRows(metadata));
    setOriginalMetadata(metadata);
  }, [metadataKey]);

  useEffect(() => {
    if (!session) return;
    const duration = session.retention.deleteAfterCloseMs;
    const days = duration == null ? "" : String(duration / 86_400_000);
    setRetentionDaysDraft(days);
    setOriginalRetentionDays(days);
  }, [retentionKey]);

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

  const nextWorkspaceAttachments = workspaceAttachmentsFromConfig(configDraft);
  const configDirty = !sameConfig(configDraft, originalConfig);
  const instructionsDirty = instructionsDraft !== undefined
    && originalInstructions !== undefined
    && instructionsDraft !== originalInstructions;
  const activeEnvironmentDirty = activeEnvironmentDraft !== originalActiveEnvironmentId;
  const metadataDraft = rowsToMetadata(metadataRows);
  const metadataDirty = !sameMetadata(metadataDraft, originalMetadata);
  const retentionDirty = retentionDaysDraft.trim() !== originalRetentionDays;
  const retentionDays = retentionDaysDraft.trim() ? Number(retentionDaysDraft) : null;
  const retentionError = retentionDays !== null
    && (!Number.isFinite(retentionDays) || retentionDays <= 0)
      ? "Automatic deletion must be a positive number of days."
      : null;
  const dirty = configDirty || instructionsDirty || activeEnvironmentDirty || metadataDirty || retentionDirty;
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
      const attachmentError = workspaceAttachmentsError(nextWorkspaceAttachments);
      if (attachmentError) throw new Error(attachmentError);
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

      if (metadataDirty) {
        // A complete map: the put replaces, an empty map clears.
        await api<SessionView>(
          "PUT",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/metadata`,
          { metadata: metadataDraft },
        );
      }

      if (retentionDirty) {
        await api<SessionView>(
          "PUT",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/retention`,
          {
            deleteAfterCloseMs: retentionDays === null
              ? null
              : Math.round(retentionDays * 86_400_000),
          },
        );
      }
    },
    onSuccess: async () => {
      setError(null);
      onApplied();
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
              <h2 className="text-sm font-semibold">Model configuration</h2>
              <p className="text-xs text-muted-foreground">
                Choose the model and its default reasoning behavior. Unset values inherit deployment or provider defaults.
              </p>
            </div>
            <SessionConfigEditor
              value={configDraft}
              onChange={(config) => {
                setConfigDraft(config);
                if (activeEnvironmentDraft && !isEnvironmentAttached(config, activeEnvironmentDraft)) setActiveEnvironmentDraft(null);
              }}
              onValidityChange={setConfigError}
              mcpServers={options.mcpServers}
              workspaces={options.workspaces}
              workspacesLoading={options.workspacesLoading}
              models={options.models}
              profiles={options.profiles}
              environments={options.environments}
              mcpToolDiscovery={options.mcpToolDiscovery}
              featureDisableReasons={resourceFeatureDisableReasons({
                config: configDraft,
              })}
              metadataSetup={(
                <MetadataEditor
                  rows={metadataRows}
                  onChange={setMetadataRows}
                  disabled={save.isPending}
                />
              )}
              metadataDescription="Descriptive key/value pairs for finding this session in the list. Keys are limited to 64 bytes, values to 256 bytes, and the lightspeed. prefix is reserved."
              retentionSetup={session?.retention.rootSessionId === sessionId ? (
                <div className="grid max-w-xs gap-1.5">
                  <label className="text-xs font-medium" htmlFor="session-retention-days">
                    Delete after close (days)
                  </label>
                  <Input
                    id="session-retention-days"
                    type="number"
                    min="0.000001"
                    step="any"
                    value={retentionDaysDraft}
                    onChange={(event) => setRetentionDaysDraft(event.target.value)}
                    placeholder="Keep until manually deleted"
                    disabled={save.isPending}
                  />
                  <p className="text-xs text-muted-foreground">
                    Deletes this session, its history forks, and delegated children after the root closes. Leave blank to keep the tree.
                  </p>
                  {retentionError && <p className="text-xs text-destructive">{retentionError}</p>}
                </div>
              ) : (
                <p className="text-sm text-muted-foreground">
                  Retained with root session{" "}
                  <a className="font-mono underline" href={`../${session?.retention.rootSessionId}`}>
                    {session?.retention.rootSessionId}
                  </a>. Change automatic deletion from the root.
                </p>
              )}
              retentionDescription="Delete the retained session tree automatically after its root session closes."
              environmentSetup={(
                <ActiveEnvironmentEditor
                  embedded
                  value={activeEnvironmentDraft}
                  environments={attachedEnvironments(configDraft, environments.data ?? [])}
                  loading={environments.isLoading}
                  disabled={!hasSessionFeature(configDraft, "environments")}
                  onChange={setActiveEnvironmentDraft}
                />
              )}
              pinnedApiKind={pinnedApiKind || undefined}
            />
          </section>
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
        {retentionError && <p className="text-sm text-destructive">{retentionError}</p>}
        {error && <p className="text-sm text-destructive">{error}</p>}
        <div className="flex justify-end">
          <Button
            disabled={
              !session
              || !dirty
              || Boolean(configError || environmentError || retentionError)
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
  environments: allEnvironments,
  loading,
  disabled,
  embedded = false,
  onChange,
}: {
  value: string | null;
  environments: Environment[];
  loading: boolean;
  disabled: boolean;
  embedded?: boolean;
  onChange: (environmentId: string | null) => void;
}) {
  const none = "__no_active_environment__";
  const environments = selectableEnvironments(allEnvironments, value);
  const ids = [...new Set([
    ...environments.map((environment) => environment.environmentId),
    ...(value ? [value] : []),
  ])];
  const content = (
    <>
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
                disabled={!environment
                  || (!isActivatableEnvironmentStatus(environment.status) && environmentId !== value)}
              >
                {environment
                  ? `${environmentLabel(environment)}${environment.status === "ready" ? "" : ` — ${environment.status}`}`
                  : `${environmentId} (unavailable)`}
              </SelectItem>
            );
          })}
        </SelectContent>
      </Select>
    </>
  );

  if (embedded) {
    return (
      <div className="grid gap-3">
        <div className="grid gap-0.5">
          <p className="text-sm font-medium">Active environment</p>
          <p className="text-xs text-muted-foreground">
            The one universe environment used by process and environment-targeted tools. Selection does not transfer ownership.
          </p>
        </div>
        {content}
      </div>
    );
  }

  return (
    <SetupEditorSection
      title="Active environment"
      description="The one universe environment used by process and environment-targeted tools. Selection does not transfer ownership."
    >
      {content}
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
  if (!isEnvironmentAttached(config, environmentId)) return `Attach environment ${environmentId} before activating it.`;
  const environment = environments.find((candidate) => candidate.environmentId === environmentId);
  if (!environment) {
    return environmentId === originalEnvironmentId
      ? null
      : `Environment is unavailable: ${environmentId}`;
  }
  if (environmentId !== originalEnvironmentId && !isActivatableEnvironmentStatus(environment.status)) {
    return `Environment is not currently selectable: ${environmentId} (${environment.status})`;
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
