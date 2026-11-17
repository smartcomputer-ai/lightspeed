// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Routes, Route } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AdminApiKeysPage } from "./AdminApiKeysPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  card: vi.fn(),
  platformAdmin: false,
  role: "admin",
}));
vi.mock("@/lib/permissions", () => ({
  usePlatformAdmin: () => mocks.platformAdmin,
}));
vi.mock("@/lib/platform-api", () => ({
  platformApi: mocks.api,
  useOrganizations: () => ({
    data: {
      organizations: [
        { id: "team", name: "Research", enabled: true, role: mocks.role },
        { id: "other", name: "Operations", enabled: true, role: "member" },
        { id: "suspended", name: "Suspended", enabled: false, role: "admin" },
      ],
    },
  }),
}));
vi.mock("@/components/api-keys-card", () => ({
  ApiKeysCard: (props: unknown) => {
    mocks.card(props);
    return <div>Service user key management</div>;
  },
}));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  mocks.platformAdmin = false;
  mocks.role = "admin";
  mocks.card.mockReset();
  mocks.api.mockReset().mockImplementation(async (operation) => {
    if (operation === "platform/organizations/service-users/list")
      return {
        serviceUsers: [
          { id: "robot", name: "Build robot", enabled: true, role: "member" },
          { id: "disabled", name: "Old robot", enabled: false, role: "member" },
        ],
      };
    if (operation === "platform/service-users/credentials/list")
      return {
        credentials: [
          {
            id: "key",
            name: "Build key",
            enabled: true,
            expiresAt: "2999-01-01",
            start: "lsa_test",
            restrictions: {},
          },
        ],
      };
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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function show(path = "/admin/organizations/team/api-keys") {
  await act(async () =>
    root.render(
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          <Routes><Route path="/admin/organizations/:organizationId/api-keys" element={<AdminApiKeysPage />} /></Routes>
        </QueryClientProvider>
      </MemoryRouter>,
    ),
  );
}
async function click(label: string) {
  await act(async () =>
    [...document.querySelectorAll("button")]
      .find((button) => button.textContent === label)!
      .click(),
  );
}
function apiCallsForRevocation() {
  return mocks.api.mock.calls.filter(
    ([operation]) => operation === "platform/service-users/credentials/revoke",
  );
}
async function select(id: string, value: string) {
  const select = document.getElementById(id) as HTMLSelectElement;
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

it("uses the route organization without a second organization selector", async () => {
  await show();
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/organizations/service-users/list", { organizationId: "team" }));
  expect(document.querySelector("#key-organization")).toBeNull();
  expect(mocks.api).not.toHaveBeenCalledWith("platform/organizations/service-users/list", { organizationId: "other" });
});

it.each(["other", "missing"])(
  "does not fetch or fall back from unauthorized organization %s",
  async (organization) => {
    await show(`/admin/organizations/${organization}/api-keys`);
    expect(document.body.textContent).toContain(
      "This organization is unavailable or you do not administer it.",
    );
    expect(mocks.api).not.toHaveBeenCalled();
  },
);

it("allows a Platform Admin to select an organization without a direct admin membership", async () => {
  mocks.platformAdmin = true;
  await show("/admin/organizations/other/api-keys?serviceUser=robot");
  await waitForUi(() =>
    expect(mocks.card).toHaveBeenCalledWith(
      expect.objectContaining({
        serviceUser: expect.objectContaining({
          organizationId: "other",
          serviceUserId: "robot",
          name: "Build robot",
        }),
      }),
    ),
  );
  expect(mocks.api).toHaveBeenCalledWith(
    "platform/organizations/service-users/list",
    { organizationId: "other" },
  );
});

it("lists service user key states and revokes with the exact owner scope", async () => {
  await show("/admin/organizations/team/api-keys");
  await waitForUi(() =>
    expect(document.body.textContent).toContain("Service user disabled"),
  );
  expect(mocks.api).toHaveBeenCalledWith(
    "platform/service-users/credentials/list",
    { organizationId: "team", serviceUserId: "robot" },
  );
  expect(mocks.api).toHaveBeenCalledWith(
    "platform/service-users/credentials/list",
    { organizationId: "team", serviceUserId: "disabled" },
  );
  expect(document.querySelector('a[href="/admin/organizations/team/api-keys?serviceUser=robot"]')).not.toBeNull();
  await click("Revoke");
  expect(apiCallsForRevocation()).toHaveLength(0);
  await click("Revoke key");
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith(
      "platform/service-users/credentials/revoke",
      { organizationId: "team", serviceUserId: "robot", keyId: "key" },
    ),
  );
});

it("selects a named owner before opening key creation and excludes disabled service users", async () => {
  await show("/admin/organizations/team/api-keys");
  await waitForUi(() =>
    expect(document.querySelector<HTMLButtonElement>("button")?.disabled).toBe(
      false,
    ),
  );
  await click("New API key");
  const dialog = document.querySelector('[role="dialog"]')!;
  expect(dialog.className).toContain("overflow-y-auto");
  expect(dialog.className).toContain("max-h-[min(92dvh,800px)]");
  expect(dialog.textContent).not.toContain("Old robot");
  await select("key-owner", "robot");
  await click("Continue");
  await waitForUi(() =>
    expect(mocks.card).toHaveBeenCalledWith(
      expect.objectContaining({
        createOpen: true,
        serviceUser: expect.objectContaining({
          serviceUserId: "robot",
          organizationId: "team",
        }),
      }),
    ),
  );
});

it("does not substitute another service user for an unavailable deep link", async () => {
  await show("/admin/organizations/team/api-keys?serviceUser=missing");
  await waitForUi(() =>
    expect(document.body.textContent).toContain(
      "This service user is unavailable",
    ),
  );
  expect(mocks.card).not.toHaveBeenCalled();
  expect(mocks.api).not.toHaveBeenCalledWith(
    "platform/service-users/credentials/list",
    expect.anything(),
  );
});

it("shows suspended organizations without making directory requests that require active access", async () => {
  await show("/admin/organizations/suspended/api-keys");
  expect(document.body.textContent).toContain("This organization is suspended");
  expect(mocks.api).not.toHaveBeenCalled();
});

it("links an empty organization key inventory to its service-user page", async () => {
  mocks.api.mockResolvedValue({ serviceUsers: [] });
  await show();
  await waitForUi(() => expect(document.querySelector('a[href="/admin/organizations/team/service-users"]')?.textContent).toContain("Create a service user"));
});
