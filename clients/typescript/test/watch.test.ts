import { describe, expect, it, vi } from "vitest";
import { LightspeedClient, LightspeedRpcError, LightspeedTransportError, readEventStream } from "../src/index.js";

async function collect<T>(stream: AsyncIterable<T>): Promise<T[]> {
  const values: T[] = [];
  for await (const value of stream) values.push(value);
  return values;
}

const encode = (value: string) => new TextEncoder().encode(value);
const reset = { method: "watch/reset", params: {} };
const event = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`;

describe("watch transport", () => {
  it("decodes UTF-8 and CRLF split at every byte, comments and multiline data", async () => {
    const bytes = encode(": hello\r\ndata: hé\r\ndata: 世界\r\n\r\ndata\n\ndata: truncated");
    const stream = new ReadableStream<Uint8Array>({ start(c) {
      for (const byte of bytes) c.enqueue(Uint8Array.of(byte));
      c.close();
    } });
    expect(await collect(readEventStream(stream))).toEqual(["hé\n世界", ""]);
    expect(stream.locked).toBe(false);
  });

  it("cancels and releases the body when iteration stops", async () => {
    const cancel = vi.fn();
    const stream = new ReadableStream<Uint8Array>({ start(c) { c.enqueue(encode(event(reset))); }, cancel });
    for await (const data of readEventStream(stream)) { expect(JSON.parse(data)).toEqual(reset); break; }
    expect(cancel).toHaveBeenCalledOnce();
    expect(stream.locked).toBe(false);
  });

  it("bounds stalled and oversized streams", async () => {
    const cancel = vi.fn();
    const idle = new ReadableStream<Uint8Array>({ cancel });
    await expect(readEventStream(idle, 5).next()).rejects.toMatchObject({ transient: true });
    expect(cancel).toHaveBeenCalledOnce();
    const huge = new ReadableStream<Uint8Array>({ start(c) { c.enqueue(encode("data: " + "x".repeat(2 * 1024 * 1024))); } });
    await expect(readEventStream(huge).next()).rejects.toBeInstanceOf(LightspeedTransportError);
  });

  it("uses POST authentication and yields workspace changes before the final response", async () => {
    const changed = { method: "workspaces/removed", params: { workspaceId: "w" } };
    const fetch = vi.fn(async (_url: unknown, init?: RequestInit) => {
      expect(init?.method).toBe("POST");
      const headers = new Headers(init?.headers);
      expect(headers.get("accept")).toBe("text/event-stream");
      expect(headers.get("authorization")).toBe("Bearer secret");
      expect(headers.get("x-lightspeed-universe")).toBe("u");
      const call = JSON.parse(String(init?.body));
      expect(call.params).toEqual({ kinds: ["workspace"] });
      return new Response(event(reset) + event(changed) + event({ id: call.id, result: { result: {} } }), {
        headers: { "content-type": "text/event-stream" },
      });
    });
    const client = new LightspeedClient({ endpoint: "https://core/rpc", apiKey: "secret", universe: "u", fetch });
    expect(await collect(client.watch({ kinds: ["workspace"] }, { reconnect: false }))).toEqual([reset, changed]);
    expect(fetch).toHaveBeenCalledOnce();
  });

  it("does not reconnect an authorization refusal or malformed event", async () => {
    const fetch = vi.fn(async () => Response.json({ error: { code: -32003, message: "forbidden" } }));
    const client = new LightspeedClient({ endpoint: "https://core/rpc", fetch });
    await expect(client.watch().next()).rejects.toBeInstanceOf(LightspeedRpcError);
    expect(fetch).toHaveBeenCalledOnce();
    fetch.mockImplementation(async () => new Response("data: {broken}\n\n", { headers: { "content-type": "text/event-stream" } }));
    await expect(client.watch().next()).rejects.toBeInstanceOf(LightspeedTransportError);
  });

  it("reconnects after completion and stops during backoff on abort", async () => {
    const fetch = vi.fn(async () => new Response(event(reset), { headers: { "content-type": "text/event-stream" } }));
    const client = new LightspeedClient({ endpoint: "https://core/rpc", fetch });
    const controller = new AbortController();
    const stream = client.watch({}, { signal: controller.signal });
    expect((await stream.next()).value).toEqual(reset);
    expect((await stream.next()).value).toEqual(reset);
    expect(fetch).toHaveBeenCalledTimes(2);
    const next = stream.next();
    controller.abort();
    expect((await next).done).toBe(true);
  });
});
