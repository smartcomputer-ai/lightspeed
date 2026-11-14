import { afterEach, expect, it, vi } from "vitest";
import { QueryClient, QueryObserver, type InfiniteData } from "@tanstack/react-query";
import type { AgentNotification, SessionSummaryView } from "@lightspeed-ai/sdk";
import type { SessionListPage, WorkspaceRow, WorkspaceTree } from "@/api";
import { WatchCache } from "./watch-cache";

const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
let cache = new WatchCache(client, "u");
afterEach(() => { cache.dispose(); client.clear(); cache = new WatchCache(client, "u"); });
const key = ["sessions", "u", { team: "blue" }, { showClosed: false, showSubagents: false, showControlledSessions: false }];
const row = (id: string, revision = 1, extra = {}): SessionSummaryView => ({
  id, revision, createdAtMs: 1, updatedAtMs: revision, lifecycleStatus: "open", metadata: { team: "blue" },
  activity: "idle", access: { visibility: "universe" }, retention: { rootSessionId: id }, lifecycleControlled: false, ...extra,
} as SessionSummaryView);
const changed = (session: SessionSummaryView): AgentNotification => ({ method: "session/changed", params: { session } });
const pages = (...rows: SessionSummaryView[]): InfiniteData<SessionListPage> => ({ pages: [{ sessions: rows, nextCursor: null }], pageParams: [null] });
const rows = () => client.getQueryData<InfiniteData<SessionListPage>>(key)!.pages.flatMap(page => page.sessions);
const workspace = (revision: number, head = `head-${revision}`): WorkspaceRow => ({ workspaceId: "w", revision, headSnapshotRef: head,
  files: 1, bytes: 1, createdAtMs: 1, updatedAtMs: revision });
const tree = (revision: number): WorkspaceTree => ({ workspace: workspace(revision), manifest: { schema_version: "1", root: { entries: {} }, totals: { files: 0, bytes: 0 } } } as WorkspaceTree);
const workspaceChanged = (revision: number, head?: string): AgentNotification => ({ method: "workspaces/changed", params: { workspace: workspace(revision, head) } });

it("updates membership and ordering using the existing list filters and rejects older revisions", () => {
  client.setQueryData(key, pages(row("a")));
  client.setQueryData(["sessions", "other"], pages(row("a")));
  cache.receive(changed(row("b", 2)));
  cache.receive(changed(row("c", 3, { metadata: { team: "red" } })));
  cache.receive(changed(row("child", 4, { origin: { parentSessionId: "a" } })));
  expect(rows().map(row => row.id)).toEqual(["b", "a"]);
  cache.receive(changed(row("a", 5)));
  cache.receive(changed(row("a", 1)));
  expect(rows().map(row => row.id)).toEqual(["a", "b"]);
  cache.receive(changed(row("a", 6, { lifecycleStatus: "closed" })));
  cache.receive({ method: "session/removed", params: { sessionId: "b" } });
  expect(rows()).toEqual([]);
  expect(client.getQueryData(["sessions", "other"])).toEqual(pages(row("a")));
});

it("keeps the loaded tail and cursor when a new session enters a paginated range", () => {
  client.setQueryData(key, { pages: [{ sessions: [row("a", 10)], nextCursor: "one" }, { sessions: [row("b", 5)], nextCursor: "two" }], pageParams: [null, "one"] });
  cache.receive(changed(row("c", 20)));
  cache.receive(changed(row("older", 1)));
  expect(rows().map(row => row.id)).toEqual(["c", "a", "b"]);
  expect(client.getQueryData<InfiniteData<SessionListPage>>(key)!.pages[1]!.nextCursor).toBe("two");
});

it("replays changes received while an older list response is in flight", async () => {
  let resolve!: (value: InfiniteData<SessionListPage>) => void;
  client.setQueryData(key, pages(row("a")));
  const request = client.fetchQuery({ queryKey: key, queryFn: () => new Promise<InfiniteData<SessionListPage>>(yes => { resolve = yes; }) });
  cache.receive(changed(row("a", 5)));
  cache.receive(changed(row("b", 6)));
  resolve(pages(row("a", 2)));
  await request;
  expect(rows().map(row => [row.id, row.revision])).toEqual([["b", 6], ["a", 5]]);
});

it("updates workspace names without relabeling an old tree as a new snapshot", () => {
  client.setQueryData(["workspaces", "u"], [workspace(1)]);
  client.setQueryData(["workspace-tree", "u", "w"], tree(1));
  client.setQueryData(["workspace-file", "u", "w", "a", "blob"], { bytesBase64: "old" });
  cache.receive(workspaceChanged(2, "head-1"));
  expect(client.getQueryData<WorkspaceTree>(["workspace-tree", "u", "w"])!.workspace.revision).toBe(2);
  cache.receive(workspaceChanged(3));
  expect(client.getQueryData<WorkspaceTree>(["workspace-tree", "u", "w"])!.workspace.headSnapshotRef).toBe("head-1");
  expect(client.getQueryState(["workspace-tree", "u", "w"])!.isInvalidated).toBe(true);
  expect(client.getQueryState(["workspace-file", "u", "w", "a", "blob"])!.isInvalidated).toBe(false);
  cache.receive(workspaceChanged(1));
  expect(client.getQueryData<WorkspaceRow[]>(["workspaces", "u"])![0]!.revision).toBe(3);
  cache.receive({ method: "workspaces/removed", params: { workspaceId: "w" } });
  expect(client.getQueryData(["workspaces", "u"])).toEqual([]);
});

it("refetches an open tree again when a newer head arrives during its read", async () => {
  let resolve!: (value: WorkspaceTree) => void;
  const queryKey = ["workspace-tree", "u", "w"];
  client.setQueryData(queryKey, tree(1));
  const queryFn = vi.fn().mockImplementationOnce(() => new Promise<WorkspaceTree>(yes => { resolve = yes; })).mockResolvedValue(tree(3));
  const observer = new QueryObserver(client, { queryKey, queryFn });
  const unsubscribe = observer.subscribe(() => {});
  cache.receive(workspaceChanged(3));
  resolve(tree(2));
  await vi.waitFor(() => expect(client.getQueryData<WorkspaceTree>(queryKey)!.workspace.revision).toBe(3));
  expect(queryFn).toHaveBeenCalledTimes(2);
  unsubscribe();
});

it("resets only live row caches in its universe", () => {
  client.setQueryData(key, pages(row("a")));
  client.setQueryData(["workspaces", "u"], [workspace(1)]);
  client.setQueryData(["workspaces", "other"], []);
  client.setQueryData(["workspace-file", "u", "w", "a", "blob"], {});
  cache.receive({ method: "watch/reset", params: {} });
  expect(client.getQueryState(key)!.isInvalidated).toBe(true);
  expect(client.getQueryState(["workspaces", "u"])!.isInvalidated).toBe(true);
  expect(client.getQueryState(["workspaces", "other"])!.isInvalidated).toBe(false);
  expect(client.getQueryState(["workspace-file", "u", "w", "a", "blob"])!.isInvalidated).toBe(false);
});
