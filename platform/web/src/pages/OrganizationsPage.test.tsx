// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter, Routes, Route } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { OrganizationsPage } from "./OrganizationsPage";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  role: "member",
  canCreate: false,
  loading: false,
  empty: false,
  personal: false,
}));
vi.mock("@/lib/platform-api", () => ({
  platformApi: mocks.api,
  useOrganizations: () => ({
    isLoading: mocks.loading,
    data: mocks.loading
      ? undefined
      : {
          canCreate: mocks.canCreate,
          organizations: mocks.empty
            ? []
            : [
                {
                  id: "team",
                  name: "Team",
                  personal: mocks.personal,
                  role: mocks.role,
                  universeQuota: 100,
                  universeCount: 3,
                  defaultProtected: true,
                  requireProtected: false,
                },
                {
                  id: "other",
                  name: "Other Team",
                  personal: false,
                  role: mocks.role,
                  universeQuota: 200,
                  defaultProtected: false,
                  requireProtected: false,
                },
              ],
        },
  }),
}));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
  mocks.role = "member";
  mocks.canCreate = false;
  mocks.loading = false;
  mocks.empty = false;
  mocks.personal = false;
  mocks.api
    .mockReset()
    .mockImplementation(async (operation) =>
      operation.endsWith("members/list")
        ? { members: [] }
        : operation.endsWith("service-users/list")
          ? { serviceUsers: [] }
          : {},
    );
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
async function show(path: string, platformAdmin = false) {
  await act(async () =>
    root.render(
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          <PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}>
            <Routes><Route path="/admin/organizations/:organizationId/people" element={<OrganizationsPage section="people" />} /><Route path="/admin/organizations/:organizationId/service-users" element={<OrganizationsPage section="service-users" />} /><Route path="/admin/organizations/:organizationId/settings" element={<OrganizationsPage section="settings" />} /></Routes>
          </PermissionIdentityProvider>
        </QueryClientProvider>
      </MemoryRouter>,
    ),
  );
}
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const button = (name: string, scope: ParentNode = document) =>
  [...scope.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => (node.getAttribute("aria-label") ?? node.textContent?.trim()) === name,
  )!;
const click = (name: string, scope?: ParentNode) => act(async () => button(name, scope).click());
async function type(selector: string, value: string) {
  const node = document.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const submit = () => act(async () => dialog().querySelector<HTMLFormElement>("form")!.requestSubmit());

it("lets a Platform Admin open an organization without a direct membership", async () => {
  await show("/admin/organizations/team/people", true);
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/members/list", {
      organizationId: "team",
    }),
  );
  expect(button("Add member")).toBeDefined();
});

it.each(["people", "service-users", "settings"])("keeps %s in the selected organization without a Platform back link", async (section) => {
  await show(`/admin/organizations/team/${section}`, true);
  expect(container.querySelector('a[href="/admin/platform/organizations"]')).toBeNull();
});

it.each(["missing", "team"])(
  "refuses an unavailable or unauthorized explicit organization: %s",
  async (organization) => {
    await show(`/admin/organizations/${organization}/people`);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("This organization is unavailable");
    expect(mocks.api).not.toHaveBeenCalled();
  },
);

it("creates a service user through a scrollable dialog in the explicitly selected organization", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/other/service-users");
  expect(container.querySelector('input[id="serviceUser-name"]')).toBeNull();
  await click("New service user");
  expect(dialog().className).toContain("overflow-y-auto");
  expect(dialog().className).toContain("92dvh");
  await type("#serviceUser-name", "Build worker");
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/service-users/create", {
      organizationId: "other",
      name: "Build worker",
      role: "member",
    }),
  );
  await waitForUi(() => expect(dialog()).toBeNull());
});

it("adds organization membership only after the add dialog is submitted", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/team/people");
  await click("Add member");
  await type("#organization-member-id", "new-user");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/members/put")).toBe(
    false,
  );
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/members/put", {
      organizationId: "team",
      userId: "new-user",
      role: "member",
    }),
  );
});

it("links service user keys to unified administration and confirms disabling a service user in a dialog", async () => {
  mocks.role = "admin";
  mocks.api.mockImplementation(async (operation) =>
    operation.endsWith("members/list")
      ? { members: [] }
      : operation.endsWith("service-users/list")
        ? {
            serviceUsers: [
              {
                id: "robot",
                name: "Build robot",
                enabled: true,
                role: "member",
              },
            ],
          }
        : {},
  );
  await show("/admin/organizations/team/service-users");
  await waitForUi(() =>
    expect(container.querySelector('[aria-label="API keys for Build robot"]')).not.toBeNull(),
  );
  expect(container.querySelector('[aria-label="API keys for Build robot"]')?.getAttribute("href")).toBe(
    "/admin/organizations/team/api-keys?serviceUser=robot",
  );
  await click("Disable Build robot");
  expect(dialog().textContent).toContain("All of its API keys will stop working");
  expect(
    mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/service-users/update"),
  ).toBe(false);
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/service-users/update", {
      organizationId: "team",
      serviceUserId: "robot",
      enabled: false,
    }),
  );
});

it("keeps policy changes as a draft until explicitly saved", async () => {
  await show("/admin/organizations/team/settings", true);
  await click("Edit policy");
  await type("#organization-quota", "50");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/update")).toBe(false);
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", {
      organizationId: "team",
      universeQuota: 50,
      defaultProtected: true,
      requireProtected: false,
    }),
  );
});

it("keeps an unsuccessful add operation in its dialog with the entered values", async () => {
  mocks.role = "admin";
  mocks.api.mockImplementation(async (operation) => {
    if (operation.endsWith("members/list")) return { members: [] };
    if (operation.endsWith("service-users/list")) return { serviceUsers: [] };
    throw new Error("user not found");
  });
  await show("/admin/organizations/team/people");
  await click("Add member");
  await type("#organization-member-id", "unknown-user");
  await submit();
  await waitForUi(() => expect(dialog().querySelector('[role="alert"]')?.textContent).toBe("user not found"));
  expect(dialog().querySelector<HTMLInputElement>("#organization-member-id")!.value).toBe("unknown-user");
});

it("shows universe usage in organization settings", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/team/settings");
  expect(container.textContent).toContain("Universes: 3 of 100");
});

it("leaves the universe quota to Platform Admins", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/team/settings");
  await click("Edit policy");
  expect(dialog().querySelector("#organization-quota")).toBeNull();
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", {
      organizationId: "team",
      defaultProtected: true,
      requireProtected: false,
    }),
  );
});

it("explains that a personal organization has no other members", async () => {
  mocks.role = "admin";
  mocks.personal = true;
  await show("/admin/organizations/team/people");
  expect(container.textContent).toContain("only its owner as a member");
  expect(button("Add member")).toBeUndefined();
  expect(mocks.api).not.toHaveBeenCalled();
});
