import { useEffect, useState } from "react";
import { Brain, ChevronRight, LoaderCircle, TriangleAlert } from "lucide-react";
import type { FullTextLoader } from "@/components/session/expandable-content";
import { RunOutcomeLine, RunStatsRow, RunStatsTrigger } from "@/components/session/run-stats";
import { ActivityIcon, GROUP_ORDER, groupStyle, useElapsed } from "@/components/session/tool-trace";
import { SystemChips, TranscriptEntryView } from "@/components/session/transcript-view";
import { sectionActivity, type RunSection } from "@/lib/sessions/run-sections";
import { formatDuration, type ActiveRun, type TranscriptEntry } from "@/lib/sessions/transcript";
import { cn } from "@/lib/utils";

/// A run as the transcript shows it: the input band, the work, the reply.
/// Live work streams inside a soft frame with a status row at its foot. A
/// finished run's work folds behind one strip that names what happened;
/// the last reply stays visible below it.
export function RunSectionView({
  section,
  activeRun,
  loadFullText,
  showRunStatistics,
  collapseCompletedRuns,
}: {
  section: RunSection;
  activeRun: ActiveRun | null;
  loadFullText?: FullTextLoader;
  showRunStatistics: boolean;
  collapseCompletedRuns: boolean;
}) {
  // A click on the strip overrides the preference for this run; changing
  // the preference applies it to every run again.
  const [override, setOverride] = useState<boolean | null>(null);
  useEffect(() => setOverride(null), [collapseCompletedRuns]);
  const open = override ?? !collapseCompletedRuns;

  return (
    <div className="flex min-w-0 flex-col gap-3">
      {section.input && <TranscriptEntryView entry={section.input} />}
      {section.live ? (
        <div className="flex min-w-0 flex-col gap-0.5 rounded-lg border border-amber-500/25 bg-card px-1 py-1">
          <WorkList entries={section.work} loadFullText={loadFullText} />
          <RunStatusRow run={activeRun} />
        </div>
      ) : section.work.length > 0 ? (
        <div className="min-w-0 rounded-lg border bg-card">
          <RunStrip
            section={section}
            open={open}
            onToggle={() => setOverride(!open)}
            showRunStatistics={showRunStatistics}
          />
          {open && (
            <div className="flex min-w-0 flex-col gap-0.5 border-t px-1 py-1">
              {showRunStatistics && section.summary && <RunStatsRow summary={section.summary} className="md:hidden" />}
              <WorkList entries={section.work} loadFullText={loadFullText} />
            </div>
          )}
        </div>
      ) : section.summary ? (
        <RunOutcomeLine summary={section.summary} showStatistics={showRunStatistics} />
      ) : null}
      {section.reply && <TranscriptEntryView entry={section.reply} loadFullText={loadFullText} />}
    </div>
  );
}

/// The folded run: chevron, the activity families it touched, what
/// happened and how long it took, and from medium widths up the statistics
/// on the right. Narrow screens get them as the first row of the opened run.
function RunStrip({
  section,
  open,
  onToggle,
  showRunStatistics,
}: {
  section: RunSection;
  open: boolean;
  onToggle: () => void;
  showRunStatistics: boolean;
}) {
  const summary = section.summary;
  const activity = sectionActivity(section);
  const groups = GROUP_ORDER.filter((group) => activity.groups.includes(group));
  const failed = summary?.status === "failed";
  const duration = summary?.durationMs === undefined ? null : formatDuration(summary.durationMs);
  const headline = summary?.status === "failed"
    ? duration ? `Failed after ${duration}` : "Failed"
    : summary?.status === "cancelled"
      ? duration ? `Cancelled after ${duration}` : "Cancelled"
      : duration ? `Worked for ${duration}` : "Worked";

  return (
    <div className="min-w-0">
      <div className="flex min-w-0 items-center gap-1 pr-1.5">
        <button
          type="button"
          aria-expanded={open}
          aria-label={open ? "Hide this run's activity" : "Show this run's activity"}
          onClick={onToggle}
          className="flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2 py-1.5 text-left text-[13px] text-muted-foreground outline-none transition-colors hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/50"
        >
          <ChevronRight
            className={cn("size-3.5 shrink-0 transition-transform", open && "rotate-90")}
            aria-hidden="true"
          />
          <span className="flex shrink-0 items-center gap-1 [&_svg]:size-3.5" aria-hidden="true">
            {activity.thinking && <Brain className="text-muted-foreground" />}
            {groups.map((group) => (
              <ActivityIcon key={group} group={group} className={groupStyle(group).text} />
            ))}
          </span>
          <span className={cn("shrink-0 font-medium text-foreground", failed && "text-destructive")}>
            {headline}
          </span>
          {activity.toolCalls > 0 && (
            <span className="min-w-0 truncate tabular-nums">
              · {activity.toolCalls} tool {activity.toolCalls === 1 ? "call" : "calls"}
            </span>
          )}
          {activity.failedCalls > 0 && (
            <span className="shrink-0 rounded-full border border-destructive/30 px-1.5 text-[11px] leading-4 text-destructive">
              {activity.failedCalls} failed
            </span>
          )}
        </button>
        {showRunStatistics && summary && <RunStatsTrigger summary={summary} className="hidden shrink-0 md:inline-flex" />}
      </div>
      {summary?.status === "failed" && (
        <p className="flex items-start gap-1.5 px-2 pb-1.5 text-xs text-destructive">
          <TriangleAlert className="mt-px size-3.5 shrink-0" />
          <span className="[overflow-wrap:anywhere]">Run failed{summary.error ? `: ${summary.error}` : ""}</span>
        </p>
      )}
    </div>
  );
}

/// The run's entries in order. Consecutive system entries (instructions,
/// catalog versions) collapse into one row of chips.
export function WorkList({
  entries,
  loadFullText,
}: {
  entries: TranscriptEntry[];
  loadFullText?: FullTextLoader;
}) {
  const rows: React.ReactNode[] = [];
  let chips: Extract<TranscriptEntry, { kind: "system" }>[] = [];
  const flush = () => {
    if (chips.length > 0) {
      rows.push(<SystemChips key={chips[0]!.key} entries={chips} />);
      chips = [];
    }
  };
  for (const entry of entries) {
    if (entry.kind === "system") {
      chips.push(entry);
      continue;
    }
    flush();
    rows.push(
      <div key={entry.key} className={cn("min-w-0", entry.kind === "message" && "px-2 py-1")}>
        <TranscriptEntryView entry={entry} loadFullText={loadFullText} />
      </div>,
    );
  }
  flush();
  return <>{rows}</>;
}

/// Where the eye rests while a run is live: the TUI's status vocabulary
/// (planning / thinking / running tools / …) with the time elapsed.
export function RunStatusRow({ run }: { run: ActiveRun | null }) {
  const elapsed = useElapsed(run?.startedAtMs, run !== null);
  if (!run) return null;
  return (
    <div role="status" className="flex items-center gap-2 px-2 py-1 text-[13px] text-muted-foreground">
      <LoaderCircle className="size-3.5 shrink-0 motion-safe:animate-spin" aria-hidden="true" />
      <span>{run.label}…</span>
      {elapsed !== undefined && <span className="ml-auto text-xs tabular-nums">{formatDuration(elapsed)}</span>}
    </div>
  );
}
