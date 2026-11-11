/// <reference types="node" />

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { LightspeedClient, LightspeedRpcError, type Method } from "../src/index.js";

const fixture = JSON.parse(readFileSync(
  new URL("../../fixtures/gateway-parity.json", import.meta.url), "utf8",
)) as {
  calls: Array<{
    method: Method;
    params: Record<string, unknown>;
    headers?: Record<string, string>;
    expectHeaders: Record<string, string | null>;
    response: { result?: unknown; error?: { code: number; message: string; data: { kind: string } } };
  }>;
  wait: {
    sessionId: string;
    runId: string;
    after: { seq: number };
    requests: Array<{ method: string; params: unknown; result: unknown }>;
    observedSequences: number[];
    finalCursor: { seq: number };
  };
};

describe("Python gateway parity fixtures", () => {
  it.each(fixture.calls)("matches the wire for $method", async (item) => {
    const client = new LightspeedClient({
      endpoint: "http://localhost/rpc", apiKey: "option", actor: "actor", universe: "option",
      headers: { Authorization: "Bearer configured", "x-lightspeed-universe": "default-universe" },
      fetch: async (_url, init) => {
        expect(JSON.parse(init?.body as string)).toEqual({ id: 1, method: item.method, params: item.params });
        const headers = new Headers(init?.headers);
        for (const [name, value] of Object.entries(item.expectHeaders)) {
          expect(headers.get(name)).toBe(value);
        }
        return Response.json({ id: 1, ...item.response });
      },
    });
    const result = client.call(item.method, item.params as never, item.headers ? { headers: item.headers } : {});
    if (item.response.error) {
      await expect(result).rejects.toBeInstanceOf(LightspeedRpcError);
      await expect(result).rejects.toMatchObject({ ...item.response.error, kind: item.response.error.data.kind });
    } else {
      await expect(result).resolves.toEqual(item.response.result);
    }
  });

  it("waits from the saved cursor and only emits the selected run's events", async () => {
    const item = fixture.wait;
    const observed: number[] = [];
    let index = 0;
    const client = new LightspeedClient({
      endpoint: "http://localhost/rpc",
      fetch: async (_url, init) => {
        const expected = item.requests[index++];
        expect(expected).toBeDefined();
        expect(JSON.parse(init?.body as string)).toEqual({ id: index, method: expected?.method, params: expected?.params });
        return Response.json({ id: index, result: { result: expected?.result, notifications: [] } });
      },
    });
    const result = await client.awaitRun(item.sessionId, item.runId, {
      after: item.after, waitMs: 1000, onEvent: (event) => { observed.push(event.cursor.seq); },
    });
    expect(index).toBe(item.requests.length);
    expect(observed).toEqual(item.observedSequences);
    expect(result).toEqual({ ...(item.requests.at(-1)?.result as object), cursor: item.finalCursor });
  });
});
