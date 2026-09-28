// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { NewUniverseDialog } from "./universe-switcher";
import { AdminUniversesPage, AdoptDialog } from "@/pages/AdminUniversesPage";
import { SlugCard } from "@/pages/GeneralSettingsPage";
import type { Universe } from "@/api";

const api = vi.hoisted(() => vi.fn());
vi.mock("@/api", async original => ({ ...await original<typeof import("@/api")>(), api }));
vi.mock("@/components/ui/dialog", () => {
  const Box = ({ children }: { children: ReactNode }) => <div>{children}</div>;
  return { Dialog: Box, DialogContent: Box, DialogDescription: Box, DialogFooter: Box, DialogHeader: Box, DialogTitle: Box, DialogTrigger: Box };
});
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  api.mockReset().mockResolvedValue({ slug: "chosen", features: {} });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear(); container.remove(); vi.unstubAllGlobals();
});
async function render(node: ReactNode) {
  await act(async () => root.render(<QueryClientProvider client={client}><MemoryRouter>{node}</MemoryRouter></QueryClientProvider>));
}
async function input(id: string, value: string) {
  const field = container.querySelector<HTMLInputElement>(`#${id}`)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function submit() {
  await act(async () => { container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); });
}
it("submits a requested slug independently from the display name", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await input("new-universe-name", "Display Name");
  expect(container.querySelector<HTMLInputElement>("#new-universe-slug")!.value).toBe("display-name");
  await input("new-universe-slug", "chosen");
  await input("new-universe-name", "Different Name");
  await submit();
  expect(api).toHaveBeenCalledWith("POST", "/api/v1/universes", { name: "Different Name", slug: "chosen" });
});
it.each(["runtime-slug", undefined])("adopts with the runtime slug or an explicit assignment (%s)", async slug => {
  const orphan = { universeId: "id", slug, sessions: 0, workspaces: 0, profiles: 0, blobBytes: 0, createdAtMs: 0 };
  await render(<AdoptDialog orphan={orphan} onOpenChange={() => {}} onAdopted={() => {}} />);
  await input("adopt-name", "My display name");
  const field = container.querySelector<HTMLInputElement>("#adopt-slug")!;
  if (slug) { expect(field.readOnly).toBe(true); expect(field.value).toBe(slug); }
  else {
    await submit(); expect(api).not.toHaveBeenCalled();
    await input("adopt-slug", "chosen");
  }
  await submit();
  expect(api).toHaveBeenCalledWith("POST", "/api/v1/universes/adopt", { lightspeedUniverseId: "id", name: "My display name", ...(!slug ? { slug: "chosen" } : {}) });
});

it("syncs cached slugs only when the administrator requests it", async () => {
  api.mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/api/v1/universes") return [];
    if (method === "GET" && path === "/api/v1/universes/reconcile") return { platform: [], orphans: [] };
    if (method === "POST" && path === "/api/v1/universes/sync-slugs") return { updated: 2, skipped: 1 };
    throw new Error(`Unexpected request: ${method} ${path}`);
  });
  await render(<AdminUniversesPage />);
  expect(api.mock.calls.every(([method]) => method === "GET")).toBe(true);
  const button = Array.from(container.querySelectorAll("button")).find(button => button.textContent === "Sync from runtime")!;
  await act(async () => button.click());
  await vi.waitFor(() => expect(container.querySelector('[role="status"]')?.textContent).toContain("2 updated"));
  expect(api.mock.calls.filter(([method]) => method === "POST")).toEqual([["POST", "/api/v1/universes/sync-slugs", {}]]);
  expect(container.querySelector('[role="status"]')?.textContent).toContain("1 missing or unnamed runtime universes skipped");
});

it("uses the returned slug for the cached universe and navigates to its new URL", async () => {
  const universe: Universe = { id: "platform-id", lightspeedUniverseId: "runtime-id", name: "Name", slug: "old-slug", gatewayUrl: null, status: "active", createdAt: "2026-01-01", features: { bots: true, channels: true } };
  client.setQueryData(["universes"], [universe]);
  api.mockResolvedValue({ ...universe, slug: "new-slug" });
  function Location() { return <output data-location>{useLocation().pathname}</output>; }
  await render(<><SlugCard universe={universe} /><Location /></>);
  await input("universe-slug", "new-slug");
  await submit();
  await vi.waitFor(() => expect(container.querySelector("[data-location]")?.textContent).toBe("/u/new-slug/settings/general"));
  expect(api).toHaveBeenCalledWith("PUT", "/api/v1/universes/platform-id/slug", { slug: "new-slug" });
  expect(client.getQueryData<Universe[]>(["universes"])?.[0]).toMatchObject({ id: "platform-id", lightspeedUniverseId: "runtime-id", slug: "new-slug" });
});
