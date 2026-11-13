import type { InfiniteData, Query, QueryClient, QueryKey } from "@tanstack/react-query";
import type { AgentNotification, BotListItem, EnvironmentView, SessionSummaryView } from "@lightspeed-ai/sdk";
import type { SessionListPage, SessionView } from "@/api";

type Pages<T> = InfiniteData<T>;
type EnvironmentPage = { environments: EnvironmentView[]; nextAfter?: string | null };
const WATCHED = new Set(["sessions", "session-children", "session", "bots", "bot", "environments"]);
const MAX_OVERLAPPING_UPDATES = 256;

function sessionMatches(row: SessionSummaryView, key: QueryKey): boolean {
  if (key[0] === "session-children") return row.origin?.parentSessionId === key[2];
  const metadata = (key[2] ?? {}) as Record<string, string>;
  const flags = key[3] as { showClosed?: boolean; showControlledSessions?: boolean; showSubagents?: boolean } | undefined;
  return (!flags || (flags.showClosed || row.lifecycleStatus !== "closed")
      && (flags.showControlledSessions || !row.lifecycleControlled)
      && (flags.showSubagents || !row.origin))
    && Object.entries(metadata).every(([key, value]) => value ? row.metadata?.[key] === value : row.metadata?.[key] !== undefined);
}

const sessionOrder = (a: SessionSummaryView, b: SessionSummaryView) => b.updatedAtMs - a.updatedAtMs || (a.id < b.id ? 1 : a.id > b.id ? -1 : 0);

/** Preserve the loaded range and its opaque continuation. Unseen rows beyond
 * that range belong to a later page; inserting at the front must not drop its tail.
 */
export function updateSessionPages(data: Pages<SessionListPage>, key: QueryKey, id: string, row?: SessionSummaryView): Pages<SessionListPage> {
  const all = data.pages.flatMap(page => page.sessions);
  const previous = all.find(item => item.id === id);
  if (row && previous && previous.revision !== undefined && row.revision !== undefined &&
    (previous.revision > row.revision || (previous.revision === row.revision && previous.updatedAtMs > row.updatedAtMs))) return data;
  const tail = all.at(-1);
  const inRange = previous || !data.pages.at(-1)?.nextCursor || !tail || (row && sessionOrder(row, tail) <= 0);
  const include = row && sessionMatches(row, key) && inRange;
  if (!previous && !include) return data;
  const rows = all.filter(item => item.id !== id);
  if (include) rows.push(row);
  rows.sort(sessionOrder);
  let offset = 0;
  return { ...data, pages: data.pages.map((page, index) => {
    const count = index === data.pages.length - 1 ? rows.length - offset : page.sessions.length;
    const sessions = rows.slice(offset, offset + count); offset += count;
    return { ...page, sessions };
  }) };
}

function updateEnvironmentPages(data: Pages<EnvironmentPage>, key: QueryKey, id: string, row?: EnvironmentView): Pages<EnvironmentPage> {
  const all = data.pages.flatMap(page => page.environments);
  const previous = all.find(item => item.environmentId === id);
  if (row && previous && previous.updatedAtMs > row.updatedAtMs) return data;
  const tail = all.at(-1);
  const inRange = previous || !data.pages.at(-1)?.nextAfter || !tail || (row && row.environmentId <= tail.environmentId);
  const include = row && (key[3] || row.state !== "closed") && inRange;
  if (!previous && !include) return data;
  const rows = all.filter(item => item.environmentId !== id);
  if (include) rows.push(row);
  rows.sort((a, b) => a.environmentId < b.environmentId ? -1 : a.environmentId > b.environmentId ? 1 : 0);
  let offset = 0;
  return { ...data, pages: data.pages.map((page, index) => {
    const count = index === data.pages.length - 1 ? rows.length - offset : page.environments.length;
    const environments = rows.slice(offset, offset + count); offset += count;
    return { ...page, environments };
  }) };
}

function notificationKey(event: AgentNotification): string {
  switch (event.method) {
    case "session/changed": return `session:${event.params.session.id}`;
    case "session/removed": return `session:${event.params.sessionId}`;
    case "bots/changed": return `bot:${event.params.bot.botId}`;
    case "bots/removed": return `bot:${event.params.botId}`;
    case "environments/changed": return `environment:${event.params.environment.environmentId}`;
    case "environments/removed": return `environment:${event.params.environmentId}`;
    default: return event.method;
  }
}

function applies(event: AgentNotification, key: QueryKey): boolean {
  switch (event.method) {
    case "session/changed": case "session/removed": {
      const id = event.method === "session/changed" ? event.params.session.id : event.params.sessionId;
      return key[0] === "sessions" || key[0] === "session-children" || (key[0] === "session" && key[2] === id);
    }
    case "bots/changed": case "bots/removed": {
      const id = event.method === "bots/changed" ? event.params.bot.botId : event.params.botId;
      return key[0] === "bots" || (key[0] === "bot" && key[2] === id);
    }
    case "environments/changed": case "environments/removed": return key[0] === "environments" && key[2] === "pages";
    default: return false;
  }
}

function patch(data: unknown, key: QueryKey, event: AgentNotification): unknown {
  if (!data) return data;
  switch (event.method) {
    case "session/changed": case "session/removed": {
      const row = event.method === "session/changed" ? event.params.session : undefined;
      const id = row?.id ?? (event as Extract<AgentNotification, { method: "session/removed" }>).params.sessionId;
      if (key[0] !== "session") return updateSessionPages(data as Pages<SessionListPage>, key, id, row);
      if (!row) return data; // The detail read will return notFound.
      const old = data as SessionView;
      return { ...old, access: row.access, activity: row.activity, displayName: row.displayName,
        metadata: row.metadata, closedAtMs: row.closedAtMs, lifecycleStatus: row.lifecycleStatus,
        retention: row.retention, lifecycleControlled: row.lifecycleControlled, origin: row.origin,
        updatedAtMs: Math.max(old.updatedAtMs, row.updatedAtMs), executionFailure: row.executionFailure ?? null,
        activeEnvironmentId: row.activeEnvironmentId ?? null };
    }
    case "bots/changed": case "bots/removed": {
      const row = event.method === "bots/changed" ? event.params.bot : undefined;
      const id = row?.botId ?? (event as Extract<AgentNotification, { method: "bots/removed" }>).params.botId;
      if (key[0] === "bot") return row ? { ...data as object, bot: row, status: row.status } : data;
      const old = data as { bots?: BotListItem[] };
      const bots = (old.bots ?? []).filter(bot => bot.botId !== id);
      if (row) bots.push(row);
      return { ...old, bots };
    }
    case "environments/changed": return updateEnvironmentPages(data as Pages<EnvironmentPage>, key, event.params.environment.environmentId, event.params.environment);
    case "environments/removed": return updateEnvironmentPages(data as Pages<EnvironmentPage>, key, event.params.environmentId);
    default: return data;
  }
}

/** Replay updates that overlap an HTTP read after its response enters the cache.
 * Session revisions also reject updates older than a completed list snapshot.
 */
export class WatchCache {
  private readonly overlapping = new Map<string, Map<string, AgentNotification> | null>();
  private readonly unsubscribe: () => void;
  constructor(private readonly client: QueryClient, private readonly universeId: string) {
    this.unsubscribe = client.getQueryCache().subscribe(event => {
      if (event.type === "removed") { this.overlapping.delete(event.query.queryHash); return; }
      if (event.type === "updated" && event.action.type === "error") { this.overlapping.delete(event.query.queryHash); return; }
      if (event.type !== "updated" || event.action.type !== "success" || event.action.manual) return;
      const queued = this.overlapping.get(event.query.queryHash);
      this.overlapping.delete(event.query.queryHash);
      if (queued === null) { void client.invalidateQueries({ queryKey: event.query.queryKey, exact: true }); return; }
      if (queued) for (const update of queued.values()) this.apply(event.query, update);
    });
  }

  dispose() { this.unsubscribe(); this.overlapping.clear(); }

  receive(event: AgentNotification) {
    if (event.method === "watch/reset") {
      this.overlapping.clear();
      for (const query of this.client.getQueryCache().getAll()) {
        if (this.belongs(query) && query.state.fetchStatus === "fetching" && !query.state.data) this.overlapping.set(query.queryHash, null);
      }
      void this.client.invalidateQueries({ predicate: query => this.belongs(query) });
      return;
    }
    for (const query of this.client.getQueryCache().getAll()) {
      if (!this.belongs(query) || !applies(event, query.queryKey)) continue;
      if (query.state.fetchStatus === "fetching") {
        let queued = this.overlapping.get(query.queryHash);
        if (queued !== null) {
          queued ??= new Map();
          queued.set(notificationKey(event), event);
          this.overlapping.set(query.queryHash, queued.size > MAX_OVERLAPPING_UPDATES ? null : queued);
        }
      }
      this.apply(query, event);
    }
  }

  private belongs(query: Query) { return query.queryKey[1] === this.universeId && WATCHED.has(String(query.queryKey[0])); }

  private apply(query: Query, event: AgentNotification) {
    this.client.setQueryData(query.queryKey, (data: unknown) => patch(data, query.queryKey, event));
    if ((event.method === "session/removed" && query.queryKey[0] === "session") ||
        (event.method === "bots/removed" && query.queryKey[0] === "bot")) {
      void this.client.invalidateQueries({ queryKey: query.queryKey, exact: true });
    }
    // Bound growth when new rows continually enter the loaded range. A fresh
    // server page restores its normal size and authoritative continuation.
    const data = query.state.data as Pages<SessionListPage> | undefined;
    if (query.state.fetchStatus === "idle" && query.queryKey[0] === "sessions" && data?.pages?.some(page => page.sessions.length > 150)) {
      void this.client.invalidateQueries({ queryKey: query.queryKey, exact: true });
    }
  }
}
