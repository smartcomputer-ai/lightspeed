/// Routes every same-origin `/api/*` and `/ingress/*` request into the
/// in-browser router. Installed before the app loads, so `api()`, the event
/// tail, webhook test-fires against the public ingest paths, and the
/// better-auth client all land here unchanged.
import type { Hono } from "hono";

export function installDemoFetch(app: Hono): void {
  const nativeFetch = globalThis.fetch.bind(globalThis);
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(input, init);
    const url = new URL(request.url);
    if (
      url.origin === window.location.origin &&
      (url.pathname.startsWith("/api/") || url.pathname.startsWith("/ingress/"))
    ) {
      return await app.fetch(request);
    }
    return nativeFetch(request);
  };
}
