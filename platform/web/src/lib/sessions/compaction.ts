import { useMutation, useMutationState, useQueryClient } from "@tanstack/react-query";
import { api, type SessionView } from "@/api";

/** Share request state between a bot's menu and its embedded session. */
export function useSessionCompaction(universeId: string, sessionId: string, session?: SessionView) {
  const queryClient = useQueryClient();
  const mutationKey = ["session-compaction", universeId, sessionId];
  const requests = useMutationState({
    filters: { mutationKey, exact: true },
    select: (mutation) => ({ status: mutation.state.status, error: mutation.state.error }),
  });
  const request = requests.at(-1);
  const mutation = useMutation({
    mutationKey,
    mutationFn: () => api("POST", `/api/v1/universes/${universeId}/sessions/${encodeURIComponent(sessionId)}/context/compact`),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["session", universeId, sessionId] }),
  });
  const state = session?.activeContext?.compaction;
  const label = state?.pending ? "Compacting context…"
    : state?.queued ? "Context compaction queued…"
    : request?.status === "pending" ? "Requesting compaction…" : null;
  return {
    label,
    error: request?.status === "error" ? request.error?.message : null,
    compact: () => { if (!label) mutation.mutate(); },
  };
}
