// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { organizationActions, universeActions } = await vi.hoisted(() => import("@lightspeed-ai/platform-shared"));
import { PermissionIdentityProvider } from "@/lib/permissions";
import { selectOption } from "@/test/select-option";
import { UniverseMembersPage } from "./UniverseMembersPage";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "owner", organizationRole: "member", base: "viewer" as string | null }));
vi.mock("@/lib/platform-api", () => ({
  platformApi: mocks.api,
  useOrganizations: () => ({ data: { organizations: [{
    id: "team", name: "Acme", role: mocks.organizationRole, baseUniverseRole: mocks.base,
    actions: [...organizationActions([mocks.organizationRole as never])],
  }] } }),
}));
vi.mock("@/lib/universes", () => ({
  universeHome: () => "/u/test/sessions",
  useActiveUniverse: () => ({
    universe: {
      name: "Test universe",
      id: "platform-id",
      lightspeedUniverseId: "runtime-id",

      handle: "test", organizationId: "team",
      role: mocks.role, actions: [...universeActions([mocks.role as never])],
      organizationRole: mocks.organizationRole,
      organizationActions: [...organizationActions([mocks.organizationRole as never])],
    },
    isLoading: false,
  }),
}));
const grants = [
  { id: "owner-grant", principal: { kind: "user", id: "alice" }, name: "Alice", role: "owner", active: true, createdAt: "2026-11-01T00:00:00.000Z" },
  { id: "app-grant", principal: { kind: "serviceUser", id: "robot" }, name: "Build robot", role: "viewer", active: true, createdAt: "2026-11-01T00:00:00.000Z" },
];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "owner";
  mocks.organizationRole = "member";
  mocks.base = "viewer";
  mocks.api.mockReset().mockImplementation(async (operation: string) => {
    if (operation === "platform/universes/grants/list") return { grants };
    if (operation === "platform/organizations/members/list")
      return { members: [{ userId: "alice", name: "Alice", email: "alice@example.com" }, { userId: "bob", name: "Bob", email: "bob@example.com" }] };
    if (operation === "platform/organizations/service-users/list")
      return { serviceUsers: [{ id: "robot", name: "Build robot", enabled: true }] };
    return {};
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
  vi.unstubAllGlobals();
});
async function show() {
  await act(async () =>
    root.render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <PermissionIdentityProvider userId="alice" platformAdmin={false}>
            <UniverseMembersPage />
          </PermissionIdentityProvider>
        </QueryClientProvider>
      </MemoryRouter>,
    ),
  );
}
const rows = () => [...container.querySelectorAll("tbody tr")];
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const button = (label: string, scope: ParentNode = document) =>
  [...scope.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);

it("says who gets in through the organization's default before listing the roles given here", async () => {
  await show();
  await waitForUi(() => expect(rows()).toHaveLength(2));
  expect(container.querySelector("h1")?.textContent).toBe("Universe Members");
  expect(container.textContent).toContain("Everyone in Acme can use this universe as a Viewer, and its admins as Owners.");
  expect(container.querySelector('a[href="/admin/organizations/team/settings"]')).toBeNull();
  expect(rows()[0]!.textContent).toContain("alice@example.com");
  expect(rows()[1]!.textContent).toContain("Service user");
  for (const row of rows()) expect(row.querySelectorAll("button")).toHaveLength(1);
});

it("tells members they need a role here when the organization gives none by default, and links admins to the setting", async () => {
  mocks.base = null;
  mocks.organizationRole = "admin";
  await show();
  expect(container.textContent).toContain("Only the members below can use this universe, plus Acme's admins as Owners.");
  expect(container.querySelector('a[href="/admin/organizations/team/settings"]')).not.toBeNull();
});

it("changes a role only on Save and removes it only after confirming", async () => {
  await show();
  await waitForUi(() => expect(rows()).toHaveLength(2));
  await act(async () => (rows()[1]!.querySelector("td") as HTMLElement).click());
  await selectOption(dialog().querySelector<HTMLElement>("#grant-role")!, "Operator");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/universes/grants/put")).toBe(false);
  await act(async () => dialog().querySelector<HTMLFormElement>("form")!.requestSubmit());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/put", { universe: "runtime-id", principal: { kind: "serviceUser", id: "robot" }, role: "operator" }));
  await act(async () => button("Remove role", dialog())!.click());
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/universes/grants/delete")).toBe(false);
  const confirm = [...document.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button')].find((node) => node.textContent?.trim() === "Remove role")!;
  await act(async () => confirm.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/delete", { universe: "runtime-id", grantId: "app-grant" }));
});

it("marks a role whose holder has left the organization", async () => {
  mocks.api.mockImplementation(async (operation: string) => operation === "platform/universes/grants/list"
    ? { grants: [grants[0], { id: "gone-grant", principal: { kind: "user", id: "carol" }, name: "Carol", role: "owner", active: false, createdAt: "2026-11-01T00:00:00.000Z" }] }
    : { members: [], serviceUsers: [] });
  await show();
  await waitForUi(() => expect(rows()).toHaveLength(2));
  expect(rows()[0]!.textContent).not.toContain("No longer an organization member");
  expect(rows()[1]!.textContent).toContain("No longer an organization member");
  await act(async () => (rows()[1]!.querySelector("td") as HTMLElement).click());
  expect(dialog().textContent).toContain("gives nothing until they return");
});

it("adds a member who has no role here yet", async () => {
  await show();
  await waitForUi(() => expect(rows()).toHaveLength(2));
  await act(async () => button("Add member")!.click());
  const trigger = dialog().querySelector<HTMLElement>("#universe-member")!;
  await act(async () => trigger.click());
  const options = [...document.querySelectorAll('[data-slot="select-content"]:not([data-closed]) [role="option"]')].map((node) => node.textContent?.trim());
  expect(options).toContain("Bob (bob@example.com)");
  expect(options).not.toContain("Alice (alice@example.com)");
  expect(options).not.toContain("Build robot (service user)");
  const bob = [...document.querySelectorAll<HTMLElement>('[data-slot="select-content"]:not([data-closed]) [role="option"]')].find((node) => node.textContent?.trim() === "Bob (bob@example.com)")!;
  await act(async () => bob.click());
  await act(async () => dialog().querySelector<HTMLFormElement>("form")!.requestSubmit());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/put", { universe: "runtime-id", principal: { kind: "user", id: "bob" }, role: "contributor" }));
});

it("shows nothing to someone who cannot administer the universe", async () => {
  mocks.role = "operator";
  await show();
  expect(container.textContent).not.toContain("Universe Members");
  expect(mocks.api).not.toHaveBeenCalled();
});
