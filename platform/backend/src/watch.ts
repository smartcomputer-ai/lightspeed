import type { LightspeedClient } from "@lightspeed-ai/sdk";

/** One upstream per browser, with backpressure and cancellation in both directions.
 * Rotate every minute so cookie admission, membership and feature gates run again.
 */
export async function proxyWatch(client: LightspeedClient, downstream: AbortSignal, lifetimeMs = 60_000): Promise<Response> {
  const controller = new AbortController();
  const abort = () => controller.abort();
  downstream.addEventListener("abort", abort, { once: true });
  if (downstream.aborted) abort();
  const timer = setTimeout(abort, lifetimeMs);
  const cleanup = () => {
    clearTimeout(timer);
    downstream.removeEventListener("abort", abort);
  };
  try {
    const response = await client.openWatch({}, { signal: controller.signal });
    const reader = response.body!.getReader();
    const body = new ReadableStream<Uint8Array>({
      async pull(output) {
        try {
          const next = await reader.read();
          if (next.done) { cleanup(); reader.releaseLock(); output.close(); }
          else output.enqueue(next.value);
        } catch (error) {
          cleanup(); reader.releaseLock();
          if (controller.signal.aborted) output.close();
          else { abort(); output.error(error); }
        }
      },
      async cancel() {
        abort(); cleanup();
        await reader.cancel().catch(() => {});
      },
    });
    return new Response(body, { headers: {
      "content-type": "text/event-stream", "cache-control": "no-store", "x-accel-buffering": "no",
    } });
  } catch (error) { abort(); cleanup(); throw error; }
}
