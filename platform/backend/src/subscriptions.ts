/// Coding-agent subscription credentials: vendor-specific parsing and
/// normalisation for credentials that tools *inside environments* consume
/// (Claude Code `setup-token`, Codex `auth.json`). Core stores them as
/// ordinary `static_bearer` grants with metadata and injects them verbatim;
/// everything vendor-shaped lives here, in Platform.

export type SubscriptionProvider = "anthropic" | "openAi";
export type SubscriptionCredentialShape = "token" | "codexTokenSet";

/// Non-secret metadata written onto the grant. `subscription` is how the
/// Models page (and any other client) recognises these grants.
export interface SubscriptionMetadata extends Record<string, unknown> {
  subscription: "claudeCode" | "codex";
  credential: "token" | "tokenSet";
  source: "pasted";
  email?: string;
  accountId?: string;
  planType?: string;
}

export interface ParsedSubscriptionCredential {
  /// Provider id stored on the grant (`anthropic` | `openai`).
  providerId: string;
  /// The secret value stored and later injected verbatim.
  secret: string;
  shape: SubscriptionCredentialShape;
  metadata: SubscriptionMetadata;
  /// Best-effort expiry (Claude Code: paste + 1y; Codex: access-token `exp`).
  expiresAtMs?: number;
  subjectHint?: string;
}

export class SubscriptionCredentialError extends Error {}

/// `claude setup-token` mints one-year tokens.
export const CLAUDE_CODE_TOKEN_TTL_MS = 365 * 24 * 60 * 60 * 1000;

export function parseSubscriptionCredential(
  provider: SubscriptionProvider,
  credential: string,
  nowMs: number,
): ParsedSubscriptionCredential {
  return provider === "anthropic"
    ? parseClaudeCodeToken(credential, nowMs)
    : parseCodexCredential(credential);
}

export function parseClaudeCodeToken(input: string, nowMs: number): ParsedSubscriptionCredential {
  const token = input.trim();
  if (!token) throw new SubscriptionCredentialError("credential is empty");
  if (!token.startsWith("sk-ant-oat") || /\s/.test(token)) {
    throw new SubscriptionCredentialError(
      "not a Claude Code token (expected sk-ant-oat… from `claude setup-token`)",
    );
  }
  return {
    providerId: "anthropic",
    secret: token,
    shape: "token",
    metadata: { subscription: "claudeCode", credential: "token", source: "pasted" },
    expiresAtMs: nowMs + CLAUDE_CODE_TOKEN_TTL_MS,
  };
}

/// Accepts a pasted `$CODEX_HOME/auth.json` (Plus/Pro/Team token set) or a
/// bare ChatGPT Enterprise access token. A token set is normalised into the
/// exact document Codex reads, so the injected value can be written to
/// `auth.json` unchanged.
export function parseCodexCredential(input: string): ParsedSubscriptionCredential {
  const trimmed = input.trim();
  if (!trimmed) throw new SubscriptionCredentialError("credential is empty");
  if (!trimmed.startsWith("{")) {
    if (/\s/.test(trimmed)) {
      throw new SubscriptionCredentialError("expected an auth.json document or a single access token");
    }
    return {
      providerId: "openai",
      secret: trimmed,
      shape: "token",
      metadata: { subscription: "codex", credential: "token", source: "pasted" },
    };
  }
  let document: unknown;
  try {
    document = JSON.parse(trimmed);
  } catch (error) {
    throw new SubscriptionCredentialError(
      `auth.json is not valid JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  const tokens = isRecord(document) && isRecord(document.tokens) ? document.tokens : null;
  if (!tokens) {
    throw new SubscriptionCredentialError(
      "auth.json has no ChatGPT tokens (`tokens.access_token`); API-key-only files are not a subscription credential",
    );
  }
  const idToken = requiredString(tokens.id_token, "id_token");
  const accessToken = requiredString(tokens.access_token, "access_token");
  const refreshToken = requiredString(tokens.refresh_token, "refresh_token");
  const claims = decodeJwtPayload(idToken);
  if (!claims) throw new SubscriptionCredentialError("id_token is not a decodable JWT");
  const auth = isRecord(claims["https://api.openai.com/auth"])
    ? (claims["https://api.openai.com/auth"] as Record<string, unknown>)
    : {};
  const accountId =
    optionalString(tokens.account_id) ?? optionalString(auth.chatgpt_account_id);
  const email = optionalString(claims.email);
  const planType = optionalString(auth.chatgpt_plan_type);
  const accessClaims = decodeJwtPayload(accessToken);
  const exp = accessClaims && typeof accessClaims.exp === "number" ? accessClaims.exp * 1000 : undefined;

  const normalised = {
    auth_mode: "chatgpt",
    OPENAI_API_KEY: null,
    tokens: {
      id_token: idToken,
      access_token: accessToken,
      refresh_token: refreshToken,
      ...(accountId ? { account_id: accountId } : {}),
    },
    last_refresh: new Date().toISOString(),
  };
  return {
    providerId: "openai",
    secret: JSON.stringify(normalised),
    shape: "codexTokenSet",
    metadata: {
      subscription: "codex",
      credential: "tokenSet",
      source: "pasted",
      ...(email ? { email } : {}),
      ...(accountId ? { accountId } : {}),
      ...(planType ? { planType } : {}),
    },
    expiresAtMs: exp,
    subjectHint: email,
  };
}

/// Recognises subscription grants from their metadata (no dedicated kind).
export function isSubscriptionGrant(grant: {
  providerKind: string;
  metadata?: Record<string, unknown>;
}): boolean {
  return (
    grant.providerKind === "staticBearer"
    && (grant.metadata?.subscription === "claudeCode" || grant.metadata?.subscription === "codex")
  );
}

/// Claude Code prefers `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` over
/// `CLAUDE_CODE_OAUTH_TOKEN`; binding both silently disables the
/// subscription, so Platform refuses the pair.
export function conflictingAnthropicEnv(newName: string, existing: string[]): string | null {
  const keys = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];
  if (newName === "CLAUDE_CODE_OAUTH_TOKEN") return existing.find((n) => keys.includes(n)) ?? null;
  if (keys.includes(newName)) return existing.find((n) => n === "CLAUDE_CODE_OAUTH_TOKEN") ?? null;
  return null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function requiredString(value: unknown, field: string): string {
  const s = optionalString(value);
  if (!s) throw new SubscriptionCredentialError(`auth.json tokens are missing \`${field}\``);
  return s;
}

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  const [, payload, signature] = token.split(".");
  if (!payload || signature === undefined) return null;
  try {
    const json = Buffer.from(payload.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8");
    const value: unknown = JSON.parse(json);
    return isRecord(value) ? value : null;
  } catch {
    return null;
  }
}
