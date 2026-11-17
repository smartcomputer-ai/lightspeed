// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiKeysCard } from "./api-keys-card";
import { waitForUi } from "@/test/wait-for-ui";

const api = vi.hoisted(() => vi.fn());
vi.mock("@/lib/platform-api", () => ({
  platformApi: api,
  useOrganizations: () => ({
    data: {
      organizations: [
        { id: "team", enabled: true, name: "Research" },
        { id: "other", enabled: true, name: "Operations" },
      ],
    },
  }),
}));
vi.mock("@/lib/universes", () => ({
  useUniverses: () => ({
    data: [
      {
        lightspeedUniverseId: "universe-one",
        organizationId: "team",
        name: "Development",
        handle: "dev",
        status: "active",
      },
      {
        lightspeedUniverseId: "universe-two",
        organizationId: "other",
        name: "Production",
        handle: "prod",
        status: "active",
      },
      {
        lightspeedUniverseId: "universe-old",
        organizationId: "team",
        name: "Archived",
        handle: "old",
        status: "archived",
      },
    ],
  }),
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
  api
    .mockReset()
    .mockImplementation(async (operation) =>
      operation.endsWith("/list")
        ? { credentials: [] }
        : { secret: "one-time-secret", credential: { id: "key" } },
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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function show(serviceUser?: {
  organizationId: string;
  serviceUserId: string;
  name?: string;
  enabled?: boolean;
}) {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <ApiKeysCard serviceUser={serviceUser} />
      </QueryClientProvider>,
    ),
  );
  if (serviceUser?.enabled !== false) await click("New API key");
}
async function type(label: string, value: string) {
  const input = document.body.querySelector<HTMLInputElement>(
    `[aria-label="${label}"]`,
  )!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function select(label: string, value: string) {
  const trigger = document.body.querySelector<HTMLElement>(`[aria-label="${label}"]`)!;
  await act(async () => trigger.click());
  const prefix = value === "selected" ? "Selected" : "All";
  const option = [...document.querySelectorAll<HTMLElement>('[data-slot="select-content"]:not([data-closed]) [role="option"]')]
    .find(item => item.textContent?.trim().startsWith(prefix));
  if (!option) throw new Error(`Missing ${prefix} option`);
  await act(async () => option.click());
}
function checkbox(label: string) {
  return [...document.body.querySelectorAll("label")]
    .find((element) => element.textContent === label)!
    .querySelector<HTMLInputElement>('input[type="checkbox"]')!;
}
async function check(label: string) {
  await act(async () => checkbox(label).click());
}
async function click(label: string) {
  await act(async () =>
    [...document.body.querySelectorAll("button")]
      .find((button) => button.textContent === label)!
      .click(),
  );
}
function submit() {
  return document.body.querySelector<HTMLButtonElement>(
    'button[type="submit"]',
  )!;
}

it("uses deployment expiry defaults and keeps the one-time secret out of query and mutation caches", async () => {
  await show();
  await type("API key name", "  Local automation  ");
  await click("Create API key");
  await waitForUi(() =>
    expect(document.body.textContent).toContain("one-time-secret"),
  );
  expect(api).toHaveBeenCalledWith("platform/credentials/create", {
    name: "Local automation",
    restrictions: { organizationIds: ["team"], readOnly: false, allowUniverseCreation: false },
  });
  expect(submit()).toBeNull();
  expect(
    JSON.stringify(
      client
        .getQueryCache()
        .getAll()
        .map((query) => query.state),
    ),
  ).not.toContain("one-time-secret");
  expect(
    JSON.stringify(
      client
        .getMutationCache()
        .getAll()
        .map((mutation) => mutation.state),
    ),
  ).not.toContain("one-time-secret");
  await click("I have saved it");
  await waitForUi(() =>
    expect(document.body.textContent).not.toContain("one-time-secret"),
  );
});

it("selects named organization and universe restrictions and rejects empty selections", async () => {
  await show();
  await type("API key name", "Research script");
  await select("Organization access", "selected");
  await check("Research");
  expect(submit().disabled).toBe(true);
  await check("Research");
  await select("Universe access", "selected");
  expect(submit().disabled).toBe(true);
  expect(document.body.textContent).not.toContain("Production · prod");
  expect(document.body.textContent).not.toContain("Archived · old");
  await check("Development · dev");
  expect(submit().disabled).toBe(false);
  await click("Create API key");
  await waitForUi(() =>
    expect(api).toHaveBeenCalledWith("platform/credentials/create", {
      name: "Research script",
      restrictions: {
        organizationIds: ["team"],
        universeIds: ["universe-one"],
        readOnly: false,
        allowUniverseCreation: false,
      },
    }),
  );
});

it("creates a named service user credential restricted to its own organization", async () => {
  await show({
    organizationId: "team",
    serviceUserId: "robot",
    name: "Release bot",
  });
  expect(document.body.textContent).toContain("These keys act as Release bot");
  expect(
    document.body.querySelector('[aria-label="Organization access"]'),
  ).toBeNull();
  await type("API key name", "CI");
  await type("API key expiry in days", "14");
  await select("Universe access", "selected");
  await check("Development · dev");
  expect(document.body.textContent).not.toContain("Production · prod");
  await type(
    "Allowed operations",
    "profiles/list, profiles/read, profiles/list",
  );
  await click("Create API key");
  await waitForUi(() =>
    expect(api).toHaveBeenCalledWith(
      "platform/service-users/credentials/create",
      {
        organizationId: "team",
        serviceUserId: "robot",
        name: "CI",
        expiresInDays: 14,
        restrictions: {
          organizationIds: ["team"],
          universeIds: ["universe-one"],
          operations: ["profiles/list", "profiles/read"],
          readOnly: false,
          allowUniverseCreation: false,
        },
      },
    ),
  );
});

it("clears universe creation permission when the key becomes read only or restricted to existing universes", async () => {
  await show();
  await check("Allow universe creation");
  await check("Read only");
  expect(checkbox("Allow universe creation").checked).toBe(false);
  expect(checkbox("Allow universe creation").disabled).toBe(true);
  await check("Read only");
  await check("Allow universe creation");
  await select("Universe access", "selected");
  expect(checkbox("Allow universe creation").checked).toBe(false);
  expect(checkbox("Allow universe creation").disabled).toBe(true);
  await select("Universe access", "all");
  expect(checkbox("Allow universe creation").checked).toBe(false);
});

it("shows named restrictions and lifecycle states and permits revocation for disabled service users", async () => {
  const base = {
    start: "ls_123",
    restrictions: {
      organizationIds: ["team"],
      universeIds: ["universe-one"],
      readOnly: true,
      allowUniverseCreation: false,
    },
  };
  api.mockImplementation(async (operation) =>
    operation.endsWith("/list")
      ? {
          credentials: [
            {
              ...base,
              id: "live",
              name: "Live",
              enabled: true,
              expiresAt: "2999-01-01",
            },
            {
              ...base,
              id: "expired",
              name: "Old",
              enabled: true,
              expiresAt: "2000-01-01",
            },
            {
              ...base,
              id: "revoked",
              name: "Revoked key",
              enabled: false,
              expiresAt: "2999-01-01",
            },
          ],
        }
      : {},
  );
  await show({
    organizationId: "team",
    serviceUserId: "robot",
    name: "Release bot",
    enabled: false,
  });
  await waitForUi(() =>
    expect(document.body.textContent).toContain("Service user disabled"),
  );
  expect(document.body.textContent).toContain("Organizations: Research");
  expect(document.body.textContent).toContain("Universes: Development (dev)");
  expect(document.body.textContent).toContain("Expired");
  expect(document.body.textContent).toContain("Revoked");
  expect(submit()).toBeNull();
  const buttons = [...document.body.querySelectorAll("button")].filter(
    (button) => button.textContent === "Revoke",
  );
  expect(buttons.map((button) => button.disabled)).toEqual([
    false,
    false,
    true,
  ]);
  await act(async () => buttons[0]!.click());
  expect(api).not.toHaveBeenCalledWith(
    "platform/service-users/credentials/revoke",
    expect.anything(),
  );
  await click("Revoke key");
  await waitForUi(() =>
    expect(api).toHaveBeenCalledWith(
      "platform/service-users/credentials/revoke",
      { organizationId: "team", serviceUserId: "robot", keyId: "live" },
    ),
  );
});

it("discards a one-time secret when switching the credential owner", async () => {
  await show();
  await type("API key name", "Personal");
  await click("Create API key");
  await waitForUi(() =>
    expect(document.body.textContent).toContain("one-time-secret"),
  );
  await show({
    organizationId: "team",
    serviceUserId: "robot",
    name: "Release bot",
  });
  expect(document.body.textContent).not.toContain("one-time-secret");
  expect(
    document.body.querySelector<HTMLInputElement>(
      '[aria-label="API key name"]',
    )!.value,
  ).toBe("");
});

it("does not describe empty credential restrictions as unrestricted access", async () => {
  api.mockResolvedValue({
    credentials: [
      {
        id: "none",
        name: "No access",
        enabled: true,
        start: "ls_123",
        expiresAt: "2999-01-01",
        restrictions: { organizationIds: [], universeIds: [], operations: [] },
      },
    ],
  });
  await show();
  await waitForUi(() =>
    expect(document.body.textContent).toContain("Organizations: None"),
  );
  expect(document.body.textContent).toContain("Universes: None");
  expect(document.body.textContent).toContain("Operations: None");
});

it("opens a bounded scrollable dialog and resets an abandoned draft", async () => {
  await show();
  const dialog = document.querySelector('[role="dialog"]')!;
  expect(dialog.className).toContain("max-h-[min(92dvh,800px)]");
  expect(dialog.className).toContain("overflow-y-auto");
  await type("API key name", "Abandoned");
  await check("Allow universe creation");
  await click("Cancel");
  await click("New API key");
  expect(
    document.querySelector<HTMLInputElement>('[aria-label="API key name"]')!
      .value,
  ).toBe("");
  expect(checkbox("Allow universe creation").checked).toBe(false);
});

it("requires an explicit all-organizations choice to create an unrestricted user key", async () => {
  await show();
  expect(document.body.querySelector<HTMLButtonElement>('[aria-label="Organization access"]')!.querySelector('[data-slot="select-value"]')?.textContent).toBe("Selected organizations");
  expect(checkbox("Research").checked).toBe(true);
  await select("Organization access", "all");
  await type("API key name", "CLI everywhere");
  await click("Create API key");
  await waitForUi(() => expect(api).toHaveBeenCalledWith("platform/credentials/create", {
    name: "CLI everywhere", restrictions: { readOnly: false, allowUniverseCreation: false },
  }));
});
