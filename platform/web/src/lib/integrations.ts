import type { SecretGrant } from "@/api";

/// Pure helpers for GitHub Apps and coding-agent subscriptions (the
/// Platform's `/integrations` routes). Kept free of React so components and
/// tests can share them.

/// The grant for one installation, preferring a live one when a revoked and
/// an active grant both exist (a re-grant after revocation).
export function installationGrantFor(
  grants: Pick<SecretGrant, "status" | "metadata">[],
  installationId: number,
): Pick<SecretGrant, "status" | "metadata"> | undefined {
  const matching = grants.filter(
    (grant) => Number(grant.metadata?.installation_id) === installationId,
  );
  return (
    matching.find((grant) => grant.status === "active") ??
    matching.find((grant) => grant.status !== "revoked") ??
    matching[0]
  );
}

/// Sorted `[name, level]` pairs from GitHub's permission map.
export function permissionEntries(
  permissions: Record<string, unknown> | undefined,
): [string, string][] {
  if (!permissions) return [];
  return Object.entries(permissions)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string" && entry[1] !== "")
    .sort(([a], [b]) => a.localeCompare(b));
}

/// Compact "contents: read, pull_requests: write" rendering.
export function permissionSummary(permissions: Record<string, unknown> | undefined): string {
  const entries = permissionEntries(permissions).map(([name, level]) => `${name}: ${level}`);
  return entries.length ? entries.join(", ") : "—";
}

export function validateGitHubAppForm(input: { appId: string; privateKey: string }): string | null {
  if (!/^[0-9]+$/.test(input.appId.trim())) {
    return "the App ID must be the numeric ID from the GitHub App settings page";
  }
  if (!input.privateKey.trim()) {
    return "a private key is required";
  }
  if (!/-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(input.privateKey)) {
    return "the private key must be a PEM file (it starts with -----BEGIN … PRIVATE KEY-----)";
  }
  return null;
}

export function formatExpiry(expiresAtMs: number | null | undefined, nowMs = Date.now()): string {
  if (!expiresAtMs) return "—";
  const days = Math.floor((expiresAtMs - nowMs) / 86_400_000);
  if (days < 0) return "expired";
  if (days === 0) return "today";
  if (days < 45) return `in ${days} d`;
  return new Date(expiresAtMs).toISOString().slice(0, 10);
}
