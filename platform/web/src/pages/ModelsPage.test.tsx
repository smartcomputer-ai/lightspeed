// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { ModelsPage } from "./ModelsPage";
import { ApiError, type ModelDefaults } from "@/api";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "operator" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role, slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));

const legacy = {
  providerId: "anthropic", credentialId: "anthropic", usableForModels: false, providerKind: "modelApiKey",
  config: { type: "modelApiKey" }, hasCredential: true, status: "active", createdAtMs: 1, updatedAtMs: 1,
};
const subscription = {
  grantId: "claude", providerId: "anthropic", providerKind: "staticBearer", displayName: "Team Max", status: "active",
  exposure: "brokered", createdBy: { kind: "local" }, hasAccessToken: true, hasRefreshToken: false, leaseCount: 0,
  metadata: { subscription: "claudeCode" }, createdAtMs: 1, updatedAtMs: 1,
};
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let defaults: ModelDefaults;
let actions: string[];
let discoveryFails: boolean;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  defaults = { revision: 7, agentRun: { providerId: "openai", apiKind: "openai:responses", model: "gpt-6-sol" }, speechToText: null };
  actions = ["read", "configure_resource"];
  discoveryFails = false;
  mocks.role = "operator";
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: { slot: "agentRun" | "speechToText"; expectedRevision: number; model: ModelDefaults["agentRun"] }) => {
    if (path.endsWith("/access")) return { actions, resources: [] };
    if (path.endsWith("/models/defaults")) {
      if (method === "PUT") {
        if (body!.expectedRevision !== defaults.revision) throw new ApiError(409, { error: "defaults changed" });
        defaults = { ...defaults, revision: defaults.revision + 1, [body!.slot]: body!.model };
      }
      return defaults;
    }
    if (path.endsWith("/secrets")) return { providers: [legacy], grants: [subscription] };
    if (path.endsWith("/integrations/subscriptions")) return [subscription];
    if (path.endsWith("/integrations/model-keys")) return { ...legacy, providerId: "openai", credentialId: "model:openai", usableForModels: true };
    if (path.endsWith("/models")) {
      if (discoveryFails) throw new Error("discovery unavailable");
      return { models: [{ providerId: "openai", apiKind: "openai:audio-transcriptions", model: "speech-model", capabilities: {} }, { providerId: "openai", apiKind: "openai:responses", model: "agent-model", capabilities: {} }], providers: [{ providerId: "openai", apiKinds: ["openai:responses", "openai:audio-transcriptions"], credential: "configured", credentialSource: "deployment" }] };
    }
    throw new Error(`Unexpected request: ${path}`);
  });
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
async function show(path = "/") {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter initialEntries={[path]}>
      <ModelsPage admin={false} />
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
function button(label: string) {
  return [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
}

it("lists model providers and subscriptions with the legacy-credential warning", async () => {
  await show();
  expect(container.querySelector("h1")?.textContent).toBe("Models");
  expect(container.textContent).toContain("Operators manage them; keys and logins are never shown again.");
  expect(container.textContent).toContain("A legacy credential below has an incorrect internal ID");
  const rows = [...container.querySelectorAll("tbody tr")].map((row) => row.textContent);
  expect(rows).toHaveLength(2);
  expect(rows[0]).toContain("Anthropic (API key)");
  expect(rows[0]).toContain("needs attention");
  expect(rows[1]).toContain("Team Max");
  expect(rows[1]).toContain("Claude Code (subscription)");
});

async function click(label: string) {
  await act(async () => button(label)!.click());
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function enterModel(value: string) {
  const input = document.body.querySelector<HTMLInputElement>('input[placeholder="Model name"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it("saves an unlisted model while discovery is unavailable, then explicitly clears it", async () => {
  discoveryFails = true;
  await show();
  expect(container.textContent).toContain("OpenAI · gpt-6-sol");
  expect(container.textContent).toContain("Provider status could not be checked");
  await click("Change");
  await enterModel("private-model");
  await click("Save default");
  expect(defaults).toMatchObject({ revision: 8, agentRun: { model: "private-model" } });
  expect(container.textContent).toContain("OpenAI · private-model");
  await click("Clear");
  expect(defaults).toMatchObject({ revision: 9, agentRun: null });
  expect(container.textContent).toContain("No default selected");
  const writes = mocks.api.mock.calls.filter(([method]) => method === "PUT");
  expect(writes.map(([, , body]) => body.expectedRevision)).toEqual([7, 8]);
  expect(writes[1]?.[2]).toEqual({ slot: "agentRun", model: null, expectedRevision: 8 });
});

it("keeps a stale draft on conflict until the user reloads the saved choice", async () => {
  await show();
  await click("Change");
  await enterModel("my-edit");
  defaults = { ...defaults, revision: 8, agentRun: { ...defaults.agentRun!, model: "concurrent-edit" } };
  await click("Save default");
  expect(document.body.textContent).toContain("Defaults changed elsewhere");
  expect(document.body.querySelector<HTMLInputElement>('input[placeholder="Model name"]')!.value).toBe("my-edit");
  expect(mocks.api.mock.calls.filter(([method]) => method === "PUT")).toHaveLength(1);
  await click("Reload saved default");
  expect(document.body.querySelector<HTMLInputElement>('input[placeholder="Model name"]')!.value).toBe("concurrent-edit");
  await enterModel("reviewed-edit");
  await click("Save default");
  expect(defaults).toMatchObject({ revision: 9, agentRun: { model: "reviewed-edit" } });
});

it("shows defaults without edit controls to a read-only member", async () => {
  actions = ["read"];
  mocks.role = "viewer";
  await show();
  expect(container.textContent).toContain("OpenAI · gpt-6-sol");
  expect(button("Change")).toBeUndefined();
  expect(button("Clear")).toBeUndefined();
  expect(button("Add provider")).toBeUndefined();
});

it("offers model providers only when adding", async () => {
  await show();
  await act(async () => button("Add provider")!.click());
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  const dialog = document.body.querySelector('[role="dialog"]')!;
  expect(dialog.textContent).toContain("Add model provider");
  for (const name of ["OpenAI (API key)", "Anthropic (API key)", "OpenAI-compatible provider", "Claude Code (subscription)", "Codex (ChatGPT subscription)"]) {
    expect(dialog.textContent).toContain(name);
  }
  expect(dialog.textContent).not.toContain("GitHub");
});

it("opens the requested provider form from a readiness deep link", async () => {
  await show("/?add=openAiApiKey");
  const dialog = document.body.querySelector('[role="dialog"]')!;
  expect(dialog.textContent).toContain("OpenAI (API key)");
  expect(dialog.querySelector("input[type=password]")).not.toBeNull();
});

it("offers default selection after adding a provider without changing it automatically", async () => {
  await show("/?add=openAiApiKey");
  const input = document.body.querySelector<HTMLInputElement>('input[type="password"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "test-key");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const form = input.closest("form")!;
  await act(async () => form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(button("Choose default model")).toBeDefined();
  expect(mocks.api.mock.calls.filter(([method]) => method === "PUT")).toHaveLength(0);
  await click("Choose default model");
  expect(document.body.textContent).toContain("Default model for agent runs");
  expect(document.body.querySelector<HTMLInputElement>('input[placeholder="Model name"]')!.value).toBe("gpt-6-sol");
});

it("configures speech separately from the agent default and clears only speech", async () => {
  await show();
  const row = () => container.querySelector('[role="group"][aria-label="Speech-to-text"]')!;
  expect(row().textContent).toContain("No default selected");
  await act(async () => row().querySelector<HTMLButtonElement>("button")!.click());
  const provider = document.body.querySelector<HTMLInputElement>('input[placeholder="Provider ID"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(provider, "openai");
    provider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await enterModel("speech-model");
  await click("Save default");
  expect(defaults).toMatchObject({ revision: 8, agentRun: { model: "gpt-6-sol" }, speechToText: { providerId: "openai", apiKind: "openai:audio-transcriptions", model: "speech-model" } });
  expect(row().textContent).toContain("Provider configured");
  await act(async () => [...row().querySelectorAll("button")].find((button) => button.textContent === "Clear")!.click());
  expect(defaults).toMatchObject({ revision: 9, agentRun: { model: "gpt-6-sol" }, speechToText: null });
  expect(mocks.api.mock.calls.filter(([method]) => method === "PUT").map(([, , body]) => body.slot)).toEqual(["speechToText", "speechToText"]);
});
