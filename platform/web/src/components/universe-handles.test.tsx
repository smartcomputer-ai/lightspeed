// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { organizationActions, universeActions } from "@lightspeed-ai/platform-shared";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { NewUniverseDialog } from "./new-universe-dialog";
import { AdoptDialog } from "@/pages/UnclaimedUniversesPage";
import { HandleCard } from "@/pages/UniverseGeneralSettingsPage";
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
    if (path === "/api/v1/platform/organizations") return { organizations: [{ id: "account", name: "Personal", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: true }] };
    if (path === "/api/ui/me") return { user: { id: "user" } };
    if (path === "/api/v1/platform/organizations/account/members") return { members: [{ userId: "user", name: "User" }] };
    if (method === "POST" && path === "/api/v1/platform/organizations/account/universes") return { universe: { handle: "chosen", features: {} } };
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
const customize = () => act(async () => container.querySelector<HTMLButtonElement>('button[aria-label="Change the address"]')!.click());

it("asks only for a name, naming the one organization and the address the name gives", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.textContent).toContain("In Personal."));
  expect(container.querySelector("#universe-organization")).toBeNull();
  expect(container.querySelector("#new-universe-handle")).toBeNull();
  expect(container.querySelector("#universe-owner")).toBeNull();
  await input("new-universe-name", "Display Name");
  expect(container.textContent).toContain("/u/display-name");
  await submit();
  // The caller owns what it creates, so no owner is sent and no members are read.
  expect(api).toHaveBeenCalledWith("POST", "/api/v1/platform/organizations/account/universes", { name: "Display Name", handle: "display-name", universeId: expect.any(String) });
  expect(api.mock.calls.some(([, path]) => path.endsWith("/members"))).toBe(false);
});
it("submits a customized handle independently from the display name", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.textContent).toContain("In Personal."));
  await input("new-universe-name", "Display Name");
  await customize();
  expect(container.querySelector<HTMLInputElement>("#new-universe-handle")!.value).toBe("display-name");
  await input("new-universe-handle", "chosen");
  await input("new-universe-name", "Different Name");
  expect(container.textContent).toContain("/u/chosen");
  await submit();
  expect(api).toHaveBeenCalledWith("POST", "/api/v1/platform/organizations/account/universes", { name: "Different Name", handle: "chosen", universeId: expect.any(String) });
});
it("keeps an invalid handle in view and refuses to create with it", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.textContent).toContain("In Personal."));
  await input("new-universe-name", "Research");
  await customize();
  await input("new-universe-handle", "Not Valid");
  expect(container.querySelector<HTMLButtonElement>('button[aria-label="Change the address"]')!.disabled).toBe(true);
  expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true);
});
it("asks a Platform Admin outside the organization to choose its owner", async () => {
  api.mockImplementation(async (method: string, path: string) => {
    if (path === "/api/v1/platform/organizations") return { organizations: [{ id: "account", name: "Team", role: null, actions: [...organizationActions(["admin"])], canCreateUniverse: true }] };
    if (path === "/api/v1/platform/organizations/account/members") return { members: [{ userId: "dana", name: "Dana" }] };
    if (method === "POST") return { universe: { handle: "research", features: {} } };
    throw new Error(`Unexpected request: ${method} ${path}`);
  });
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.querySelector("#universe-owner")).not.toBeNull());
  expect(container.textContent).toContain("You are not a member of Team, so a member must own it.");
  await input("new-universe-name", "Research");
  expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true);
});
it("adopts under the runtime handle, which the dialog shows but cannot change", async () => {
  const orphan = { universeId: "id", handle: "runtime-handle", sessions: 0, workspaces: 0, profiles: 0, blobBytes: 0, createdAtMs: 0 };
  await render(<AdoptDialog orphan={orphan} onOpenChange={() => {}} onAdopted={() => {}} />);
  await vi.waitFor(() => expect(container.querySelector<HTMLButtonElement>("#adopt-owner")?.querySelector('[data-slot="select-value"]')?.textContent).toBe("User"));
  await input("adopt-name", "My display name");
  const field = container.querySelector<HTMLInputElement>("#adopt-handle")!;
  expect(field.readOnly).toBe(true);
  expect(field.value).toBe("runtime-handle");
  await submit();
  expect(api).toHaveBeenCalledWith("POST", "/api/ui/universes/adopt", { lightspeedUniverseId: "id", name: "My display name", organizationId: "account", initialOwner: { kind: "user", id: "user" } });
});

it("suggests a valid handle for a name that ends on a separator at the length limit", async () => {
  await render(<NewUniverseDialog open onOpenChange={() => {}} />);
  await input("new-universe-name", `${"a".repeat(59)} b`);
  expect(container.textContent).toContain(`/u/${"a".repeat(59)}`);
  expect(container.textContent).not.toContain(`/u/${"a".repeat(59)}-`);
});


it("uses the returned handle for the cached universe and navigates to its new URL", async () => {
  const universe: Universe = { id: "platform-id", organizationId: "team", lightspeedUniverseId: "runtime-id", name: "Name", handle: "old-handle", status: "active", createdAt: "2026-01-01", features: { bots: true } };
  client.setQueryData(["universes"], [universe]);
  api.mockResolvedValue({ ...universe, handle: "new-handle" });
  function Location() { return <output data-location>{useLocation().pathname}</output>; }
  await render(<><HandleCard universe={universe} /><Location /></>);
  await input("universe-handle", "new-handle");
  await submit();
  await vi.waitFor(() => expect(container.querySelector("[data-location]")?.textContent).toBe("/u/new-handle/settings/general"));
  expect(api).toHaveBeenCalledWith("PUT", "/api/ui/universes/platform-id/handle", { handle: "new-handle" });
  expect(client.getQueryData<Universe[]>(["universes"])?.[0]).toMatchObject({ id: "platform-id", lightspeedUniverseId: "runtime-id", handle: "new-handle" });
});

it("fixes the creation destination in organization administration", async () => {
  const original = api.getMockImplementation()!;
  api.mockImplementation(async (...args) => args[1] === "/api/v1/platform/organizations" ? { organizations: [
    { id: "first", name: "First", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: true },
    { id: "destination", name: "Destination", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: true },
  ] } : original(...args));
  await render(<NewUniverseDialog organizationId="destination" open onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.textContent).toContain("In Destination."));
  expect(container.querySelector("#universe-organization")).toBeNull();
});

it("refreshes the current working organization's creation default on each opening", async () => {
  const original = api.getMockImplementation()!;
  api.mockImplementation(async (...args) => args[1] === "/api/v1/platform/organizations" ? { organizations: [
    { id: "first", name: "First", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: true },
    { id: "second", name: "Second", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: true },
  ] } : original(...args));
  await render(<NewUniverseDialog open defaultOrganizationId="first" onOpenChange={() => {}} />);
  const destination = () => container.querySelector<HTMLButtonElement>("#universe-organization")!;
  await vi.waitFor(() => expect(destination().querySelector('[data-slot="select-value"]')?.textContent).toBe("First"));
  await render(<NewUniverseDialog open={false} defaultOrganizationId="second" onOpenChange={() => {}} />);
  await render(<NewUniverseDialog open defaultOrganizationId="second" onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(destination().querySelector('[data-slot="select-value"]')?.textContent).toBe("Second"));
});

it("offers no organization that has reached its universe limit", async () => {
  const original = api.getMockImplementation()!;
  api.mockImplementation(async (...args) => args[1] === "/api/v1/platform/organizations" ? { organizations: [
    { id: "full", name: "Full", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: false },
    { id: "open", name: "Open", role: "creator", actions: [...organizationActions(["creator"])], canCreateUniverse: true },
  ] } : original(...args));
  await render(<NewUniverseDialog open defaultOrganizationId="full" onOpenChange={() => {}} />);
  await vi.waitFor(() => expect(container.textContent).toContain("In Open."));
});

