import { describe, expect, it, vi } from "vitest";
import {
  ForgeClient,
  ForgeRpcError,
  type EventCursor,
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

describe("ForgeClient", () => {
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
              items: [],
              runs: [],
            },
          },
          notifications: [],
        },
      });
    }) as unknown as typeof fetch;

    const client = new ForgeClient({
      endpoint: "http://127.0.0.1:18080/rpc",
      fetch: fetchImpl,
      requestId: () => 42,
      headers: { authorization: "Bearer local" },
    });

    const result = await client.call("session/start", {
      sessionId: "session_1",
      cwd: null,
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
        cwd: null,
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
          data: { submissionId: "sub_1" },
        },
      }),
    ) as unknown as typeof fetch;
    const client = new ForgeClient({
      endpoint: "http://forge.local/rpc",
      fetch: fetchImpl,
      requestId: () => "req_1",
    });

    await expect(
      client.call("run/start", {
        sessionId: "session_1",
        input: [{ type: "text", text: "hello" }],
        submissionId: "sub_1",
      }),
    ).rejects.toMatchObject({
      name: "ForgeRpcError",
      code: -32009,
      kind: "conflict",
      data: { submissionId: "sub_1" },
    } satisfies Partial<ForgeRpcError>);
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
              input: [],
              items: [],
              toolBatches: [],
            },
          },
          notifications: [],
        },
      });
    }) as unknown as typeof fetch;
    const client = new ForgeClient({ endpoint: "http://forge.local/rpc", fetch: fetchImpl });

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
      submissionId: "sub_fixed",
      config: null,
    });
  });

  it("awaitRun resumes from cursors until the requested run reaches a terminal event", async () => {
    const requests: Record<string, unknown>[] = [];
    const terminalEvent: SessionEventView = {
      cursor: { seq: 3 },
      sessionId: "session_1",
      observedAtMs: 1000,
      joins: { runId: "run_1" },
      kind: {
        type: "runCompleted",
        runId: "run_1",
        outputRef: "blob_output",
      },
    };
    const pages = [
      {
        result: {
          events: [],
          headCursor: { seq: 2 } satisfies EventCursor,
          nextCursor: null,
          complete: true,
          gap: null,
        },
        notifications: [],
      },
      {
        result: {
          events: [
            {
              cursor: { seq: 2 },
              sessionId: "session_1",
              observedAtMs: 900,
              joins: { runId: "other_run" },
              kind: { type: "runCompleted", runId: "other_run", outputRef: null },
            } satisfies SessionEventView,
            terminalEvent,
          ],
          headCursor: { seq: 3 },
          nextCursor: null,
          complete: true,
          gap: null,
        },
        notifications: [],
      },
    ];
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requests.push(decodeBody(init));
      const page = pages.shift();
      expect(page).toBeDefined();
      return jsonResponse({ id: requests.length, result: page });
    }) as unknown as typeof fetch;
    const client = new ForgeClient({ endpoint: "http://forge.local/rpc", fetch: fetchImpl });
    const heartbeat = vi.fn();
    const onEvent = vi.fn();

    const result = await client.awaitRun("session_1", "run_1", {
      waitMs: 1234,
      heartbeat,
      onEvent,
    });

    expect(result.state).toMatchObject({
      status: "completed",
      outputRef: "blob_output",
    });
    expect(result.cursor).toEqual({ seq: 3 });
    expect(requests.map((request) => request.params)).toEqual([
      {
        sessionId: "session_1",
        after: null,
        waitMs: 1234,
      },
      {
        sessionId: "session_1",
        after: { seq: 2 },
        waitMs: 1234,
      },
    ]);
    expect(heartbeat).toHaveBeenCalledTimes(2);
    expect(heartbeat.mock.calls.map((call) => call[0])).toEqual([{ seq: 2 }, { seq: 3 }]);
    expect(onEvent).toHaveBeenCalledWith(terminalEvent);
  });
});
