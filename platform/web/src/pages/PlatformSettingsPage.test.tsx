// @vitest-environment jsdom
import { act } from "react";
import { MemoryRouter } from "react-router-dom";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PlatformSettingsPage } from "./PlatformSettingsPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/lib/platform-api", () => ({ platformApi: mocks.api }));

const settings = [
  { key: "personalOrganizations", value: false, source: "environment", defaultValue: true, environmentVariable: "LIGHTSPEED_PLATFORM_PERSONAL_ORGANIZATIONS", updatedAt: null },
  { key: "personalUniverseQuota", value: 10, source: "platform", defaultValue: 25, environmentVariable: "LIGHTSPEED_PLATFORM_PERSONAL_UNIVERSE_QUOTA", updatedAt: "2026-11-17T00:00:00.000Z" },
  { key: "sharedUniverseQuota", value: 100, source: "default", defaultValue: 100, environmentVariable: "LIGHTSPEED_PLATFORM_SHARED_UNIVERSE_QUOTA", updatedAt: null },
];
let root: Root;
let container: HTMLDivElement;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockImplementation(async (operation: string, params: { key?: string; value?: unknown }) => {
    if (operation === "platform/settings/list") return { settings };
    const setting = settings.find((candidate) => candidate.key === params.key)!;
    return { setting: { ...setting, value: params.value ?? setting.defaultValue, source: params.value === null ? "default" : "platform" } };
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => root.render(
    <MemoryRouter><QueryClientProvider client={client}><PlatformSettingsPage /></QueryClientProvider></MemoryRouter>,
  ));
  await waitForUi(() => expect(container.querySelector("#setting-sharedUniverseQuota")).not.toBeNull());
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});
const field = (key: string) => container.querySelector<HTMLInputElement>(`#setting-${key}`)!;
const section = (key: string) => field(key).closest(".grid")!.parentElement!;
const puts = () => mocks.api.mock.calls.filter(([operation]) => operation === "platform/settings/put");

it("locks a setting the environment holds and names the variable that sets it", async () => {
  const toggle = container.querySelector('[data-slot="switch"]')!;
  expect(toggle.hasAttribute("data-disabled")).toBe(true);
  expect(toggle.hasAttribute("data-checked")).toBe(false);
  expect(container.textContent).toContain("Set by LIGHTSPEED_PLATFORM_PERSONAL_ORGANIZATIONS in the deployment's environment.");
});

it("saves a limit only with its own Save, and resets one changed here to the default", async () => {
  expect(section("sharedUniverseQuota").textContent).toContain("Using the default.");
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field("sharedUniverseQuota"), "40");
    field("sharedUniverseQuota").dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(puts()).toHaveLength(0);
  await act(async () => field("sharedUniverseQuota").form!.requestSubmit());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/settings/put", { key: "sharedUniverseQuota", value: 40 }));

  expect(section("personalUniverseQuota").textContent).toContain("Changed here. The default is 25.");
  const reset = [...section("personalUniverseQuota").querySelectorAll("button")].find((node) => node.textContent === "Reset to default")!;
  await act(async () => reset.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("platform/settings/put", { key: "personalUniverseQuota", value: null }));
  await waitForUi(() => expect(field("personalUniverseQuota").value).toBe("25"));
});
