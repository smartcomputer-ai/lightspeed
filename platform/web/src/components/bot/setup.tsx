import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, ChevronRight, SlidersHorizontal } from "lucide-react";
import { Link, useNavigate } from "react-router-dom";
import {
  api,
  botLabel,
  type BotCloseResponse,
  type BotDeleteResponse,
  type BotInput,
  type BotListItem,
  type BotListResponse,
  type BotStateView,
  type BotTriggerView,
  type BotView,
  type Environment,
  type ProfileDocument,
  type ProfileEnvironment,
  type ProfileSummary,
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
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { ProfileEnvironmentEditor } from "@/components/session/profile-environment-editor";
import { MetadataMapEditor } from "@/components/session/metadata-editor";
import { ProfileRetentionEditor } from "@/components/session/profile-retention-editor";
import { SessionConfigEditor } from "@/components/session/session-config-editor";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import {
  hasSessionFeature,
  resourceFeatureDisableReasons,
  setupResourceFeatureError,
} from "@/lib/sessions/resource-features";
import { cn } from "@/lib/utils";
import { BotEnvironmentCard } from "./environment-card";
import { BotAvatar } from "./face";
import { botInputOf } from "./identity";
import { mergeSessionProfileFields, type SessionProfileFields } from "./session-profile";
import { briefSummary, capabilitySummary, environmentSummary, guardrailsSummary, otherBotsSummary } from "./setup-summary";
import { triggerSummary } from "./trigger-summary";
import {
  BotMultiSelect,
  SavedTriggerFormCard,
  TriggersSection,
  inboxSelectionSpec,
  triggerInputOf,
  useChannelAccounts,
  type BotEnvStatus,
} from "./triggers";

/**
 * Everything about how a bot works, as a stack of sections that each read
 * as one line when closed — so the modal at a glance is a summary, and you
 * open only what you are editing. Each section saves on its own.
 */
export function BotSetup({
  universeId,
  slug,
  bot,
  state,
  manage,
}: {
  universeId: string;
  slug: string;
  bot: BotView;
  state?: BotStateView;
  manage: boolean;
}) {
  const profile = useQuery({
    queryKey: ["profile", universeId, bot.profileId],
    queryFn: () =>
      api<ProfileDocument>(
        "GET",
        `/api/v1/universes/${universeId}/profiles/${encodeURIComponent(bot.profileId)}`,
      ),
    staleTime: 0,
    retry: false,
  });
  const triggers = useQuery({
    queryKey: ["bot-triggers", universeId, bot.botId],
    queryFn: () =>
      api<{ triggers?: BotTriggerView[] }>("GET", `/api/v1/universes/${universeId}/bots/${bot.botId}/triggers`),
  });
  const accounts = useChannelAccounts(universeId);
  const env: BotEnvStatus =
    profile.isLoading || profile.isError || profile.data === undefined
      ? { kind: "unknown" }
      : profile.data.environment == null
        ? { kind: "none" }
        : profile.data.environment.type === "existing"
          ? { kind: "existing", environmentId: profile.data.environment.environmentId }
          : { kind: "none" };
  const triggerList = triggers.data?.triggers ?? [];
  const wakeups = triggerList.filter((trigger) => trigger.kind !== "bot");
  const triggersLine =
    wakeups.length === 0
      ? "Nothing wakes it yet — you can always message it"
      : `${wakeups.length} ${wakeups.length === 1 ? "trigger" : "triggers"} · ${wakeups
          .slice(0, 2)
          .map((trigger) => `${trigger.triggerId}: ${triggerSummary(trigger, accounts.data?.accounts ?? [])}`)
          .join(" · ")}${wakeups.length > 2 ? " · …" : ""}`;

  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-x-hidden overflow-y-auto">
      <div className="grid w-full min-w-0 gap-3 px-4 py-5 text-sm md:px-6">
        <IdentitySection universeId={universeId} bot={bot} manage={manage} />
        <BriefSection universeId={universeId} bot={bot} manage={manage} />
        <SetupSection
          id="triggers"
          title="Triggers"
          description="When this bot wakes up: schedules, webhooks, polls, chats, other bots."
          summary={triggersLine}
          defaultOpen
        >
          <TriggersSection
            universeId={universeId}
            botId={bot.botId}
            manage={manage}
            env={env}
            headless
            hideKinds={["bot"]}
          />
        </SetupSection>
        <SessionProfileSection
          universeId={universeId}
          slug={slug}
          bot={bot}
          manage={manage}
          profile={profile.data}
          profileError={profile.error?.message}
        />
        <OtherBotsSection
          universeId={universeId}
          bot={bot}
          manage={manage}
          inbox={triggerList.find((trigger) => trigger.kind === "bot")}
        />
        <GuardrailsSection universeId={universeId} bot={bot} state={state} manage={manage} />
        <DangerSection universeId={universeId} slug={slug} bot={bot} manage={manage} />
      </div>
    </div>
  );
}

function SetupSection({
  id,
  title,
  description,
  summary,
  defaultOpen = false,
  actions,
  tone,
  children,
}: {
  id: string;
  title: string;
  description: string;
  /** One line shown while closed: what is set, not what the section is. */
  summary: string;
  defaultOpen?: boolean;
  actions?: React.ReactNode;
  tone?: "danger";
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(() => defaultOpen || window.location.hash === `#${id}`);
  useEffect(() => {
    if (window.location.hash !== `#${id}`) return;
    setOpen(true);
    document.getElementById(id)?.scrollIntoView({ block: "start" });
  }, [id]);
  return (
    <section
      id={id}
      className={cn("min-w-0 max-w-full scroll-mt-4 rounded-lg border bg-card", tone === "danger" && "border-destructive/40")}
    >
      <div className="flex min-w-0 items-center gap-2 pr-3">
        <button
          type="button"
          onClick={() => setOpen((value) => !value)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-3 px-4 py-3 text-left"
        >
          <ChevronRight
            className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")}
          />
          <span className="min-w-0 flex-1">
            <span className={cn("block text-sm font-semibold", tone === "danger" && "text-destructive")}>{title}</span>
            <span className="block truncate text-xs text-muted-foreground" title={open ? undefined : summary}>
              {open ? description : summary}
            </span>
          </span>
        </button>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      {open && <div className="grid min-w-0 gap-4 border-t px-4 py-4">{children}</div>}
    </section>
  );
}

/// The core replaces the bot document whole; each save merges its fields
/// over the current record and puts with the record's revision.
function useBotPut(universeId: string, bot: BotView) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (fields: Partial<BotInput>) =>
      api<{ bot: BotView }>("PUT", `/api/v1/universes/${universeId}/bots/${bot.botId}`, {
        bot: { ...botInputOf(bot), ...fields },
        expectedRevision: bot.revision,
      }),
    onSuccess: async ({ bot: updated }) => {
      queryClient.setQueryData(["bot", universeId, bot.botId], { bot: updated });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["bots", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] }),
      ]);
    },
  });
}

function SaveRow({
  dirty,
  pending,
  error,
  onSave,
  disabled = false,
  note,
}: {
  dirty: boolean;
  pending: boolean;
  error: string | null | undefined;
  onSave: () => void;
  disabled?: boolean;
  note?: string;
}) {
  return (
    <div className="flex flex-wrap items-center gap-3">
      <Button size="sm" disabled={!dirty || pending || disabled} onClick={onSave}>
        {pending ? "Saving…" : dirty ? "Save" : "Saved"}
      </Button>
      {note && !error && <span className="text-xs text-muted-foreground">{note}</span>}
      {error && <span className="text-xs text-destructive">{error}</span>}
    </div>
  );
}

function IdentitySection({ universeId, bot, manage }: { universeId: string; bot: BotView; manage: boolean }) {
  const [displayName, setDisplayName] = useState(bot.displayName ?? "");
  const [description, setDescription] = useState(bot.description ?? "");
  useEffect(() => {
    setDisplayName(bot.displayName ?? "");
    setDescription(bot.description ?? "");
  }, [bot.displayName, bot.description]);
  const save = useBotPut(universeId, bot);
  const dirty = displayName.trim() !== (bot.displayName ?? "") || description.trim() !== (bot.description ?? "");
  return (
    <SetupSection
      id="identity"
      title="Identity"
      description="How people and other bots know it."
      summary={`${botLabel(bot)} · ${bot.botId}${bot.description ? ` · ${bot.description}` : ""}`}
    >
      <div className="grid gap-4 sm:grid-cols-[auto_minmax(0,1fr)]">
        <BotAvatar botId={bot.botId} size={56} className="rounded-xl" />
        <div className="grid gap-4">
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="bot-display-name">Name</FieldLabel>
              <Input
                id="bot-display-name"
                value={displayName}
                onChange={(event) => setDisplayName(event.target.value)}
                placeholder={bot.botId}
                disabled={!manage}
              />
            </Field>
            <Field>
              <FieldLabel>Id</FieldLabel>
              <Input value={bot.botId} readOnly className="font-mono" />
              <FieldDescription>What other bots, briefs, and URLs use. It cannot change.</FieldDescription>
            </Field>
          </div>
          <Field>
            <FieldLabel htmlFor="bot-description">Description</FieldLabel>
            <Input
              id="bot-description"
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="One line other bots read when deciding whether to message this bot."
              disabled={!manage}
            />
          </Field>
          {manage && (
            <SaveRow
              dirty={dirty}
              pending={save.isPending}
              error={save.error?.message}
              onSave={() =>
                save.mutate({
                  displayName: displayName.trim() ? displayName.trim() : null,
                  description: description.trim() ? description.trim() : null,
                })
              }
            />
          )}
        </div>
      </div>
    </SetupSection>
  );
}

function BriefSection({ universeId, bot, manage }: { universeId: string; bot: BotView; manage: boolean }) {
  const [brief, setBrief] = useState(bot.brief ?? "");
  useEffect(() => setBrief(bot.brief ?? ""), [bot.brief]);
  const save = useBotPut(universeId, bot);
  const dirty = brief.trim() !== (bot.brief ?? "");
  const closed = bot.closedAtMs != null;
  return (
    <SetupSection
      id="brief"
      title="Brief"
      description="The job. Sent with every event, after the profile's base instructions."
      summary={briefSummary(bot.brief)}
      defaultOpen
    >
      <Textarea
        value={brief}
        onChange={(event) => setBrief(event.target.value)}
        rows={10}
        placeholder="What this bot is for, how it should behave, what good work looks like."
        disabled={!manage || closed}
        className="leading-relaxed"
      />
      {manage && (
        <SaveRow
          dirty={dirty}
          pending={save.isPending}
          error={save.error?.message}
          disabled={closed}
          onSave={() => save.mutate({ brief: brief.trim() ? brief.trim() : null })}
          note={
            bot.selfConfig
              ? "The bot can rewrite this itself when asked in a conversation; changes reach it at its next idle moment."
              : "Changes reach the bot at its next idle moment."
          }
        />
      )}
    </SetupSection>
  );
}

type ProfileSaveRequest = {
  fields: SessionProfileFields;
};

/**
 * All session setup lives in the bot's profile, one document with one
 * revision. Saving overlays only edited fields onto the latest document;
 * the core then reconciles bots onto the new profile revision on its own.
 */
function SessionProfileSection({
  universeId,
  slug,
  bot,
  manage,
  profile,
  profileError,
}: {
  universeId: string;
  slug: string;
  bot: BotView;
  manage: boolean;
  profile: ProfileDocument | undefined;
  profileError: string | undefined;
}) {
  const queryClient = useQueryClient();
  const profileUrl = `/api/v1/universes/${universeId}/profiles/${encodeURIComponent(bot.profileId)}`;
  const options = useSessionConfigEditorOptions(universeId);
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
  });
  const bots = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/v1/universes/${universeId}/bots`),
  });
  const sharedWith = (bots.data?.bots ?? []).filter(
    (other) => other.botId !== bot.botId && other.profileId === bot.profileId && other.closedAtMs == null,
  );

  const [configDraft, setConfigDraft] = useState<Record<string, unknown> | undefined>();
  const [instructionsDraft, setInstructionsDraft] = useState("");
  const [environmentDraft, setEnvironmentDraft] = useState<ProfileEnvironment | undefined>();
  const [metadataDraft, setMetadataDraft] = useState<Record<string, string> | undefined>();
  const [retentionDraft, setRetentionDraft] = useState<number | undefined>();
  const [configError, setConfigError] = useState<string | null>(null);
  const [retentionError, setRetentionError] = useState<string | null>(null);
  const revision = profile?.revision;
  useEffect(() => {
    if (!profile) return;
    setConfigDraft(profile.config ? structuredClone(profile.config as Record<string, unknown>) : undefined);
    const instructions = profile.instructions as { type: "text"; text: string } | { type: "textRef" } | undefined;
    setInstructionsDraft(instructions?.type === "text" ? instructions.text : "");
    setEnvironmentDraft(profile.environment ?? undefined);
    setMetadataDraft(profile.metadata ? structuredClone(profile.metadata) : undefined);
    setRetentionDraft(profile.retention?.deleteAfterCloseMs);
    // Re-sync on a new revision only; an unrelated refetch must not wipe edits.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revision, bot.profileId]);

  const profileConfig = profile?.config as Record<string, unknown> | undefined;
  const configDirty = profile !== undefined
    && JSON.stringify(configDraft ?? null) !== JSON.stringify(profileConfig ?? null);
  const baseInstructions = (profile?.instructions as { type?: string; text?: string } | undefined)?.type === "text"
    ? ((profile?.instructions as { text: string }).text ?? "")
    : "";
  const instructionsDirty = profile !== undefined && instructionsDraft !== baseInstructions;
  const environmentDirty =
    profile !== undefined && JSON.stringify(environmentDraft ?? null) !== JSON.stringify(profile.environment ?? null);
  const metadataDirty =
    profile !== undefined && JSON.stringify(metadataDraft ?? null) !== JSON.stringify(profile.metadata ?? null);
  const retentionDirty =
    profile !== undefined && retentionDraft !== profile.retention?.deleteAfterCloseMs;

  const save = useMutation({
    mutationFn: async ({ fields }: ProfileSaveRequest) => {
      const latest = await api<ProfileDocument>("GET", profileUrl);
      const next = mergeSessionProfileFields(latest, fields);
      const problem = setupResourceFeatureError(next);
      if (problem) throw new Error(problem);
      await api("PUT", profileUrl, next);
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["profile", universeId, bot.profileId] }),
        queryClient.invalidateQueries({ queryKey: ["profiles", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] }),
      ]);
    },
  });
  const merged = { ...(profile ?? {}), config: configDraft, environment: environmentDraft };
  const closed = bot.closedAtMs != null;
  const readOnly = !manage || closed;
  const capabilities = capabilitySummary(profileConfig);
  const environment = hasSessionFeature(profileConfig, "environments")
    ? environmentSummary(profile?.environment, environments.data)
    : null;
  const textRef = (profile?.instructions as { type?: string } | undefined)?.type === "textRef";

  return (
    <SetupSection
      id="session-profile"
      title="Session profile"
      description="The model, instructions, tools, environment, metadata, and retention applied to this bot's sessions."
      summary={profileError
        ? `Profile ${bot.profileId} could not be read`
        : [
            capabilities.length > 0 ? capabilities.join(" · ") : "Default model, no tools",
            environment,
            `profile ${bot.profileId}`,
          ].filter(Boolean).join(" · ")}
    >
      <ProfileSwitcher universeId={universeId} bot={bot} slug={slug} manage={manage && !closed} sharedWith={sharedWith} />
      {profileError && (
        <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">
          Profile <code>{bot.profileId}</code> could not be read: {profileError}
        </p>
      )}
      {profile && (
        <>
            <Field>
              <FieldLabel htmlFor="bot-base-instructions">Base instructions</FieldLabel>
              <Textarea
                id="bot-base-instructions"
                value={instructionsDraft}
                onChange={(event) => setInstructionsDraft(event.target.value)}
                rows={4}
                placeholder="Usually empty for a bot: the brief above is its job. Use this for a system prompt the profile should carry on its own."
                disabled={readOnly || textRef}
              />
              {textRef && <FieldDescription>Stored as a blob reference; edit it on the Profiles page.</FieldDescription>}
            </Field>
            <SessionConfigEditor
              value={configDraft}
              mcpServers={options.mcpServers}
              workspaces={options.workspaces}
              workspacesLoading={options.workspacesLoading}
              models={options.models}
              profiles={options.profiles}
              environmentProviders={options.environmentProviders}
              featureDisableReasons={resourceFeatureDisableReasons(merged)}
              environmentSetup={(
                <div className="grid gap-4">
                  <ProfileEnvironmentEditor
                    embedded
                    value={environmentDraft}
                    environments={environments.data}



                    disabled={readOnly}
                    description="Choose an existing environment shared by this bot's sessions. Manage its lifecycle on the Environments page."
                    onChange={setEnvironmentDraft}
                  />
                  {environmentDraft?.type === "existing" && (
                    <BotEnvironmentCard
                      slug={slug}
                      universeId={universeId}
                      environmentId={environmentDraft.environmentId}
                      manage={manage}
                    />
                  )}
                </div>
              )}
              metadataSetup={(
                <MetadataMapEditor value={metadataDraft} onChange={setMetadataDraft} disabled={readOnly} />
              )}
              metadataDescription="Defaults copied to every session this bot creates. Metadata helps with filtering and does not affect runtime behavior."
              retentionSetup={(
                <ProfileRetentionEditor
                  value={retentionDraft}
                  onChange={setRetentionDraft}
                  onValidityChange={setRetentionError}
                  disabled={readOnly}
                />
              )}
              retentionDescription="Default automatic deletion for each new root session this bot creates."
              onValidityChange={setConfigError}
              onChange={(config) => setConfigDraft(config as Record<string, unknown> | undefined)}
            />
            {manage && (
              <SaveRow
                dirty={configDirty || instructionsDirty || environmentDirty || metadataDirty || retentionDirty}
                pending={save.isPending}
                error={configError ? `Config: ${configError}` : retentionError ? `Retention: ${retentionError}` : save.error?.message}
                disabled={closed || configError !== null || retentionError !== null}
                onSave={() =>
                  save.mutate({
                    fields: {
                      ...(configDirty ? { config: configDraft } : {}),
                      ...(instructionsDirty
                        ? {
                            instructions: instructionsDraft.trim()
                              ? { type: "text", text: instructionsDraft }
                              : undefined,
                          }
                        : {}),
                      ...(metadataDirty ? { metadata: metadataDraft } : {}),
                      ...(retentionDirty
                        ? { retention: retentionDraft === undefined ? undefined : { deleteAfterCloseMs: retentionDraft } }
                        : {}),
                      ...(environmentDirty ? { environment: environmentDraft } : {}),
                    },
                  })
                }
                note="Applies to Main at its next idle moment; open threads keep their setup until they close."
              />
            )}
        </>
      )}
    </SetupSection>
  );
}

function ProfileSwitcher({
  universeId,
  bot,
  slug,
  manage,
  sharedWith,
}: {
  universeId: string;
  bot: BotView;
  slug: string;
  manage: boolean;
  sharedWith: BotListItem[];
}) {
  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
  });
  const [selected, setSelected] = useState(bot.profileId);
  useEffect(() => setSelected(bot.profileId), [bot.profileId]);
  const patch = useBotPut(universeId, bot);
  const known = profiles.data?.some((profile) => profile.profileId === bot.profileId) ?? true;
  return (
    <Field>
      <FieldLabel htmlFor="bot-profile">Profile</FieldLabel>
      <div className="flex flex-wrap items-center gap-2">
        <Select value={selected} onValueChange={(value) => value && setSelected(value)} disabled={!manage}>
          <SelectTrigger id="bot-profile" className="w-64">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {!known && <SelectItem value={bot.profileId}>{bot.profileId}</SelectItem>}
            {profiles.data?.map((profile) => (
              <SelectItem key={profile.profileId} value={profile.profileId}>
                {profile.displayName ?? profile.profileId}
                {profile.displayName && profile.displayName !== profile.profileId ? ` · ${profile.profileId}` : ""}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {selected !== bot.profileId ? (
          <>
            <Button size="sm" disabled={patch.isPending} onClick={() => patch.mutate({ profileId: selected })}>
              {patch.isPending ? "Switching…" : "Use this profile"}
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setSelected(bot.profileId)}>
              Cancel
            </Button>
          </>
        ) : (
          <Button
            variant="ghost"
            size="sm"
            nativeButton={false} render={<Link to={`/u/${slug}/profiles/${encodeURIComponent(bot.profileId)}`} />}
          >
            Open on the Profiles page <ArrowUpRight data-icon="inline-end" />
          </Button>
        )}
      </div>
      <FieldDescription>
        {patch.error ? (
          <span className="text-destructive">{patch.error.message}</span>
        ) : sharedWith.length > 0 ? (
          <>
            Shared: {sharedWith.map(botLabel).join(", ")} use{sharedWith.length === 1 ? "s" : ""} this profile too, so
            changes below reach {sharedWith.length === 1 ? "it" : "them"} as well.
          </>
        ) : (
          "Everything below belongs to this profile; switching profiles swaps its instructions, model, tools, environment, metadata, and retention at the bot's next idle moment."
        )}
      </FieldDescription>
    </Field>
  );
}

function GuardrailsSection({
  universeId,
  bot,
  state,
  manage,
}: {
  universeId: string;
  bot: BotView;
  state?: BotStateView;
  manage: boolean;
}) {
  const controller = state?.controller ?? undefined;
  const [runsPerDay, setRunsPerDay] = useState(bot.runsPerDay?.toString() ?? "");
  const [breakerFires, setBreakerFires] = useState(bot.breaker?.fires.toString() ?? "");
  const [breakerWindow, setBreakerWindow] = useState(bot.breaker ? String(Math.round(bot.breaker.windowMs / 60_000)) : "");
  const [closeAfterDays, setCloseAfterDays] = useState(bot.routedSessionCloseAfterMs ? String(Math.round(bot.routedSessionCloseAfterMs / 86_400_000)) : "");
  const [selfConfig, setSelfConfig] = useState(bot.selfConfig ?? false);
  useEffect(() => {
    setRunsPerDay(bot.runsPerDay?.toString() ?? "");
    setBreakerFires(bot.breaker?.fires.toString() ?? "");
    setBreakerWindow(bot.breaker ? String(Math.round(bot.breaker.windowMs / 60_000)) : "");
    setCloseAfterDays(bot.routedSessionCloseAfterMs ? String(Math.round(bot.routedSessionCloseAfterMs / 86_400_000)) : "");
    setSelfConfig(bot.selfConfig ?? false);
  }, [bot.runsPerDay, bot.breaker, bot.routedSessionCloseAfterMs, bot.selfConfig]);

  const fields = () => ({
    runsPerDay: runsPerDay.trim() ? Number(runsPerDay) : null,
    breaker: breakerFires.trim()
      ? { fires: Number(breakerFires), windowMs: Math.round(Number(breakerWindow.trim() || "10") * 60_000) }
      : null,
    routedSessionCloseAfterMs: closeAfterDays.trim() ? Math.round(Number(closeAfterDays) * 86_400_000) : null,
    selfConfig,
  });
  const botDirty =
    JSON.stringify(fields()) !==
    JSON.stringify({
      runsPerDay: bot.runsPerDay ?? null,
      breaker: bot.breaker ?? null,
      routedSessionCloseAfterMs: bot.routedSessionCloseAfterMs ?? null,
      selfConfig: bot.selfConfig ?? false,
    });
  const problem = closeAfterDays.trim() && !(Number(closeAfterDays) >= 1) ? "Idle close must be at least one day." : null;

  const save = useBotPut(universeId, bot);
  const closed = bot.closedAtMs != null;
  const readOnly = !manage || closed;
  const usedToday = (controller?.runsToday ?? 0) + (controller?.descendantsToday ?? 0);

  return (
    <SetupSection
      id="guardrails"
      title="Guardrails"
      description="Limits and permissions."
      summary={guardrailsSummary(bot)}
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <Field>
          <FieldLabel htmlFor="bot-runs-per-day">Daily run limit</FieldLabel>
          <Input
            id="bot-runs-per-day"
            type="number"
            min={1}
            value={runsPerDay}
            onChange={(event) => setRunsPerDay(event.target.value)}
            placeholder="No limit"
            disabled={readOnly}
          />
          <FieldDescription>
            {controller ? `${usedToday} used today. ` : ""}Runs and sub-agents count; events beyond the limit wait for the
            next UTC day.
          </FieldDescription>
        </Field>
        <Field>
          <FieldLabel htmlFor="bot-close-days">Close inactive threads after (days)</FieldLabel>
          <Input
            id="bot-close-days"
            type="number"
            min={1}
            value={closeAfterDays}
            onChange={(event) => setCloseAfterDays(event.target.value)}
            placeholder="Keep forever"
            disabled={readOnly}
          />
          <FieldDescription>
            Threads idle this long are closed; a later event for the same key opens a fresh one. Triggers can
            override it.
          </FieldDescription>
        </Field>
        <div className="grid gap-3 sm:col-span-2 sm:grid-cols-[1fr_1fr]">
          <Field>
            <FieldLabel htmlFor="bot-breaker-fires">Flood protection: events</FieldLabel>
            <Input
              id="bot-breaker-fires"
              type="number"
              min={1}
              value={breakerFires}
              onChange={(event) => setBreakerFires(event.target.value)}
              placeholder="Off"
              disabled={readOnly}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="bot-breaker-window">per window (minutes)</FieldLabel>
            <Input
              id="bot-breaker-window"
              type="number"
              min={1}
              value={breakerWindow}
              onChange={(event) => setBreakerWindow(event.target.value)}
              placeholder="10"
              disabled={readOnly}
            />
          </Field>
          <p className="-mt-1 text-xs text-muted-foreground sm:col-span-2">
            A trigger exceeding this rate pauses itself until someone resumes it.
          </p>
        </div>
      </div>
      <div className="grid gap-2">
        <ToggleRow
          id="bot-self-config"
          label="Can change its own brief and triggers"
          hint="Ask it in a conversation to add a schedule or rewrite its job. Off: it can only look."
          checked={selfConfig}
          onCheckedChange={setSelfConfig}
          disabled={readOnly}
        />
      </div>
      {manage && (
        <SaveRow
          dirty={botDirty}
          pending={save.isPending}
          error={problem ?? save.error?.message}
          disabled={closed || problem !== null}
          onSave={() => save.mutate(fields())}
          note="Grants change the bot's toolset; its sessions pick that up at their next idle moment."
        />
      )}
    </SetupSection>
  );
}

type InboxMode = "off" | "any" | "selected";

/**
 * Talking to other bots, both directions in one place: sending is a grant
 * on this bot (`emit`); receiving is its inbox — a trigger under the hood,
 * with the routing and batching of one, shown here in the person's words.
 */
function OtherBotsSection({
  universeId,
  bot,
  manage,
  inbox,
}: {
  universeId: string;
  bot: BotView;
  manage: boolean;
  inbox: BotTriggerView | undefined;
}) {
  const queryClient = useQueryClient();
  const bots = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/v1/universes/${universeId}/bots`),
  });
  const inboxFrom = inbox && inbox.kind === "bot" ? (inbox.from ?? null) : null;
  // A paused inbox reads as "Nobody" but keeps its sender list and routing,
  // so switching receiving back on restores them.
  const paused = inbox !== undefined && inbox.enabled === false;
  const baseMode: InboxMode = !inbox || paused ? "off" : inboxFrom == null ? "any" : "selected";
  const baseIds = inboxFrom ?? [];
  const botEmit = bot.emit ?? false;
  const [emit, setEmit] = useState(botEmit);
  const [mode, setMode] = useState<InboxMode>(baseMode);
  const [ids, setIds] = useState<string[]>(baseIds);
  const [editingInbox, setEditingInbox] = useState(false);
  useEffect(() => setEmit(botEmit), [botEmit]);
  useEffect(() => {
    setMode(baseMode);
    setIds(baseIds);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [inbox?.triggerId, baseMode, JSON.stringify(baseIds)]);

  const emitDirty = emit !== botEmit;
  const inboxDirty =
    mode !== baseMode || (mode === "selected" && JSON.stringify([...ids].sort()) !== JSON.stringify([...baseIds].sort()));
  const problem = mode === "selected" && ids.length === 0 ? "Choose at least one bot, or allow any bot." : null;
  const save = useMutation({
    mutationFn: async () => {
      if (emitDirty) {
        await api("PUT", `/api/v1/universes/${universeId}/bots/${bot.botId}`, {
          bot: { ...botInputOf(bot), emit },
          expectedRevision: bot.revision,
        });
      }
      if (inboxDirty) {
        const url = `/api/v1/universes/${universeId}/bots/${bot.botId}/triggers`;
        if (mode === "off") {
          // Pause, never delete: the sender list and routing survive.
          if (inbox && inbox.enabled !== false) {
            await api("PUT", `${url}/${inbox.triggerId}`, {
              trigger: { ...triggerInputOf(inbox), enabled: false },
              expectedRevision: inbox.revision,
            });
          }
        } else {
          const selection = inboxSelectionSpec(mode === "any" ? "any" : "selected", ids);
          if (inbox) {
            await api("PUT", `${url}/${inbox.triggerId}`, {
              trigger: { ...triggerInputOf(inbox), ...selection, enabled: true },
              expectedRevision: inbox.revision,
            });
          } else {
            await api("PUT", `${url}/inbox`, {
              trigger: { triggerId: "inbox", kind: "bot", ...selection },
            });
          }
        }
      }
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["bot", universeId, bot.botId] }),
        queryClient.invalidateQueries({ queryKey: ["bots", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-triggers", universeId, bot.botId] }),
      ]);
    },
  });
  const closed = bot.closedAtMs != null;
  const readOnly = !manage || closed;
  const others = (bots.data?.bots ?? []).filter((other) => other.botId !== bot.botId && other.closedAtMs == null);

  return (
    <SetupSection
      id="other-bots"
      title="Other bots"
      description="Bots in this universe can message each other; every message is an event, and each side decides for itself."
      summary={otherBotsSummary(botEmit, baseMode === "off" ? "off" : baseMode === "any" ? "any" : baseIds)}
    >
      <ToggleRow
        id="bot-emit"
        label="Can message other bots"
        hint="Sees which bots accept it and addresses them by id; may also post to itself. Rate-capped, and a message travels at most a few hops. Turning this on also opens the inbox — sending without listening is the rare case."
        checked={emit}
        onCheckedChange={(checked) => {
          setEmit(checked);
          if (checked && mode === "off") setMode("any");
        }}
        disabled={readOnly}
      />
      <div className="grid min-w-0 max-w-full gap-2 rounded-md border p-3">
        <div className="flex min-w-0 items-center justify-between gap-3">
          <Label htmlFor="bot-inbox-mode" className="min-w-0 text-sm">
            Accepts messages from
            <span className="block text-xs font-normal text-muted-foreground">
              {others.length === 0
                ? "There are no other bots in this universe yet."
                : "Which bots may address this one. Messages from them arrive like any other event."}
            </span>
          </Label>
          <Select value={mode} onValueChange={(value) => value && setMode(value as InboxMode)} disabled={readOnly}>
            <SelectTrigger id="bot-inbox-mode" size="sm" className="w-40 max-w-full shrink-0">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="off">Nobody</SelectItem>
              <SelectItem value="any">Any bot here</SelectItem>
              <SelectItem value="selected">Only these bots</SelectItem>
            </SelectContent>
          </Select>
        </div>
        {mode === "selected" && (
          <BotMultiSelect currentBotId={bot.botId} bots={bots.data?.bots ?? []} value={ids} onChange={setIds} />
        )}
        {mode !== "off" && manage && (
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={!inbox || inboxDirty}
              onClick={() => setEditingInbox(true)}
              title={!inbox || inboxDirty ? "Save first, then set routing and batching." : undefined}
            >
              <SlidersHorizontal data-icon="inline-start" /> Routing &amp; batching…
            </Button>
            <span className="text-xs text-muted-foreground">
              {!inbox || inboxDirty
                ? "Available after you save."
                : inbox.route?.policy === "perKey"
                  ? "One thread per key."
                  : inbox.route?.policy === "perEvent"
                    ? "One thread per message."
                    : "Messages arrive in Main."}
            </span>
          </div>
        )}
        {mode === "off" && paused && (
          <p className="text-xs text-muted-foreground">
            Inbox paused{inbox?.disabledReason === "breaker" ? " by flood protection" : ""}; its sender list and
            routing are kept.
          </p>
        )}
      </div>
      {manage && (
        <SaveRow
          dirty={emitDirty || inboxDirty}
          pending={save.isPending}
          error={problem ?? save.error?.message}
          disabled={closed || problem !== null}
          onSave={() => save.mutate()}
          note="Sending is a tool grant, picked up at the bot's next idle moment; receiving applies to the next message."
        />
      )}
      {manage && inbox && editingInbox && (
        <SavedTriggerFormCard
          universeId={universeId}
          botId={bot.botId}
          bots={bots.data?.bots ?? []}
          trigger={inbox}
          open
          deliveryOnly
          onOpenChange={setEditingInbox}
        />
      )}
    </SetupSection>
  );
}

function ToggleRow({
  id,
  label,
  hint,
  checked,
  onCheckedChange,
  disabled,
}: {
  id: string;
  label: string;
  hint: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <div className="flex min-w-0 max-w-full items-center justify-between gap-3 rounded-md border p-3">
      <Label htmlFor={id} className="min-w-0 text-sm">
        {label}
        <span className="block text-xs font-normal text-muted-foreground">{hint}</span>
      </Label>
      <Switch id={id} checked={checked} onCheckedChange={onCheckedChange} disabled={disabled} />
    </div>
  );
}

function DangerSection({
  universeId,
  slug,
  bot,
  manage,
}: {
  universeId: string;
  slug: string;
  bot: BotView;
  manage: boolean;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const closed = bot.closedAtMs != null;
  const close = useMutation({
    mutationFn: () =>
      api<BotCloseResponse>("POST", `/api/v1/universes/${universeId}/bots/${bot.botId}/close`),
    onSuccess: async ({ bot: updated }) => {
      queryClient.setQueryData(["bot", universeId, bot.botId], { bot: updated });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["bots", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] }),
      ]);
    },
  });
  const remove = useMutation({
    mutationFn: () => api<BotDeleteResponse>("DELETE", `/api/v1/universes/${universeId}/bots/${bot.botId}`),
    onSuccess: async () => {
      queryClient.removeQueries({ queryKey: ["bot", universeId, bot.botId] });
      await queryClient.invalidateQueries({ queryKey: ["bots", universeId] });
      navigate(`/u/${slug}/bots`);
    },
  });
  if (!manage) return null;
  return (
    <SetupSection
      id="danger"
      title="Danger zone"
      description="Pausing is reversible and lives in the header. These are not."
      summary={closed ? `Closed ${new Date(bot.closedAtMs ?? 0).toLocaleString()} · delete to free the id` : "Close or delete this bot"}
      tone="danger"
    >
      {closed ? (
        <p className="text-xs text-muted-foreground">
          Closed {new Date(bot.closedAtMs ?? 0).toLocaleString()}: conversations and schedules were released and
          events are refused. The record and its history stay until the bot is deleted.
        </p>
      ) : (
        <div className="flex min-w-0 items-start justify-between gap-3">
          <p className="min-w-0 text-xs text-muted-foreground">
            <b className="font-medium text-foreground">Close</b> is final: in-flight runs are cancelled, every
            conversation is closed, schedules are dropped, and new events are refused. The record, its history, the
            id, and its environment stay.
          </p>
          <AlertDialog>
            <AlertDialogTrigger render={<Button variant="outline" size="sm" disabled={close.isPending} />}>
              {close.isPending ? "Closing…" : "Close bot"}
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Close {botLabel(bot)}?</AlertDialogTitle>
                <AlertDialogDescription>
                  This cannot be undone. Pending events are archived, active runs are cancelled, all conversations
                  are closed, and webhooks and other bots are refused from now on. The environment is left alone.
                  To pause instead, use Pause in the header.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Keep running</AlertDialogCancel>
                <AlertDialogAction onClick={() => close.mutate()}>Close bot</AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      )}
      <div className="flex min-w-0 items-start justify-between gap-3">
        <p className="min-w-0 text-xs text-muted-foreground">
          <b className="font-medium text-foreground">Delete</b> erases the bot, its triggers, its event history, and
          its conversations, and frees the id{closed ? "." : " — it closes the bot first."} Environments and
          profiles are never deleted with a bot.
        </p>
        <AlertDialog>
          <AlertDialogTrigger render={<Button variant="destructive" size="sm" disabled={remove.isPending} />}>
            {remove.isPending ? "Deleting…" : "Delete bot"}
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete {botLabel(bot)}?</AlertDialogTitle>
              <AlertDialogDescription>
                {closed
                  ? "The record, its event history, and its conversations are erased; the id becomes available again."
                  : "The bot is closed first (runs cancelled, conversations closed, events refused), then the record, its event history, and its conversations are erased and the id becomes available again."}
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Keep</AlertDialogCancel>
              <AlertDialogAction onClick={() => remove.mutate()}>Delete bot</AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
      {(close.error || remove.error) && (
        <p className="text-xs text-destructive">{close.error?.message ?? remove.error?.message}</p>
      )}
    </SetupSection>
  );
}
