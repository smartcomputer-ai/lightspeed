// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter, Routes, Route } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const { organizationActions, universeActions } = await vi.hoisted(() => import("@lightspeed-ai/platform-shared"));
import { OrganizationSettingsPage } from "./OrganizationSettingsPage";
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
  quotaSource: "deployment",
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
                  universeQuotaSource: mocks.quotaSource,
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
                  universeQuotaSource: "organization",
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
  mocks.quotaSource = "deployment";
  mocks.api.mockReset().mockImplementation(async (operation) => {
    if (operation.endsWith("modules/list")) return { modules: [] };
    if (operation.endsWith("members/list")) return { members: [] };
    if (operation.endsWith("service-users/list")) return { serviceUsers: [] };
    return {};
  });
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
            <Routes><Route path="/admin/organizations/:organizationId/settings" element={<OrganizationSettingsPage />} /></Routes>
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

it("saves module availability for the current organization", async () => {
  mocks.role = "admin";
  let enabled = true;
  mocks.api.mockImplementation(async (operation, params) => {
    if (operation.endsWith("modules/put")) enabled = params.enabled;
    const module = { id: "artifacts", name: "Artifacts", description: "Workspace files", enabled };
    return operation.endsWith("modules/list") ? { modules: [module] } : { module };
  });
  await show("/admin/organizations/team/settings");
  const toggle = () => container.querySelector<HTMLElement>('[role="switch"][aria-describedby="module-help-artifacts"]');
  await waitForUi(() => expect(toggle()?.getAttribute("aria-checked")).toBe("true"));
  await act(async () => toggle()!.click());
  await waitForUi(() => expect(toggle()?.getAttribute("aria-checked")).toBe("false"));
  expect(mocks.api).toHaveBeenCalledWith("platform/organizations/modules/put", {
    organizationId: "team", moduleId: "artifacts", enabled: false,
  });
});

it("saves a changed universe quota only when its own Save is pressed", async () => {
  await show("/admin/organizations/team/settings", true);
  expect(container.textContent).toContain("3 of 100 used");
  await type("#organization-quota", "50");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/organizations/update")).toBe(false);
  await act(async () => container.querySelector<HTMLFormElement>("#organization-quota")!.form!.requestSubmit());
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", { organizationId: "team", universeQuota: 50 }),
  );
});

it("says an organization follows the deployment default, and returns one with its own limit to it", async () => {
  await show("/admin/organizations/team/settings", true);
  expect(container.textContent).toContain("Follows the default for shared organizations, set in Platform settings.");
  expect(container.querySelector('a[href="/admin/platform/settings"]')).not.toBeNull();
  await act(async () => root.unmount());
  root = createRoot(container);
  mocks.quotaSource = "organization";
  await show("/admin/organizations/team/settings", true);
  expect(container.textContent).toContain("This organization has its own limit.");
  const reset = [...container.querySelectorAll("button")].find((node) => node.textContent === "Use the default for shared organizations")!;
  await act(async () => reset.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", { organizationId: "team", universeQuota: null }));
});

it("shows an Organization Admin the usage but leaves the quota to Platform Admins", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/team/settings");
  expect(container.textContent).toContain("3 of 100 used");
  expect(container.textContent).toContain("A Platform Admin sets the limit.");
  expect(container.querySelector("#organization-quota")).toBeNull();
});

it("saves default universe access and protection as soon as they change", async () => {
  mocks.role = "admin";
  await show("/admin/organizations/team/settings");
  expect(button("Viewer").getAttribute("aria-pressed")).toBe("true");
  const note = container.querySelector<HTMLElement>('[data-slot="info-box"]')!;
  expect(note.dataset.tone).toBe("info");
  expect(note.textContent).toContain("Unshared sessions stay private");
  await click("Operator");
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", { organizationId: "team", baseUniverseRole: "operator" }),
  );
  await click("None");
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", { organizationId: "team", baseUniverseRole: null }),
  );
  await act(async () => container.querySelector<HTMLButtonElement>("#organization-requireProtected")!.click());
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/organizations/update", { organizationId: "team", requireProtected: true }),
  );
});

it("leaves name and default universe access out of a personal organization's settings", async () => {
  mocks.role = "admin";
  mocks.personal = true;
  await show("/admin/organizations/team/settings");
  expect(container.querySelector("#organization-name")).toBeNull();
  expect(container.textContent).not.toContain("Default universe access");
});
