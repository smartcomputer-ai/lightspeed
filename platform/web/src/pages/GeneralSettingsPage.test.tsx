// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Universe } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { GeneralSettingsPage } from "./GeneralSettingsPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

let universe: Universe;
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  universe = {
    id: "platform-id", lightspeedUniverseId: "core-id", name: "Factory", handle: "factory", gatewayUrl: null,
    status: "active", createdAt: "2026-01-01T00:00:00Z", role: "admin", features: { bots: true, channels: true },
  };
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: Record<string, unknown>) => {
    if (method === "GET" && path === "/api/v1/universes") return [universe];
    if (method === "PATCH" && path === "/api/v1/universes/platform-id") return { ...universe, ...body };
    if (method === "PUT" && path === "/api/v1/universes/platform-id/handle") return { ...universe, ...body };
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
  vi.unstubAllGlobals();
});

function Location() {
  return <p data-testid="location">{useLocation().pathname}</p>;
}
async function show(path = "/u/factory/settings/general") {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/u/:handle/settings/general" element={<><GeneralSettingsPage admin={false} /><Location /></>} />
        <Route path="*" element={<Location />} />
      </Routes>
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  await waitForUi(() => expect(container.textContent).not.toContain("Loading"));
}
const location = () => container.querySelector('[data-testid="location"]')?.textContent;
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
const input = (id: string) => container.querySelector<HTMLInputElement>(`#${id}`)!;
async function type(id: string, value: string) {
  const node = input(id);
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const writes = () => mocks.api.mock.calls.filter(([method]) => method !== "GET");

it("hides settings from a member who cannot manage access", async () => {
  universe.role = "operator";
  await show();
  expect(container.querySelector("#universe-name")).toBeNull();
  expect(container.textContent).not.toContain("Danger zone");
});

it("shows the identifiers the universe is reached by", async () => {
  await show();
  const codes = [...container.querySelectorAll("code")].map((node) => node.textContent);
  expect(codes).toEqual(["factory", "core-id"]);
});

it("renames with a trimmed display name once it changes", async () => {
  await show();
  const save = () => input("universe-name").closest("form")!.querySelector<HTMLButtonElement>('button[type="submit"]')!;
  expect(save().disabled).toBe(true);
  await type("universe-name", "   ");
  expect(save().disabled).toBe(true);
  await type("universe-name", " Plant ");
  expect(save().disabled).toBe(false);
  await act(async () => input("universe-name").closest("form")!.requestSubmit());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/platform-id", { name: "Plant" }));
});

it("holds an invalid handle and follows a saved handle to its new URL", async () => {
  await show();
  await type("universe-handle", "-bad handle");
  expect(button("Save handle")!.disabled).toBe(true);
  await type("universe-handle", " plant ");
  expect(button("Save handle")!.disabled).toBe(false);
  await act(async () => input("universe-handle").closest("form")!.requestSubmit());
  await waitForUi(() => expect(location()).toBe("/u/plant/settings/general"));
  expect(mocks.api).toHaveBeenCalledWith("PUT", "/api/v1/universes/platform-id/handle", { handle: "plant" });
});

it("shows a refused handle change and stays put", async () => {
  mocks.api.mockImplementation(async (method: string) => {
    if (method === "PUT") throw new Error("handle already in use");
    return [universe];
  });
  await show();
  await type("universe-handle", "taken");
  await act(async () => input("universe-handle").closest("form")!.requestSubmit());
  await waitForUi(() => expect(container.querySelector('[role="alert"]')?.textContent).toBe("handle already in use"));
  expect(location()).toBe("/u/factory/settings/general");
});

it("archives only after confirmation, then leaves the universe", async () => {
  await show();
  await act(async () => button("Archive universe")!.click());
  await waitForUi(() => expect(document.querySelector('[role="alertdialog"]')?.textContent).toContain("Archive Factory?"));
  expect(writes()).toHaveLength(0);
  await act(async () => button("Archive")!.click());
  await waitForUi(() => expect(location()).toBe("/"));
  expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/platform-id", { status: "archived" });
});

it("restores an archived universe without a confirmation", async () => {
  universe.status = "archived";
  await show();
  expect(button("Archive universe")).toBeUndefined();
  await act(async () => button("Restore universe")!.click());
  await waitForUi(() => expect(mocks.api).toHaveBeenCalledWith("PATCH", "/api/v1/universes/platform-id", { status: "active" }));
  expect(location()).toBe("/u/factory/settings/general");
});


it("shows a Runtime-managed name without offering a conflicting local edit", async () => {
  universe.runtimeConfiguration = { displayName: "Factory" };
  await show();
  expect(container.querySelector<HTMLInputElement>("#universe-name")?.disabled).toBe(true);
  expect(container.textContent).toContain("Managed by the universe configuration");
  expect(mocks.api.mock.calls.some(([method]) => method === "PATCH")).toBe(false);
});
