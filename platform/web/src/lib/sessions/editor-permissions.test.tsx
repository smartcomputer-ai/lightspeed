// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { useMcpToolDiscovery } from "@/lib/mcp/tool-discovery";
import { useSessionConfigEditorOptions } from "./editor-options";
const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
let client: QueryClient;
afterEach(async () => {
  await act(async () => root?.unmount());
  client?.clear();
  container?.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
function Probe() {
  const options = useSessionConfigEditorOptions("universe");
  useMcpToolDiscovery({ source: options.mcpToolDiscovery, serverId: "server", enabled: true });
  return null;
}
it("requires resource configuration permission for live discovery even in a writable session/profile editor", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  let configure = false;
  mocks.api.mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions: configure ? ["read", "create_profile", "configure_resource"] : ["read", "create_profile"], resources: [] };
    if (path.endsWith("/tools/discover")) return { status: "success", tools: [] };
    return [];
  });
  container = document.createElement("div");
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  await act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><Probe /></PermissionIdentityProvider></QueryClientProvider>));
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(mocks.api.mock.calls.filter((call) => call[1].endsWith("/tools/discover"))).toHaveLength(0);
  configure = true;
  await act(async () => { await client.invalidateQueries({ queryKey: ["action-permissions"] }); });
  await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(mocks.api.mock.calls.filter((call) => call[1].endsWith("/tools/discover"))).toHaveLength(1);
});
