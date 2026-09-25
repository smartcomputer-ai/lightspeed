import { afterEach, describe, expect, it, vi } from "vitest";
import { LightspeedRpcError, METHODS } from "@lightspeed-ai/agent-client";
import type { UniverseRole } from "@lightspeed/platform-shared";
import { deploymentClient, GateRefusal, memberClient } from "./runtime-client.js";
import { METHOD_ROLES, UNMEMBERED_METHODS } from "./routes/method-roles.js";
import type { ServerEnv } from "./env.js";

const env = { lightspeedApiUrl: "https://core.example/rpc", lightspeedApiKey: "lsk_fixture" } as ServerEnv;
const universe = { lightspeedUniverseId: "11111111-1111-4111-8111-111111111111", gatewayUrl: null };
afterEach(() => vi.unstubAllGlobals());

type Access = { visibility: "universe" | "restricted"; createdBy?: { kind: "actor"; id: string } };

/// A core that knows `sessions` by id and records every call it receives.
function core(sessions: Record<string, Access> = {}) {
  const calls: { method: string; params: Record<string, unknown>; headers: Headers }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push({ method: rpc.method, params: rpc.params, headers: new Headers(init.headers) });
    expect(init.redirect).toBe("error");
    if (rpc.method === "session/read") {
      const access = sessions[rpc.params.sessionId];
      if (!access) {
        return Response.json({ id: rpc.id, error: { code: -32004, message: "not found", data: { kind: "not_found", message: "not found" } } });
      }
      return Response.json({ id: rpc.id, result: { result: { session: { id: rpc.params.sessionId, access } }, notifications: [] } });
    }
    return Response.json({ id: rpc.id, result: { result: {}, notifications: [] } });
  }));
  return calls;
}

const as = (role: UniverseRole, userId = "alice") => memberClient(env, universe, { userId, role });
const refusal = (promise: Promise<unknown>) => promise.then(() => null, (error: unknown) => error);

describe("method roles", () => {
  it("classify every core method exactly once", () => {
    for (const method of METHODS) {
      expect([method in METHOD_ROLES, UNMEMBERED_METHODS.has(method)], method).toContain(true);
      expect(method in METHOD_ROLES && UNMEMBERED_METHODS.has(method), method).toBe(false);
    }
  });

  it("refuse a role below the method's before reaching core", async () => {
    const calls = core({ s: { visibility: "universe" } });
    const viewer = await refusal(as("viewer").call("session/runs/start", { sessionId: "s", source: { type: "input", items: [] } }));
    expect(viewer).toBeInstanceOf(GateRefusal);
    expect((viewer as GateRefusal).status).toBe(403);
    const contributor = await refusal(as("contributor").call("profiles/put", { profile: { profileId: "p" } } as never));
    expect((contributor as GateRefusal).status).toBe(403);
    expect(calls).toHaveLength(0);
  });

  it("never call a deployment method on a member's behalf", async () => {
    const calls = core();
    const error = await refusal(as("admin").call("deployment/universes/list", {}));
    expect((error as GateRefusal).status).toBe(403);
    expect(calls).toHaveLength(0);
  });
});

describe("session targets", () => {
  it("hide another member's unshared session and allow it once shared", async () => {
    const sessions: Record<string, Access> = { draft: { visibility: "restricted", createdBy: { kind: "actor", id: "bob" } } };
    core(sessions);
    const hidden = await refusal(as("contributor").call("session/events/read", { sessionId: "draft" }));
    expect((hidden as GateRefusal).status).toBe(404);
    sessions.draft = { visibility: "universe", createdBy: { kind: "actor", id: "bob" } };
    await expect(as("contributor").call("session/events/read", { sessionId: "draft" })).resolves.toBeDefined();
  });

  it("let the creator and admins act on unshared work; admins without a pre-read", async () => {
    const calls = core({ draft: { visibility: "restricted", createdBy: { kind: "actor", id: "alice" } } });
    await as("contributor").call("session/events/read", { sessionId: "draft" });
    expect(calls.map((call) => call.method)).toEqual(["session/read", "session/events/read"]);
    calls.length = 0;
    await as("admin", "carol").call("session/events/read", { sessionId: "draft" });
    expect(calls.map((call) => call.method)).toEqual(["session/events/read"]);
  });

  it("keep share and delete with the creator even on shared work", async () => {
    core({ team: { visibility: "universe", createdBy: { kind: "actor", id: "bob" } } });
    for (const method of ["session/share", "session/delete"] as const) {
      const error = await refusal(as("contributor").call(method, { sessionId: "team" } as never));
      expect((error as GateRefusal).status).toBe(404);
    }
    await expect(as("contributor", "bob").call("session/share", { sessionId: "team" })).resolves.toBeDefined();
  });

  it("let a start name a session that does not exist yet, but not someone else's", async () => {
    core({ theirs: { visibility: "restricted", createdBy: { kind: "actor", id: "bob" } } });
    await expect(as("contributor").call("session/start", { sessionId: "fresh" })).resolves.toBeDefined();
    const error = await refusal(as("contributor").call("session/start", { sessionId: "theirs" }));
    expect((error as GateRefusal).status).toBe(404);
    const missing = await refusal(as("contributor").call("session/events/read", { sessionId: "gone" }));
    expect(missing).toBeInstanceOf(LightspeedRpcError);
  });
});

describe("lists and transport", () => {
  it("narrow a member's session list to shared work and their own", async () => {
    const calls = core();
    await as("viewer").call("session/list", { limit: 10 });
    await as("admin").call("session/list", { limit: 10 });
    expect(calls[0]!.params).toEqual({ limit: 10, visibleTo: "alice" });
    expect(calls[1]!.params).toEqual({ limit: 10 });
  });

  it("send the deployment key, the universe and the member as actor", async () => {
    const calls = core();
    await as("viewer").call("profiles/list", {});
    await deploymentClient(env).call("deployment/universes/list", {});
    expect(calls[0]!.headers.get("authorization")).toBe("Bearer lsk_fixture");
    expect(calls[0]!.headers.get("x-lightspeed-universe")).toBe(universe.lightspeedUniverseId);
    expect(calls[0]!.headers.get("x-lightspeed-actor")).toBe("alice");
    expect(calls[1]!.headers.get("x-lightspeed-universe")).toBeNull();
    expect(calls[1]!.headers.get("x-lightspeed-actor")).toBeNull();
  });

  it("keep the key on the configured runtime", () => {
    for (const gatewayUrl of ["https://other.example/rpc", "https://core.example/other", "https://user:password@core.example/rpc"]) {
      expect(() => memberClient(env, { ...universe, gatewayUrl }, { userId: "alice", role: "admin" })).toThrow("runtime endpoint");
      expect(() => deploymentClient(env, gatewayUrl)).toThrow("runtime endpoint");
    }
    expect(() => deploymentClient({ ...env, lightspeedApiKey: null })).toThrow("runtime endpoint");
  });
});
