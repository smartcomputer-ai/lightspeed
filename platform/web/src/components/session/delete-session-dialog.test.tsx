// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { DeleteSessionDialog } from "./delete-session-dialog";

const mocks = vi.hoisted(() => ({ api: vi.fn(), deleted: vi.fn(), cancel: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.api.mockReset().mockResolvedValue({});
  mocks.deleted.mockReset(); mocks.cancel.mockReset();
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear(); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals();
});
async function settle() {
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
}
async function show(platformAdmin: boolean) {
  await act(async () => root.render(<QueryClientProvider client={client}>
    <PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}>
      <DeleteSessionDialog universeId="u" sessionId="s" onCancel={mocks.cancel} onDeleted={mocks.deleted} />
    </PermissionIdentityProvider>
  </QueryClientProvider>));
  await settle();
}
const button = (label: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent === label)!;
async function check(label: string) {
  const field = [...document.body.querySelectorAll("label")].find((node) => node.textContent?.includes(label))!;
  await act(async () => field.querySelector<HTMLButtonElement>('[role="checkbox"]')!.click());
}
async function submit(label: string) {
  await act(async () => button(label).click());
  await settle();
}
it("keeps permanent deletion unavailable without platform-admin identity", async () => {
  await show(false);
  expect(document.body.textContent).not.toContain("Also permanently delete retained history");
  await submit("Delete session");
  expect(mocks.api.mock.calls).toEqual([["DELETE", "/api/v1/universes/u/sessions/s"]]);
  expect(mocks.deleted).toHaveBeenCalledOnce();
});
it("defaults to soft deletion even for platform admins", async () => {
  await show(true);
  expect(document.body.textContent).toContain("Also permanently delete retained history");
  await submit("Delete session");
  expect(mocks.api.mock.calls).toEqual([["DELETE", "/api/v1/universes/u/sessions/s"]]);
});
it("soft-deletes first and permanently deletes through the audited admin endpoint", async () => {
  await show(true);
  await check("Also delete forks");
  await check("Also permanently delete");
  expect(document.body.textContent).toContain("This cannot be undone");
  expect(mocks.api).not.toHaveBeenCalled();
  await submit("Permanently delete");
  expect(mocks.api.mock.calls).toEqual([
    ["DELETE", "/api/v1/universes/u/sessions/s?cascade=true"],
    ["POST", "/api/v1/admin/universes/u/sessions/s/purge", {}],
  ]);
  expect(mocks.deleted).toHaveBeenCalledOnce();
});
it("does not purge when soft deletion fails", async () => {
  mocks.api.mockRejectedValue(new Error("Session is still open"));
  await show(true);
  await check("Also permanently delete");
  await submit("Permanently delete");
  expect(mocks.api).toHaveBeenCalledTimes(1);
  expect(document.querySelector('[role="alert"]')?.textContent).toContain("Session is still open");
  expect(mocks.deleted).not.toHaveBeenCalled();
});
it("retries failed permanent deletion without repeating soft deletion", async () => {
  mocks.api.mockResolvedValueOnce({}).mockRejectedValueOnce(new Error("Purge unavailable")).mockResolvedValue({});
  await show(true);
  await check("Also permanently delete");
  await submit("Permanently delete");
  expect(document.querySelector('[role="alert"]')?.textContent).toContain("already soft-deleted");
  expect(mocks.deleted).not.toHaveBeenCalled();
  await submit("Retry permanent deletion");
  expect(mocks.api.mock.calls.map(([method]) => method)).toEqual(["DELETE", "POST", "POST"]);
  expect(mocks.deleted).toHaveBeenCalledOnce();
});
