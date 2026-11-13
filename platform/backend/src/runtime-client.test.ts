import { afterEach, describe, expect, it, vi } from "vitest";
import { LightspeedRpcError, METHODS } from "@lightspeed-ai/sdk";
import type { UniverseRole } from "@lightspeed-ai/platform-shared";
import { deploymentClient, GateRefusal, memberClient } from "./runtime-client.js";
import { METHOD_ROLES, UNMEMBERED_METHODS } from "./routes/method-roles.js";
import type { ServerEnv } from "./env.js";

const env = { lightspeedApiUrl: "https://core.example/rpc", lightspeedApiKey: "lsk_fixture" } as ServerEnv;
const universe = { lightspeedUniverseId: "11111111-1111-4111-8111-111111111111", gatewayUrl: null };
const integration = { version: 1, tools: [] };
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
        return Response.json({ id: rpc.id, error: { code: -32004, message: "not found", data: { kind: "notFound", message: "not found" } } });
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

  it.each(["viewer", "contributor"] as const)("refuses workspace creation by %s before reaching core", async (role) => {
    const calls = core();
    const error = await refusal(as(role).call("vfs/workspaces/create", {}));
    expect(error).toBeInstanceOf(GateRefusal);
    expect((error as GateRefusal).status).toBe(403);
    expect(calls).toHaveLength(0);
  });

  it.each(["operator", "admin"] as const)("allows workspace creation by %s", async (role) => {
    const calls = core();
    await expect(as(role).call("vfs/workspaces/create", {})).resolves.toBeDefined();
    expect(calls.map((call) => call.method)).toEqual(["vfs/workspaces/create"]);
  });

  it("still allows contributors to update workspace contents", async () => {
    const calls = core();
    await expect(as("contributor").call("vfs/workspaces/update", { workspaceId: "docs", snapshotRef: `sha256:${"a".repeat(64)}` })).resolves.toBeDefined();
    expect(calls.map((call) => call.method)).toEqual(["vfs/workspaces/update"]);
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

  it("keep sharing with the creator even on shared work", async () => {
    core({ team: { visibility: "universe", createdBy: { kind: "actor", id: "bob" } } });
    for (const method of ["session/share"] as const) {
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


describe("prepared contributor sessions", () => {
  it("names the operator role when refusing an integration", async () => {
    const error = await refusal(as("contributor").call("session/start", { integration } as never));
    expect((error as GateRefusal).message).toBe("operator role required to create integrated sessions");
  });

  it("lets an operator start an integrated session", async () => {
    const calls = core();
    await as("operator").call("session/start", { integration } as never);
    expect(calls.at(-1)!.params).toMatchObject({ integration });
  });

  it.each([
    {},
    { displayName: "Research", profile: { kind: "inline", profile: {} } },
    { profile: { kind: "named", profileId: "approved" } },
  ])("allows defaults and existing profiles: %j", async (params) => {
    const calls = core();
    await as("contributor").call("session/start", params as never);
    expect(calls.map((call) => call.method)).toEqual(["session/start"]);
    expect(calls[0]!.params).toMatchObject(params);
  });

  it.each([
    { integration },
    { integration: null },
    { profile: { kind: "named", profileId: "approved" }, integration },
    { config: {} },
    { config: { model: { model: "custom" } } },
    { metadata: { team: "custom" } },
    { deleteAfterCloseMs: null },
    { deleteAfterCloseMs: 1000 },
    { access: { visibility: "universe" } },
    { profile: { kind: "inline", profile: { instructions: { type: "text", text: "Override" } } } },
    { profile: { kind: "inline", profile: { config: {} } } },
    { profile: { kind: "named", profileId: "approved", config: {} } },
    { profile: { kind: "named", profileId: "approved" }, config: {} },
  ])("refuses contributor overrides before calling core: %j", async (params) => {
    const calls = core();
    const error = await refusal(as("contributor").call("session/start", params as never));
    expect(error).toBeInstanceOf(GateRefusal);
    expect((error as GateRefusal).status).toBe(403);
    expect(calls).toHaveLength(0);
  });

  it.each([
    "session/config/put", "session/profiles/apply", "session/metadata/put",
    "session/environments/activate", "session/environments/deactivate",
    "profiles/create", "profiles/put", "profiles/delete",
  ] as const)("keeps %s out of contributor access, including owned sessions", async (method) => {
    const calls = core({ own: { visibility: "restricted", createdBy: { kind: "actor", id: "alice" } } });
    const error = await refusal(as("contributor").call(method, { sessionId: "own" } as never));
    expect(error).toBeInstanceOf(GateRefusal);
    expect((error as GateRefusal).status).toBe(403);
    expect(calls).toHaveLength(0);
  });

  it("allows contributor runs but refuses per-run configuration overrides", async () => {
    const calls = core({ own: { visibility: "restricted", createdBy: { kind: "actor", id: "alice" } } });
    const params = { sessionId: "own", source: { type: "input", items: [] } } as const;
    const error = await refusal(as("contributor").call("session/runs/start", { ...params, config: {} } as never));
    expect((error as GateRefusal).status).toBe(403);
    expect(calls).toHaveLength(0);
    await as("contributor").call("session/runs/start", params as never);
    expect(calls.map((call) => call.method)).toEqual(["session/read", "session/runs/start"]);
  });

  it.each(["operator", "admin"] as const)("preserves custom creation and configuration for %s", async (role) => {
    const calls = core({ own: { visibility: "restricted", createdBy: { kind: "actor", id: "alice" } } });
    await as(role).call("session/start", { profile: { kind: "inline", profile: { config: {} } } } as never);
    await as(role).call("session/config/put", { sessionId: "own", config: {} } as never);
    expect(calls.map((call) => call.method)).toContain("session/config/put");
  });
});


describe("session lifecycle permissions", () => {
  for (const role of ["viewer", "contributor", "operator", "admin"] as const) {
    for (const creator of [true, false]) for (const shared of [true, false]) {
      it(`${role} closes creator=${creator} shared=${shared} only when permitted`, async () => {
        core({ s: { visibility: shared ? "universe" : "restricted", createdBy: { kind: "actor", id: creator ? "alice" : "bob" } } });
        const allowed = role === "admin" || (role === "operator" && (creator || shared)) || (role === "contributor" && creator && !shared);
        for (const force of [false, true]) {
          const error = await refusal(as(role).call("session/close", { sessionId: "s", force }));
          expect(error === null).toBe(allowed);
        }
      });
    }
    for (const shared of [true, false]) it(`${role} soft deletes shared=${shared} only when permitted`, async () => {
      const calls = core({ s: { visibility: shared ? "universe" : "restricted", createdBy: { kind: "actor", id: "alice" } } });
      const error = await refusal(as(role).call("session/delete", { sessionId: "s", sharedOnly: false, softDelete: false }));
      expect(error === null).toBe(role === "admin" || (role === "operator" && shared));
      if (error === null) expect(calls.at(-1)?.params.softDelete).toBe(true);
      if (role === "operator" && shared) expect(calls.at(-1)?.params.sharedOnly).toBe(true);
    });
    it(`${role} deletes and configures retention only as admin`, async () => {
      const calls = core({ s: { visibility: "restricted", createdBy: { kind: "actor", id: "alice" } } });
      for (const method of ["session/delete", "session/retention/put"] as const) {
        const error = await refusal(as(role).call(method, { sessionId: "s", deleteAfterCloseMs: 1 } as never));
        expect(error === null).toBe(role === "admin");
      }
      if (role !== "admin") expect(calls.every((call) => call.method === "session/read")).toBe(true);
    });
  }
  it("contributors still cancel shared runs", async () => {
    core({ s: { visibility: "universe" } });
    await expect(as("contributor").call("session/runs/cancel", { sessionId: "s", runId: "r" })).resolves.toBeDefined();
  });
  it("non-admin starts override profile retention and reject an explicit deletion schedule", async () => {
    const calls = core();
    for (const start of [{}, { integration }]) {
      await as("operator").call("session/start", { ...start, profile: { kind: "named", profileId: "scheduled" } } as never);
      expect(calls.at(-1)!.params.deleteAfterCloseMs).toBeNull();
      expect(await refusal(as("operator").call("session/start", { ...start, deleteAfterCloseMs: 1 } as never))).toBeInstanceOf(GateRefusal);
    }
  });
});

describe("feature switches", () => {
  const within = (features: unknown, role: UniverseRole = "admin") =>
    memberClient(env, { ...universe, features }, { userId: "alice", role });
  const featureMethods = (prefix: string) =>
    (Object.keys(METHOD_ROLES) as (typeof METHODS)[number][]).filter((method) => method.startsWith(prefix));

  it("refuse every bot and channel method, for an admin too, in a universe that stored no switch", async () => {
    const calls = core();
    expect(featureMethods("bots/").length).toBeGreaterThan(0);
    expect(featureMethods("channels/").length).toBeGreaterThan(0);
    for (const client of [as("admin"), within({}), within(null)]) {
      for (const method of [...featureMethods("bots/"), ...featureMethods("channels/")]) {
        const error = await refusal(client.call(method, {} as never));
        expect(error, method).toBeInstanceOf(GateRefusal);
        expect((error as GateRefusal).status, method).toBe(403);
      }
    }
    expect(calls).toHaveLength(0);
  });

  it("answer with the switch that is off, in the shape of every other gate refusal", async () => {
    core();
    const bots = await refusal(within({}).call("bots/list", {}));
    expect((bots as GateRefusal).message).toBe("Bots are turned off in this universe");
    const channels = await refusal(within({ bots: true }).call("channels/accounts/list", {} as never));
    expect((channels as GateRefusal).status).toBe(403);
    expect((channels as GateRefusal).message).toBe("Channels are turned off in this universe");
  });

  it("let bot methods through with Bots on, and keep channel methods refused until Channels is on", async () => {
    const calls = core();
    await within({ bots: true }).call("bots/list", {});
    expect(await refusal(within({ bots: true }).call("channels/accounts/list", {} as never))).toBeInstanceOf(GateRefusal);
    await within({ bots: true, channels: true }).call("channels/accounts/list", {} as never);
    await within({ bots: true, channels: true }).call("bots/list", {});
    expect(calls.map((call) => call.method)).toEqual(["bots/list", "channels/accounts/list", "bots/list"]);
  });

  it("refuse channel methods while Bots are off, whatever Channels' own switch says", async () => {
    const calls = core();
    for (const features of [{ channels: true }, { bots: false, channels: true }]) {
      const error = await refusal(within(features).call("channels/accounts/list", {} as never));
      expect((error as GateRefusal).message).toBe("Channels are turned off in this universe");
      expect(await refusal(within(features).call("bots/list", {}))).toBeInstanceOf(GateRefusal);
    }
    expect(calls).toHaveLength(0);
  });

  it("treat stored switches it cannot read as nothing stored", async () => {
    const calls = core();
    for (const features of ["on", [true], { bots: "yes" }]) {
      expect(await refusal(within(features).call("bots/list", {}))).toBeInstanceOf(GateRefusal);
    }
    expect(calls).toHaveLength(0);
  });

  it("still hold a member to the method's role once the feature is on", async () => {
    const calls = core();
    const error = await refusal(within({ bots: true, channels: true }, "viewer").call("bots/create", {} as never));
    expect((error as GateRefusal).message).toBe("operator role required");
    await within({ bots: true }, "viewer").call("bots/list", {});
    expect(calls.map((call) => call.method)).toEqual(["bots/list"]);
  });

  it("leave every other method to its own rules", async () => {
    const calls = core();
    await within({}, "viewer").call("profiles/list", {});
    await within({ bots: false }, "viewer").call("session/list", { limit: 1 });
    expect(calls.map((call) => call.method)).toEqual(["profiles/list", "session/list"]);
  });
});

describe("trigger secrets", () => {
  const within = (role: UniverseRole) =>
    memberClient(env, { ...universe, features: { bots: true, channels: true } }, { userId: "alice", role });
  const target = { botId: "triage", triggerId: "github" };

  it("let every member read a trigger and only operators read its secrets", async () => {
    const calls = core();
    for (const role of ["viewer", "contributor"] as const) {
      await within(role).call("bots/triggers/list", { botId: "triage" });
      await within(role).call("bots/triggers/read", target);
      const error = await refusal(within(role).call("bots/triggers/secrets/read", target));
      expect(error, role).toBeInstanceOf(GateRefusal);
      expect((error as GateRefusal).status, role).toBe(403);
      expect((error as GateRefusal).message, role).toBe("operator role required");
    }
    expect(calls.map((call) => call.method)).not.toContain("bots/triggers/secrets/read");
    for (const role of ["operator", "admin"] as const) {
      await within(role).call("bots/triggers/secrets/read", target);
    }
    expect(calls.filter((call) => call.method === "bots/triggers/secrets/read")).toHaveLength(2);
  });

  it("hold the methods that answer with secrets to the operator role too", () => {
    expect(METHOD_ROLES["bots/triggers/secrets/read"]).toBe("operator");
    expect(METHOD_ROLES["bots/triggers/put"]).toBe("operator");
    expect(METHOD_ROLES["bots/create"]).toBe("operator");
    expect(METHOD_ROLES["bots/triggers/read"]).toBe("viewer");
    expect(METHOD_ROLES["bots/triggers/list"]).toBe("viewer");
  });
});
