import type { ProfileDocument, ProfileSessionRetention } from "@/api";

export type SessionProfileFields = {
  config?: Record<string, unknown> | undefined;
  instructions?: { type: "text"; text: string } | undefined;
  metadata?: Record<string, string> | undefined;
  retention?: ProfileSessionRetention | undefined;
};

/** Overlay edited fields onto the latest profile while removing view timestamps. */
export function mergeSessionProfileFields(
  latest: ProfileDocument,
  fields: SessionProfileFields,
): ProfileDocument {
  const { createdAtMs: _created, updatedAtMs: _updated, ...document } = latest;
  const next: Record<string, unknown> = { ...document };
  for (const [key, value] of Object.entries(fields)) {
    if (value === undefined) delete next[key];
    else next[key] = value;
  }
  return next as ProfileDocument;
}
