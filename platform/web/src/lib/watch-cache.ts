import type { InfiniteData, Query, QueryClient, QueryKey } from "@tanstack/react-query";
import type { AgentNotification, EnvironmentView } from "@lightspeed-ai/sdk";
import type { SessionView, WorkspaceRow, WorkspaceTree } from "@/api";

type Pages<T> = InfiniteData<T>;
type EnvironmentPage = { environments: EnvironmentView[]; nextAfter?: string | null };
const WATCHED = new Set(["session", "environments", "workspaces", "workspace-tree"]);
const MAX_OVERLAPPING_UPDATES = 256;

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
    case "environments/changed": return `environment:${event.params.environment.environmentId}`;
    case "environments/removed": return `environment:${event.params.environmentId}`;
    case "workspaces/changed": return `workspace:${event.params.workspace.workspaceId}`;
    case "workspaces/removed": return `workspace:${event.params.workspaceId}`;
    default: return event.method;
  }
}

function applies(event: AgentNotification, key: QueryKey): boolean {
  switch (event.method) {
    case "session/changed": case "session/removed": {
      const id = event.method === "session/changed" ? event.params.session.id : event.params.sessionId;
      return key[0] === "session" && key[2] === id;
    }
    case "environments/changed": case "environments/removed": return key[0] === "environments" && key[2] === "pages";
    case "workspaces/changed": case "workspaces/removed": {
      const id = event.method === "workspaces/changed" ? event.params.workspace.workspaceId : event.params.workspaceId;
      return key[0] === "workspaces" || (key[0] === "workspace-tree" && key[2] === id);
    }
    default: return false;
  }
}

function patch(data: unknown, key: QueryKey, event: AgentNotification): unknown {
  if (!data) return data;
  switch (event.method) {
    case "session/changed": case "session/removed": {
      const row = event.method === "session/changed" ? event.params.session : undefined;
      if (!row) return data; // The detail read will return notFound.
      const old = data as SessionView;
      return { ...old, access: row.access, activity: row.activity, displayName: row.displayName,
        metadata: row.metadata, closedAtMs: row.closedAtMs, lifecycleStatus: row.lifecycleStatus,
        retention: row.retention, lifecycleControlled: row.lifecycleControlled, origin: row.origin,
        updatedAtMs: Math.max(old.updatedAtMs, row.updatedAtMs), executionFailure: row.executionFailure ?? null,
        activeEnvironmentId: row.activeEnvironmentId ?? null };
    }
    case "environments/changed": return updateEnvironmentPages(data as Pages<EnvironmentPage>, key, event.params.environment.environmentId, event.params.environment);
    case "environments/removed": return updateEnvironmentPages(data as Pages<EnvironmentPage>, key, event.params.environmentId);
    case "workspaces/changed": case "workspaces/removed": {
      const row = event.method === "workspaces/changed" ? event.params.workspace : undefined;
      const id = event.method === "workspaces/changed" ? event.params.workspace.workspaceId : event.params.workspaceId;
      if (key[0] === "workspace-tree") {
        const old = data as WorkspaceTree;
        // Never label an older manifest with a newer head. Reload that snapshot.
        return row && row.revision >= old.workspace.revision && row.headSnapshotRef === old.workspace.headSnapshotRef
          ? { ...old, workspace: row } : data;
      }
      const rows = data as WorkspaceRow[];
      if (row && rows.some(old => old.workspaceId === id && old.revision > row.revision)) return data;
      const next = rows.filter(old => old.workspaceId !== id);
      if (row) next.push(row);
      return next.sort((a, b) => b.updatedAtMs - a.updatedAtMs || a.workspaceId.localeCompare(b.workspaceId));
    }
    default: return data;
  }
}

/** Replay updates that overlap an HTTP read after its response enters the cache.
 * Session revisions also reject updates older than a completed list snapshot.
 */
export class WatchCache {
  private readonly overlapping = new Map<string, Map<string, () => void> | null>();
  private readonly unsubscribe: () => void;
  constructor(private readonly client: QueryClient, private readonly universeId: string) {
    this.unsubscribe = client.getQueryCache().subscribe(event => {
      if (event.type === "removed") { this.overlapping.delete(event.query.queryHash); return; }
      if (event.type === "updated" && event.action.type === "error") { this.overlapping.delete(event.query.queryHash); return; }
      if (event.type !== "updated" || event.action.type !== "success" || event.action.manual) return;
      const queued = this.overlapping.get(event.query.queryHash);
      this.overlapping.delete(event.query.queryHash);
      if (queued === null) { void client.invalidateQueries({ queryKey: event.query.queryKey, exact: true }); return; }
      if (queued) for (const apply of queued.values()) apply();
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
      if (!this.belongs(query)) continue;
      const update = this.cacheUpdate(query, event);
      if (!update) continue;
      if (query.state.fetchStatus === "fetching") {
        let queued = this.overlapping.get(query.queryHash);
        if (queued !== null) {
          queued ??= new Map();
          queued.set(update.key, update.apply);
          this.overlapping.set(query.queryHash, queued.size > MAX_OVERLAPPING_UPDATES ? null : queued);
        }
      }
      update.apply();
    }
  }

  private belongs(query: Query) {
    return query.meta?.universeWatch
      ? query.meta.universeWatch.universeId === this.universeId
      : query.queryKey[1] === this.universeId && WATCHED.has(String(query.queryKey[0]));
  }

  private cacheUpdate(query: Query, event: AgentNotification) {
    const watch = query.meta?.universeWatch;
    if (!watch) {
      if (!applies(event, query.queryKey)) return;
      return { key: notificationKey(event), apply: () => this.applyHostUpdate(query, event) };
    }
    const update = watch.update(event);
    if (!update) return;
    return {
      key: update.key,
      apply: () => {
        const patch = update.apply;
        if (patch) {
          this.client.setQueryData(query.queryKey, (data: unknown) => data === undefined ? undefined : patch(data));
        }
        const { refetch } = update;
        const { data, fetchStatus } = query.state;
        if (refetch === true || (refetch && fetchStatus === "idle" && data !== undefined && refetch(data))) {
          void this.client.invalidateQueries({ queryKey: query.queryKey, exact: true });
        }
      },
    };
  }

  private applyHostUpdate(query: Query, event: AgentNotification) {
    this.client.setQueryData(query.queryKey, (data: unknown) => patch(data, query.queryKey, event));
    if (event.method === "session/removed" && query.queryKey[0] === "session") {
      void this.client.invalidateQueries({ queryKey: query.queryKey, exact: true });
    }
    if (query.queryKey[0] === "workspace-tree") {
      const tree = query.state.data as WorkspaceTree | undefined;
      const needsRead = event.method === "workspaces/removed" || (event.method === "workspaces/changed" &&
        (!tree || tree.workspace.revision < event.params.workspace.revision));
      if (needsRead) void this.client.invalidateQueries({ queryKey: query.queryKey, exact: true }, { cancelRefetch: false });
    }
  }
}
