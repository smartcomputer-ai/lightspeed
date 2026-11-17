// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UNIVERSE_ROLES, universeActions, universeRoleAllows, type UniverseAction, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { PermissionIdentityProvider, useActionPermissions } from "./permissions";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
let container: HTMLDivElement;
let root: Root;
let client: QueryClient;
let current: ReturnType<typeof useActionPermissions>;

function Probe({ universeId }: { universeId: string }) {
  current = useActionPermissions(universeId);
  return null;
}
async function show(platformAdmin = false, universeId = "universe") {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId="user" platformAdmin={platformAdmin}>
        <MemoryRouter initialEntries={["/u/test/sessions"]}><Probe universeId={universeId} /></MemoryRouter>
      </PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
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

/// The server sends the caller's highest role and the actions it holds.
const universes = (role: UniverseRole | null, actions: readonly UniverseAction[] = role ? [...universeActions([role])] : []) =>
  mocks.api.mockReset().mockResolvedValue([{ id: "universe", handle: "test", role, actions }]);

it("gives each role what the one below has, plus its own", () => {
  const offered = (action: UniverseAction) => UNIVERSE_ROLES.filter((role) => universeRoleAllows(role, action));
  expect(offered("read")).toEqual(["viewer", "contributor", "operator", "owner"]);
  expect(offered("controlSession")).toEqual(["contributor", "operator", "owner"]);
  expect(offered("configureSession")).toEqual(["operator", "owner"]);
  expect(offered("deleteSession")).toEqual(["operator", "owner"]);
  expect(offered("setSessionRetention")).toEqual(["owner"]);
  expect(offered("closeSession")).toEqual(["contributor", "operator", "owner"]);
  expect(offered("shareSession")).toEqual(["contributor", "operator", "owner"]);
  expect(offered("configureResource")).toEqual(["operator", "owner"]);
  expect(offered("createWorkspace")).toEqual(["operator", "owner"]);
  expect(offered("useResource")).toEqual(["contributor", "operator", "owner"]);
  expect(offered("createProfile")).toEqual(["operator", "owner"]);
  expect(offered("administer")).toEqual(["owner"]);
  expect(offered("accessPrivateSessions")).toEqual(["owner"]);
  expect(offered("closeSharedSessions")).toEqual(["operator", "owner"]);
  expect(universeRoleAllows(null, "read")).toBe(false);
});

it("reads the member's role in the universe on screen", async () => {
  universes("contributor");
  await show();
  expect(current.role).toBe("contributor");
  expect(current.can("controlSession")).toBe(true);
  expect(current.can("configureResource")).toBe(false);
});

it("decides on the actions the server sent, not on the role name", async () => {
  // A Viewer granted one more action holds exactly that union.
  universes("viewer", [...universeActions(["viewer"]), "invokeBot"]);
  await show();
  expect(current.role).toBe("viewer");
  expect(current.can("invokeBot")).toBe(true);
  expect(current.can("createSession")).toBe(false);
});

it("offers nothing for another universe or before the universe loads", async () => {
  mocks.api.mockReset().mockReturnValue(new Promise(() => {}));
  await show();
  expect(current.isLoading).toBe(true);
  expect(current.can("read")).toBe(false);
  universes("owner");
  client.clear();
  await show(false, "elsewhere");
  expect(current.role).toBeNull();
  expect(current.can("read")).toBe(false);
});
