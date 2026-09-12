import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import { NavLink, useNavigate } from "react-router-dom";
import {
  api,
  type BotCreateResponse,
  type BotListResponse,
  type Environment,
  type ProfileDocument,
  type ProfileEnvironment,
  type ProfileSummary,
} from "@/api";
import { BotAvatar, botColor } from "@/components/bot/face";
import { BotEditorDialog } from "@/components/bot/editor-dialog";
import { botIdFrom } from "@/components/bot/identity";
import { capabilitySummary } from "@/components/bot/setup-summary";
import { BOT_TEMPLATES, type BotTemplate } from "@/components/bot/templates";
import { describeCron } from "@/components/bot/trigger-summary";
import {
  BotMultiSelect,
  NAME_PATTERN,
  TriggerFormCard,
  TriggerKindPicker,
  defaultPollForm,
  defaultTriggerForms,
  defaultTriggerName,
  triggerCreateBody,
  triggerDraftSummary,
  triggerFormProblem,
  type BotEnvStatus,
  type TriggerForms,
  type TriggerKind,
} from "@/components/bot/triggers";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { MetadataMapEditor } from "@/components/session/metadata-editor";
import { ProfileEnvironmentEditor } from "@/components/session/profile-environment-editor";
import { ProfileRetentionEditor } from "@/components/session/profile-retention-editor";
import { SessionConfigEditor } from "@/components/session/session-config-editor";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { ProgressSteps } from "@/components/ui/progress-steps";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import { setupResourceFeatureError } from "@/lib/sessions/resource-features";
import { cn } from "@/lib/utils";

type Step = "job" | "wakeups" | "profile" | "bots" | "guardrails";
/** The same sections as Bot settings, in the same order. */
const STEPS: Array<{ id: Step; label: string }> = [
  { id: "job", label: "Job" },
  { id: "wakeups", label: "Triggers" },
  { id: "profile", label: "Session profile" },
  { id: "bots", label: "Other bots" },
  { id: "guardrails", label: "Guardrails" },
];

interface WakeupDraft {
  key: string;
  kind: TriggerKind;
  name: string;
  forms: TriggerForms;
}

export { botIdFrom };

export function botOwnedProfileDocument({
  profileId,
  displayName,
  config,
  baseInstructions,
  environment,
  metadata,
  retention,
}: {
  profileId: string;
  displayName: string;
  config?: Record<string, unknown>;
  baseInstructions: string;
  environment?: ProfileEnvironment;
  metadata?: Record<string, string>;
  retention?: number;
}): ProfileDocument {
  return {
    profileId,
    displayName,
    description: `Setup of bot ${profileId}`,
    ...(config ? { config } : {}),
    ...(baseInstructions.trim() ? { instructions: { type: "text", text: baseInstructions } } : {}),
    ...(environment ? { environment } : {}),
    ...(metadata ? { metadata } : {}),
    ...(retention !== undefined ? { retention: { deleteAfterCloseMs: retention } } : {}),
  };
}

export function uniqueTriggerName(base: string, taken: string[]): string {
  if (!taken.includes(base)) return base;
  let index = 2;
  while (taken.includes(`${base}-${index}`)) index += 1;
  return `${base}-${index}`;
}

/** Plain words for a trigger still being drafted, from its form. */
export function wakeupSummary(draft: WakeupDraft): string {
  return triggerDraftSummary(draft.kind, draft.forms);
}

/** What a template comes with, in a few words: its triggers, then its capabilities. */
export function templateHighlights(template: BotTemplate): string[] {
  const wakeups = template.triggers.map((trigger) => {
    switch (trigger.kind) {
      case "schedule":
        return describeCron(trigger.cron, trigger.timezone === "UTC" ? null : trigger.timezone);
      case "webhook":
        return trigger.preset === "github" ? "GitHub events" : "Webhook";
      case "chat":
        return "Chat messages";
      case "bot":
        return "Other bots";
    }
  });
  return [...wakeups, ...capabilitySummary({ features: template.features })];
}

export function BotCreateDialog({
  universeId,
  slug,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return <Wizard universeId={universeId} slug={slug} open={open} onOpenChange={onOpenChange} />;
}

function Wizard({
  universeId,
  slug,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [step, setStep] = useState<Step>("job");
  const [templateId, setTemplateId] = useState("blank");
  // The last name a template suggested: a person's own name is never
  // overwritten, a suggestion is replaced by the next template's.
  const [suggestedName, setSuggestedName] = useState<string | null>(null);
  const [displayName, setDisplayName] = useState("");
  const [botId, setBotId] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [description, setDescription] = useState("");
  const [brief, setBrief] = useState("");
  const [wakeups, setWakeups] = useState<WakeupDraft[]>([]);
  const [openWakeup, setOpenWakeup] = useState<string | null>(null);
  const [setupMode, setSetupMode] = useState<"own" | "shared">("own");
  const [sharedProfileId, setSharedProfileId] = useState("");
  const [config, setConfig] = useState<Record<string, unknown> | undefined>(undefined);
  const [configError, setConfigError] = useState<string | null>(null);
  const [baseInstructions, setBaseInstructions] = useState("");
  const [environment, setEnvironment] = useState<ProfileEnvironment | undefined>(undefined);
  const [metadata, setMetadata] = useState<Record<string, string> | undefined>();
  const [retention, setRetention] = useState<number | undefined>();
  const [retentionError, setRetentionError] = useState<string | null>(null);
  const [runsPerDay, setRunsPerDay] = useState("50");
  const [selfConfig, setSelfConfig] = useState(true);
  const [emit, setEmit] = useState(false);
  // The inbox is a trigger draft like any other, but it is driven from the
  // "Other bots" block, not the trigger picker, so send and receive sit together.
  const inboxDraft = wakeups.find((draft) => draft.kind === "bot");
  const inboxMode: "off" | "any" | "selected" = !inboxDraft
    ? "off"
    : inboxDraft.forms.inbox.fromMode === "any"
      ? "any"
      : "selected";
  const inboxIds = inboxDraft?.forms.inbox.fromBotIds ?? [];
  const setInbox = (mode: "off" | "any" | "selected", ids: string[] = inboxIds) => {
    setWakeups((current) => {
      const without = current.filter((draft) => draft.kind !== "bot");
      if (mode === "off") return without;
      const existing = current.find((draft) => draft.kind === "bot");
      const forms = existing?.forms ?? defaultTriggerForms();
      const draft: WakeupDraft = {
        key: existing?.key ?? crypto.randomUUID(),
        kind: "bot",
        name:
          existing?.name ??
          uniqueTriggerName(
            "inbox",
            without.map((entry) => entry.name),
          ),
        forms: {
          ...forms,
          inbox: {
            ...forms.inbox,
            fromMode: mode === "any" ? "any" : "selected",
            fromBotIds: ids,
          },
        },
      };
      return [...without, draft];
    });
  };
  const visibleWakeups = wakeups.filter((draft) => draft.kind !== "bot");

  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
  });
  const bots = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/v1/universes/${universeId}/bots`),
  });
  const options = useSessionConfigEditorOptions(universeId, step === "profile" || step === "wakeups");
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled: step === "profile" || step === "wakeups",
  });

  const env: BotEnvStatus =
    setupMode === "shared"
      ? { kind: "unknown" }
      : environment?.type === "existing"
        ? { kind: "existing", environmentId: environment.environmentId }
        : { kind: "none" };

  const applyTemplate = (template: BotTemplate) => {
    setTemplateId(template.id);
    // A suggested name is replaced by the next template's, and Blank clears
    // it — a name the person typed themselves is never touched.
    if (displayName.trim() === "" || displayName === suggestedName) {
      setDisplayName(template.suggestedName ?? "");
      if (!idTouched) setBotId(template.suggestedName ? botIdFrom(template.suggestedName) : "");
    }
    setSuggestedName(template.suggestedName);
    setBrief(template.brief);
    setConfig(Object.keys(template.features).length > 0 ? { features: structuredClone(template.features) } : undefined);
    setRunsPerDay(template.runsPerDay === null ? "" : String(template.runsPerDay));
    setSelfConfig(template.selfConfig);
    setEmit(template.emit);
    const drafts: WakeupDraft[] = [];
    for (const trigger of template.triggers) {
      const forms = defaultTriggerForms();
      if (trigger.kind === "schedule") {
        forms.schedule = {
          ...forms.schedule,
          once: false,
          cron: trigger.cron,
          timezone: trigger.timezone,
          summary: trigger.summary,
        };
      } else if (trigger.kind === "webhook") {
        forms.webhook = {
          ...forms.webhook,
          preset: trigger.preset === "github",
          routePolicy: trigger.perKey ? "perKey" : "bot",
          ...(trigger.filter ? { filter: trigger.filter } : {}),
        };
      }
      drafts.push({
        key: crypto.randomUUID(),
        kind: trigger.kind,
        name: trigger.name,
        forms,
      });
    }
    setWakeups(drafts);
    setOpenWakeup(null);
  };

  const idInvalid = botId.trim().length > 0 && !NAME_PATTERN.test(botId.trim());
  const idTaken = (bots.data?.bots ?? []).some((bot) => bot.botId === botId.trim());
  const profileTaken =
    setupMode === "own" && (profiles.data?.some((profile) => profile.profileId === botId.trim()) ?? false);
  const wakeupProblems = wakeups.map((draft) => ({
    key: draft.key,
    problem:
      !draft.name.trim() || !NAME_PATTERN.test(draft.name.trim())
        ? "Give it a name in lowercase letters, numbers, and dashes."
        : wakeups.filter((other) => other.name.trim() === draft.name.trim()).length > 1
          ? "Two triggers share this name."
          : triggerFormProblem(draft.kind, draft.forms, env),
  }));
  const setupProblem =
    setupMode === "own"
      ? configError
        ? `Session profile: ${configError}`
        : retentionError
          ? `Session profile: ${retentionError}`
          : setupResourceFeatureError({ config, environment })
      : sharedProfileId
        ? null
        : "Pick the shared profile this bot applies.";
  const problems = [
    ...(botId.trim() ? [] : ["Give the bot a name."]),
    ...(idInvalid ? ["The id uses lowercase letters, numbers, and dashes."] : []),
    ...(idTaken ? [`A bot named ${botId.trim()} already exists.`] : []),
    ...(profileTaken
      ? [`A profile named ${botId.trim()} already exists — pick another id, or use it as a shared profile.`]
      : []),
    ...wakeupProblems.filter((entry) => entry.problem).map((entry) => `Trigger: ${entry.problem}`),
    ...(setupProblem ? [setupProblem] : []),
    ...(runsPerDay.trim() && !(Number(runsPerDay) >= 1) ? ["The daily run limit is at least 1."] : []),
  ];

  const create = useMutation({
    mutationFn: async () => {
      const id = botId.trim();
      let profileId = sharedProfileId;
      if (setupMode === "own") {
        profileId = id;
        await api(
          "PUT",
          `/api/v1/universes/${universeId}/profiles/${encodeURIComponent(profileId)}`,
          botOwnedProfileDocument({
            profileId,
            displayName: displayName.trim() || id,
            config,
            baseInstructions,
            environment,
            metadata,
            retention,
          }),
        );
      }
      try {
        const { bot } = await api<BotCreateResponse>("POST", `/api/v1/universes/${universeId}/bots`, {
          bot: {
            botId: id,
            ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
            ...(description.trim() ? { description: description.trim() } : {}),
            profileId,
            ...(brief.trim() ? { brief: brief.trim() } : {}),
            ...(runsPerDay.trim() ? { runsPerDay: Number(runsPerDay) } : {}),
            selfConfig,
            emit,
          },
          triggers: wakeups.map((draft) => triggerCreateBody(draft.kind, draft.name.trim(), draft.forms)),
        });
        return bot;
      } catch (error) {
        if (setupMode === "own") {
          await api("DELETE", `/api/v1/universes/${universeId}/profiles/${encodeURIComponent(profileId)}`).catch(
            () => undefined,
          );
        }
        throw error;
      }
    },
    onSuccess: async (bot) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["bots", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["profiles", universeId] }),
      ]);
      navigate(`/u/${slug}/bots/${bot.botId}`, { state: { introduce: true } });
    },
  });

  const stepIndex = STEPS.findIndex((entry) => entry.id === step);
  const next = () => setStep(STEPS[Math.min(stepIndex + 1, STEPS.length - 1)]!.id);
  const back = () => setStep(STEPS[Math.max(stepIndex - 1, 0)]!.id);
  const label = displayName.trim() || botId.trim() || "your bot";

  const addWakeup = (kind: TriggerKind, pick?: { pollSource: "http" | "exec" }) => {
    const forms = defaultTriggerForms();
    if (kind === "poll") {
      forms.poll = {
        ...defaultPollForm,
        sourceKind: pick?.pollSource ?? "http",
        environmentId: pick?.pollSource === "exec" && env.kind === "existing" ? env.environmentId : "",
      };
    }
    const draft: WakeupDraft = {
      key: crypto.randomUUID(),
      kind,
      name: uniqueTriggerName(
        defaultTriggerName(kind, pick?.pollSource),
        wakeups.map((entry) => entry.name),
      ),
      forms,
    };
    setWakeups((current) => [...current, draft]);
    setOpenWakeup(draft.key);
  };
  const updateWakeup = (key: string, mutate: (draft: WakeupDraft) => WakeupDraft) =>
    setWakeups((current) => current.map((draft) => (draft.key === key ? mutate(draft) : draft)));

  return (
    <BotEditorDialog
      open={open}
      onOpenChange={onOpenChange}
      icon={
        <BotAvatar
          botId={botId.trim() || "new-bot"}
          size={36}
          className={cn(!botId.trim() && "text-muted-foreground")}
          color={botId.trim() ? undefined : "var(--muted)"}
        />
      }
      title="New bot"
      description="Define its job, triggers, session profile, collaborators, and guardrails."
      contentClassName="sm:max-w-4xl"
    >
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <ProgressSteps
          steps={STEPS}
          current={step}
          onSelect={setStep}
          canSelect={() => true}
          label="Bot creation progress"
        />
        <main className="min-h-0 min-w-0 flex-1 overflow-x-hidden overflow-y-auto overscroll-contain">
          <div className="grid w-full min-w-0 content-start gap-6 px-5 py-6 md:px-6">
            {step === "job" && (
              <section className="grid min-w-0 gap-5">
                <header className="grid gap-1">
                  <h1 className="text-xl font-semibold tracking-tight">What is this bot's job?</h1>
                  <p className="text-sm text-muted-foreground">
                    Start from a template or from scratch; everything stays editable later.
                  </p>
                </header>
                <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                  {BOT_TEMPLATES.map((template) => (
                    <TemplateCard
                      key={template.id}
                      template={template}
                      selected={templateId === template.id}
                      onSelect={() => applyTemplate(template)}
                    />
                  ))}
                </div>
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field>
                    <FieldLabel htmlFor="new-bot-name">Name</FieldLabel>
                    <Input
                      id="new-bot-name"
                      value={displayName}
                      onChange={(event) => {
                        setDisplayName(event.target.value);
                        if (!idTouched) setBotId(event.target.value ? botIdFrom(event.target.value) : "");
                      }}
                      placeholder="Triage"
                      autoFocus
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="new-bot-id">Id</FieldLabel>
                    <Input
                      id="new-bot-id"
                      value={botId}
                      onChange={(event) => {
                        setBotId(event.target.value);
                        setIdTouched(event.target.value.length > 0);
                      }}
                      placeholder="triage"
                      className="font-mono"
                      aria-invalid={idInvalid || idTaken || undefined}
                    />
                    <FieldDescription>
                      {idTaken
                        ? "Taken by another bot."
                        : "What other bots, briefs, and URLs use — it cannot change later."}
                    </FieldDescription>
                  </Field>
                </div>
                <Field>
                  <FieldLabel htmlFor="new-bot-brief">Brief</FieldLabel>
                  <Textarea
                    id="new-bot-brief"
                    value={brief}
                    onChange={(event) => setBrief(event.target.value)}
                    rows={9}
                    placeholder="What this bot is for, how it should behave, what good work looks like. It reads this with every event."
                    className="leading-relaxed"
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor="new-bot-description">Description for other bots (optional)</FieldLabel>
                  <Input
                    id="new-bot-description"
                    value={description}
                    onChange={(event) => setDescription(event.target.value)}
                    placeholder="One line other bots read when deciding whether to message this bot."
                  />
                </Field>
              </section>
            )}

            {step === "wakeups" && (
              <section className="grid gap-5">
                <header className="grid gap-1">
                  <h1 className="text-xl font-semibold tracking-tight">When should {label} wake up?</h1>
                  <p className="text-sm text-muted-foreground">
                    Pick any that apply. You can always message it from Chat, and add more triggers later
                    {selfConfig ? " — or ask it to add them itself" : ""}.
                  </p>
                </header>
                <TriggerKindPicker env={env} onPick={addWakeup} exclude={["bot"]} />
                {visibleWakeups.length > 0 && (
                  <div className="grid gap-2">
                    {visibleWakeups.map((draft) => {
                      const problem = wakeupProblems.find((entry) => entry.key === draft.key)?.problem ?? null;
                      const open = openWakeup === draft.key;
                      return (
                        <TriggerFormCard
                          key={draft.key}
                          universeId={universeId}
                          botId={botId.trim() || "new-bot"}
                          bots={bots.data?.bots ?? []}
                          kind={draft.kind}
                          name={draft.name}
                          forms={draft.forms}
                          patch={(key, value) =>
                            updateWakeup(draft.key, (current) => ({
                              ...current,
                              forms: { ...current.forms, [key]: value },
                            }))
                          }
                          summary={wakeupSummary(draft)}
                          problem={problem}
                          open={open}
                          onOpenChange={(next) => setOpenWakeup(next ? draft.key : null)}
                          nameEditable
                          onNameChange={(name) =>
                            updateWakeup(draft.key, (current) => ({
                              ...current,
                              name,
                            }))
                          }
                          idPrefix={`wakeup-${draft.key}`}
                          actions={(
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              aria-label="Remove trigger"
                              onClick={() =>
                                setWakeups((current) => current.filter((entry) => entry.key !== draft.key))
                              }
                            >
                              <Trash2 />
                            </Button>
                          )}
                        />
                      );
                    })}
                  </div>
                )}
              </section>
            )}

            {step === "bots" && (
              <section className="grid gap-5">
                <header className="grid gap-1">
                  <h1 className="text-xl font-semibold tracking-tight">Should {label} talk to other bots?</h1>
                  <p className="text-sm text-muted-foreground">
                    Bots in this universe can message each other; every message is an event, and each side decides for
                    itself. Skip this if {label} works alone.
                  </p>
                </header>
                <div className="grid gap-2">
                  <div className="flex min-w-0 max-w-full items-center justify-between gap-3 rounded-md border p-3">
                    <Label htmlFor="new-bot-emit" className="min-w-0 text-sm">
                      Can message other bots
                      <span className="block text-xs font-normal text-muted-foreground">
                        Sees which bots accept it and addresses them by id; rate-capped. Turning this on also opens the
                        inbox — sending without listening is the rare case.
                      </span>
                    </Label>
                    <Switch
                      id="new-bot-emit"
                      checked={emit}
                      onCheckedChange={(checked) => {
                        setEmit(checked);
                        if (checked && inboxMode === "off") setInbox("any");
                      }}
                    />
                  </div>
                  <div className="grid min-w-0 max-w-full gap-2 rounded-md border p-3">
                    <div className="flex min-w-0 items-center justify-between gap-3">
                      <Label htmlFor="new-bot-inbox" className="min-w-0 text-sm">
                        Accepts messages from
                        <span className="block text-xs font-normal text-muted-foreground">
                          Which bots may address this one. Routing and batching remain editable in Bot settings.
                        </span>
                      </Label>
                      <Select
                        value={inboxMode}
                        onValueChange={(value) => value && setInbox(value as "off" | "any" | "selected")}
                      >
                        <SelectTrigger id="new-bot-inbox" size="sm" className="w-40 max-w-full shrink-0">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="off">Nobody</SelectItem>
                          <SelectItem value="any">Any bot here</SelectItem>
                          <SelectItem value="selected">Only these bots</SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                    {inboxMode === "selected" && (
                      <BotMultiSelect
                        currentBotId={botId.trim() || "new-bot"}
                        bots={bots.data?.bots ?? []}
                        value={inboxIds}
                        onChange={(ids) => setInbox("selected", ids)}
                      />
                    )}
                  </div>
                </div>
              </section>
            )}

            {step === "profile" && (
              <section className="grid gap-5">
                <header className="grid gap-1">
                  <h1 className="text-xl font-semibold tracking-tight">How should {label}&apos;s sessions run?</h1>
                  <p className="text-sm text-muted-foreground">
                    Choose the instructions, model, tools, environment, metadata, and retention saved in its session
                    profile.
                  </p>
                </header>
                <ProviderReadinessBanner universeId={universeId} slug={slug} />
                <div className="grid gap-2 sm:grid-cols-2">
                  <SetupModeChoice
                    active={setupMode === "own"}
                    title="Its own profile"
                    description="A session profile named after the bot; edit it from Bot settings."
                    onClick={() => setSetupMode("own")}
                  />
                  <SetupModeChoice
                    active={setupMode === "shared"}
                    title="A shared profile"
                    description="Apply an existing profile; changes to it reach every bot that uses it."
                    onClick={() => setSetupMode("shared")}
                  />
                </div>
                {setupMode === "shared" ? (
                  <Field>
                    <FieldLabel>Profile</FieldLabel>
                    <Select value={sharedProfileId} onValueChange={(value) => value && setSharedProfileId(value)}>
                      <SelectTrigger className="w-full sm:w-80">
                        <SelectValue placeholder="Select a profile" />
                      </SelectTrigger>
                      <SelectContent>
                        {profiles.data?.map((profile) => (
                          <SelectItem key={profile.profileId} value={profile.profileId}>
                            {profile.displayName ?? profile.profileId}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    {profiles.data?.length === 0 && (
                      <FieldDescription>No profiles yet — give the bot its own setup instead.</FieldDescription>
                    )}
                  </Field>
                ) : (
                  <>
                    <Field>
                      <FieldLabel htmlFor="new-bot-base-instructions">Base instructions</FieldLabel>
                      <Textarea
                        id="new-bot-base-instructions"
                        value={baseInstructions}
                        onChange={(event) => setBaseInstructions(event.target.value)}
                        rows={4}
                        placeholder="Usually empty: the brief already describes the bot's job. Use this for a system prompt its session profile should carry."
                      />
                    </Field>
                    <SessionConfigEditor
                      value={config}
                      mcpServers={options.mcpServers}
                      workspaces={options.workspaces}
                      workspacesLoading={options.workspacesLoading}
                      models={options.models}
                      profiles={options.profiles}
                      environmentProviders={options.environmentProviders}
                      environmentSetup={
                        <div className="grid gap-3">
                          <ProfileEnvironmentEditor
                            embedded
                            value={environment}
                            environments={environments.data}



                            description="Choose an existing environment shared by this bot's sessions. Manage its lifecycle on the Environments page."
                            onChange={setEnvironment}
                          />
                          {environments.data?.length === 0 && (
                            <p className="text-xs text-muted-foreground">
                              No environments yet — create one under{" "}
                              <NavLink to={`/u/${slug}/settings/environments`} className="underline">
                                Settings › Environments
                              </NavLink>{" "}
                              if the bot needs a machine.
                            </p>
                          )}
                        </div>
                      }
                      metadataSetup={<MetadataMapEditor value={metadata} onChange={setMetadata} />}
                      metadataDescription="Defaults copied to every session this bot creates. Metadata helps with filtering and does not affect runtime behavior."
                      retentionSetup={
                        <ProfileRetentionEditor
                          value={retention}
                          onChange={setRetention}
                          onValidityChange={setRetentionError}
                        />
                      }
                      retentionDescription="Default automatic deletion for each new root session this bot creates."
                      onValidityChange={setConfigError}
                      onChange={(next) => setConfig(next as Record<string, unknown> | undefined)}
                    />
                  </>
                )}
              </section>
            )}

            {step === "guardrails" && (
              <section className="grid gap-5">
                <header className="grid gap-1">
                  <h1 className="text-xl font-semibold tracking-tight">Limits and permissions</h1>
                  <p className="text-sm text-muted-foreground">
                    Sensible defaults; all of it remains editable under Bot settings.
                  </p>
                </header>
                <Field className="sm:w-64">
                  <FieldLabel htmlFor="new-bot-runs">Daily run limit</FieldLabel>
                  <Input
                    id="new-bot-runs"
                    type="number"
                    min={1}
                    value={runsPerDay}
                    onChange={(event) => setRunsPerDay(event.target.value)}
                    placeholder="No limit"
                  />
                  <FieldDescription>
                    Runs and sub-agents count; events beyond it wait for the next UTC day.
                  </FieldDescription>
                </Field>
                <div className="grid gap-2">
                  <div className="flex min-w-0 max-w-full items-center justify-between gap-3 rounded-md border p-3">
                    <Label htmlFor="new-bot-self-config" className="min-w-0 text-sm">
                      Can change its own brief and triggers
                      <span className="block text-xs font-normal text-muted-foreground">
                        Ask it in Chat to add a schedule or rewrite its job. Off: it can only look.
                      </span>
                    </Label>
                    <Switch id="new-bot-self-config" checked={selfConfig} onCheckedChange={setSelfConfig} />
                  </div>
                </div>
              </section>
            )}
          </div>
        </main>
        <div className="flex shrink-0 items-center gap-3 border-t bg-popover px-5 py-3">
          <Button className="shrink-0" variant="outline" onClick={back} disabled={stepIndex === 0}>
            Back
          </Button>
          <div className="min-w-0 flex-1 truncate text-center text-xs">
            {create.error ? (
              <span className="text-destructive">{create.error.message}</span>
          ) : problems.length > 0 && stepIndex === STEPS.length - 1 ? (
            <span className="text-amber-700 dark:text-amber-400">
              {problems.length} {problems.length === 1 ? "item needs" : "items need"} attention
            </span>
          ) : null}
          </div>
          {stepIndex < STEPS.length - 1 ? (
            <Button className="shrink-0" onClick={next}>
              <span className="sm:hidden">Next</span>
              <span className="hidden sm:inline">Next: {STEPS[stepIndex + 1]!.label}</span>
            </Button>
          ) : (
            <Button
              className="shrink-0"
              onClick={() => create.mutate()}
              disabled={create.isPending || problems.length > 0}
            >
              {create.isPending ? "Creating…" : "Create bot"}
            </Button>
          )}
        </div>
      </div>
    </BotEditorDialog>
  );
}

/**
 * A template is a colleague you could hire, so its card carries a face and a
 * line of what it comes with — heavier than the option cards further on,
 * without shouting.
 */
function TemplateCard({
  template,
  selected,
  onSelect,
}: {
  template: BotTemplate;
  selected: boolean;
  onSelect: () => void;
}) {
  const blank = template.id === "blank";
  const highlights = templateHighlights(template);
  // The face is the bot's, not the template's: colour it by the id the bot
  // will get, so it does not change the moment the bot exists.
  const faceId = template.suggestedName ? botIdFrom(template.suggestedName) : "new-bot";
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={cn(
        "grid min-h-32 min-w-0 max-w-full content-start gap-2.5 rounded-xl border bg-card p-4 text-left shadow-sm transition-all",
        "hover:-translate-y-0.5 hover:shadow-md motion-reduce:transition-none motion-reduce:hover:translate-y-0",
        selected ? "border-primary ring-2 ring-primary/40" : "border-border",
      )}
    >
      <span className="flex min-w-0 items-center gap-3">
        <BotAvatar
          botId={faceId}
          size={36}
          className={cn("rounded-lg", blank && "text-muted-foreground")}
          color={blank ? "var(--muted)" : botColor(faceId)}
        />
        <span className="grid min-w-0 gap-0.5">
          <span className="truncate text-sm font-semibold">{template.name}</span>
          {template.suggestedName && (
            <span className="truncate text-[11px] text-muted-foreground">
              named {template.suggestedName} · <span className="font-mono">{botIdFrom(template.suggestedName)}</span>
            </span>
          )}
        </span>
      </span>
      <span className="text-xs leading-relaxed text-muted-foreground wrap-anywhere">{template.description}</span>
      {highlights.length > 0 && (
        <span className="mt-auto flex flex-wrap gap-1 pt-1">
          {highlights.map((highlight) => (
            <span
              key={highlight}
              className="rounded-full border bg-background px-2 py-0.5 text-[10px] font-medium text-foreground/80"
            >
              {highlight}
            </span>
          ))}
        </span>
      )}
    </button>
  );
}

function SetupModeChoice({
  active,
  title,
  description,
  onClick,
}: {
  active: boolean;
  title: string;
  description: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "grid min-w-0 max-w-full content-start gap-1 rounded-lg border p-3 text-left transition-colors hover:bg-muted/40",
        active && "border-primary bg-primary/5",
      )}
    >
      <span className="text-sm font-medium">{title}</span>
      <span className="text-xs text-muted-foreground wrap-anywhere">{description}</span>
    </button>
  );
}
