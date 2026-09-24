// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ResourceRef } from "@lightspeed-ai/agent-client";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { ProfilesPage } from "./ProfilesPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "contributor" }, slug: "universe", isLoading: false }) }));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
vi.mock("@/components/session/session-config-editor", () => ({ SessionConfigEditor: () => null }));
// Every select becomes a native control named by its trigger; jsdom cannot lay out the popup.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  function SelectTrigger(_props: { id?: string; children?: ReactNode }) {
    return null;
  }
  function Select({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) {
    const options: { value: string; label: ReactNode }[] = [];
    let id: string | undefined;
    const visit = (nodes: ReactNode) =>
      React.Children.forEach(nodes, (child) => {
        if (!React.isValidElement(child)) return;
        const props = child.props as { value?: string; children?: ReactNode; id?: string };
        if (child.type === SelectTrigger) id = props.id;
        else if (props.value !== undefined) options.push({ value: props.value, label: props.children });
        else if (props.children) visit(props.children);
      });
    visit(children);
    return (
      <select id={id} value={value} onChange={(event) => onValueChange(event.target.value)}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>
    );
  }
  const Part = () => null;
  return { Select, SelectTrigger, SelectContent: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectItem: Part, SelectValue: Part };
});

const profiles = [
  { profileId: "own", displayName: "Own", revision: 1 },
  { profileId: "support", displayName: "Support desk", revision: 3, config: { model: { model: "internal-model" } } },
];
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: { resources?: ResourceRef[] }) => {
    if (path.endsWith("/access")) return {
      actions: ["read", "create_profile"],
      resources: (body?.resources ?? []).map((resource) => ({ resource, actions: ["read"] })),
    };
    if (method === "GET" && path.endsWith("/profiles")) return profiles;
    if (method === "GET" && path.includes("/profiles/"))
      return profiles.find((profile) => path.endsWith(`/${profile.profileId}`)) ?? { profileId: path.split("/").pop(), revision: 1 };
    if (method === "PUT" && path.includes("/profiles/")) return {};
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
const dialog = () => document.body.querySelector<HTMLElement>('[role="dialog"]')!;
const lines = () => [...dialog().querySelectorAll('[data-slot="settings-disclosure"] > p')].map((line) => line.textContent);
async function click(target: Element | null | undefined) {
  expect(target).toBeTruthy();
  await act(async () => (target as HTMLElement).click());
  await settle();
}
async function type(selector: string, value: string) {
  const input = dialog().querySelector<HTMLInputElement>(selector)!;
  expect(input).not.toBeNull();
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it("keeps the display name open and summarizes the id and starting point", async () => {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter initialEntries={["/u/universe/profiles/own"]}>
      <Routes><Route path="/u/:slug/profiles/:profileId" element={<ProfilesPage admin />} /></Routes>
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
  await click(container.querySelector('[aria-label="New profile"]'));
  expect(dialog().querySelector("#new-profile-name")).not.toBeNull();
  expect(dialog().querySelector("#new-profile-id")).toBeNull();
  expect(dialog().querySelector("#new-profile-source")).toBeNull();
  expect(lines()).toEqual(["Id derived from the display name·Change", "Starts empty·Change"]);

  await type("#new-profile-name", "Support owner");
  expect(lines()[0]).toBe("Id: support-owner·Change");
  await click(dialog().querySelector('[aria-label="Change profile id"]'));
  expect(dialog().querySelector<HTMLInputElement>("#new-profile-id")!.value).toBe("support-owner");
  expect(dialog().textContent).toContain("cannot be changed later");
  await type("#new-profile-id", "tier-two");

  await click(dialog().querySelector('[aria-label="Change starting point"]'));
  const source = dialog().querySelector<HTMLSelectElement>("#new-profile-source")!;
  await act(async () => {
    source.value = "support";
    source.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(lines()[1]).toBe("Copies Support desk·Hide");

  await click(dialog().querySelector('button[type="submit"]'));
  const put = mocks.api.mock.calls.find(([method]) => method === "PUT");
  expect(put?.[1]).toBe("/api/v1/universes/universe/profiles/tier-two");
  expect(put?.[2]).toEqual({
    profileId: "tier-two",
    displayName: "Support owner",
    revision: 0,
    config: { model: { model: "internal-model" } },
  });
});

it("opens the id on its own when neither a name nor an id is given", async () => {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><MemoryRouter initialEntries={["/u/universe/profiles/own"]}>
      <Routes><Route path="/u/:slug/profiles/:profileId" element={<ProfilesPage admin />} /></Routes>
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await settle();
  await click(container.querySelector('[aria-label="New profile"]'));
  await click(dialog().querySelector('button[type="submit"]'));
  expect(dialog().textContent).toContain("a name or id is required");
  expect(dialog().querySelector("#new-profile-id")).not.toBeNull();
  expect(mocks.api.mock.calls.some(([method]) => method === "PUT")).toBe(false);
});
