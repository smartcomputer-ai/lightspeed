// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PlatformOrganizationsPage } from "./PlatformOrganizationsPage";

const organization = (id: string, name: string, overrides: Record<string, unknown> = {}) => ({
  id, name, handle: id, personal: false, role: "admin", universeQuota: 25, universeCount: 3,
  defaultProtected: true, requireProtected: false, createdAt: "2026-11-01T00:00:00.000Z", ...overrides,
});
const mocks = vi.hoisted(() => ({
  loading: false,
  canCreate: false,
  organizations: [] as unknown[],
}));
vi.mock("@/lib/platform-api", () => ({
  platformApi: vi.fn(),
  useOrganizations: () => ({
    isLoading: mocks.loading,
    data: mocks.loading ? undefined : { canCreate: mocks.canCreate, organizations: mocks.organizations },
  }),
}));

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.loading = false;
  mocks.canCreate = false;
  mocks.organizations = [
    organization("research", "Research"),
    organization("acme", "Acme", { universeCount: 25 }),
    organization("personal-ada", "Ada's organization", { personal: true }),
  ];
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

function Location() {
  return <output data-testid="location">{useLocation().pathname}</output>;
}
async function show() {
  await act(async () =>
    root.render(
      <MemoryRouter initialEntries={["/admin/platform/organizations"]}>
        <QueryClientProvider client={new QueryClient()}>
          <Routes><Route path="*" element={<><PlatformOrganizationsPage /><Location /></>} /></Routes>
        </QueryClientProvider>
      </MemoryRouter>,
    ),
  );
}
const rows = () => [...container.querySelectorAll("tbody tr")];
const button = (name: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === name)!;

it("lists team organizations by name with universe usage, and no inline actions", async () => {
  await show();
  expect(rows().map((row) => row.querySelector("td span[title]")?.getAttribute("title"))).toEqual(["Acme", "Research"]);
  expect(rows()[0]!.textContent).toContain("25 / 25");
  expect(rows()[1]!.textContent).toContain("3 / 25");
  expect(container.querySelector("tbody button")).toBeNull();
  expect(container.textContent).not.toContain("Ada's organization");
});

it("keeps personal organizations behind their own filter", async () => {
  await show();
  await act(async () => button("Personal").click());
  expect(rows()).toHaveLength(1);
  expect(rows()[0]!.textContent).toContain("Ada's organization");
});

it("opens an organization's universes from anywhere on its row", async () => {
  await show();
  expect(container.querySelector('a[aria-label="Open Research"]')?.getAttribute("href")).toBe("/admin/organizations/research/universes");
  await act(async () => (rows()[1]!.querySelector("td") as HTMLElement).click());
  expect(container.querySelector('[data-testid="location"]')?.textContent).toBe("/admin/organizations/research/universes");
});

it("omits the filter when the deployment has no personal organizations", async () => {
  mocks.organizations = [organization("research", "Research")];
  await show();
  expect(container.querySelector('[aria-label="Organization type"]')).toBeNull();
});

it("distinguishes loading from an empty inventory", async () => {
  mocks.loading = true;
  await show();
  expect(container.textContent).not.toContain("No team organizations yet");
  mocks.loading = false;
  mocks.organizations = [];
  await show();
  expect(container.textContent).toContain("No team organizations yet");
});

it("offers a new organization when creation is permitted", async () => {
  mocks.canCreate = true;
  await show();
  await act(async () => button("New organization").click());
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain("You become its first Organization Admin");
});
