import { useQuery } from "@tanstack/react-query";
import { api, type ModelConfig, type ModelDefaults, type ModelListResponse } from "@/api";

export const modelDefaultsKey = (universeId: string) => ["model-defaults", universeId] as const;

export function useModelDefaults(universeId: string, enabled = true) {
  return useQuery({
    queryKey: modelDefaultsKey(universeId),
    queryFn: () => api<ModelDefaults>("GET", `/api/v1/universes/${universeId}/models/defaults`),
    enabled,
  });
}

export function useModelDiscovery(universeId: string, enabled = true) {
  return useQuery({
    queryKey: ["models", universeId],
    queryFn: () => api<ModelListResponse>("GET", `/api/v1/universes/${universeId}/models`),
    staleTime: 60_000,
    enabled,
  });
}

export function modelFromConfig(config: unknown): ModelConfig | null {
  if (!config || typeof config !== "object") return null;
  const model = (config as { model?: unknown }).model;
  if (!model || typeof model !== "object") return null;
  const route = model as Partial<ModelConfig>;
  return typeof route.providerId === "string" && typeof route.apiKind === "string" && typeof route.model === "string"
    ? route as ModelConfig : null;
}

export function resolveCreationModel(
  explicit: ModelConfig | null | undefined,
  profile: ModelConfig | null | undefined,
  defaults: ModelDefaults | undefined,
): { model: ModelConfig | null; source: "Session model" | "Profile model" | "Universe default" } {
  if (explicit) return { model: explicit, source: "Session model" };
  if (profile) return { model: profile, source: "Profile model" };
  return { model: defaults?.agentRun ?? null, source: "Universe default" };
}

export function modelLabel(model: ModelConfig) {
  const provider = model.providerId === "openai" ? "OpenAI" : model.providerId === "anthropic" ? "Anthropic" : model.providerId;
  return `${provider} · ${model.model}`;
}
