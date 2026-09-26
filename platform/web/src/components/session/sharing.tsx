import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Share2 } from "lucide-react";
import type { ResourceAccessSummary } from "@lightspeed-ai/agent-client";
import { api } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
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
import { usePermissionIdentity, useUniverseRole } from "@/lib/permissions";

/** Marks work only its creator and admins see. */
export function UnsharedBadge({ access }: { access: ResourceAccessSummary | undefined }) {
  if (access?.visibility !== "restricted") return null;
  return (
    <Badge variant="outline" className="shrink-0 text-muted-foreground" title="Only its creator and admins see this">
      Unshared
    </Badge>
  );
}

/**
 * Whether the signed-in user created this work or administers the universe:
 * who may share or delete a session. The server decides again.
 */
export function useSessionOwner(universeId: string, access: ResourceAccessSummary | undefined): boolean {
  const userId = usePermissionIdentity();
  const role = useUniverseRole(universeId);
  if (role === "admin") return true;
  return access?.createdBy?.kind === "actor" && access.createdBy.id === userId;
}

/**
 * The one sharing action: an unshared root session is shared with the
 * universe, once and for good. A sub-agent's session follows its root.
 */
export function ShareSessionButton({
  universeId,
  sessionId,
  access,
  delegated,
}: {
  universeId: string;
  sessionId: string;
  access: ResourceAccessSummary | undefined;
  delegated: boolean;
}) {
  const queryClient = useQueryClient();
  const owner = useSessionOwner(universeId, access);
  const share = useMutation({
    mutationFn: () => api<{ access: ResourceAccessSummary }>("POST", `/api/v1/universes/${universeId}/sessions/${sessionId}/share`),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["session", universeId, sessionId] }),
        queryClient.invalidateQueries({ queryKey: ["sessions", universeId] }),
      ]);
    },
  });
  if (!access) return null;
  if (access.visibility === "universe") {
    return <span className="hidden shrink-0 text-xs text-muted-foreground lg:inline">Shared with universe</span>;
  }
  if (delegated || !owner) return null;
  return (
    <AlertDialog>
      <AlertDialogTrigger render={<Button variant="outline" size="xs" disabled={share.isPending} />}>
        <Share2 data-icon="inline-start" /> Share with universe
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Share with the universe?</AlertDialogTitle>
          <AlertDialogDescription>
            Every member will see this session and its sub-agents, and continue it according to their
            role. Sharing cannot be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        {share.error && <p className="text-sm text-destructive">{share.error.message}</p>}
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={() => share.mutate()}>Share</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
