export function supportsOpenAiProcessingTier(config: unknown): boolean {
  if (!config || typeof config !== "object") return false;
  const model = (config as { model?: unknown }).model;
  if (!model || typeof model !== "object") return false;
  const route = model as { providerId?: unknown; apiKind?: unknown };
  return route.providerId === "openai" &&
    (route.apiKind === "openai:responses" || route.apiKind === "openai:completions");
}
