// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiKeysSection } from "./api-keys-section";
import type { ManagedApiKeyOwner as ApiKeyOwner } from "./api-keys";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn(), credentials: [] as unknown[] }));
vi.mock("@/lib/platform-api", () => ({
  platformApi: mocks.api,
  useOrganizations: () => ({ data: { organizations: [{ id: "team", name: "Research" }, { id: "other", name: "Operations" }] } }),
}));
vi.mock("@/lib/universes", () => ({
  useUniverses: () => ({
    data: [
      { lightspeedUniverseId: "universe-one", organizationId: "team", name: "Development", handle: "dev", status: "active" },
      { lightspeedUniverseId: "universe-two", organizationId: "other", name: "Production", handle: "prod", status: "active" },
    ],
  }),
}));

const key = (overrides: Record<string, unknown> = {}) => ({
  id: "key-1", name: "Deploy", prefix: "lsp_", start: "lsp_abcd", enabled: true, createdAt: "2026-11-01T00:00:00.000Z",
  expiresAt: null, lastUsedAt: null, restrictions: { organizationIds: ["team"], readOnly: true }, ...overrides,
});
const robot: ApiKeyOwner = { kind: "serviceUser", organizationId: "team", serviceUserId: "robot", name: "Build robot", enabled: true };

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener: () => {}, removeEventListener: () => {} }));
  mocks.credentials = [];
  mocks.api.mockReset().mockImplementation(async (operation: string) =>
    operation.endsWith("/list") ? { credentials: mocks.credentials }
      : operation.endsWith("/revoke") ? { revoked: true }
        : { secret: "one-time-secret", credential: key() });
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

async function show(owner: ApiKeyOwner = { kind: "personal" }) {
  await act(async () => root.render(<QueryClientProvider client={client}><ApiKeysSection owner={owner} /></QueryClientProvider>));
}
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const click = (label: string) => act(async () => button(label)!.click());
async function type(label: string, value: string) {
  const input = document.body.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function choose(label: string, option: string) {
  await act(async () => document.body.querySelector<HTMLElement>(`[aria-label="${label}"]`)!.click());
  const item = [...document.querySelectorAll<HTMLElement>('[data-slot="select-content"]:not([data-closed]) [role="option"]')]
    .find((node) => node.textContent?.trim() === option);
  if (!item) throw new Error(`Missing option ${option}`);
  await act(async () => item.click());
}
const create = () => act(async () => document.body.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());

it("creates a personal key limited to one organization by default and shows the secret once, outside every cache", async () => {
  await show();
  await click("New key");
  await type("API key name", "  Local automation  ");
  await create();
  await waitForUi(() => expect(document.body.textContent).toContain("one-time-secret"));
  expect(mocks.api).toHaveBeenCalledWith("platform/credentials/create", {
    name: "Local automation",
    restrictions: { organizationIds: ["team"], readOnly: false, allowUniverseCreation: false },
  });
  const cached = JSON.stringify([...client.getQueryCache().getAll().map((query) => query.state), ...client.getMutationCache().getAll().map((mutation) => mutation.state)]);
  expect(cached).not.toContain("one-time-secret");
  await click("Done");
  await waitForUi(() => expect(document.body.textContent).not.toContain("one-time-secret"));
});

it("needs an explicit choice for every organization, and for a key that never expires", async () => {
  await show();
  await click("New key");
  await type("API key name", "Everywhere");
  await choose("Organization access", "All my organizations");
  await choose("API key expiry", "No expiry");
  await create();
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/credentials/create", {
    name: "Everywhere", expiresInDays: null, restrictions: { readOnly: false, allowUniverseCreation: false },
  }));
});

it("refuses an empty universe selection and turns off universe creation for read-only keys", async () => {
  await show();
  await click("New key");
  await type("API key name", "Limited");
  await choose("Universe access", "Selected universes");
  expect(document.body.querySelector<HTMLButtonElement>('[role="dialog"] button[type="submit"]')!.disabled).toBe(true);
  await choose("Universe access", "Every universe I can access there");
  // The form's switches, in order: Read only, then Allow creating universes.
  const switches = () => [...document.body.querySelectorAll<HTMLElement>('[role="dialog"] [data-slot="switch"]')];
  await act(async () => switches()[1]!.click());
  expect(switches()[1]!.hasAttribute("data-checked")).toBe(true);
  await act(async () => switches()[0]!.click());
  expect(switches()[1]!.hasAttribute("data-checked")).toBe(false);
  expect(switches()[1]!.hasAttribute("data-disabled")).toBe(true);
});

it("pins a service user's key to its organization without offering others", async () => {
  await show(robot);
  await click("New key");
  expect(document.body.querySelector('[aria-label="Organization access"]')).toBeNull();
  await type("API key name", "CI");
  await create();
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/service-users/credentials/create", {
    name: "CI", organizationId: "team", serviceUserId: "robot",
    restrictions: { organizationIds: ["team"], readOnly: false, allowUniverseCreation: false },
  }));
});

it("lists keys as rows that open their details, where a key is revoked only after confirming", async () => {
  mocks.credentials = [key()];
  await show();
  await waitForUi(() => expect(container.querySelectorAll("tbody tr")).toHaveLength(1));
  const row = container.querySelector("tbody tr")!;
  expect(row.textContent).toContain("Never");
  expect(row.textContent).toContain("read only");
  await act(async () => (row.querySelector("td") as HTMLElement).click());
  expect(document.body.querySelector('[role="dialog"]')!.textContent).toContain("Last used");
  await click("Revoke key");
  expect(mocks.api.mock.calls.some(([operation]) => operation === "platform/credentials/revoke")).toBe(false);
  const confirm = [...document.body.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button')].find((node) => node.textContent?.trim() === "Revoke key");
  await act(async () => confirm!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/credentials/revoke", { keyId: "key-1" }));
});

it("keeps a disabled service user's keys listed but offers no new key", async () => {
  await show({ ...robot, enabled: false } as ApiKeyOwner);
  expect(button("New key")!.disabled).toBe(true);
  expect(container.textContent).toContain("This service user is disabled");
});
