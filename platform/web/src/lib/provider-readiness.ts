import { AGENT_MODEL_API_KINDS } from "@lightspeed/platform-shared";
import type { ModelConfig, ModelProviderDiscovery } from "@/api";
import { useModelDefaults, useModelDiscovery } from "./model-defaults";

export type ModelReadiness = {
  state: "unset" | "configured" | "missing" | "invalid" | "unsupported" | "unknown";
  blocked: boolean;
  message: string;
};

/// Configuration follows the exact provider/API route. Discovery is advisory:
/// an unlisted model or a network failure does not invalidate a manual choice.
export function summarizeProviderReadiness(
  model: ModelConfig | null | undefined,
  providers: ModelProviderDiscovery[] | undefined,
  purpose: "agentRun" | "speechToText" = "agentRun",
): ModelReadiness {
  if (model === undefined) return { state: "unknown", blocked: false, message: "Model settings could not be checked." };
  if (model === null) return { state: "unset", blocked: true, message: "No model selected. Choose a model or set the universe default." };
  const supported = purpose === "speechToText" ? model.apiKind === "openai:audio-transcriptions" : (AGENT_MODEL_API_KINDS as readonly string[]).includes(model.apiKind);
  if (!model.providerId || !model.model || !supported) {
    return { state: "unsupported", blocked: true, message: `This model selection cannot be used for ${purpose === "agentRun" ? "agent runs" : "speech-to-text"}.` };
  }
  if (!providers) return { state: "unknown", blocked: false, message: "Provider status could not be checked." };
  const provider = providers.find((entry) => entry.providerId === model.providerId);
  if (!provider) return { state: "missing", blocked: true, message: `Provider ${model.providerId} is not configured.` };
  if (provider.credential === "invalid") return { state: "invalid", blocked: true, message: `Provider ${model.providerId} is disabled or its credential is unusable.` };
  if (provider.credential === "missing") return { state: "missing", blocked: true, message: `Provider ${model.providerId} needs a credential.` };
  if (!provider.apiKinds.includes(model.apiKind)) {
    return { state: "unsupported", blocked: true, message: `Provider ${model.providerId} does not support ${model.apiKind}.` };
  }
  if (provider.error) return { state: "unknown", blocked: false, message: "Provider configured; availability could not be checked." };
  return { state: "configured", blocked: false, message: provider.credential === "notRequired" ? "Provider configured · no credential required" : "Provider configured" };
}

export function useProviderReadiness(universeId: string, model?: ModelConfig | null, enabled = true) {
  const defaults = useModelDefaults(universeId, enabled && model === undefined);
  const discovery = useModelDiscovery(universeId, enabled);
  const selection = model === undefined ? defaults.data?.agentRun : model;
  const readiness = summarizeProviderReadiness(selection, discovery.error ? undefined : discovery.data?.providers);
  return {
    ...readiness,
    isLoading: enabled && (discovery.isLoading || (model === undefined && defaults.isLoading)),
  };
}

export function addModelProviderHref(slug: string, kind: string): string {
  return `/u/${slug}/models?add=${encodeURIComponent(kind)}`;
}
