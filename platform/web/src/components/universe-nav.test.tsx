// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider, type PermissionAction } from "@/lib/permissions";
import { SettingsIndexRedirect, settingsIndexPath } from "./universe-nav";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", async (original) => ({
  ...await original<typeof import("@/lib/universes")>(),
  useActiveUniverse: () => ({ universe: { id: "universe", handle: "test", name: "Test", role: mocks.role }, handle: "test", isLoading: false }),
}));

const ROLE_ACTIONS: Record<string, PermissionAction[]> = {
  viewer: ["read"],
  contributor: ["read", "createSession", "useResource"],
  operator: ["read", "createSession", "useResource", "configureResource"],
  admin: ["read", "createSession", "useResource", "configureResource", "manageAccess"],
};
const DESTINATIONS: [string, string][] = [
  ["viewer", "/u/test/bots"],
  ["contributor", "/u/test/bots"],
  ["operator", "/u/test/settings/channels"],
  ["admin", "/u/test/settings/general"],
];

it.each(DESTINATIONS)("resolves bare settings for a %s to %s", (role, path) => {
  expect(settingsIndexPath({ handle: "test" }, (action) => ROLE_ACTIONS[role]!.includes(action))).toBe(path);
});

it("skips a switched-off settings page, and lands on sessions when bots are off", () => {
  const operator = (action: PermissionAction) => ROLE_ACTIONS.operator!.includes(action);
  expect(settingsIndexPath({ handle: "test", features: { bots: true, channels: false } }, operator))
    .toBe("/u/test/settings/templates");
  const viewer = (action: PermissionAction) => ROLE_ACTIONS.viewer!.includes(action);
  expect(settingsIndexPath({ handle: "test", features: { bots: false, channels: false } }, viewer))
    .toBe("/u/test/sessions");
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
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(container.querySelector('[data-testid="location"]')?.textContent).toBe(path);
});
