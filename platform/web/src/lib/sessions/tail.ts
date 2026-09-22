import { useCallback, useEffect, useRef, useState } from "react";
import { withReadMetadata, type ReadMetadata, type SessionEventsPage, type SessionRunView } from "@/api";
import { emptyTranscript, type TranscriptState } from "./transcript";
import { TranscriptWindow } from "./transcript-window";
import { isTransientReadError } from "@/lib/read-errors";

export interface SessionTail {
  privilegedRead?: boolean;
  transcript: TranscriptState;
  phase: "loading" | "live";
  error: string | null;
  errorIsTransient: boolean;
  hasOlder: boolean;
  loadingOlder: boolean;
  historyError: string | null;
  historyErrorIsTransient: boolean;
  historyRevision: number;
  loadOlder: () => void;
  reconcileRuns: (runs: SessionRunView[]) => void;
}

const PAGE_LIMIT = 500;
const WAIT_MS = 25_000;
const REQUEST_GRACE_MS = 10_000;
const FAST_EMPTY_MS = 1_500;
const PACING_SLEEP_MS = 2_000;
const RETRY_SLEEP_MS = 3_000;

type TailState = Omit<SessionTail, "loadOlder" | "reconcileRuns"> & { scope?: string };
const initialState = (): TailState => ({
  transcript: emptyTranscript(), phase: "loading", error: null, errorIsTransient: false,
  hasOlder: false, loadingOlder: false, historyError: null, historyErrorIsTransient: false, historyRevision: 0,
});

/** The first history page fences the live cursor. Older-page requests and
 * forward long-polls then run independently, sharing only a synchronous merge.
 * No event cap or jump-to-head can discard retained history.
 */
export function useSessionTail(universeId: string, sessionId: string): SessionTail {
  const scope = JSON.stringify([universeId, sessionId]);
  const [tail, setTail] = useState<TailState>(initialState);
  const reconcileRef = useRef<(runs: SessionRunView[]) => void>(() => undefined);
  const olderRef = useRef<() => void>(() => undefined);
  const reconcile = useCallback((runs: SessionRunView[]) => reconcileRef.current(runs), []);
  const loadOlder = useCallback(() => olderRef.current(), []);

  useEffect(() => {
    const abort = new AbortController();
    const signal = abort.signal;
    const window = new TranscriptWindow();
    let cursor = 0;
    let before: number | null = null;
    let live = false;
    let loadingOlder = false;
    let historyRevision = 0;
    setTail({ ...initialState(), scope });
    let privilegedRead = false;
    const observeRead = (response: ReadMetadata) => {
      if (response.privilegedRead && !privilegedRead && !signal.aborted) { privilegedRead = true; push({ privilegedRead }); }
    };
    const push = (patch: Partial<TailState>) => {
      if (!signal.aborted) setTail((prev) => ({ ...prev, ...patch }));
    };
    reconcileRef.current = (runs) => {
      if (signal.aborted || !live) return;
      const previous = window.state;
      window.reconcile(runs);
      if (window.state !== previous) push({ transcript: window.state });
    };

    olderRef.current = () => {
      if (!live || loadingOlder || before === null || signal.aborted) return;
      loadingOlder = true;
      const requestedBefore = before;
      push({ loadingOlder: true, historyError: null });
      void (async () => {
        let retry = RETRY_SLEEP_MS;
        while (!signal.aborted) {
          try {
            const response = await fetchHistory(universeId, sessionId, requestedBefore, signal);
            if (signal.aborted) return;
            observeRead(response);
            if (!response.complete && (!response.nextCursor || response.nextCursor.seq >= requestedBefore)) {
              throw new Error("History cursor did not advance");
            }
            window.prepend(response.events ?? []);
            before = !response.complete ? response.nextCursor!.seq : null;
            loadingOlder = false;
            push({ transcript: window.state, hasOlder: before !== null,
              loadingOlder: false, historyError: null, historyRevision: ++historyRevision });
            return;
          } catch (error) {
            if (signal.aborted) return;
            push({ historyError: errorText(error), historyErrorIsTransient: isTransientReadError(error) });
            await sleep(retry, signal);
            retry = Math.min(retry * 2, 30_000);
          }
        }
      })();
    };

    void (async () => {
      while (!signal.aborted && !live) {
        try {
          const response = await fetchHistory(universeId, sessionId, null, signal);
          if (signal.aborted) return;
          observeRead(response);
          if (!response.complete && !response.nextCursor) throw new Error("Missing history cursor");
          window.append(response.events ?? []);
          // Only the INITIAL history fence can initialize live consumption.
          // Older requests must never move the independently advancing cursor.
          if (!response.headCursor) throw new Error("Missing history head cursor");
          cursor = response.headCursor.seq;
          before = !response.complete ? response.nextCursor!.seq : null;
          live = true;
          push({ transcript: window.state, phase: "live", error: null, hasOlder: before !== null });
        } catch (error) {
          if (signal.aborted) return;
          push({ error: errorText(error), errorIsTransient: isTransientReadError(error) });
          await sleep(RETRY_SLEEP_MS, signal);
        }
      }
      let recovering = false;
      while (!signal.aborted && !window.state.closed) {
        const startedAt = Date.now();
        try {
          // A reconnect probes immediately. Waiting for another long-poll to
          // finish would leave a recovered, idle session looking disconnected.
          const waitMs = recovering ? 0 : WAIT_MS;
          const response = await fetchEvents(universeId, sessionId, cursor, waitMs, signal);
          observeRead(response);
          if (signal.aborted) return;
          if (response.gap) throw new Error("The session event stream contains an unavailable interval");
          const events = (response.events ?? []).filter((event) => event.cursor.seq > cursor);
          if (events.length) {
            if (events.some((event, index) => event.cursor.seq !== cursor + index + 1)) {
              throw new Error("The session event stream is not contiguous");
            }
            window.append(events);
            cursor = events.at(-1)!.cursor.seq;
            push({ transcript: window.state, error: null });
          } else {
            push({ error: null });
            if (waitMs > 0 && Date.now() - startedAt < FAST_EMPTY_MS) await sleep(PACING_SLEEP_MS, signal);
          }
          recovering = false;
        } catch (error) {
          if (signal.aborted) return;
          const retryImmediately = !recovering && isTransientReadError(error);
          recovering = true;
          // One dropped connection is recoverable without interrupting the
          // transcript. Failed probes still report an outage and are paced.
          if (!retryImmediately) {
            push({ error: errorText(error), errorIsTransient: isTransientReadError(error) });
            await sleep(RETRY_SLEEP_MS, signal);
          }
        }
      }
    })();
    return () => {
      abort.abort();
      reconcileRef.current = () => undefined;
      olderRef.current = () => undefined;
    };
  }, [universeId, sessionId, scope]);

  return { ...(tail.scope === scope ? tail : initialState()), reconcileRuns: reconcile, loadOlder };
}

function baseUrl(universeId: string, sessionId: string) {
  return `/api/v1/universes/${encodeURIComponent(universeId)}/sessions/${encodeURIComponent(sessionId)}`;
}

async function fetchHistory(universeId: string, sessionId: string, before: number | null, signal: AbortSignal): Promise<SessionEventsPage & ReadMetadata> {
  const params = new URLSearchParams({ direction: "backward", limit: String(PAGE_LIMIT) });
  if (before !== null) params.set("before", String(before));
  const page = await readPage<SessionEventsPage>(`${baseUrl(universeId, sessionId)}/events?${params}`, signal);
  const head = page.headCursor?.seq;
  const events = page.events ?? [];
  const first = events[0]?.cursor.seq;
  const through = before === null ? head : Math.min(head ?? 0, before - 1);
  // A server that ignores backward pagination must not make us accept an old
  // prefix and then jump over all recent messages. Check every history boundary
  // before mutating the cache or either continuation cursor.
  if (page.gap || head === undefined || !Number.isSafeInteger(head) || head < 0 ||
      (through === 0 ? events.length !== 0 : events.at(-1)?.cursor.seq !== through) ||
      events.some((event, index) => event.cursor.seq !== (first ?? 0) + index) ||
      (page.complete
        ? (events.length > 0 && first !== 1) || page.nextCursor != null
        : first === undefined || first <= 1 || page.nextCursor?.seq !== first)) {
    throw new Error("Server did not return the requested recent history window");
  }
  return page;
}

async function fetchEvents(universeId: string, sessionId: string, after: number, waitMs: number, signal: AbortSignal): Promise<SessionEventsPage & ReadMetadata> {
  // A probe needs at most one event to establish progress; do not make its
  // shorter deadline depend on projecting a full catch-up page.
  const params = new URLSearchParams({ limit: String(waitMs === 0 ? 1 : PAGE_LIMIT), after: String(after), waitMs: String(waitMs) });
  const request = new AbortController();
  const cancel = () => request.abort(signal.reason);
  signal.addEventListener("abort", cancel, { once: true });
  if (signal.aborted) cancel();
  // Bound stalled connections too, including a reconnect probe whose socket
  // never returns a response. Leave room for auth, projection, and transport.
  const timeout = setTimeout(() => request.abort(
    new DOMException("Session event request timed out", "TimeoutError"),
  ), waitMs + REQUEST_GRACE_MS);
  try {
    return await readPage(`${baseUrl(universeId, sessionId)}/events?${params}`, request.signal);
  } catch (error) {
    throw request.signal.aborted ? request.signal.reason : error;
  } finally {
    clearTimeout(timeout);
    signal.removeEventListener("abort", cancel);
  }
}

async function readPage<T>(url: string, signal: AbortSignal): Promise<T & ReadMetadata> {
  const res = await fetch(url, { credentials: "same-origin", signal });
  if (!res.ok) throw new SessionReadHttpError(res.status);
  return withReadMetadata(await res.json() as T, res.headers);
}

class SessionReadHttpError extends Error {
  constructor(readonly status: number) {
    super(`Reading session history failed (${status})`);
  }
}

function errorText(error: unknown): string { return error instanceof Error ? error.message : String(error); }

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => { signal.removeEventListener("abort", done); clearTimeout(timer); resolve(); };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
  });
}
