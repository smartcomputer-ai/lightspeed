// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
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
  useAccounts: () => ({
    data: {
      accounts: [
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
    return <div>Application key management</div>;
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
    if (operation === "platform/accounts/applications/list")
      return {
        applications: [
          { id: "robot", name: "Build robot", enabled: true, role: "member" },
          { id: "disabled", name: "Old robot", enabled: false, role: "member" },
        ],
      };
    if (operation === "platform/applications/credentials/list")
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
async function show(path = "/admin/api-keys") {
  await act(async () =>
    root.render(
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          <AdminApiKeysPage />
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
    ([operation]) => operation === "platform/applications/credentials/revoke",
  );
}
async function select(id: string, value: string) {
  const select = document.getElementById(id) as HTMLSelectElement;
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

it("only offers administered accounts and waits for an explicit account selection", async () => {
  await show();
  const choices = [...document.querySelectorAll("#key-account option")].map(
    (option) => option.textContent,
  );
  expect(choices).toContain("Research");
  expect(choices).not.toContain("Operations");
  expect(mocks.api).not.toHaveBeenCalled();
});

it.each(["other", "missing"])(
  "does not fetch or fall back from unauthorized account %s",
  async (account) => {
    await show(`/admin/api-keys?account=${account}`);
    expect(document.body.textContent).toContain(
      "This account is unavailable or you do not administer it.",
    );
    expect(mocks.api).not.toHaveBeenCalled();
  },
);

it("allows a Platform Admin to select an account without a direct admin membership", async () => {
  mocks.platformAdmin = true;
  await show("/admin/api-keys?account=other&application=robot");
  await waitForUi(() =>
    expect(mocks.card).toHaveBeenCalledWith(
      expect.objectContaining({
        application: expect.objectContaining({
          accountId: "other",
          applicationId: "robot",
          name: "Build robot",
        }),
      }),
    ),
  );
  expect(mocks.api).toHaveBeenCalledWith(
    "platform/accounts/applications/list",
    { accountId: "other" },
  );
});

it("lists application key states and revokes with the exact owner scope", async () => {
  await show("/admin/api-keys?account=team");
  await waitForUi(() =>
    expect(document.body.textContent).toContain("Application disabled"),
  );
  expect(mocks.api).toHaveBeenCalledWith(
    "platform/applications/credentials/list",
    { accountId: "team", applicationId: "robot" },
  );
  expect(mocks.api).toHaveBeenCalledWith(
    "platform/applications/credentials/list",
    { accountId: "team", applicationId: "disabled" },
  );
  await click("Revoke");
  expect(apiCallsForRevocation()).toHaveLength(0);
  await click("Revoke key");
  await waitForUi(() =>
    expect(mocks.api).toHaveBeenCalledWith(
      "platform/applications/credentials/revoke",
      { accountId: "team", applicationId: "robot", keyId: "key" },
    ),
  );
});

it("selects a named owner before opening key creation and excludes disabled applications", async () => {
  await show("/admin/api-keys?account=team");
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
        application: expect.objectContaining({
          applicationId: "robot",
          accountId: "team",
        }),
      }),
    ),
  );
});

it("does not substitute another application for an unavailable deep link", async () => {
  await show("/admin/api-keys?account=team&application=missing");
  await waitForUi(() =>
    expect(document.body.textContent).toContain(
      "This application is unavailable",
    ),
  );
  expect(mocks.card).not.toHaveBeenCalled();
  expect(mocks.api).not.toHaveBeenCalledWith(
    "platform/applications/credentials/list",
    expect.anything(),
  );
});

it("shows suspended accounts without making directory requests that require active access", async () => {
  await show("/admin/api-keys?account=suspended");
  expect(document.body.textContent).toContain("This account is suspended");
  expect(mocks.api).not.toHaveBeenCalled();
});
