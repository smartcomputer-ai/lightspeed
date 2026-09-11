import type { ActiveRun, TranscriptEntry, TranscriptRunSummary } from "./transcript";

/// One level of structure over the flat transcript: the entries a run
/// produced, between the input that started it and the summary that ended
/// it. The view folds a finished section's work behind a strip and leaves
/// the final reply visible; a live section stays open while it streams.

export type TranscriptMessage = Extract<TranscriptEntry, { kind: "message" }>;

export interface RunSection {
  kind: "run";
  key: string;
  runId?: string;
  /// The user message that started the run, when it is in the loaded window.
  input?: TranscriptMessage;
  /// Everything the run did: thinking, tool calls, interim notes, steering
  /// input, and system entries appended mid-run, in order.
  work: TranscriptEntry[];
  /// The last assistant message of a finished run, shown outside the fold.
  reply?: TranscriptMessage;
  summary?: TranscriptRunSummary;
  /// The engine is still executing this run.
  live: boolean;
}

export type TranscriptSystemEntry = Extract<TranscriptEntry, { kind: "system" }>;

export type TranscriptSection =
  | RunSection
  /// Consecutive instruction and catalog entries between runs, one chips row.
  | { kind: "system"; key: string; entries: TranscriptSystemEntry[] }
  | { kind: "entry"; key: string; entry: TranscriptEntry };

/// Group entries into run sections. Session-level markers outside any run
/// stay top-level. A run whose end is not loaded (window cut, session closed
/// mid-run) folds like a finished one, without an outcome. When the engine
/// reports an active run that has produced nothing yet, an empty live
/// section stands in for it so the status row has a home.
export function sectionsByRun(
  entries: TranscriptEntry[],
  activeRun: ActiveRun | null,
): TranscriptSection[] {
  const sections: TranscriptSection[] = [];
  let open: RunSection | null = null;

  const close = (section: RunSection) => {
    const last = section.work.at(-1);
    if (last?.kind === "message" && last.role === "assistant") {
      section.reply = last;
      section.work = section.work.slice(0, -1);
    }
    sections.push(section);
  };
  const start = (input: TranscriptMessage | undefined, first: TranscriptEntry): RunSection => {
    const runId = input?.runId ?? ("runId" in first ? first.runId : undefined);
    return {
      kind: "run",
      key: input?.key ?? (runId ? `run-${runId}` : `run-at-${first.key}`),
      ...(runId ? { runId } : {}),
      ...(input ? { input } : {}),
      work: [],
      live: false,
    };
  };

  for (const entry of entries) {
    if (entry.kind === "message" && entry.role === "user" && !entry.steering) {
      if (open) close(open);
      open = start(entry, entry);
      continue;
    }
    if (entry.kind === "run-summary") {
      const section = open ?? { kind: "run" as const, key: entry.key, runId: entry.runId, work: [], live: false };
      section.summary = entry;
      section.runId ??= entry.runId;
      close(section);
      open = null;
      continue;
    }
    if (entry.kind === "system" && !open) {
      const last = sections.at(-1);
      if (last?.kind === "system") {
        last.entries.push(entry);
      } else {
        sections.push({ kind: "system", key: entry.key, entries: [entry] });
      }
      continue;
    }
    if (entry.kind === "marker" && !open) {
      sections.push({ kind: "entry", key: entry.key, entry });
      continue;
    }
    if (!open) open = start(undefined, entry);
    if (!open.runId && "runId" in entry && entry.runId) open.runId = entry.runId;
    open.work.push(entry);
  }

  if (open) {
    if (activeRun && (!open.runId || open.runId === activeRun.runId)) {
      open.live = true;
      open.runId ??= activeRun.runId;
      sections.push(open);
    } else {
      close(open);
    }
  } else if (activeRun) {
    sections.push({ kind: "run", key: `run-${activeRun.runId}`, runId: activeRun.runId, work: [], live: true });
  }
  return sections;
}

/// Tool call counts and the activity families a section touched, for the
/// folded strip.
export function sectionActivity(section: RunSection): {
  toolCalls: number;
  failedCalls: number;
  groups: string[];
  thinking: boolean;
} {
  let toolCalls = 0;
  let failedCalls = 0;
  let thinking = false;
  const groups = new Set<string>();
  for (const entry of section.work) {
    if (entry.kind === "reasoning") {
      thinking = true;
    } else if (entry.kind === "tool-group") {
      for (const call of entry.calls) {
        toolCalls += 1;
        if (call.isError || call.status === "failed" || call.status === "unavailable") failedCalls += 1;
        groups.add(call.display?.group ?? "other");
      }
    }
  }
  return { toolCalls, failedCalls, groups: [...groups], thinking };
}
