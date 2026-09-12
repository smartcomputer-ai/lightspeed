import { useQuery } from "@tanstack/react-query";
import {
  api,
  type EnvironmentProviderBinding,
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
  const environmentProviders = useQuery({
    queryKey: ["environment-provider-bindings", universeId],
    queryFn: () =>
      api<EnvironmentProviderBinding[]>(
        "GET",
        `/api/v1/universes/${universeId}/environment-provider-bindings`,
      ),
    enabled,
  });
  return {
    mcpServers: servers.data,
    workspaces: workspaces.data,
    workspacesLoading: workspaces.isLoading,
    models: models.data?.models,
    profiles: profiles.data,
    environmentProviders: environmentProviderOptions(environmentProviders.data ?? []),
  };
}

export function environmentProviderOptions(bindings: EnvironmentProviderBinding[]) {
  return [...new Map(
    bindings
      .filter((binding) => binding.status === "enabled")
      .map((binding) => [binding.providerId, {
        providerId: binding.providerId,
        displayName: binding.metadata?.displayName,
      }]),
  ).values()].sort((left, right) => left.providerId.localeCompare(right.providerId));
}
