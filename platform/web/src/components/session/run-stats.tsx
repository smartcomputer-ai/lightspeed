import { ChevronDown, TriangleAlert } from "lucide-react";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  formatDuration,
  formatTokens,
  type TranscriptRunSummary,
} from "@/lib/sessions/transcript";
import { cn } from "@/lib/utils";

// No pill anywhere it appears: transparent at rest, on hover, and while the
// popover is open; the text brightening is the whole affordance.
const triggerClass = "inline-flex max-w-full items-center gap-1 bg-transparent px-1 py-0.5 text-xs text-muted-foreground tabular-nums transition-colors hover:bg-transparent hover:text-foreground data-[popup-open]:bg-transparent data-[popup-open]:text-foreground focus-visible:rounded-sm focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring";

function tokens(value: number | undefined) {
  return value === undefined ? "Unavailable" : `${formatTokens(value)} tokens`;
}

function usageTotal(summary: TranscriptRunSummary): number | undefined {
  const usage = summary.usage;
  return usage?.inputTokens !== undefined && usage.outputTokens !== undefined
    ? usage.inputTokens + usage.outputTokens : undefined;
}

export function hasRunStats(summary: TranscriptRunSummary): boolean {
  return summary.contextTokens !== undefined || usageTotal(summary) !== undefined;
}

/// "Context 42k · Usage 61k" with the breakdown behind a popover. Renders
/// nothing when neither figure is known.
export function RunStatsTrigger({ summary, className }: { summary: TranscriptRunSummary; className?: string }) {
  const { contextTokens } = summary;
  const total = usageTotal(summary);
  if (contextTokens === undefined && total === undefined) return null;
  return (
    <Popover>
      <PopoverTrigger className={cn(triggerClass, className)} aria-label="Run statistics">
        {contextTokens !== undefined && <span>Context {formatTokens(contextTokens)}</span>}
        {contextTokens !== undefined && total !== undefined && <span aria-hidden="true">·</span>}
        {total !== undefined && <span>Usage {formatTokens(total)}</span>}
        <ChevronDown className="size-3! shrink-0" />
      </PopoverTrigger>
      <PopoverContent className="w-[min(21rem,calc(100vw-2rem))] p-4" aria-label="Run statistics" side="top">
        <RunStatsDetails summary={summary} />
      </PopoverContent>
    </Popover>
  );
}

/// The statistics as the first row of an opened run, for narrow screens
/// where the strip has no room for them.
export function RunStatsRow({ summary, className }: { summary: TranscriptRunSummary; className?: string }) {
  if (!hasRunStats(summary)) return null;
  return (
    <div className={cn("min-w-0 px-0.5", className)}>
      <RunStatsTrigger summary={summary} />
    </div>
  );
}

/// A finished run that did no tool work: its outcome on one quiet line —
/// status when it is not success, duration, and the statistics button when
/// statistics are wanted.
export function RunOutcomeLine({
  summary,
  showStatistics = true,
}: {
  summary: TranscriptRunSummary;
  showStatistics?: boolean;
}) {
  const { status, error, durationMs } = summary;
  const duration = durationMs === undefined ? null : formatDuration(durationMs);
  const stats = showStatistics && hasRunStats(summary);
  if (status === "completed" && !duration && !stats) return null;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 px-2 text-xs text-muted-foreground">
      {status === "failed" ? (
        <span className="flex min-w-0 items-start gap-1.5 text-destructive">
          <TriangleAlert className="mt-px size-3.5 shrink-0" />
          <span className="[overflow-wrap:anywhere]">Run failed{error ? `: ${error}` : ""}</span>
        </span>
      ) : status === "cancelled" ? (
        <span>Run cancelled</span>
      ) : null}
      <span className="ml-auto flex items-center gap-2">
        {duration && <span className="tabular-nums">{duration}</span>}
        {showStatistics && <RunStatsTrigger summary={summary} />}
      </span>
    </div>
  );
}

export function RunStatsDetails({ summary }: { summary: TranscriptRunSummary }) {
  const { contextTokens, usage } = summary;
  const cachePercent = usage?.inputTokens !== undefined && usage.inputTokens > 0 && usage.cachedInputTokens !== undefined
    ? `${Math.round(100 * usage.cachedInputTokens / usage.inputTokens)}%` : "Unavailable";
  return (
    <>
      <p className="text-xs font-medium">Context at last model call</p>
      <p className="mt-1 text-lg font-medium tabular-nums">{tokens(contextTokens)}</p>
      <div className="mt-4 border-t pt-3">
        <p className="mb-2 text-xs font-medium">Run usage</p>
        {summary.usageComplete ? (
          <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-2 text-xs">
            <dt className="text-muted-foreground">Input</dt><dd className="text-right tabular-nums">{tokens(usage?.inputTokens)}</dd>
            <dt className="text-muted-foreground">Output</dt><dd className="text-right tabular-nums">{tokens(usage?.outputTokens)}</dd>
            <dt className="text-muted-foreground">Model calls</dt><dd className="text-right tabular-nums">{usage?.modelCalls ?? 0}</dd>
            <dt className="text-muted-foreground">Tool calls</dt><dd className="text-right tabular-nums">{summary.toolCalls ?? "Unavailable"}</dd>
            <dt className="text-muted-foreground">Input served from cache</dt><dd className="text-right tabular-nums">{cachePercent}</dd>
          </dl>
        ) : (
          <p className="text-xs text-muted-foreground">Full run usage is unavailable until earlier history is loaded.</p>
        )}
        <p className="mt-2 text-xs text-muted-foreground">Cumulative across model calls in this run.</p>
      </div>
    </>
  );
}
