// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiKeysPage } from "./ApiKeysPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "platform-universe" }, slug: "factory", isLoading: false }),
}));
vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: () => true }) }));

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const universeScope = { kind: "universe", universeId: "core-universe" };
const keys = [
  { keyPrefix: "lsk_agent", displayName: "Agent", scope: universeScope, groups: ["session", "blobs/put"], assertActor: false, createdAtMs: 2, createdBy: { kind: "local" } },
  { keyPrefix: "lsk_old", displayName: "Old", scope: universeScope, groups: ["session"], assertActor: false, createdAtMs: 3, revokedAtMs: 4, createdBy: { kind: "local" } },
];
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.api.mockReset().mockImplementation(async (method: string) => method === "POST"
    ? { apiKey: { keyPrefix: "lsk_new" }, secret: "lsk_new_secret" }
    : keys);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function settle() {
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function render() {
  await act(async () => root.render(<QueryClientProvider client={client}><ApiKeysPage admin /></QueryClientProvider>));
  await settle();
}
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const dialogText = () => document.querySelector('[role="dialog"]')?.textContent ?? "";
/// Base UI keeps the id on a hidden input beside the visible checkbox.
const checkbox = (group: string) =>
  document.getElementById(`api-key-group-${group}`)!.parentElement!.querySelector<HTMLElement>('[role="checkbox"]')!;

it("lists active keys with what each may call, and revoked keys only on request", async () => {
  await render();
  const rows = () => [...container.querySelectorAll("tbody tr")].map((row) => row.textContent);
  expect(rows()).toHaveLength(1);
  expect(rows()[0]).toContain("Agent");
  expect(rows()[0]).toContain("Sessions and runs, reading blobs, Uploading blobs");

  const toggle = [...container.querySelectorAll("label")].find((label) => label.textContent?.includes("Show revoked keys (1)"));
  await act(async () => toggle!.querySelector<HTMLElement>('[role="switch"]')!.click());
  await settle();
  expect(rows()).toHaveLength(2);
  expect(rows()[1]).toContain("revoked");
});

it("starts a key as an agent client and sends exactly the groups chosen", async () => {
  await render();
  await act(async () => button("Create key")!.click());
  await settle();
  expect(button("Agent client")!.getAttribute("aria-pressed")).toBe("true");
  expect(checkbox("auth/lease").getAttribute("aria-checked")).toBe("false");

  await act(async () => button("Configuration")!.click());
  await settle();
  expect(button("Configuration")!.getAttribute("aria-pressed")).toBe("true");
  await act(async () => checkbox("auth/lease").click());
  await settle();
  expect(dialogText()).toContain("Custom: only the groups ticked below.");
  expect(dialogText()).toContain("Returns the plaintext access tokens");

  const name = document.querySelector<HTMLInputElement>("#api-key-name")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(name, "Configurator");
    name.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => document.querySelector<HTMLFormElement>('[role="dialog"] form')!.requestSubmit());
  await settle();
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/universes/platform-universe/api-keys", {
    displayName: "Configurator",
    groups: ["profiles", "models", "mcp", "environments", "bots", "channels", "auth", "auth/lease"],
  });
  expect(document.querySelector<HTMLInputElement>("#api-key-secret")?.value).toBe("lsk_new_secret");
});
