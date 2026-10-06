import type { DeploymentDeletedSessionsListResponse, DeletedSessionView } from "@lightspeed-ai/sdk";
import { useState } from "react";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { LoadingNote } from "@/components/page";

/** Mounted only in Platform administration, never in the universe browser. */
export function DeletedSessionsDialog({ universe, onClose }: { universe: { id: string; name: string } | null; onClose: () => void }) {
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<DeletedSessionView | null>(null);
  const base = `/api/v1/admin/universes/${encodeURIComponent(universe?.id ?? "")}`;
  const sessions = useInfiniteQuery({
    queryKey: ["admin", "deleted-sessions", universe?.id], enabled: !!universe,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => api<DeploymentDeletedSessionsListResponse>("GET", `${base}/deleted-sessions${pageParam ? `?after=${encodeURIComponent(pageParam)}` : ""}`),
    getNextPageParam: (page) => page.nextAfter ?? undefined,
  });
  const purge = useMutation({
    mutationFn: (sessionId: string) => api("POST", `${base}/sessions/${encodeURIComponent(sessionId)}/purge`, {}),
    onSuccess: () => {
      setSelected(null);
      void queryClient.invalidateQueries({ queryKey: ["admin", "deleted-sessions", universe?.id] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
  });
  const close = () => { if (!purge.isPending) { setSelected(null); purge.reset(); onClose(); } };
  return <Dialog open={!!universe} onOpenChange={(open) => { if (!open) close(); }}>
    <DialogContent>
      <DialogHeader><DialogTitle>Deleted sessions · {universe?.name}</DialogTitle>
        <DialogDescription>These sessions are hidden from everyone in the universe. Platform admins can permanently delete their retained history.</DialogDescription>
      </DialogHeader>
      {sessions.isLoading && <LoadingNote />}
      {sessions.error && <p role="alert">{sessions.error.message}</p>}
      {selected ? <>
        <p>Permanently delete <strong>{selected.displayName ?? selected.sessionId}</strong> and its deleted history forks and delegated children? This cannot be undone.</p>
        <p className="text-sm text-muted-foreground">Attached workspaces, environments and external copies are separate resources.</p>
        {purge.error && <p role="alert" className="text-destructive">{purge.error.message}</p>}
        <DialogFooter>
          <Button variant="outline" disabled={purge.isPending} onClick={() => { setSelected(null); purge.reset(); }}>Back</Button>
          <Button variant="destructive" disabled={purge.isPending} onClick={() => purge.mutate(selected.sessionId)}>{purge.isPending ? "Deleting…" : "Permanently delete"}</Button>
        </DialogFooter>
      </> : <>
        <div className="max-h-96 space-y-3 overflow-auto">
          {sessions.data?.pages.flatMap((page) => page.sessions).map((session) => <div key={session.sessionId} className="flex items-center justify-between gap-3">
            <div className="min-w-0"><div className="truncate">{session.displayName ?? session.sessionId}</div><div className="text-xs text-muted-foreground">{new Date(session.deletedAtMs).toLocaleString()}</div></div>
            <Button variant="outline" size="sm" onClick={() => setSelected(session)}>Permanently delete…</Button>
          </div>)}
          {sessions.data?.pages[0]?.sessions.length === 0 && <p>No deleted sessions.</p>}
        </div>
        {sessions.hasNextPage && <Button variant="outline" disabled={sessions.isFetchingNextPage} onClick={() => void sessions.fetchNextPage()}>Load more</Button>}
      </>}
    </DialogContent>
  </Dialog>;
}
