import { afterEach, describe, expect, it, vi } from "vitest";
import type { EffectiveAccess } from "@lightspeed-ai/agent-client";
import { actingPrincipal, requestIdentity, provisioningClient } from "./runtime-client.js";
import { engineClientFor, deploymentClientFor } from "./routes/gateway.js";
import type { AppContext } from "./context.js";

const ctx = { env: { lightspeedApiUrl: "https://core.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext;
const universe = { lightspeedUniverseId: "11111111-1111-4111-8111-111111111111", gatewayUrl: null } as Parameters<typeof engineClientFor>[1];
const identity = (id: string) => ({ principal: { id }, roles: [] }) as unknown as EffectiveAccess;
afterEach(() => vi.unstubAllGlobals());

describe("canonical user request boundary", () => {
  it("fails closed without a user; provisioning requires the explicit service client", () => {
    expect(() => engineClientFor(ctx, universe)).toThrow("user context required");
    expect(() => deploymentClientFor(ctx)).toThrow("user context required");
    expect(() => provisioningClient(ctx.env)).not.toThrow();
  });
  it("isolates parallel callers and attributes both deployment and universe calls", async () => {
    const seen: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (_: unknown, init: RequestInit) => {
      const rpc = JSON.parse(String(init.body));
      const headers = new Headers(init.headers);
      expect(headers.get("authorization")).toBe("Bearer lsk_fixture");
      expect(init.redirect).toBe("error");
      const principal = headers.get("x-lightspeed-principal")!;
      seen.push(principal);
      expect(principal).toBe(`user:${actingPrincipal()}`);
      return Response.json({ id: rpc.id, result: { result: {}, notifications: [] } });
    }));
    await Promise.all(["alice", "bob"].map((id) => requestIdentity.run(identity(id), async () => {
      await new Promise((resolve) => setTimeout(resolve, id === "alice" ? 5 : 1));
      await deploymentClientFor(ctx).call("deployment/identity/self", { scope: { kind: "deployment" } });
      await engineClientFor(ctx, universe).call("profiles/list", {});
    })));
    expect(seen.sort()).toEqual(["user:alice", "user:alice", "user:bob", "user:bob"]);
    expect(() => actingPrincipal()).toThrow();
  });
  it("checks endpoint confinement even with a valid user context", () => {
    requestIdentity.run(identity("alice"), () => {
      for (const gatewayUrl of ["https://other.example/rpc", "https://core.example/other", "https://user:password@core.example/rpc"]) {
        expect(() => engineClientFor(ctx, { ...universe, gatewayUrl })).toThrow("runtime endpoint");
        expect(() => deploymentClientFor(ctx, gatewayUrl)).toThrow("runtime endpoint");
      }
    });
  });
});
