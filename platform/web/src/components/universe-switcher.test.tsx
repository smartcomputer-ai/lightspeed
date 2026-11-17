// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, onTestFinished, vi } from "vitest";
import type { Universe } from "@/api";
import { UniverseSwitcher } from "./universe-switcher";
import { SidebarProvider } from "./ui/sidebar";
import { waitForUi } from "@/test/wait-for-ui";

const fixtures = vi.hoisted(() => ({
  // The personal organization's creation state: whether the caller creates there, and whether it has room.
  personal: { actions: [] as string[], canCreateUniverse: false },
  universes: [
    {
      id: "one",
      organizationId: "personal",
      name: "Exploration",
      handle: "explore",
      status: "active",
      role: "owner",
    },
    {
      id: "two",
      organizationId: "team",
      name: "Production",
      handle: "production",
      status: "active",
      role: "viewer",
    },
    {
      id: "old",
      organizationId: "team",
      name: "Retired",
      handle: "retired",
      status: "archived",
      role: "owner",
    },
  ],
}));
vi.mock("@/lib/universes", async (original) => ({
  ...(await original<typeof import("@/lib/universes")>()),
  useUniverses: () => ({ data: fixtures.universes }),
}));
vi.mock("@/lib/platform-api", () => ({
  platformApi: vi.fn(),
  useOrganizations: () => ({
    data: {
      organizations: [
        {
          id: "personal",
          name: "Jane's organization",
          personal: true,
          role: "member",
          ...fixtures.personal,
        },
        {
          id: "team",
          name: "Research",
          personal: false,
          role: "member",
        },
      ],
    },
  }),
}));
vi.mock("@/lib/theme", () => ({ useIsDark: () => false }));
let root: Root, container: HTMLDivElement, client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  }));
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.unstubAllGlobals();
});
function Location() {
  return <output>{useLocation().pathname}</output>;
}
async function show() {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <SidebarProvider>
            <UniverseSwitcher
              active={fixtures.universes[0] as Universe}
            />
            <Location />
          </SidebarProvider>
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  );
  const trigger = container.querySelector<HTMLButtonElement>(
    'button[aria-haspopup="menu"]',
  )!;
  expect(trigger.textContent).toBe("Exploration");
  await act(async () => trigger.click());
  await waitForUi(() =>
    expect(document.querySelector('[role="menu"]')).not.toBeNull(),
  );
}
it("groups all accessible active universes and switches directly across organizations", async () => {
  await show();
  const menu = document.querySelector('[role="menu"]')!;
  expect(menu.textContent).toContain("Personal");
  expect(menu.textContent).toContain("Research");
  expect(menu.textContent).not.toContain("Retired");
  const production = [
    ...menu.querySelectorAll<HTMLElement>('[role="menuitem"]'),
  ].find((item) => item.textContent === "Production")!;
  await act(async () => production.click());
  expect(container.querySelector("output")!.textContent).toBe(
    "/u/production/sessions",
  );
});
it("drops the organization headings when every universe is in one organization", async () => {
  const all = [...fixtures.universes];
  fixtures.universes.splice(0, fixtures.universes.length, ...all.filter((universe) => universe.organizationId === "personal"));
  onTestFinished(() => { fixtures.universes.splice(0, fixtures.universes.length, ...all); });
  await show();
  const menu = document.querySelector('[role="menu"]')!;
  expect(menu.textContent).toContain("Exploration");
  expect(menu.textContent).not.toContain("Personal");
});
it.each([
  [[], false, undefined],
  [["createUniverse"], true, false],
  [["createUniverse"], false, true],
])("offers New universe to a creator (%s), enabled only while an organization has room (%s)", async (actions, room, disabled) => {
  fixtures.personal = { actions, canCreateUniverse: room };
  onTestFinished(() => { fixtures.personal = { actions: [], canCreateUniverse: false }; });
  await show();
  const item = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((node) => node.textContent?.startsWith("New universe"));
  expect(item === undefined ? undefined : item.hasAttribute("data-disabled")).toBe(disabled);
  if (disabled) expect(item!.textContent).toContain("Universe limit reached");
});
it("offers no search while the list is short", async () => {
  await show();
  expect(document.querySelector('[aria-label="Search universes"]')).toBeNull();
});
it("searches universe and organization names once there are more than ten", async () => {
  const base = fixtures.universes.length;
  fixtures.universes.push(...Array.from({ length: 9 }, (_, index) => ({
    id: `extra-${index}`, organizationId: "personal", name: `Extra ${index}`, handle: `extra-${index}`, status: "active", role: "owner",
  })));
  onTestFinished(() => { fixtures.universes.splice(base); });
  await show();
  const input = document.querySelector<HTMLInputElement>(
    '[aria-label="Search universes"]',
  )!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "research");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const menu = document.querySelector('[role="menu"]')!;
  expect(menu.textContent).toContain("Production");
  expect(menu.textContent).not.toContain("Exploration");
});
