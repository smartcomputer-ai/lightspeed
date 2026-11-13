import { LightspeedTransportError } from "./errors.js";

/** Decode SSE data fields, including chunked UTF-8, CRLF and multiline data.
 * Cancelling iteration cancels the response body and releases its connection.
 */
export async function* readEventStream(body: ReadableStream<Uint8Array>, idleTimeoutMs = 45_000): AsyncGenerator<string> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  let data: string[] = [];
  let size = 0;
  let swallowLf = false;
  try {
    while (true) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const { done, value } = await Promise.race([
        reader.read(),
        new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new LightspeedTransportError(
          "Watch connection stopped responding", { cause: new Error("heartbeat timeout") },
        )), idleTimeoutMs); }),
      ]).finally(() => clearTimeout(timer));
      if (done) return;
      pending += decoder.decode(value, { stream: true });
      if (swallowLf && pending.length) {
        if (pending[0] === "\n") pending = pending.slice(1);
        swallowLf = false;
      }
      let end: number;
      while ((end = pending.search(/[\r\n]/)) >= 0) {
        const line = pending.slice(0, end);
        const cr = pending[end] === "\r";
        pending = pending.slice(end + 1);
        if (cr) {
          if (pending[0] === "\n") pending = pending.slice(1);
          else if (!pending.length) swallowLf = true;
        }
        if (!line) {
          if (data.length) yield data.join("\n");
          data = []; size = 0;
        } else if (line === "data" || line.startsWith("data:")) {
          const field = line === "data" ? "" : line.slice(5).replace(/^ /, "");
          data.push(field); size += field.length;
        }
        if (size > 2 * 1024 * 1024) throw new LightspeedTransportError("Watch event exceeded its size limit");
      }
      if (pending.length + size > 2 * 1024 * 1024) throw new LightspeedTransportError("Watch event exceeded its size limit");
    }
  } catch (error) {
    if (error instanceof LightspeedTransportError) throw error;
    throw new LightspeedTransportError("Watch stream interrupted", { cause: error });
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
