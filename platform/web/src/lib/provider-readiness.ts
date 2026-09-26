import { useQuery } from "@tanstack/react-query";
import { api, type ModelListResponse, type ModelProviderDiscovery } from "@/api";

/// Whether this universe can run sessions at all: at least one model provider
/// with a usable credential (universe key or deployment fallback). Shares the
/// `["models", universeId]` query with the session editor so it is fetched
/// once and invalidated when keys change.
export interface ProviderReadiness {
  isLoading: boolean;
  /// True while unknown (loading/error) so callers never nag prematurely.
  ready: boolean;
  missing: ModelProviderDiscovery[];
  invalid: ModelProviderDiscovery[];
}

export function summarizeProviderReadiness(
  providers: ModelProviderDiscovery[] | undefined,
): Pick<ProviderReadiness, "ready" | "missing" | "invalid"> {
  if (!providers) return { ready: true, missing: [], invalid: [] };
  return {
    ready: providers.some((provider) => provider.credential === "configured"),
    missing: providers.filter((provider) => provider.credential === "missing"),
    invalid: providers.filter((provider) => provider.credential === "invalid"),
  };
}

export function useProviderReadiness(universeId: string, enabled = true): ProviderReadiness {
  const models = useQuery({
    queryKey: ["models", universeId],
    queryFn: () => api<ModelListResponse>("GET", `/api/v1/universes/${universeId}/models`),
    staleTime: 60_000,
    enabled,
  });
  const summary = summarizeProviderReadiness(models.error ? undefined : models.data?.providers);
  return { isLoading: models.isLoading, ...summary };
}

/// Deep link that opens the Add-model-provider dialog on the given catalog entry.
export function addModelProviderHref(slug: string, kind: string): string {
  return `/u/${slug}/models?add=${encodeURIComponent(kind)}`;
}
