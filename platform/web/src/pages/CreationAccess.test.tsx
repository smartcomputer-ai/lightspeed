// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ResourceRef } from "@lightspeed-ai/agent-client";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { SessionsPage } from "./SessionsPage";
import { WorkspacesPage } from "./WorkspacesPage";
import { BotCreateDialog } from "./BotCreatePage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "contributor", slug: "test", name: "Test" }, slug: "test", isLoading: false }) }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
// Choose access with a native control; jsdom cannot lay out the floating popup.
vi.mock("@/components/access/shared", async (original) => {
  const actual = await original<typeof import("@/components/access/shared")>();
  return {
    ...actual,
    AccessSelect: ({ label, value, options, onChange }: Parameters<typeof actual.AccessSelect>[0]) => (
      <select aria-label={label} value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>
    ),
  };
});

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let personalExecutionEnabled: boolean;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  window.localStorage.clear();
  personalExecutionEnabled = false;
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: { resources?: ResourceRef[] }) => {
    if (path.endsWith("/access")) return {
      actions: ["read", "create_session", "create_bot", "create_profile", "create_workspace"],
      resources: (body?.resources ?? []).map((resource) => ({ resource, actions: ["read"] })),
    };
    if (path.endsWith("/access/execution")) return { policy: { executionPrincipalId: "agent", personalExecutionEnabled } };
    if (path.includes("/sessions?")) return { sessions: [] };
    if (method === "POST" && path.endsWith("/sessions")) return { id: "created" };
    if (path.endsWith("/profiles")) return [];
    if (method === "PUT" && path.includes("/profiles/")) return {};
    if (method === "GET" && path.endsWith("/bots")) return { bots: [] };
    if (method === "POST" && path.endsWith("/bots")) return { bot: { botId: "triage" } };
    if (method === "GET" && path.endsWith("/workspaces")) return [];
    if (method === "POST" && path.endsWith("/workspaces")) return { workspaceId: "notes" };
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
  for (let step = 0; step < 5; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function show(page: ReactNode) {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter>
      <Routes><Route path="*" element={page} /></Routes>
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
}
const dialog = () => document.body.querySelector<HTMLElement>('[role="dialog"]')!;
const button = (text: string) =>
  [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.trim() === text);
async function click(target: HTMLElement | null | undefined) {
  expect(target).toBeTruthy();
  await act(async () => target!.click());
  await settle();
}
async function type(selector: string, value: string) {
  const input = document.body.querySelector<HTMLInputElement>(selector)!;
  expect(input).not.toBeNull();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function choose(label: string, value: string) {
  const select = document.body.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)!;
  expect(select).not.toBeNull();
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
function request(method: string, suffix: string) {
  return mocks.api.mock.calls.find(([verb, path]) => verb === method && (path as string).endsWith(suffix))?.[2];
}

it("creates a session with the audience chosen on its closed access line", async () => {
  await show(<SessionsPage admin={false} />);
  await click(container.querySelector<HTMLButtonElement>('[aria-label="New session"]'));
  const line = dialog().querySelector('[data-slot="settings-disclosure"]')!;
  expect(line.textContent).toBe("Universe members can read it · Runs as Default agent identity·Change");
  // The access line sits at the bottom of the form, right above the submit buttons.
  expect(line.nextElementSibling?.getAttribute("data-slot")).toBe("dialog-footer");
  expect(dialog().querySelector('[aria-label="Who can read"]')).toBeNull();

  await click(dialog().querySelector<HTMLButtonElement>('[aria-label="Change access and identity"]'));
  // Personal execution is off for this universe: the identity is plain text.
  expect(dialog().querySelector('[aria-label="Running as"]')).toBeNull();
  await choose("Who can read", "restricted");
  expect(line.textContent).toContain("Only you and people you share with can read it · Runs as Default agent identity");
  await click(button("Create"));
  expect(request("POST", "/sessions")).toMatchObject({
    access: { visibility: "restricted" },
    execution: { kind: "service" },
  });
});

it("creates a workspace with universe use by default and restricted use when changed", async () => {
  await show(<WorkspacesPage admin />);
  await click(container.querySelector<HTMLButtonElement>('[aria-label="New workspace"]'));
  const line = dialog()
    .querySelector('[aria-label="Change who can use it"]')!
    .closest('[data-slot="settings-disclosure"]')!;
  expect(line.textContent).toBe("Universe members can use it·Change");
  await type("#new-workspace-name", "Notes");
  await click(dialog().querySelector<HTMLButtonElement>('[aria-label="Change who can use it"]'));
  await choose("Who can use", "restricted");
  expect(line.textContent).toContain("Only you and people you share with can use it");
  await click(button("Create"));
  expect(request("POST", "/workspaces")).toEqual({
    workspaceId: "notes",
    displayName: "Notes",
    access: { visibility: "restricted" },
  });
});

it("keeps the workspace id behind Change, derived from the name until edited", async () => {
  await show(<WorkspacesPage admin />);
  await click(container.querySelector<HTMLButtonElement>('[aria-label="New workspace"]'));
  expect(dialog().querySelector("#new-workspace-id")).toBeNull();
  await type("#new-workspace-name", "Team Notes");
  const idLine = dialog()
    .querySelector('[aria-label="Change workspace id"]')!
    .closest('[data-slot="settings-disclosure"]')!;
  expect(idLine.textContent).toBe("Id: team-notes·Change");
  await click(dialog().querySelector<HTMLButtonElement>('[aria-label="Change workspace id"]'));
  await type("#new-workspace-id", "shared-notes");
  await click(button("Create"));
  expect(request("POST", "/workspaces")).toMatchObject({
    workspaceId: "shared-notes",
    displayName: "Team Notes",
  });
});

it("asks for bot access only on the wizard's final step, next to Create", async () => {
  personalExecutionEnabled = true;
  await show(<BotCreateDialog universeId="universe" slug="test" open onOpenChange={() => {}} />);
  expect(dialog().textContent).toContain("What is this bot's job?");
  expect(dialog().querySelector('[aria-label="Change access and identity"]')).toBeNull();
  await type("#new-bot-name", "Triage");

  const steps = [...dialog().querySelectorAll<HTMLButtonElement>('[aria-label="Bot creation progress"] button')];
  for (const step of steps.slice(1, -1)) {
    await click(step);
    expect(dialog().querySelector('[aria-label="Change access and identity"]')).toBeNull();
  }
  await click(steps.at(-1));
  const line = dialog().querySelector('[data-slot="settings-disclosure"]')!;
  expect(line.textContent).toBe("Universe members can read it · Runs as Default agent identity·Change");
  await click(dialog().querySelector<HTMLButtonElement>('[aria-label="Change access and identity"]'));
  await choose("Running as", "personal");
  expect(line.textContent).toContain("Only you and people you share with can read it · Runs as you");
  await click(button("Create bot"));
  expect(request("POST", "/bots")).toMatchObject({
    bot: { botId: "triage" },
    access: { visibility: "restricted" },
    execution: { kind: "personal" },
  });
});
