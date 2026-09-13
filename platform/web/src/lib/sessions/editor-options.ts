import { useCallback } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  api,
  type Environment,
  type McpToolDiscovery,
  type ModelListResponse,
  type ProfileSummary,
} from "@/api";
import type {
  McpServerOption,
  WorkspaceOption,
} from "@/components/session/session-config-editor";

export function useSessionConfigEditorOptions(universeId: string, enabled = true) {
  const servers = useQuery({
    queryKey: ["mcp-servers", universeId],
    queryFn: () => api<McpServerOption[]>("GET", `/api/v1/universes/${universeId}/mcp-servers`),
    enabled,
  });
  const workspaces = useQuery({
    queryKey: ["workspaces", universeId],
    queryFn: () =>
      api<WorkspaceOption[]>("GET", `/api/v1/universes/${universeId}/workspaces`),
    enabled,
  });
  const models = useQuery({
    queryKey: ["models", universeId],
    queryFn: () => api<ModelListResponse>("GET", `/api/v1/universes/${universeId}/models`),
    staleTime: 60_000,
    enabled,
  });
  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
    enabled,
  });
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled,
  });
  const discoverMcpTools = useCallback(async (serverId: string) => {
    const result = await api<McpToolDiscovery>("POST", `/api/v1/universes/${universeId}/mcp-servers/${encodeURIComponent(serverId)}/tools/discover`);
    if (result.status === "failure") throw new Error(result.message);
    return result.tools.map((tool) => tool.name);
  }, [universeId]);
  return {
    mcpServers: servers.data,
    workspaces: workspaces.data,
    workspacesLoading: workspaces.isLoading,
    models: models.data?.models,
    profiles: profiles.data,
    environments: environments.data,
    discoverMcpTools,
  };
}
