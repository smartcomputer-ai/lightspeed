import { describe, expect, it } from "vitest";
import {
  CLAUDE_CODE_TOKEN_TTL_MS,
  conflictingAnthropicEnv,
  isSubscriptionGrant,
  parseClaudeCodeToken,
  parseCodexCredential,
  SubscriptionCredentialError,
} from "./subscriptions.js";

function jwt(payload: Record<string, unknown>): string {
  const b64 = (v: string) => Buffer.from(v).toString("base64url");
  return `${b64('{"alg":"none"}')}.${b64(JSON.stringify(payload))}.${b64("sig")}`;
}

describe("Claude Code token", () => {
  it("accepts setup-token output and records a one-year expiry", () => {
    const parsed = parseClaudeCodeToken("  sk-ant-oat01-abc  ", 1_000);
    expect(parsed).toMatchObject({
      providerId: "anthropic",
      secret: "sk-ant-oat01-abc",
      shape: "token",
      metadata: { subscription: "claudeCode", credential: "token" },
      expiresAtMs: 1_000 + CLAUDE_CODE_TOKEN_TTL_MS,
    });
  });

  it("rejects API keys and empty input", () => {
    expect(() => parseClaudeCodeToken("sk-ant-api03-key", 0)).toThrow(SubscriptionCredentialError);
    expect(() => parseClaudeCodeToken("   ", 0)).toThrow(SubscriptionCredentialError);
  });
});

describe("Codex credential", () => {
  it("treats a bare value as an Enterprise access token", () => {
    expect(parseCodexCredential(" codex_pat_x ")).toMatchObject({
      providerId: "openai",
      secret: "codex_pat_x",
      shape: "token",
      metadata: { subscription: "codex", credential: "token" },
    });
    expect(() => parseCodexCredential("two words")).toThrow(SubscriptionCredentialError);
  });

  it("normalises an auth.json token set and extracts account facts", () => {
    const input = JSON.stringify({
      auth_mode: "chatgpt",
      OPENAI_API_KEY: null,
      tokens: {
        id_token: jwt({
          email: "a@b.c",
          "https://api.openai.com/auth": { chatgpt_account_id: "acct", chatgpt_plan_type: "pro" },
        }),
        access_token: jwt({ exp: 2_000_000_000 }),
        refresh_token: "rt",
        account_id: "",
      },
      last_refresh: "2026-01-01T00:00:00Z",
      unrelated: true,
    });
    const parsed = parseCodexCredential(input);
    expect(parsed.shape).toBe("codexTokenSet");
    expect(parsed.metadata).toMatchObject({
      subscription: "codex",
      credential: "tokenSet",
      email: "a@b.c",
      accountId: "acct",
      planType: "pro",
    });
    expect(parsed.expiresAtMs).toBe(2_000_000_000_000);
    expect(parsed.subjectHint).toBe("a@b.c");
    const stored = JSON.parse(parsed.secret) as Record<string, unknown>;
    expect(stored.auth_mode).toBe("chatgpt");
    expect(stored.OPENAI_API_KEY).toBeNull();
    expect(stored.tokens).toMatchObject({ refresh_token: "rt", account_id: "acct" });
    expect(stored).not.toHaveProperty("unrelated");
    expect(typeof stored.last_refresh).toBe("string");
  });

  it("rejects files without tokens or with missing fields", () => {
    expect(() => parseCodexCredential('{"OPENAI_API_KEY":"sk-proj"}')).toThrow(/no ChatGPT tokens/);
    expect(() =>
      parseCodexCredential('{"tokens":{"access_token":"a","refresh_token":"r"}}'),
    ).toThrow(/id_token/);
  });
});

describe("subscription grant recognition and guards", () => {
  it("recognises grants by metadata, not kind", () => {
    expect(isSubscriptionGrant({ providerKind: "staticBearer", metadata: { subscription: "codex" } })).toBe(true);
    expect(isSubscriptionGrant({ providerKind: "staticBearer", metadata: {} })).toBe(false);
    expect(isSubscriptionGrant({ providerKind: "mcpOAuth", metadata: { subscription: "codex" } })).toBe(false);
  });

  it("refuses to pair the Claude Code token with an Anthropic API key", () => {
    expect(conflictingAnthropicEnv("CLAUDE_CODE_OAUTH_TOKEN", ["ANTHROPIC_API_KEY"])).toBe("ANTHROPIC_API_KEY");
    expect(conflictingAnthropicEnv("ANTHROPIC_AUTH_TOKEN", ["CLAUDE_CODE_OAUTH_TOKEN"])).toBe("CLAUDE_CODE_OAUTH_TOKEN");
    expect(conflictingAnthropicEnv("OPENAI_API_KEY", ["CLAUDE_CODE_OAUTH_TOKEN"])).toBeNull();
  });
});
