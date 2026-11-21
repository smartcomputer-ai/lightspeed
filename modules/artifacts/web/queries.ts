import { api } from "@lightspeed-ai/platform-web/modules/client";
import { queryOptions } from "@tanstack/react-query";
import type { Artifact, ArtifactContent, ArtifactList } from "../contract";

export const artifactsKey = (universe: string) => ["artifacts", universe] as const;
export const artifactsApi = (universe: string) => `/api/v1/universes/${encodeURIComponent(universe)}/modules/artifacts`;

export function publications(universe: string, workspace: string | null) {
  const query = workspace ? `?${new URLSearchParams({ workspaceId: workspace })}` : "";
  return queryOptions({
    queryKey: [...artifactsKey(universe), "list", workspace],
    queryFn: ({ signal }) => api<ArtifactList>("GET", artifactsApi(universe) + query, undefined, signal),
    staleTime: 0,
  });
}

export function publication(universe: string, id: string) {
  return queryOptions({
    queryKey: [...artifactsKey(universe), "record", id],
    queryFn: ({ signal }) => api<{ artifact: Artifact }>("GET", `${artifactsApi(universe)}/${encodeURIComponent(id)}`, undefined, signal),
    staleTime: 0,
    gcTime: 0,
  });
}

export function publicationContent(universe: string, id: string) {
  return queryOptions({
    queryKey: [...artifactsKey(universe), "content", id],
    queryFn: ({ signal }) => api<ArtifactContent>("GET", `${artifactsApi(universe)}/${encodeURIComponent(id)}/content`, undefined, signal),
    staleTime: 0,
    gcTime: 0,
  });
}
