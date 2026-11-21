import { skipToken, useQuery, type QueryClient } from "@tanstack/react-query";
import { api, type WorkspaceRow, type WorkspaceTree } from "../api";
import { useWatchConnected } from "../lib/universe-watch";
import { selectedFiles, uploadPayload, workspaceBaseUrl } from "../lib/workspace-transfers";

export type { VfsFileEntry, VfsTreeEntry, WorkspaceTree } from "../api";
export { blobHref } from "../lib/blob-view";

export function useWorkspaceList(universeId: string) {
  const connected = useWatchConnected(universeId);
  return useQuery({
    queryKey: ["workspaces", universeId],
    queryFn: ({ signal }) => api<WorkspaceRow[]>("GET", `/api/ui/universes/${encodeURIComponent(universeId)}/workspaces`, undefined, signal),
    refetchInterval: connected ? 60_000 : 10_000,
  });
}

export function useWorkspaceTree(universeId: string, workspaceId?: string) {
  const connected = useWatchConnected(universeId);
  return useQuery({
    queryKey: ["workspace-tree", universeId, workspaceId],
    queryFn: workspaceId ? ({ signal }) => api<WorkspaceTree>("GET", `${workspaceBaseUrl(universeId, workspaceId)}/tree`, undefined, signal) : skipToken,
    refetchInterval: connected ? 60_000 : 10_000,
  });
}

/** Add files to the selected snapshot. Concurrent edits and name collisions surface for review. */
export async function addWorkspaceFiles(universeId: string, snapshot: WorkspaceTree, folder: string, files: File[]) {
  const { workspaceId, revision } = snapshot.workspace;
  const payload = await uploadPayload(selectedFiles(files), folder, revision, false, () => {}, snapshot.uploadLimits);
  await api("POST", `${workspaceBaseUrl(universeId, workspaceId)}/upload`, payload);
}

export async function refreshWorkspace(client: QueryClient, universeId: string, workspaceId: string | undefined) {
  await Promise.all([
    client.invalidateQueries({ queryKey: ["workspaces", universeId] }),
    client.invalidateQueries({ queryKey: ["workspace-tree", universeId, workspaceId] }),
    client.invalidateQueries({ queryKey: ["workspace-file", universeId, workspaceId] }),
  ]);
}
