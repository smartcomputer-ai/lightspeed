// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SecretGrant } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { CredentialsPage, isReusableCredential } from "./CredentialsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "operator", slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));

function grant(grantId: string, providerKind: string, extra: Partial<SecretGrant> = {}): SecretGrant {
  return {
    grantId, providerId: grantId, providerKind, displayName: `${grantId} credential`, status: "active", exposure: "brokered",
    principal: {}, hasAccessToken: true, hasRefreshToken: false, leaseCount: 0, createdAtMs: 1, updatedAtMs: 1, ...extra,
  };
}
const reusable = [
  grant("bearer", "staticBearer"),
  grant("environment", "staticBearer", { providerId: "environment-secret" }),
  grant("installation", "gitHubApp", { providerId: "github-app:1" }),
  grant("oauth", "customOAuth", { hasRefreshToken: true }),
];
const hidden = [
  grant("claude", "staticBearer", { providerId: "anthropic", metadata: { subscription: "claudeCode" } }),
  grant("telegram-bot", "staticBearer", { providerId: "telegram" }),
  grant("mcp-login", "mcpOAuth"),
  grant("model-login", "modelOAuth"),
  grant("model-key", "modelApiKey"),
  grant("model-endpoint", "modelEndpoint"),
];
const app = {
  providerId: "github-app:1", providerKind: "gitHubApp", displayName: "Acme App", hasCredential: true, status: "active",
  config: { type: "githubApp", appId: "1", apiBaseUrl: "https://api.github.com" }, createdAtMs: 1, updatedAtMs: 1,
};
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions: ["read", "configure_resource"], resources: [] };
    if (path.endsWith("/secrets")) return { providers: [], grants: [...hidden, ...reusable] };
    if (path.endsWith("/integrations/github")) return { apps: [app], grants: [reusable[2]] };
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

it("lists pasted tokens, GitHub App installations and custom OAuth, not credentials made for one thing", () => {
  expect(reusable.every(isReusableCredential)).toBe(true);
  expect(hidden.some(isReusableCredential)).toBe(false);
});

it("lists reusable credentials only and points model and MCP logins at their pages", async () => {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter>
      <CredentialsPage admin={false} />
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });

  const table = container.querySelector("table")!;
  expect(table.querySelectorAll("tbody tr")).toHaveLength(reusable.length);
  for (const credential of reusable) expect(table.textContent).toContain(`${credential.grantId} credential`);
  for (const credential of hidden) expect(container.textContent).not.toContain(`${credential.grantId} credential`);
  expect(table.textContent).toContain("Environment secret");
  expect(table.textContent).toContain("GitHub App");

  const links = Object.fromEntries([...container.querySelectorAll("a")].map((link) => [link.textContent, link.getAttribute("href")]));
  expect(links).toMatchObject({ Models: "/u/test/models", "MCP servers": "/u/test/mcp-servers" });
  expect(container.textContent).toContain("GitHub Apps");
  expect(container.textContent).toContain("Acme App");
  expect(container.textContent).toContain("1 installation granted");
  expect(container.querySelector('[aria-label="Revoke bearer credential"]')).not.toBeNull();
});
