import type { DeploymentApiKeyView } from "@lightspeed-ai/sdk";

/** A key id the runtime allocated because the key's creator chose none. */
const ALLOCATED_KEY_ID = /^api_key_[0-9a-f]{32}$/;

/**
 * How a key is identified under its name: the id its creator chose, when
 * there is one, and the first characters of its current secret.
 */
export function keyIdentifier(key: Pick<DeploymentApiKeyView, "keyId" | "keyPrefix">): string {
  return ALLOCATED_KEY_ID.test(key.keyId) ? key.keyPrefix : `${key.keyId} · ${key.keyPrefix}`;
}

/**
 * The query that tells the admin key routes which scope a key id belongs to:
 * the runtime universe, or nothing for a deployment key.
 */
export function keyScopeQuery(key: Pick<DeploymentApiKeyView, "scope">): string {
  return key.scope.kind === "universe" ? `?universeId=${encodeURIComponent(key.scope.universeId)}` : "";
}
