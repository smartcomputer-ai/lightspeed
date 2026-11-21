import { activeEnvironmentItems, sessionCollections, sessionMetaItems, sessionName } from "@lightspeed-ai/session-components";
import { rosterQuery } from "./roster-query";
import {
  useWatchConnected,
  useCreateParam,
  useActionPermissions,
  useSessionClosePermission,
  useSessionDeletePermission,
} from "@lightspeed-ai/platform-web/modules";
import { SessionDot } from "@lightspeed-ai/platform-web/modules/ui/status";
import {
  requestRowMenu,
  LIST_CHECKBOX_INSET_CLASS,
  ListPaneBody,
  ListPaneFooter,
  ListPaneHeader,
  ListPaneState,
  ListPaneToolbar,
  ListRow,
  ListRowLine,
  ListSelectionButtons,
  type ListRowExpander,
  type ListRowTree,
  type ListSelectionAction,
} from "@lightspeed-ai/platform-web/modules/ui/lists";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { AddIcon, DeleteIcon, FilterIcon, LoadingIcon, RemoveIcon, SelectModeIcon, SessionCloseIcon } from "@lightspeed-ai/platform-web/modules/ui/icons";
import { api, type SessionListPage, type Environment, type SessionSummary, type SessionView } from "@lightspeed-ai/platform-web/modules/client";
import { Button } from "@lightspeed-ai/platform-web/modules/ui/button";
import { Badge } from "@lightspeed-ai/platform-web/modules/ui/badge";
import { Checkbox } from "@lightspeed-ai/platform-web/modules/ui/checkbox";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@lightspeed-ai/platform-web/modules/ui/alert-dialog";
import { Input } from "@lightspeed-ai/platform-web/modules/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@lightspeed-ai/platform-web/modules/ui/popover";
import { openEnvironmentsQuery } from "@lightspeed-ai/platform-web/modules/environments";
import { cn } from "@lightspeed-ai/platform-web/modules/ui";
import {
  sessionListPreferences,
  sessionMetadataFilter,
  metadataFilterFromSearchParams,
  parseMetadataPair,
  hiddenSessionsNote,
  searchParamsWithMetadataFilter,
} from "./list-preferences";
import { settings, useSetting } from "@lightspeed-ai/platform-web/modules/settings";
import { NewSessionDialog } from "./new-session-dialog";

const SESSION_LIST_REFRESH_MS = 5_000;

export function SessionList({
  universeId,
  handle,
  activeId,
}: {
  universeId: string;
  handle: string;
  activeId: string | undefined;
}) {
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const metadataFilter = metadataFilterFromSearchParams(searchParams);
  const [filterDraft, setFilterDraft] = useState("");
  const [metadataKeyDraft, setMetadataKeyDraft] = useState("");
  const filterEntries = Object.entries(metadataFilter);
  const [preferences, setPreferences] = useSetting(sessionListPreferences(universeId));
  const { showClosed, showSubagents, showControlledSessions, showSessionIds, metadataKeys } = preferences;
  const watchConnected = useWatchConnected(universeId);
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
    ...rosterQuery(universeId, metadataFilter, { showClosed, showControlledSessions, showSubagents }),
    queryFn: ({ pageParam }) =>
      api<SessionListPage>(
        "GET",
        `/api/ui/universes/${universeId}/modules/sessions?${listQuery.toString()}${
          pageParam ? `&cursor=${encodeURIComponent(pageParam)}` : ""
        }`,
    ),
    initialPageParam: "",
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    // Sessions can be created by runtime workflows (not only by this browser),
    // so frontend mutation invalidation alone cannot keep this list current.
    refetchInterval: watchConnected ? 60_000 : SESSION_LIST_REFRESH_MS,
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
  const openRows = useOpenSessionRows(tree, activeId);
  const visibleIds = sessions.map((session) => session.id);
  const selectedSessions = sessions.filter((session) => selected.has(session.id));
  const permissions = useActionPermissions(universeId);
  const mayClose = useSessionClosePermission(universeId);
  const mayDelete = useSessionDeletePermission(universeId);
  const canCreate = permissions.can("createSession");
  const canSelect = sessions.some((session) => session.lifecycleStatus === "closed" ? mayDelete(session.access) : (!session.lifecycleControlled || permissions.can("administer")) && mayClose(session.access));
  const selectedOpen = selectedSessions.filter((session) => (!session.lifecycleControlled || permissions.can("administer")) && session.lifecycleStatus !== "closed" && mayClose(session.access));
  const selectedClosed = selectedSessions.filter((session) => session.lifecycleStatus === "closed" && mayDelete(session.access));
  const allVisibleSelected = visibleIds.length > 0 && visibleIds.every((id) => selected.has(id));
  // Only metadata filters count; which sessions the list includes is its scope.
  const activeFilterCount = filterEntries.length;
  const listSearch = searchParams.toString();
  const restoredFilterUniverse = useRef<string | null>(null);

  useEffect(() => {
    if (restoredFilterUniverse.current !== universeId) {
      restoredFilterUniverse.current = universeId;
      if (searchParams.has("metadata")) {
        rememberMetadataFilter(universeId, metadataFilter);
        return;
      }
      const stored = settings.get(sessionMetadataFilter(universeId));
      if (Object.keys(stored).length > 0) {
        setSearchParams(searchParamsWithMetadataFilter(searchParams, stored), { replace: true });
      }
      return;
    }
    rememberMetadataFilter(universeId, metadataFilter);
  }, [metadataFilter, searchParams, setSearchParams, universeId]);

  const updateMetadataFilter = (next: Record<string, string>) => {
    rememberMetadataFilter(universeId, next);
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
              `/api/ui/universes/${universeId}/sessions/${id}/close`,
              { force: true },
            )
          : api<SessionSummary>("DELETE", `/api/ui/universes/${universeId}/sessions/${id}`),
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
      await queryClient.invalidateQueries(sessionCollections(universeId));
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
      <ListPaneBody expand="title">
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
              handle={handle}
              activeId={activeId}
              openRows={openRows}
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
        handle={handle}
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
async function runBatched<T, R>(
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
/// Keeps the list's metadata filter for the universe's next visit. The URL
/// holds the filter while the list is open.
function rememberMetadataFilter(universeId: string, filter: Record<string, string>) {
  settings.set(sessionMetadataFilter(universeId), filter).catch(() => {
    // URL-backed filtering still works when browser storage does not.
  });
}

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

export type SessionNode = { session: SessionSummary; children: SessionNode[] };

/// Group sub-agent sessions under their parent when the parent is in the
/// loaded page set; the parent row counts them. Children whose parent is not
/// loaded stay at the top level, their lineage in the title's tooltip; list
/// order (most recently updated first) is preserved at every level.
/// Which sessions show their sub-agents. Selecting a session opens it, one
/// level down, and the rows above it, so it is in sight, once per selection
/// (as soon as it is loaded); anything opened or closed by hand stays so
/// while the list is shown.
export function useOpenSessionRows(tree: SessionNode[], activeId: string | undefined) {
  const [open, setOpen] = useState<Set<string>>(() => new Set());
  const [openedFor, setOpenedFor] = useState<string | undefined | null>(null);
  if (activeId !== openedFor) {
    const path = activeId === undefined ? [] : sessionPath(tree, activeId);
    if (path !== null) {
      setOpenedFor(activeId);
      const ids = activeId === undefined ? path : [...path, activeId];
      if (ids.some((id) => !open.has(id))) setOpen(new Set([...open, ...ids]));
    }
  }
  return {
    isOpen: (id: string) => open.has(id),
    toggle: (id: string) =>
      setOpen((current) => {
        const next = new Set(current);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      }),
  };
}

type OpenSessionRows = ReturnType<typeof useOpenSessionRows>;

/// The ids of the sessions above the one with `id`, outermost first; null
/// when it is not loaded.
function sessionPath(nodes: SessionNode[], id: string, above: string[] = []): string[] | null {
  for (const node of nodes) {
    if (node.session.id === id) return above;
    const found = sessionPath(node.children, id, [...above, node.session.id]);
    if (found) return found;
  }
  return null;
}

export function buildSessionTree(sessions: SessionSummary[]): SessionNode[] {
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
  handle,
  activeId,
  openRows,
  depth,
  last = true,
  lines = [],
  ...controls
}: {
  node: SessionNode;
  handle: string;
  activeId: string | undefined;
  /// Which rows show their sub-agents (a caret after the title opens and closes them).
  openRows: OpenSessionRows;
  depth: number;
  /// The last of its siblings.
  last?: boolean;
  /// Per ancestor level below the root: whether that ancestor has a later sibling.
  lines?: boolean[];
} & SessionRowControls) {
  // What its children pass on: below the root, whether this row's own line runs on.
  const childLines = depth > 0 ? [...lines, !last] : [];
  const id = node.session.id;
  const count = node.children.length;
  const open = count > 0 && openRows.isOpen(id);
  return (
    <>
      <SessionListItem
        session={node.session}
        handle={handle}
        active={id === activeId}
        tree={{ depth, last, lines, children: open }}
        {...(count > 0 ? {
          expander: {
            open,
            onToggle: () => openRows.toggle(id),
            label: `${open ? "Hide" : "Show"} ${count} sub-agent${count === 1 ? "" : "s"}`,
          },
        } : {})}
        subAgentCount={count}
        firstSubAgent={node.children[0] && sessionName(node.children[0].session)}
        {...controls}
      />
      {open && node.children.map((child, index) => (
        <SessionTreeItem
          key={child.session.id}
          node={child}
          handle={handle}
          activeId={activeId}
          openRows={openRows}
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
  handle,
  active,
  tree,
  expander,
  subAgentCount = 0,
  firstSubAgent,
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
  handle: string;
  active: boolean;
  /// Its place in the tree of sessions and their sub-agents.
  tree: ListRowTree;
  /// Opens and closes its sub-agents.
  expander?: ListRowExpander;
  /// Sub-agent sessions listed directly under this row.
  subAgentCount?: number;
  /// The first sub-agent's name, for the count's tooltip.
  firstSubAgent?: string;
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
  const to = `/u/${handle}/sessions/${session.id}${search ? `?${search}` : ""}`;
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
      {...(expander ? { expander } : {})}
      indicator={<SessionDot session={session} />}
      title={displayName || session.id}
      // A closed session's title fades to its second line's colour.
      muted={session.lifecycleStatus === "closed"}
      // A sub-agent row is marked by its indent and ↳; its lineage is the
      // title's tooltip.
      titleHint={origin
        ? `Sub-agent of ${origin.parentSessionId} (depth ${origin.depth}, profile ${origin.agent.profileId} rev ${origin.agent.revision})`
        : displayName ? undefined : session.id}
      marks={sessionMetaItems({
        session,
        subAgents: { count: subAgentCount, first: firstSubAgent },
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

function relativeTime(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h`;
  return `${Math.floor(delta / 86_400_000)}d`;
}
