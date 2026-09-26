import { describe, expect, it } from "vitest";
import { addModelProviderHref, summarizeProviderReadiness } from "./provider-readiness";

const provider = (providerId: string, credential: "configured" | "missing" | "invalid") => ({
  providerId,
  apiKinds: [],
  credential,
  credentialSource: "none" as const,
});

describe("provider readiness", () => {
  it("is ready when any provider has a usable credential", () => {
    const summary = summarizeProviderReadiness([
      provider("openai", "missing"),
      provider("anthropic", "configured"),
    ]);
    expect(summary.ready).toBe(true);
    expect(summary.missing.map((p) => p.providerId)).toEqual(["openai"]);
  });

  it("is not ready when every provider is missing or invalid", () => {
    const summary = summarizeProviderReadiness([
      provider("openai", "missing"),
      provider("anthropic", "invalid"),
    ]);
    expect(summary.ready).toBe(false);
    expect(summary.invalid.map((p) => p.providerId)).toEqual(["anthropic"]);
  });

  it("does not nag while unknown", () => {
    expect(summarizeProviderReadiness(undefined).ready).toBe(true);
  });

  it("encodes reserved characters in add-provider kinds", () => {
    const href = addModelProviderHref("acme", "custom provider/alpha?x=1&y=2#fragment");
    expect(href).toBe(
      "/u/acme/models?add=custom%20provider%2Falpha%3Fx%3D1%26y%3D2%23fragment",
    );
  });

  it("builds the add-provider deep link", () => {
    expect(addModelProviderHref("acme", "openAiApiKey")).toBe(
      "/u/acme/models?add=openAiApiKey",
    );
  });
});
