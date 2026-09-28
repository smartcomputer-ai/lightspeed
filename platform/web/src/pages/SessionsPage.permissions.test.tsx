// @vitest-environment jsdom
import { act, Children, isValidElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionsPage } from "./SessionsPage";
import { PermissionIdentityProvider } from "@/lib/permissions";
import type { ModelDefaults } from "@/api";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "contributor" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, slug: "universe", isLoading: false }) }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
vi.mock("@/components/ui/select", () => {
  // Exercise the page's selection behavior without popup positioning in jsdom.
  const SelectItem = () => null;
  const Select = ({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) => {
    const options = (nodes: ReactNode): ReactNode => Children.map(nodes, (node) => {
      if (!isValidElement<{ value?: string; children?: ReactNode }>(node)) return null;
      return node.type === SelectItem ? <option value={node.props.value}>{node.props.children}</option> : options(node.props.children);
    });
    return <select value={value} onChange={(event) => onValueChange(event.target.value)}>{options(children)}</select>;
  };
  return { Select, SelectItem, SelectContent: () => null, SelectTrigger: () => null, SelectValue: () => null };
});
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let defaults: ModelDefaults;
const sessions = [
  { id: "own", access: { visibility: "restricted", createdBy: { kind: "actor", id: "user" } } },
  { id: "other", access: { visibility: "universe", createdBy: { kind: "actor", id: "someone" } } },
].map((session) => ({ ...session, displayName: session.id, lifecycleStatus: "open", managed: false, createdAtMs: 0, updatedAtMs: 0, retention: { rootSessionId: session.id } }));
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  window.localStorage.clear();
  mocks.role = "contributor";
  defaults = { revision: 1, agentRun: { providerId: "openai", apiKind: "openai:responses", model: "gpt-6-sol" }, speechToText: null };
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (path.includes("/sessions?")) return { sessions };
    if (path.endsWith("/models/defaults")) return defaults;
    if (path.endsWith("/profiles")) return [{ profileId: "custom", displayName: "Custom model" }];
    if (path.endsWith("/profiles/custom")) return { profileId: "custom", config: { model: { providerId: "anthropic", apiKind: "anthropic:messages", model: "profile-model" } } };
    if (method === "POST" && path.endsWith("/sessions")) return { id: "created" };
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
async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter><SessionsPage admin={false} /></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>));
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
it("keeps the session list readable without showing create or bulk controls to a viewer", async () => {
  mocks.role = "viewer";
  await show();
  expect(container.textContent).toContain("own");
  expect(container.textContent).toContain("other");
  expect(container.querySelector('[aria-label="New session"]')).toBeNull();
  expect(container.querySelector('[aria-label="Select sessions"]')).toBeNull();
});
it("leaves managed work to its manager unless asked", async () => {
  await show();
  const listed = mocks.api.mock.calls.map(([, path]) => String(path)).find((path) => path.includes("/sessions?"));
  expect(listed).toContain("managed=false");
});
it("marks shared work in the list and leaves private work unmarked", async () => {
  await show();
  const rows = [...container.querySelectorAll("a")].filter((link) => link.textContent?.includes("own") || link.textContent?.includes("other"));
  expect(rows.find((row) => row.textContent?.includes("own"))?.querySelector('[aria-label="Shared"]')).toBeNull();
  expect(rows.find((row) => row.textContent?.includes("other"))?.querySelector('[aria-label="Shared"]')).not.toBeNull();
});
it("offers bulk actions to a contributor over the listed sessions", async () => {
  await show();
  expect(container.querySelector('[aria-label="New session"]')).not.toBeNull();
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Select sessions"]')!.click());
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Select all listed sessions"]')!.click());
  expect(container.textContent).toContain("2 selected");
  expect([...container.querySelectorAll("button")].some((button) => button.textContent === "Close 2")).toBe(true);
});

async function openCreate() {
  await show();
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="New session"]')!.click());
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  return document.body.querySelector<HTMLElement>('[role="dialog"]')!;
}

it("previews the universe route while leaving default resolution to session creation", async () => {
  const dialog = await openCreate();
  expect(dialog.textContent).toContain("Universe default: OpenAI · gpt-6-sol");
  await act(async () => [...dialog.querySelectorAll("button")].find((button) => button.textContent === "Create")!.click());
  const created = mocks.api.mock.calls.find(([method]) => method === "POST");
  expect(created?.[2]).toEqual({ profile: { kind: "inline", profile: {} } });
});

it("blocks creation without a default but allows a profile's own model", async () => {
  defaults.agentRun = null;
  const dialog = await openCreate();
  const create = () => [...dialog.querySelectorAll("button")].find((button) => button.textContent === "Create")!;
  expect(dialog.textContent).toContain("Universe default: Not selected");
  expect(create().disabled).toBe(true);
  await act(async () => {
    const select = dialog.querySelector("select")!;
    select.value = "custom";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(10); });
  expect(dialog.textContent).toContain("Profile model: Anthropic · profile-model");
  expect(create().disabled).toBe(false);
  await act(async () => create().click());
  expect(mocks.api.mock.calls.find(([method]) => method === "POST")?.[2]).toEqual({ profile: { kind: "named", profileId: "custom" } });
});
