// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter, Routes, Route } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { organizationActions, universeActions } = await vi.hoisted(() => import("@lightspeed-ai/platform-shared"));
import { OrganizationMembersPage } from "./OrganizationMembersPage";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  role: "member",
  platformAdmin: false,
  canCreate: false,
  loading: false,
  empty: false,
  personal: false,
}));
vi.mock("@/lib/deployment-users", () => ({
  useDeploymentUsers: () => ({
    isLoading: false,
    error: null,
    data: [
      { id: "user", name: "Current", email: "current@example.com" },
      { id: "dana", name: "Dana", email: "dana@example.com" },
      { id: "erin", name: "Erin", email: "erin@example.com", banned: true },
    ],
  }),
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
                  role: mocks.role, actions: [...organizationActions(mocks.platformAdmin ? [mocks.role as never, "admin"] : [mocks.role as never])], baseUniverseRole: "viewer",
                  universeQuota: 100,
                  universeCount: 3,
                  defaultProtected: true,
                  requireProtected: false,
                },
                {
                  id: "other",
                  name: "Other Team",
                  personal: false,
                  role: mocks.role, actions: [...organizationActions(mocks.platformAdmin ? [mocks.role as never, "admin"] : [mocks.role as never])], baseUniverseRole: "viewer",
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
  mocks.platformAdmin = platformAdmin;
  await act(async () =>
    root.render(
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          <PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}>
            <Routes><Route path="/admin/organizations/:organizationId/members" element={<OrganizationMembersPage />} /></Routes>
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

const people = [
  { id: "m-ada", userId: "ada", name: "Ada", email: "ada@example.test", role: "admin", added: true, declared: false, createdAt: "2026-11-01T00:00:00.000Z" },
  { id: "declared:bob", userId: "bob", name: "Bob", email: "bob@example.test", role: "member", added: false, declared: true, createdAt: "2026-11-01T00:00:00.000Z" },
];
const robots = [{ id: "robot", organizationId: "team", name: "Build robot", enabled: true, role: "member", createdAt: "2026-11-01T00:00:00.000Z" }];
function withDirectory(federation = false) {
  let credentials: { id: string; issuer: string; subject: string; restrictions: object; lastUsedAt: string }[] = [];
  mocks.api.mockImplementation(async (operation: string, params) => {
    if (operation.endsWith("members/list")) return { members: people };
    if (operation.endsWith("service-users/list")) return { serviceUsers: robots };
    if (operation.endsWith("federated-credentials/list")) return { credentials, issuers: federation ? ["cluster"] : [] };
    if (operation.endsWith("federated-credentials/put")) {
      const binding = { ...params, id: "binding", lastUsedAt: "2026-11-01T00:00:00Z" };
      credentials = [binding];
      return { credential: binding };
    }
    if (operation.endsWith("federated-credentials/delete")) { credentials = []; return { ok: true }; }
    return { credentials: [] };
  });
}
const rows = (label: string) => [...container.querySelectorAll(`table[aria-label="${label}"] tbody tr`)];
const openRow = (label: string, index: number) => act(async () => (rows(label)[index]!.querySelector("td") as HTMLElement).click());

it("lists people with their role and service users with their status, without row buttons", async () => {
  mocks.role = "admin";
  withDirectory();
  await show("/admin/organizations/team/members");
  await waitForUi(() => expect(rows("People")).toHaveLength(2));
  expect(rows("People")[0]!.textContent).toContain("Admin");
  expect(rows("People")[1]!.textContent).toContain("Member");
  expect(rows("Service users")[0]!.textContent).toContain("Active");
  for (const row of [...rows("People"), ...rows("Service users")]) {
    expect([...row.querySelectorAll("button")].map((node) => node.getAttribute("aria-label"))).toEqual([expect.stringMatching(/^Details for /)]);
  }
  expect(button("Add person")).toBeDefined();
  expect(button("New service user")).toBeDefined();
});

it("changes a person's role only when its Save is pressed, and removes only members added by hand", async () => {
  mocks.role = "admin";
  withDirectory();
  await show("/admin/organizations/team/members");
  await waitForUi(() => expect(rows("People")).toHaveLength(2));
  await openRow("People", 0);
  expect(dialog().textContent).toContain("ada");
  expect(dialog().textContent).toContain("Joined viaAn admin");
  expect(button("Remove from organization", dialog())).toBeDefined();
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/members/put")).toBe(false);
  await act(async () => dialog().querySelector<HTMLButtonElement>('[data-icon="dismiss"], [data-slot="dialog-close"]')?.click());
  await act(async () => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  await waitForUi(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
  await openRow("People", 1);
  expect(button("Remove from organization", dialog())).toBeUndefined();
  expect(dialog().textContent).toContain("Joined viaThe membership policy");
  expect(dialog().textContent).toContain("Change the policy to remove them");
});

it("adds a person only after the add dialog is submitted, and keeps a failed add's values", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/team/members");
  await click("Add person");
  await type("#organization-member-id", "new-user");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/members/put")).toBe(false);
  mocks.api.mockImplementation(async (operation: string) => {
    if (operation.endsWith("members/list")) return { members: [] };
    if (operation.endsWith("service-users/list")) return { serviceUsers: [] };
    throw new Error("user not found");
  });
  await submit();
  await waitForUi(() => expect(dialog().querySelector('[role="alert"]')?.textContent).toBe("user not found"));
  expect(mocks.api).toHaveBeenCalledWith("platform/organizations/members/put", { organizationId: "team", userId: "new-user", role: "member" });
  expect(dialog().querySelector<HTMLInputElement>("#organization-member-id")!.value).toBe("new-user");
});

it("creates a service user and manages its keys and lifecycle from its details", async () => {
  mocks.role = "admin";
  withDirectory();
  await show("/admin/organizations/team/members");
  await click("New service user");
  await type("#serviceUser-name", "Deployer");
  await submit();
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/organizations/service-users/create", { organizationId: "team", name: "Deployer", role: "member" }));
  await waitForUi(() => expect(rows("Service users")).toHaveLength(1));
  await openRow("Service users", 0);
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/service-users/credentials/list", { organizationId: "team", serviceUserId: "robot" }));
  expect(dialog().textContent).toContain("API keys");
  await waitForUi(() => expect(dialog().textContent).toContain("Federated credentials"));
  expect(dialog().textContent).toContain("LIGHTSPEED_PLATFORM_FEDERATION_FILE");
  expect(button("Add credential", dialog())).toBeUndefined();
  expect(button("New key", dialog())).toBeDefined();
  await click("Disable", dialog());
  expect(mocks.api.mock.calls.some(([operation, params]) => operation === "platform/organizations/service-users/update" && (params as { enabled?: boolean }).enabled === false)).toBe(false);
});

it("adds a federated credential, shows its last use, and revokes it from service user details", async () => {
  mocks.role = "admin";
  withDirectory(true);
  await show("/admin/organizations/team/members");
  await waitForUi(() => expect(rows("Service users")).toHaveLength(1));
  await openRow("Service users", 0);
  await waitForUi(() => expect(dialog().querySelector("#federated-subject")).toBeTruthy());
  await type("#federated-subject", "builder");
  await click("Add credential", dialog());
  await waitForUi(() => expect(dialog().textContent).toContain("Last used"));
  expect(mocks.api).toHaveBeenCalledWith("platform/service-users/federated-credentials/put", {
    organizationId: "team", serviceUserId: "robot", issuer: "cluster", subject: "builder",
    serviceAccountUid: null, restrictions: {}, createOnly: true,
  });
  await click("Revoke", dialog());
  await click("Revoke", document.querySelector('[role="alertdialog"]')!);
  await waitForUi(() => expect(dialog().textContent).not.toContain("Last used"));
  expect(mocks.api).toHaveBeenCalledWith("platform/service-users/federated-credentials/delete", {
    organizationId: "team", serviceUserId: "robot", credentialId: "binding",
  });
});

it("keeps a personal organization to its owner while allowing service users", async () => {
  mocks.role = "admin";
  mocks.personal = true;
  await show("/admin/organizations/team/members");
  expect(button("Add person")).toBeUndefined();
  expect(button("New service user")).toBeDefined();
  expect(container.textContent).toContain("its owner is the only person in it");
});

it("lets a Platform Admin manage members without a direct membership", async () => {
  await show("/admin/organizations/team/members", true);
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/organizations/members/list", { organizationId: "team" }));
  expect(button("Add person")).toBeDefined();
});

it("lets a Platform Admin pick the person from the deployment's users who are not members yet", async () => {
  mocks.api.mockImplementation(async (operation: string) => {
    if (operation.endsWith("members/list")) return { members: [{ id: "m1", userId: "user", name: "Current", email: "current@example.com", role: "member", added: true, declared: false, createdAt: "2026-11-01T00:00:00.000Z" }] };
    if (operation.endsWith("service-users/list")) return { serviceUsers: [] };
    return {};
  });
  await show("/admin/organizations/team/members", true);
  await waitForUi(() => expect(rows("People")).toHaveLength(1));
  await click("Add person");
  expect(dialog().querySelector("#organization-member-id")).toBeNull();
  const input = dialog().querySelector<HTMLInputElement>("#organization-member-user")!;
  await act(async () => { input.focus(); input.click(); });
  await type("#organization-member-user", "da");
  const options = () => [...document.querySelectorAll<HTMLElement>('[role="option"]')];
  await act(async () => { input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })); });
  await waitForUi(() => expect(options().map((option) => option.textContent)).toEqual(["Danadana@example.com"]));
  await act(async () => options()[0]!.click());
  await submit();
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/organizations/members/put", { organizationId: "team", userId: "dana", role: "member" }));
});

it.each(["missing", "team"])("refuses an unavailable or unauthorized organization without fetching: %s", async (organization) => {
  await show(`/admin/organizations/${organization}/members`);
  expect(container.querySelector('[role="alert"]')?.textContent).toContain("This organization is unavailable");
  expect(mocks.api).not.toHaveBeenCalled();
});
