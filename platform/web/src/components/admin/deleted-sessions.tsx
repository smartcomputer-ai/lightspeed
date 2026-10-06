import type { DeploymentDeletedSessionsListResponse, DeploymentSessionPurgeResponse } from "@lightspeed-ai/sdk";
import { useState } from "react";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { LoadingNote } from "@/components/page";

type Universe = { id: string; name: string };
type Selection = { ids: string[]; all: boolean };

/** Mounted only in Platform administration, never in the universe browser. */
export function DeletedSessionsDialog({ universe, onClose }: { universe: Universe | null; onClose: () => void }) {
  return universe ? <DeletedSessionsContent key={universe.id} universe={universe} onClose={onClose} /> : null;
}

function DeletedSessionsContent({ universe, onClose }: { universe: Universe; onClose: () => void }) {
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [confirmation, setConfirmation] = useState<Selection | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const base = `/api/v1/admin/universes/${encodeURIComponent(universe.id)}`;
  const readPage = (after?: string) => api<DeploymentDeletedSessionsListResponse>("GET", `${base}/deleted-sessions${after ? `?after=${encodeURIComponent(after)}` : ""}`);
  const sessions = useInfiniteQuery({
    queryKey: ["admin", "deleted-sessions", universe.id],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => readPage(pageParam),
    getNextPageParam: (page) => page.nextAfter ?? undefined,
  });
  const rows = sessions.data?.pages.flatMap((page) => page.sessions) ?? [];
  const selectedIds = rows.filter((session) => selected.has(session.sessionId)).map((session) => session.sessionId);
  const allLoadedSelected = rows.length > 0 && rows.every((session) => selected.has(session.sessionId));
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["admin", "deleted-sessions", universe.id] });
    void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
  };
  const prepareAll = useMutation({
    mutationFn: async () => {
      // Capture all pages before confirmation. Later arrivals are not added to
      // the confirmed selection while permanent deletion is in progress.
      const ids = new Set<string>();
      let after: string | undefined;
      do {
        const page = await readPage(after);
        for (const session of page.sessions) ids.add(session.sessionId);
        after = page.nextAfter ?? undefined;
      } while (after);
      return [...ids];
    },
    onSuccess: (ids) => {
      if (ids.length) { setNotice(null); setConfirmation({ ids, all: true }); }
      else { setNotice("No deleted sessions remain."); refresh(); }
    },
  });
  const purge = useMutation({
    mutationFn: async (ids: string[]) => {
      const removed = new Set<string>();
      const failures = new Map<string, string>();
      for (const id of ids) {
        if (removed.has(id)) continue; // A previously purged parent included it.
        try {
          const result = await api<DeploymentSessionPurgeResponse>("POST", `${base}/sessions/${encodeURIComponent(id)}/purge`, {});
          for (const removedId of result.deletedSessionIds) removed.add(removedId);
        } catch (error) {
          failures.set(id, error instanceof Error ? error.message : String(error));
        }
      }
      // A later parent's purge may have removed an earlier failed child.
      for (const id of removed) failures.delete(id);
      return { removed: removed.size, failures: [...failures].map(([id, error]) => ({ id, error })) };
    },
    onSuccess: ({ removed, failures }) => {
      setNotice(`Permanently deleted ${removed} ${removed === 1 ? "session" : "sessions"}.${failures.length ? ` ${failures.length} failed; retry the remaining selection.` : ""}`);
      setSelected(new Set(failures.map(({ id }) => id)));
      setConfirmation(failures.length ? { ids: failures.map(({ id }) => id), all: false } : null);
      refresh();
    },
  });
  const busy = purge.isPending || prepareAll.isPending;
  const reviewSelected = () => {
    prepareAll.reset(); purge.reset(); setNotice(null);
    setConfirmation({ ids: selectedIds, all: false });
  };
  return <Dialog open onOpenChange={(open) => { if (!open && !busy) onClose(); }}>
    <DialogContent>
      <DialogHeader><DialogTitle>Deleted sessions · {universe.name}</DialogTitle>
        <DialogDescription>These sessions are hidden from everyone in the universe. Select sessions to purge, or purge all deleted sessions in this universe.</DialogDescription>
      </DialogHeader>
      {notice && <p role="status" className="text-sm">{notice}</p>}
      {confirmation ? <>
        <p>Permanently delete {confirmation.all ? "all " : ""}<strong>{confirmation.ids.length} {confirmation.ids.length === 1 ? "selected session" : "selected sessions"}</strong> and their deleted history forks and delegated children? This cannot be undone.</p>
        <p className="text-sm text-muted-foreground">Attached workspaces, environments and external copies are separate resources. Each permanent deletion is recorded in the audit log.</p>
        {!!purge.data?.failures.length && <div role="alert" className="max-h-32 overflow-auto text-sm text-destructive">
          {purge.data.failures.map(({ id, error }) => <p key={id}>{id}: {error}</p>)}
        </div>}
        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={() => { setConfirmation(null); purge.reset(); }}>Back</Button>
          <Button variant="destructive" disabled={busy} onClick={() => purge.mutate(confirmation.ids)}>{purge.isPending ? "Purging…" : purge.data?.failures.length ? "Retry failed" : confirmation.all ? "Purge all" : "Purge selected"}</Button>
        </DialogFooter>
      </> : <>
        {sessions.isLoading && <LoadingNote />}
        {sessions.error && <p role="alert">{sessions.error.message}</p>}
        {prepareAll.error && <p role="alert" className="text-destructive">{prepareAll.error.message}</p>}
        {rows.length > 0 && <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={allLoadedSelected} indeterminate={!allLoadedSelected && selectedIds.length > 0} disabled={busy} onCheckedChange={(checked) => setSelected(checked === true ? new Set(rows.map((session) => session.sessionId)) : new Set())} />
          Select loaded sessions <span className="ml-auto text-muted-foreground">{selectedIds.length} selected</span>
        </label>}
        <div className="max-h-96 space-y-3 overflow-auto">
          {rows.map((session) => <label key={session.sessionId} className="flex items-center gap-3 rounded-lg border p-3">
            <Checkbox aria-label={`Select session ${session.sessionId}`} checked={selected.has(session.sessionId)} disabled={busy} onCheckedChange={(checked) => setSelected((current) => {
              const next = new Set(current);
              if (checked === true) next.add(session.sessionId); else next.delete(session.sessionId);
              return next;
            })} />
            <div className="min-w-0"><div className="truncate">{session.displayName ?? session.sessionId}</div><div className="text-xs text-muted-foreground">{new Date(session.deletedAtMs).toLocaleString()}</div></div>
          </label>)}
          {sessions.data?.pages[0]?.sessions.length === 0 && <p>No deleted sessions.</p>}
        </div>
        {sessions.hasNextPage && <Button variant="outline" disabled={busy || sessions.isFetchingNextPage} onClick={() => void sessions.fetchNextPage()}>Load more</Button>}
        <DialogFooter>
          <Button variant="outline" disabled={busy || rows.length === 0} onClick={() => { purge.reset(); prepareAll.mutate(); }}>{prepareAll.isPending ? "Collecting sessions…" : "Purge all…"}</Button>
          <Button variant="destructive" disabled={busy || selectedIds.length === 0} onClick={reviewSelected}>Purge selected…</Button>
        </DialogFooter>
      </>}
    </DialogContent>
  </Dialog>;
}
