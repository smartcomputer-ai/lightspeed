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
  /// Local input awaiting backend acceptance, rather than transcript arrival.
  pendingInput?: boolean;
  /// Everything the run did: thinking, tool calls, interim notes, steering
  /// input, and system entries appended mid-run, in order.
  work: TranscriptEntry[];
  /// The recorded output of a finished run, shown outside the fold.
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

/// Render local sends in the same section and component position as their
/// eventual run input. Only local sends use submission keys; backend-loaded
/// runs use their run id even when older history later reveals their input.
export function withPendingRunInputs(
  sections: TranscriptSection[],
  pending: { id: string; text: string; runId: string | null }[],
  submissionKeys: ReadonlyMap<string, string>,
): TranscriptSection[] {
  const attached = new Set<string>();
  const result = sections.map((section) => {
    if (section.kind !== "run") return section;
    const message = pending.find((message) => message.runId !== null && message.runId === section.runId);
    if (message) attached.add(message.id);
    return {
      ...section,
      key: section.runId ? submissionKeys.get(section.runId) ?? `run-${section.runId}` : section.key,
      ...(!section.input && message ? {
        input: pendingInput(message),
        // Matching a backend run already confirms acceptance.
        pendingInput: false,
      } : {}),
    };
  });
  for (const message of pending) {
    if (attached.has(message.id)) continue;
    result.push({
      kind: "run", key: message.id, runId: message.runId ?? undefined,
      input: pendingInput(message), pendingInput: message.runId === null, work: [], live: false,
    });
  }
  return result;
}

function pendingInput(message: { id: string; text: string }): TranscriptMessage {
  return { kind: "message", key: message.id, role: "user", text: message.text };
}

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
    const outputRef = section.summary?.outputContentRef;
    for (let index = section.work.length - 1; index >= 0; index -= 1) {
      const entry = section.work[index]!;
      // A loaded completion is authoritative, including an explicit empty
      // output. Never substitute an interim note when its output isn't loaded.
      const matches = outputRef !== undefined
        ? outputRef !== null && entry.kind === "message"
          && entry.contentRef === outputRef && entry.runId === section.runId
        : index === section.work.length - 1;
      if (matches && entry.kind === "message" && entry.role === "assistant") {
        section.reply = entry;
        section.work = section.work.filter((_, i) => i !== index);
        break;
      }
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
    appendWork(open.work, entry);
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

/// Completions splits fields of one native message into separate entries.
/// Present its reasoning before its text, within that turn only. Providers
/// with ordered output items retain their original order.
function appendWork(work: TranscriptEntry[], entry: TranscriptEntry) {
  let index = work.length;
  if (entry.kind === "reasoning" && entry.providerKind === "openai.completions.reasoning_state"
    && entry.runId !== undefined && entry.turnId !== undefined) {
    while (index > 0) {
      const previous = work[index - 1]!;
      if (previous.kind !== "message" || previous.role !== "assistant"
        || previous.providerKind !== "openai.completions.message"
        || previous.runId !== entry.runId || previous.turnId !== entry.turnId) break;
      index -= 1;
    }
  }
  work.splice(index, 0, entry);
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
