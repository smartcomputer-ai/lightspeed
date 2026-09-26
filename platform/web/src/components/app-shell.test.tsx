// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionUser } from "@/auth";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { AppShell } from "./app-shell";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", async (original) => ({
  ...await original<typeof import("@/lib/universes")>(),
  rememberUniverse: () => {},
}));
vi.mock("@/components/universe-switcher", () => ({ UniverseSwitcher: () => null }));
vi.mock("@/components/user-menu", () => ({ UserMenu: () => null }));

const WORK = ["Bots", "Sessions"];
const SETUP = ["Profiles", "Workspaces", "Models", "Environments", "MCP servers"];

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
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

/// The sidebar as label → nav items, with the unlabelled first group as "".
async function sidebarFor(role: string): Promise<Record<string, string[]>> {
  mocks.role = role;
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path === "/api/v1/universes") return [{ id: "universe", slug: "test", name: "Test", status: "active", role: mocks.role }];
    throw new Error(`Unexpected request: ${path}`);
  });
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}>
      <MemoryRouter initialEntries={["/u/test/bots"]}>
        <Routes>
          <Route element={<AppShell user={{ id: "user", name: "User", email: "user@example.test" } as SessionUser} admin={false} />}>
            <Route path="*" element={null} />
          </Route>
        </Routes>
      </MemoryRouter>
    </PermissionIdentityProvider></QueryClientProvider>,
  ));
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  const groups = [...container.querySelectorAll('[data-sidebar="group"]')];
  return Object.fromEntries(groups.map((group) => [
    group.querySelector('[data-sidebar="group-label"]')?.textContent ?? "",
    [...group.querySelectorAll("a")].map((link) => link.textContent ?? ""),
  ]));
}

it.each(["viewer", "contributor"])("shows a %s the work, the setup and readable access pages", async (role) => {
  expect(await sidebarFor(role)).toEqual({
    "": WORK,
    Setup: SETUP,
    Access: ["Members"],
  });
});

it("adds credentials, channels and templates for an Operator", async () => {
  expect(await sidebarFor("operator")).toEqual({
    "": WORK,
    Setup: SETUP,
    Access: ["Credentials", "Members"],
    Settings: ["Channels", "Templates"],
  });
});

it("adds API keys and general settings for an Admin", async () => {
  expect(await sidebarFor("admin")).toEqual({
    "": WORK,
    Setup: SETUP,
    Access: ["Credentials", "API keys", "Members"],
    Settings: ["General", "Channels", "Templates"],
  });
});

it("links each page at its flat route", async () => {
  await sidebarFor("admin");
  expect([...container.querySelectorAll("a")].map((link) => link.getAttribute("href"))).toEqual([
    "/u/test/bots", "/u/test/sessions",
    "/u/test/profiles", "/u/test/workspaces", "/u/test/models", "/u/test/environments", "/u/test/mcp-servers",
    "/u/test/credentials", "/u/test/api-keys", "/u/test/members",
    "/u/test/settings/general", "/u/test/settings/channels", "/u/test/settings/templates",
  ]);
});
