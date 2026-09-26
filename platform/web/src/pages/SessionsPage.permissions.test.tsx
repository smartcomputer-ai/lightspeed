// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionsPage } from "./SessionsPage";
import { PermissionIdentityProvider } from "@/lib/permissions";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "contributor" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, slug: "universe", isLoading: false }) }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const sessions = [
  { id: "own", access: { visibility: "restricted", createdBy: { kind: "actor", id: "user" } } },
  { id: "other", access: { visibility: "universe", createdBy: { kind: "actor", id: "someone" } } },
].map((session) => ({ ...session, displayName: session.id, lifecycleStatus: "open", managed: false, createdAtMs: 0, updatedAtMs: 0, retention: { rootSessionId: session.id } }));
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  window.localStorage.clear();
  mocks.role = "contributor";
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.includes("/sessions?")) return { sessions };
    throw new Error(`Unexpected request: ${path}`);
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter><SessionsPage admin={false} /></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>));
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
it("keeps the session list readable without showing create or bulk controls to a viewer", async () => {
  mocks.role = "viewer";
  await show();
  expect(container.textContent).toContain("own");
  expect(container.textContent).toContain("other");
  expect(container.querySelector('[aria-label="New session"]')).toBeNull();
  expect(container.querySelector('[aria-label="Select sessions"]')).toBeNull();
});
it("marks unshared work in the list", async () => {
  await show();
  const rows = [...container.querySelectorAll("a")].filter((link) => link.textContent?.includes("own") || link.textContent?.includes("other"));
  expect(rows.find((row) => row.textContent?.includes("own"))?.textContent).toContain("Unshared");
  expect(rows.find((row) => row.textContent?.includes("other"))?.textContent).not.toContain("Unshared");
});
it("offers bulk actions to a contributor over the listed sessions", async () => {
  await show();
  expect(container.querySelector('[aria-label="New session"]')).not.toBeNull();
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Select sessions"]')!.click());
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Select all listed sessions"]')!.click());
  expect(container.textContent).toContain("2 selected");
  expect([...container.querySelectorAll("button")].some((button) => button.textContent === "Close 2")).toBe(true);
});
