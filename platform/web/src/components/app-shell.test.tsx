// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { organizationActions, universeActions } = await vi.hoisted(() => import("@lightspeed-ai/platform-shared"));
import type { SessionUser } from "@/auth";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { UserPreferencesProvider } from "@/lib/user-preferences";
import { AppShell, shellTitleTrail } from "./app-shell";
import { documentTitle } from "@/lib/document-title";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "viewer" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", async (original) => ({
  ...await original<typeof import("@/lib/universes")>(),
  rememberUniverse: () => {},
}));
vi.mock("@/components/universe-switcher", () => ({ UniverseSwitcher: () => null }));
vi.mock("@/components/user-menu", () => ({ UserMenu: () => null }));

const WORK = ["Bots", "Sessions", "Workspaces"];
const SETUP = ["Profiles", "Environments", "MCP servers", "Models"];

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let personalOrganization = false;
beforeEach(() => {
  personalOrganization = false;
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
async function sidebarFor(role: string, features = { bots: true, channels: true }, platformAdmin = false, adminPath?: string, organizationRole?: string): Promise<Record<string, string[]>> {
  mocks.role = role;
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path === "/api/v1/platform/organizations") {
      // The server's view: the caller's own role, plus Admin's actions for a Platform Admin.
      const roles = [organizationRole ?? "member", ...(platformAdmin ? ["admin"] : [])] as never[];
      return { organizations: [{ id: "team", name: "Team", personal: personalOrganization, role: organizationRole ?? "member", actions: [...organizationActions(roles)] }], canCreate: false };
    }
    if (path === "/api/ui/universes") return [{ id: "universe", organizationId: "team", handle: "test", name: "Test", status: "active", role: mocks.role, actions: [...universeActions([mocks.role as never])], features }];
    throw new Error(`Unexpected request: ${path}`);
  });
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}>
      <MemoryRouter initialEntries={[adminPath ?? (platformAdmin ? "/admin/platform/audit" : "/u/test/bots")]}>
        <Routes>
          <Route element={<AppShell user={{ id: "user", name: "User", email: "user@example.test" } as SessionUser} />}>
            <Route path="/admin/organizations/:organizationId/*" element={null} /><Route path="*" element={null} />
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
  });
});

it("adds credentials, channels and templates for an Operator", async () => {
  expect(await sidebarFor("operator")).toEqual({
    "": WORK,
    Setup: [...SETUP, "Credentials"],
    Settings: ["Channels", "Templates"],
  });
});

it("gives an Owner the universe's own settings and members in its menu", async () => {
  expect(await sidebarFor("owner")).toEqual({
    "": WORK,
    Setup: [...SETUP, "Credentials"],
    Settings: ["General", "Members", "Channels", "Templates"],
  });
});

it("keeps settings and members out of an Operator's menu", async () => {
  expect(await sidebarFor("operator")).toMatchObject({ Settings: ["Channels", "Templates"] });
});

it("leaves switched-off channels out of the menu", async () => {
  expect(await sidebarFor("owner", { bots: true, channels: false })).toMatchObject({
    "": WORK,
    Settings: ["General", "Members", "Templates"],
  });
});

it("leaves bots, and the channels they need, out of the menu", async () => {
  expect(await sidebarFor("owner", { bots: false, channels: false })).toMatchObject({
    "": ["Sessions", "Workspaces"],
    Settings: ["General", "Members", "Templates"],
  });
});

it("titles the tab with the page and its universe, read backwards", async () => {
  await sidebarFor("owner");
  expect(document.title).toBe(documentTitle(["Bots", "Test"]));
});

it.each([
  ["/u/test/sessions", ["Sessions", "Test"]],
  ["/u/test/sessions/s-1", ["Sessions", "Test"]],
  ["/u/test/workspaces/w-1/files/src/main.rs", ["Workspaces", "Test"]],
  ["/u/test/mcp-servers", ["MCP servers", "Test"]],
  ["/u/test/blobs/abc", ["Test"]],
  ["/admin/platform/audit", ["Audit log", "Platform administration"]],
  ["/admin/platform/environment-providers", ["Environment providers", "Platform administration"]],
  ["/account", ["Your profile"]],
  ["/admin/organizations/team/settings", ["Settings", "Admin"]],
  ["/preferences", ["Preferences"]],
  ["/accounts-elsewhere", []],
  ["/", []],
])("names %s from the menu", (path, trail) => {
  expect(shellTitleTrail(path, "Test")).toEqual(trail);
});

it("leaves the universe out until it has loaded", () => {
  expect(shellTitleTrail("/u/test/sessions", undefined)).toEqual(["Sessions"]);
});

it("links each page at its flat route", async () => {
  await sidebarFor("owner");
  expect([...container.querySelectorAll("a")].map((link) => link.getAttribute("href"))).toEqual([
    "/u/test/bots", "/u/test/sessions", "/u/test/workspaces",
    "/u/test/profiles", "/u/test/environments", "/u/test/mcp-servers", "/u/test/models",
    "/u/test/credentials",
    "/u/test/settings/general", "/u/test/settings/members", "/u/test/settings/channels", "/u/test/settings/templates",
  ]);
});

it("keeps the menu folded to icons from the account's preference, and ⌘B unfolds it", async () => {
  const key = "lightspeed:user-preferences:user";
  localStorage.setItem(key, JSON.stringify({ sidebarCollapsed: true }));
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path === "/api/ui/universes") return [{ id: "universe", organizationId: "team", handle: "test", name: "Test", status: "active", role: "viewer", actions: [...universeActions(["viewer"])] }];
    throw new Error(`Unexpected request: ${path}`);
  });
  await act(async () => root.render(
    <QueryClientProvider client={client}><UserPreferencesProvider userId="user"><PermissionIdentityProvider userId="user" platformAdmin={false}>
      <MemoryRouter initialEntries={["/u/test/bots"]}>
        <Routes>
          <Route element={<AppShell user={{ id: "user", name: "User", email: "user@example.test" } as SessionUser} />}>
            <Route path="/admin/organizations/:organizationId/*" element={null} /><Route path="*" element={null} />
          </Route>
        </Routes>
      </MemoryRouter>
    </PermissionIdentityProvider></UserPreferencesProvider></QueryClientProvider>,
  ));
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  const sidebar = () => container.querySelector<HTMLElement>('[data-slot="sidebar"]')!;
  expect(sidebar().dataset.state).toBe("collapsed");
  expect(sidebar().dataset.collapsible).toBe("icon");
  await act(async () => window.dispatchEvent(new KeyboardEvent("keydown", { key: "b", metaKey: true })));
  expect(sidebar().dataset.state).toBe("expanded");
  expect(JSON.parse(localStorage.getItem(key)!).sidebarCollapsed).toBe(false);
  localStorage.removeItem(key);
});

it("offers Audit log in Platform admin navigation", async () => {
  const groups = await sidebarFor("owner", { bots: true, channels: true }, true);
  expect(groups["Platform"]).toContain("Audit log");
  expect(container.querySelector('[data-sidebar="separator"]')).not.toBeNull();
  expect(container.querySelector('a[href="/admin/platform/audit"]')?.getAttribute("aria-current")).toBe("page");
});

it("puts Back and the Organizations heading above the organization selector", async () => {
  await sidebarFor("owner", undefined, false, "/admin/organizations/team/universes", "admin");
  const header = container.querySelector('[data-sidebar="header"]')!;
  const back = header.querySelector('a[href="/"]')!;
  const heading = header.querySelector("h2")!;
  const selector = header.querySelector('button[aria-haspopup="menu"]')!;
  expect(back.textContent).toBe("Back to universes");
  expect(heading.textContent).toBe("Organizations");
  expect(selector.textContent).toBe("Team");
  expect(back.compareDocumentPosition(heading) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(heading.compareDocumentPosition(selector) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

it("opens a personal-key deep link and keeps profile section links navigable", async () => {
  mocks.api.mockReset().mockImplementation(async (_method, path) => path === "/api/v1/platform/organizations" ? { organizations: [] } : []);
  const scroll = vi.fn();
  const previous = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView");
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { configurable: true, value: scroll });
  try {
    await act(async () => root.render(
      <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}>
        <MemoryRouter initialEntries={["/account#api-keys"]}><Routes>
          <Route element={<AppShell user={{ id: "user", name: "User", email: "user@example.test" } as SessionUser} />}>
            <Route path="account" element={<><div id="profile" /><div id="security" /><div id="api-keys" /></>} />
          </Route>
        </Routes></MemoryRouter>
      </PermissionIdentityProvider></QueryClientProvider>,
    ));
    expect(scroll.mock.contexts).toContain(container.querySelector("#api-keys"));
    const signIn = container.querySelector<HTMLAnchorElement>('a[href="/account#security"]')!;
    expect(signIn.textContent).toBe("Sign-in");
    await act(async () => signIn.click());
    expect(scroll.mock.contexts.at(-1)).toBe(container.querySelector("#security"));
    expect(signIn.hasAttribute("data-active")).toBe(true);
  } finally {
    if (previous) Object.defineProperty(HTMLElement.prototype, "scrollIntoView", previous);
    else delete (HTMLElement.prototype as Partial<HTMLElement>).scrollIntoView;
  }
});


it("shows only universe administration to a universe Owner", async () => {
  expect(await sidebarFor("owner", undefined, false, "/admin/organizations/team/universes")).toEqual({ "": ["Universes"] });
  expect(container.querySelector('[data-sidebar="separator"]')).toBeNull();
});

it("shows a personal organization the same administration sections", async () => {
  personalOrganization = true;
  expect(await sidebarFor("viewer", undefined, false, "/admin/organizations/team/settings", "admin")).toEqual({ "": ["Universes", "Members", "API keys", "Settings"] });
});

it("shows organization administration without deployment controls to an Organization Admin", async () => {
  expect(await sidebarFor("viewer", undefined, false, "/admin/organizations/team/settings", "admin")).toEqual({ "": ["Universes", "Members", "API keys", "Settings"] });
  expect(container.querySelector('[data-sidebar="separator"]')).toBeNull();
});

it("does not offer administrative pages to a Creator with no owned universe", async () => {
  expect(await sidebarFor("viewer", undefined, false, "/admin", "creator")).toEqual({});
});
