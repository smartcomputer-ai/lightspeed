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

it("marks attachment choices the session's identity may not use, with the reason", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const usable = new Set(["mcp_server:github", "workspace:docs", "environment:ci"]);
  mocks.api.mockImplementation(async (_method: string, path: string, body?: { resources?: { kind: string; id: string }[]; as?: string }) => {
    if (path.endsWith("/access")) return {
      actions: ["read", "use_resource"],
      resources: (body?.resources ?? []).map((resource) => ({
        resource,
        // The default agent identity is refused what the person may use.
        actions: body?.as === "execution_service" && !usable.has(`${resource.kind}:${resource.id}`) ? ["read"] : ["read", "use_resource"],
      })),
    };
    if (path.endsWith("/mcp-servers")) return [{ serverId: "github" }, { serverId: "configurator" }];
    if (path.endsWith("/workspaces")) return [{ workspaceId: "docs" }];
    if (path.endsWith("/environments")) return [{ environmentId: "production" }, { environmentId: "ci" }];
    return [];
  });
  let options: ReturnType<typeof useSessionConfigEditorOptions> | undefined;
  function Options({ execution }: { execution?: "service" | "personal" }) {
    options = useSessionConfigEditorOptions("universe", true, execution);
    return null;
  }
  container = document.createElement("div");
  root = createRoot(container);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  const render = (execution?: "service" | "personal") =>
    act(async () => root.render(<QueryClientProvider client={client}><PermissionIdentityProvider userId="user"><Options execution={execution} /></PermissionIdentityProvider></QueryClientProvider>));
  await render("service");
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  const reason = "Default agent identity cannot use this";
  expect(options?.mcpServers?.map((server) => [server.serverId, server.unusable])).toEqual([["github", undefined], ["configurator", reason]]);
  expect(options?.workspaces?.map((workspace) => workspace.unusable)).toEqual([undefined]);
  expect(options?.environments?.map((environment) => [environment.environmentId, environment.unusable])).toEqual([["production", reason], ["ci", undefined]]);
  expect(mocks.api.mock.calls.some(([, path, body]) => path.endsWith("/access") && body?.as === "execution_service")).toBe(true);
  // Personal work is decided for the person themselves.
  await render("personal");
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(options?.environments?.every((environment) => environment.unusable === undefined)).toBe(true);
  const lastRead = mocks.api.mock.calls.filter(([, path]) => path.endsWith("/access")).at(-1);
  expect(lastRead?.[2]).toHaveProperty("resources");
  expect(lastRead?.[2]).not.toHaveProperty("as");
  // A profile runs as nobody: its attachments are checked when applied.
  await render(undefined);
  for (let step = 0; step < 4; step++) await act(async () => { await vi.advanceTimersByTimeAsync(5); });
  expect(options?.mcpServers?.every((server) => server.unusable === undefined)).toBe(true);
  expect(options?.environments?.every((environment) => environment.unusable === undefined)).toBe(true);
});
