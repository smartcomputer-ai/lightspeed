import type { SecretGrant } from "@/api";

/// Coding-agent subscription grants and how they bind into
/// environments. Kept free of React so pages and tests can share it.

export type SubscriptionProvider = "anthropic" | "openAi";

type GrantLike = Pick<SecretGrant, "providerKind" | "metadata">;

/// Subscription credentials are ordinary bearer grants tagged by Platform at
/// import time: `metadata.subscription = "claudeCode" | "codex"`. Core knows
/// nothing vendor-specific about them.
export function subscriptionProviderOf(grant: GrantLike): SubscriptionProvider | null {
  if (grant.providerKind !== "staticToken") return null;
  if (grant.metadata?.subscription === "claudeCode") return "anthropic";
  if (grant.metadata?.subscription === "codex") return "openAi";
  return null;
}

export function isSubscriptionGrant(grant: GrantLike): boolean {
  return subscriptionProviderOf(grant) !== null;
}

/// True for a Codex grant that holds a full ChatGPT token set (normalised
/// auth.json) rather than a single Enterprise access token.
export function isCodexTokenSet(grant: GrantLike): boolean {
  return subscriptionProviderOf(grant) === "openAi" && grant.metadata?.credential === "tokenSet";
}

export interface SubscriptionBinding {
  /// Suggested environment variable name.
  envName: string;
  /// Human label for the credential source option.
  label: string;
  /// True when the injected value is Codex `auth.json` content (a token
  /// set), which the environment writes to `$CODEX_HOME/auth.json`.
  authJson: boolean;
}

/// How a subscription grant is expected to be bound into an environment.
export function subscriptionBinding(grant: GrantLike): SubscriptionBinding | null {
  const provider = subscriptionProviderOf(grant);
  if (provider === "anthropic") {
    return { envName: "CLAUDE_CODE_OAUTH_TOKEN", label: "Claude Code subscription", authJson: false };
  }
  if (provider === "openAi") {
    return isCodexTokenSet(grant)
      ? { envName: "CODEX_AUTH_JSON", label: "Codex auth.json (ChatGPT subscription)", authJson: true }
      : { envName: "CODEX_ACCESS_TOKEN", label: "Codex access token (ChatGPT Enterprise)", authJson: false };
  }
  return null;
}

/// Shell snippet the environment runs before Codex to materialize the
/// injected auth.json content.
export const CODEX_AUTH_JSON_BOOTSTRAP = [
  'install -d -m 700 "${CODEX_HOME:-$HOME/.codex}" \\',
  '  && printf \'%s\' "$CODEX_AUTH_JSON" > "${CODEX_HOME:-$HOME/.codex}/auth.json" \\',
  '  && chmod 600 "${CODEX_HOME:-$HOME/.codex}/auth.json" && unset CODEX_AUTH_JSON',
].join("\n");

/// Best-effort plan/account label from grant metadata.
export function subscriptionAccountLabel(grant: SecretGrant): string {
  const parts: string[] = [];
  const email = typeof grant.metadata?.email === "string" ? grant.metadata.email : grant.subjectHint;
  if (email) parts.push(email);
  if (typeof grant.metadata?.planType === "string") parts.push(grant.metadata.planType);
  if (subscriptionProviderOf(grant) === "openAi" && !isCodexTokenSet(grant)) parts.push("access token");
  return parts.join(" · ");
}
