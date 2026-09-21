// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { ProfilesPage } from "./ProfilesPage";
import type { ResourceRef, UniverseAction } from "@lightspeed-ai/agent-client";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "contributor" }, slug: "universe", isLoading: false }) }));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
vi.mock("@/components/session/session-config-editor", () => ({ SessionConfigEditor: () => <div data-testid="config-editor">Model configuration editor</div> }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let globalActions: UniverseAction[];
let editableProfiles: Set<string>;
let accessError: boolean;
const profiles = ["own", "other"].map((profileId) => ({ profileId, displayName: profileId, revision: 1, config: { model: { model: "internal-model" } }, instructions: { type: "text", text: "Profile instructions" } }));
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  globalActions = ["read", "create_profile"];
  editableProfiles = new Set(["own"]);
  accessError = false;
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: { resources: ResourceRef[] }) => {
    if (path.endsWith("/access")) {
      if (accessError) throw new Error("Permission lookup unavailable");
      return {
        actions: globalActions,
        resources: (body?.resources ?? []).map((resource) => ({ resource, actions: editableProfiles.has(resource.id) ? ["read", "manage_profile"] : ["read"] })),
      };
    }
    if (method === "GET" && path.endsWith("/profiles")) return profiles;
    if (method === "GET" && path.includes("/profiles/")) return profiles.find((profile) => path.endsWith(`/${profile.profileId}`));
    throw new Error(`Unexpected request: ${method} ${path}`);
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
  await act(async () => { await vi.advanceTimersByTimeAsync(10); });
  await act(async () => { await vi.advanceTimersByTimeAsync(10); });
}
async function show(id = "other") {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter initialEntries={[`/u/universe/profiles/${id}`]}><Routes><Route path="/u/:slug/profiles/:profileId" element={<ProfilesPage admin={true} />} /></Routes></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
it("lets a contributor create profiles while keeping another person's profile read-only", async () => {
  await show();
  expect(container.querySelector('[aria-label="New profile"]')).not.toBeNull();
  expect(container.querySelector('[aria-label="Delete profile"]')).toBeNull();
  expect(container.querySelector<HTMLInputElement>("#profile-display-name")?.readOnly).toBe(true);
  expect(container.textContent).toContain("Profile instructions");
  expect(container.textContent).toContain("internal-model");
  expect(container.querySelector('[data-testid="config-editor"]')).toBeNull();
  expect(mocks.api.mock.calls.some(([, path, body]) => path.endsWith("/access") && body.resources.length === 2)).toBe(true);
});
it("offers mutation controls for an owned profile using the returned target permissions", async () => {
  await show("own");
  expect(container.querySelector('[aria-label="Delete profile"]')).not.toBeNull();
  expect(container.querySelector<HTMLInputElement>("#profile-display-name")?.readOnly).toBe(false);
  expect(container.querySelector('[data-testid="config-editor"]')).not.toBeNull();
});
it("keeps JSON available but read-only for a viewer", async () => {
  globalActions = ["read"];
  editableProfiles.clear();
  await show();
  expect(container.querySelector('[aria-label="New profile"]')).toBeNull();
  const json = [...container.querySelectorAll<HTMLButtonElement>('button')].find((button) => button.textContent === "JSON")!;
  await act(async () => json.click());
  const text = container.querySelector<HTMLTextAreaElement>('[aria-label="Profile JSON"]');
  expect(text?.readOnly).toBe(true);
  expect(text?.value).toContain("internal-model");
});
it("withdraws edit controls when a previously allowed preview fails", async () => {
  await show("own");
  expect(container.querySelector('[aria-label="Delete profile"]')).not.toBeNull();
  accessError = true;
  await act(async () => { await client.invalidateQueries({ queryKey: ["action-permissions"] }); });
  await settle();
  expect(container.querySelector('[aria-label="New profile"]')).toBeNull();
  expect(container.querySelector('[aria-label="Delete profile"]')).toBeNull();
  expect(container.querySelector<HTMLInputElement>("#profile-display-name")?.readOnly).toBe(true);
  expect(container.textContent).toContain("Profile permissions are unavailable. Editing is disabled.");
});
