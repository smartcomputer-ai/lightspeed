// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { AdminUniverseAccessPage } from "./AdminUniverseAccessPage";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "owner", organizationRole: "member" }));
vi.mock("@/lib/platform-api", () => ({ platformApi: mocks.api }));
vi.mock("@/lib/universes", () => ({
  universeHome: () => "/u/test/sessions",
  useActiveUniverse: () => ({
    universe: {
      name: "Test universe",
      id: "platform-id",
      lightspeedUniverseId: "runtime-id",

      handle: "test", organizationId: "team",
      role: mocks.role,
      organizationRole: mocks.organizationRole,
    },
    isLoading: false,
  }),
}));
const grants = [
  { id: "owner-grant", principal: { kind: "user", id: "alice" }, name: "Alice", role: "owner" },
  { id: "app-grant", principal: { kind: "serviceUser", id: "robot" }, name: "Build robot", role: "viewer" },
];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "owner";
  mocks.organizationRole = "member";
  mocks.api.mockReset().mockImplementation(async (operation: string) => {
    if (operation === "platform/universes/grants/list") return { grants };
    if (operation === "platform/organizations/members/list")
      return { members: [{ userId: "alice", name: "Alice", email: "alice@example.com" }] };
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
            <AdminUniverseAccessPage admin={false} />
          </PermissionIdentityProvider>
        </QueryClientProvider>
      </MemoryRouter>,
    ),
  );
}
async function choose(label: string, value: string) {
  const node = document.querySelector<HTMLSelectElement>(
    label === "Organization member or service user"
      ? "#universe-access-recipient"
      : label === "Universe role"
        ? "#universe-access-role"
        : `select[aria-label="${label}"]`,
  )!;
  await act(async () => {
    node.value = value;
    node.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
it("changes a service user's universe role using the native scoped grant API", async () => {
  await show();
  await waitForUi(() => expect(container.textContent).toContain("Build robot"));
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Edit role for Build robot"]')!.click());
  expect(document.querySelector<HTMLInputElement>("#universe-access-recipient")?.readOnly).toBe(true);
  expect(document.querySelector<HTMLInputElement>("#universe-access-recipient")?.value).toBe("Build robot");
  await choose("Universe role", "operator");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/universes/grants/put")).toBe(false);
  await act(async () => document.querySelector<HTMLButtonElement>('[role="dialog"] button[type="submit"]')!.click());
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/put", {
      universe: "runtime-id",
      principal: { kind: "serviceUser", id: "robot" },
      role: "operator",
    }),
  );
});
it("preserves the displayed Owner when the server refuses demotion", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (operation, params) => {
    if (operation === "platform/universes/grants/put") throw new Error("Keep at least one explicit Owner");
    return implementation(operation, params);
  });
  await show();
  await waitForUi(() => expect(container.querySelector('[aria-label="Edit role for Alice"]')).not.toBeNull());
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Edit role for Alice"]')!.click());
  await choose("Universe role", "viewer");
  await act(async () => document.querySelector<HTMLButtonElement>('[role="dialog"] button[type="submit"]')!.click());
  await waitForUi(() =>
    expect(document.querySelector('[role="dialog"] [role="alert"]')?.textContent).toContain("at least one explicit Owner"),
  );
  expect(container.querySelector('[aria-label="Edit role for Alice"]')?.closest("tr")?.querySelectorAll("td")[2]?.textContent).toBe("Owner");
  expect(document.querySelector('[role="dialog"] [role="alert"]')?.textContent).toContain("at least one explicit Owner");
});
it("grants an existing organization service user access without creating an organization member", async () => {
  await show();
  await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "Add access")!.click());
  await waitForUi(() => expect(document.querySelector('[value="serviceUser:robot"]')).not.toBeNull());
  await choose("Organization member or service user", "serviceUser:robot");
  await choose("Universe role", "owner");
  await act(async () => document.querySelector<HTMLButtonElement>('button[type="submit"]')!.click());
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/universes/grants/put", {
      universe: "runtime-id",
      principal: { kind: "serviceUser", id: "robot" },
      role: "owner",
    }),
  );
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/members/put")).toBe(false);
});
it.each(["viewer", "contributor", "operator"])("does not fetch the grant directory for a %s", async (role) => {
  mocks.role = role;
  await show();
  expect(container.textContent).toContain("Only a universe Owner");
  expect(mocks.api).not.toHaveBeenCalled();
});

it("explains personal versus service user keys without organization admin controls for an Owner", async () => {
  await show();
  await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "Add access")!.click());
  await waitForUi(() => expect(document.querySelector('[value="user:alice"]')?.textContent).toContain("alice@example.com"));
  expect(container.querySelector("h1")?.textContent).toBe("Test universe");
  expect(document.querySelector('[role="dialog"]')?.className).toContain("overflow-y-auto");
  expect(container.querySelector('a[href="/account#api-keys"]')).not.toBeNull();
  expect(container.querySelector('a[href="/admin/organizations/team/people"]')).toBeNull();
  expect(container.textContent).toContain("ask an Organization Admin");
  expect(container.textContent).not.toContain("Principal");
});

it("links Organization Admins to this organization's members and service user keys", async () => {
  mocks.organizationRole = "admin";
  await show();
  expect(container.querySelector('a[href="/admin/organizations/team/people"]')?.textContent).toBe(
    "organization members",
  );
});

it("shows directory failures and disables grants instead of presenting an empty selection", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (operation, params) => {
    if (operation === "platform/organizations/members/list") throw new Error("Directory unavailable");
    return implementation(operation, params);
  });
  await show();
  await waitForUi(() => expect(container.querySelector('[role="alert"]')?.textContent).toBe("Directory unavailable"));
  await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "Add access")!.click());
  expect(document.querySelector<HTMLSelectElement>("#universe-access-recipient")?.disabled).toBe(true);
  expect(document.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true);
  expect(container.textContent).not.toContain("No organization members or enabled service users are available");
});

it("confirms removal in a scrollable dialog and keeps a refused grant visible", async () => {
  const implementation = mocks.api.getMockImplementation()!;
  mocks.api.mockImplementation(async (operation, params) => {
    if (operation === "platform/universes/grants/delete") throw new Error("Keep at least one explicit Owner");
    return implementation(operation, params);
  });
  await show();
  await waitForUi(() => expect(container.querySelector('[aria-label="Remove access for Alice"]')).not.toBeNull());
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Remove access for Alice"]')!.click());
  const dialog = document.querySelector<HTMLElement>('[role="alertdialog"]')!;
  expect(dialog.className).toContain("overflow-y-auto");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/universes/grants/delete")).toBe(false);
  await act(async () => [...dialog.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "Remove access")!.click());
  await waitForUi(() => expect(dialog.querySelector('[role="alert"]')?.textContent).toContain("Keep at least one explicit Owner"));
  expect(container.querySelector('[aria-label="Edit role for Alice"]')).not.toBeNull();
});


it("cancels a role edit without changing access and resets the next edit", async () => {
  await show();
  await waitForUi(() => expect(container.querySelector('[aria-label="Edit role for Alice"]')).not.toBeNull());
  const edit = container.querySelector<HTMLButtonElement>('[aria-label="Edit role for Alice"]')!;
  await act(async () => edit.click());
  expect(document.querySelector<HTMLButtonElement>('[role="dialog"] button[type="submit"]')!.disabled).toBe(true);
  await choose("Universe role", "viewer");
  const dialog = document.querySelector<HTMLElement>('[role="dialog"]')!;
  expect(dialog.className).toContain("overflow-y-auto");
  await act(async () => [...dialog.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "Cancel")!.click());
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/universes/grants/put")).toBe(false);
  expect(edit.closest("tr")?.querySelectorAll("td")[2]?.textContent).toBe("Owner");
  await act(async () => edit.click());
  expect(document.querySelector<HTMLSelectElement>("#universe-access-role")?.value).toBe("owner");
});
