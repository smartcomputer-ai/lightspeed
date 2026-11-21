import { afterEach, beforeEach, expect, it } from "vitest";
import { QueryClient, type InfiniteData } from "@tanstack/react-query";
import type { AgentNotification, SessionSummaryView } from "@lightspeed-ai/sdk";
import type { SessionListPage } from "@lightspeed-ai/platform-web/modules/client";
import { WatchCache } from "@/lib/watch-cache";
import { rosterQuery } from "./roster-query";

const client = new QueryClient();
let cache: WatchCache;
beforeEach(() => {
  cache = new WatchCache(client, "u");
  client.setQueryDefaults(key, { meta: roster.meta });
});
afterEach(() => { cache.dispose(); client.clear(); });
const roster = rosterQuery("u", { team: "blue" }, { showClosed: false, showSubagents: false, showControlledSessions: false });
const key = roster.queryKey;
const row = (id: string, revision = 1, extra = {}): SessionSummaryView => ({
  id, revision, createdAtMs: 1, updatedAtMs: revision, lifecycleStatus: "open", metadata: { team: "blue" },
  activity: "idle", access: { visibility: "universe" }, retention: { rootSessionId: id }, lifecycleControlled: false, ...extra,
} as SessionSummaryView);
const changed = (session: SessionSummaryView): AgentNotification => ({ method: "session/changed", params: { session } });
const pages = (...rows: SessionSummaryView[]): InfiniteData<SessionListPage> => ({ pages: [{ sessions: rows, nextCursor: null }], pageParams: [null] });
const rows = () => client.getQueryData<InfiniteData<SessionListPage>>(key)!.pages.flatMap(page => page.sessions);

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
