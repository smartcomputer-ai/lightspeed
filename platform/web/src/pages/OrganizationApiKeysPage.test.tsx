// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { organizationActions } = await vi.hoisted(() => import("@lightspeed-ai/platform-shared"));
import { OrganizationApiKeysPage } from "./OrganizationApiKeysPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "admin" }));
vi.mock("@/lib/platform-api", () => ({
  platformApi: mocks.api,
  useOrganizations: () => ({ data: { organizations: [{ id: "team", name: "Team", role: mocks.role, actions: [...organizationActions([mocks.role as never])] }] } }),
}));
vi.mock("@/lib/universes", () => ({ useUniverses: () => ({ data: [] }) }));
const key = (id: string, owner: unknown) => ({
  id, name: `Key ${id}`, prefix: "lsp_", start: `lsp_${id}`, enabled: true, createdAt: "2026-11-01T00:00:00.000Z",
  expiresAt: null, lastUsedAt: null, restrictions: {}, owner,
});

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener: () => {}, removeEventListener: () => {} }));
  mocks.role = "admin";
  mocks.api.mockReset().mockResolvedValue({ credentials: [
    key("person", { kind: "user", id: "ada", name: "Ada", email: "ada@example.test" }),
    key("robot", { kind: "serviceUser", id: "robot", name: "Deployer", enabled: true }),
  ] });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
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
async function show() {
  await act(async () => root.render(
    <MemoryRouter initialEntries={["/admin/organizations/team/api-keys"]}>
      <QueryClientProvider client={client}>
        <Routes><Route path="/admin/organizations/:organizationId/api-keys" element={<OrganizationApiKeysPage />} /></Routes>
      </QueryClientProvider>
    </MemoryRouter>,
  ));
}
const rows = () => [...container.querySelectorAll("tbody tr")];
const openRow = (index: number) => act(async () => (rows()[index]!.querySelector("td") as HTMLElement).click());
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const closeDialog = () => act(async () => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));

it("lists every key that reaches the organization with its owner, and explains where keys are made", async () => {
  await show();
  await waitForUi(() => expect(rows()).toHaveLength(2));
  expect(mocks.api).toHaveBeenCalledWith("platform/organizations/credentials/list", { organizationId: "team" });
  expect(rows()[0]!.textContent).toContain("ada@example.test");
  expect(rows()[1]!.textContent).toContain("Service user");
  expect(container.textContent).toContain("Keys are created by their owners");
  expect(container.querySelector('a[href="/admin/organizations/team/members"]')).not.toBeNull();
  expect([...container.querySelectorAll("button")].some((node) => node.textContent?.includes("New key"))).toBe(false);
});

it("lets an administrator revoke a service user's key but not a person's", async () => {
  await show();
  await waitForUi(() => expect(rows()).toHaveLength(2));
  await openRow(0);
  expect(dialog().textContent).toContain("Only Ada can revoke this key");
  expect([...dialog().querySelectorAll("button")].some((node) => node.textContent?.trim() === "Revoke key")).toBe(false);
  await closeDialog();
  await waitForUi(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
  await openRow(1);
  expect([...dialog().querySelectorAll("button")].some((node) => node.textContent?.trim() === "Revoke key")).toBe(true);
});

it("refuses an organization the caller does not administer without fetching keys", async () => {
  mocks.role = "member";
  await show();
  expect(container.querySelector('[role="alert"]')?.textContent).toContain("unavailable");
  expect(mocks.api).not.toHaveBeenCalled();
});
