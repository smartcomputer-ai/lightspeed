import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/api";
import { usePlatformAdmin } from "@/lib/permissions";
import { Checkbox } from "@/components/ui/checkbox";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog";

/** Mount for one deletion attempt; keep a failed purge retryable after soft deletion. */
export function DeleteSessionDialog({ universeId, sessionId, onCancel, onDeleted }: {
  universeId: string;
  sessionId: string;
  onCancel: () => void;
  onDeleted: () => void | Promise<void>;
}) {
  const platformAdmin = usePlatformAdmin();
  const queryClient = useQueryClient();
  const [cascade, setCascade] = useState(false);
  const [permanent, setPermanent] = useState(false);
  const [softDeleted, setSoftDeleted] = useState(false);
  const deletion = useMutation({
    mutationFn: async () => {
      if (permanent && !platformAdmin) throw new Error("Platform admin access is required for permanent deletion.");
      if (!softDeleted) {
        await api("DELETE", `/api/v1/universes/${encodeURIComponent(universeId)}/sessions/${encodeURIComponent(sessionId)}${cascade ? "?cascade=true" : ""}`);
        setSoftDeleted(true);
      }
      if (permanent) {
        // Use the same server-side authorization and audit path as the admin page.
        await api("POST", `/api/v1/admin/universes/${encodeURIComponent(universeId)}/sessions/${encodeURIComponent(sessionId)}/purge`, {});
        void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
        void queryClient.invalidateQueries({ queryKey: ["admin", "deleted-sessions", universeId] });
      }
    },
    onSuccess: onDeleted,
  });
  const dismiss = () => {
    if (deletion.isPending) return;
    if (softDeleted) void onDeleted();
    else onCancel();
  };
  return <AlertDialog open onOpenChange={(open) => { if (!open) dismiss(); }}>
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogTitle>{permanent ? "Permanently delete this session?" : "Delete this session?"}</AlertDialogTitle>
        <AlertDialogDescription>
          {permanent
            ? "This removes the session and its retained history subtree permanently. This cannot be undone. Attached workspaces and environments are separate resources."
            : "This hides the session and its history from all universe members, including admins. History is retained until a platform admin permanently deletes it."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <label className="flex min-w-0 items-start gap-2 rounded-lg border p-3 text-sm">
        <Checkbox checked={cascade} onCheckedChange={(checked) => setCascade(checked === true)} disabled={deletion.isPending || softDeleted} />
        <span className="min-w-0">
          <span className="block font-medium">Also delete forks and delegated children</span>
          <span className="block text-xs text-muted-foreground">Required if the session has history forks or delegated children. Every selected session must already be closed. Config-only clones are not included.</span>
        </span>
      </label>
      {platformAdmin && <label className="flex min-w-0 items-start gap-2 rounded-lg border p-3 text-sm">
        <Checkbox checked={permanent} onCheckedChange={(checked) => setPermanent(checked === true)} disabled={deletion.isPending || softDeleted} />
        <span className="min-w-0">
          <span className="block font-medium">Also permanently delete retained history</span>
          <span className="block text-xs text-muted-foreground">Platform admins only. Permanent deletion is recorded in the audit log.</span>
        </span>
      </label>}
      {deletion.error && <div role="alert" className="text-sm text-destructive">
        {softDeleted && <p>The session is already soft-deleted. You can retry permanent deletion here or later from Platform → Universes → Deleted sessions.</p>}
        <p>{deletion.error.message}</p>
      </div>}
      <AlertDialogFooter>
        <AlertDialogCancel disabled={deletion.isPending}>{softDeleted ? "Done" : "Cancel"}</AlertDialogCancel>
        <AlertDialogAction className="bg-destructive text-white hover:bg-destructive/90" disabled={deletion.isPending} onClick={() => deletion.mutate()}>
          {deletion.isPending ? "Deleting…" : permanent ? softDeleted ? "Retry permanent deletion" : "Permanently delete" : "Delete session"}
        </AlertDialogAction>
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>;
}
