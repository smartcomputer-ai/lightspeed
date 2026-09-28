import { describe, expect, it } from "vitest";
import type { ModelConfig, ModelProviderDiscovery } from "@/api";
import { addModelProviderHref, summarizeProviderReadiness } from "./provider-readiness";
import { resolveCreationModel } from "./model-defaults";

const route: ModelConfig = { providerId: "openai", apiKind: "openai:responses", model: "private-model" };
const provider = (providerId: string, credential: ModelProviderDiscovery["credential"], apiKinds = ["openai:responses"]): ModelProviderDiscovery => ({
  providerId, apiKinds, credential, credentialSource: "none",
});

describe("selected model readiness", () => {
  it("does not use another provider's credential", () => {
    expect(summarizeProviderReadiness(route, [provider("openai", "missing"), provider("anthropic", "configured")]))
      .toMatchObject({ state: "missing", blocked: true });
  });
  it("accepts credentialless and unlisted manual models", () => {
    expect(summarizeProviderReadiness(route, [provider("openai", "notRequired")]))
      .toMatchObject({ state: "configured", blocked: false });
  });
  it("checks the protocol as well as the provider", () => {
    expect(summarizeProviderReadiness(route, [provider("openai", "configured", ["openai:completions"])]).state).toBe("unsupported");
    expect(summarizeProviderReadiness(route, [provider("openai", "invalid")]).state).toBe("invalid");
    expect(summarizeProviderReadiness(route, []).state).toBe("missing");
  });
  it("distinguishes absent configuration from unknown provider health", () => {
    expect(summarizeProviderReadiness(null, undefined)).toMatchObject({ state: "unset", blocked: true });
    expect(summarizeProviderReadiness(undefined, undefined)).toMatchObject({ state: "unknown", blocked: false });
    expect(summarizeProviderReadiness(route, undefined)).toMatchObject({ state: "unknown", blocked: false });
    expect(summarizeProviderReadiness(route, [{ ...provider("openai", "configured"), error: "discovery timed out" }]))
      .toMatchObject({ state: "unknown", blocked: false });
  });
  it("resolves explicit, profile, and universe choices without requiring a default", () => {
    const profile = { ...route, providerId: "profile" };
    const defaults = { revision: 1, agentRun: { ...route, providerId: "default" }, speechToText: null };
    expect(resolveCreationModel(route, profile, defaults)).toEqual({ model: route, source: "Session model" });
    expect(resolveCreationModel(null, profile, undefined)).toEqual({ model: profile, source: "Profile model" });
    expect(resolveCreationModel(null, null, defaults)).toEqual({ model: defaults.agentRun, source: "Universe default" });
    expect(resolveCreationModel(null, null, { ...defaults, agentRun: null }).model).toBeNull();
  });
  it("encodes add-provider deep links", () => {
    expect(addModelProviderHref("acme", "custom provider/alpha?x=1&y=2#fragment"))
      .toBe("/u/acme/models?add=custom%20provider%2Falpha%3Fx%3D1%26y%3D2%23fragment");
  });
});
