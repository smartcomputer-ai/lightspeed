import { sessionCollectionMeta } from "@lightspeed-ai/platform-web/modules/conversations";

interface RosterFilters {
  showClosed: boolean;
  showControlledSessions: boolean;
  showSubagents: boolean;
}

export function rosterQuery(universeId: string, metadata: Record<string, string>, filters: RosterFilters) {
  return {
    queryKey: ["sessions", universeId, metadata, filters],
    meta: sessionCollectionMeta(universeId, row =>
      (filters.showClosed || row.lifecycleStatus !== "closed")
      && (filters.showControlledSessions || !row.lifecycleControlled)
      && (filters.showSubagents || !row.origin)
      && Object.entries(metadata).every(([key, value]) =>
        value ? row.metadata?.[key] === value : row.metadata?.[key] !== undefined)),
  };
}
