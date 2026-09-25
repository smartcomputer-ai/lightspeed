// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionEvent } from "@/api";
import { useSessionTail, type SessionTail } from "./tail";

interface Request {
  url: URL;
  signal: AbortSignal;
  reply: (body: unknown, status?: number) => void;
  reject: (error: Error) => void;
}
let requests: Request[];
let root: Root;
let container: HTMLDivElement;
let tail: SessionTail;
function Harness({ id = "session" }: { id?: string }) {
  tail = useSessionTail("universe", id);
  return <div>{tail.transcript.entries.map((entry) => entry.key).join(",")}</div>;
}
function message(seq: number): SessionEvent {
  return {
    sessionId: "session", cursor: { seq }, observedAtMs: seq, joins: {},
    kind: { type: "contextEntriesApplied", baseRevision: seq - 1, revision: seq, entries: [{
      id: `message-${seq}`, kind: { type: "message", role: "assistant" },
      content: { contentRef: `sha256:${seq}` }, text: `Message ${seq}`,
    }] },
  };
}
function history(seqs: number[], head: number, before?: number) {
  return { events: seqs.map(message), headCursor: { seq: head },
    complete: before === undefined, nextCursor: before === undefined ? null : { seq: before } };
}
async function reply(index: number, body: unknown, status = 200) {
  await act(async () => requests[index]!.reply(body, status));
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  requests = [];
  vi.stubGlobal("fetch", vi.fn((url: string, options: RequestInit) => new Promise<Response>((resolve, reject) => {
    const signal = options.signal as AbortSignal;
    signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
    requests.push({ url: new URL(url, "http://localhost"), signal, reject,
      reply: (body, status = 200) => resolve(new Response(JSON.stringify(body), { status })) });
  })));
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("recent transcript and live history", () => {
  it("opens at the latest window and merges concurrent older/live pages without moving the live cursor", async () => {
    await act(async () => root.render(<Harness />));
    expect(requests).toHaveLength(1);
    expect(requests[0]!.url.pathname).toMatch(/\/events$/);
    expect(requests[0]!.url.searchParams.get("direction")).toBe("backward");
    expect(requests[0]!.url.searchParams.has("before")).toBe(false);
    await reply(0, history([11999, 12000], 12000, 11999));
    expect(tail.phase).toBe("live");
    expect(tail.hasOlder).toBe(true);
    expect(requests).toHaveLength(2);
    expect(requests[1]!.url.searchParams.get("after")).toBe("12000");
    await act(async () => { tail.loadOlder(); tail.loadOlder(); });
    expect(requests).toHaveLength(3);
    expect(requests[2]!.url.searchParams.get("before")).toBe("11999");
    await reply(1, { events: [message(12001)], nextCursor: { seq: 12001 }, complete: true });
    expect(requests[3]!.url.searchParams.get("after")).toBe("12001");
    await reply(2, history([11998], 13000, 11998));
    expect(tail.transcript.entries.map((entry) => entry.key)).toEqual([
      "message-11998", "message-11999", "message-12000", "message-12001",
    ]);
    expect(tail.historyRevision).toBe(1);
    expect(tail.loadingOlder).toBe(false);
    await reply(3, { events: [message(12002)], nextCursor: { seq: 12002 }, complete: true });
    expect(requests[4]!.url.searchParams.get("after")).toBe("12002");
  });

  it("follows an initially empty session from zero", async () => {
    await act(async () => root.render(<Harness />));
    await reply(0, history([], 0));
    expect(tail.hasOlder).toBe(false);
    expect(requests[1]!.url.searchParams.get("after")).toBe("0");
    await reply(1, { events: [message(1)], nextCursor: { seq: 1 }, complete: true });
    expect(tail.transcript.entries[0]!.key).toBe("message-1");
  });

  it.each(["network", "gateway"])("recovers one %s failure immediately without flashing a disconnect", async (failure) => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    expect(requests[1]!.url.searchParams.get("waitMs")).toBe("25000");
    if (failure === "network") {
      await act(async () => requests[1]!.reject(new TypeError("Failed to fetch")));
    } else {
      await reply(1, { error: "upstream unavailable" }, 502);
    }
    expect(tail.error).toBeNull();
    expect(requests).toHaveLength(3);
    expect(requests[2]!.url.searchParams.get("after")).toBe("10");
    expect(requests[2]!.url.searchParams.get("waitMs")).toBe("0");
    expect(requests[2]!.url.searchParams.get("limit")).toBe("1");

    // Even an idle session proves it has reconnected without waiting 25s.
    await reply(2, { events: [], complete: true });
    expect(tail.error).toBeNull();
    expect(requests).toHaveLength(4);
    expect(requests[3]!.url.searchParams.get("waitMs")).toBe("25000");
    expect(requests[3]!.url.searchParams.get("limit")).toBe("500");
    await reply(3, { events: [message(11)], complete: true });
    expect(requests[4]!.url.searchParams.get("after")).toBe("11");
    expect(tail.transcript.entries.map((entry) => entry.key)).toEqual(["message-10", "message-11"]);
  });

  it("reports a failed reconnect, paces retries, and clears the warning on an empty probe", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    await act(async () => requests[1]!.reject(new TypeError("Failed to fetch")));
    await act(async () => requests[2]!.reject(new TypeError("Failed to fetch")));
    expect(tail.error).toBe("Failed to fetch");
    expect(tail.errorIsTransient).toBe(true);
    expect(requests).toHaveLength(3);
    await act(async () => vi.advanceTimersByTime(2999));
    expect(requests).toHaveLength(3);
    await act(async () => vi.advanceTimersByTime(1));
    expect(requests[3]!.url.searchParams.get("after")).toBe("10");
    expect(requests[3]!.url.searchParams.get("waitMs")).toBe("0");
    await reply(3, { events: [], complete: true });
    expect(tail.error).toBeNull();
    expect(requests[4]!.url.searchParams.get("waitMs")).toBe("25000");
  });

  it("bounds stalled polls and reconnect probes instead of hanging indefinitely", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    await act(async () => vi.advanceTimersByTime(34_999));
    expect(requests).toHaveLength(2);
    await act(async () => vi.advanceTimersByTime(1));
    expect(requests[1]!.signal.aborted).toBe(true);
    expect(requests[2]!.url.searchParams.get("waitMs")).toBe("0");
    expect(tail.error).toBeNull();
    await act(async () => vi.advanceTimersByTime(10_000));
    expect(requests[2]!.signal.aborted).toBe(true);
    expect(tail.error).toContain("timed out");
    await act(async () => vi.advanceTimersByTime(3000));
    expect(requests[3]!.url.searchParams.get("after")).toBe("10");
    await reply(3, { events: [message(11)], complete: true });
    expect(tail.error).toBeNull();
    expect(tail.transcript.entries).toHaveLength(2);
  });

  it("reports authorization errors immediately instead of treating them as connection churn", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    await reply(1, { error: "unauthorized" }, 401);
    expect(tail.error).toContain("401");
    expect(tail.errorIsTransient).toBe(false);
    expect(requests).toHaveLength(2);
  });

  it("aborts a reconnect and clears its deadline when navigating away", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    await act(async () => requests[1]!.reject(new TypeError("Failed to fetch")));
    await act(async () => root.render(<Harness id="other" />));
    expect(requests[2]!.signal.aborted).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    expect(tail.error).toBeNull();
    expect(tail.phase).toBe("loading");
    await reply(3, history([1], 1));
    expect(tail.transcript.entries.map((entry) => entry.key)).toEqual(["message-1"]);
  });

  it("retries older history automatically while live events keep arriving and stops at the beginning", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([3], 3, 3));
    await act(async () => tail.loadOlder());
    await reply(2, { error: "offline" }, 503);
    expect(tail.historyError).toContain("503");
    expect(tail.loadingOlder).toBe(true);
    await reply(1, { events: [message(4)], complete: true });
    await act(async () => vi.advanceTimersByTime(3000));
    expect(requests[4]!.url.searchParams.get("before")).toBe("3");
    await reply(4, history([1, 2], 4));
    expect(tail.historyError).toBeNull();
    expect(tail.hasOlder).toBe(false);
    expect(tail.transcript.entries).toHaveLength(4);
    const count = requests.length;
    await act(async () => tail.loadOlder());
    expect(requests).toHaveLength(count);
  });

  it("aborts both directions on a session switch and rejects late responses", async () => {
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    await act(async () => tail.loadOlder());
    await act(async () => root.render(<Harness id="other" />));
    expect(requests[1]!.signal.aborted).toBe(true);
    expect(requests[2]!.signal.aborted).toBe(true);
    expect(tail.transcript.entries).toEqual([]);
    await reply(2, history([9], 10, 9));
    expect(tail.transcript.entries).toEqual([]);
    await reply(3, history([1], 1));
    expect(tail.transcript.entries.map((entry) => entry.key)).toEqual(["message-1"]);
  });

  it("never advances past a gap in the live stream", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, history([10], 10, 10));
    await reply(1, { events: [message(12)], nextCursor: { seq: 12 }, headCursor: { seq: 50 } });
    expect(tail.error).toContain("not contiguous");
    await act(async () => vi.advanceTimersByTime(3000));
    expect(requests[2]!.url.searchParams.get("after")).toBe("10");
    expect(tail.transcript.entries).toHaveLength(1);
  });

  it("rejects an old server's forward prefix instead of skipping recent history", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Harness />));
    await reply(0, { events: [message(1)], nextCursor: { seq: 1 }, headCursor: { seq: 12000 }, complete: false });
    expect(tail.phase).toBe("loading");
    expect(tail.transcript.entries).toEqual([]);
    expect(tail.error).toContain("requested recent history window");
    await act(async () => vi.advanceTimersByTime(3000));
    expect(requests[1]!.url.searchParams.get("direction")).toBe("backward");
    expect(requests[1]!.url.searchParams.has("after")).toBe(false);
  });
});
