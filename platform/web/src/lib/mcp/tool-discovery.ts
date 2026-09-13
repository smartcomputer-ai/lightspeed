import { useEffect, useMemo, useState } from "react";
import { api, type McpToolDiscovery } from "@/api";

export type McpToolDiscoverySource = {
  universeId: string;
  discover: (serverId: string) => Promise<McpToolDiscovery>;
};

export function useMcpToolDiscoverySource(
  universeId: string,
): McpToolDiscoverySource {
  return useMemo(
    () => ({
      universeId,
      discover: (serverId: string) =>
        api<McpToolDiscovery>(
          "POST",
          `/api/v1/universes/${universeId}/mcp-servers/${encodeURIComponent(serverId)}/tools/discover`,
        ),
    }),
    [universeId],
  );
}

type Observation = {
  source: McpToolDiscoverySource;
  serverId: string;
  revision?: number;
  refresh: number;
  result?: McpToolDiscovery;
  error?: string;
};

/** One temporary live observation. Connection changes discard it; responses never change selections. */
export function useMcpToolDiscovery({
  source,
  serverId,
  revision,
  enabled,
  disabled = false,
}: {
  source?: McpToolDiscoverySource;
  serverId: string;
  revision?: number;
  enabled: boolean;
  disabled?: boolean;
}) {
  const [refresh, setRefresh] = useState(0);
  const [observation, setObservation] = useState<Observation>();
  const canLoad = Boolean(source && serverId && enabled && !disabled);
  useEffect(() => {
    let cancelled = false;
    setObservation(undefined);
    if (!source || !serverId || !enabled || disabled) return;
    const identity = { source, serverId, revision, refresh };
    void source.discover(serverId).then(
      (result) => {
        if (!cancelled) setObservation({ ...identity, result });
      },
      (error: unknown) => {
        if (!cancelled)
          setObservation({
            ...identity,
            error:
              error instanceof Error ? error.message : "Unable to load tools.",
          });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [source, serverId, revision, enabled, disabled, refresh]);
  const current =
    canLoad &&
    observation?.source === source &&
    observation?.serverId === serverId &&
    observation?.revision === revision &&
    observation?.refresh === refresh
      ? observation
      : undefined;
  return {
    result: current?.result,
    error: current?.error,
    loading: canLoad && !current,
    refresh: () => setRefresh((value) => value + 1),
  };
}

export function mcpDiscoveryFailureAction(
  code: Extract<McpToolDiscovery, { status: "failure" }>["code"],
): string {
  switch (code) {
    case "credentialAbsent":
      return "Connect a credential to this server, then try again.";
    case "grantNeedsReauth":
    case "unauthorized":
      return "Reconnect this server to refresh its access.";
    case "grantAudienceMismatch":
      return "Use a credential issued for this exact server address.";
    case "forbidden":
      return "Check the account's scopes and workspace or administrator policy.";
    case "additionalConsentRequired":
      return "Reconnect this server and explicitly approve the additional scopes.";
    case "remoteRateLimited":
      return "Wait briefly before refreshing again.";
    case "unreachable":
      return "Check the server address, network reachability, and TLS setup.";
    case "unsupportedProtocol":
    case "invalidResponse":
      return "Check that this address is a current Streamable HTTP MCP endpoint.";
    case "paginationLimit":
    case "responseTooLarge":
      return "The server's inventory exceeded safe discovery limits; narrow or fix the server response.";
    case "remoteFailure":
      return "Check the server or provider status, then retry.";
  }
}
