import type { Attribution, ContextEntryView, SessionEventView } from "./generated/types.js";

export type TranscriptMessageAuthor =
  | {
      kind: "actor";
      /** Opaque actor ID. Resolve names in the application, not the runtime. */
      id: string;
      /** Only requestedBy is authenticated attribution; origin is a display claim. */
      source: "requestedBy" | "origin";
    }
  | { kind: "event" };

export interface TranscriptMessageMetadata {
  /** Server acceptance time for input, otherwise the earliest loaded event
   * carrying this entry. Neither a device send time nor a read receipt.
   */
  sentAtMs: number;
  /** contextEvent is provisional for input whose acceptance is not loaded. */
  timeSource: "runAccepted" | "runSteeringAccepted" | "contextEvent";
  /** User-message display author. Null for assistant messages or unresolved authors. */
  author: TranscriptMessageAuthor | null;
  /** Full attribution from this input's acceptance, including keys and internal work. */
  requestedBy: Attribution | null;
}

/** Resolve a user message's display author, or a user-role list preview's origin.
 * An authenticated actor takes precedence over a user:<id> display claim.
 * origin=event stays a bot delivery. Other origins alone are unknown. This helper
 * never establishes membership or authorization, and does not resolve names.
 */
export function transcriptAuthor(
  origin: string | null | undefined,
  requestedBy?: Attribution | null,
): TranscriptMessageAuthor | null {
  if (origin === "event") return { kind: "event" };
  if (requestedBy?.kind === "actor") {
    return { kind: "actor", id: requestedBy.id, source: "requestedBy" };
  }
  return origin?.startsWith("user:") && origin.length > 5
    ? { kind: "actor", id: origin.slice(5), source: "origin" }
    : null;
}

type Acceptance = {
  seq: number;
  atMs: number;
  kind: "runAccepted" | "runSteeringAccepted";
  requestedBy: Attribution | null;
};
type Message = { seq: number; atMs: number; entry: ContextEntryView };

function rememberFirst<T extends { seq: number }>(index: Map<string, T>, id: string, value: T) {
  const previous = index.get(id);
  if (!previous || value.seq < previous.seq) index.set(id, value);
}

const steeringKey = (runId: string, steeringId: string) => JSON.stringify([runId, steeringId]);

/** Derive metadata by message entry ID for one session's loaded history.
 *
 * Pass the union of loaded history pages and live events, in any order.
 * Overlapping pages are safe; events from other sessions are ignored. Rerun
 * when that union changes: older acceptance events correct input times and
 * authors, and earlier context events correct provisional message times.
 * Sequence determines the first appearance, even if wall-clock time regresses.
 *
 * This performs no I/O, mutates no input, and retains no state between calls.
 * It indexes metadata in linear time without reading blobs or copying message
 * content. Map iteration order is not transcript order. Entries absent from
 * these events (including queued input not yet applied) have no result.
 */
export function transcriptMetadata(
  sessionId: string,
  events: readonly SessionEventView[],
): ReadonlyMap<string, TranscriptMessageMetadata> {
  const runs = new Map<string, Acceptance>();
  const steering = new Map<string, Acceptance>();
  const messages = new Map<string, Message>();
  for (const event of events) {
    if (event.sessionId !== sessionId) continue;
    const { kind, observedAtMs: atMs, cursor: { seq } } = event;
    switch (kind.type) {
      case "runAccepted":
      case "runSteeringAccepted": {
        const acceptance: Acceptance = {
          seq, atMs, kind: kind.type, requestedBy: kind.requestedBy ?? null,
        };
        if (kind.type === "runAccepted") {
          rememberFirst(runs, kind.runId, acceptance);
        } else {
          rememberFirst(steering, steeringKey(kind.runId, kind.steeringId), acceptance);
        }
        break;
      }
      case "contextEntriesApplied":
      case "contextEntriesReplaced":
      case "contextKeyPrefixReplaced":
        for (const entry of kind.entries) {
          if (entry.kind.type === "message") rememberFirst(messages, entry.id, { seq, atMs, entry });
        }
        break;
    }
  }

  const result = new Map<string, TranscriptMessageMetadata>();
  for (const [id, { entry, atMs }] of messages) {
    const user = entry.kind.type === "message" && entry.kind.role === "user";
    const source = entry.source;
    let acceptance: Acceptance | undefined;
    if (user && source?.type === "runInput") {
      acceptance = runs.get(source.runId);
    } else if (user && source?.type === "steering") {
      acceptance = steering.get(steeringKey(source.runId, source.steeringId));
    }
    const requestedBy = acceptance?.requestedBy ? { ...acceptance.requestedBy } : null;
    result.set(id, {
      sentAtMs: acceptance?.atMs ?? atMs,
      timeSource: acceptance?.kind ?? "contextEvent",
      author: user ? transcriptAuthor(entry.origin, requestedBy) : null,
      requestedBy,
    });
  }
  return result;
}
