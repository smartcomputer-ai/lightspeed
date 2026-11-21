import { defineSettingFamily } from "@lightspeed-ai/platform-web/modules/settings";

export interface SessionListPreferences {
  showClosed: boolean;
  showSubagents: boolean;
  /// Managed work (bots' conversations, workflow-driven sessions) has its
  /// manager's page; the list shows it only when asked.
  showControlledSessions: boolean;
  showSessionIds: boolean;
  metadataKeys: string[];
}

export const DEFAULT_SESSION_LIST_PREFERENCES: SessionListPreferences = {
  showClosed: true,
  showSubagents: true,
  showControlledSessions: false,
  showSessionIds: false,
  metadataKeys: [],
};

/**
 * What the list's scope leaves out, as a sentence for an empty list, or null
 * when it leaves out nothing. Scope is not a filter and is never counted as
 * one; only metadata filters are.
 */
export function hiddenSessionsNote(
  preferences: Pick<SessionListPreferences, "showClosed" | "showSubagents" | "showControlledSessions">,
): string | null {
  const hidden = [
    !preferences.showClosed && "closed",
    !preferences.showSubagents && "sub-agent",
    !preferences.showControlledSessions && "managed",
  ].filter((kind): kind is string => Boolean(kind));
  const last = hidden.pop();
  if (!last) return null;
  const listed = hidden.length === 0 ? last : `${hidden.join(", ")} and ${last}`;
  return `${listed.charAt(0).toUpperCase()}${listed.slice(1)} sessions are hidden.`;
}

/// A stored session list choice made valid field by field: a missing or
/// invalid field takes its default.
export function parseSessionListPreferences(stored: unknown): SessionListPreferences | undefined {
  if (!stored || typeof stored !== "object" || Array.isArray(stored)) return undefined;
  const record = stored as Record<string, unknown>;
  return {
    showClosed: typeof record.showClosed === "boolean"
      ? record.showClosed
      : DEFAULT_SESSION_LIST_PREFERENCES.showClosed,
    showSubagents: typeof record.showSubagents === "boolean"
      ? record.showSubagents
      : DEFAULT_SESSION_LIST_PREFERENCES.showSubagents,
    showControlledSessions: typeof record.showControlledSessions === "boolean"
      ? record.showControlledSessions
      : DEFAULT_SESSION_LIST_PREFERENCES.showControlledSessions,
    showSessionIds: typeof record.showSessionIds === "boolean"
      ? record.showSessionIds
      : DEFAULT_SESSION_LIST_PREFERENCES.showSessionIds,
    metadataKeys: Array.isArray(record.metadataKeys)
      ? [...new Set(record.metadataKeys.filter(
        (key): key is string => typeof key === "string" && key.trim().length > 0,
      ).map((key) => key.trim()))]
      : DEFAULT_SESSION_LIST_PREFERENCES.metadataKeys,
  };
}

/// A stored metadata filter without its invalid entries.
export function parseSessionMetadataFilter(stored: unknown): Record<string, string> | undefined {
  if (!stored || typeof stored !== "object" || Array.isArray(stored)) return undefined;
  return Object.fromEntries(
    Object.entries(stored).filter(
      ([key, value]) => key.length > 0 && typeof value === "string",
    ),
  ) as Record<string, string>;
}

export function metadataFilterFromSearchParams(params: URLSearchParams): Record<string, string> {
  const filter: Record<string, string> = {};
  for (const raw of params.getAll("metadata")) {
    const pair = parseMetadataPair(raw);
    if (pair) filter[pair.key] = pair.value;
  }
  return filter;
}

export function searchParamsWithMetadataFilter(
  current: URLSearchParams,
  filter: Record<string, string>,
): URLSearchParams {
  const next = new URLSearchParams(current);
  next.delete("metadata");
  for (const [key, value] of Object.entries(filter)) {
    next.append("metadata", value ? `${key}=${value}` : key);
  }
  return next;
}

/** The value may itself contain `=`; only the first separator is structural. */
export function parseMetadataPair(raw: string): { key: string; value: string } | null {
  const at = raw.indexOf("=");
  const key = (at < 0 ? raw : raw.slice(0, at)).trim();
  const value = at < 0 ? "" : raw.slice(at + 1).trim();
  return key ? { key, value } : null;
}

export const sessionListPreferences = defineSettingFamily("sessions.list", (universeId: string) => ({
  key: `lightspeed:sessions:list-preferences:${universeId}`,
  defaultValue: DEFAULT_SESSION_LIST_PREFERENCES,
  parse: parseSessionListPreferences,
}));

export const sessionMetadataFilter = defineSettingFamily("sessions.metadata-filter", (universeId: string) => ({
  key: `lightspeed:sessions:metadata-filter:${universeId}`,
  defaultValue: {} as Record<string, string>,
  parse: parseSessionMetadataFilter,
}));
