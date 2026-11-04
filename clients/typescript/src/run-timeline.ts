import type {
  ContextEntryView,
  ContextMessageRoleView,
  RunView,
  ToolCallView,
} from "./generated/types.js";

export type RunTimelineItem =
  | { type: "message"; role: ContextMessageRoleView; entry: ContextEntryView }
  | { type: "reasoning"; entry: ContextEntryView }
  | RunTimelineTool
  /** Anything else in the run's context, such as instructions or a catalog. */
  | { type: "other"; entry: ContextEntryView };

export interface RunTimelineTool {
  type: "tool";
  callId: string;
  name: string;
  batchId: string | null;
  /** Status, timing, and output preview. Null when the run has no view of the call. */
  call: ToolCallView | null;
  callEntry: ContextEntryView | null;
  resultEntry: ContextEntryView | null;
}

/**
 * A run's entries and tool calls as one list in the order they happened, with
 * each tool call merged with its result. A call that has no entry yet, because
 * the run is still going, comes last. When `run.earlierEventsOmitted` is set, the
 * middle of the run is absent from this list too.
 */
export function runTimeline(run: RunView): RunTimelineItem[] {
  const views = new Map<string, { call: ToolCallView; batchId: string }>();
  for (const batch of run.toolBatches ?? []) {
    for (const call of batch.calls ?? []) {
      views.set(call.callId, { call, batchId: batch.id });
    }
  }

  const items: RunTimelineItem[] = [];
  const tools = new Map<string, RunTimelineTool>();
  const toolFor = (callId: string, name: string): RunTimelineTool => {
    const existing = tools.get(callId);
    if (existing) {
      return existing;
    }
    const view = views.get(callId);
    const tool: RunTimelineTool = {
      type: "tool",
      callId,
      name: view?.call.toolName ?? name,
      batchId: view?.batchId ?? null,
      call: view?.call ?? null,
      callEntry: null,
      resultEntry: null,
    };
    tools.set(callId, tool);
    items.push(tool);
    return tool;
  };

  for (const entry of run.entries ?? []) {
    const kind = entry.kind;
    switch (kind.type) {
      case "message":
        items.push({ type: "message", role: kind.role, entry });
        break;
      case "reasoningState":
        items.push({ type: "reasoning", entry });
        break;
      case "toolCall":
        toolFor(kind.callId, kind.name).callEntry = entry;
        break;
      case "toolResult":
        toolFor(kind.callId, "unknown").resultEntry = entry;
        break;
      default:
        items.push({ type: "other", entry });
    }
  }
  for (const [callId, { call }] of views) {
    toolFor(callId, call.toolName);
  }
  return items;
}
