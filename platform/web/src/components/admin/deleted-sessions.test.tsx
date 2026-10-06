// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DeletedSessionsDialog } from "./deleted-sessions";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let remaining: string[];
let pageSize: number;
let failed: Set<string>;
let cascades: Record<string, string[]>;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
  remaining = ["s1", "s2"];
  pageSize = 100;
  failed = new Set(); cascades = {};
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method === "GET") {
      const after = new URL(path, "http://test").searchParams.get("after");
      const ids = remaining.filter((id) => !after || id > after).slice(0, pageSize + 1);
      return { sessions: ids.slice(0, pageSize).map((id) => ({ sessionId: id, displayName: `Deleted ${id}`, deletedAtMs: 1 })), nextAfter: ids.length > pageSize ? ids[pageSize - 1] : null };
    }
    const id = decodeURIComponent(path.split("/").at(-2)!);
    if (failed.has(id)) throw new Error(`Could not purge ${id}`);
    const removed = remaining.filter((candidate) => (cascades[id] ?? [id]).includes(candidate));
    remaining = remaining.filter((candidate) => !removed.includes(candidate));
    return { deletedSessionIds: removed };
  });
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear(); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals();
});
async function settle() {
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent === label)!;
const purgeCalls = () => mocks.api.mock.calls.filter(([method]) => method === "POST");
async function show(id = "u") {
  await act(async () => root.render(<QueryClientProvider client={client}><DeletedSessionsDialog universe={{ id, name: "Factory" }} onClose={() => undefined} /></QueryClientProvider>));
  await settle();
}
async function click(label: string) {
  await act(async () => button(label).click());
  await settle();
}
async function select(id: string) {
  await act(async () => document.querySelector<HTMLButtonElement>(`[aria-label="Select session ${id}"]`)!.click());
}
it("purges only selected sessions across pages after a separate confirmation", async () => {
  remaining = ["s1", "s2", "s3"]; pageSize = 2;
  await show();
  expect(button("Purge selected…").disabled).toBe(true);
  await select("s1");
  await click("Load more");
  await select("s3");
  await click("Purge selected…");
  expect(document.body.textContent).toContain("2 selected sessions");
  expect(document.body.textContent).toContain("This cannot be undone");
  expect(purgeCalls()).toEqual([]);
  await click("Purge selected");
  expect(purgeCalls()).toEqual([
    ["POST", "/api/v1/admin/universes/u/sessions/s1/purge", {}],
    ["POST", "/api/v1/admin/universes/u/sessions/s3/purge", {}],
  ]);
  expect(remaining).toEqual(["s2"]);
});
it("purge all captures every page before confirmation and excludes later arrivals", async () => {
  pageSize = 1;
  await show();
  expect(document.querySelector('[aria-label="Select session s2"]')).toBeNull();
  await click("Purge all…");
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/v1/admin/universes/u/deleted-sessions?after=s1");
  expect(document.body.textContent).toContain("2 selected sessions");
  expect(purgeCalls()).toEqual([]);
  remaining.push("s3");
  await click("Purge all");
  expect(purgeCalls().map(([, path]) => path)).toEqual([
    "/api/v1/admin/universes/u/sessions/s1/purge", "/api/v1/admin/universes/u/sessions/s2/purge",
  ]);
  expect(remaining).toEqual(["s3"]);
});
it("does not offer a partial purge-all selection when collecting a page fails", async () => {
  pageSize = 1;
  await show();
  mocks.api.mockResolvedValueOnce({ sessions: [{ sessionId: "s1" }], nextAfter: "s1" }).mockRejectedValueOnce(new Error("Page unavailable"));
  await click("Purge all…");
  expect(document.querySelector('[role="alert"]')?.textContent).toBe("Page unavailable");
  expect(button("Purge all")).toBeUndefined();
  expect(purgeCalls()).toEqual([]);
});
it("retries only failures after partially successful deletion", async () => {
  failed.add("s2");
  await show();
  await click("Purge all…");
  await click("Purge all");
  expect(document.querySelector('[role="status"]')?.textContent).toContain("Permanently deleted 1 session. 1 failed");
  expect(document.querySelector('[role="alert"]')?.textContent).toContain("Could not purge s2");
  failed.clear();
  await click("Retry failed");
  expect(purgeCalls().map(([, path]) => path)).toEqual([
    "/api/v1/admin/universes/u/sessions/s1/purge", "/api/v1/admin/universes/u/sessions/s2/purge", "/api/v1/admin/universes/u/sessions/s2/purge",
  ]);
  expect(remaining).toEqual([]);
});
it("skips selected descendants already removed by a parent's purge", async () => {
  cascades.s1 = ["s1", "s2"];
  await show();
  await click("Purge all…");
  await click("Purge all");
  expect(purgeCalls()).toHaveLength(1);
  expect(document.querySelector('[role="status"]')?.textContent).toBe("Permanently deleted 2 sessions.");
});
it("resets the selection when switching universes", async () => {
  await show(); await select("s1");
  expect(button("Purge selected…").disabled).toBe(false);
  await show("another-universe");
  expect(button("Purge selected…").disabled).toBe(true);
  expect(purgeCalls()).toEqual([]);
});
it("disables both purge actions when no deleted sessions exist", async () => {
  remaining = [];
  await show();
  expect(document.body.textContent).toContain("No deleted sessions.");
  expect(button("Purge all…").disabled).toBe(true);
  expect(button("Purge selected…").disabled).toBe(true);
});
