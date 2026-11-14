import { expect, it, vi } from "vitest";
import type { LightspeedClient } from "@lightspeed-ai/sdk";
import { proxyWatch } from "./watch.js";

function upstream() {
  let signal!: AbortSignal;
  const cancel = vi.fn();
  const body = new ReadableStream<Uint8Array>({ start(c) { c.enqueue(new TextEncoder().encode("data: reset\n\n")); }, cancel });
  const openWatch = vi.fn(async (_params, options) => {
    signal = options.signal;
    signal.addEventListener("abort", () => void body.cancel().catch(() => {}));
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  });
  return { client: { openWatch } as unknown as LightspeedClient, cancel, openWatch, signal: () => signal, body };
}

it("passes SSE bytes and cancels the upstream when the browser disconnects", async () => {
  const core = upstream();
  const response = await proxyWatch(core.client, new AbortController().signal);
  expect(response.headers.get("cache-control")).toBe("no-store");
  const reader = response.body!.getReader();
  expect(new TextDecoder().decode((await reader.read()).value)).toBe("data: reset\n\n");
  await reader.cancel();
  expect(core.signal().aborted).toBe(true);
  expect(core.cancel).toHaveBeenCalledOnce();
  expect(core.body.locked).toBe(false);
});

it("bounds upstream admission and forwards browser aborts", async () => {
  const controller = new AbortController();
  const openWatch = vi.fn(async (_params, options) => new Promise<Response>((_resolve, reject) => {
    options.signal.addEventListener("abort", () => reject(options.signal.reason));
  }));
  const client = { openWatch } as unknown as LightspeedClient;
  const expired = proxyWatch(client, controller.signal, 5);
  await expect(expired).rejects.toBeDefined();
  const disconnected = proxyWatch(client, controller.signal);
  controller.abort();
  await expect(disconnected).rejects.toBeDefined();
});
