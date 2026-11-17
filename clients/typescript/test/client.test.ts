import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LightspeedClient, LightspeedApiError, LightspeedTransportError, type SessionEventView } from "../src/index.js";
const jsonResponse = (body: unknown) => Response.json(body);
const problem = (kind: string, status: number) => Response.json({ type: "about:blank", title: kind, detail: kind, kind, status }, { status });
const hangingFetch = ((_input: RequestInfo | URL, init?: RequestInit) => new Promise((_resolve, reject) => {
  const signal = init?.signal;
  if (signal?.aborted) reject(signal.reason);
  signal?.addEventListener("abort", () => reject(signal.reason));
})) as typeof fetch;

describe("HTTP client", () => {
  it("sends bearer and actor headers and returns direct DTOs", async () => {
    const client = new LightspeedClient({ endpoint: "http://runtime/api/v1", universe: "u", token: "secret", actor: "a",
      fetch: async function (url, init) {
        expect(this).toBeUndefined();
        expect(String(url)).toBe("http://runtime/api/v1/universes/u/models");
        expect(init?.method).toBe("GET");
        expect(init?.body).toBeUndefined();
        expect(new Headers(init?.headers).get("authorization")).toBe("Bearer secret");
        expect(new Headers(init?.headers).get("x-lightspeed-actor")).toBe("a");
        expect(new Headers(init?.headers).has("x-lightspeed-universe")).toBe(false);
        return Response.json({ models: [] });
      },
    });
    expect(await client.listModels()).toEqual({ models: [] });
  });
  it("does not retry mutations and preserves explicit submission identifiers", async () => {
    const bodies: unknown[] = [];
    const client = new LightspeedClient({ endpoint: "http://runtime/api/v1", universe: "u", fetch: async (_url, init) => {
      bodies.push(JSON.parse(String(init?.body)));
      throw new TypeError("connection lost");
    } });
    await expect(client.startRun("s", "hello", { submissionId: "stable", config: null })).rejects.toBeInstanceOf(LightspeedTransportError);
    expect(bodies).toEqual([{ source: { type: "input", items: [{ type: "text", text: "hello" }] }, submissionId: "stable", config: null }]);
  });
  it("preserves HTTP problems and proxy statuses", async () => {
    const client = new LightspeedClient({ endpoint: "http://runtime/api/v1", fetch: async () => problem("unauthenticated", 401) });
    await expect(client.initialize()).rejects.toMatchObject({ name: "LightspeedApiError", status: 401, kind: "unauthenticated" });
    const proxy = new LightspeedClient({ endpoint: "http://runtime/api/v1", fetch: async () => new Response("proxy", { status: 502 }) });
    await expect(proxy.initialize()).rejects.toMatchObject({ name: "LightspeedTransportError", status: 502, transient: true });
    const invalid = new LightspeedClient({ endpoint: "http://runtime/api/v1", fetch: async () => new Response("invalid") });
    await expect(invalid.initialize()).rejects.toBeInstanceOf(LightspeedTransportError);
  });
  it("downloads raw bytes and uploads bytes with a verified length", async () => {
    const client = new LightspeedClient({ endpoint: "http://runtime/api/v1", universe: "u", fetch: async (_url, init) => {
      if (init?.method === "GET") return new Response(new Uint8Array([255, 0]));
      expect(init?.body).toEqual(new Uint8Array([1, 2]));
      expect(new Headers(init?.headers).get("content-type")).toBe("application/octet-stream");
      expect(new Headers(init?.headers).get("content-length")).toBe("2");
      return Response.json({ blobRef: "sha256:test", bytes: 2 });
    } });
    expect(await client.readBlobBytes("sha256:test")).toEqual(new Uint8Array([255, 0]));
    await expect(client.readBlobText("sha256:test")).rejects.toBeInstanceOf(TypeError);
    expect(await client.uploadBlob("sha256:test", new Uint8Array([1, 2]))).toEqual({ blobRef: "sha256:test", bytes: 2 });
    const response = await client.openDownload("blobs/read", { blobRef: "sha256:test" });
    expect(new Uint8Array(await response.arrayBuffer())).toEqual(new Uint8Array([255, 0]));
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
        const url = new URL(String(_input));
        const segments = url.pathname.split("/");
        const sessionId = decodeURIComponent(segments[6]!);
        const method = segments[7] === "events" ? "session/events/read" : segments[7] === "runs" ? "session/runs/read" : "session/read";
        const params: Record<string, unknown> = { sessionId };
        if (method === "session/events/read") {
          params.after = url.searchParams.has("after") ? JSON.parse(url.searchParams.get("after")!) : null;
          params.waitMs = Number(url.searchParams.get("waitMs"));
          if (url.searchParams.has("limit")) params.limit = Number(url.searchParams.get("limit"));
        }
        if (method === "session/runs/read") params.runId = decodeURIComponent(segments[8]!);
        calls.push({ method, params });
        const handler = handlers[method];
        if (!handler) {
          throw new Error(`unexpected call to ${method}`);
        }
        const result = handler(params);
        return result instanceof Response
          ? result
          : jsonResponse(result);
      }) as typeof fetch;
      return { calls, client: new LightspeedClient({ endpoint: "http://lightspeed.local/api/v1", universe: "u", fetch: fetchImpl }) };
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
        name: "LightspeedApiError",
        kind: "sessionExecutionStopped",
      });
    });

    it("rejects for a run the session does not have", async () => {
      const { client } = gateway({
        "session/events/read": () => page([], 3),
        "session/read": () => ({ session: { runs: [] } }),
        "session/runs/read": () =>
          problem("notFound", 404),
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
            problem("forbidden", 403),
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
        const result = attempts === 2 ? page([completed(1)]) : { run: finishedRun };
        return Promise.resolve(jsonResponse(result));
      }) as typeof fetch;
      const shortTimeout = vi.spyOn(AbortSignal, "timeout").mockImplementationOnce(() => {
        const controller = new AbortController();
        setTimeout(() => controller.abort(new DOMException("timed out", "TimeoutError")), 5);
        return controller.signal;
      });
      const client = new LightspeedClient({ endpoint: "http://lightspeed.local/api/v1", universe: "u", fetch: fetchImpl });

      await expect(client.awaitRun("session_1", "run_1")).resolves.toMatchObject({ cursor: { seq: 1 } });

      expect(attempts).toBe(3);
      shortTimeout.mockRestore();
    });

    it("rejects with the caller's own abort reason", async () => {
      const client = new LightspeedClient({ endpoint: "http://lightspeed.local/api/v1", universe: "u", fetch: hangingFetch });
      const controller = new AbortController();

      const waiting = client.awaitRun("session_1", "run_1", { signal: controller.signal });
      controller.abort();

      await expect(waiting).rejects.toMatchObject({ name: "AbortError" });
    });
  });

});

it("named helpers preserve full run sources, terminal notifications and backward event pagination", async () => {
  const calls: Array<{ url: URL; body: unknown }> = [];
  const client = new LightspeedClient({ endpoint: "https://runtime.test", universe: "u", fetch: async (url, init) => {
    calls.push({ url: new URL(String(url)), body: init?.body ? JSON.parse(String(init.body)) : undefined });
    return Response.json({});
  } });
  const source = { type: "input", items: [{ type: "text", text: "hello" }] } as const;
  await client.startRun("session", { ...source, items: [...source.items] }, { submissionId: null, notifyOnTerminal: { token: "controller" }, config: null });
  expect(calls[0]!.body).toEqual({ source, submissionId: null, notifyOnTerminal: { token: "controller" }, config: null });
  await client.readEvents("session", { direction: "backward", before: { seq: 42 }, limit: 10 });
  expect(calls[1]!.url.searchParams.get("direction")).toBe("backward");
  expect(JSON.parse(calls[1]!.url.searchParams.get("before")!)).toEqual({ seq: 42 });
  expect(calls[1]!.url.searchParams.get("limit")).toBe("10");
});

it("named binary helpers carry streams and raw workspace bytes", async () => {
  const stream = new ReadableStream<Uint8Array>({ start(controller) { controller.enqueue(new Uint8Array([9, 8])); controller.close(); } });
  const client = new LightspeedClient({ endpoint: "https://runtime.test", universe: "u", fetch: async (_url, init) => {
    if (init?.method === "GET") return new Response(new Uint8Array([255, 0]));
    expect(init?.body).toBe(stream);
    expect(new Headers(init?.headers).get("content-length")).toBe("2");
    expect(new Headers(init?.headers).get("content-type")).toBe("application/octet-stream");
    return Response.json({ blobRef: "digest", bytes: 2 });
  } });
  expect(await client.uploadBlob("digest", stream, { contentLength: 2 })).toEqual({ blobRef: "digest", bytes: 2 });
  expect(await client.readWorkspaceFile("workspace", "file.bin")).toEqual(new Uint8Array([255, 0]));
});
