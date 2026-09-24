import { expect, it, vi } from "vitest";
import { LightspeedRpcError, type LightspeedClient } from "@lightspeed-ai/agent-client";
import type { AppContext } from "../context.js";
import { ensureMcpServer } from "./setups.js";

function install(existing: { revision: number } | null) {
  const puts: Record<string, unknown>[] = [];
  const client = {
    call: vi.fn(async (method: string, params: Record<string, unknown>) => {
      if (method === "mcp/servers/read") {
        if (!existing) throw new LightspeedRpcError({ code: -32004, message: "not found" });
        return { result: { server: { serverId: "lightspeed-configurator", ...existing } } };
      }
      expect(method).toBe("mcp/servers/put");
      puts.push(params);
      return { result: { server: params.server } };
    }),
  } as unknown as LightspeedClient;
  const ctx = {
    db: { update: () => ({ set: () => ({ where: async () => undefined }) }) },
  } as unknown as AppContext;
  const state = {
    grantId: "authgrant_configurator",
    ...(existing ? { serverId: "lightspeed-configurator" } : {}),
  };
  return {
    puts,
    run: () => ensureMcpServer(ctx, "installation", client, state, "https://configurator.example/mcp", false),
  };
}

it("registers a new Configurator server restricted to the installing Admin", async () => {
  const { puts, run } = install(null);
  await run();
  expect(puts).toHaveLength(1);
  expect(puts[0]).toMatchObject({ access: { visibility: "restricted" } });
  expect(puts[0]).not.toHaveProperty("expectedRevision");
});

it("repairs an installed server without touching the access chosen since", async () => {
  const { puts, run } = install({ revision: 4 });
  await run();
  expect(puts).toHaveLength(1);
  expect(puts[0]).toMatchObject({ expectedRevision: 4 });
  expect(puts[0]).not.toHaveProperty("access");
});
