import { expect, it } from "vitest";
import type { ModelOption } from "@/api";
import { composerModelChoice, configWithRunChoice, effortLabel, normalizeRunChoice } from "./composer-model";

const option = (providerId: string, apiKind: string, model: string, reasoningEfforts?: string[]): ModelOption => ({
  providerId, apiKind, model, displayName: model.toUpperCase(),
  capabilities: reasoningEfforts ? { reasoningEfforts } : {}, source: "provider", fetchedAtMs: 0,
});

it("lists only the session's pinned route, with the session model first", () => {
  const config = { model: { providerId: "anthropic", apiKind: "anthropic:messages", model: "claude-opus-5" } };
  const choice = composerModelChoice(config, [
    option("anthropic", "anthropic:messages", "claude-sonnet-5"),
    option("openai", "openai:responses", "gpt-5.5"),
  ], {})!;
  expect(choice.models.map((model) => model.model)).toEqual(["claude-opus-5", "claude-sonnet-5"]);
  // Without discovered tiers, the API's accepted tiers are offered.
  expect(choice.efforts).toEqual(["none", "low", "medium", "high", "xhigh", "max"]);
  expect(choice.options).toBeUndefined();
});

it("sends only the fields that differ from the session and leaves other generation settings alone", () => {
  const config = {
    model: { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.5" },
    generation: { reasoningEffort: "high", processingTier: "flex" },
  };
  const choice = composerModelChoice(config, [option("openai", "openai:responses", "gpt-5.5", ["low", "high"])], {
    model: "gpt-5.5", reasoningEffort: "low",
  })!;
  expect(choice.options).toEqual({ reasoningEffort: "low" });
  expect(choice.efforts).toEqual(["low", "high"]);
  expect(normalizeRunChoice({ model: "gpt-5.5", reasoningEffort: "low" }, choice.session))
    .toEqual({ reasoningEffort: "low" });
  // The session's processing tier is kept when the choice is saved.
  expect(configWithRunChoice(config, choice)).toEqual({
    model: config.model,
    generation: { reasoningEffort: "low", processingTier: "flex" },
  });
});

it("keeps a custom model the provider did not list", () => {
  const config = { model: { providerId: "local", apiKind: "openai:completions", model: "qwen" } };
  const choice = composerModelChoice(config, [], { model: "qwen-large" })!;
  expect(choice.models.map((model) => model.model)).toEqual(["qwen", "qwen-large"]);
  expect(choice.options).toEqual({ model: { providerId: "local", apiKind: "openai:completions", model: "qwen-large" } });
  expect(composerModelChoice({}, [], {})).toBeNull();
  expect(effortLabel(undefined)).toBe("Default");
  expect(effortLabel("xhigh")).toBe("Extra high");
});
