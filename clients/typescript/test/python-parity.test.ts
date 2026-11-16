/// <reference types="node" />
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { LightspeedClient, LightspeedApiError, prepareHttpRequest, type OperationId } from "../src/index.js";

const fixture = JSON.parse(readFileSync(new URL("../../fixtures/gateway-parity.json", import.meta.url), "utf8")) as {
  calls: Array<{ method: OperationId; verb: string; path: string; body: unknown; params: Record<string, unknown>; headers?: Record<string, string>; expectHeaders: Record<string, string | null>; status: number; response: unknown }>;
  wait: { sessionId: string; runId: string; after: { seq: number }; requests: Array<{ method: OperationId; params: unknown; result: unknown }>; observedSequences: number[]; finalCursor: { seq: number } };
};
describe("Python HTTP parity fixtures", () => {
  it.each(fixture.calls)("matches the wire for $method", async (item) => {
    const client = new LightspeedClient({ endpoint: "http://localhost/api/v1", token: "option", actor: "actor", universe: "fixture-universe",
      headers: { Authorization: "Bearer configured" }, fetch: async (url, init) => {
        expect(String(url)).toBe("http://localhost" + item.path);
        expect(init?.method).toBe(item.verb);
        expect(init?.body === undefined ? null : JSON.parse(String(init.body))).toEqual(item.body);
        for (const [name, value] of Object.entries(item.expectHeaders)) expect(new Headers(init?.headers).get(name)).toBe(value);
        return Response.json(item.response, { status: item.status });
      },
    });
    const result = client.call(item.method, item.params as never, item.headers ? { headers: item.headers } : {});
    if (item.status >= 400) {
      await expect(result).rejects.toBeInstanceOf(LightspeedApiError);
      await expect(result).rejects.toMatchObject({ payload: item.response });
    } else await expect(result).resolves.toEqual(item.response);
  });
  it("waits from the saved cursor and only emits the selected run's events", async () => {
    const item = fixture.wait;
    const observed: number[] = [];
    let index = 0;
    const client = new LightspeedClient({ endpoint: "http://localhost/api/v1", universe: "u", fetch: async (url, init) => {
      const expected = item.requests[index++]!;
      const prepared = prepareHttpRequest(expected.method, expected.params as never, { endpoint: "http://localhost/api/v1", universe: "u" });
      expect(String(url)).toBe(prepared.url.toString());
      expect(init?.method).toBe(prepared.method);
      return Response.json(expected.result);
    } });
    const result = await client.awaitRun(item.sessionId, item.runId, { after: item.after, waitMs: 1000, onEvent: event => { observed.push(event.cursor.seq); } });
    expect(index).toBe(item.requests.length);
    expect(observed).toEqual(item.observedSequences);
    expect(result).toEqual({ ...(item.requests.at(-1)?.result as object), cursor: item.finalCursor });
  });
});
