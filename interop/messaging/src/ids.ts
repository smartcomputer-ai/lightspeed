import { createHash } from "node:crypto";

export function stableHash(parts: readonly unknown[]): string {
  const hash = createHash("sha256");
  for (const part of parts) {
    hash.update(JSON.stringify(part));
    hash.update("\0");
  }
  return hash.digest("base64url").slice(0, 32);
}

export function stableSessionId(prefix: string, parts: readonly unknown[]): string {
  return `${sanitizeIdPrefix(prefix)}_${stableHash(parts)}`;
}

export function stableSubmissionId(provider: string, parts: readonly unknown[]): string {
  return `${sanitizeIdPrefix(provider)}_${stableHash(parts)}`;
}

function sanitizeIdPrefix(prefix: string): string {
  const sanitized = prefix
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return sanitized || "bridge";
}
