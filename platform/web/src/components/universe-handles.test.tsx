// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { NewUniverseDialog } from "./universe-switcher";
import { AdminUniversesPage, AdoptDialog } from "@/pages/AdminUniversesPage";
import { HandleCard } from "@/pages/AdminUniverseSettingsPage";
import type { Universe } from "@/api";
import { universeHandleFromName, universeHandleSchema } from "@lightspeed-ai/platform-shared";

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
  api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (path === "/api/v1/platform/accounts") return { accounts: [{ id: "account", name: "Personal", enabled: true, role: "creator" }] };
    if (path === "/api/ui/me") return { user: { id: "user" } };
    if (path === "/api/v1/platform/accounts/account/members") return { members: [{ userId: "user", name: "User" }] };
    if (method === "POST" && path === "/api/v1/platform/accounts/account/universes") return { universe: { handle: "chosen", features: {} } };
    if (method === "POST" && path === "/api/ui/universes/adopt") return { handle: "chosen", features: {} };
    throw new Error(`Unexpected request: ${method} ${path}`);
  });
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
it("accepts the handles the runtime accepts and suggests only those", () => {
  for (const handle of ["research", "a", "0", "team-2", "2026-research", "a".repeat(63)]) {
    expect(universeHandleSchema.safeParse(handle).success, handle).toBe(true);
  }
  for (const handle of [
    "", "Research", "my.universe", "my_universe", "my:universe", "-edge", "edge-", "with space", "a".repeat(64),
    "0190f3e8a4c7b219f2c41d07ab355e1a", "0190f3e8-a4c7-7b21-9f2c-41d07ab355e1",
  ]) {
    expect(universeHandleSchema.safeParse(handle).success, handle).toBe(false);
  }
  for (const name of ["Research Team", "  Ünïcode & Co.  ", "!!!", "0190f3e8a4c7b219f2c41d07ab355e1a", `${"a".repeat(59)} b`]) {
    expect(universeHandleSchema.safeParse(universeHandleFromName(name)).success, name).toBe(true);
  }
});
it("submits a requested handle independently from the display name", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.textContent).toContain("Personal"));
  await input("new-universe-name", "Display Name");
  expect(container.querySelector<HTMLInputElement>("#new-universe-handle")!.value).toBe("display-name");
  await input("new-universe-handle", "chosen");
  await input("new-universe-name", "Different Name");
  await submit();
  expect(api).toHaveBeenCalledWith("POST", "/api/v1/platform/accounts/account/universes", { name: "Different Name", handle: "chosen", universeId: expect.any(String), initialOwner: { kind: "user", id: "user" } });
  expect(api.mock.calls.some(([, path]) => path.endsWith("/members"))).toBe(false);
});
it("adopts under the runtime handle, which the dialog shows but cannot change", async () => {
  const orphan = { universeId: "id", handle: "runtime-handle", sessions: 0, workspaces: 0, profiles: 0, blobBytes: 0, createdAtMs: 0 };
  await render(<AdoptDialog orphan={orphan} onOpenChange={() => {}} onAdopted={() => {}} />);
  await vi.waitFor(() => expect(container.querySelector<HTMLSelectElement>("#adopt-owner")?.value).toBe("user"));
  await input("adopt-name", "My display name");
  const field = container.querySelector<HTMLInputElement>("#adopt-handle")!;
  expect(field.readOnly).toBe(true);
  expect(field.value).toBe("runtime-handle");
  await submit();
  expect(api).toHaveBeenCalledWith("POST", "/api/ui/universes/adopt", { lightspeedUniverseId: "id", name: "My display name", accountId: "account", initialOwner: { kind: "user", id: "user" } });
});

it("suggests a valid handle for a name that ends on a separator at the length limit", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await input("new-universe-name", `${"a".repeat(59)} b`);
  expect(container.querySelector<HTMLInputElement>("#new-universe-handle")!.value).toBe("a".repeat(59));
});

it("syncs cached handles only when the administrator requests it", async () => {
  api.mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/api/ui/universes") return [];
    if (method === "GET" && path === "/api/ui/universes/reconcile") return { platform: [], orphans: [] };
    if (method === "POST" && path === "/api/ui/universes/sync-handles") return { updated: 2, skipped: 1 };
    throw new Error(`Unexpected request: ${method} ${path}`);
  });
  await render(<PermissionIdentityProvider userId="user" platformAdmin><AdminUniversesPage /></PermissionIdentityProvider>);
  expect(api.mock.calls.every(([method]) => method === "GET")).toBe(true);
  const button = Array.from(container.querySelectorAll("button")).find(button => button.textContent === "Sync from runtime")!;
  await act(async () => button.click());
  await vi.waitFor(() => expect(container.querySelector('[role="status"]')?.textContent).toContain("2 updated"));
  expect(api.mock.calls.filter(([method]) => method === "POST")).toEqual([["POST", "/api/ui/universes/sync-handles", {}]]);
  expect(container.querySelector('[role="status"]')?.textContent).toContain("1 missing from the runtime and skipped");
});

it("uses the returned handle for the cached universe and navigates to its new URL", async () => {
  const universe: Universe = { id: "platform-id", lightspeedUniverseId: "runtime-id", name: "Name", handle: "old-handle", status: "active", createdAt: "2026-01-01", features: { bots: true, channels: true } };
  client.setQueryData(["universes"], [universe]);
  api.mockResolvedValue({ ...universe, handle: "new-handle" });
  function Location() { return <output data-location>{useLocation().pathname}</output>; }
  await render(<><HandleCard universe={universe} /><Location /></>);
  await input("universe-handle", "new-handle");
  await submit();
  await vi.waitFor(() => expect(container.querySelector("[data-location]")?.textContent).toBe("/admin/universes/new-handle/settings"));
  expect(api).toHaveBeenCalledWith("PUT", "/api/ui/universes/platform-id/handle", { handle: "new-handle" });
  expect(client.getQueryData<Universe[]>(["universes"])?.[0]).toMatchObject({ id: "platform-id", lightspeedUniverseId: "runtime-id", handle: "new-handle" });
});
