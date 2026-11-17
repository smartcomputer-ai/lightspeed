// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AccountsPage } from "./AccountsPage";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { waitForUi } from "@/test/wait-for-ui";
const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  role: "member",
  canCreate: false,
  loading: false,
  empty: false,
}));
vi.mock("@/lib/platform-api", () => ({
  platformApi: mocks.api,
  useAccounts: () => ({
    isLoading: mocks.loading,
    data: mocks.loading
      ? undefined
      : {
          canCreate: mocks.canCreate,
          accounts: mocks.empty
            ? []
            : [
                {
                  id: "team",
                  name: "Team",
                  kind: "shared",
                  enabled: true,
                  role: mocks.role,
                  universeQuota: 100,
                  defaultProtected: true,
                  requireProtected: false,
                },
                {
                  id: "other",
                  name: "Other Team",
                  kind: "shared",
                  enabled: true,
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
  mocks.api
    .mockReset()
    .mockImplementation(async (operation) =>
      operation.endsWith("members/list")
        ? { members: [] }
        : operation.endsWith("applications/list")
          ? { applications: [] }
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
async function show(path = "/admin/accounts", platformAdmin = false) {
  await act(async () =>
    root.render(
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          <PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}>
            <AccountsPage />
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

it("excludes ordinary memberships from account administration and does not fetch their directories", async () => {
  await show();
  expect(container.textContent).toContain("You do not administer any accounts");
  expect(container.querySelector("table")).toBeNull();
  expect(mocks.api).not.toHaveBeenCalled();
});

it("lists manageable accounts without silently selecting one", async () => {
  mocks.role = "admin";
  await show();
  expect(container.querySelector('a[href="/admin/accounts?account=team"]')?.textContent).toBe("Team");
  expect(container.querySelector('a[href="/admin/accounts?account=other"]')?.textContent).toBe("Other Team");
  expect(mocks.api).not.toHaveBeenCalled();
});

it("lets a Platform Admin open an account without a direct membership", async () => {
  await show("/admin/accounts?account=team", true);
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/accounts/members/list", {
      accountId: "team",
    }),
  );
  expect(button("Add member")).toBeDefined();
});

it.each(["missing", "team"])(
  "refuses an unavailable or unauthorized explicit account: %s",
  async (account) => {
    await show(`/admin/accounts?account=${account}`);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("This account is unavailable");
    expect(mocks.api).not.toHaveBeenCalled();
  },
);

it("creates an application through a scrollable dialog in the explicitly selected account", async () => {
  mocks.role = "admin";
  await show("/admin/accounts?account=other");
  expect(container.querySelector('input[id="application-name"]')).toBeNull();
  await click("New application");
  expect(dialog().className).toContain("overflow-y-auto");
  expect(dialog().className).toContain("92dvh");
  await type("#application-name", "Build worker");
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/accounts/applications/create", {
      accountId: "other",
      name: "Build worker",
      role: "member",
    }),
  );
  await waitForUi(() => expect(dialog()).toBeNull());
});

it("offers a new account dialog when self-service is permitted, even without existing admin memberships", async () => {
  mocks.canCreate = true;
  await show();
  await click("New account");
  expect(dialog().textContent).toContain("You become its first Account Admin");
  expect(dialog().querySelector('[aria-label="New account name"]')).not.toBeNull();
  expect(dialog().className).toContain("overflow-y-auto");
});

it("adds account membership only after the add dialog is submitted", async () => {
  mocks.role = "admin";
  await show("/admin/accounts?account=team");
  await click("Add member");
  await type("#account-member-id", "new-user");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/accounts/members/put")).toBe(
    false,
  );
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/accounts/members/put", {
      accountId: "team",
      userId: "new-user",
      role: "member",
    }),
  );
});

it("links application keys to unified administration and confirms disabling an application in a dialog", async () => {
  mocks.role = "admin";
  mocks.api.mockImplementation(async (operation) =>
    operation.endsWith("members/list")
      ? { members: [] }
      : operation.endsWith("applications/list")
        ? {
            applications: [
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
  await show("/admin/accounts?account=team");
  await waitForUi(() =>
    expect(container.querySelector('[aria-label="API keys for Build robot"]')).not.toBeNull(),
  );
  expect(container.querySelector('[aria-label="API keys for Build robot"]')?.getAttribute("href")).toBe(
    "/admin/api-keys?account=team&application=robot",
  );
  await click("Disable Build robot");
  expect(dialog().textContent).toContain("All of its API keys will stop working");
  expect(
    mocks.api.mock.calls.some(([operation]) => operation === "platform/accounts/applications/update"),
  ).toBe(false);
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/accounts/applications/update", {
      accountId: "team",
      applicationId: "robot",
      enabled: false,
    }),
  );
});

it("keeps policy changes as a draft until explicitly saved", async () => {
  mocks.role = "admin";
  await show("/admin/accounts?account=team");
  await click("Edit policy");
  await type("#account-quota", "50");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/accounts/update")).toBe(false);
  await submit();
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith("platform/accounts/update", {
      accountId: "team",
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
    if (operation.endsWith("applications/list")) return { applications: [] };
    throw new Error("user not found");
  });
  await show("/admin/accounts?account=team");
  await click("Add member");
  await type("#account-member-id", "unknown-user");
  await submit();
  await waitForUi(() => expect(dialog().querySelector('[role="alert"]')?.textContent).toBe("user not found"));
  expect(dialog().querySelector<HTMLInputElement>("#account-member-id")!.value).toBe("unknown-user");
});

it("distinguishes loading from having no accounts to administer", async () => {
  mocks.loading = true;
  await show();
  expect(container.textContent).not.toContain("You do not administer any accounts");
  mocks.loading = false;
  mocks.empty = true;
  await show();
  expect(container.textContent).toContain("You do not administer any accounts");
});
