import { useActionPermissions } from "@/lib/permissions";
import { useMcpToolDiscoverySource } from "@/lib/mcp/tool-discovery";
import { useQuery } from "@tanstack/react-query";
import {
  api,
  type Environment,
  type ModelListResponse,
  type ProfileSummary,
} from "@/api";
import type {
  McpServerOption,
  WorkspaceOption,
} from "@/components/session/session-config-editor";

/** Attachment choices for a session setup. */
export function useSessionConfigEditorOptions(
  universeId: string,
  enabled = true,
) {
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
  const permissions = useActionPermissions(enabled ? universeId : undefined);
  const mcpToolDiscovery = useMcpToolDiscoverySource(universeId);
  return {
    mcpServers: servers.data,
    workspaces: workspaces.data,
    workspacesLoading: workspaces.isLoading,
    models: models.data?.models,
    profiles: profiles.data,
    environments: environments.data,
    mcpToolDiscovery: permissions.can("configure_resource") ? mcpToolDiscovery : undefined,
  };
}
