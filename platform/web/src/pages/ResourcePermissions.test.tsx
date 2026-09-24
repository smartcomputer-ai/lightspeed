// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { UniverseAction } from "@lightspeed-ai/agent-client";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { WorkspacesPage } from "./WorkspacesPage";
import { McpServersPage } from "./McpServersPage";
import { EnvironmentsPage } from "./EnvironmentsPage";
import { ChannelsPage } from "./ChannelsPage";
import { SecretsPage } from "./SecretsPage";
import { IntegrationsPage } from "./IntegrationsPage";
import { ApiKeysPage } from "./ApiKeysPage";
import { MembersPage } from "./MembersPage";
import { GeneralSettingsPage } from "./GeneralSettingsPage";
import { SetupsPage } from "./SetupsPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
// Deliberately leave both local role and deployment-admin hints elevated: core action decisions win.
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "admin", slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let actions: UniverseAction[];
// Decisions for each named workspace, environment or MCP server.
let resourceActions: UniverseAction[];
const workspace = { workspaceId: "docs", displayName: "Documents", files: 1, revision: 1, head: "snapshot" };
const provider = {
  credentialId: "model-key", providerId: "openai", displayName: "Internal model", config: { type: "modelApiKey" },
  status: "active", hasCredential: true, usableForModels: true, createdAtMs: 0, updatedAtMs: 0,
};
const environment = {
  environmentId: "computer", displayName: "Research computer", source: { type: "provisioned", providerId: "provider", bindingId: "binding" },
  incarnation: { powerStates: ["running", "paused", "stopped"] }, status: "ready", desiredPower: "running", updatedAtMs: 0,
};
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  actions = ["read"];
  resourceActions = ["read"];
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string, body?: { resources?: unknown[] }) => {
    if (path.endsWith("/access")) return { actions, resources: (body?.resources ?? []).map((resource) => ({ resource, actions: resourceActions })) };
    if (path.endsWith("/workspaces")) return [workspace];
    if (path.endsWith("/tree")) return { workspace, manifest: { root: { entries: { "notes.txt": { kind: "file", blob_ref: "text", size_bytes: 5, media_type: "text/plain" } } } } };
    if (path.endsWith("/workspaces/docs/files/notes.txt")) return { bytesBase64: btoa("hello") };
    if (path.endsWith("/mcp-servers")) return [{ serverId: "tools", displayName: "Research tools", serverUrl: "https://example.test/mcp", authPolicy: { type: "requiredOAuth" }, status: "needsAuthConfig" }];
    if (path.endsWith("/auth-grants")) return [];
    if (path.endsWith("/secrets")) return { providers: [provider], grants: [] };
    if (path.endsWith("/environments")) return [environment];
    if (path.endsWith("/environments/hints")) return { devEnvdEndpoint: null };
    if (/\/(environment-provider-bindings|environment-templates|environment-registration-keys|credentials)$/.test(path)) return [];
    if (path.endsWith("/channel-accounts")) return { accounts: [{ accountId: "telegram", displayName: "Telegram account", provider: "telegram", providerAccountId: "helper", enabled: true, config: { type: "telegram", botUsername: "helper" } }] };
    if (path.endsWith("/channel-pairings")) return { pairings: [] };
    if (path.endsWith("/channel-status")) return { accounts: [] };
    if (path.endsWith("/integrations/github")) return { apps: [], grants: [] };
    if (path.endsWith("/integrations/subscriptions")) return [];
    if (path.includes("/models")) return { models: [] };
    if (path.endsWith("/api-keys")) return [{ keyPrefix: "lsk_own", displayName: "My key", createdAtMs: 0, principalId: "own" }];
    if (path.endsWith("/key-principals")) return [{ id: "own", displayName: "My account", kind: "user" }];
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
async function settle() {
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function show(page: ReactNode, path = "/") {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter initialEntries={[path]}>
      <Routes><Route path="*" element={page} /><Route path="/workspaces/:workspaceId/files/*" element={page} /></Routes>
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
function button(label: string) {
  return [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
}

it("keeps workspace text read-only when this workspace may not be used", async () => {
  // A universe-wide right to use resources does not reach a restricted workspace.
  actions = ["read", "use_resource", "configure_resource"];
  await show(<WorkspacesPage admin />, "/workspaces/docs/files/notes.txt");
  expect(container.querySelector("textarea")?.value).toBe("hello");
  expect(container.querySelector("textarea")?.readOnly).toBe(true);
  expect(container.querySelector('[aria-label="New workspace"]')).toBeNull();
  expect(container.querySelector('[aria-label="Delete file"]')).toBeNull();
  expect(button("New file")).toBeUndefined();
  expect(container.querySelector('[aria-label="Access and sharing"]')).not.toBeNull();
});
it("offers workspace writes when core allows using that workspace", async () => {
  actions = ["read", "use_resource", "create_workspace"];
  resourceActions = ["read", "use_resource"];
  await show(<WorkspacesPage admin />, "/workspaces/docs/files/notes.txt");
  expect(container.querySelector("textarea")?.readOnly).toBe(false);
  expect(container.querySelector('[aria-label="New workspace"]')).not.toBeNull();
  expect(container.querySelector('[aria-label="Delete file"]')).not.toBeNull();
  expect(button("New file")).toBeDefined();
});
it("shows MCP status and access to readers without OAuth or edit controls", async () => {
  await show(<McpServersPage admin />);
  expect(container.textContent).toContain("Research tools");
  expect(button("Connect")).toBeUndefined();
  expect(button("Add server")).toBeUndefined();
  expect(container.querySelector('[aria-label="Edit tools"]')).toBeNull();
  expect(container.querySelector('[aria-label="Access and sharing"]')).not.toBeNull();
});
it("offers MCP connection and editing when configuration is allowed", async () => {
  actions.push("configure_resource");
  resourceActions = ["read", "configure_resource"];
  await show(<McpServersPage admin />);
  expect(button("Connect")).toBeDefined();
  expect(button("Add server")).toBeDefined();
  expect(container.querySelector('[aria-label="Edit tools"]')).not.toBeNull();
});
it("decides MCP row controls per server, not from the universe role", async () => {
  actions.push("configure_resource");
  await show(<McpServersPage admin />);
  expect(button("Add server")).toBeDefined();
  expect(button("Connect")).toBeUndefined();
  expect(container.querySelector('[aria-label="Edit tools"]')).toBeNull();
});
it("removes an open configuration dialog when current permissions no longer allow editing", async () => {
  actions.push("configure_resource");
  resourceActions = ["read", "configure_resource"];
  await show(<McpServersPage admin />);
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Edit tools"]')!.click());
  await settle();
  expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
  actions = ["read"];
  resourceActions = ["read"];
  await act(async () => { await client.invalidateQueries({ queryKey: ["action-permissions"] }); });
  await settle();
  expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  expect(container.textContent).toContain("Research tools");
  expect(container.querySelector('[aria-label="Edit tools"]')).toBeNull();
});
it("keeps environment details readable without registration, power or credential controls", async () => {
  await show(<EnvironmentsPage admin />);
  expect(container.textContent).toContain("Research computer");
  await act(async () => button("Details")!.click());
  await settle();
  expect(container.textContent).toContain("Environment ID");
  expect(button("Pause")).toBeUndefined();
  expect(button("Idle policy…")).toBeUndefined();
  expect(button("Assign secret")).toBeUndefined();
  expect(mocks.api.mock.calls.some(([, path]) => path.endsWith("/environment-registration-keys"))).toBe(false);
});
it("lets an environment's owner configure it while secrets stay with Operators", async () => {
  resourceActions = ["read", "use_resource", "configure_resource", "share_resource"];
  await show(<EnvironmentsPage admin />);
  expect(container.querySelector('[aria-label="Access and sharing"]')).not.toBeNull();
  await act(async () => button("Details")!.click());
  await settle();
  expect(button("Idle policy…")).toBeDefined();
  expect(button("Assign secret")).toBeUndefined();
});
it("shows channel accounts and secret metadata without modification controls", async () => {
  await show(<ChannelsPage admin />);
  expect(container.textContent).toContain("Telegram account");
  expect(button("Connect channel")).toBeUndefined();
  expect(button("Disable")).toBeUndefined();
  await show(<SecretsPage admin />);
  expect(container.textContent).toContain("Internal model");
  expect(button("Add secret")).toBeUndefined();
  expect(container.querySelector('[aria-label="Remove Internal model"]')).toBeNull();
});
it("retains integration details without replacement or removal forms for readers", async () => {
  await show(<IntegrationsPage admin />);
  expect(button("Add integration")).toBeUndefined();
  const row = [...container.querySelectorAll("tr")].find((node) => node.textContent?.includes("OpenAI (API key)"));
  expect(row).toBeDefined();
  await act(async () => row!.click());
  await settle();
  expect(document.body.textContent).toContain("Credential ID");
  expect(button("Replace key")).toBeUndefined();
  expect(button("Remove key")).toBeUndefined();
});
it("lets a reader manage their own API keys with an authorized principal picker", async () => {
  await show(<ApiKeysPage admin />);
  expect(container.textContent).toContain("My key");
  expect(container.querySelector('[aria-label="Revoke My key"]')).not.toBeNull();
  await act(async () => button("Create key")!.click());
  await settle();
  expect(document.body.textContent).toContain("My account (user)");
  expect(document.body.querySelector('input#api-key-principal')).toBeNull();
  expect(document.body.querySelector('[id="api-key-principal"]')).not.toBeNull();
});
it.each([["members", MembersPage], ["general settings", GeneralSettingsPage], ["templates", SetupsPage]] as const)("keeps %s closed without manage_access even for deployment administrators", async (_name, Page) => {
  actions = ["read", "configure_resource"];
  await show(<Page admin />);
  expect(mocks.api.mock.calls.every(([, path]) => path.endsWith("/access"))).toBe(true);
  expect(button("Add member")).toBeUndefined();
  expect(button("Archive universe")).toBeUndefined();
  expect(button("Install")).toBeUndefined();
});
it("reports permission lookup failure explicitly and exposes no mutations", async () => {
  mocks.api.mockRejectedValue(new Error("Permission service unavailable"));
  await show(<McpServersPage admin />);
  expect(container.textContent).toContain("Permissions unavailable");
  expect(container.textContent).not.toContain("Universe not found");
  expect(button("Add server")).toBeUndefined();
});
