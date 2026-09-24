// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { AccessReadResponse, ResourceRef } from "@lightspeed-ai/agent-client";
import { allowsAction, PermissionIdentityProvider, useActionPermissions } from "./permissions";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
const own = { kind: "session", id: "own" } as const;
const other = { kind: "session", id: "other" } as const;
const preview: AccessReadResponse = {
  actions: ["read", "create_session"],
  resources: [
    { resource: own, actions: ["read", "control_session", "stop_session", "delete_session"] },
    { resource: other, actions: ["read", "stop_session"] },
  ],
};
let container: HTMLDivElement;
let root: Root;
let client: QueryClient;
let current: ReturnType<typeof useActionPermissions>;

function Probe({ resources, cascade }: { resources: ResourceRef[]; cascade: boolean }) {
  current = useActionPermissions("universe", resources, { sessionDeleteCascade: cascade });
  return <span>{String(current.can("control_session", own))}</span>;
}
async function show(user = "user", resources: ResourceRef[] = [own, other], cascade = false) {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId={user}><Probe resources={resources} cascade={cascade} /></PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockResolvedValue(preview);
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
it("uses only target-specific core decisions, without granting control from stop or creation", () => {
  expect(allowsAction(preview, "create_session")).toBe(true);
  expect(allowsAction(preview, "control_session", own)).toBe(true);
  expect(allowsAction(preview, "control_session", other)).toBe(false);
  expect(allowsAction(preview, "stop_session", other)).toBe(true);
  expect(allowsAction(preview, "control_session", { kind: "bot", id: "own" })).toBe(false);
  expect(allowsAction(preview, "delete_session", { kind: "session", id: "missing" })).toBe(false);
  expect(allowsAction(undefined, "create_session")).toBe(false);
});
it("fails closed before load and after an unsuccessful refresh", async () => {
  let resolve!: (value: AccessReadResponse) => void;
  mocks.api.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  await show();
  expect(current.can("control_session", own)).toBe(false);
  await act(async () => resolve(preview));
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
  expect(current.can("control_session", own)).toBe(true);
  mocks.api.mockRejectedValueOnce(new Error("Permission lookup failed"));
  await act(async () => { await current.refetch(); });
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
  expect(current.can("control_session", own)).toBe(false);
});
it("does not reuse another signed-in user's cached allowances", async () => {
  await show();
  expect(current.can("control_session", own)).toBe(true);
  mocks.api.mockImplementationOnce(() => new Promise(() => {}));
  await show("another-user");
  expect(current.can("control_session", own)).toBe(false);
});
it("deduplicates targets, separates cascade queries and batches long lists", async () => {
  await show("user", [own, own, other]);
  expect(mocks.api).toHaveBeenLastCalledWith("POST", "/api/v1/universes/universe/access", { resources: [other, own], sessionDeleteCascade: false });
  await show("user", [own], true);
  expect(mocks.api).toHaveBeenLastCalledWith("POST", "/api/v1/universes/universe/access", { resources: [own], sessionDeleteCascade: true });
  mocks.api.mockClear();
  await show("user", Array.from({ length: 201 }, (_, index) => ({ kind: "session", id: String(index) })));
  expect(mocks.api.mock.calls.map((call) => call[2].resources.length)).toEqual([100, 100, 1]);
});
it("asks for the default agent identity's resource decisions in a separate query", async () => {
  function ServiceProbe() {
    current = useActionPermissions("universe", [own], { as: "execution_service" });
    return null;
  }
  await show("user", [own]);
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId="user"><ServiceProbe /></PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
  expect(mocks.api).toHaveBeenLastCalledWith("POST", "/api/v1/universes/universe/access", {
    resources: [own], sessionDeleteCascade: false, as: "execution_service",
  });
  expect(mocks.api).toHaveBeenCalledTimes(2);
});
