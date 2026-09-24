import { useActionPermissions } from "@/lib/permissions";
import { useMcpToolDiscoverySource } from "@/lib/mcp/tool-discovery";
import { useQuery } from "@tanstack/react-query";
import type { ExecutionKind, ResourceRef } from "@lightspeed-ai/agent-client";
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

/// Why the identity a session runs as cannot use a listed resource.
export function unusableReason(execution: ExecutionKind): string {
  return execution === "service"
    ? "Default agent identity cannot use this"
    : "You cannot use this";
}

/**
 * Attachment choices for a session setup, each marked with whether the
 * identity the session runs as may use it. Without an identity (a profile,
 * which runs nothing itself) nothing is marked: the runtime checks the
 * attachments when a profile is applied to a session.
 */
export function useSessionConfigEditorOptions(
  universeId: string,
  enabled = true,
  execution?: ExecutionKind,
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
  const resources: ResourceRef[] = [
    ...(servers.data ?? []).map((server): ResourceRef => ({ kind: "mcp_server", id: server.serverId })),
    ...(workspaces.data ?? []).map((workspace): ResourceRef => ({ kind: "workspace", id: workspace.workspaceId })),
    ...(environments.data ?? []).map((environment): ResourceRef => ({ kind: "environment", id: environment.environmentId })),
  ];
  // Universe-level actions stay the caller's under either identity.
  const permissions = useActionPermissions(enabled ? universeId : undefined, resources, {
    as: execution === "service" ? "execution_service" : "caller",
  });
  // Hints only: nothing is marked until the decisions have loaded.
  const mark = <T extends object>(
    items: T[] | undefined,
    ref: (item: T) => ResourceRef,
  ): (T & { unusable?: string })[] | undefined =>
    items?.map((item) =>
      execution && permissions.data && !permissions.can("use_resource", ref(item))
        ? { ...item, unusable: unusableReason(execution) }
        : item,
    );
  const mcpToolDiscovery = useMcpToolDiscoverySource(universeId);
  return {
    mcpServers: mark(servers.data, (server) => ({ kind: "mcp_server", id: server.serverId })),
    workspaces: mark(workspaces.data, (workspace) => ({ kind: "workspace", id: workspace.workspaceId })),
    workspacesLoading: workspaces.isLoading,
    models: models.data?.models,
    profiles: profiles.data,
    environments: mark(environments.data, (environment) => ({ kind: "environment", id: environment.environmentId })),
    mcpToolDiscovery: permissions.can("configure_resource") ? mcpToolDiscovery : undefined,
  };
}
