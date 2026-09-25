// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UNIVERSE_ROLES } from "@lightspeed/platform-shared";
import { allowsAction, PermissionIdentityProvider, useActionPermissions, type PermissionAction } from "./permissions";

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

const universes = (role: string | null) => mocks.api.mockReset().mockResolvedValue([{ id: "universe", slug: "test", role }]);

it("gives each role what the one below has, plus its own", () => {
  const offered = (action: PermissionAction) => UNIVERSE_ROLES.filter((role) => allowsAction(role, action));
  expect(offered("read")).toEqual(["viewer", "contributor", "operator", "admin"]);
  expect(offered("control_session")).toEqual(["contributor", "operator", "admin"]);
  expect(offered("share_session")).toEqual(["contributor", "operator", "admin"]);
  expect(offered("configure_resource")).toEqual(["operator", "admin"]);
  expect(offered("create_profile")).toEqual(["operator", "admin"]);
  expect(offered("manage_access")).toEqual(["admin"]);
  expect(allowsAction(null, "read")).toBe(false);
});

it("reads the member's role in the universe on screen", async () => {
  universes("contributor");
  await show();
  expect(current.role).toBe("contributor");
  expect(current.can("control_session")).toBe(true);
  expect(current.can("configure_resource")).toBe(false);
});

it("treats a platform admin as an admin of every universe", async () => {
  universes(null);
  await show(true);
  expect(current.role).toBe("admin");
  expect(current.can("manage_access")).toBe(true);
});

it("offers nothing for another universe or before the universe loads", async () => {
  mocks.api.mockReset().mockReturnValue(new Promise(() => {}));
  await show();
  expect(current.isLoading).toBe(true);
  expect(current.can("read")).toBe(false);
  universes("admin");
  client.clear();
  await show(false, "elsewhere");
  expect(current.role).toBeNull();
  expect(current.can("read")).toBe(false);
});
