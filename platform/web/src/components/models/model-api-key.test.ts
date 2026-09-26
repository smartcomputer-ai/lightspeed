import { describe, expect, it } from "vitest";
import type { ModelOption } from "@/api";
import { providerModelCatalog } from "./model-api-key";

const model = (
  providerId: string,
  name: string,
  apiKind = "openai:completions",
  displayName = name,
): ModelOption => ({
  providerId,
  apiKind,
  model: name,
  displayName,
  capabilities: {},
  source: "provider",
  fetchedAtMs: 1,
});

describe("provider model catalog", () => {
  it("shows only the selected provider and sorts by display name", () => {
    expect(
      providerModelCatalog(
        [
          model("openrouter", "z-model", "openai:completions", "Zulu"),
          model(
            "deepseek",
            "deepseek-chat",
            "openai:completions",
            "DeepSeek Chat",
          ),
          model("openrouter", "a-model", "openai:completions", "Alpha"),
        ],
        "openrouter",
        "",
      ).map((entry) => entry.model),
    ).toEqual(["a-model", "z-model"]);
  });

  it("collapses API-kind variants of the same provider model", () => {
    expect(
      providerModelCatalog(
        [
          model("openrouter", "openai/gpt-5.5"),
          model("openrouter", "openai/gpt-5.5", "openai:responses"),
          model("openrouter", "openai/gpt-5.5"),
        ],
        "openrouter",
        "",
      ),
    ).toEqual([
      {
        model: "openai/gpt-5.5",
        displayName: "openai/gpt-5.5",
        apiKinds: ["openai:completions", "openai:responses"],
      },
    ]);
  });

  it("searches model IDs and display names case-insensitively", () => {
    const models = [
      model(
        "openrouter",
        "deepseek/deepseek-v4",
        "openai:completions",
        "DeepSeek V4",
      ),
      model(
        "openrouter",
        "anthropic/claude-opus",
        "openai:completions",
        "Claude Opus",
      ),
    ];
    expect(providerModelCatalog(models, "openrouter", "DEEPSEEK")).toHaveLength(
      1,
    );
    expect(providerModelCatalog(models, "openrouter", "opus")[0]?.model).toBe(
      "anthropic/claude-opus",
    );
    expect(providerModelCatalog(models, "openrouter", "missing")).toEqual([]);
  });

  it("orders provider models by their reported creation date", () => {
    const older = {
      ...model("openai", "gpt-5.4"),
      createdAtMs: 1_700_000_000_000,
    };
    const newer = {
      ...model("openai", "gpt-5.6-sol"),
      createdAtMs: 1_800_000_000_000,
    };
    expect(
      providerModelCatalog([older, newer], "openai", "").map(
        (entry) => entry.model,
      ),
    ).toEqual(["gpt-5.6-sol", "gpt-5.4"]);
  });
});
