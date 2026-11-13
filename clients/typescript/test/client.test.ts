import { afterEach, beforeEach, describe, expect, expectTypeOf, it, vi } from "vitest";
import {
  CLIENT_HELPERS,
  LightspeedClient,
  LightspeedRpcError,
  METHOD_INFO,
  METHODS,
  type LightspeedClientOptions,
  type MethodData,
  type RunFailureKindView,
  type SessionEventKindOf,
  type SessionEventView,
} from "../src/index.js";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function decodeBody(init: RequestInit | undefined): Record<string, unknown> {
  expect(init?.body).toBeTypeOf("string");
  return JSON.parse(init?.body as string) as Record<string, unknown>;
}

/** Answers only by rejecting on abort, as fetch does, including a signal already aborted. */
const hangingFetch = ((_input: RequestInfo | URL, init?: RequestInit) =>
  new Promise((_resolve, reject) => {
    const signal = init?.signal;
    if (signal?.aborted) {
      reject(signal.reason);
    }
    signal?.addEventListener("abort", () => reject(signal.reason));
  })) as typeof fetch;

describe("LightspeedClient", () => {
  it("classifies missing model defaults and retains the affected slot", () => {
    const error = new LightspeedRpcError({ code: -32014, message: "Choose a model", data: { kind: "modelDefaultUnset", message: "Choose a model", modelDefaultSlot: "agentRun" } });
    expect(error.kind).toBe("modelDefaultUnset");
    expect(new LightspeedRpcError({ code: -32015, message: "still held" }).kind).toBe("operationPending");
    expect(new LightspeedRpcError({ code: -32016, message: "stopped" }).kind).toBe("sessionExecutionStopped");
    expect(error.data?.modelDefaultSlot).toBe("agentRun");
  });

  it("takes the server's kind over the code, which several kinds can share", () => {
    const kinds = [
      [-32602, "audioBlobTooLarge"],
      [-32011, "sessionBootstrapFailed"],
      [-32013, "responseTooLarge"],
    ] as const;
    for (const [code, kind] of kinds) {
      expect(new LightspeedRpcError({ code, message: "m", data: { kind, message: "m" } }).kind).toBe(kind);
    }
    expect(new LightspeedRpcError({ code: -32013, message: "m" }).kind).toBe("responseTooLarge");
    expect(new LightspeedRpcError({ code: -32601, message: "m" }).kind).toBe("methodNotFound");
    expect(new LightspeedRpcError({ code: -1, message: "m" }).kind).toBe("unknown");
  });

  it("omits universe selection on connection and deployment calls", async () => {
    const captured: Headers[] = [];
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      captured.push(new Headers(init?.headers));
      return jsonResponse({ id: decodeBody(init).id, result: { result: {}, notifications: [] } });
    }) as unknown as typeof fetch;
    const client = new LightspeedClient({ endpoint: "http://localhost/rpc", fetch: fetchImpl,
      headers: { authorization: "Bearer fixture", "x-lightspeed-universe": "universe" } });
    await client.call("initialize", {});
    await client.call("deployment/universes/list", {});
    await client.call("models/list", {});
    expect(captured.map(h => h.get("x-lightspeed-universe"))).toEqual([null, null, "universe"]);
    expect(captured.every(h => h.get("authorization") === "Bearer fixture")).toBe(true);
  });
  describe("connection options", () => {
    function capturing(options: Partial<LightspeedClientOptions>) {
      const captured: Headers[] = [];
      const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
        captured.push(new Headers(init?.headers));
        return jsonResponse({ id: decodeBody(init).id, result: { result: {}, notifications: [] } });
      }) as typeof fetch;
      return { captured, client: new LightspeedClient({ endpoint: "http://localhost/rpc", fetch: fetchImpl, ...options }) };
    }

    it("sends the key, universe and actor as their headers", async () => {
      const { captured, client } = capturing({ apiKey: "lsk_fixture", universe: "universe_1", actor: "user_1" });
      await client.call("models/list", {});
      expect(captured[0]?.get("authorization")).toBe("Bearer lsk_fixture");
      expect(captured[0]?.get("x-lightspeed-universe")).toBe("universe_1");
      expect(captured[0]?.get("x-lightspeed-actor")).toBe("user_1");
    });

    it("sends none of them when they are not given", async () => {
      const { captured, client } = capturing({});
      await client.call("models/list", {});
      for (const name of ["authorization", "x-lightspeed-universe", "x-lightspeed-actor"]) {
        expect(captured[0]?.has(name)).toBe(false);
      }
    });

    it("omits the universe on calls that address no universe", async () => {
      const { captured, client } = capturing({ apiKey: "lsk_fixture", universe: "universe_1" });
      await client.call("initialize", {});
      expect(captured[0]?.has("x-lightspeed-universe")).toBe(false);
      expect(captured[0]?.get("authorization")).toBe("Bearer lsk_fixture");
    });

    it("lets explicit headers override the options, and a call override both", async () => {
      const { captured, client } = capturing({ apiKey: "lsk_option", headers: { authorization: "Bearer lsk_headers" } });
      await client.call("models/list", {});
      await client.call("models/list", {}, { headers: { authorization: "Bearer lsk_call" } });
      expect(captured.map((headers) => headers.get("authorization"))).toEqual(["Bearer lsk_headers", "Bearer lsk_call"]);
    });

    it("withActor attributes only the new client's requests", async () => {
      const { captured, client } = capturing({ apiKey: "lsk_fixture" });
      const alice = client.withActor("alice");
      await alice.call("models/list", {});
      await client.call("models/list", {});
      await alice.withActor("bob").call("models/list", {});
      expect(captured.map((headers) => headers.get("x-lightspeed-actor"))).toEqual(["alice", null, "bob"]);
      expect(captured.every((headers) => headers.get("authorization") === "Bearer lsk_fixture")).toBe(true);
    });
  });

  it("names a method's data and each kind of event", () => {
    expectTypeOf<MethodData<"session/runs/read">["run"]["id"]>().toEqualTypeOf<string>();
    expectTypeOf<SessionEventKindOf<"runFailed">["kind"]>().toEqualTypeOf<RunFailureKindView>();
    expectTypeOf<SessionEventKindOf<"runFailed">["type"]>().toEqualTypeOf<"runFailed">();
  });

  it("ships canonical method documentation from the Rust manifest", () => {
    expect(METHOD_INFO["session/config/put"]).toEqual({
      scope: "universe",
      access: { kind: "universe", action: "configureSession" },
      summary: "Replace session configuration",
      description:
        "Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked, an omitted model preserves the current model, and an identical document is a no-op. A newly attached MCP server must be usable; a held one that has gone away only drops out of later runs.",
    });
    expect(METHOD_INFO["deployment/api-keys/create"]).toMatchObject({
      scope: "deployment",
      access: { kind: "deployment" },
      summary: "Create a scoped API key",
      description: expect.stringContaining("returned exactly once"),
    });
  });

  it("calls fetch the way a browser requires, not as a method of the client", async () => {
    const browserFetch = function (this: unknown, _input: RequestInfo | URL, init?: RequestInit) {
      if (this !== undefined && this !== globalThis) {
        throw new TypeError("Failed to execute 'fetch' on 'Window': Illegal invocation");
      }
      return Promise.resolve(jsonResponse({ id: decodeBody(init).id, result: { result: {}, notifications: [] } }));
    } as typeof fetch;
    const client = new LightspeedClient({ endpoint: "http://localhost/rpc", fetch: browserFetch });

    await expect(client.call("initialize", {})).resolves.toBeDefined();
  });

  it("posts typed JSON-RPC calls and returns the result envelope", async () => {
    const requests: Array<{ url: string; body: Record<string, unknown>; headers: Headers }> = [];
    const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      requests.push({
        url: input.toString(),
        body: decodeBody(init),
        headers: new Headers(init?.headers),
      });
      return jsonResponse({
        id: 42,
        result: {
          result: {
            session: {
              id: "session_1",
              status: "idle",
              runs: [],
            },
          },
          notifications: [],
        },
      });
    }) as unknown as typeof fetch;

    const client = new LightspeedClient({
      endpoint: "http://127.0.0.1:18080/rpc",
      fetch: fetchImpl,
      requestId: () => 42,
      headers: { authorization: "Bearer local" },
    });

    const result = await client.call("session/start", {
      sessionId: "session_1",
      config: null,
    });

    expect(result.result.session.id).toBe("session_1");
    expect(requests).toHaveLength(1);
    expect(requests[0]?.url).toBe("http://127.0.0.1:18080/rpc");
    expect(requests[0]?.headers.get("content-type")).toBe("application/json");
    expect(requests[0]?.headers.get("authorization")).toBe("Bearer local");
    expect(requests[0]?.body).toEqual({
      id: 42,
      method: "session/start",
      params: {
        sessionId: "session_1",
          config: null,
      },
    });
  });

  it("preserves JSON-RPC error code and data", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse({
        id: "req_1",
        error: {
          code: -32009,
          message: "submission id was reused with different input",
          data: {
            kind: "conflict",
            message: "submission id was reused with different input",
          },
        },
      }),
    ) as unknown as typeof fetch;
    const client = new LightspeedClient({
      endpoint: "http://lightspeed.local/rpc",
      fetch: fetchImpl,
      requestId: () => "req_1",
    });

    await expect(
      client.call("session/runs/start", {
        sessionId: "session_1",
        source: {
          type: "input",
          items: [{ type: "text", text: "hello" }],
        },
        submissionId: "sub_1",
      }),
    ).rejects.toMatchObject({
      name: "LightspeedRpcError",
      code: -32009,
      kind: "conflict",
      data: {
        kind: "conflict",
        message: "submission id was reused with different input",
      },
    } satisfies Partial<LightspeedRpcError>);
  });

  describe("blob helpers", () => {
    const serving = (bytes: Uint8Array) => {
      const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
        const body = decodeBody(init);
        const bytesBase64 = btoa(String.fromCharCode(...bytes));
        return jsonResponse({
          id: body.id,
          result: { result: { blobRef: "blob_1", bytes: bytes.length, bytesBase64 }, notifications: [] },
        });
      }) as typeof fetch;
      return new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });
    };

    it("returns the exact bytes, including those outside ASCII", async () => {
      const bytes = new Uint8Array([0, 127, 128, 255]);
      await expect(serving(bytes).readBlobBytes("blob_1")).resolves.toEqual(bytes);
    });

    it("decodes UTF-8 text", async () => {
      const client = serving(new TextEncoder().encode("héllo ✓"));
      await expect(client.readBlobText("blob_1")).resolves.toBe("héllo ✓");
    });

    it("refuses to present binary as text", async () => {
      await expect(serving(new Uint8Array([0xff, 0xfe])).readBlobText("blob_1")).rejects.toBeInstanceOf(TypeError);
    });
  });

  describe("named helpers", () => {
    function recording(data: unknown = {}) {
      const calls: Array<{ method: string; params: unknown; signal: AbortSignal | null | undefined; headers: Headers }> = [];
      const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
        const body = decodeBody(init);
        calls.push({ method: body.method as string, params: body.params, signal: init?.signal, headers: new Headers(init?.headers) });
        return jsonResponse({ id: body.id, result: { result: data, notifications: [] } });
      }) as typeof fetch;
      return { calls, client: new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl }) };
    }

    it("returns the method's data, without the envelope", async () => {
      const { client } = recording({ session: { id: "session_1" } });
      await expect(client.startSession({ displayName: "x" })).resolves.toEqual({ session: { id: "session_1" } });
    });

    it("sends positional IDs and options as one params object", async () => {
      const { calls, client } = recording();
      await client.closeSession("session_1", { force: true });
      await client.readRun("session_1", "run_1");
      await client.decideApprovals("session_1", "run_1", { decisions: [{ approvalId: "a", decision: "approve" }] });
      expect(calls.map(({ method, params }) => ({ method, params }))).toEqual([
        { method: "session/close", params: { sessionId: "session_1", force: true } },
        { method: "session/runs/read", params: { sessionId: "session_1", runId: "run_1" } },
        {
          method: "session/runs/approvals/decide",
          params: { sessionId: "session_1", runId: "run_1", decisions: [{ approvalId: "a", decision: "approve" }] },
        },
      ]);
    });

    it("keeps signal and headers out of the params and applies them to the request", async () => {
      const { calls, client } = recording();
      const controller = new AbortController();
      await client.listSessions({ limit: 5, signal: controller.signal, headers: { "x-test": "1" } });
      expect(calls[0]?.params).toEqual({ limit: 5 });
      expect(calls[0]?.signal).toBe(controller.signal);
      expect(calls[0]?.headers.get("x-test")).toBe("1");
    });

    it("keeps transport options out of the params of the hand-written helpers too", async () => {
      const { calls, client } = recording({ run: { id: "run_1" } });
      const options = { submissionId: "sub_1", signal: new AbortController().signal, headers: { "x-test": "1" } };
      await client.startRun("session_1", "hi", options);
      await client.steerRun("session_1", "run_1", "more", options);
      await client.readEvents("session_1", { limit: 1, signal: options.signal });
      expect(calls.map((call) => call.params)).toEqual([
        { sessionId: "session_1", source: { type: "input", items: [{ type: "text", text: "hi" }] }, submissionId: "sub_1" },
        { sessionId: "session_1", runId: "run_1", items: [{ type: "text", text: "more" }], submissionId: "sub_1" },
        { sessionId: "session_1", limit: 1 },
      ]);
      expect(calls.map((call) => call.headers.get("x-test"))).toEqual(["1", "1", null]);
    });

    it("gives steering a submission ID unless the caller supplies one", async () => {
      const { calls, client } = recording({});
      await client.steerRun("session_1", "run_1", [{ type: "text", text: "more" }]);
      expect((calls[0]?.params as { submissionId: unknown }).submissionId).toBeTypeOf("string");
    });

    it("rejects the same way as call", async () => {
      const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) =>
        jsonResponse({
          id: decodeBody(init).id,
          error: { code: -32004, message: "gone", data: { kind: "notFound", message: "gone" } },
        })) as typeof fetch;
      const client = new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });
      await expect(client.readSession("session_1")).rejects.toMatchObject({ kind: "notFound" });
    });

    it("has a real method for every API method in the table", () => {
      const client = new LightspeedClient("http://lightspeed.local/rpc");
      for (const [helper, method] of Object.entries(CLIENT_HELPERS)) {
        expect(METHODS).toContain(method);
        expect(client[helper as keyof typeof CLIENT_HELPERS], helper).toBeTypeOf("function");
      }
    });
  });

  it("putBlob stores text as UTF-8 and returns the reference", async () => {
    let sent: { blobs: Array<{ bytesBase64: string }> } | undefined;
    const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = decodeBody(init);
      sent = body.params as typeof sent;
      return jsonResponse({ id: body.id, result: { result: { blobs: [{ blobRef: "blob_1" }] }, notifications: [] } });
    }) as typeof fetch;
    const client = new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });

    await expect(client.putBlob("héllo")).resolves.toBe("blob_1");

    expect(new TextDecoder().decode(Uint8Array.from(atob(sent?.blobs[0]?.bytesBase64 ?? ""), (c) => c.charCodeAt(0)))).toBe("héllo");
  });

  it("startRun takes plain text as a single text item", async () => {
    let params: unknown;
    const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = decodeBody(init);
      params = body.params;
      return jsonResponse({ id: body.id, result: { result: { run: { id: "run_1" } }, notifications: [] } });
    }) as typeof fetch;
    const client = new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });

    await client.startRun("session_1", "hello", { submissionId: "sub_1" });

    expect(params).toEqual({
      sessionId: "session_1",
      source: { type: "input", items: [{ type: "text", text: "hello" }] },
      submissionId: "sub_1",
    });
  });

  it("generates submission ids for startRun unless the caller supplies one", async () => {
    const bodies: Record<string, unknown>[] = [];
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      bodies.push(decodeBody(init));
      return jsonResponse({
        id: bodies.length,
        result: {
          result: {
            run: {
              id: "run_1",
              status: "queued",
              source: { type: "input", items: [] },
              entries: [],
              toolBatches: [],
            },
          },
          notifications: [],
        },
      });
    }) as unknown as typeof fetch;
    const client = new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });

    await client.startRun("session_1", [{ type: "text", text: "hello" }]);
    await client.startRun("session_1", [{ type: "text", text: "hello again" }], {
      submissionId: "sub_fixed",
      config: null,
    });

    const firstParams = bodies[0]?.params as { submissionId?: unknown };
    expect(firstParams.submissionId).toBeTypeOf("string");
    expect(firstParams.submissionId).not.toBe("");
    expect(bodies[1]?.params).toMatchObject({
      sessionId: "session_1",
      source: {
        type: "input",
        items: [{ type: "text", text: "hello again" }],
      },
      submissionId: "sub_fixed",
      config: null,
    });
  });

  describe("awaitRun", () => {
    const completed = (seq: number, runId = "run_1"): SessionEventView => ({
      cursor: { seq },
      sessionId: "session_1",
      observedAtMs: seq,
      joins: { runId },
      kind: { type: "runCompleted", runId, output: null },
    });
    const turnStarted = (seq: number, runId = "run_1"): SessionEventView => ({
      cursor: { seq },
      sessionId: "session_1",
      observedAtMs: seq,
      joins: { runId, turnId: "1" },
      kind: { type: "turnStarted", runId, turnId: "1" },
    });
    const page = (events: SessionEventView[], headSeq?: number) => ({
      events,
      headCursor: { seq: headSeq ?? events.at(-1)?.cursor.seq ?? 0 },
      nextCursor: events.at(-1)?.cursor ?? null,
      complete: true,
    });
    const finishedRun = { id: "run_1", status: "completed", outputText: "done", source: { type: "input", items: [] } };

    /** A gateway answering each method from a handler, recording every call. */
    function gateway(handlers: Record<string, (params: Record<string, unknown>) => unknown>) {
      const calls: Array<{ method: string; params: Record<string, unknown> }> = [];
      const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
        const { id, method, params } = decodeBody(init) as {
          id: number;
          method: string;
          params: Record<string, unknown>;
        };
        calls.push({ method, params });
        const handler = handlers[method];
        if (!handler) {
          throw new Error(`unexpected call to ${method}`);
        }
        const result = handler(params);
        return result instanceof Response
          ? result
          : jsonResponse({ id, result: { result, notifications: [] } });
      }) as typeof fetch;
      return { calls, client: new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl }) };
    }

    it("resumes from cursors, returns the finished run, and reports only its own events", async () => {
      const pages = [page([turnStarted(1, "other_run"), turnStarted(2)]), page([completed(3, "other_run"), completed(4)])];
      const { calls, client } = gateway({
        "session/events/read": () => pages.shift(),
        "session/runs/read": () => ({ run: finishedRun }),
      });
      const onEvent = vi.fn();
      const heartbeat = vi.fn();

      const result = await client.awaitRun("session_1", "run_1", { waitMs: 1234, onEvent, heartbeat });

      expect(result.run.status).toBe("completed");
      expect(result.run.outputText).toBe("done");
      expect(result.cursor).toEqual({ seq: 4 });
      expect(onEvent.mock.calls.map(([event]) => event.cursor.seq)).toEqual([2, 4]);
      expect(heartbeat.mock.calls.map(([cursor]) => cursor)).toEqual([{ seq: 2 }, { seq: 4 }]);
      expect(calls.filter((call) => call.method === "session/events/read").map((call) => call.params)).toEqual([
        { sessionId: "session_1", after: null, waitMs: 0 },
        { sessionId: "session_1", after: { seq: 2 }, waitMs: 1234 },
      ]);
    });

    it("never advances to a head that moved past the page it read", async () => {
      // The terminal event landed between the empty read and the head query.
      const pages = [page([], 1), page([completed(1)])];
      const { calls, client } = gateway({
        "session/events/read": () => pages.shift(),
        "session/read": () => ({ session: { runs: [{ id: "run_1", status: "running" }] } }),
        "session/runs/read": () => ({ run: finishedRun }),
      });

      const result = await client.awaitRun("session_1", "run_1");

      expect(result.cursor).toEqual({ seq: 1 });
      expect(calls.filter((call) => call.method === "session/events/read").map((call) => call.params.after)).toEqual([
        null,
        null,
      ]);
    });

    it("returns at once when the run ended behind the cursor", async () => {
      const { calls, client } = gateway({
        "session/events/read": () => page([], 9),
        "session/read": () => ({ session: { runs: [{ id: "run_1", status: "completed" }] } }),
        "session/runs/read": () => ({ run: finishedRun }),
      });

      const result = await client.awaitRun("session_1", "run_1", { after: { seq: 9 } });

      expect(result).toEqual({ run: finishedRun, cursor: { seq: 9 } });
      expect(calls.map((call) => call.method)).toEqual([
        "session/events/read",
        "session/read",
        "session/runs/read",
      ]);
    });

    it("rejects when the session's execution has stopped", async () => {
      const { client } = gateway({
        "session/events/read": () => page([], 3),
        "session/read": () => ({
          session: { executionFailure: "terminated", runs: [{ id: "run_1", status: "running" }] },
        }),
      });

      await expect(client.awaitRun("session_1", "run_1", { after: { seq: 3 } })).rejects.toMatchObject({
        name: "LightspeedRpcError",
        kind: "sessionExecutionStopped",
      });
    });

    it("rejects for a run the session does not have", async () => {
      const { client } = gateway({
        "session/events/read": () => page([], 3),
        "session/read": () => ({ session: { runs: [] } }),
        "session/runs/read": () =>
          jsonResponse({
            id: 3,
            error: { code: -32004, message: "run not found", data: { kind: "notFound", message: "run not found" } },
          }),
      });

      await expect(client.awaitRun("session_1", "missing")).rejects.toMatchObject({ kind: "notFound" });
    });

    it("keeps waiting while the run is still going", async () => {
      const pages = [page([], 1), page([], 1), page([completed(2)])];
      const { calls, client } = gateway({
        "session/events/read": () => pages.shift(),
        "session/read": () => ({ session: { runs: [{ id: "run_1", status: "running" }] } }),
        "session/runs/read": () => ({ run: finishedRun }),
      });

      const result = await client.awaitRun("session_1", "run_1", { after: { seq: 1 }, waitMs: 5000 });

      expect(result.cursor).toEqual({ seq: 2 });
      expect(calls.filter((call) => call.method === "session/events/read").map((call) => call.params)).toEqual([
        { sessionId: "session_1", after: { seq: 1 }, waitMs: 0 },
        { sessionId: "session_1", after: { seq: 1 }, waitMs: 5000 },
        { sessionId: "session_1", after: { seq: 1 }, waitMs: 5000 },
      ]);
    });

    describe("with failing reads", () => {
      beforeEach(() => vi.useFakeTimers());
      afterEach(() => vi.useRealTimers());

      it("repeats a read through a network failure or a proxy error, from the same cursor", async () => {
        const failures: Array<() => unknown> = [
          () => { throw new TypeError("fetch failed"); },
          () => new Response("<html>Bad Gateway</html>", { status: 502 }),
        ];
        const { calls, client } = gateway({
          "session/events/read": () => {
            const failure = failures.shift();
            return failure ? failure() : page([completed(8)]);
          },
          "session/runs/read": () => ({ run: finishedRun }),
        });

        const waiting = client.awaitRun("session_1", "run_1", { after: { seq: 7 } });
        await vi.advanceTimersByTimeAsync(10_000);

        await expect(waiting).resolves.toMatchObject({ cursor: { seq: 8 } });
        const reads = calls.filter((call) => call.method === "session/events/read");
        expect(reads.map((call) => call.params.after)).toEqual([{ seq: 7 }, { seq: 7 }, { seq: 7 }]);
      });

      it("gives up after a bounded number of attempts", async () => {
        const { calls, client } = gateway({
          "session/events/read": () => { throw new TypeError("fetch failed"); },
        });

        const waiting = client.awaitRun("session_1", "run_1");
        const rejected = expect(waiting).rejects.toMatchObject({
          name: "LightspeedTransportError",
          transient: true,
          cause: { name: "TypeError" },
        });
        await vi.advanceTimersByTimeAsync(60_000);

        await rejected;
        expect(calls).toHaveLength(6);
      });

      it("does not repeat a read the server refused", async () => {
        const { calls, client } = gateway({
          "session/events/read": () =>
            jsonResponse({ id: 1, error: { code: -32003, message: "no", data: { kind: "forbidden", message: "no" } } }),
        });

        await expect(client.awaitRun("session_1", "run_1")).rejects.toMatchObject({ kind: "forbidden" });
        expect(calls).toHaveLength(1);
      });
    });

    it("abandons a connection that stalls instead of waiting on it forever", async () => {
      let attempts = 0;
      const fetchImpl = ((input: RequestInfo | URL, init?: RequestInit) => {
        attempts += 1;
        if (attempts === 1) {
          return hangingFetch(input, init);
        }
        const { id } = decodeBody(init) as { id: number };
        const result = attempts === 2 ? page([completed(1)]) : { run: finishedRun };
        return Promise.resolve(jsonResponse({ id, result: { result, notifications: [] } }));
      }) as typeof fetch;
      const shortTimeout = vi.spyOn(AbortSignal, "timeout").mockImplementationOnce(() => {
        const controller = new AbortController();
        setTimeout(() => controller.abort(new DOMException("timed out", "TimeoutError")), 5);
        return controller.signal;
      });
      const client = new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });

      await expect(client.awaitRun("session_1", "run_1")).resolves.toMatchObject({ cursor: { seq: 1 } });

      expect(attempts).toBe(3);
      shortTimeout.mockRestore();
    });

    it("rejects with the caller's own abort reason", async () => {
      const client = new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: hangingFetch });
      const controller = new AbortController();

      const waiting = client.awaitRun("session_1", "run_1", { signal: controller.signal });
      controller.abort();

      await expect(waiting).rejects.toMatchObject({ name: "AbortError" });
    });
  });

  describe("transport failures", () => {
    const client = (fetchImpl: typeof fetch) =>
      new LightspeedClient({ endpoint: "http://lightspeed.local/rpc", fetch: fetchImpl });

    it("rejects with the signal's reason, not a transport error", async () => {
      const controller = new AbortController();
      const call = client(hangingFetch).call("initialize", {}, { signal: controller.signal });
      controller.abort();
      await expect(call).rejects.toMatchObject({ name: "AbortError" });

      const reason = new DOMException("gave up", "TimeoutError");
      const timedOut = new AbortController();
      const timedOutCall = client(hangingFetch).call("initialize", {}, { signal: timedOut.signal });
      timedOut.abort(reason);
      await expect(timedOutCall).rejects.toBe(reason);
    });

    it("keeps the underlying network failure as its cause", async () => {
      const failure = new TypeError("fetch failed");
      const call = client((async () => { throw failure; }) as typeof fetch).call("initialize", {});
      await expect(call).rejects.toMatchObject({
        name: "LightspeedTransportError",
        cause: failure,
        transient: true,
      });
    });

    it("reports a proxy's HTML error page by its status", async () => {
      const call = client((async () => new Response("<html>Bad Gateway</html>", { status: 502 })) as typeof fetch)
        .call("initialize", {});
      await expect(call).rejects.toMatchObject({
        name: "LightspeedTransportError",
        message: "Lightspeed API request failed with HTTP 502",
        status: 502,
        body: "<html>Bad Gateway</html>",
        transient: true,
      });
    });

    it("reports the gateway's 401 as the server's unauthenticated error", async () => {
      // No valid key: the gateway answers before reading the body, so there is no request id.
      const refusal = {
        id: null,
        error: {
          code: -32001,
          message: "request is not authenticated",
          data: { kind: "unauthenticated", message: "request is not authenticated" },
        },
      };
      const call = client((async () => Response.json(refusal, { status: 401 })) as typeof fetch)
        .call("initialize", {});
      await expect(call).rejects.toBeInstanceOf(LightspeedRpcError);
      await expect(call).rejects.toMatchObject({ kind: "unauthenticated", code: -32001 });
    });

    it("reports a 401 that is not the gateway's by its status", async () => {
      const call = client((async () => new Response("<html>Sign in</html>", { status: 401 })) as typeof fetch)
        .call("initialize", {});
      await expect(call).rejects.toMatchObject({
        name: "LightspeedTransportError",
        status: 401,
        transient: false,
      });
    });

    it("points at the endpoint when the URL is wrong", async () => {
      const call = client((async () => new Response("", { status: 404 })) as typeof fetch).call("initialize", {});
      await expect(call).rejects.toMatchObject({
        status: 404,
        message: expect.stringContaining("http://lightspeed.local/rpc is the gateway's /rpc URL"),
        transient: false,
      });
    });

    it("does not treat a malformed success as transient", async () => {
      const call = client((async () => new Response("not json", { status: 200 })) as typeof fetch).call("initialize", {});
      await expect(call).rejects.toMatchObject({
        message: expect.stringContaining("not valid JSON"),
        transient: false,
      });
    });
  });
});
