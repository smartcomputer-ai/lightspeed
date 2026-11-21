import type { AgentNotification } from "@lightspeed-ai/sdk";

interface WatchUpdate<T> {
  /** Events for the same entity supersede each other during an overlapping read. */
  key: string;
  apply?: (current: T) => T;
  /** A predicate runs against the updated cache when no read is in flight. */
  refetch?: true | ((current: T) => boolean);
}

interface UniverseWatch {
  universeId: string;
  update: (event: AgentNotification) => WatchUpdate<unknown> | undefined;
}

declare module "@tanstack/react-query" {
  interface Register {
    queryMeta: Record<string, unknown> & { universeWatch?: UniverseWatch };
  }
}

/** Attach a cache projection to one module query on the shared universe event stream. */
export function universeWatchMeta<T>(universeId: string, update: (event: AgentNotification) => WatchUpdate<T> | undefined) {
  return { universeWatch: { universeId, update } as UniverseWatch };
}
