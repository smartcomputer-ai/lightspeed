import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { PlatformOperationParams, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import type { ResourceStatusInfo } from "@/lib/resource-status";

export type ApiKey = PlatformOperationResult<"platform/credentials/list">["credentials"][number];
export type ApiKeyInput = Omit<PlatformOperationParams<"platform/credentials/create">, never>;

/// Whose keys these are: the signed-in person, one service user (whose keys
/// are always confined to its organization), or another person, whose keys
/// an organization's administrators may see but not change.
export type ApiKeyOwner =
  | { kind: "personal" }
  | { kind: "serviceUser"; organizationId: string; serviceUserId: string; name: string; enabled: boolean }
  | { kind: "person"; name: string; email: string };
/// An owner whose keys this person may list, create and revoke.
export type ManagedApiKeyOwner = Exclude<ApiKeyOwner, { kind: "person" }>;

export function apiKeysQueryKey(owner: ManagedApiKeyOwner) {
  return owner.kind === "personal" ? ["api-keys", "personal"] : ["api-keys", owner.organizationId, owner.serviceUserId];
}

export function useApiKeys(owner: ManagedApiKeyOwner) {
  return useQuery({
    queryKey: apiKeysQueryKey(owner),
    queryFn: () => owner.kind === "personal"
      ? platformApi("platform/credentials/list", {})
      : platformApi("platform/service-users/credentials/list", { organizationId: owner.organizationId, serviceUserId: owner.serviceUserId }),
  });
}

export function useInvalidateApiKeys(owner: ManagedApiKeyOwner) {
  const client = useQueryClient();
  // An organization's key inventory lists service-user and personal keys too.
  return () => Promise.all([
    client.invalidateQueries({ queryKey: apiKeysQueryKey(owner) }),
    client.invalidateQueries({ queryKey: ["organization-api-keys"] }),
  ]);
}

export function createApiKey(owner: ManagedApiKeyOwner, input: ApiKeyInput) {
  return owner.kind === "personal"
    ? platformApi("platform/credentials/create", input)
    : platformApi("platform/service-users/credentials/create", { ...input, organizationId: owner.organizationId, serviceUserId: owner.serviceUserId });
}

/// A new secret for the same key; the old secret stops working at once.
export function replaceApiKey(owner: ManagedApiKeyOwner, keyId: string) {
  return owner.kind === "personal"
    ? platformApi("platform/credentials/replace", { keyId })
    : platformApi("platform/service-users/credentials/replace", { organizationId: owner.organizationId, serviceUserId: owner.serviceUserId, keyId });
}

export function revokeApiKey(owner: ManagedApiKeyOwner, keyId: string) {
  return owner.kind === "personal"
    ? platformApi("platform/credentials/revoke", { keyId })
    : platformApi("platform/service-users/credentials/revoke", { organizationId: owner.organizationId, serviceUserId: owner.serviceUserId, keyId });
}

export function apiKeyExpired(key: ApiKey): boolean {
  return key.expiresAt !== null && new Date(key.expiresAt).getTime() <= Date.now();
}

export function apiKeyStatus(key: ApiKey, owner: ApiKeyOwner): ResourceStatusInfo {
  if (!key.enabled) return { label: "Revoked", tone: "neutral" };
  if (apiKeyExpired(key)) return { label: "Expired", tone: "neutral" };
  if (owner.kind === "serviceUser" && !owner.enabled) return { label: "Service user disabled", tone: "warning" };
  return { label: "Active", tone: "positive" };
}

export function apiKeyExpiry(key: ApiKey): string {
  if (key.expiresAt === null) return "Never";
  return `${apiKeyExpired(key) ? "Expired" : ""} ${new Date(key.expiresAt).toLocaleDateString()}`.trim();
}

/// What a key may reach, in a few words. An empty selection is shown as
/// such, never as unrestricted access.
export function apiKeyAccessSummary(key: ApiKey, owner: ApiKeyOwner): string {
  const { organizationIds, universeIds, readOnly, operations } = key.restrictions;
  const parts: string[] = [];
  if (universeIds) parts.push(universeIds.length ? `${universeIds.length} ${universeIds.length === 1 ? "universe" : "universes"}` : "No universes");
  else if (owner.kind !== "serviceUser") {
    parts.push(organizationIds ? (organizationIds.length ? `${organizationIds.length} ${organizationIds.length === 1 ? "organization" : "organizations"}` : "No organizations") : "All organizations");
  } else parts.push("Its organization");
  if (operations) parts.push(`${operations.length} ${operations.length === 1 ? "operation" : "operations"}`);
  if (readOnly) parts.push("read only");
  return parts.join(" · ");
}
