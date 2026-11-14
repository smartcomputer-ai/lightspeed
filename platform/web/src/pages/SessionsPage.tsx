import { DeleteSessionDialog } from "@/components/session/delete-session-dialog";
import { useSessionCompaction } from "@/lib/sessions/compaction";
import { useDictationAvailability } from "@/lib/use-dictation";
import { ShareSessionDialog, useCanShareSession, useSessionOwner } from "@/components/session/sharing";
import { ActivityDot } from "@/components/activity-dot";
import { SessionActionsMenu } from "@/components/session/session-actions-menu";
import { requestRowMenu } from "@/lib/row-menu";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent } from "react";
import {
  type InfiniteData,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useLocation, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { AddIcon, BotIcon, DeleteIcon, FilterIcon, LoadingIcon, RemoveIcon, SelectModeIcon, SessionCloseIcon, SessionIcon } from "@/icons";
import {
  api,
  botLabel,
  type BotListResponse,
  type InlineProfile,
  type ProfileDocument,
  type ProfileSource,
  type ProfileSummary,
  type SessionListPage,
  type SessionRunAccepted,
  type SessionRunApprovalsDecided,
  SessionRunCancelled,
  SessionRunSteered,
  SessionRunView,
  type Environment,
  type SessionSummary,
  type SessionView,
} from "@/api";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { MetadataMapEditor } from "@/components/session/metadata-editor";
import { useUserPreferences } from "@/lib/user-preferences";
import { ProfileRetentionEditor } from "@/components/session/profile-retention-editor";
import { SessionConfigEditor } from "@/components/session/session-config-editor";
import { SessionSettingsDialog } from "@/components/session/session-settings-sheet";
import { SessionHistoryLoader } from "@/components/session/history-loader";
import { SetupEditorSection } from "@/components/session/setup-editor-section";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DIALOG_WARP_MS,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
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
import { sessionDraftKey } from "@/lib/sessions/draft";
import { SessionComposer, type ComposerMessage, type ComposerMode } from "@/components/session/composer";
import { activeEnvironmentItems, sessionResourceItems, useSessionResources } from "@/components/session/session-resources";
import { openEnvironmentsQuery } from "@/lib/environments";
import { hasLineage, SESSION_META, sessionMetaItems, useSessionLineage } from "@/components/session/session-meta";
import { MetaToolbar } from "@/components/meta-toolbar";
import { hasResources } from "@/lib/sessions/session-resources";
import { findFileReference, filesByHandle } from "@/lib/file-references";
import { appHref, blobHref } from "@/lib/blob-view";
import { Switch } from "@/components/ui/switch";
import {
  ApprovalCards,
  QueuedRunsBar,
  SystemChips,
  TranscriptEntryView,
  UserBand,
  type QueuedRunItem,
} from "@/components/session/transcript-view";
import { RunSectionView } from "@/components/session/run-section";
import { TranscriptEntrance, TranscriptMotionProvider } from "@/components/session/transcript-motion";
import { TranscriptLinksContext, type TranscriptLinks } from "@/components/session/tool-trace";
import { sectionsByRun, withPendingRunInputs } from "@/lib/sessions/run-sections";
import { DetailPrompt, LoadingNote, UniverseNotFound } from "@/components/page";
import { useCreateParam } from "@/lib/create-param";
import { ReadError } from "@/components/read-error";
import { fetchHistory, useSessionTail } from "@/lib/sessions/tail";
import {
  mediaByHandle,
  mediaHandleFor,
  runInProgress,
  type ActiveRun,
  type TranscriptEntry,
  type TranscriptMedia,
} from "@/lib/sessions/transcript";
import type { SentAttachment } from "@/lib/composer-attachments";
import type { RunView } from "@lightspeed-ai/sdk";
import { postUntilAccepted } from "@/lib/sessions/held-request";
import { queuedMessage, queuedRecoverySubmissionId, readQueuedRecovery, steeringWasNotSubmitted, storeQueuedRecovery, type QueuedMessageRecovery } from "@/lib/sessions/queued-message";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import { controlledSessionBotId, controlledSessionOwnerLabel } from "@/lib/sessions/lifecycle-controller";
import {
  resourceFeatureDisableReasons,
  setupResourceFeatureError,
} from "@/lib/sessions/resource-features";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { modelFromConfig, modelLabel, resolveCreationModel, useModelDefaults, useModelDiscovery } from "@/lib/model-defaults";
import { useActionPermissions, useSessionClosePermission, useSessionDeletePermission } from "@/lib/permissions";
import { useActiveUniverse, useFeature } from "@/lib/universes";
import { cn } from "@/lib/utils";
import {
  metadataFilterFromSearchParams,
  parseMetadataPair,
  readSessionMetadataFilter,
  readSessionListPreferences,
  hiddenSessionsNote,
  searchParamsWithMetadataFilter,
  writeSessionMetadataFilter,
  writeSessionListPreferences,
} from "@/lib/sessions/list-preferences";
import { DetailHeader } from "@/components/detail-header";
import { useDocumentTitle } from "@/lib/document-title";
import {
  LIST_CHECKBOX_INSET_CLASS,
  ListPane,
  ListPaneBody,
  ListPaneFooter,
  ListPaneHeader,
  ListPaneState,
  ListPaneToolbar,
  ListRow,
  ListRowLine,
  ListSelectionButtons,
  type ListRowTree,
  type ListSelectionAction,
} from "@/components/list-pane";

/// U4a+U4d: master-detail session chat. Pane = paged session list plus
/// New session (sub-agent tree expansion arrives with engine D1 parent
/// linkage); detail = live transcript (long-poll tail) with a composer.
const SESSION_LIST_REFRESH_MS = 5_000;
/// A dead workflow emits nothing, so a working session is re-read to notice it.
const SESSION_LIVENESS_REFRESH_MS = 15_000;
/// The strip of tags under a session header (the meta toolbar in full). Off
/// for now: the header beside the title shows the same items, expanding to
/// tags on hover.
const SHOW_META_STRIP = false;
const sessionMayBeRunning = (session?: Pick<SessionView, "lifecycleStatus" | "activity" | "executionFailure">) =>
  session?.lifecycleStatus !== "closed" && session?.activity !== undefined && session.activity !== "idle" && !session.executionFailure;
const EXECUTION_FAILURE_TEXT: Record<NonNullable<SessionView["executionFailure"]>, string> = {
  failed: "This session's workflow failed.",
  terminated: "This session's workflow was stopped.",
  timedOut: "This session's workflow timed out.",
  missing: "This session's workflow no longer exists.",
};

export function SessionsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { sessionId } = useParams<{ sessionId: string }>();
  const location = useLocation();
  const canCreate = useActionPermissions(universe?.id).can("createSession");
  const [, setCreateOpen] = useCreateParam("session");

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !universe.role) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 min-w-0 max-w-full flex-1">
      <ListPane detailOpen={Boolean(sessionId)}>
        <SessionList key={universe.id} universeId={universe.id} slug={slug!} activeId={sessionId} />
      </ListPane>
      <section className={cn("min-w-0 flex-1 flex-col", sessionId ? "flex" : "hidden md:flex")}>
        {!sessionId && <ProviderReadinessBanner universeId={universe.id} slug={slug!} />}
        {sessionId ? (
          <SessionDetail
            key={sessionDraftKey(universe.id, sessionId)}
            universeId={universe.id}
            slug={slug!}
            sessionId={sessionId}
            backTo={`/u/${slug}/sessions${location.search}`}
            sessionHref={(target) => `/u/${slug}/sessions/${target}${location.search}`}
          />
        ) : (
          <DetailPrompt
            icon={<SessionIcon className="size-10 text-muted-foreground/60" />}
            create={canCreate ? { label: "New session", onClick: () => setCreateOpen(true) } : undefined}
          >
            Pick a session{canCreate ? ", or start one" : ""}.
          </DetailPrompt>
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
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const metadataFilter = metadataFilterFromSearchParams(searchParams);
  const [filterDraft, setFilterDraft] = useState("");
  const [metadataKeyDraft, setMetadataKeyDraft] = useState("");
  const filterEntries = Object.entries(metadataFilter);
  const [preferences, setPreferences] = useState(() => readSessionListPreferences(universeId));
  const { showClosed, showSubagents, showControlledSessions, showSessionIds, metadataKeys } = preferences;
  // One read for the whole list: each row's environment by name and status.
  const environments = useQuery({ ...openEnvironmentsQuery(universeId), refetchInterval: 30_000 });
  const listQuery = new URLSearchParams({ limit: "50" });
  if (!showClosed) listQuery.set("closed", "false");
  if (!showControlledSessions) listQuery.set("lifecycleControlled", "false");
  if (!showSubagents) listQuery.set("subagent", "false");
  for (const [key, value] of filterEntries) {
    listQuery.append("metadata", value ? `${key}=${value}` : key);
  }
  const pages = useInfiniteQuery({
    queryKey: ["sessions", universeId, metadataFilter, { showClosed, showControlledSessions, showSubagents }],
    queryFn: ({ pageParam }) =>
      api<SessionListPage>(
        "GET",
        `/api/v1/universes/${universeId}/sessions?${listQuery.toString()}${
          pageParam ? `&cursor=${encodeURIComponent(pageParam)}` : ""
        }`,
    ),
    initialPageParam: "",
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    // Sessions can be created by runtime workflows (not only by this browser),
    // so frontend mutation invalidation alone cannot keep this list current.
    refetchInterval: SESSION_LIST_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const [createOpen, setCreateOpen] = useCreateParam("session");
  // The new-session dialog stays mounted until it has finished closing, so
  // its close (and a warp's fade-out) can play.
  const [createShown, setCreateShown] = useState(createOpen);
  useEffect(() => {
    if (createOpen) setCreateShown(true);
  }, [createOpen]);
  const [selecting, setSelecting] = useState(false);
  const [selectingAll, setSelectingAll] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [bulkNotice, setBulkNotice] = useState<string | null>(null);
  // The bulk action awaiting confirmation, from the selection bar or a selected row's menu.
  const [confirming, setConfirming] = useState<"close" | "delete" | null>(null);

  const sessions = pages.data?.pages.flatMap((page) => page.sessions) ?? [];
  const tree = buildSessionTree(sessions);
  const visibleIds = sessions.map((session) => session.id);
  const selectedSessions = sessions.filter((session) => selected.has(session.id));
  const permissions = useActionPermissions(universeId);
  const mayClose = useSessionClosePermission(universeId);
  const mayDelete = useSessionDeletePermission(universeId);
  const canCreate = permissions.can("createSession");
  const canSelect = sessions.some((session) => session.lifecycleStatus === "closed" ? mayDelete(session.access) : (!session.lifecycleControlled || permissions.role === "admin") && mayClose(session.access));
  const selectedOpen = selectedSessions.filter((session) => (!session.lifecycleControlled || permissions.role === "admin") && session.lifecycleStatus !== "closed" && mayClose(session.access));
  const selectedClosed = selectedSessions.filter((session) => session.lifecycleStatus === "closed" && mayDelete(session.access));
  const allVisibleSelected = visibleIds.length > 0 && visibleIds.every((id) => selected.has(id));
  // Only metadata filters count; which sessions the list includes is its scope.
  const activeFilterCount = filterEntries.length;
  const listSearch = searchParams.toString();
  const restoredFilterUniverse = useRef<string | null>(null);

  useEffect(() => {
    writeSessionListPreferences(universeId, preferences);
  }, [preferences, universeId]);

  useEffect(() => {
    if (restoredFilterUniverse.current !== universeId) {
      restoredFilterUniverse.current = universeId;
      if (searchParams.has("metadata")) {
        writeSessionMetadataFilter(universeId, metadataFilter);
        return;
      }
      const stored = readSessionMetadataFilter(universeId);
      if (Object.keys(stored).length > 0) {
        setSearchParams(searchParamsWithMetadataFilter(searchParams, stored), { replace: true });
      }
      return;
    }
    writeSessionMetadataFilter(universeId, metadataFilter);
  }, [metadataFilter, searchParams, setSearchParams, universeId]);

  const updateMetadataFilter = (next: Record<string, string>) => {
    writeSessionMetadataFilter(universeId, next);
    setSearchParams(searchParamsWithMetadataFilter(searchParams, next), { replace: true });
    setSelected(new Set());
  };

  const addFilter = (key: string, value: string) => {
    updateMetadataFilter({ ...metadataFilter, [key]: value });
  };
  const removeFilter = (key: string) => {
    const next = { ...metadataFilter };
    delete next[key];
    updateMetadataFilter(next);
  };
  const submitFilter = (event: FormEvent) => {
    event.preventDefault();
    const pair = parseMetadataPair(filterDraft);
    if (!pair) return;
    addFilter(pair.key, pair.value);
    setFilterDraft("");
  };
  const submitMetadataKey = (event: FormEvent) => {
    event.preventDefault();
    const key = metadataKeyDraft.trim();
    if (!key || metadataKeys.includes(key)) return;
    setPreferences((current) => ({
      ...current,
      metadataKeys: [...current.metadataKeys, key],
    }));
    setMetadataKeyDraft("");
  };
  const removeMetadataKey = (key: string) => setPreferences((current) => ({
    ...current,
    metadataKeys: current.metadataKeys.filter((candidate) => candidate !== key),
  }));
  const toggleSelected = (id: string) =>
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const exitSelecting = () => {
    setSelecting(false);
    setSelectingAll(false);
    setSelected(new Set());
  };

  const selectAllMatching = async () => {
    setSelectingAll(true);
    setBulkNotice(null);
    try {
      let result = await pages.fetchNextPage();
      while (result.hasNextPage) result = await pages.fetchNextPage();
      const matches = (result.data?.pages.flatMap((page) => page.sessions) ?? [])
        .filter((session) => showClosed || session.lifecycleStatus !== "closed")
        .filter((session) => showSubagents || !session.origin);
      setSelected(new Set(matches.map((session) => session.id)));
    } catch (error) {
      setBulkNotice(error instanceof Error ? error.message : "Could not load all matching sessions.");
    } finally {
      setSelectingAll(false);
    }
  };

  /// The API has no bulk operation by design: the filtered list is the
  /// primitive and the client loops, a few requests at a time.
  const bulk = useMutation({
    meta: { blocking: true },
    mutationFn: async ({ action, ids }: { action: "close" | "delete"; ids: string[] }) => {
      const actionName = action === "close" ? "closeSession" : "deleteSession";
      if (!permissions.can(actionName)) throw new Error("Session permissions changed. Review the selection.");
      const results = await runBatched(ids, 6, (id): Promise<unknown> =>
        action === "close"
          ? api<SessionView>(
              "POST",
              `/api/v1/universes/${universeId}/sessions/${id}/close`,
              { force: true },
            )
          : api<SessionSummary>("DELETE", `/api/v1/universes/${universeId}/sessions/${id}`),
      );
      const failed = results.filter((result) => result.status === "rejected").length;
      return { action, done: ids.length - failed, failed };
    },
    onSuccess: (result) => {
      const verb = result.action === "close" ? "Closed" : "Deleted";
      setBulkNotice(
        `${verb} ${result.done} ${result.done === 1 ? "session" : "sessions"}${
          result.failed > 0 ? `, ${result.failed} failed` : ""
        }.`,
      );
      exitSelecting();
    },
    onError: (error) => setBulkNotice(error.message),
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
    },
  });
  // What can be done with the selection: the bar's buttons and a selected row's right-click menu.
  const bulkBusy = bulk.isPending || selectingAll;
  const selectionActions: ListSelectionAction[] = [
    { key: "close", label: "Close", icon: <SessionCloseIcon />, count: selectedOpen.length, destructive: true, disabled: bulkBusy, onSelect: () => setConfirming("close") },
    { key: "delete", label: "Delete", icon: <DeleteIcon />, count: selectedClosed.length, destructive: true, disabled: bulkBusy, onSelect: () => setConfirming("delete") },
  ];

  return (
    <>
      <ListPaneHeader
        title="Sessions"
        count={`${sessions.length}${pages.hasNextPage ? "+" : ""}`}
        actions={<>
            <Popover>
              <PopoverTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className={cn("relative", activeFilterCount > 0 && "text-primary")}
                    aria-label={activeFilterCount > 0
                      ? `Filter sessions, ${activeFilterCount} active`
                      : "Filter sessions"}
                  />
                }
              >
                <FilterIcon />
                {activeFilterCount > 0 && (
                  <span className="absolute -right-0.5 -top-0.5 flex size-4 items-center justify-center rounded-full bg-primary text-[9px] font-semibold text-primary-foreground">
                    {activeFilterCount}
                  </span>
                )}
              </PopoverTrigger>
              <PopoverContent align="end" className="grid gap-4 p-4">
                <h2 className="text-sm font-semibold">Filter sessions</h2>
                <div className="grid gap-2">
                  <span className="text-xs font-medium">Include</span>
                  <label className="flex cursor-action items-center gap-2 text-sm">
                    <Checkbox
                      checked={showClosed}
                      onCheckedChange={(checked) => setPreferences((current) => ({
                        ...current,
                        showClosed: checked === true,
                      }))}
                    />
                    Closed sessions
                  </label>
                  <label className="flex cursor-action items-center gap-2 text-sm">
                    <Checkbox
                      checked={showSubagents}
                      onCheckedChange={(checked) => setPreferences((current) => ({
                        ...current,
                        showSubagents: checked === true,
                      }))}
                    />
                    Sub-agent sessions
                  </label>
                  <label className="flex cursor-action items-center gap-2 text-sm">
                    <Checkbox
                      checked={showControlledSessions}
                      onCheckedChange={(checked) => setPreferences((current) => ({
                        ...current,
                        showControlledSessions: checked === true,
                      }))}
                    />
                    Managed sessions
                  </label>
                </div>
                <div className="grid gap-2 border-t pt-3">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-xs font-medium">Metadata filters</span>
                    {filterEntries.length > 0 && (
                      <button
                        type="button"
                        className="text-xs text-muted-foreground hover:text-foreground"
                        onClick={() => updateMetadataFilter({})}
                      >
                        Clear all
                      </button>
                    )}
                  </div>
                  {filterEntries.length > 0 && (
                    <div className="flex flex-wrap gap-1.5">
                      {filterEntries.map(([key, value]) => (
                        <Badge
                          key={key}
                          variant="secondary"
                          className="max-w-full gap-1 font-mono text-[11px]"
                        >
                          <span className="truncate">{value ? `${key}=${value}` : key}</span>
                          <button
                            type="button"
                            onClick={() => removeFilter(key)}
                            aria-label={`Remove filter ${key}`}
                            className="shrink-0 rounded hover:text-foreground"
                          >
                            <RemoveIcon className="size-3" />
                          </button>
                        </Badge>
                      ))}
                    </div>
                  )}
                  <form onSubmit={submitFilter} className="flex gap-2">
                    <Input
                      value={filterDraft}
                      onChange={(event) => setFilterDraft(event.target.value)}
                      placeholder="key or key=value"
                      aria-label="Metadata filter"
                      className="h-8 min-w-0 flex-1 font-mono text-xs"
                    />
                    <Button
                      type="submit"
                      variant="outline"
                      size="sm"
                      disabled={!parseMetadataPair(filterDraft)}
                    >
                      Add
                    </Button>
                  </form>
                  <p className="text-xs text-muted-foreground">
                    A key alone matches its presence. Key/value pairs match exactly.
                  </p>
                </div>
                <div className="grid gap-2 border-t pt-3">
                  <h2 className="text-sm font-semibold">Display</h2>
                  <label className="flex cursor-action items-center gap-2 text-sm">
                    <Checkbox
                      checked={showSessionIds}
                      onCheckedChange={(checked) => setPreferences((current) => ({
                        ...current,
                        showSessionIds: checked === true,
                      }))}
                    />
                    Show session IDs
                  </label>
                  <span className="mt-1 text-xs font-medium">Metadata keys to show</span>
                  {metadataKeys.length > 0 && (
                    <div className="flex flex-wrap gap-1.5">
                      {metadataKeys.map((key) => (
                        <Badge
                          key={key}
                          variant="secondary"
                          className="max-w-full gap-1 font-mono text-[11px]"
                        >
                          <span className="truncate">{key}</span>
                          <button
                            type="button"
                            onClick={() => removeMetadataKey(key)}
                            aria-label={`Stop showing metadata key ${key}`}
                            className="shrink-0 rounded hover:text-foreground"
                          >
                            <RemoveIcon className="size-3" />
                          </button>
                        </Badge>
                      ))}
                    </div>
                  )}
                  <form onSubmit={submitMetadataKey} className="flex gap-2">
                    <Input
                      value={metadataKeyDraft}
                      onChange={(event) => setMetadataKeyDraft(event.target.value)}
                      placeholder="key"
                      aria-label="Metadata key to show"
                      className="h-8 min-w-0 flex-1 font-mono text-xs"
                    />
                    <Button
                      type="submit"
                      variant="outline"
                      size="sm"
                      disabled={!metadataKeyDraft.trim() || metadataKeys.includes(metadataKeyDraft.trim())}
                    >
                      Add
                    </Button>
                  </form>
                </div>
              </PopoverContent>
            </Popover>
            {(canSelect || selecting) && <Button
              variant="ghost"
              size="icon-sm"
              // On while selecting, drawn like an open menu's button.
              aria-pressed={selecting}
              onClick={() => (selecting ? exitSelecting() : setSelecting(true))}
              aria-label={selecting ? "Exit selection" : "Select sessions"}
              title={selecting ? "Exit selection" : "Select sessions to close or delete"}
            >
              <SelectModeIcon />
            </Button>}
            {canCreate && <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => setCreateOpen(true)}
              aria-label="New session"
            >
              <AddIcon />
            </Button>}
        </>}
      />
      {selecting && (
        <ListPaneToolbar className="bg-muted/40">
          <Checkbox
            // In the rows' checkbox column.
            className={LIST_CHECKBOX_INSET_CLASS}
            checked={allVisibleSelected}
            disabled={selectingAll}
            onCheckedChange={(checked) =>
              setSelected(checked === true ? new Set(visibleIds) : new Set())
            }
            aria-label="Select all listed sessions"
          />
          <span className="text-muted-foreground">
            {selected.size} selected
            {pages.hasNextPage ? ` of ${sessions.length} loaded` : ""}
          </span>
          {pages.hasNextPage && (
            <button
              type="button"
              className="text-primary hover:underline disabled:opacity-50"
              disabled={selectingAll}
              onClick={() => void selectAllMatching()}
            >
              {selectingAll ? "Loading all…" : "Select all matching"}
            </button>
          )}
          <ListSelectionButtons actions={selectionActions} />
        </ListPaneToolbar>
      )}
      <BulkActionDialog
        action="close"
        count={selectedOpen.length}
        open={confirming === "close"}
        onOpenChange={(open) => setConfirming(open ? "close" : null)}
        onConfirm={() => bulk.mutate({ action: "close", ids: selectedOpen.map((session) => session.id) })}
      />
      <BulkActionDialog
        action="delete"
        count={selectedClosed.length}
        open={confirming === "delete"}
        onOpenChange={(open) => setConfirming(open ? "delete" : null)}
        onConfirm={() => bulk.mutate({ action: "delete", ids: selectedClosed.map((session) => session.id) })}
      />
      {bulkNotice && (
        <ListPaneToolbar className="py-1.5 text-muted-foreground">
          {bulk.isPending && <LoadingIcon className="size-3 animate-spin" />}
          {bulkNotice}
          <button
            type="button"
            className="ml-auto hover:text-foreground"
            onClick={() => setBulkNotice(null)}
            aria-label="Dismiss"
          >
            <RemoveIcon className="size-3" />
          </button>
        </ListPaneToolbar>
      )}
      <ListPaneBody>
        <ListPaneState
          loading={pages.isLoading}
          error={pages.error}
          hasData={Boolean(pages.data)}
          empty={sessions.length === 0 && (
            filterEntries.length > 0
              ? "No sessions match this metadata filter."
              : hiddenSessionsNote(preferences)
                ? `No sessions to show. ${hiddenSessionsNote(preferences)}`
                : "No sessions yet."
          )}
        />
        <ul>
          {tree.map((node) => (
            <SessionTreeItem
              key={node.session.id}
              node={node}
              slug={slug}
              activeId={activeId}
              depth={0}
              selecting={selecting}
              selected={selected}
              onToggle={toggleSelected}
              selectionActions={selectionActions}
              showSessionIds={showSessionIds}
              metadataKeys={metadataKeys}
              search={listSearch}
              environments={environments.data}
            />
          ))}
        </ul>
      </ListPaneBody>
      {pages.hasNextPage && (
        <ListPaneFooter>
          <Button
            variant="outline"
            size="sm"
            className="w-full"
            disabled={pages.isFetchingNextPage}
            onClick={() => void pages.fetchNextPage()}
          >
            {pages.isFetchingNextPage ? "Loading…" : "Load more sessions"}
          </Button>
        </ListPaneFooter>
      )}
      {canCreate && (createOpen || createShown) && <NewSessionDialog
        universeId={universeId}
        slug={slug}
        open={createOpen}
        onOpenChange={setCreateOpen}
        onClosed={() => setCreateShown(false)}
        search={listSearch}
      />}
    </>
  );
}

/// Run `task` over `items` with at most `width` in flight; every outcome is
/// kept so the caller can count failures without aborting the rest.
export async function runBatched<T, R>(
  items: T[],
  width: number,
  task: (item: T) => Promise<R>,
): Promise<PromiseSettledResult<R>[]> {
  const results: PromiseSettledResult<R>[] = [];
  for (let at = 0; at < items.length; at += width) {
    results.push(...(await Promise.allSettled(items.slice(at, at + width).map(task))));
  }
  return results;
}

/// The confirmation of a bulk close or delete, opened by its selection
/// action (the selection bar's button or a selected row's menu).
function BulkActionDialog({
  action,
  count,
  open,
  onOpenChange,
  onConfirm,
}: {
  action: "close" | "delete";
  count: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  const verb = action === "close" ? "Close" : "Delete";
  const noun = count === 1 ? "session" : "sessions";
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {verb} {count} {noun}?
          </AlertDialogTitle>
          <AlertDialogDescription>
            {action === "close"
              ? "Each permitted open session is force-closed in turn: active and queued work is cancelled and the session cannot be reopened. Other selected sessions are left alone."
              : "Each permitted closed session disappears for all universe members. History is retained. Other selected sessions are left alone."}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive text-white hover:bg-destructive/90"
            onClick={() => {
              onOpenChange(false);
              onConfirm();
            }}
          >
            {verb} {count} {noun}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

type SessionNode = { session: SessionSummary; children: SessionNode[] };

/// Group sub-agent sessions under their parent when the parent is in the
/// loaded page set; the parent row counts them. Children whose parent is not
/// loaded stay at the top level, their lineage in the title's tooltip; list
/// order (most recently updated first) is preserved at every level.
function buildSessionTree(sessions: SessionSummary[]): SessionNode[] {
  const byId = new Map(sessions.map((session) => [session.id, { session, children: [] as SessionNode[] }]));
  const roots: SessionNode[] = [];
  for (const node of byId.values()) {
    const parentId = node.session.origin?.parentSessionId;
    const parent = parentId ? byId.get(parentId) : undefined;
    if (parent && parent !== node) parent.children.push(node);
    else roots.push(node);
  }
  return roots;
}

interface SessionRowControls {
  selecting: boolean;
  selected: Set<string>;
  onToggle: (id: string) => void;
  /// What can be done with the selection: a selected row's right-click menu.
  selectionActions: readonly ListSelectionAction[];
  showSessionIds: boolean;
  metadataKeys: string[];
  search: string;
  /// The universe's open environments, naming and coloring each row's.
  environments: readonly Environment[] | undefined;
}

function SessionTreeItem({
  node,
  slug,
  activeId,
  depth,
  last = true,
  lines = [],
  ...controls
}: {
  node: SessionNode;
  slug: string;
  activeId: string | undefined;
  depth: number;
  /// The last of its siblings.
  last?: boolean;
  /// Per ancestor level below the root: whether that ancestor has a later sibling.
  lines?: boolean[];
} & SessionRowControls) {
  // What its children pass on: below the root, whether this row's own line runs on.
  const childLines = depth > 0 ? [...lines, !last] : [];
  return (
    <>
      <SessionListItem
        session={node.session}
        slug={slug}
        active={node.session.id === activeId}
        tree={{ depth, last, lines, children: node.children.length > 0 }}
        subAgentCount={node.children.length}
        {...controls}
      />
      {node.children.map((child, index) => (
        <SessionTreeItem
          key={child.session.id}
          node={child}
          slug={slug}
          activeId={activeId}
          depth={depth + 1}
          last={index === node.children.length - 1}
          lines={childLines}
          {...controls}
        />
      ))}
    </>
  );
}

function SessionListItem({
  session,
  slug,
  active,
  tree,
  subAgentCount = 0,
  selecting,
  selected,
  onToggle,
  selectionActions,
  showSessionIds,
  metadataKeys,
  search,
  environments,
}: {
  session: SessionSummary;
  slug: string;
  active: boolean;
  /// Its place in the tree of sessions and their sub-agents.
  tree: ListRowTree;
  /// Sub-agent sessions listed directly under this row.
  subAgentCount?: number;
} & SessionRowControls) {
  const origin = session.origin ?? null;
  const isSelected = selected.has(session.id);
  const displayName = session.displayName?.trim();
  const showSecondaryId = showSessionIds && Boolean(displayName);
  const visibleMetadata = metadataKeys.flatMap((key) => {
    const value = session.metadata?.[key];
    return value === undefined ? [] : [{ key, value }];
  });
  const navigate = useNavigate();
  const to = `/u/${slug}/sessions/${session.id}${search ? `?${search}` : ""}`;
  return (
    <ListRow
      to={to}
      // Right-click: the session (opened if it is not) and its page's ⋯
      // menu, at the pointer; while selecting, on a selected row, what can
      // be done with the selection instead (ListRow decides).
      entryProps={{
        onContextMenu: (event) => {
          event.preventDefault();
          if (!active) navigate(to);
          requestRowMenu("session", session.id, event);
        },
      }}
      active={active}
      selection={selecting
        ? { checked: isSelected, onToggle: () => onToggle(session.id), label: `Select ${session.displayName ?? session.id}`, actions: selectionActions }
        : undefined}
      tree={tree}
      indicator={<ActivityDot activity={session.activity} closed={session.lifecycleStatus === "closed"} />}
      title={displayName || session.id}
      // A sub-agent row is marked by its indent and ↳; its lineage is the
      // title's tooltip.
      titleHint={origin
        ? `Sub-agent of ${origin.parentSessionId} (depth ${origin.depth}, profile ${origin.agent.profileId} rev ${origin.agent.revision})`
        : displayName ? undefined : session.id}
      marks={sessionMetaItems({
        session,
        subAgents: { count: subAgentCount },
        resources: activeEnvironmentItems(session.activeEnvironmentId, environments),
      })}
      meta={relativeTime(session.updatedAtMs)}
    >
      {session.lastMessagePreview && (
        <ListRowLine title={session.lastMessagePreview}>{session.lastMessagePreview}</ListRowLine>
      )}
      {showSecondaryId && <ListRowLine mono title={session.id}>{session.id}</ListRowLine>}
      {visibleMetadata.map(({ key, value }) => (
        <ListRowLine key={key} mono className="text-[11px]" title={`${key}=${value}`}>
          {key}={value}
        </ListRowLine>
      ))}
    </ListRow>
  );
}

function NewSessionDialog({
  universeId,
  slug,
  open,
  onOpenChange,
  onClosed,
  search,
}: {
  universeId: string;
  slug: string;
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
      <ProviderReadinessBanner universeId={universeId} slug={slug} model={effectiveModel.model ?? (defaults.data ? null : undefined)} enabled={open && !modelPending} className="rounded-lg border" />
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
      api<SessionView>("POST", `/api/v1/universes/${universeId}/sessions`, {
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        profile: creationProfile,
      }),
    onSuccess: async (session) => {
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
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
      const path = `/u/${slug}/sessions/${target}`;
      const nextSearch = new URLSearchParams(search);
      navigate(`${path}?${nextSearch}`);
      nextSearch.delete("new");
      const remaining = Math.max(0, DIALOG_WARP_MS - (performance.now() - warpStarted.current));
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
                A new session is private: you and the universe admins see it until you share it.
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

export function SessionDetail({
  universeId,
  slug,
  sessionId,
  backTo = `/u/${slug}/sessions`,
  embedded = false,
  sessionHref,
}: {
  universeId: string;
  slug: string;
  sessionId: string;
  backTo?: string;
  /**
   * Rendered inside another surface (a bot's Chat tab): no back link, and a
   * controlled session takes plain input — the bot page is where its operator
   * talks to it, so there is nothing to override.
   */
  embedded?: boolean;
  /** Where lineage links go; defaults to the Sessions page. */
  sessionHref?: (sessionId: string) => string;
}) {
  const dictation = useDictationAvailability(universeId);
  const modelDiscovery = useModelDiscovery(universeId);
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
    refetchInterval: (query) => sessionMayBeRunning(query.state.data) ? SESSION_LIVENESS_REFRESH_MS : false,
  });
  const title = session.data?.displayName ?? sessionId.slice(0, 24);
  // Embedded, the surrounding page names what is open.
  useDocumentTitle(embedded ? [] : [title]);
  const compaction = useSessionCompaction(universeId, sessionId, session.data);
  const owner = useSessionOwner(universeId, session.data?.access);
  const canShare = useCanShareSession(universeId, session.data?.access, Boolean(session.data?.origin));
  const [pending, setPending] = useState<PendingMessage[]>([]);
  // Retain only this view's local submission identities after pending cleanup.
  // Historical acknowledgements must never rekey a backend-loaded run. Scope
  // these records because run ids repeat across sessions.
  const [localSubmissions, setLocalSubmissions] = useState<Map<string, Map<string, string | null>>>(() => new Map());
  const submissionScope = JSON.stringify([universeId, sessionId]);
  const currentScope = useRef(submissionScope);
  currentScope.current = submissionScope;
  const [pendingSteers, setPendingSteers] = useState<PendingSteer[]>([]);
  const [notices, setNotices] = useState<{ id: string; text: string }[]>([]);
  const [stoppingRunId, setStoppingRunId] = useState<string | null>(null);
  const [cancellingQueued, setCancellingQueued] = useState<Set<string>>(() => new Set());
  const [steeringQueued, setSteeringQueued] = useState<string | null>(null);
  const queueActions = useRef(new Set<string>());
  const [recoveredMessages, setRecoveredMessages] = useState<QueuedMessageRecovery[]>([]);
  useEffect(() => {
    setRecoveredMessages((prev) => [
      ...prev.filter((item) => item.scope !== submissionScope), ...readQueuedRecovery(submissionScope),
    ]);
  }, [submissionScope]);
  const [followRequest, setFollowRequest] = useState(0);
  const [sendError, setSendError] = useState<string | null>(null);
  const [closeError, setCloseError] = useState<string | null>(null);
  const [closeOpen, setCloseOpen] = useState(false);
  const [shareOpen, setShareOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const permissions = useActionPermissions(universeId);
  const mayClose = useSessionClosePermission(universeId);
  const mayDelete = useSessionDeletePermission(universeId);
  const canControl = permissions.can("controlSession");
  const canConfigure = permissions.can("configureSession");
  const canStop = permissions.can("stopSession");
  const canClose = mayClose(session.data?.access);
  const canDelete = mayDelete(session.data?.access);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [decidingApproval, setDecidingApproval] = useState<{
    approvalId: string;
    decision: "approve" | "reject";
  } | null>(null);
  const [approvalError, setApprovalError] = useState<{
    approvalId: string;
    message: string;
  } | null>(null);

  const { showRunStatistics, collapseCompletedRuns, showSessionResources } = useUserPreferences();
  const resources = useSessionResources(
    universeId, session.data?.config, session.data?.activeEnvironmentId, showSessionResources && Boolean(session.data),
  );
  const entries = tail.transcript.entries;
  const loadFullText = useCallback(
    async (blobRef: string) => {
      const result = await api<{ bytesBase64: string }>(
        "GET", `/api/v1/universes/${universeId}/blobs/${encodeURIComponent(blobRef)}?resourceKind=session&resourceId=${encodeURIComponent(sessionId)}`,
      );
      return new TextDecoder().decode(Uint8Array.from(atob(result.bytesBase64), (char) => char.charCodeAt(0)));
    },
    [universeId, sessionId],
  );
  const activeRun = tail.transcript.activeRun;
  const queuedRuns = tail.transcript.queuedRuns;
  const runRevision = tail.transcript.runRevision;
  const sections = useMemo(() => sectionsByRun(entries, activeRun), [entries, activeRun]);

  // The session view is authoritative for run state; fold it into the tail
  // whenever it arrives (forward moves only), and refresh it on every run
  // lifecycle change the tail reports so queued-run text and terminal
  // statuses stay current.
  const reconcileRuns = tail.reconcileRuns;
  const sessionRuns = session.data?.runs;
  const sessionActiveRun = session.data?.activeRun;
  const approvalRun = sessionActiveRun?.pendingApprovals?.length
    ? sessionActiveRun : sessionRuns?.find((run) => (run.pendingApprovals?.length ?? 0) > 0);
  useEffect(() => {
    if (tail.phase === "live") {
      reconcileRuns([...(sessionRuns ?? []), ...(sessionActiveRun ? [sessionActiveRun] : [])]);
    }
  }, [sessionRuns, sessionActiveRun, tail.phase, reconcileRuns]);
  const refetchSession = session.refetch;
  useEffect(() => {
    if (runRevision > 0) {
      void refetchSession();
    }
  }, [runRevision, refetchSession]);

  // An optimistic send learns its run id from whichever arrives first: the
  // POST response, or the tail's `runAccepted` carrying our submission id.
  const runBySubmission = tail.transcript.runBySubmission;
  useEffect(() => {
    setPending((prev) => {
      let changed = false;
      const next = prev.map((message) => {
        if (message.runId) {
          return message;
        }
        const runId = runBySubmission.get(message.id);
        if (!runId) {
          return message;
        }
        changed = true;
        return { ...message, runId };
      });
      return changed ? next : prev;
    });
  }, [runRevision, runBySubmission]);

  // Reconcile optimistic echoes against the engine's own entries. A sent
  // message is confirmed by the run input entry carrying its run id (the
  // id can arrive after the entry, so this re-runs when pending changes
  // too); a steer by a steering entry with the same text on its run.
  const pendingRunIds = pending.map((message) => message.runId ?? "").join("|");
  useEffect(() => {
    setPending((prev) => {
      if (prev.length === 0) {
        return prev;
      }
      const confirmedRuns = new Set(
        entries
          .filter((entry) => entry.kind === "message" && entry.role === "user" && !entry.steering)
          .map((entry) => (entry as { runId?: string }).runId)
          .filter((runId): runId is string => Boolean(runId)),
      );
      const next = prev.filter((message) => !(message.runId && confirmedRuns.has(message.runId)));
      return next.length === prev.length ? prev : next;
    });
    setPendingSteers((prev) => {
      if (prev.length === 0) {
        return prev;
      }
      const confirmed = new Set(
        entries
          .filter((entry) => entry.kind === "message" && entry.role === "user" && entry.steering)
          .map((entry) => `${(entry as { runId?: string }).runId ?? ""}\u0000${(entry as { text: string }).text.trim()}`),
      );
      const next = prev.filter((steer) => !confirmed.has(`${steer.runId}\u0000${steer.text.trim()}`));
      return next.length === prev.length ? prev : next;
    });
  }, [entries, pendingRunIds]);

  // A pending message whose run the tail now knows is no longer optimistic
  // for status purposes; drop the ones whose run ended without ever
  // materializing input (cancelled while queued).
  useEffect(() => {
    setPending((prev) => {
      const next = prev.filter(
        (message) => !(message.runId && tail.transcript.runPhases.get(message.runId) === "terminal"),
      );
      return next.length === prev.length ? prev : next;
    });
    setPendingSteers((prev) => {
      const dropped = prev.filter(
        (steer) => tail.transcript.runPhases.get(steer.runId) === "terminal",
      );
      if (dropped.length === 0) {
        return prev;
      }
      // A steer that never materialized before its run ended was not
      // seen by the model; say so instead of letting it vanish.
      setNotices((current) => [
        ...current,
        ...dropped.map((steer) => ({
          id: steer.id,
          text: `steering not delivered — the run ended before its next turn: “${steer.text}”`,
        })),
      ]);
      return prev.filter((steer) => !dropped.includes(steer));
    });
    if (stoppingRunId && tail.transcript.runPhases.get(stoppingRunId) === "terminal") {
      setStoppingRunId(null);
    }
    setCancellingQueued((prev) => {
      if (prev.size === 0) {
        return prev;
      }
      const next = new Set(
        [...prev].filter((runId) => tail.transcript.runPhases.get(runId) !== "terminal"),
      );
      return next.size === prev.size ? prev : next;
    });
  }, [runRevision, tail.transcript.runPhases, stoppingRunId]);

  // Resolve run ids synchronously for rendering (the effect above persists
  // them a render later); otherwise the optimistic row and the tail's row
  // coexist for one frame under different keys and the list flickers.
  const resolvedPending: PendingMessage[] = pending.map((message) =>
    message.runId ? message : { ...message, runId: runBySubmission.get(message.id) ?? null },
  );
  const queuedIds = new Set(queuedRuns.map((run) => run.runId));
  // A message sent while a run was already live will be queued by the
  // engine; show it in the queue from the start rather than as a
  // transcript echo that jumps into the queue a moment later.
  const isQueuedPending = (message: PendingMessage) =>
    message.status === "queued" ||
    (message.status === "sending" && message.expectQueued) ||
    (message.runId !== null && queuedIds.has(message.runId));
  // Hide an echo the same frame its engine entry shows (the effect above
  // removes it from state a render later).
  const confirmedInputRuns = new Set(
    entries
      .filter((entry) => entry.kind === "message" && entry.role === "user" && !entry.steering)
      .map((entry) => (entry as { runId?: string }).runId)
      .filter((runId): runId is string => Boolean(runId)),
  );
  const pendingInTranscript = resolvedPending.filter(
    (message) =>
      !isQueuedPending(message) && !(message.runId && confirmedInputRuns.has(message.runId)),
  );
  const submissionKeys = new Map<string, string>();
  for (const [submissionId, acknowledgedRunId] of localSubmissions.get(submissionScope) ?? []) {
    const runId = acknowledgedRunId ?? runBySubmission.get(submissionId);
    if (runId) submissionKeys.set(runId, submissionId);
  }
  const displaySections = withPendingRunInputs(sections, pendingInTranscript, submissionKeys);
  const confirmedSteers = new Set(
    entries
      .filter((entry) => entry.kind === "message" && entry.role === "user" && entry.steering)
      .map((entry) => `${(entry as { runId?: string }).runId ?? ""}\u0000${(entry as { text: string }).text.trim()}`),
  );
  const visiblePendingSteers = pendingSteers.filter(
    (steer) => !confirmedSteers.has(`${steer.runId}\u0000${steer.text.trim()}`),
  );
  // The run to steer or stop: the tail's active run, or — before the tail
  // has reported it — the run the engine just accepted as running.
  const steerTargetRunId =
    activeRun?.runId ??
    resolvedPending.find(
      (message) =>
        message.runId &&
        (message.status === "running" ||
          tail.transcript.runPhases.get(message.runId) === "running"),
    )?.runId ??
    null;
  const runActive = runInProgress(tail.transcript) || pending.length > 0;
  const stopping = stoppingRunId !== null && steerTargetRunId === stoppingRunId;
  const canSteer = canControl && steerTargetRunId !== null && !(activeRun?.cancelling ?? false) && !stopping;
  const queuedItems: QueuedRunItem[] = [
    ...queuedRuns.map((run) => {
      const sent = resolvedPending.find((message) => message.runId === run.runId);
      return {
        key: sent?.id ?? run.runId,
        runId: run.runId,
        text: queuedRunText(run.runId, sessionRuns, resolvedPending),
        cancelling: cancellingQueued.has(run.runId),
        steering: steeringQueued === run.runId,
      };
    }),
    ...resolvedPending
      .filter(
        (message) =>
          isQueuedPending(message) &&
          !(message.runId && tail.transcript.runPhases.has(message.runId)),
      )
      .map((message) => ({
        key: message.id,
        runId: message.runId ?? null,
        text: message.text || attachmentSummary(message.media),
        cancelling: message.runId !== null && cancellingQueued.has(message.runId),
        steering: message.runId !== null && steeringQueued === message.runId,
      })),
  ];
  const closed = session.data?.lifecycleStatus === "closed";
  const integration = session.data?.integration;
  const controlled = session.data?.lifecycleControlled === true;
  const managerLabel = controlledSessionOwnerLabel(integration);
  const owningBotId = controlledSessionBotId(integration, session.data?.metadata);
  // With bots switched off, a bot's session reads as controlled, with no way
  // into the hidden bot page.
  const botsOn = useFeature("bots");
  const owningBotHref = owningBotId && botsOn
    ? `/u/${slug}/bots/${encodeURIComponent(owningBotId)}/chat/${encodeURIComponent(sessionId)}`
    : null;
  // Emit rows and event bands name peer bots; only a bot's own sessions
  // carry that traffic, and only bot managers may list the roster.
  const botDirectory = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/v1/universes/${universeId}/bots`),
    enabled: owningBotId !== null,
    staleTime: 60_000,
    retry: false,
  });
  const botRoster = botDirectory.data?.bots;
  const owningBot = owningBotId ? botRoster?.find((bot) => bot.botId === owningBotId) : undefined;
  // The session's meta items, drawn compact beside the title and full in
  // the strip under the header. The strip shows while there are resources
  // (when wanted) or a lineage to show.
  const lineage = useSessionLineage({ universeId, sessionId, origin: session.data?.origin, runRevision });
  const resourcesShown = showSessionResources && Boolean(session.data) && hasResources(resources);
  const meta = session.data ? sessionMetaItems({
    session: session.data,
    controller: owningBotHref
      ? { bot: true, botId: owningBotId ?? undefined, label: owningBot ? botLabel(owningBot) : owningBotId ?? "a bot", href: owningBotHref }
      : { bot: false, label: managerLabel },
    lineage,
    lineageHref: sessionHref ?? ((target: string) => `/u/${slug}/sessions/${target}`),
    resources: resourcesShown
      ? sessionResourceItems({ resources, slug, onConfigure: !embedded && canConfigure ? () => setSettingsOpen(true) : undefined })
      : [],
  }) : [];
  const showStrip = SHOW_META_STRIP && meta.length > 0 && (resourcesShown || hasLineage(lineage));
  const loadMedia = useCallback(
    async (blobRef: string, mime: string) => {
      const result = await api<{ bytesBase64: string }>(
        "GET", `/api/v1/universes/${universeId}/blobs/${encodeURIComponent(blobRef)}?resourceKind=session&resourceId=${encodeURIComponent(sessionId)}`,
      );
      const bytes = Uint8Array.from(atob(result.bytesBase64), (char) => char.charCodeAt(0));
      return new Blob([bytes], { type: mime });
    },
    [universeId, sessionId],
  );
  const transcriptMedia = useMemo(() => mediaByHandle(entries), [entries]);
  const transcriptFiles = useMemo(() => filesByHandle(entries), [entries]);
  const transcriptLinks = useMemo<TranscriptLinks>(() => {
    const names = new Map((botRoster ?? []).map((bot) => [bot.botId, botLabel(bot)]));
    const href = sessionHref ?? ((target: string) => `/u/${slug}/sessions/${target}`);
    return {
      botName: (botId) => names.get(botId),
      sessionHref: href,
      // A browser address: these links open in a new tab, outside the router.
      blobHref: (blobRef, hints) => {
        const path = blobHref(slug, blobRef, { ...hints, session: sessionId });
        return path && appHref(path);
      },
      navigate: (target) => void navigate(target),
      loadMedia,
      mediaByHandle: transcriptMedia,
      filesByHandle: transcriptFiles,
      fileReferenceSource: {
        universeId, sessionId,
        load: (handle, signal) => findFileReference(handle, (before) => fetchHistory(universeId, sessionId, before, signal)),
      },
    };
  }, [botRoster, sessionHref, slug, sessionId, navigate, loadMedia, transcriptMedia, transcriptFiles, universeId]);
  // Operator override: the engine happily admits direct runs on a controlled
  // session (they queue like any client run), so the gate here is policy,
  // not capability. Off by default because direct input bypasses the
  // manager's ingress; resets when the operator navigates away.
  const controlledGate = controlled && !embedded;
  const [directInput, setDirectInput] = useState(false);
  const queueSteerTarget = useRef<string | null>(null);
  queueSteerTarget.current = canSteer && canStop && !closed && (!controlledGate || directInput) ? steerTargetRunId : null;
  useEffect(() => {
    setDirectInput(false);
  }, [sessionId]);

  useEffect(() => {
    if (settingsOpen && !runActive) {
      void session.refetch();
    }
  }, [settingsOpen, runActive]);

  const send = async (message: ComposerMessage, mode: ComposerMode | null, submissionId: string = crypto.randomUUID()) => {
    if (!canControl) return false;
    if (currentScope.current === submissionScope) setSendError(null);
    if (mode === "steer") {
      const result = await steer(message);
      if (!result.ok && currentScope.current === submissionScope) {
        setSendError(result.error instanceof Error ? result.error.message : String(result.error));
      }
      return result.ok;
    }
    const { text } = message;
    const media = echoMedia(message.attachments);
    // The submission id doubles as the engine idempotency key: a retried
    // POST returns the original run instead of starting a second one.
    const expectQueued = runActive;
    setLocalSubmissions((previous) => new Map(previous).set(
      submissionScope, new Map(previous.get(submissionScope)).set(submissionId, null),
    ));
    if (currentScope.current === submissionScope) setPending((prev) => [
      ...prev,
      { id: submissionId, text, media, runId: null, status: "sending", expectQueued },
    ]);
    try {
      const accepted = await postUntilAccepted(
        () => api<SessionRunAccepted>(
          "POST",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/messages`,
          {
            text,
            submissionId,
            ...(message.attachments.length ? { attachments: message.attachments.map(wireAttachment) } : {}),
            ...(canConfigure && message.options ? { options: message.options } : {}),
          },
        ),
        () => currentScope.current === submissionScope,
      );
      if (!accepted) return true;
      if (currentScope.current === submissionScope) setFollowRequest((request) => request + 1);
      setLocalSubmissions((previous) => new Map(previous).set(
        submissionScope, new Map(previous.get(submissionScope)).set(submissionId, accepted.run.id),
      ));
      if (currentScope.current === submissionScope) {
        // An idempotent retry can return a run that already finished.
        const terminal = ["completed", "cancelled", "failed"].includes(accepted.run.status);
        setPending((prev) => terminal
          ? prev.filter((message) => message.id !== submissionId)
          : prev.map((message) => message.id === submissionId ? {
            ...message,
            runId: message.runId ?? accepted.run.id,
            status: accepted.run.status === "queued" ? "queued" : "running",
          } : message));
        if (terminal) void session.refetch();
      }
      return true;
    } catch (error) {
      if (currentScope.current === submissionScope) {
        setPending((prev) => prev.filter((message) => message.id !== submissionId));
        setSendError(error instanceof Error ? error.message : String(error));
      }
      return false;
    }
  };

  const steer = async (
    { text, attachments }: ComposerMessage, runId = steerTargetRunId,
  ): Promise<{ ok: true } | { ok: false; error: unknown }> => {
    if (!canControl) return { ok: false, error: new Error("You have read-only access to this session.") };
    if (!runId) {
      return { ok: false, error: new Error(
        "There is no run to steer yet — wait a moment and try again, or press Enter to queue it.",
      ) };
    }
    const id = crypto.randomUUID();
    if (currentScope.current === submissionScope) {
      setPendingSteers((prev) => [...prev, { id, runId, text, media: echoMedia(attachments) }]);
    }
    try {
      await postUntilAccepted(
        () => api<SessionRunSteered>(
          "POST",
          `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${runId}/steer`,
          {
            text,
            submissionId: id,
            ...(attachments.length ? { attachments: attachments.map(wireAttachment) } : {}),
          },
        ),
        () => currentScope.current === submissionScope,
      );
      if (currentScope.current === submissionScope) setFollowRequest((request) => request + 1);
      return { ok: true };
    } catch (error) {
      setPendingSteers((prev) => prev.filter((steer) => steer.id !== id));
      return { ok: false, error };
    }
  };

  /// The composer's model choice becomes the session default. Config
  /// replacement needs an idle session and the revision last read.
  const saveModelDefault = async (config: Record<string, unknown>) => {
    if (!canConfigure) return;
    const current = session.data;
    if (!current) return;
    const updated = await api<SessionView>(
      "PUT",
      `/api/v1/universes/${universeId}/sessions/${sessionId}/config`,
      { config, expectedConfigRevision: current.configRevision },
    );
    queryClient.setQueryData(["session", universeId, sessionId], updated);
  };

  const cancelRun = async (runId: string) => {
    if (!canStop) return null;
    setSendError(null);
    try {
      return await api<SessionRunCancelled>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${runId}/cancel`,
        {},
      );
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
      return null;
    }
  };

  const stop = async () => {
    // Stop the run the engine is executing; if only queued runs exist
    // (the active one just ended), stop the next one instead so the queue
    // does not start behind the reader's back.
    const target =
      steerTargetRunId ?? queuedRuns[0]?.runId ?? resolvedPending.find((m) => m.runId)?.runId;
    if (!target) {
      return;
    }
    if (queueActions.current.has(target)) return;
    if (steerTargetRunId === target) {
      setStoppingRunId(target);
    } else {
      setCancellingQueued((prev) => new Set(prev).add(target));
    }
    const response = await cancelRun(target);
    if (!response) {
      setStoppingRunId((current) => (current === target ? null : current));
      setCancellingQueued((prev) => {
        const next = new Set(prev);
        next.delete(target);
        return next;
      });
      return;
    }
    if (response.run.status === "cancelled") {
      setStoppingRunId((current) => (current === target ? null : current));
    }
    void session.refetch();
  };

  const cancelQueued = async (runId: string) => {
    if (queueActions.current.has(runId)) return;
    queueActions.current.add(runId);
    setCancellingQueued((prev) => new Set(prev).add(runId));
    const response = await cancelRun(runId);
    queueActions.current.delete(runId);
    if (!response) {
      setCancellingQueued((prev) => {
        const next = new Set(prev);
        next.delete(runId);
        return next;
      });
      return;
    }
    void session.refetch();
  };

  const steerQueued = async (runId: string) => {
    const target = steerTargetRunId;
    if (!canSteer || !canStop || closed || (controlledGate && !directInput)
      || !target || target === runId || queueActions.current.size > 0) return;
    queueActions.current.add(runId);
    setSteeringQueued(runId);
    setSendError(null);
    let message: ComposerMessage | undefined;
    let cancellationAttempted = false;
    try {
      const run = await api<RunView>("GET", `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${runId}`);
      if (run.status !== "queued") throw new Error("This message is no longer queued.");
      message = await queuedMessage(run, loadFullText);
      if (currentScope.current !== submissionScope) return;
      if (queueSteerTarget.current !== target) {
        throw new Error("The active run is no longer available for steering. The message is still queued.");
      }
      storeQueuedRecovery(submissionScope, runId, {
        id: runId, scope: submissionScope, message,
        error: "Conversion was interrupted. Check the queue and conversation before sending this saved message again.",
      });
      cancellationAttempted = true;
      const cancelled = await api<SessionRunCancelled>(
        "POST", `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${runId}/cancel`, {},
      );
      if (cancelled.run.status !== "cancelled" || cancelled.run.startedAtMs != null) {
        throw new Error("The queued message started before it could be converted. Steering was not sent.");
      }
      if (currentScope.current === submissionScope) {
        setPending((prev) => prev.filter((pending) => (pending.runId ?? runBySubmission.get(pending.id)) !== runId));
        reconcileRuns([{ id: runId, status: "cancelled", acceptedAtMs: 0, source: { type: "input" } }]);
      }
      // Keep the original target even if the tail reports a different active run.
      const result = await steer(message, target);
      if (result.ok) {
        dismissRecoveredMessage(submissionScope, runId);
      } else if (steeringWasNotSubmitted(result.error)) {
        const recovery = {
          id: runId, scope: submissionScope, message,
          error: "Steering was not sent. The follow-up could not be confirmed. Use Queue again to retry.",
        };
        storeQueuedRecovery(submissionScope, runId, recovery);
        if (await send(message, "queue", queuedRecoverySubmissionId(runId))) {
          dismissRecoveredMessage(submissionScope, runId);
          if (currentScope.current === submissionScope) setNotices((prev) => [...prev, {
            id: queuedRecoverySubmissionId(runId),
            text: "Steering was unavailable. Sent your message as a follow-up.",
          }]);
        } else {
          recoverQueuedMessage(recovery);
        }
      } else {
        if (currentScope.current === submissionScope) {
          setSendError(result.error instanceof Error ? result.error.message : String(result.error));
        }
        recoverQueuedMessage({
          id: runId, scope: submissionScope, message,
          error: "Removed from the queue, but steering could not be confirmed. Check the conversation before sending again.",
        });
      }
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      if (currentScope.current === submissionScope) setSendError(detail);
      if (message && cancellationAttempted) {
        recoverQueuedMessage({
          id: runId, scope: submissionScope, message,
          error: `${detail} Steering was not sent. Check the queue and conversation before sending again.`,
        });
      }
    } finally {
      queueActions.current.delete(runId);
      setSteeringQueued((current) => current === runId ? null : current);
      void session.refetch();
    }
  };

  const recoverQueuedMessage = (recovery: QueuedMessageRecovery) => {
    storeQueuedRecovery(recovery.scope, recovery.id, recovery);
    setRecoveredMessages((prev) => [
      ...prev.filter((item) => item.scope !== recovery.scope || item.id !== recovery.id), recovery,
    ]);
  };

  const dismissRecoveredMessage = (scope: string, id: string) => {
    storeQueuedRecovery(scope, id);
    setRecoveredMessages((prev) => prev.filter((item) => item.scope !== scope || item.id !== id));
  };

  const requeueRecovered = async (id: string, message: ComposerMessage) => {
    if (queueActions.current.has(id)) return;
    queueActions.current.add(id);
    setRecoveredMessages((prev) => prev.map((item) => item.id === id ? { ...item, sending: true } : item));
    const sent = await send(message, "queue", queuedRecoverySubmissionId(id));
    if (sent) dismissRecoveredMessage(submissionScope, id);
    else setRecoveredMessages((prev) => prev.map((item) => item.id === id ? { ...item, sending: false } : item));
    queueActions.current.delete(id);
  };

  const decideApproval = async (
    approvalId: string,
    decision: "approve" | "reject",
  ) => {
    if (!canControl || !approvalRun) return;
    setApprovalError(null);
    setDecidingApproval({ approvalId, decision });
    try {
      const response = await api<SessionRunApprovalsDecided>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${approvalRun.id}/approvals`,
        { decisions: [{ approvalId, decision }] },
      );
      const result = response.results[0];
      if (!result || result.status === "failed") {
        throw new Error(result?.failure?.message ?? "The approval decision was not accepted");
      }
      if (result.status === "pending") {
        setApprovalError({ approvalId, message: "The session is busy, so this decision has not applied yet. It can still apply." });
      }
      queryClient.setQueryData<SessionView>(
        ["session", universeId, sessionId],
        (current) => current
          ? {
              ...current,
              runs: current.runs?.map((run) =>
                run.id === response.run.id ? response.run : run,
              ),
            }
          : current,
      );
      await session.refetch();
    } catch (error) {
      setApprovalError({
        approvalId,
        message: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setDecidingApproval(null);
    }
  };

  const closeSession = useMutation({
    meta: { blocking: true },
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

  const restartSession = useMutation({
    meta: { blocking: true },
    mutationFn: () =>
      api<SessionView>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/restart`,
        {},
      ),
    onSuccess: async (restarted) => {
      queryClient.setQueryData(["session", universeId, sessionId], restarted);
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
    },
  });

  const onSessionDeleted = async () => {
    setDeleteOpen(false);
    queryClient.setQueriesData<InfiniteData<SessionListPage>>(
      { queryKey: ["sessions", universeId] },
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
    navigate(backTo);
    await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
  };

  return (
    <>
      <ProviderReadinessBanner universeId={universeId} slug={slug} model={modelFromConfig(session.data?.config)} enabled={Boolean(session.data)} />
      {session.data?.executionFailure && (
        <div role="alert" className="flex items-center gap-3 border-b bg-destructive/10 px-4 py-2 text-sm text-destructive">
          <span className="min-w-0 flex-1">
            {EXECUTION_FAILURE_TEXT[session.data.executionFailure]} Its history is intact, but nothing is running and new messages will fail.
            {restartSession.error && ` Restart failed: ${restartSession.error.message}`}
          </span>
          {canConfigure && (
            <Button
              size="sm"
              variant="outline"
              className="shrink-0"
              disabled={restartSession.isPending}
              onClick={() => restartSession.mutate()}
            >
              {restartSession.isPending ? "Restarting…" : "Restart"}
            </Button>
          )}
        </div>
      )}
      {!embedded && (
        <>
        {/* Left is state: what it is, who sees it, who drives it, what it is
            doing. Right is action, all of it behind ⋯. */}
        <DetailHeader
          back={{ to: backTo, label: "Back to sessions" }}
          title={title}
          tags={meta.length > 0 && <MetaToolbar items={meta} place="header" full label="Session details" className="grow" />}
          actions={[{
            kind: "custom",
            key: "session",
            node: <SessionActionsMenu
              variant="more"
              sessionId={sessionId}
              metadata={session.data?.metadata}
              open={owningBotHref ? { label: "Open in bot", href: owningBotHref, icon: <BotIcon /> } : undefined}
              onSettings={permissions.can("read") ? () => setSettingsOpen(true) : undefined}
              onCompact={canControl && session.data && !closed ? compaction.compact : undefined}
              compactionLabel={compaction.label}
              onShare={canShare ? () => setShareOpen(true) : undefined}
              lifecycle={((!controlled || permissions.role === "admin") && canClose && !closed) || (canDelete && closed) ? (
                closed ? (
                  <DropdownMenuItem
                    variant="destructive"
                    onClick={() => setDeleteOpen(true)}
                  >
                    <DeleteIcon /> Delete session…
                  </DropdownMenuItem>
                ) : (
                  <DropdownMenuItem
                    variant="destructive"
                    onClick={() => {
                      setCloseError(null);
                      setCloseOpen(true);
                    }}
                  >
                    <SessionCloseIcon /> {runActive ? "Force close session…" : "Close session…"}
                  </DropdownMenuItem>
                )
              ) : undefined}
            />,
          }]}
        />

        <ShareSessionDialog
          universeId={universeId}
          sessionId={sessionId}
          open={shareOpen && canShare}
          onOpenChange={setShareOpen}
        />

        <AlertDialog
          open={closeOpen && canClose}
          onOpenChange={(open) => {
            setCloseOpen(open);
            if (open) setCloseError(null);
          }}
        >
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

        {deleteOpen && <DeleteSessionDialog
          key={`${universeId}:${sessionId}`}
          universeId={universeId}
          sessionId={sessionId}
          onCancel={() => setDeleteOpen(false)}
          onDeleted={onSessionDeleted}
        />}
        </>
      )}
      {showStrip && (
        <div className="shrink-0 border-b bg-muted/30">
          <div className="mx-auto w-full max-w-5xl px-4 py-1.5 md:px-8">
            {/* Embedded in its bot's page, a session need not name the bot. */}
            <MetaToolbar
              items={embedded ? meta.filter((item) => item.key !== SESSION_META.controller) : meta}
              place="strip"
              label="Session details"
            />
          </div>
        </div>
      )}
      <TranscriptLinksContext.Provider value={transcriptLinks}>
      <MessageScrollerProvider key={`${universeId}/${sessionId}`} autoScroll defaultScrollPosition="end">
        <TranscriptMotionProvider sections={displaySections} pendingKeys={visiblePendingSteers.map((steer) => steer.id)} ready={tail.phase === "live"} historyRevision={tail.historyRevision}>
        <MessageScroller className="min-h-0 flex-1">
          <MessageScrollerViewport preserveScrollOnPrepend>
            <SessionHistoryLoader tail={tail} />
            <MessageScrollerContent className="mx-auto w-full max-w-5xl cursor-select gap-3 px-4 py-6 md:px-8">
              {tail.phase === "loading" && entries.length === 0 && !tail.error && (
                <LoadingNote />
              )}
              {tail.error && entries.length === 0 && (
                <ReadError error={tail.error} transient={tail.errorIsTransient} retrying loading graceMs={10_000} />
              )}
              {tail.phase === "live" &&
                entries.length === 0 &&
                pendingInTranscript.length === 0 && (
                  <SessionWelcome
                    state={closed ? "closed" : !canControl ? "readOnly" : controlledGate && !directInput ? "controlled" : "open"}
                    unshared={owner && session.data?.access.visibility === "restricted" && !session.data?.origin}
                  />
                )}
              {displaySections.map((section) => (
                <MessageScrollerItem key={section.key} messageId={section.key}>
                  {section.kind === "run" ? (
                    <RunSectionView
                      section={section}
                      activeRun={activeRun}
                      loadFullText={loadFullText}
                      showRunStatistics={showRunStatistics}
                      collapseCompletedRuns={collapseCompletedRuns}
                    />
                  ) : section.kind === "system" ? (
                    <SystemChips entries={section.entries} />
                  ) : (
                    <TranscriptEntryView entry={section.entry} loadFullText={loadFullText} />
                  )}
                </MessageScrollerItem>
              ))}
              {visiblePendingSteers.map((steer) => (
                <MessageScrollerItem key={steer.id} messageId={steer.id}>
                  <TranscriptEntrance motionKey={steer.id}><UserBand text={steer.text} media={steer.media} steering /></TranscriptEntrance>
                </MessageScrollerItem>
              ))}
              {notices.map((notice) => (
                <MessageScrollerItem key={notice.id} messageId={notice.id}>
                  <TranscriptEntryView
                    entry={{ kind: "marker", key: notice.id, text: notice.text, tone: "muted" }}
                  />
                </MessageScrollerItem>
              ))}
              {compaction.error && (
                <MessageScrollerItem messageId="compaction-request-error">
                  <TranscriptEntryView entry={{
                    kind: "marker", key: "compaction-request-error",
                    text: `Compaction failed: ${compaction.error}`, tone: "error",
                  }} />
                </MessageScrollerItem>
              )}
              {approvalRun && (
                <MessageScrollerItem messageId={`approvals-${approvalRun.id}`}>
                  <ApprovalCards
                    approvals={approvalRun.pendingApprovals ?? []}
                    deciding={decidingApproval}
                    error={approvalError}
                    onDecide={canControl ? (approvalId, decision) =>
                      void decideApproval(approvalId, decision) : undefined}
                  />
                </MessageScrollerItem>
              )}
              {tail.error && entries.length > 0 && (
                <ReadError error={tail.error} transient={tail.errorIsTransient} retrying graceMs={10_000} className="text-center text-xs" />
              )}
            </MessageScrollerContent>
          </MessageScrollerViewport>
          <MessageScrollerButton />
        </MessageScroller>
        <SessionScrollFollower
          followRequest={followRequest}
          ready={tail.phase === "live"}
          historyRevision={tail.historyRevision}
          entries={entries}
          pending={pendingInTranscript}
          activeRun={activeRun}
        />
        </TranscriptMotionProvider>
      </MessageScrollerProvider>
      </TranscriptLinksContext.Provider>
      {!closed && (
        <QueuedRunsBar
          items={queuedItems}
          onCancel={canStop ? (runId) => void cancelQueued(runId) : undefined}
          onSteer={canControl && canStop && (!controlledGate || directInput) ? (runId) => void steerQueued(runId) : undefined}
          canSteer={canSteer && steeringQueued === null}
        />
      )}
      {recoveredMessages.filter((item) => item.scope === submissionScope).map((item) => (
        <div key={item.id} className="shrink-0 border-t bg-muted/40 px-4 py-2" role="alert">
          <div className="mx-auto flex w-full max-w-5xl flex-col gap-2 text-sm md:px-4">
            <p>{item.error}</p>
            <details>
              <summary className="cursor-action text-xs">Saved message</summary>
              <p className="max-h-40 overflow-auto whitespace-pre-wrap">{item.message.text}</p>
              {item.message.attachments.map((attachment) => <p key={attachment.blobRef}>{attachment.name}</p>)}
            </details>
            <div className="flex gap-2">
              <Button size="xs" variant="outline"
                disabled={item.sending || !canControl || closed || (controlledGate && !directInput)}
                onClick={() => void requeueRecovered(item.id, item.message)}>Queue again</Button>
              <Button size="xs" variant="ghost" disabled={item.sending}
                onClick={() => dismissRecoveredMessage(item.scope, item.id)}>Dismiss</Button>
            </div>
          </div>
        </div>
      ))}
      <SessionComposer
        dictation={{ ...dictation, settingsHref: `/u/${slug}/models` }}
        key={sessionDraftKey(universeId, sessionId)}
        draftKey={sessionDraftKey(universeId, sessionId)}
        runActive={runActive}
        canSteer={canSteer}
        stopping={stopping}
        canStop={canStop && !closed}
        disabled={!canControl || closed || (controlledGate && !directInput)}
        disabledReason={!canControl
          ? "You have read-only access to this session."
          : controlledGate && !directInput
          ? `Managed by ${managerLabel} — flip Direct input to message this session anyway.`
          : undefined}
        banner={canControl && controlledGate && !closed ? (
          <div className="flex min-w-0 items-center gap-2 pb-2 text-xs">
            <Switch
              className="shrink-0"
              checked={directInput}
              onCheckedChange={setDirectInput}
              aria-label="Direct input"
            />
            <span className="shrink-0 font-medium">Direct input</span>
            <span
              className={`min-w-0 truncate ${directInput ? "text-foreground" : "text-muted-foreground"}`}
              title={directInput
                ? `Direct input bypasses ${managerLabel}'s ingress: messages are not tracked as events, skip its budget and delivery policies, and may interleave with its deliveries.`
                : `Managed by ${managerLabel} — flip Direct input to message this session anyway.`}
            >
              {directInput
                ? `Bypasses ${managerLabel}'s ingress: messages are not tracked as events, skip its budget and delivery policies, and may interleave with its deliveries.`
                : `Managed by ${managerLabel} — flip to message this session anyway.`}
            </span>
          </div>
        ) : undefined}
        attachments={{ universeId, apiKind: modelFromConfig(session.data?.config)?.apiKind }}
        model={canConfigure && session.data?.config ? {
          config: session.data.config,
          models: modelDiscovery.data?.models,
          canSaveDefault: canConfigure && !closed,
          onSaveDefault: saveModelDefault,
        } : undefined}
        error={sendError}
        onDismissError={() => setSendError(null)}
        onSend={(message, mode) => void send(message, mode)}
        onStop={() => void stop()}
      />
      {!embedded && permissions.can("read") && (
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
      )}
    </>
  );
}

interface PendingMessage {
  id: string;
  text: string;
  /// Attachments sent with the message, previewed from local files.
  media?: TranscriptMedia[];
  /// Engine run id once the POST returned; null while in flight.
  runId: string | null;
  status: "sending" | "running" | "queued";
  /// Sent while a run was already live, so the engine will queue it.
  expectQueued: boolean;
}

interface PendingSteer {
  id: string;
  runId: string;
  text: string;
  media?: TranscriptMedia[];
}

/// The attachment fields the message routes accept.
function wireAttachment({ blobRef, mime, kind, name }: SentAttachment) {
  return { blobRef, mime, kind, name };
}

/// Attachments as transcript media for the optimistic echo, previewed from
/// the picked files until the stored entries arrive.
function echoMedia(attachments: SentAttachment[]): TranscriptMedia[] | undefined {
  if (!attachments.length) return undefined;
  return attachments.map((attachment) => ({
    handle: mediaHandleFor(attachment.blobRef),
    blobRef: attachment.blobRef,
    mime: attachment.mime,
    kind: attachment.kind,
    name: attachment.name,
    ...(attachment.previewUrl ? { localUrl: attachment.previewUrl } : {}),
  }));
}

function attachmentSummary(media: TranscriptMedia[] | undefined): string {
  const count = media?.length ?? 0;
  if (count === 1) return media![0]!.name ?? "1 attachment";
  return count ? `${count} attachments` : "";
}

/// Text for a queued run: from the authoritative session view when it has
/// been refetched, else from the optimistic send that produced it.
function queuedRunText(
  runId: string,
  runs: SessionRunView[] | undefined,
  pending: PendingMessage[],
): string {
  const run = runs?.find((candidate) => String(candidate.id) === runId);
  if (run?.source.type === "input") {
    const text = run.source.preview?.trim();
    if (text) {
      return text;
    }
  }
  const sent = pending.find((message) => message.runId === runId);
  return sent?.text || attachmentSummary(sent?.media) || "(queued message)";
}

function SessionScrollFollower({
  followRequest,
  ready,
  entries,
  pending,
  activeRun,
  historyRevision,
}: {
  followRequest: number;
  ready: boolean;
  entries: TranscriptEntry[];
  pending: { id: string; text: string }[];
  activeRun: ActiveRun | null;
  historyRevision: number;
}) {
  const { scrollToEnd } = useMessageScroller();
  const scrollable = useMessageScrollerScrollable();
  const initialized = useRef(false);
  const followedRequest = useRef(0);
  const followedHistory = useRef(historyRevision);

  useLayoutEffect(() => {
    if (followRequest !== followedRequest.current) {
      followedRequest.current = followRequest;
      // With autoScroll enabled, scrollToEnd also switches the scroller
      // from manual reading back to following-bottom, including during catch-up.
      scrollToEnd({ behavior: "auto" });
    }
    if (!ready) {
      return;
    }
    if (followedHistory.current !== historyRevision) {
      followedHistory.current = historyRevision;
      return;
    }

    // On open, always start at the latest message. After that, append only
    // follows when the viewport was already at its end before this render;
    // a reader who scrolled up keeps their position.
    if (!initialized.current || !scrollable.end) {
      initialized.current = true;
      scrollToEnd({ behavior: "auto" });
    }
  }, [followRequest, ready, entries, pending, activeRun, historyRevision, scrollable.end, scrollToEnd]);

  return null;
}

/// Sub-agent lineage strip: where this session came from and the
/// children it delegated to. Children are re-read whenever the run revision
/// moves, since delegations appear and close mid-run.
function relativeTime(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h`;
  return `${Math.floor(delta / 86_400_000)}d`;
}

/// An empty session: an invitation to whoever can write to it, a plain note
/// to anyone who cannot.
function SessionWelcome({
  state,
  unshared,
}: {
  state: "open" | "closed" | "readOnly" | "controlled";
  unshared: boolean;
}) {
  const [title, body] = {
    open: [
      "What should we work on?",
      "Ask a question or hand over a task. The agent works through it here, step by step, and you can steer it while it runs.",
    ],
    controlled: [
      "Nothing here yet",
      "This session is driven by its manager; its work will appear here.",
    ],
    readOnly: ["Nothing here yet", "Messages and the agent's work will appear here."],
    closed: ["This session is closed", "It ended before anything was said."],
  }[state];
  return (
    <div className="flex min-h-[45vh] flex-col items-center justify-center gap-3 px-4 text-center">
      <SessionIcon className="size-10 text-muted-foreground/50" />
      <h2 className="text-lg font-medium tracking-tight">{title}</h2>
      <p className="max-w-md text-sm text-muted-foreground">{body}</p>
      {unshared && state === "open" && (
        <p className="max-w-md text-xs text-muted-foreground">
          Only you and the universe's admins see this session until you share it.
        </p>
      )}
    </div>
  );
}
