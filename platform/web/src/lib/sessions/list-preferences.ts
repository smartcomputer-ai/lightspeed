export interface SessionListPreferences {
  showClosed: boolean;
  showSubagents: boolean;
  /// Managed work (bots' conversations, workflow-driven sessions) has its
  /// manager's page; the list shows it only when asked.
  showManagedSessions: boolean;
  showSessionIds: boolean;
  metadataKeys: string[];
}

export const DEFAULT_SESSION_LIST_PREFERENCES: SessionListPreferences = {
  showClosed: true,
  showSubagents: true,
  showManagedSessions: false,
  showSessionIds: false,
  metadataKeys: [],
};

/**
 * What the list's scope leaves out, as a sentence for an empty list, or null
 * when it leaves out nothing. Scope is not a filter and is never counted as
 * one; only metadata filters are.
 */
export function hiddenSessionsNote(
  preferences: Pick<SessionListPreferences, "showClosed" | "showSubagents" | "showManagedSessions">,
): string | null {
  const hidden = [
    !preferences.showClosed && "closed",
    !preferences.showSubagents && "sub-agent",
    !preferences.showManagedSessions && "managed",
  ].filter((kind): kind is string => Boolean(kind));
  const last = hidden.pop();
  if (!last) return null;
  const listed = hidden.length === 0 ? last : `${hidden.join(", ")} and ${last}`;
  return `${listed.charAt(0).toUpperCase()}${listed.slice(1)} sessions are hidden.`;
}

const LIST_PREFERENCES_KEY_PREFIX = "lightspeed:sessions:list-preferences:";
const METADATA_FILTER_KEY_PREFIX = "lightspeed:sessions:metadata-filter:";

interface StorageReader {
  getItem(key: string): string | null;
}

interface StorageWriter {
  setItem(key: string, value: string): void;
}

export function readSessionListPreferences(
  universeId: string,
  storage: StorageReader | undefined = typeof window === "undefined"
    ? undefined
    : window.localStorage,
): SessionListPreferences {
  if (!storage) return DEFAULT_SESSION_LIST_PREFERENCES;
  try {
    const parsed: unknown = JSON.parse(
      storage.getItem(`${LIST_PREFERENCES_KEY_PREFIX}${universeId}`) ?? "null",
    );
    if (!parsed || typeof parsed !== "object") return DEFAULT_SESSION_LIST_PREFERENCES;
    const record = parsed as Record<string, unknown>;
    return {
      showClosed: typeof record.showClosed === "boolean"
        ? record.showClosed
        : DEFAULT_SESSION_LIST_PREFERENCES.showClosed,
      showSubagents: typeof record.showSubagents === "boolean"
        ? record.showSubagents
        : DEFAULT_SESSION_LIST_PREFERENCES.showSubagents,
      showManagedSessions: typeof record.showManagedSessions === "boolean"
        ? record.showManagedSessions
        : DEFAULT_SESSION_LIST_PREFERENCES.showManagedSessions,
      showSessionIds: typeof record.showSessionIds === "boolean"
        ? record.showSessionIds
        : DEFAULT_SESSION_LIST_PREFERENCES.showSessionIds,
      metadataKeys: Array.isArray(record.metadataKeys)
        ? [...new Set(record.metadataKeys.filter(
          (key): key is string => typeof key === "string" && key.trim().length > 0,
        ).map((key) => key.trim()))]
        : DEFAULT_SESSION_LIST_PREFERENCES.metadataKeys,
    };
  } catch {
    return DEFAULT_SESSION_LIST_PREFERENCES;
  }
}

export function writeSessionListPreferences(
  universeId: string,
  preferences: SessionListPreferences,
  storage: StorageWriter | undefined = typeof window === "undefined"
    ? undefined
    : window.localStorage,
) {
  try {
    storage?.setItem(
      `${LIST_PREFERENCES_KEY_PREFIX}${universeId}`,
      JSON.stringify(preferences),
    );
  } catch {
    // Browsing remains usable when storage is unavailable or full.
  }
}

export function readSessionMetadataFilter(
  universeId: string,
  storage: StorageReader | undefined = typeof window === "undefined"
    ? undefined
    : window.localStorage,
): Record<string, string> {
  if (!storage) return {};
  try {
    const raw = storage.getItem(`${METADATA_FILTER_KEY_PREFIX}${universeId}`);
    const parsed: unknown = JSON.parse(raw ?? "null");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        ([key, value]) => key.length > 0 && typeof value === "string",
      ),
    ) as Record<string, string>;
  } catch {
    return {};
  }
}

export function writeSessionMetadataFilter(
  universeId: string,
  filter: Record<string, string>,
  storage: StorageWriter | undefined = typeof window === "undefined"
    ? undefined
    : window.localStorage,
) {
  try {
    storage?.setItem(`${METADATA_FILTER_KEY_PREFIX}${universeId}`, JSON.stringify(filter));
  } catch {
    // URL-backed filtering still works when browser storage does not.
  }
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
