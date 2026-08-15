import { useEffect, useState } from "react";
import type { SessionEventsPage } from "@/api";
import {
  applyEvents,
  emptyTranscript,
  type TranscriptState,
} from "./transcript";

/// Live transcript tail: paged catch-up through the event log, then a
/// long-poll follow loop (`waitMs` parks at the engine). One loop per
/// mounted session detail; unmount aborts the in-flight request.

export interface SessionTail {
  transcript: TranscriptState;
  phase: "loading" | "live";
  error: string | null;
  /// Catch-up hit the page cap: a stretch of events between the loaded
  /// prefix and the live head was skipped so the chat can go live instead
  /// of paging forever.
  truncated: boolean;
}

const PAGE_LIMIT = 500;
const MAX_CATCHUP_PAGES = 20;
const WAIT_MS = 25_000;
/// A follow response that comes back empty *fast* means the backend does
/// not actually long-poll (e.g. a minimal stub) — pace client-side rather
/// than spinning.
const FAST_EMPTY_MS = 1_500;
const PACING_SLEEP_MS = 2_000;
const RETRY_SLEEP_MS = 3_000;

export function useSessionTail(universeId: string, sessionId: string): SessionTail {
  const [tail, setTail] = useState<SessionTail>(() => ({
    transcript: emptyTranscript(),
    phase: "loading",
    error: null,
    truncated: false,
  }));

  useEffect(() => {
    const abort = new AbortController();
    const signal = abort.signal;
    let transcript = emptyTranscript();
    let cursor: number | null = null;
    let truncated = false;
    setTail({ transcript, phase: "loading", error: null, truncated: false });

    const push = (patch: Partial<SessionTail>) => {
      if (!signal.aborted) {
        setTail((prev) => ({ ...prev, ...patch }));
      }
    };

    void (async () => {
      // Catch-up: page forward without waiting until the log is drained.
      try {
        for (let page = 0; ; page++) {
          const response = await fetchEvents(universeId, sessionId, cursor, 0, signal);
          if (response.events?.length) {
            transcript = applyEvents(transcript, response.events);
          }
          cursor = response.nextCursor?.seq ?? cursor;
          if (response.complete || !response.nextCursor) {
            break;
          }
          if (page + 1 >= MAX_CATCHUP_PAGES) {
            // Jump to the live head — following matters more than the
            // middle of a pathological backlog.
            truncated = true;
            const head = response.headCursor?.seq;
            if (head !== undefined && head !== null && head > (cursor ?? 0)) {
              cursor = head;
            }
            break;
          }
        }
      } catch (error) {
        if (!signal.aborted) {
          push({ error: errorText(error) });
        }
        return;
      }
      push({ transcript, phase: "live", truncated });
      if (transcript.closed) {
        return;
      }

      // Follow: long-poll; errors retry with backoff instead of killing
      // the chat (network blips are normal for a tab left open).
      let hadError = false;
      while (!signal.aborted) {
        const startedAt = Date.now();
        try {
          const response = await fetchEvents(universeId, sessionId, cursor, WAIT_MS, signal);
          if (response.events?.length) {
            transcript = applyEvents(transcript, response.events);
            cursor = response.nextCursor?.seq ?? cursor;
            push({ transcript, ...(hadError ? { error: null } : {}) });
            hadError = false;
            if (transcript.closed) {
              return;
            }
          } else {
            if (hadError) {
              push({ error: null });
              hadError = false;
            }
            if (Date.now() - startedAt < FAST_EMPTY_MS) {
              await sleep(PACING_SLEEP_MS, signal);
            }
          }
        } catch (error) {
          if (signal.aborted) {
            return;
          }
          hadError = true;
          push({ error: errorText(error) });
          await sleep(RETRY_SLEEP_MS, signal);
        }
      }
    })();

    return () => abort.abort();
  }, [universeId, sessionId]);

  return tail;
}

async function fetchEvents(
  universeId: string,
  sessionId: string,
  after: number | null,
  waitMs: number,
  signal: AbortSignal,
): Promise<SessionEventsPage> {
  const params = new URLSearchParams({ limit: String(PAGE_LIMIT) });
  if (after !== null) {
    params.set("after", String(after));
  }
  if (waitMs > 0) {
    params.set("waitMs", String(waitMs));
  }
  const res = await fetch(
    `/api/v1/universes/${universeId}/sessions/${sessionId}/events?${params}`,
    { credentials: "same-origin", signal },
  );
  if (!res.ok) {
    throw new Error(`reading session events failed (${res.status})`);
  }
  return (await res.json()) as SessionEventsPage;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      signal.removeEventListener("abort", done);
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done);
  });
}
