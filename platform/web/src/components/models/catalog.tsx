import type { ComponentType, SVGProps } from "react";
import { Sparkles } from "lucide-react";
import { AnthropicLogo, OpenAiLogo } from "@/components/icons/logos";

/// Every kind of model provider a universe can add on the Models page.
/// Adding one = an entry here plus its form/details components; the page
/// itself stays generic.
export type ModelProviderKind =
  | "openAiApiKey"
  | "openAiCompatible"
  | "anthropicApiKey"
  | "anthropicSubscription"
  | "openAiSubscription";

export interface ModelProviderDefinition {
  kind: ModelProviderKind;
  name: string;
  /// One line for the picker card.
  tagline: string;
  Logo: ComponentType<SVGProps<SVGSVGElement> & { size?: number }>;
  /// Whether more than one instance can be connected per universe.
  multiple: boolean;
}

export const MODEL_PROVIDER_CATALOG: ModelProviderDefinition[] = [
  {
    kind: "openAiApiKey",
    name: "OpenAI (API key)",
    tagline: "API key that Lightspeed sessions use for OpenAI models — discovery and inference.",
    Logo: OpenAiLogo,
    multiple: false,
  },
  {
    kind: "anthropicApiKey",
    name: "Anthropic (API key)",
    tagline: "API key that Lightspeed sessions use for Anthropic models — discovery and inference.",
    Logo: AnthropicLogo,
    multiple: false,
  },
  {
    kind: "openAiCompatible",
    name: "OpenAI-compatible provider",
    tagline: "Connect OpenRouter, vLLM, Ollama, or another compatible endpoint.",
    Logo: Sparkles,
    multiple: true,
  },
  {
    kind: "anthropicSubscription",
    name: "Claude Code (subscription)",
    tagline:
      "Lets the Claude Code agent run inside environments on your Claude Pro/Max/Team plan. Not used by Lightspeed's own sessions.",
    Logo: AnthropicLogo,
    multiple: true,
  },
  {
    kind: "openAiSubscription",
    name: "Codex (ChatGPT subscription)",
    tagline:
      "Lets the Codex agent run inside environments on your ChatGPT Plus/Pro/Team/Enterprise plan. Not used by Lightspeed's own sessions.",
    Logo: OpenAiLogo,
    multiple: true,
  },
];

export function modelProviderDefinition(kind: ModelProviderKind): ModelProviderDefinition {
  const found = MODEL_PROVIDER_CATALOG.find((entry) => entry.kind === kind);
  if (!found) throw new Error(`unknown model provider kind ${kind}`);
  return found;
}
