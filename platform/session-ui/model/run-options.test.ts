import { describe, expect, it } from "vitest";
import {
  supportsOpenAiProcessingTier,
} from "./run-options";

describe("run processing tier options", () => {
  it("enables processing tiers for both built-in OpenAI API kinds", () => {
    for (const apiKind of ["openai:responses", "openai:completions"]) {
      expect(supportsOpenAiProcessingTier({
        model: { providerId: "openai", apiKind, model: "gpt-5.6-sol" },
      })).toBe(true);
    }
  });

  it("does not offer the OpenAI tier control to compatible custom providers", () => {
    expect(supportsOpenAiProcessingTier({
      model: {
        providerId: "openrouter",
        apiKind: "openai:completions",
        model: "openai/gpt-5.6",
      },
    })).toBe(false);
    expect(supportsOpenAiProcessingTier({
      model: {
        providerId: "deepseek",
        apiKind: "openai:completions",
        model: "deepseek-chat",
      },
    })).toBe(false);
  });
});
