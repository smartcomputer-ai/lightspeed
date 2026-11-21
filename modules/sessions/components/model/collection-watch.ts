import type { InfiniteData, Query } from "@tanstack/react-query";
import type { SessionSummaryView } from "@lightspeed-ai/sdk";
import type { SessionListPage } from "@lightspeed-ai/platform-web/modules/client";
import { universeWatchMeta } from "@lightspeed-ai/platform-web/modules";

type SessionPages = InfiniteData<SessionListPage>;

const sessionOrder = (a: SessionSummaryView, b: SessionSummaryView) => b.updatedAtMs - a.updatedAtMs || (a.id < b.id ? 1 : a.id > b.id ? -1 : 0);

/** Preserve the loaded range and its opaque continuation. Unseen rows beyond
 * that range belong to a later page; inserting at the front must not drop its tail.
 */
function updateSessionPages(data: SessionPages, matches: (row: SessionSummaryView) => boolean, id: string, row?: SessionSummaryView): SessionPages {
  const all = data.pages.flatMap(page => page.sessions);
  const previous = all.find(item => item.id === id);
  if (row && previous && previous.revision !== undefined && row.revision !== undefined &&
    (previous.revision > row.revision || (previous.revision === row.revision && previous.updatedAtMs > row.updatedAtMs))) return data;
  const tail = all.at(-1);
  const inRange = previous || !data.pages.at(-1)?.nextCursor || !tail || (row && sessionOrder(row, tail) <= 0);
  const include = row && matches(row) && inRange;
  if (!previous && !include) return data;
  const rows = all.filter(item => item.id !== id);
  if (include) rows.push(row);
  rows.sort(sessionOrder);
  let offset = 0;
  const pages = data.pages.map((page, index) => {
    const count = index === data.pages.length - 1 ? rows.length - offset : page.sessions.length;
    const sessions = rows.slice(offset, offset + count);
    offset += count;
    return { ...page, sessions };
  });
  return { ...data, pages };
}

/** Lineage readers and module rosters subscribe with their own membership rules. */
export function sessionCollectionMeta(universeId: string, matches: (row: SessionSummaryView) => boolean) {
  return {
    sessionCollection: universeId,
    ...universeWatchMeta<SessionPages>(universeId, event => {
      if (event.method !== "session/removed" && event.method !== "session/changed") return;
      const row = event.method === "session/changed" ? event.params.session : undefined;
      const id = event.method === "session/removed" ? event.params.sessionId : event.params.session.id;
      return {
        key: id,
        apply: pages => updateSessionPages(pages, matches, id, row),
        // A fresh page bounds growth when new rows continually enter a loaded range.
        refetch: pages => pages.pages.some(page => page.sessions.length > 150),
      };
    }),
  };
}

/** Sessions can change from any surface that hosts the shared session controls. */
export function sessionCollections(universeId: string) {
  return { predicate: (query: Query) => query.meta?.sessionCollection === universeId };
}
