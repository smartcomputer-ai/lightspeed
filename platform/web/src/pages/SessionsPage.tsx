import { useEffect, useLayoutEffect, useRef, useState, type FormEvent } from "react";
import {
  type InfiniteData,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { NavLink, useNavigate, useParams } from "react-router-dom";
import { Archive, ArrowLeft, Check, Copy, ListFilter, LoaderCircle, Plus, ShieldCheck, SlidersHorizontal, Trash2 } from "lucide-react";
import {
  api,
  type Environment,
  type InlineProfile,
  type ProfileDocument,
  type ProfileSource,
  type ProfileSummary,
  type SessionListPage,
  type SessionRunAccepted,
  type SessionSummary,
  type SessionView,
} from "@/api";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
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
import { ProfileEnvironmentEditor } from "@/components/session/profile-environment-editor";
import { SessionConfigEditor } from "@/components/session/session-config-editor";
import { SessionSettingsDialog } from "@/components/session/session-settings-sheet";
import { SetupEditorSection } from "@/components/session/setup-editor-section";
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
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
  useMessageScroller,
  useMessageScrollerScrollable,
} from "@/components/ui/message-scroller";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SessionComposer } from "@/components/session/composer";
import {
  ActiveRunMarker,
  TranscriptEntryView,
  UserBand,
} from "@/components/session/transcript-view";
import { CenteredNote, LoadingNote, UniverseNotFound } from "@/components/page";
import { useSessionTail } from "@/lib/sessions/tail";
import {
  isTerminalToolStatus,
  type ActiveRun,
  type TranscriptEntry,
} from "@/lib/sessions/transcript";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import { managedSessionOwnerLabel } from "@/lib/sessions/management";
import {
  hasSessionFeature,
  resourceFeatureDisableReasons,
  setupResourceFeatureError,
} from "@/lib/sessions/resource-features";
import { canManage, useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";

/// U4a+U4d: master-detail session chat. Pane = paged session list plus
/// New session (sub-agent tree expansion arrives with engine D1 parent
/// linkage); detail = live transcript (long-poll tail) with a composer.
export function SessionsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { sessionId } = useParams<{ sessionId: string }>();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 min-w-0 max-w-full flex-1">
      <aside
        className={cn(
          "w-full shrink-0 flex-col border-r md:flex md:w-80",
          sessionId ? "hidden" : "flex",
        )}
      >
        <SessionList universeId={universe.id} slug={slug!} activeId={sessionId} />
      </aside>
      <section className={cn("min-w-0 flex-1 flex-col", sessionId ? "flex" : "hidden md:flex")}>
        {sessionId ? (
          <SessionDetail
            key={sessionId}
            universeId={universe.id}
            slug={slug!}
            sessionId={sessionId}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
            Select a session, or start a new one.
          </div>
        )}
      </section>
    </div>
  );
}

function SessionList({
  universeId,
  slug,
  activeId,
}: {
  universeId: string;
  slug: string;
  activeId: string | undefined;
}) {
  const pages = useInfiniteQuery({
    queryKey: ["sessions", universeId],
    queryFn: ({ pageParam }) =>
      api<SessionListPage>(
        "GET",
        `/api/v1/universes/${universeId}/sessions?limit=50${
          pageParam ? `&cursor=${encodeURIComponent(pageParam)}` : ""
        }`,
      ),
    initialPageParam: "",
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
  const [createOpen, setCreateOpen] = useState(false);
  const [showClosed, setShowClosed] = useState(true);

  const allSessions = pages.data?.pages.flatMap((page) => page.sessions) ?? [];
  const sessions = showClosed
    ? allSessions
    : allSessions.filter((session) => session.lifecycleStatus !== "closed");

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <h1 className="text-sm font-semibold">Sessions</h1>
        <span className="text-xs text-muted-foreground">
          {sessions.length}
          {pages.hasNextPage ? "+" : ""}
        </span>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                className={cn("ml-auto", !showClosed && "text-primary")}
                aria-label="Session list settings"
              />
            }
          >
            <ListFilter />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-48">
            <DropdownMenuGroup>
              <DropdownMenuLabel>List settings</DropdownMenuLabel>
              <DropdownMenuCheckboxItem
                checked={showClosed}
                onCheckedChange={(checked) => setShowClosed(checked === true)}
              >
                Show closed sessions
              </DropdownMenuCheckboxItem>
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => setCreateOpen(true)}
          aria-label="New session"
        >
          <Plus />
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {pages.isLoading && <p className="p-4 text-sm text-muted-foreground">Loading…</p>}
        {pages.error && (
          <p className="p-4 text-sm text-destructive">{pages.error.message}</p>
        )}
        {pages.data && allSessions.length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            No sessions yet — start one, or bind a chat.
          </p>
        )}
        {pages.data && !showClosed && allSessions.length > 0 && sessions.length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            No open sessions in the loaded results.
          </p>
        )}
        <ul>
          {sessions.map((session) => (
            <SessionListItem
              key={session.id}
              session={session}
              slug={slug}
              active={session.id === activeId}
            />
          ))}
        </ul>
        {pages.hasNextPage && (
          <div className="p-3">
            <Button
              variant="outline"
              size="sm"
              className="w-full"
              disabled={pages.isFetchingNextPage}
              onClick={() => void pages.fetchNextPage()}
            >
              {pages.isFetchingNextPage ? "Loading…" : "Load more"}
            </Button>
          </div>
        )}
      </div>
      <NewSessionDialog
        universeId={universeId}
        slug={slug}
        open={createOpen}
        onOpenChange={setCreateOpen}
      />
    </>
  );
}

function SessionListItem({
  session,
  slug,
  active,
}: {
  session: SessionSummary;
  slug: string;
  active: boolean;
}) {
  return (
    <li>
      <NavLink
        to={`/u/${slug}/sessions/${session.id}`}
        className={cn(
          "flex flex-col gap-0.5 border-b px-4 py-2.5 text-sm hover:bg-muted/50",
          active && "bg-muted",
        )}
      >
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate font-medium">
            {session.displayName ?? session.id.slice(0, 18)}
          </span>
          {session.lifecycleStatus === "closed" && (
            <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
              closed
            </span>
          )}
          {session.managed && <Badge variant="secondary">Managed</Badge>}
        </span>
        <span className="flex gap-2 font-mono text-xs text-muted-foreground">
          <span className="truncate">{session.id.slice(0, 14)}…</span>
          <span className="ml-auto shrink-0 font-sans">
            {relativeTime(session.updatedAtMs)}
          </span>
        </span>
      </NavLink>
    </li>
  );
}

function NewSessionDialog({
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
  const [displayName, setDisplayName] = useState("");
  const [profileId, setProfileId] = useState("");
  const [step, setStep] = useState<"basics" | "setup">("basics");
  const [inlineProfile, setInlineProfile] = useState<InlineProfile | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
    enabled: open,
  });
  const selectedProfile = useQuery({
    queryKey: ["profile", universeId, profileId],
    queryFn: () =>
      api<ProfileDocument>("GET", `/api/v1/universes/${universeId}/profiles/${profileId}`),
    enabled: open && Boolean(profileId),
  });
  const editorOptions = useSessionConfigEditorOptions(universeId, open && step === "setup");
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled: open && step === "setup",
  });
  const create = useMutation({
    mutationFn: () =>
      api<SessionView>("POST", `/api/v1/universes/${universeId}/sessions`, {
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        profile: profileForCreate(profileId, inlineProfile, selectedProfile.data),
      }),
    onSuccess: async (session) => {
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
      onOpenChange(false);
      const target = session.id;
      setDisplayName("");
      setProfileId("");
      setStep("basics");
      setInlineProfile(null);
      setConfigError(null);
      setError(null);
      navigate(`/u/${slug}/sessions/${target}`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const resourceError = setupResourceFeatureError(
      inlineProfile ?? selectedProfile.data ?? {},
    );
    if (configError || resourceError) {
      setError(configError ? `Config: ${configError}` : resourceError);
      return;
    }
    create.mutate();
  };

  const changeOpen = (next: boolean) => {
    onOpenChange(next);
    if (!next && !create.isPending) {
      setDisplayName("");
      setProfileId("");
      setStep("basics");
      setInlineProfile(null);
      setConfigError(null);
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
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogContent
        className={step === "setup"
          ? "h-[min(92dvh,900px)] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 p-0 sm:max-w-4xl"
          : undefined}
      >
        {step === "basics" ? (
          <>
            <DialogHeader>
              <DialogTitle>New session</DialogTitle>
              <DialogDescription>
                Start from a named profile or customize an inline setup for this session.
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
                    setError(null);
                  }}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue>
                      {(value: string) =>
                        value
                          ? (profiles.data?.find((p) => p.profileId === value)?.displayName ?? value)
                          : "No profile (engine defaults)"
                      }
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="">No profile (engine defaults)</SelectItem>
                    {(profiles.data ?? []).map((profile) => (
                      <SelectItem key={profile.profileId} value={profile.profileId}>
                        {profile.displayName ?? profile.profileId}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <FieldDescription>
                  The profile is resolved at creation; later profile edits do not change this session.
                </FieldDescription>
              </Field>
              <Button
                type="button"
                variant="outline"
                disabled={Boolean(profileId) && selectedProfile.isLoading}
                onClick={customize}
              >
                {inlineProfile ? "Edit customized setup" : "Customize setup…"}
              </Button>
              {selectedProfile.error && (
                <p className="text-sm text-destructive">{selectedProfile.error.message}</p>
              )}
              {error && <p className="text-sm text-destructive">{error}</p>}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={() => changeOpen(false)}>
                  Cancel
                </Button>
                <Button type="submit" disabled={create.isPending}>
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
            <div className="min-h-0 overflow-y-auto p-6">
              <InlineSetupEditor
                value={inlineProfile ?? {}}
                options={editorOptions}
                environments={environments.data}
                onValidityChange={setConfigError}
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
                  disabled={create.isPending || Boolean(configError || resourceFeatureError)}
                  onClick={() => create.mutate()}
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
  value,
  options,
  environments,
  onValidityChange,
  onChange,
}: {
  value: InlineProfile;
  options: ReturnType<typeof useSessionConfigEditorOptions>;
  environments: Environment[] | undefined;
  onValidityChange: (message: string | null) => void;
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
        <textarea
          className="min-h-32 w-full resize-y rounded-lg border border-input bg-transparent p-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
          value={instructions}
          onChange={(event) => change((next) => {
            if (event.target.value) next.instructions = { type: "text", text: event.target.value };
            else delete next.instructions;
          })}
          spellCheck={false}
        />
      </SetupEditorSection>
      <SetupEditorSection
        title="Session config"
        description="Sparse behavior and capability grants. Unset values inherit engine defaults."
      >
        <SessionConfigEditor
          value={value.config}
          mcpServers={options.mcpServers}
          workspaces={options.workspaces}
          workspacesLoading={options.workspacesLoading}
          models={options.models}
          profiles={options.profiles}
          environmentProviders={options.environmentProviders}
          featureDisableReasons={resourceFeatureDisableReasons(value)}
          onValidityChange={onValidityChange}
          onChange={(config) => change((next) => {
            if (config) next.config = config;
            else delete next.config;
          })}
        />
      </SetupEditorSection>
      <ProfileEnvironmentEditor
        value={value.activeEnvironmentId}
        environments={environments}
        disabled={!hasSessionFeature(value.config, "environments")}
        onChange={(environmentId) => change((next) => {
          if (environmentId) next.activeEnvironmentId = environmentId;
          else delete next.activeEnvironmentId;
        })}
      />
    </div>
  );
}

function inlineProfileFromDocument(document: ProfileDocument): InlineProfile {
  const profile: InlineProfile = {};
  if (isRecord(document.config)) profile.config = structuredClone(document.config);
  if (isRecord(document.instructions)) profile.instructions = structuredClone(document.instructions) as InlineProfile["instructions"];
  if (document.activeEnvironmentId) profile.activeEnvironmentId = document.activeEnvironmentId;
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

export function SessionDetail({
  universeId,
  slug,
  sessionId,
  backTo = `/u/${slug}/sessions`,
}: {
  universeId: string;
  slug: string;
  sessionId: string;
  backTo?: string;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const tail = useSessionTail(universeId, sessionId);
  const session = useQuery({
    queryKey: ["session", universeId, sessionId],
    queryFn: () =>
      api<SessionView>(
        "GET",
        `/api/v1/universes/${universeId}/sessions/${sessionId}`,
      ),
  });
  const [pending, setPending] = useState<{ id: string; text: string }[]>([]);
  const [sendError, setSendError] = useState<string | null>(null);
  const [sessionIdCopied, setSessionIdCopied] = useState(false);
  const [closeError, setCloseError] = useState<string | null>(null);
  const [closeOpen, setCloseOpen] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

  const entries = tail.transcript.entries;
  const activeRun = tail.transcript.activeRun;
  const activeToolGroup = entries.some(
    (entry) => entry.kind === "tool-group" && !isTerminalToolStatus(entry.status),
  );

  // Reconcile the optimistic echo: once the engine's own userMessage item
  // arrives with the same content, drop the pending copy (TUI pattern —
  // match on content, the item id is engine-minted).
  useEffect(() => {
    setPending((prev) => {
      if (prev.length === 0) {
        return prev;
      }
      const confirmed = new Set(
        entries
          .filter((entry) => entry.kind === "message" && entry.role === "user")
          .map((entry) => (entry as { text: string }).text.trim()),
      );
      const next = prev.filter((message) => !confirmed.has(message.text.trim()));
      return next.length === prev.length ? prev : next;
    });
  }, [entries]);

  const runActive = activeRun !== null || pending.length > 0;
  const closed = session.data?.status === "closed";
  const management = session.data?.management;
  const managed = session.data?.managed === true;
  const foundryManaged =
    management?.lifecycleController?.workflowKind === "foundryPackWorkflowV1";
  const managerLabel = managedSessionOwnerLabel(management);

  useEffect(() => {
    if (!sessionIdCopied) return;
    const timer = window.setTimeout(() => setSessionIdCopied(false), 1_500);
    return () => window.clearTimeout(timer);
  }, [sessionIdCopied]);

  useEffect(() => {
    if (settingsOpen && !runActive) {
      void session.refetch();
    }
  }, [settingsOpen, runActive]);

  const send = async (text: string) => {
    // The submission id doubles as the engine idempotency key: a retried
    // POST returns the original run instead of starting a second one.
    const submissionId = crypto.randomUUID();
    setSendError(null);
    setPending((prev) => [...prev, { id: submissionId, text }]);
    try {
      await api<SessionRunAccepted>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/messages`,
        { text, submissionId },
      );
    } catch (error) {
      setPending((prev) => prev.filter((message) => message.id !== submissionId));
      setSendError(error instanceof Error ? error.message : String(error));
    }
  };

  const stop = async () => {
    if (!activeRun) {
      return;
    }
    try {
      await api(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${activeRun.runId}/cancel`,
        {},
      );
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
    }
  };

  const closeSession = useMutation({
    mutationFn: () =>
      api<SessionView>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/close`,
        { force: true },
      ),
    onSuccess: async (closedSession) => {
      setCloseOpen(false);
      queryClient.setQueryData(
        ["session", universeId, sessionId],
        closedSession,
      );
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
    },
    onError: (error) => setCloseError(error.message),
  });

  const deleteSession = useMutation({
    mutationFn: () =>
      api<SessionSummary>(
        "DELETE",
        `/api/v1/universes/${universeId}/sessions/${sessionId}`,
      ),
    onSuccess: async () => {
      setDeleteOpen(false);
      queryClient.setQueryData<InfiniteData<SessionListPage>>(
        ["sessions", universeId],
        (current) => current
          ? {
              ...current,
              pages: current.pages.map((page) => ({
                ...page,
                sessions: page.sessions.filter((candidate) => candidate.id !== sessionId),
              })),
            }
          : current,
      );
      navigate(`/u/${slug}/sessions`);
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
    },
    onError: (error) => setDeleteError(error.message),
  });

  return (
    <>
      <header className="flex h-12 shrink-0 items-center gap-3 border-b px-4">
        <NavLink to={backTo} className="md:hidden">
          <ArrowLeft className="size-4" />
        </NavLink>
        <h1 className="min-w-0 truncate text-sm font-semibold">
          {session.data?.displayName ?? sessionId.slice(0, 24)}
        </h1>
        {closed && (
          <span className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
            Closed
          </span>
        )}
        {managed && (
          <Tooltip>
            <TooltipTrigger
              render={
                foundryManaged
                  ? <span />
                  : <button type="button" onClick={() => setSettingsOpen(true)} />
              }
            >
              <Badge variant="secondary" className="gap-1">
                <ShieldCheck /> Managed by {managerLabel}
              </Badge>
            </TooltipTrigger>
            <TooltipContent>
              {foundryManaged
                ? "Foundry owns lifecycle and event delivery; operators can chat with this manager directly."
                : `Lifecycle and chat input are controlled by ${managerLabel}; configuration remains editable.`}
            </TooltipContent>
          </Tooltip>
        )}
        <div className="ml-auto flex items-center gap-1">
          {!closed && !managed && (
            <AlertDialog
              open={closeOpen}
              onOpenChange={(open) => {
                setCloseOpen(open);
                if (open) setCloseError(null);
              }}
            >
              <AlertDialogTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-destructive"
                    disabled={closeSession.isPending}
                    aria-label={runActive ? "Force close session" : "Close session"}
                    title={runActive
                      ? "Cancel active work and permanently close this session"
                      : "Permanently close this session"}
                  />
                }
              >
                {closeSession.isPending
                  ? <LoaderCircle className="animate-spin" />
                  : <Archive />}
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>
                    {runActive ? "Force close this session?" : "Close this session?"}
                  </AlertDialogTitle>
                  <AlertDialogDescription>
                    {runActive
                      ? "This cancels active and queued work, then permanently closes the session. Recovery of a stuck workflow can take up to about 90 seconds while the engine terminates it and reconciles the session. The history remains available, but the session cannot be reopened."
                      : "This permanently closes the session. It remains in the session list so its history can be inspected, but it cannot be reopened."}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                {closeSession.isPending && (
                  <p className="text-sm text-muted-foreground">
                    Force close is running in the background. You can hide this dialog and
                    continue using the app.
                  </p>
                )}
                {closeError && <p className="text-sm text-destructive">{closeError}</p>}
                <AlertDialogFooter>
                  <AlertDialogCancel>
                    {closeSession.isPending ? "Hide" : "Cancel"}
                  </AlertDialogCancel>
                  <AlertDialogAction
                    className="bg-destructive text-white hover:bg-destructive/90"
                    disabled={closeSession.isPending}
                    onClick={() => closeSession.mutate()}
                  >
                    {closeSession.isPending
                      ? "Force-closing…"
                      : runActive
                        ? "Force close session"
                        : "Close session"}
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}
          {closed && !managed && (
            <AlertDialog
              open={deleteOpen}
              onOpenChange={(open) => {
                setDeleteOpen(open);
                if (open) setDeleteError(null);
              }}
            >
              <AlertDialogTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-destructive"
                    aria-label="Delete session"
                  />
                }
              >
                <Trash2 />
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete this session permanently?</AlertDialogTitle>
                  <AlertDialogDescription>
                    This removes the session and its retained history. It cannot be undone.
                    Sessions with forks that still inherit their history must be deleted
                    leaf-first.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                {deleteError && <p className="text-sm text-destructive">{deleteError}</p>}
                <AlertDialogFooter>
                  <AlertDialogCancel disabled={deleteSession.isPending}>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    className="bg-destructive text-white hover:bg-destructive/90"
                    disabled={deleteSession.isPending}
                    onClick={() => deleteSession.mutate()}
                  >
                    {deleteSession.isPending ? "Deleting…" : "Delete permanently"}
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}
          {!foundryManaged && (
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Session settings"
              onClick={() => setSettingsOpen(true)}
            >
              <SlidersHorizontal />
            </Button>
          )}
        </div>
        {activeRun && !activeToolGroup && (
          <span className="shrink-0 text-xs text-muted-foreground">{activeRun.label}…</span>
        )}
        <Button
          variant="ghost"
          size="xs"
          className="shrink-0 gap-1.5 px-2 font-mono text-xs text-muted-foreground"
          aria-label={sessionIdCopied ? "Session ID copied" : "Copy session ID"}
          title={sessionIdCopied ? "Copied" : `Copy ${sessionId}`}
          onClick={() => {
            void navigator.clipboard
              .writeText(sessionId)
              .then(() => setSessionIdCopied(true))
              .catch(() => undefined);
          }}
        >
          {sessionIdCopied ? "Copied" : `${sessionId.slice(0, 18)}…`}
          {sessionIdCopied ? <Check /> : <Copy />}
        </Button>
      </header>
      <MessageScrollerProvider autoScroll defaultScrollPosition="end">
        <MessageScroller className="min-h-0 flex-1">
          <MessageScrollerViewport>
            <MessageScrollerContent className="gap-3 px-4 py-6 md:px-8">
              {tail.phase === "loading" && entries.length === 0 && !tail.error && (
                <LoadingNote />
              )}
              {tail.error && entries.length === 0 && (
                <p className="text-sm text-destructive">{tail.error}</p>
              )}
              {tail.truncated && (
                <p className="text-center text-xs text-muted-foreground">
                  Very long session — a stretch of older events was skipped.
                </p>
              )}
              {tail.phase === "live" &&
                entries.length === 0 &&
                pending.length === 0 && (
                  <CenteredNote>No conversation yet — say something below.</CenteredNote>
                )}
              {entries.map((entry) => (
                <MessageScrollerItem
                  key={entry.key}
                  messageId={entry.key}
                >
                  <TranscriptEntryView entry={entry} />
                </MessageScrollerItem>
              ))}
              {pending.map((message) => (
                <MessageScrollerItem key={message.id} messageId={message.id}>
                  <UserBand text={message.text} pending />
                </MessageScrollerItem>
              ))}
              {activeRun && !activeToolGroup && (
                <MessageScrollerItem messageId="active-run">
                  <ActiveRunMarker run={activeRun} />
                </MessageScrollerItem>
              )}
              {tail.error && entries.length > 0 && (
                <p className="text-center text-xs text-destructive">
                  Connection lost — retrying. ({tail.error})
                </p>
              )}
            </MessageScrollerContent>
          </MessageScrollerViewport>
          <MessageScrollerButton />
        </MessageScroller>
        <SessionScrollFollower
          ready={tail.phase === "live"}
          entries={entries}
          pending={pending}
          activeRun={activeRun}
        />
      </MessageScrollerProvider>
      <SessionComposer
        runActive={runActive}
        disabled={closed || (managed && !foundryManaged)}
        disabledReason={managed && !foundryManaged
          ? `Managed by ${managerLabel} — send messages through the connected channel.`
          : undefined}
        error={sendError}
        onSend={(text) => void send(text)}
        onStop={() => void stop()}
      />
      <SessionSettingsDialog
        universeId={universeId}
        sessionId={sessionId}
        session={session.data}
        runActive={runActive}
        open={settingsOpen}
        onOpenChange={(open) => {
          setSettingsOpen(open);
          if (open) void session.refetch();
        }}
      />
    </>
  );
}

function SessionScrollFollower({
  ready,
  entries,
  pending,
  activeRun,
}: {
  ready: boolean;
  entries: TranscriptEntry[];
  pending: { id: string; text: string }[];
  activeRun: ActiveRun | null;
}) {
  const { scrollToEnd } = useMessageScroller();
  const scrollable = useMessageScrollerScrollable();
  const initialized = useRef(false);

  useLayoutEffect(() => {
    if (!ready) {
      return;
    }

    // On open, always start at the latest message. After that, append only
    // follows when the viewport was already at its end before this render;
    // a reader who scrolled up keeps their position.
    if (!initialized.current || !scrollable.end) {
      initialized.current = true;
      scrollToEnd({ behavior: "auto" });
    }
  }, [ready, entries, pending, activeRun, scrollable.end, scrollToEnd]);

  return null;
}

function relativeTime(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h`;
  return `${Math.floor(delta / 86_400_000)}d`;
}
