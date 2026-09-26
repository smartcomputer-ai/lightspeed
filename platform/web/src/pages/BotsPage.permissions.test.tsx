// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { BotsPage } from "./BotsPage";
import { BotSetup } from "@/components/bot/setup";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "contributor" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, slug: "universe", isLoading: false }) }));
vi.mock("@/pages/SessionsPage", () => ({ SessionDetail: () => <div>Readable transcript</div> }));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
vi.mock("@/components/session/session-config-editor", () => ({ SessionConfigEditor: () => <div data-testid="config-editor">Model configuration editor</div> }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
// The ⋯ menu's popup is slow in jsdom; what it is offered is what matters here.
vi.mock("@/components/bot/bot-actions-menu", () => ({
  BotActionsMenu: ({ pause }: { pause?: { enabled: boolean } }) => pause
    ? <button type="button">{pause.enabled ? "Pause bot" : "Resume bot"}</button>
    : null,
}));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const bot = { botId: "bot", displayName: "Test bot", profileId: "shared", revision: 1, eventSeq: 0, selfConfig: true, emit: false, enabled: true, createdAtMs: 0, updatedAtMs: 0, triggerCount: 0, pendingCount: 0, lastEvent: null };
const state = { controller: { mainSessionId: "main", controllerStatus: "idle", setupStatus: "ready", enabled: true, closed: false, sessions: [{ sessionId: "main", label: "main", kind: "main", busy: false, generation: 1 }], activeDeliveries: [] } };
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "contributor";
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method === "POST" && path.endsWith("/messages")) return {};
    if (path.endsWith("/bots")) return { bots: [bot] };
    if (path.endsWith("/bots/bot")) return { bot };
    if (path.endsWith("/state")) return { state };
    if (path.includes("/events?")) return { events: [{ seq: 1, eventId: "event", documentRef: "blob:sha256:0", kind: "test", summary: "Visible history", occurredAtMs: 0, receivedAtMs: 0, outcome: "handled" }] };
    if (path.endsWith("/sessions/main")) return { id: "main", metadata: {} };
    if (path.endsWith("/profiles")) return [{ profileId: "shared", revision: 1 }];
    if (path.endsWith("/profiles/shared")) return { profileId: "shared", revision: 1, config: { features: { environments: { environments: [{ environmentId: "env", default: true }] } } } };
    if (path.endsWith("/triggers")) return { triggers: [] };
    if (path.endsWith("/channel-accounts")) return { accounts: [] };
    if (path.endsWith("/environments")) return [{ environmentId: "env", displayName: "Shared environment", status: "ready", desiredPower: "running", source: { type: "provisioned" }, incarnation: { powerStates: ["running", "paused"] } }];
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
  await act(async () => { await vi.advanceTimersByTimeAsync(10); });
}
async function show(view: "chat" | "activity" = "activity", introduce = false) {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter initialEntries={[{ pathname: "/u/universe/bots/bot", state: { introduce } }]}><Routes><Route path="/u/:slug/bots/:botId" element={<BotsPage admin={true} view={view} />} /></Routes></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
const button = (text: string) => [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.trim() === text);
it("lets a contributor invoke without creating, managing or replaying bots", async () => {
  await show();
  expect(container.querySelector('[aria-label="New bot"]')).toBeNull();
  expect(button("Send a test event")).toBeDefined();
  expect(button("Pause bot")).toBeUndefined();
  await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes("#1"))!.click());
  expect(button("Replay this event")).toBeUndefined();
  expect(container.textContent).toContain("Visible history");
});
it("offers bot creation and management to an operator", async () => {
  mocks.role = "operator";
  await show();
  expect(container.querySelector('[aria-label="New bot"]')).not.toBeNull();
  expect(button("Pause bot")).toBeDefined();
  await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes("#1"))!.click());
  expect(button("Replay this event")).toBeDefined();
});
it("keeps activity readable without creation, invocation or management for viewers", async () => {
  mocks.role = "viewer";
  await show();
  expect(container.querySelector('[aria-label="New bot"]')).toBeNull();
  expect(button("Send a test event")).toBeUndefined();
  expect(button("Pause bot")).toBeUndefined();
  expect(container.textContent).toContain("Visible history");
});
it("sends the introduction from router state only with session control", async () => {
  mocks.role = "viewer";
  await show("chat", true);
  expect(container.textContent).toContain("Readable transcript");
  expect(mocks.api.mock.calls.filter(([method, path]) => method === "POST" && path.endsWith("/messages"))).toHaveLength(0);
  mocks.role = "contributor";
  await show("chat", true);
  expect(mocks.api.mock.calls.filter(([method, path]) => method === "POST" && path.endsWith("/messages"))).toHaveLength(1);
});

it("does not turn bot management into profile or environment administration", async () => {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter><BotSetup universeId="universe" slug="universe" bot={bot} manage /></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
  const profileToggle = [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.startsWith("Session profile"))!;
  await act(async () => profileToggle.click());
  await settle();
  expect(container.querySelector<HTMLButtonElement>("#bot-profile")?.disabled).toBe(false);
  expect(container.querySelector('[data-testid="config-editor"]')).toBeNull();
  expect(container.querySelector('[aria-label="Profile configuration"]')).not.toBeNull();
  expect(container.textContent).toContain("Shared environment");
  expect(button("Pause")).toBeUndefined();
  expect(button("Idle policy…")).toBeUndefined();
  // Configuring the environment is an operator's.
  mocks.role = "operator";
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter><BotSetup universeId="universe" slug="universe" bot={bot} manage /></MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
  expect(button("Pause")).toBeDefined();
  expect(button("Idle policy…")).toBeDefined();
});
