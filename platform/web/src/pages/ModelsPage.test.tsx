// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { ModelsPage } from "./ModelsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "operator", slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));

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
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions: ["read", "configure_resource"], resources: [] };
    if (path.endsWith("/secrets")) return { providers: [legacy], grants: [subscription] };
    if (path.endsWith("/integrations/subscriptions")) return [subscription];
    if (path.endsWith("/models")) return { models: [], providers: [{ providerId: "openai", apiKinds: [], credential: "configured", credentialSource: "deployment" }] };
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
  expect(container.textContent).toContain("Operators add them; keys and logins are never shown again.");
  expect(container.textContent).toContain("A legacy credential below has an incorrect internal ID");
  const rows = [...container.querySelectorAll("tbody tr")].map((row) => row.textContent);
  expect(rows).toHaveLength(2);
  expect(rows[0]).toContain("Anthropic (API key)");
  expect(rows[0]).toContain("needs attention");
  expect(rows[1]).toContain("Team Max");
  expect(rows[1]).toContain("Claude Code (subscription)");
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
