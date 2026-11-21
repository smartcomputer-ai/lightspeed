import { afterEach, expect, it, vi } from "vitest";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import type { AgentNotification } from "@lightspeed-ai/sdk";
import type { WorkspaceRow, WorkspaceTree } from "@/api";
import { WatchCache } from "./watch-cache";

const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
let cache = new WatchCache(client, "u");
afterEach(() => { cache.dispose(); client.clear(); cache = new WatchCache(client, "u"); });
const workspace = (revision: number, head = `head-${revision}`): WorkspaceRow => ({ workspaceId: "w", revision, headSnapshotRef: head,
  files: 1, bytes: 1, createdAtMs: 1, updatedAtMs: revision });
const tree = (revision: number): WorkspaceTree => ({ workspace: workspace(revision), manifest: { schema_version: "1", root: { entries: {} }, totals: { files: 0, bytes: 0 } } } as WorkspaceTree);
const workspaceChanged = (revision: number, head?: string): AgentNotification => ({ method: "workspaces/changed", params: { workspace: workspace(revision, head) } });

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
  client.setQueryData(["workspaces", "u"], [workspace(1)]);
  client.setQueryData(["workspaces", "other"], []);
  client.setQueryData(["workspace-file", "u", "w", "a", "blob"], {});
  cache.receive({ method: "watch/reset", params: {} });
  expect(client.getQueryState(["workspaces", "u"])!.isInvalidated).toBe(true);
  expect(client.getQueryState(["workspaces", "other"])!.isInvalidated).toBe(false);
  expect(client.getQueryState(["workspace-file", "u", "w", "a", "blob"])!.isInvalidated).toBe(false);
});
