import type { RunStatus } from "@lightspeed-ai/sdk";
import { sectionsByRun, type RunSection } from "./run-sections";
import type { TranscriptEntry, TranscriptMedia, TranscriptState } from "./transcript";

/** Local identity lasts for this view, including after the server confirms it. */
export interface LocalSubmission {
  id: string;
  kind: "run" | "steering";
  text: string;
  media?: TranscriptMedia[];
  runId: string | null;
  steeringId?: string;
  /** Initial placement; the tail can reveal an earlier run we hadn't loaded. */
  queued: boolean;
  status?: RunStatus;
  withdrawn?: boolean;
}

type SubmissionAction =
  | { type: "send"; submission: LocalSubmission }
  | { type: "accepted"; id: string; runId: string; status?: RunStatus; steeringId?: string }
  | { type: "withdrawn"; id: string };

export function submissionReducer(state: LocalSubmission[], action: SubmissionAction): LocalSubmission[] {
  if (action.type === "send") {
    const { submission } = action;
    const index = state.findIndex(item => item.id === submission.id);
    return index < 0 ? [...state, submission] : state.map((item, i) => i === index ? submission : item);
  }
  return state.map(item => {
    if (item.id !== action.id) return item;
    if (action.type === "withdrawn") return { ...item, withdrawn: true };
    return { ...item, runId: action.runId,
      ...(item.kind === "run" ? { status: action.status } : { steeringId: action.steeringId }),
    };
  });
}

const steeringKey = (runId: string, steeringId: string) => JSON.stringify([runId, steeringId]);
type Message = Extract<TranscriptEntry, { kind: "message" }>;

/** One projection owns echo visibility, queue placement, and stable render keys.
 * Only loaded transcript outcomes retire an unmaterialized echo: a newer HTTP
 * snapshot can report completion before the event reader has reached its input.
 */
export function projectSubmissions(state: TranscriptState, submissions: LocalSubmission[]) {
  const local = submissions.map(item => ({
    ...item,
    runId: item.runId ?? state.runBySubmission.get(item.id) ?? null,
    steeringId: item.steeringId ?? state.steeringBySubmission.get(item.id),
  }));
  const inputs = new Map<string, Message>();
  const steers = new Map<string, Message>();
  for (const entry of state.entries) {
    if (entry.kind !== "message" || entry.role !== "user" || !entry.runId) continue;
    if (!entry.steering) inputs.set(entry.runId, entry);
    else if (entry.steeringId) steers.set(steeringKey(entry.runId, entry.steeringId), entry);
  }

  const aliases = new Map<string, LocalSubmission>();
  const runKeys = new Map<string, string>();
  const pendingRuns: LocalSubmission[] = [];
  const pendingSteers: LocalSubmission[] = [];
  const queued = new Set<string>();
  const notices: { id: string; text: string }[] = [];
  for (const item of local) {
    const runId = item.runId;
    const confirmed = !runId ? undefined : item.kind === "run" ? inputs.get(runId)
      : item.steeringId ? steers.get(steeringKey(runId, item.steeringId)) : undefined;
    if (item.kind === "run" && runId) runKeys.set(runId, item.id);
    if (confirmed) {
      aliases.set(confirmed.key, item);
      continue;
    }
    if (item.withdrawn) continue;
    if (state.closed || (runId && state.finishedRuns.has(runId))) {
      if (item.kind === "steering") notices.push({
        id: item.id,
        text: `steering not delivered — the run ended before its next turn: “${item.text}”`,
      });
      continue;
    }
    if (item.kind === "steering") {
      pendingSteers.push(item);
      continue;
    }
    pendingRuns.push(item);
    const leftQueue = runId && (state.runPhases.get(runId) === "running" || state.runStartedAtMs.has(runId))
      || item.status === "running" || item.status === "parked" || item.status === "cancelling"
      || item.status === "completed" || item.status === "failed";
    const behind = runId !== null && state.runPhases.get(runId) === "queued"
      && (state.activeRun !== null || state.queuedRuns[0]?.runId !== runId);
    if (!leftQueue && (item.queued || behind)) queued.add(item.id);
  }

  const entries = state.entries.map(entry => {
    const item = aliases.get(entry.key);
    if (!item || entry.kind !== "message") return entry;
    // Keep already-loaded attachment previews during confirmation too.
    const media = entry.media?.map(media => ({ ...media,
      localUrl: item.media?.find(local => local.blobRef === media.blobRef)?.localUrl ?? media.localUrl,
    }));
    return { ...entry, key: item.id, local: true, ...(media ? { media } : {}) };
  });
  const sections = sectionsByRun(entries, state.activeRun);
  const runs = new Map<string, RunSection>();
  for (const section of sections) {
    if (section.kind !== "run" || !section.runId) continue;
    section.key = runKeys.get(section.runId) ?? `run-${section.runId}`;
    runs.set(section.runId, section);
  }
  const sectionFor = (item: LocalSubmission) => {
    let section = item.runId ? runs.get(item.runId) : undefined;
    if (!section) {
      section = { kind: "run", key: item.kind === "run" ? item.id : `run-${item.runId}`,
        runId: item.runId ?? undefined, work: [], live: false };
      sections.push(section);
      if (item.runId) runs.set(item.runId, section);
    }
    return section;
  };
  for (const item of pendingRuns) {
    if (!queued.has(item.id)) sectionFor(item).input ??= echo(item);
  }
  // Steering lives in the run's work list from the first render. Confirmation
  // changes its data at the same key and parent, rather than moving a bottom row.
  for (const item of pendingSteers) {
    const section = sectionFor(item);
    // A newer snapshot can close the run before its terminal event is loaded.
    // Keep its interim note in order while the steering echo is still pending.
    if (!section.summary && section.reply) {
      section.work.push(section.reply);
      section.reply = undefined;
    }
    section.live = true;
    section.work.push(echo(item));
  }

  const queuedRuns = state.queuedRuns.filter(run => {
    const item = local.find(item => item.kind === "run" && item.runId === run.runId);
    return !item || queued.has(item.id);
  });
  return { sections, pendingRuns, queued, queuedRuns, notices,
    inlinePending: pendingRuns.filter(item => !queued.has(item.id)),
  };
}

function echo(item: LocalSubmission): Message {
  return { kind: "message", key: item.id, role: "user", text: item.text, local: true,
    runId: item.runId ?? undefined, media: item.media,
    ...(item.kind === "steering" ? { steering: true, steeringId: item.steeringId } : {}),
  };
}
