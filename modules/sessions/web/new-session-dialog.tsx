import {
  MetadataMapEditor, ProfileRetentionEditor, resourceFeatureDisableReasons, sessionCollections, SessionConfigEditor, SetupEditorSection, setupResourceFeatureError, useSessionConfigEditorOptions,
} from "@lightspeed-ai/platform-web/modules/conversations";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import {
  api,
  type InlineProfile,
  type ProfileDocument,
  type ProfileSource,
  type ProfileSummary,
  type SessionView,
} from "@lightspeed-ai/platform-web/modules/client";
import { Button } from "@lightspeed-ai/platform-web/modules/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  dialogWarpMs,
} from "@lightspeed-ai/platform-web/modules/ui/dialog";
import { Field, FieldLabel } from "@lightspeed-ai/platform-web/modules/ui/field";
import { Input } from "@lightspeed-ai/platform-web/modules/ui/input";
import { Textarea } from "@lightspeed-ai/platform-web/modules/ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@lightspeed-ai/platform-web/modules/ui/select";
import {
  ProviderReadinessBanner,
  modelFromConfig,
  modelLabel,
  resolveCreationModel,
  useModelDefaults,
} from "@lightspeed-ai/platform-web/modules/models";
import { useActionPermissions } from "@lightspeed-ai/platform-web/modules";

export function NewSessionDialog({
  universeId,
  handle,
  open,
  onOpenChange,
  onClosed,
  search,
}: {
  universeId: string;
  handle: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Its close has finished (animations included). */
  onClosed?: () => void;
  search: string;
}) {
  const canSetRetention = useActionPermissions(universeId).can("setSessionRetention");
  const [displayName, setDisplayName] = useState("");
  const [profileId, setProfileId] = useState("");
  const canConfigure = useActionPermissions(universeId).can("configureSession");
  const [step, setStep] = useState<"basics" | "setup">("basics");
  const [inlineProfile, setInlineProfile] = useState<InlineProfile | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const [retentionError, setRetentionError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/ui/universes/${universeId}/profiles`),
    enabled: open,
  });
  const selectedProfile = useQuery({
    queryKey: ["profile", universeId, profileId],
    queryFn: () =>
      api<ProfileDocument>("GET", `/api/ui/universes/${universeId}/profiles/${profileId}`),
    enabled: open && Boolean(profileId),
  });
  const editorOptions = useSessionConfigEditorOptions(universeId, open && step === "setup");
  const defaults = useModelDefaults(universeId, open);
  const creationProfile = profileForCreate(profileId, canConfigure ? inlineProfile : null, selectedProfile.data);
  const effectiveModel = resolveCreationModel(
    creationProfile.kind === "inline" ? modelFromConfig(creationProfile.profile.config) : null,
    creationProfile.kind === "named" ? modelFromConfig(selectedProfile.data?.config) : null,
    defaults.data,
  );
  const modelPending = Boolean(profileId && !inlineProfile && selectedProfile.isLoading)
    || (!effectiveModel.model && defaults.isLoading);
  const modelMissing = !modelPending && Boolean(defaults.data) && !effectiveModel.model;
  const modelSummary = (
    <div className="grid gap-2">
      <p className="text-sm">
        <span className="text-muted-foreground">
          {creationProfile.kind === "named" ? "Profile model" : effectiveModel.source}:{" "}
        </span>
        {modelPending ? "Loading…" : effectiveModel.model ? modelLabel(effectiveModel.model) : defaults.error ? "Could not load default" : "Not selected"}
      </p>
      <ProviderReadinessBanner universeId={universeId} handle={handle} model={effectiveModel.model ?? (defaults.data ? null : undefined)} enabled={open && !modelPending} className="rounded-lg border" />
    </div>
  );
  // Creating sends the dialog off to warp (see launch): it closes once the
  // session exists and the warp has built up, fading out over the new
  // session, which opens behind it.
  const [warp, setWarp] = useState(false);
  const warpStarted = useRef(0);
  useEffect(() => {
    if (open) setWarp(false);
  }, [open]);

  const create = useMutation({
    mutationFn: () =>
      api<SessionView>("POST", `/api/ui/universes/${universeId}/modules/sessions`, {
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        profile: creationProfile,
      }),
    onSuccess: async (session) => {
      await queryClient.invalidateQueries(sessionCollections(universeId));
      const target = session.id;
      setDisplayName("");
      setProfileId("");
      setStep("basics");
      setInlineProfile(null);
      setConfigError(null);
      setRetentionError(null);
      setError(null);
      // The new session opens behind the warp; the dialog (open while the
      // address asks for it) stays until the warp has built up, then closes,
      // which fades it all out over the session.
      const path = `/u/${handle}/sessions/${target}`;
      const nextSearch = new URLSearchParams(search);
      navigate(`${path}?${nextSearch}`);
      nextSearch.delete("new");
      const remaining = Math.max(0, dialogWarpMs() - (performance.now() - warpStarted.current));
      window.setTimeout(
        () => navigate(`${path}${nextSearch.size ? `?${nextSearch}` : ""}`, { replace: true }),
        remaining,
      );
    },
    onError: (err) => {
      // Called off: the dialog comes back with the error.
      setWarp(false);
      setError(err.message);
    },
  });

  /// Creates the session and sends the dialog off to warp meanwhile.
  const launch = () => {
    warpStarted.current = performance.now();
    setWarp(true);
    create.mutate();
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const resourceError = setupResourceFeatureError(
      inlineProfile ?? selectedProfile.data ?? {},
    );
    if (modelPending || modelMissing || (creationProfile.kind === "named" && selectedProfile.error)) return;
    if (configError || retentionError || resourceError) {
      setError(configError ? `Config: ${configError}` : retentionError ? `Retention: ${retentionError}` : resourceError);
      return;
    }
    launch();
  };

  const changeOpen = (next: boolean) => {
    onOpenChange(next);
    if (!next && !create.isPending) {
      setDisplayName("");
      setProfileId("");
      setStep("basics");
      setInlineProfile(null);
      setConfigError(null);
      setRetentionError(null);
      setError(null);
    }
  };

  const customize = () => {
    if (inlineProfile) {
      setStep("setup");
      return;
    }
    if (profileId && !selectedProfile.data) return;
    setInlineProfile(
      profileId && selectedProfile.data
        ? inlineProfileFromDocument(selectedProfile.data)
        : {},
    );
    setStep("setup");
  };
  const resourceFeatureError = inlineProfile
    ? setupResourceFeatureError(inlineProfile)
    : null;

  return (
    <Dialog open={open} onOpenChange={changeOpen} onOpenChangeComplete={(next) => { if (!next) onClosed?.(); }}>
      <DialogContent
        starscape
        warp={warp}
        className={step === "setup"
          ? "h-[min(92dvh,900px)] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 p-0 sm:max-w-4xl"
          : undefined}
      >
        {step === "basics" ? (
          <>
            <DialogHeader>
              <DialogTitle>New session</DialogTitle>
              <DialogDescription>
                {canConfigure ? "Start from a named profile or customize an inline setup for this session." : "Start with the universe defaults or choose an existing profile."}
              </DialogDescription>
            </DialogHeader>
            <form onSubmit={submit} className="grid gap-4">
              <Field>
                <FieldLabel htmlFor="new-session-name">Name</FieldLabel>
                <Input
                  id="new-session-name"
                  value={displayName}
                  onChange={(e) => setDisplayName(e.target.value)}
                  placeholder="Scratch chat"
                  autoFocus
                />
              </Field>
              <Field>
                <FieldLabel>Profile</FieldLabel>
                <Select
                  value={profileId}
                  onValueChange={(value) => {
                    setProfileId(value as string);
                    setInlineProfile(null);
                    setConfigError(null);
                    setRetentionError(null);
                    setError(null);
                  }}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue>
                      {(value: string) =>
                        value
                          ? (profiles.data?.find((p) => p.profileId === value)?.displayName ?? value)
                          : "No profile (universe default)"
                      }
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="">No profile (universe default)</SelectItem>
                    {(profiles.data ?? []).map((profile) => (
                      <SelectItem key={profile.profileId} value={profile.profileId}>
                        {profile.displayName ?? profile.profileId}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>
              {modelSummary}
              {canConfigure && <Button
                type="button"
                variant="outline"
                disabled={Boolean(profileId) && selectedProfile.isLoading}
                onClick={customize}
              >
                {inlineProfile ? "Edit customized setup" : "Customize setup…"}
              </Button>}
              <p className="text-xs text-muted-foreground">
                A new session is private: you and the universe Owners see it until you share it.
              </p>
              {selectedProfile.error && (
                <p className="text-sm text-destructive">{selectedProfile.error.message}</p>
              )}
              {error && <p className="text-sm text-destructive">{error}</p>}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={() => changeOpen(false)}>
                  Cancel
                </Button>
                <Button type="submit" disabled={create.isPending || modelPending || modelMissing || (creationProfile.kind === "named" && Boolean(selectedProfile.error))}>
                  {create.isPending ? "Creating…" : "Create"}
                </Button>
              </DialogFooter>
            </form>
          </>
        ) : (
          <>
            <DialogHeader className="border-b p-6 pr-14">
              <DialogTitle>Configure new session</DialogTitle>
              <DialogDescription>
                {profileId
                  ? `Customized from ${profiles.data?.find((profile) => profile.profileId === profileId)?.displayName ?? profileId}.`
                  : "Inline setup for this session."}
              </DialogDescription>
            </DialogHeader>
            <div className="min-h-0 space-y-5 overflow-y-auto p-6">
              {modelSummary}
              <InlineSetupEditor
                canSetRetention={canSetRetention}
                value={inlineProfile ?? {}}
                options={editorOptions}
                onValidityChange={setConfigError}
                onRetentionValidityChange={setRetentionError}
                onChange={setInlineProfile}
              />
            </div>
            <div className="grid gap-2 border-t p-4">
              {resourceFeatureError && (
                <p className="text-sm text-destructive">{resourceFeatureError}</p>
              )}
              {error && <p className="text-sm text-destructive">{error}</p>}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={() => setStep("basics")}>
                  Back
                </Button>
                {profileId && selectedProfile.data && (
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => setInlineProfile(inlineProfileFromDocument(selectedProfile.data!))}
                  >
                    Reset to profile
                  </Button>
                )}
                <Button
                  type="button"
                  disabled={create.isPending || modelPending || modelMissing || Boolean(configError || retentionError || resourceFeatureError)}
                  onClick={launch}
                >
                  {create.isPending ? "Creating…" : "Create session"}
                </Button>
              </DialogFooter>
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function InlineSetupEditor({
  canSetRetention,
  value,
  options,
  onValidityChange,
  onRetentionValidityChange,
  onChange,
}: {
  canSetRetention: boolean;
  value: InlineProfile;
  options: ReturnType<typeof useSessionConfigEditorOptions>;
  onValidityChange: (message: string | null) => void;
  onRetentionValidityChange: (message: string | null) => void;
  onChange: (profile: InlineProfile) => void;
}) {
  const change = (mutate: (next: InlineProfile) => void) => {
    const next = structuredClone(value);
    mutate(next);
    onChange(next);
  };
  const instructions = value.instructions?.type === "text" ? value.instructions.text : "";

  return (
    <div className="grid gap-8">
      <SetupEditorSection title="Instructions" description="System prompt applied when the session starts.">
        {value.instructions?.type === "textRef" ? (
          <p className="text-sm text-muted-foreground">
            This profile uses a blob-backed instruction. Editing replaces it with inline text.
          </p>
        ) : null}
        <Textarea
          className="min-h-32 resize-y field-sizing-fixed p-3"
          value={instructions}
          onChange={(event) => change((next) => {
            if (event.target.value) next.instructions = { type: "text", text: event.target.value };
            else delete next.instructions;
          })}
          spellCheck={false}
        />
      </SetupEditorSection>
      <SetupEditorSection
        title="Model configuration"
        description="Choose a model or inherit the universe default. Unset reasoning uses the provider default."
      >
        <SessionConfigEditor
          value={value.config}
          mcpServers={options.mcpServers}
          workspaces={options.workspaces}
          workspacesLoading={options.workspacesLoading}
          models={options.models}
          defaultModelLabel={options.defaultModelLabel}
          profiles={options.profiles}
          environments={options.environments}
          mcpToolDiscovery={options.mcpToolDiscovery}
          featureDisableReasons={resourceFeatureDisableReasons(value)}
          metadataSetup={(
            <MetadataMapEditor
              value={value.metadata}
              onChange={(metadata) => change((next) => {
                if (metadata) next.metadata = metadata;
                else delete next.metadata;
              })}
            />
          )}
          metadataDescription="Metadata copied onto the new session. It helps with filtering and does not affect how the session runs."
          retentionSetup={canSetRetention && (
            <ProfileRetentionEditor
              value={value.retention?.deleteAfterCloseMs}
              onValidityChange={onRetentionValidityChange}
              onChange={(deleteAfterCloseMs) => change((next) => {
                if (deleteAfterCloseMs !== undefined) next.retention = { deleteAfterCloseMs };
                else delete next.retention;
              })}
            />
          )}
          retentionDescription="Automatic deletion for this new root session after it closes."
          onValidityChange={onValidityChange}
          onChange={(config) => change((next) => {
            if (config) next.config = config;
            else delete next.config;
          })}
        />
      </SetupEditorSection>
    </div>
  );
}

function inlineProfileFromDocument(document: ProfileDocument): InlineProfile {
  const profile: InlineProfile = {};
  if (document.metadata) profile.metadata = structuredClone(document.metadata);
  if (document.retention) profile.retention = structuredClone(document.retention);
  if (isRecord(document.config)) profile.config = structuredClone(document.config);
  if (isRecord(document.instructions)) profile.instructions = structuredClone(document.instructions) as InlineProfile["instructions"];
  return profile;
}

function profileForCreate(
  profileId: string,
  inlineProfile: InlineProfile | null,
  selectedProfile: ProfileDocument | undefined,
): ProfileSource {
  if (!inlineProfile) {
    return profileId
      ? { kind: "named", profileId }
      : { kind: "inline", profile: {} };
  }
  if (profileId && selectedProfile) {
    const original = inlineProfileFromDocument(selectedProfile);
    if (JSON.stringify(original) === JSON.stringify(inlineProfile)) {
      return { kind: "named", profileId };
    }
  }
  return { kind: "inline", profile: inlineProfile };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
