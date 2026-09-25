// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { ProfilesPage } from "./ProfilesPage";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "operator" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, slug: "universe", isLoading: false }) }));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
vi.mock("@/components/session/session-config-editor", () => ({ SessionConfigEditor: () => <div data-testid="config-editor">Model configuration editor</div> }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const profiles = ["own", "other"].map((profileId) => ({ profileId, displayName: profileId, revision: 1, config: { model: { model: "internal-model" } }, instructions: { type: "text", text: "Profile instructions" } }));
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "operator";
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
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
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter initialEntries={[`/u/universe/profiles/${id}`]}><Routes><Route path="/u/:slug/profiles/:profileId" element={<ProfilesPage admin={true} />} /></Routes></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
it("lets an operator create, edit and delete profiles", async () => {
  await show();
  expect(container.querySelector('[aria-label="New profile"]')).not.toBeNull();
  expect(container.querySelector('[aria-label="Delete profile"]')).not.toBeNull();
  expect(container.querySelector<HTMLInputElement>("#profile-display-name")?.readOnly).toBe(false);
  expect(container.querySelector('[data-testid="config-editor"]')).not.toBeNull();
});
it("keeps profiles readable but not editable for a contributor", async () => {
  mocks.role = "contributor";
  await show();
  expect(container.querySelector('[aria-label="New profile"]')).toBeNull();
  expect(container.querySelector('[aria-label="Delete profile"]')).toBeNull();
  expect(container.querySelector<HTMLInputElement>("#profile-display-name")?.readOnly).toBe(true);
  expect(container.textContent).toContain("Profile instructions");
  expect(container.textContent).toContain("internal-model");
  expect(container.querySelector('[data-testid="config-editor"]')).toBeNull();
});
it("keeps JSON available but read-only for a viewer", async () => {
  mocks.role = "viewer";
  await show();
  expect(container.querySelector('[aria-label="New profile"]')).toBeNull();
  const json = [...container.querySelectorAll<HTMLButtonElement>('button')].find((button) => button.textContent === "JSON")!;
  await act(async () => json.click());
  const text = container.querySelector<HTMLTextAreaElement>('[aria-label="Profile JSON"]');
  expect(text?.readOnly).toBe(true);
  expect(text?.value).toContain("internal-model");
});
