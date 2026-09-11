import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent } from "react";
import {
  type InfiniteData,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { NavLink, useLocation, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { Archive, ArrowLeft, ChevronDown, ListChecks, ListFilter, LoaderCircle, Plus, ShieldCheck, SlidersHorizontal, Trash2, X } from "lucide-react";
import {
  api,
  botLabel,
  type BotListResponse,
  type Environment,
  type InlineProfile,
  type ProfileDocument,
  type ProfileSource,
  type ProfileSummary,
  type SessionListPage,
  type SessionEnvironmentOverride,
  type SessionOrigin,
  type SessionRunAccepted,
  type SessionRunApprovalsDecided,
  SessionRunCancelled,
  SessionRunSteered,
  SessionRunView,
  type SessionSummary,
  type SessionView,
} from "@/api";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { BotFaceIcon } from "@/components/icons/bot";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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
import { MetadataMapEditor } from "@/components/session/metadata-editor";
import { SessionMenuIdentity, SessionMenuMetadata } from "@/components/session/session-menu-details";
import { SessionMenuPreferences } from "@/components/session/session-menu-preferences";
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
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
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
import { SessionComposer, type ComposerMode } from "@/components/session/composer";
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
import { TranscriptLinksContext, type TranscriptLinks } from "@/components/session/tool-trace";
import { sectionsByRun } from "@/lib/sessions/run-sections";
import { CenteredNote, LoadingNote, UniverseNotFound } from "@/components/page";
import { useSessionTail } from "@/lib/sessions/tail";
import {
  runInProgress,
  type ActiveRun,
  type TranscriptEntry,
} from "@/lib/sessions/transcript";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import { managedSessionBotId, managedSessionOwnerLabel } from "@/lib/sessions/management";
import {
  hasSessionFeature,
  selectableEnvironments,
  resourceFeatureDisableReasons,
  setupResourceFeatureError,
} from "@/lib/sessions/resource-features";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { canManage, useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import {
  metadataFilterFromSearchParams,
  parseMetadataPair,
  readSessionMetadataFilter,
  readSessionListPreferences,
  sessionListActiveFilterCount,
  searchParamsWithMetadataFilter,
  writeSessionMetadataFilter,
  writeSessionListPreferences,
} from "@/lib/sessions/list-preferences";

/// U4a+U4d: master-detail session chat. Pane = paged session list plus
/// New session (sub-agent tree expansion arrives with engine D1 parent
/// linkage); detail = live transcript (long-poll tail) with a composer.
const SESSION_LIST_REFRESH_MS = 5_000;
const INLINE_SUBAGENT_LIMIT = 5;

export function SessionsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { sessionId } = useParams<{ sessionId: string }>();
  const location = useLocation();

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
        <SessionList key={universe.id} universeId={universe.id} slug={slug!} activeId={sessionId} />
      </aside>
      <section className={cn("min-w-0 flex-1 flex-col", sessionId ? "flex" : "hidden md:flex")}>
        <ProviderReadinessBanner universeId={universe.id} slug={slug!} />
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
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const metadataFilter = metadataFilterFromSearchParams(searchParams);
  const [filterDraft, setFilterDraft] = useState("");
  const [metadataKeyDraft, setMetadataKeyDraft] = useState("");
  const filterEntries = Object.entries(metadataFilter);
  const [preferences, setPreferences] = useState(() => readSessionListPreferences(universeId));
  const { showClosed, showSubagents, showSessionIds, metadataKeys } = preferences;
  const listQuery = new URLSearchParams({ limit: "50" });
  if (!showClosed) listQuery.set("excludeClosed", "true");
  for (const [key, value] of filterEntries) {
    listQuery.append("metadata", value ? `${key}=${value}` : key);
  }
  const pages = useInfiniteQuery({
    queryKey: ["sessions", universeId, metadataFilter, { showClosed }],
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
  const [createOpen, setCreateOpen] = useState(false);
  const [selecting, setSelecting] = useState(false);
  const [selectingAll, setSelectingAll] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [bulkNotice, setBulkNotice] = useState<string | null>(null);

  const allSessions = pages.data?.pages.flatMap((page) => page.sessions) ?? [];
  const sessions = allSessions.filter((session) => showSubagents || !session.origin);
  const tree = buildSessionTree(sessions);
  const visibleIds = sessions.map((session) => session.id);
  const selectedSessions = sessions.filter((session) => selected.has(session.id));
  const selectedOpen = selectedSessions.filter((session) => session.lifecycleStatus !== "closed");
  const selectedClosed = selectedSessions.filter((session) => session.lifecycleStatus === "closed");
  const allVisibleSelected = visibleIds.length > 0 && visibleIds.every((id) => selected.has(id));
  const activeFilterCount = sessionListActiveFilterCount(metadataFilter, preferences);
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
    mutationFn: async ({ action, ids }: { action: "close" | "delete"; ids: string[] }) => {
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

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <h1 className="text-sm font-semibold">Sessions</h1>
        <span className="text-xs text-muted-foreground">
          {sessions.length}
          {pages.hasNextPage ? "+" : ""}
        </span>
        <Popover>
          <PopoverTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                className={cn("relative ml-auto", activeFilterCount > 0 && "text-primary")}
                aria-label={activeFilterCount > 0
                  ? `Filter sessions, ${activeFilterCount} active`
                  : "Filter sessions"}
              />
            }
          >
            <ListFilter />
            {activeFilterCount > 0 && (
              <span className="absolute -right-0.5 -top-0.5 flex size-4 items-center justify-center rounded-full bg-primary text-[9px] font-semibold text-primary-foreground">
                {activeFilterCount}
              </span>
            )}
          </PopoverTrigger>
          <PopoverContent align="end" className="grid gap-4 p-4">
            <h2 className="text-sm font-semibold">Filter sessions</h2>
            <div className="grid gap-2">
              <label className="flex cursor-pointer items-center gap-2 text-sm">
                <Checkbox
                  checked={!showClosed}
                  onCheckedChange={(checked) => setPreferences((current) => ({
                    ...current,
                    showClosed: checked !== true,
                  }))}
                />
                Hide closed sessions
              </label>
              <label className="flex cursor-pointer items-center gap-2 text-sm">
                <Checkbox
                  checked={!showSubagents}
                  onCheckedChange={(checked) => setPreferences((current) => ({
                    ...current,
                    showSubagents: checked !== true,
                  }))}
                />
                Hide sub-agent sessions
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
                        <X className="size-3" />
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
              <h2 className="text-sm font-semibold">List Appearance</h2>
              <label className="flex cursor-pointer items-center gap-2 text-sm">
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
                        <X className="size-3" />
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
        <Button
          variant="ghost"
          size="icon-sm"
          className={cn(selecting && "text-primary")}
          onClick={() => (selecting ? exitSelecting() : setSelecting(true))}
          aria-label={selecting ? "Exit selection" : "Select sessions"}
          title={selecting ? "Exit selection" : "Select sessions to close or delete"}
        >
          <ListChecks />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => setCreateOpen(true)}
          aria-label="New session"
        >
          <Plus />
        </Button>
      </div>
      {selecting && (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b bg-muted/40 px-4 py-2 text-xs">
          <Checkbox
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
          <div className="ml-auto flex items-center gap-1">
            <BulkActionDialog
              action="close"
              count={selectedOpen.length}
              pending={bulk.isPending || selectingAll}
              onConfirm={() =>
                bulk.mutate({ action: "close", ids: selectedOpen.map((session) => session.id) })
              }
            />
            <BulkActionDialog
              action="delete"
              count={selectedClosed.length}
              pending={bulk.isPending || selectingAll}
              onConfirm={() =>
                bulk.mutate({ action: "delete", ids: selectedClosed.map((session) => session.id) })
              }
            />
          </div>
        </div>
      )}
      {bulkNotice && (
        <p className="flex shrink-0 items-center gap-2 border-b px-4 py-1.5 text-xs text-muted-foreground">
          {bulk.isPending && <LoaderCircle className="size-3 animate-spin" />}
          {bulkNotice}
          <button
            type="button"
            className="ml-auto hover:text-foreground"
            onClick={() => setBulkNotice(null)}
            aria-label="Dismiss"
          >
            <X className="size-3" />
          </button>
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {pages.isLoading && <p className="p-4 text-sm text-muted-foreground">Loading…</p>}
        {pages.error && (
          <p className="p-4 text-sm text-destructive">{pages.error.message}</p>
        )}
        {pages.data && allSessions.length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            {filterEntries.length > 0
              ? "No sessions match this metadata filter."
              : !showClosed
                ? "No open sessions."
              : "No sessions yet — start one, or bind a chat."}
          </p>
        )}
        {pages.data && !showSubagents && allSessions.length > 0 && sessions.length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            No top-level sessions in the loaded results.
          </p>
        )}
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
              showSessionIds={showSessionIds}
              metadataKeys={metadataKeys}
              search={listSearch}
            />
          ))}
        </ul>
      </div>
      {pages.hasNextPage && (
        <div className="shrink-0 border-t p-2">
          <Button
            variant="outline"
            size="sm"
            className="w-full"
            disabled={pages.isFetchingNextPage}
            onClick={() => void pages.fetchNextPage()}
          >
            {pages.isFetchingNextPage ? "Loading…" : "Load more sessions"}
          </Button>
        </div>
      )}
      <NewSessionDialog
        universeId={universeId}
        slug={slug}
        open={createOpen}
        onOpenChange={setCreateOpen}
        search={listSearch}
      />
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

function BulkActionDialog({
  action,
  count,
  pending,
  onConfirm,
}: {
  action: "close" | "delete";
  count: number;
  pending: boolean;
  onConfirm: () => void;
}) {
  const [open, setOpen] = useState(false);
  const verb = action === "close" ? "Close" : "Delete";
  const noun = count === 1 ? "session" : "sessions";
  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      <AlertDialogTrigger
        render={
          <Button
            variant="outline"
            size="sm"
            className="text-destructive"
            disabled={count === 0 || pending}
          />
        }
      >
        {action === "close" ? <Archive /> : <Trash2 />}
        {verb} {count}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {verb} {count} {noun}?
          </AlertDialogTitle>
          <AlertDialogDescription>
            {action === "close"
              ? "Each selected open session is force-closed in turn: active and queued work is cancelled and the session cannot be reopened. Closed sessions in the selection are left alone."
              : "Each selected closed session is deleted in turn, removing its history. Open sessions in the selection are left alone."}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive text-white hover:bg-destructive/90"
            onClick={() => {
              setOpen(false);
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
/// loaded page set. Children whose parent is not loaded stay
/// at the top level and still carry their sub-agent badge; list order (most
/// recently updated first) is preserved at every level.
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
  showSessionIds: boolean;
  metadataKeys: string[];
  search: string;
}

function SessionTreeItem({
  node,
  slug,
  activeId,
  depth,
  ...controls
}: {
  node: SessionNode;
  slug: string;
  activeId: string | undefined;
  depth: number;
} & SessionRowControls) {
  return (
    <>
      <SessionListItem
        session={node.session}
        slug={slug}
        active={node.session.id === activeId}
        depth={depth}
        {...controls}
      />
      {node.children.map((child) => (
        <SessionTreeItem
          key={child.session.id}
          node={child}
          slug={slug}
          activeId={activeId}
          depth={depth + 1}
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
  depth = 0,
  selecting,
  selected,
  onToggle,
  showSessionIds,
  metadataKeys,
  search,
}: {
  session: SessionSummary;
  slug: string;
  active: boolean;
  depth?: number;
} & SessionRowControls) {
  const botManaged = session.managed && session.id.startsWith("bot:v1:");
  const origin = session.origin ?? null;
  const isSelected = selected.has(session.id);
  const displayName = session.displayName?.trim();
  const showSecondaryId = showSessionIds && Boolean(displayName);
  const visibleMetadata = metadataKeys.flatMap((key) => {
    const value = session.metadata?.[key];
    return value === undefined ? [] : [{ key, value }];
  });
  const indent = depth > 0 ? { paddingLeft: `${1 + depth * 1.25}rem` } : undefined;
  const summary = (
    <>
      <span className="flex min-w-0 items-center gap-2">
        {depth > 0 && <span className="shrink-0 text-muted-foreground">↳</span>}
        <span className="min-w-0 flex-1 truncate font-medium" title={displayName ? undefined : session.id}>
          {displayName || session.id}
        </span>
        {origin && (
          <Badge
            variant="outline"
            title={`Sub-agent of ${origin.parentSessionId} (depth ${origin.depth}, profile ${origin.agent.profileId} rev ${origin.agent.revision})`}
          >
            sub-agent
          </Badge>
        )}
        {session.managed && (
          <Badge className="shrink-0" variant="secondary" title={botManaged ? "Bot-managed session" : undefined}>
            {botManaged && <BotFaceIcon />}
            {botManaged ? "Bot" : "Managed"}
          </Badge>
        )}
        {session.lifecycleStatus === "closed" && (
          <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
            closed
          </span>
        )}
        <span className="ml-auto shrink-0 font-sans text-xs text-muted-foreground">
          {relativeTime(session.updatedAtMs)}
        </span>
      </span>
      {showSecondaryId && (
        <span className="truncate font-mono text-xs text-muted-foreground" title={session.id}>
          {session.id}
        </span>
      )}
      {visibleMetadata.map(({ key, value }) => (
        <span
          key={key}
          className="truncate font-mono text-[11px] text-muted-foreground"
          title={`${key}=${value}`}
        >
          {key}={value}
        </span>
      ))}
    </>
  );
  const rowClass = cn(
    "flex flex-col gap-0.5 border-b px-4 py-2.5 text-sm hover:bg-muted/50",
    (active || isSelected) && "bg-muted",
  );
  return (
    <li>
      {selecting ? (
        <label className={cn(rowClass, "cursor-pointer")} style={indent}>
          <span className="flex items-start gap-2">
            <Checkbox
              checked={isSelected}
              onCheckedChange={() => onToggle(session.id)}
              className="mt-0.5"
              aria-label={`Select ${session.displayName ?? session.id}`}
            />
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">{summary}</span>
          </span>
        </label>
      ) : (
        <NavLink
          to={`/u/${slug}/sessions/${session.id}${search ? `?${search}` : ""}`}
          className={rowClass}
          style={indent}
        >
          {summary}
        </NavLink>
      )}
    </li>
  );
}

function NewSessionDialog({
  universeId,
  slug,
  open,
  onOpenChange,
  search,
}: {
  universeId: string;
  slug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  search: string;
}) {
  const [displayName, setDisplayName] = useState("");
  const [profileId, setProfileId] = useState("");
  const [step, setStep] = useState<"basics" | "setup">("basics");
  const [inlineProfile, setInlineProfile] = useState<InlineProfile | null>(null);
  const [environmentOverride, setEnvironmentOverride] = useState<SessionEnvironmentOverride>();
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
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled: open,
  });
  const create = useMutation({
    mutationFn: () =>
      api<SessionView>("POST", `/api/v1/universes/${universeId}/sessions`, {
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        profile: profileForCreate(profileId, inlineProfile, selectedProfile.data),
        ...(environmentOverride ? { environment: environmentOverride } : {}),
      }),
    onSuccess: async (session) => {
      await queryClient.invalidateQueries({ queryKey: ["sessions", universeId] });
      onOpenChange(false);
      const target = session.id;
      setDisplayName("");
      setProfileId("");
      setStep("basics");
      setInlineProfile(null);
      setEnvironmentOverride(undefined);
      setConfigError(null);
      setRetentionError(null);
      setError(null);
      navigate(`/u/${slug}/sessions/${target}${search ? `?${search}` : ""}`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const resourceError = setupResourceFeatureError(
      inlineProfile ?? selectedProfile.data ?? {},
    );
    if (configError || retentionError || resourceError) {
      setError(configError ? `Config: ${configError}` : retentionError ? `Retention: ${retentionError}` : resourceError);
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
      setEnvironmentOverride(undefined);
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
    setEnvironmentOverride(undefined);
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
                    setEnvironmentOverride(undefined);
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
              {profileId
                && selectedProfile.data
                && hasSessionFeature(selectedProfile.data.config, "environments")
                && !inlineProfile ? (
                <Field>
                  <FieldLabel>Environment</FieldLabel>
                  <Select
                    value={environmentOverrideValue(environmentOverride)}
                    onValueChange={(value) => {
                      setEnvironmentOverride(environmentOverrideFromValue(value as string));
                      setError(null);
                    }}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="profile">Use profile default</SelectItem>
                      <SelectItem value="none">No active environment</SelectItem>
                      {selectableEnvironments(environments.data ?? []).map((environment) => (
                        <SelectItem
                          key={environment.environmentId}
                          value={`existing:${environment.environmentId}`}
                        >
                          {environment.displayName ?? environment.environmentId}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <FieldDescription>
                    Override this session’s environment without converting the profile to an inline setup.
                  </FieldDescription>
                </Field>
              ) : null}
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
                  disabled={create.isPending || Boolean(configError || retentionError || resourceFeatureError)}
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
  onRetentionValidityChange,
  onChange,
}: {
  value: InlineProfile;
  options: ReturnType<typeof useSessionConfigEditorOptions>;
  environments: Environment[] | undefined;
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
        title="Model configuration"
        description="Choose the model and its default reasoning behavior. Unset values inherit deployment or provider defaults."
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
          retentionSetup={(
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
          environmentSetup={(
            <ProfileEnvironmentEditor
              embedded
              value={value.environment}
              environments={environments}
              bindings={options.environmentBindings}
              templates={options.environmentTemplates}
              secrets={options.secrets}
              onChange={(environment) => change((next) => {
                if (environment) next.environment = environment;
                else delete next.environment;
              })}
            />
          )}
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
  if (document.environment) profile.environment = structuredClone(document.environment);
  return profile;
}

function environmentOverrideValue(environment: SessionEnvironmentOverride | undefined): string {
  if (!environment) return "profile";
  return environment.type === "none" ? "none" : `existing:${environment.environmentId}`;
}

function environmentOverrideFromValue(value: string): SessionEnvironmentOverride | undefined {
  if (value === "profile") return undefined;
  if (value === "none") return { type: "none" };
  return { type: "existing", environmentId: value.slice("existing:".length) };
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
   * managed session takes plain input — the bot page is where its operator
   * talks to it, so there is nothing to override.
   */
  embedded?: boolean;
  /** Where lineage links go; defaults to the Sessions page. */
  sessionHref?: (sessionId: string) => string;
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
  const [pending, setPending] = useState<PendingMessage[]>([]);
  const [pendingSteers, setPendingSteers] = useState<PendingSteer[]>([]);
  const [notices, setNotices] = useState<{ id: string; text: string }[]>([]);
  const [stoppingRunId, setStoppingRunId] = useState<string | null>(null);
  const [cancellingQueued, setCancellingQueued] = useState<Set<string>>(() => new Set());
  const [followRequest, setFollowRequest] = useState(0);
  const [sendError, setSendError] = useState<string | null>(null);
  const [closeError, setCloseError] = useState<string | null>(null);
  const [closeOpen, setCloseOpen] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleteCascade, setDeleteCascade] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [decidingApproval, setDecidingApproval] = useState<{
    approvalId: string;
    decision: "approve" | "reject";
  } | null>(null);
  const [approvalError, setApprovalError] = useState<{
    approvalId: string;
    message: string;
  } | null>(null);

  const { showRunStatistics, collapseCompletedRuns } = useUserPreferences();
  const entries = tail.transcript.entries;
  const loadFullText = useCallback(
    async (blobRef: string) => {
      const result = await api<{ bytesBase64: string }>(
        "GET", `/api/v1/universes/${universeId}/blobs/${encodeURIComponent(blobRef)}`,
      );
      return new TextDecoder().decode(Uint8Array.from(atob(result.bytesBase64), (char) => char.charCodeAt(0)));
    },
    [universeId],
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
  const canSteer = steerTargetRunId !== null && !(activeRun?.cancelling ?? false) && !stopping;
  const queuedItems: QueuedRunItem[] = [
    ...queuedRuns.map((run) => {
      const sent = resolvedPending.find((message) => message.runId === run.runId);
      return {
        key: sent?.id ?? run.runId,
        runId: run.runId,
        text: queuedRunText(run.runId, sessionRuns, resolvedPending),
        cancelling: cancellingQueued.has(run.runId),
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
        text: message.text,
        pending: true,
      })),
  ];
  const closed = session.data?.status === "closed";
  const management = session.data?.management;
  const managed = session.data?.managed === true;
  const managerLabel = managedSessionOwnerLabel(management);
  const owningBotId = managedSessionBotId(management, session.data?.metadata);
  const owningBotHref = owningBotId
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
  const transcriptLinks = useMemo<TranscriptLinks>(() => {
    const names = new Map((botRoster ?? []).map((bot) => [bot.botId, botLabel(bot)]));
    const href = sessionHref ?? ((target: string) => `/u/${slug}/sessions/${target}`);
    return {
      botName: (botId) => names.get(botId),
      sessionHref: href,
      navigate: (target) => void navigate(target),
    };
  }, [botRoster, sessionHref, slug, navigate]);
  // Operator override: the engine happily admits direct runs on a managed
  // session (they queue like any client run), so the gate here is policy,
  // not capability. Off by default because direct input bypasses the
  // manager's ingress; resets when the operator navigates away.
  const managedGate = managed && !embedded;
  const [directInput, setDirectInput] = useState(false);
  useEffect(() => {
    setDirectInput(false);
  }, [sessionId]);

  useEffect(() => {
    if (settingsOpen && !runActive) {
      void session.refetch();
    }
  }, [settingsOpen, runActive]);

  const send = async (text: string, mode: ComposerMode | null) => {
    setSendError(null);
    if (mode === "steer") {
      await steer(text);
      return;
    }
    // The submission id doubles as the engine idempotency key: a retried
    // POST returns the original run instead of starting a second one.
    const submissionId = crypto.randomUUID();
    const expectQueued = runActive;
    setPending((prev) => [
      ...prev,
      { id: submissionId, text, runId: null, status: "sending", expectQueued },
    ]);
    try {
      const accepted = await api<SessionRunAccepted>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/messages`,
        { text, submissionId },
      );
      setFollowRequest((request) => request + 1);
      setPending((prev) =>
        prev.map((message) =>
          message.id === submissionId
            ? {
                ...message,
                runId: message.runId ?? accepted.run.id,
                status: accepted.run.status === "queued" ? "queued" : "running",
              }
            : message,
        ),
      );
    } catch (error) {
      setPending((prev) => prev.filter((message) => message.id !== submissionId));
      setSendError(error instanceof Error ? error.message : String(error));
    }
  };

  const steer = async (text: string) => {
    const runId = steerTargetRunId;
    if (!runId) {
      setSendError(
        "There is no run to steer yet — wait a moment and try again, or press Enter to queue it.",
      );
      return;
    }
    const id = crypto.randomUUID();
    setPendingSteers((prev) => [...prev, { id, runId, text }]);
    try {
      await api<SessionRunSteered>(
        "POST",
        `/api/v1/universes/${universeId}/sessions/${sessionId}/runs/${runId}/steer`,
        { text },
      );
      setFollowRequest((request) => request + 1);
    } catch (error) {
      setPendingSteers((prev) => prev.filter((steer) => steer.id !== id));
      setSendError(error instanceof Error ? error.message : String(error));
    }
  };

  const cancelRun = async (runId: string) => {
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
    setCancellingQueued((prev) => new Set(prev).add(runId));
    const response = await cancelRun(runId);
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

  const decideApproval = async (
    approvalId: string,
    decision: "approve" | "reject",
  ) => {
    if (!approvalRun) return;
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
        `/api/v1/universes/${universeId}/sessions/${sessionId}${deleteCascade ? "?cascade=true" : ""}`,
      ),
    onSuccess: async () => {
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
    },
    onError: (error) => setDeleteError(error.message),
  });

  return (
    <>
      {!embedded && (
        <>
        <header className="flex h-12 min-w-0 shrink-0 items-center gap-3 overflow-hidden border-b px-4">
          <NavLink to={backTo} className="shrink-0 md:hidden">
            <ArrowLeft className="size-4" />
          </NavLink>
          <div className="flex min-w-0 items-center gap-0.5">
            <h1 className="min-w-0 truncate text-sm font-semibold">
              {session.data?.displayName ?? sessionId.slice(0, 24)}
            </h1>
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="shrink-0 text-muted-foreground"
                    aria-label="Session details"
                    title="Session details"
                  />
                }
              >
                <ChevronDown />
              </DropdownMenuTrigger>
              <DropdownMenuContent
                align="start"
                className="max-h-[min(28rem,calc(100vh-1rem))] w-80 max-w-[calc(100vw-1rem)]"
              >
                <SessionMenuIdentity sessionId={sessionId} />
                <SessionMenuPreferences />
                {owningBotHref && (
                  <>
                    <DropdownMenuSeparator />
                    <DropdownMenuGroup>
                    <DropdownMenuItem onClick={() => navigate(owningBotHref)}>
                      <BotFaceIcon /> Open in bot
                    </DropdownMenuItem>
                    </DropdownMenuGroup>
                  </>
                )}
                <SessionMenuMetadata metadata={session.data?.metadata} />
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
          <div className="ml-auto flex min-w-0 shrink-0 items-center gap-1">
            {activeRun && (
              <span className="hidden max-w-40 shrink truncate text-xs text-muted-foreground xl:inline">
                {activeRun.label}…
              </span>
            )}
            {closed && (
              <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
                Closed
              </span>
            )}
            {managed && (
              <Tooltip>
                <TooltipTrigger render={<span className="shrink-0" />}>
                  <Badge variant="secondary" className="gap-1">
                    <ShieldCheck />
                    <span className="hidden xl:inline">Managed by {managerLabel}</span>
                  </Badge>
                </TooltipTrigger>
                <TooltipContent>
                  {`Lifecycle and chat input are controlled by ${managerLabel}; configuration remains editable.`}
                </TooltipContent>
              </Tooltip>
            )}
            {!closed && !managed && (
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-destructive"
                disabled={closeSession.isPending}
                onClick={() => {
                  setCloseError(null);
                  setCloseOpen(true);
                }}
                aria-label={runActive ? "Force close session" : "Close session"}
                title={runActive ? "Force close session" : "Close session"}
              >
                {closeSession.isPending ? <LoaderCircle className="animate-spin" /> : <Archive />}
              </Button>
            )}
            {closed && !managed && (
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-destructive"
                disabled={deleteSession.isPending}
                onClick={() => {
                  setDeleteError(null);
                  setDeleteCascade(false);
                  setDeleteOpen(true);
                }}
                aria-label="Delete session"
                title="Delete session"
              >
                {deleteSession.isPending ? <LoaderCircle className="animate-spin" /> : <Trash2 />}
              </Button>
            )}
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => setSettingsOpen(true)}
              aria-label="Session settings"
              title="Session settings"
            >
              <SlidersHorizontal />
            </Button>
          </div>
        </header>

        <AlertDialog
          open={closeOpen}
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

        <AlertDialog
          open={deleteOpen}
          onOpenChange={(open) => {
            setDeleteOpen(open);
            if (open) {
              setDeleteError(null);
              setDeleteCascade(false);
            }
          }}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete this session permanently?</AlertDialogTitle>
              <AlertDialogDescription>
                This removes the session and its retained history. It cannot be undone.
                A session with history forks or delegated children cannot be deleted
                unless cascade is enabled.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <label className="flex min-w-0 items-start gap-2 rounded-lg border p-3 text-sm">
              <Checkbox
                checked={deleteCascade}
                onCheckedChange={(checked) => setDeleteCascade(checked === true)}
                disabled={deleteSession.isPending}
              />
              <span className="min-w-0">
                <span className="block font-medium">Also delete forks and delegated children</span>
                <span className="block text-xs text-muted-foreground">
                  Every descendant must already be closed. Config-only clones are not included.
                </span>
              </span>
            </label>
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
        </>
      )}
      <SessionLineage
        universeId={universeId}
        slug={slug}
        sessionId={sessionId}
        origin={session.data?.origin ?? null}
        runRevision={runRevision}
        sessionHref={sessionHref}
      />
      <TranscriptLinksContext.Provider value={transcriptLinks}>
      <MessageScrollerProvider key={`${universeId}/${sessionId}`} autoScroll defaultScrollPosition="end">
        <MessageScroller className="min-h-0 flex-1">
          <MessageScrollerViewport preserveScrollOnPrepend>
            <SessionHistoryLoader tail={tail} />
            <MessageScrollerContent className="mx-auto w-full max-w-5xl gap-3 px-4 py-6 md:px-8">
              {tail.phase === "loading" && entries.length === 0 && !tail.error && (
                <LoadingNote />
              )}
              {tail.error && entries.length === 0 && (
                <p className="text-sm text-destructive">{tail.error}</p>
              )}
              {tail.phase === "live" &&
                entries.length === 0 &&
                pendingInTranscript.length === 0 && (
                  <CenteredNote>No conversation yet — say something below.</CenteredNote>
                )}
              {sections.map((section) => (
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
              {pendingInTranscript.map((message) => (
                <MessageScrollerItem key={message.id} messageId={message.id}>
                  <UserBand text={message.text} pending />
                </MessageScrollerItem>
              ))}
              {visiblePendingSteers.map((steer) => (
                <MessageScrollerItem key={steer.id} messageId={steer.id}>
                  <UserBand text={steer.text} pending steering />
                </MessageScrollerItem>
              ))}
              {notices.map((notice) => (
                <MessageScrollerItem key={notice.id} messageId={notice.id}>
                  <TranscriptEntryView
                    entry={{ kind: "marker", key: notice.id, text: notice.text, tone: "muted" }}
                  />
                </MessageScrollerItem>
              ))}
              {approvalRun && (
                <MessageScrollerItem messageId={`approvals-${approvalRun.id}`}>
                  <ApprovalCards
                    approvals={approvalRun.pendingApprovals ?? []}
                    deciding={decidingApproval}
                    error={approvalError}
                    onDecide={(approvalId, decision) =>
                      void decideApproval(approvalId, decision)}
                  />
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
          followRequest={followRequest}
          ready={tail.phase === "live"}
          historyRevision={tail.historyRevision}
          entries={entries}
          pending={pendingInTranscript}
          activeRun={activeRun}
        />
      </MessageScrollerProvider>
      </TranscriptLinksContext.Provider>
      {!closed && (
        <QueuedRunsBar items={queuedItems} onCancel={(runId) => void cancelQueued(runId)} />
      )}
      <SessionComposer
        key={sessionDraftKey(universeId, sessionId)}
        draftKey={sessionDraftKey(universeId, sessionId)}
        runActive={runActive}
        canSteer={canSteer}
        stopping={stopping}
        disabled={closed || (managedGate && !directInput)}
        disabledReason={managedGate && !directInput
          ? `Managed by ${managerLabel} — flip Direct input to message this session anyway.`
          : undefined}
        banner={managedGate && !closed ? (
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
        error={sendError}
        onSend={(text, mode) => void send(text, mode)}
        onStop={() => void stop()}
      />
      {!embedded && (
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
  return pending.find((message) => message.runId === runId)?.text ?? "(queued message)";
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
export function SessionLineage({
  universeId,
  slug,
  sessionId,
  origin,
  runRevision,
  sessionHref,
}: {
  universeId: string;
  slug: string;
  sessionId: string;
  origin: SessionOrigin | null;
  runRevision: number;
  sessionHref?: (sessionId: string) => string;
}) {
  const href = sessionHref ?? ((id: string) => `/u/${slug}/sessions/${id}`);
  const parentId = origin?.parentSessionId;
  const parent = useQuery({
    queryKey: ["session", universeId, parentId],
    queryFn: () =>
      api<SessionView>(
        "GET",
        `/api/v1/universes/${universeId}/sessions/${encodeURIComponent(parentId!)}`,
      ),
    enabled: Boolean(parentId),
  });
  const children = useInfiniteQuery({
    queryKey: ["session-children", universeId, sessionId],
    queryFn: ({ pageParam }) => {
      const params = new URLSearchParams({ limit: "50", parentSessionId: sessionId });
      if (pageParam) params.set("cursor", pageParam);
      return api<SessionListPage>(
        "GET",
        `/api/v1/universes/${universeId}/sessions?${params.toString()}`,
      );
    },
    initialPageParam: "",
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
  const refetchChildren = children.refetch;
  useEffect(() => {
    // Run activity refreshes the same lineage. Keep its data mounted while
    // fetching so the header does not collapse and resize the transcript.
    if (runRevision > 0) {
      void refetchChildren();
    }
  }, [runRevision, refetchChildren]);
  const list = children.data?.pages.flatMap((page) => page.sessions) ?? [];
  const inlineChildren = list.slice(0, INLINE_SUBAGENT_LIMIT);
  const overflowChildren = list.slice(INLINE_SUBAGENT_LIMIT);
  const hiddenCount = overflowChildren.length;
  const parentName = parent.data?.displayName?.trim();
  const parentLabel = parentName || (parentId ? compactSessionId(parentId) : "");
  const tagClass = "inline-flex min-w-0 max-w-64 items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium text-foreground transition-colors hover:bg-muted";
  if (!origin && list.length === 0) return null;
  return (
    <div className="shrink-0 border-b bg-muted/30">
      <div className="mx-auto flex w-full max-w-5xl flex-wrap items-center gap-x-3 gap-y-1 px-4 py-1.5 text-xs text-muted-foreground md:px-8">
      {origin && (
        <span className="flex min-w-0 flex-wrap items-center gap-1">
          <span>Parent:</span>
          <NavLink to={href(origin.parentSessionId)} className={tagClass}>
            <span className={cn("truncate", !parentName && "font-mono font-normal")}>
              {parentLabel}
            </span>
            {parent.data?.status && (
              <span
                className={cn(
                  "size-1.5 shrink-0 rounded-full",
                  parent.data.status === "closed" ? "bg-muted-foreground/50" : "bg-foreground",
                )}
                aria-hidden="true"
              />
            )}
          </NavLink>
        </span>
      )}
      {list.length > 0 && (
        <span className="flex min-w-0 flex-wrap items-center gap-1">
          <span>Sub-agents ({list.length}{children.hasNextPage ? "+" : ""}):</span>
          {inlineChildren.map((child) => (
            <SubagentLineageLink
              key={child.id}
              child={child}
              to={href(child.id)}
              className={tagClass}
            />
          ))}
          {(hiddenCount > 0 || children.hasNextPage) && (
            <Popover>
              <PopoverTrigger
                render={
                  <button
                    type="button"
                    className={tagClass}
                    aria-label={`Show ${hiddenCount}${children.hasNextPage ? " or more" : ""} additional sub-agents`}
                  />
                }
              >
                +{hiddenCount}{children.hasNextPage ? "+" : ""}
              </PopoverTrigger>
              <PopoverContent align="start" className="w-80 p-2">
                <div className="mb-1 px-2 py-1 text-xs font-medium text-muted-foreground">
                  Additional sub-agents
                </div>
                <div className="max-h-72 overflow-y-auto">
                  {overflowChildren.map((child) => (
                    <SubagentLineageLink
                      key={child.id}
                      child={child}
                      to={href(child.id)}
                      className="flex min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-sm text-foreground transition-colors hover:bg-muted"
                    />
                  ))}
                  {hiddenCount === 0 && children.isFetchingNextPage && (
                    <p className="px-2 py-1.5 text-xs text-muted-foreground">Loading…</p>
                  )}
                </div>
                {children.hasNextPage && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="mt-1 w-full"
                    disabled={children.isFetchingNextPage}
                    onClick={() => void children.fetchNextPage()}
                  >
                    {children.isFetchingNextPage ? "Loading…" : "Load more sub-agents"}
                  </Button>
                )}
              </PopoverContent>
            </Popover>
          )}
        </span>
      )}
      </div>
    </div>
  );
}

function SubagentLineageLink({
  child,
  to,
  className,
}: {
  child: SessionSummary;
  to: string;
  className: string;
}) {
  const childName = child.displayName?.trim();
  return (
    <NavLink to={to} className={className}>
      <span className={cn("min-w-0 flex-1 truncate", !childName && "font-mono font-normal")}>
        {childName || compactSessionId(child.id)}
      </span>
      <span
        className={cn(
          "size-1.5 shrink-0 rounded-full",
          child.lifecycleStatus === "closed" ? "bg-muted-foreground/50" : "bg-foreground",
        )}
        aria-hidden="true"
      />
    </NavLink>
  );
}

function compactSessionId(id: string, length = 18): string {
  return `${id.slice(0, length)}${id.length > length ? "…" : ""}`;
}

function relativeTime(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h`;
  return `${Math.floor(delta / 86_400_000)}d`;
}
