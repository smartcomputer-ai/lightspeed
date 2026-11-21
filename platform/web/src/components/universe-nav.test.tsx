// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { organizationActions, universeActions } = await vi.hoisted(() => import("@lightspeed-ai/platform-shared"));
import type { UniverseAction } from "@lightspeed-ai/platform-shared";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { SettingsIndexRedirect, settingsIndexPath } from "./universe-nav";
import { settleQueries } from "@/test/settle-queries";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
const ALL_ON = vi.hoisted(() => ({ bots: true }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", async (original) => ({
  ...await original<typeof import("@/lib/universes")>(),
  useActiveUniverse: () => ({ universe: { id: "universe", handle: "test", name: "Test", role: mocks.role, actions: [...universeActions([mocks.role as never])], features: ALL_ON }, handle: "test", isLoading: false }),
}));

const ROLE_ACTIONS: Record<string, UniverseAction[]> = {
  viewer: ["read"],
  contributor: ["read", "createSession", "useResource"],
  operator: ["read", "createSession", "useResource", "configureResource"],
  owner: ["read", "createSession", "useResource", "configureResource", "administer"],
};
const DESTINATIONS: [string, string][] = [
  ["viewer", "/u/test"],
  ["contributor", "/u/test"],
  ["operator", "/u/test"],
  ["owner", "/u/test/settings/general"],
];

it.each(DESTINATIONS)("resolves bare settings for a %s to %s", (role, path) => {
  expect(settingsIndexPath({ handle: "test" }, (action) => ROLE_ACTIONS[role]!.includes(action))).toBe(path);
});

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
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

function Location() {
  return <span data-testid="location">{useLocation().pathname}</span>;
}

it.each(DESTINATIONS)("redirects a %s from /settings to %s", async (role, path) => {
  mocks.role = role;
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}>
      <MemoryRouter initialEntries={["/u/test/settings"]}>
        <Routes>
          <Route path="u/:handle/settings" element={<SettingsIndexRedirect />} />
          <Route path="*" element={<Location />} />
        </Routes>
      </MemoryRouter>
    </PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settleQueries(client);
  expect(container.querySelector('[data-testid="location"]')?.textContent).toBe(path);
});
