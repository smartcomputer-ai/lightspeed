import type { SessionEvent, SessionRunView } from "@/api";
import { applyEvents, emptyTranscript, reconcileRuns } from "./transcript";

/** A contiguous loaded window, with independent historical and live state.
 * Older events are folded chronologically into a fresh projection. Only its
 * display and projection indexes replace the current transcript; old lifecycle
 * events never re-enter the live controls or trigger session refetches.
 */
export class TranscriptWindow {
  private events = new Map<number, SessionEvent>();
  state = emptyTranscript();

  append(events: SessionEvent[]) {
    const fresh = this.remember(events);
    this.state = applyEvents(this.state, fresh);
  }

  prepend(events: SessionEvent[]) {
    if (this.remember(events).length === 0) return;
    const live = this.state;
    const base = emptyTranscript();
    // Snapshot-supplied start times stay authoritative even while the event
    // window still begins inside that run. Usage completeness remains separate.
    base.runStartedAtMs = new Map(live.runStartedAtMs);
    const rebuilt = applyEvents(base, [...this.events.values()].sort(bySequence));
    // Preserve mounted group identities when an older page supplies the start
    // of a previously displayed continuation. The scroller anchors DOM nodes.
    const keys = new Map(live.entries.flatMap((entry) => entry.kind === "tool-group"
      ? entry.calls.map((call) => [call.callId, entry.key] as const) : []));
    const used = new Set<string>();
    rebuilt.entries = rebuilt.entries.map((entry) => {
      if (entry.kind !== "tool-group") return entry;
      const key = entry.calls.map((call) => keys.get(call.callId)).find((key) => key && !used.has(key));
      if (!key) return entry;
      used.add(key);
      return { ...entry, key };
    });
    // An older page can reveal a queued request before the loaded start.
    // Keep the mounted progress marker's key and hydrate its run attribution.
    const compaction = live.compaction && rebuilt.compaction
      ? { ...live.compaction, runId: live.compaction.runId ?? rebuilt.compaction.runId }
      : live.compaction;
    if (compaction && rebuilt.compaction) {
      const rebuiltKey = rebuilt.compaction.markerKey;
      rebuilt.entries = rebuilt.entries.map((entry) => entry.key === rebuiltKey
        ? { ...entry, key: compaction.markerKey } : entry);
    }
    this.state = {
      ...rebuilt,
      activeRun: live.activeRun,
      queuedRuns: live.queuedRuns,
      runPhases: live.runPhases,
      runBySubmission: live.runBySubmission,
      runRevision: live.runRevision,
      closed: live.closed,
      compaction,
    };
  }

  reconcile(runs: SessionRunView[]) {
    this.state = reconcileRuns(this.state, runs);
  }

  private remember(events: SessionEvent[]) {
    const fresh: SessionEvent[] = [];
    for (const event of events) {
      if (this.events.has(event.cursor.seq)) continue;
      this.events.set(event.cursor.seq, event);
      fresh.push(event);
    }
    return fresh.sort(bySequence);
  }
}

function bySequence(a: SessionEvent, b: SessionEvent) { return a.cursor.seq - b.cursor.seq; }
