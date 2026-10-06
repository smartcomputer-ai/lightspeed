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
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  mocks.api.mockReset().mockResolvedValue({ sessions: [{ sessionId: "s1", displayName: "Deleted work", deletedAtMs: 1 }], nextAfter: null });
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear(); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals();
});
async function settle() {
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent === label)!;
async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}><DeletedSessionsDialog universe={{ id: "u", name: "Factory" }} onClose={() => undefined} /></QueryClientProvider>));
  await settle();
}
it("lists deleted sessions and requires confirmation before permanent deletion", async () => {
  await show();
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/v1/admin/universes/u/deleted-sessions");
  await act(async () => button("Permanently delete…").click());
  expect(document.body.textContent).toContain("This cannot be undone");
  expect(mocks.api.mock.calls.some(([method]) => method === "POST")).toBe(false);
  await act(async () => button("Permanently delete").click());
  await settle();
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/admin/universes/u/sessions/s1/purge", {});
});
it("preserves the confirmation and shows failed permanent deletion", async () => {
  await show();
  mocks.api.mockRejectedValue(new Error("session must be soft-deleted first"));
  await act(async () => button("Permanently delete…").click());
  await act(async () => button("Permanently delete").click());
  await settle();
  expect(document.querySelector('[role="alert"]')?.textContent).toBe("session must be soft-deleted first");
  expect(button("Permanently delete")).toBeDefined();
});
