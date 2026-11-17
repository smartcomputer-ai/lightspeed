// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Universe } from "@/api";
import { UniverseSwitcher } from "./universe-switcher";
import { SidebarProvider } from "./ui/sidebar";
import { waitForUi } from "@/test/wait-for-ui";

const fixtures = vi.hoisted(() => ({
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
          kind: "personal",
          enabled: true,
          role: "member",
        },
        {
          id: "team",
          name: "Research",
          kind: "shared",
          enabled: true,
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
              admin={false}
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
  expect(trigger.textContent).toContain("ExplorationPersonal");
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
it("searches universe and organization names", async () => {
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
