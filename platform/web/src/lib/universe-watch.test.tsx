// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { UniverseWatchProvider, useWatchConnected } from "./universe-watch";
import { waitForUi } from "@/test/wait-for-ui";

vi.mock("./universes", () => ({ useActiveUniverse: () => ({ universe: { id: "u", role: "admin", features: {} } }) }));

it("shares one stream between consumers and falls back when it disconnects", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  let output!: ReadableStreamDefaultController<Uint8Array>;
  let signal!: AbortSignal;
  const cancel = vi.fn();
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    signal = init.signal!;
    return new Response(new ReadableStream<Uint8Array>({ start(c) { output = c; }, cancel }), { headers: { "content-type": "text/event-stream" } });
  });
  vi.stubGlobal("fetch", fetch);
  const client = new QueryClient();
  const container = document.createElement("div");
  const root = createRoot(container);
  const Consumer = () => <span>{useWatchConnected("u") ? "live" : "poll"}</span>;
  try {
    await act(async () => root.render(<QueryClientProvider client={client}><UniverseWatchProvider><Consumer /><Consumer /></UniverseWatchProvider></QueryClientProvider>));
    expect(container.textContent).toBe("pollpoll");
    await act(async () => { output.enqueue(new TextEncoder().encode('data: {"method":"watch/reset","params":{}}\n\n')); });
    await waitForUi(() => expect(container.textContent).toBe("livelive"));
    expect(fetch).toHaveBeenCalledOnce();
    await act(async () => output.close());
    await waitForUi(() => expect(container.textContent).toBe("pollpoll"));
    await act(async () => root.unmount());
    expect(signal.aborted).toBe(true);
  } finally {
    client.clear(); vi.unstubAllGlobals();
  }
});
