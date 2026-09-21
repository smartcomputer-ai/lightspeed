// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionsPage } from "./SessionsPage";
import { PermissionIdentityProvider } from "@/lib/permissions";
import type { AccessReadResponse, ResourceRef } from "@lightspeed-ai/agent-client";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "contributor" }, slug: "universe", isLoading: false }) }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let contributor: boolean;
const sessions = ["own", "other"].map((id) => ({ id, displayName: id, lifecycleStatus: "open", managed: false, createdAtMs: 0, updatedAtMs: 0, retention: { rootSessionId: id } }));
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  window.localStorage.clear();
  contributor = true;
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string, body?: { resources: ResourceRef[] }) => {
    if (path.endsWith("/access")) return {
      actions: contributor ? ["read", "create_session"] : ["read"],
      resources: (body?.resources ?? []).map((resource) => ({ resource, actions: contributor && resource.id === "own" ? ["read", "control_session", "stop_session", "delete_session"] : ["read"] })),
    } as AccessReadResponse;
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
  await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter><SessionsPage admin={false} /></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>));
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
it("keeps the session list readable without showing create or bulk controls to a viewer", async () => {
  contributor = false;
  await show();
  expect(container.textContent).toContain("own");
  expect(container.textContent).toContain("other");
  expect(container.querySelector('[aria-label="New session"]')).toBeNull();
  expect(container.querySelector('[aria-label="Select sessions"]')).toBeNull();
});
it("offers bulk actions only for the permitted sessions in a mixed selection", async () => {
  await show();
  expect(container.querySelector('[aria-label="New session"]')).not.toBeNull();
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Select sessions"]')!.click());
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Select all listed sessions"]')!.click());
  expect(container.textContent).toContain("2 selected · 1 actionable");
  expect([...container.querySelectorAll("button")].some((button) => button.textContent === "Close 1")).toBe(true);
  const deleteButton = [...container.querySelectorAll("button")].find((button) => button.textContent === "Delete 0");
  expect(deleteButton?.disabled).toBe(true);
});
