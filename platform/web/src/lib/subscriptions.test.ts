import { describe, expect, it } from "vitest";
import {
  CODEX_AUTH_JSON_BOOTSTRAP,
  isCodexTokenSet,
  subscriptionBinding,
  subscriptionProviderOf,
} from "./subscriptions";

describe("subscription grant bindings", () => {
  it("maps Claude Code grants to the OAuth token variable", () => {
    const grant = { providerKind: "staticToken", metadata: { subscription: "claudeCode" } };
    expect(subscriptionProviderOf(grant)).toBe("anthropic");
    expect(subscriptionBinding(grant)).toMatchObject({
      envName: "CLAUDE_CODE_OAUTH_TOKEN",
      authJson: false,
    });
  });

  it("distinguishes Codex token sets from Enterprise access tokens", () => {
    const tokenSet = { providerKind: "staticToken", metadata: { subscription: "codex", credential: "tokenSet" } };
    const accessToken = { providerKind: "staticToken", metadata: { subscription: "codex", credential: "token" } };
    expect(isCodexTokenSet(tokenSet)).toBe(true);
    expect(subscriptionBinding(tokenSet)).toMatchObject({ envName: "CODEX_AUTH_JSON", authJson: true });
    expect(isCodexTokenSet(accessToken)).toBe(false);
    expect(subscriptionBinding(accessToken)).toMatchObject({
      envName: "CODEX_ACCESS_TOKEN",
      authJson: false,
    });
  });

  it("ignores non-subscription grants", () => {
    expect(subscriptionProviderOf({ providerKind: "staticToken", metadata: {} })).toBeNull();
    expect(subscriptionBinding({ providerKind: "gitHubApp", metadata: {} })).toBeNull();
  });

  it("bootstrap writes a 0600 auth.json and clears the variable", () => {
    expect(CODEX_AUTH_JSON_BOOTSTRAP).toContain("chmod 600");
    expect(CODEX_AUTH_JSON_BOOTSTRAP).toContain("unset CODEX_AUTH_JSON");
    expect(CODEX_AUTH_JSON_BOOTSTRAP).toContain('/auth.json"');
  });
});
