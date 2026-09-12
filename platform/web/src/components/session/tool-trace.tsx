import { useContext, useEffect, useState, type ComponentType, type ReactNode, type SVGProps } from "react";
import { MediaStrip } from "@/components/session/media";
import { TranscriptLinksContext, type TranscriptLinks } from "@/components/session/transcript-links";
import {
  Brain,
  Check,
  CircleAlert,
  CircleMinus,
  Clock3,
  Copy,
  ExternalLink,
  GitFork,
  Hourglass,
  Layers,
  LoaderCircle,
  PencilLine,
  Plug,
  Repeat,
  Search,
  Send,
  SquareTerminal,
  Wrench,
} from "lucide-react";
import { BotIcon } from "@/components/icons/bot";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { MarkdownContent } from "@/components/session/markdown-content";
import {
  ExpandableContent,
  type FullTextLoader,
} from "@/components/session/expandable-content";
import {
  formatDuration,
  isFailedToolCall,
  isTerminalToolStatus,
  toolTarget,
  type TranscriptToolCall,
  type TranscriptToolGroup,
} from "@/lib/sessions/transcript";
import { cn } from "@/lib/utils";

/// Every step a run takes — thinking, a tool call, a sub-agent, a bot emit,
/// a channel send — is one row: icon · verb · target · detail · duration.
/// Success costs no ink; only running, waiting, failed and cancelled rows
/// carry a mark. Clicking a row opens its details beneath it.

export { TranscriptLinksContext, type TranscriptLinks } from "@/components/session/transcript-links";

type IconComponent = ComponentType<SVGProps<SVGSVGElement> & { className?: string }>;

interface GroupStyle {
  icon: IconComponent;
  text: string;
  tile: string;
}

const GROUP_STYLES: Record<string, GroupStyle> = {
  explore: { icon: Search, text: "text-cyan-700 dark:text-cyan-300", tile: "bg-cyan-500/10" },
  edit: { icon: PencilLine, text: "text-amber-700 dark:text-amber-300", tile: "bg-amber-500/10" },
  execute: { icon: SquareTerminal, text: "text-blue-700 dark:text-blue-300", tile: "bg-blue-500/10" },
  mcp: { icon: Plug, text: "text-violet-700 dark:text-violet-300", tile: "bg-violet-500/10" },
  agent: { icon: GitFork, text: "text-indigo-700 dark:text-indigo-300", tile: "bg-indigo-500/10" },
  bot: { icon: BotIcon, text: "text-teal-700 dark:text-teal-300", tile: "bg-teal-500/10" },
  message: { icon: Send, text: "text-emerald-700 dark:text-emerald-300", tile: "bg-emerald-500/10" },
  other: { icon: Wrench, text: "text-foreground/75", tile: "bg-muted" },
};

/// Display order of activity families on a folded run strip.
export const GROUP_ORDER = ["explore", "edit", "execute", "mcp", "agent", "bot", "message", "other"];

export function groupStyle(group: string | null | undefined): GroupStyle {
  return GROUP_STYLES[group ?? "other"] ?? GROUP_STYLES.other!;
}

export function ActivityIcon({ group, className }: { group?: string | null; className?: string }) {
  const Icon = groupStyle(group).icon;
  return <Icon className={className} aria-hidden="true" />;
}

function callIcon(call: TranscriptToolCall): IconComponent {
  const display = call.display;
  if (display?.group === "bot" && display.verb === "Emit" && display.target === "to self") return Repeat;
  if (display?.group === "agent" && display.verb === "Await") return Hourglass;
  return groupStyle(display?.group).icon;
}

/// Full paths from the environment are long; keep the tail that identifies
/// the file and let the title carry the rest.
export function shortenTarget(target: string, max = 64): string {
  if (target.length <= max || !target.includes("/")) return target;
  const parts = target.split("/").filter(Boolean);
  let tail = parts.pop() ?? target;
  while (parts.length > 0) {
    const candidate = `${parts.at(-1)}/${tail}`;
    if (candidate.length + 2 > max) break;
    tail = candidate;
    parts.pop();
  }
  return parts.length > 0 ? `…/${tail}` : `/${tail}`;
}

function firstLine(text: string | null | undefined, max = 160): string | null {
  const line = text?.split("\n").map((line) => line.trim()).find((line) => line.length > 0);
  if (!line) return null;
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/// Milliseconds elapsed since `since`, ticking once a second while active.
export function useElapsed(since: number | undefined, active: boolean): number | undefined {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active || since === undefined) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [active, since]);
  if (!active || since === undefined) return undefined;
  return Math.max(0, now - since);
}

/// One activity row. The row is the trigger; the detail opens beneath it.
export function StepRow({
  icon,
  tone,
  verb,
  target,
  targetTitle,
  detail,
  right,
  failed = false,
  cancelled = false,
  note,
  extra,
  ariaLabel,
  children,
}: {
  icon: ReactNode;
  tone: string;
  verb: string;
  target?: string | null;
  targetTitle?: string;
  detail?: string | null;
  right?: ReactNode;
  failed?: boolean;
  cancelled?: boolean;
  /// A line under the row: the first line of an error, or a link.
  note?: ReactNode;
  /// Trailing inline content before the right slot, such as a link.
  extra?: ReactNode;
  ariaLabel?: string;
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={cn("min-w-0", failed && "rounded-md bg-destructive/5", cancelled && "opacity-60")}>
      <div className="group/step flex min-w-0 items-center gap-2 rounded-md pr-2 transition-colors hover:bg-muted/50">
        <button
          type="button"
          aria-expanded={open}
          aria-label={ariaLabel}
          onClick={() => setOpen((value) => !value)}
          className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1 text-left text-[13px] leading-5 outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          <span className={cn("flex size-3.5 shrink-0 items-center justify-center [&_svg]:size-3.5", tone)}>
            {icon}
          </span>
          <span className={cn("shrink-0 font-medium", tone)}>{verb}</span>
          {target && (
            <span
              className={cn("min-w-0 truncate text-foreground/85", cancelled && "line-through")}
              title={targetTitle ?? target}
            >
              {target}
            </span>
          )}
          {detail && <span className="min-w-0 shrink-[2] truncate text-muted-foreground">{detail}</span>}
        </button>
        {extra}
        {right !== undefined && right !== null && (
          <span
            className={cn(
              "flex shrink-0 items-center gap-1 text-xs tabular-nums text-muted-foreground [&_svg]:size-3",
              failed && "text-destructive",
            )}
          >
            {right}
          </span>
        )}
      </div>
      {note}
      {open && children}
    </div>
  );
}

export function ReasoningTrace({ text }: { text: string }) {
  return (
    <StepRow
      icon={<Brain />}
      tone="text-muted-foreground"
      verb="Thinking"
      target={reasoningTitle(text)}
      targetTitle=""
      ariaLabel="Thinking"
    >
      <div className="mb-1 pl-[1.875rem] pr-2 text-muted-foreground/80">
        <MarkdownContent className="text-xs italic">{text}</MarkdownContent>
      </div>
    </StepRow>
  );
}

export function ToolGroupTrace({
  group,
  loadFullText,
}: {
  group: TranscriptToolGroup;
  loadFullText?: FullTextLoader;
}) {
  const waiting = group.status === "waiting";
  if (group.calls.length <= 1) {
    const call = group.calls[0];
    return call ? <ToolCallRow call={call} waiting={waiting} loadFullText={loadFullText} /> : null;
  }
  const active = !isTerminalToolStatus(group.status);
  const completed = group.calls.filter((call) => isTerminalToolStatus(call.status)).length;
  const failed = group.calls.filter(isFailedToolCall).length;
  const groups = new Set(group.calls.map((call) => call.display?.group ?? "other"));
  const single = groups.size === 1 ? [...groups][0] : undefined;
  const started = group.calls.map((call) => call.startedAtMs).filter((value): value is number => value !== undefined);
  const finished = group.calls.map((call) => call.completedAtMs).filter((value): value is number => value !== undefined);
  const durationMs = !active && started.length > 0 && finished.length === group.calls.length
    ? Math.max(0, Math.max(...finished) - Math.min(...started)) : undefined;

  return (
    <div className={cn("min-w-0 rounded-lg border bg-card", active && "border-amber-500/30")}>
      <div className="flex items-center gap-2 px-2 py-1.5 text-[13px]">
        <span
          className={cn(
            "flex size-6 shrink-0 items-center justify-center rounded-md [&_svg]:size-3.5",
            single ? groupStyle(single).tile : "bg-muted",
            single ? groupStyle(single).text : "text-muted-foreground",
          )}
        >
          {single ? <ActivityIcon group={single} /> : <Layers aria-hidden="true" />}
        </span>
        <span className="min-w-0 truncate font-medium">{batchTitle(group.calls)}</span>
        <span
          className={cn(
            "ml-auto flex shrink-0 items-center gap-1 text-xs tabular-nums text-muted-foreground [&_svg]:size-3",
            failed > 0 && !active && "text-destructive",
          )}
        >
          {active
            ? waiting
              ? <><Clock3 aria-hidden="true" />waiting</>
              : <><LoaderCircle className="motion-safe:animate-spin" aria-hidden="true" />{completed} of {group.calls.length}</>
            : failed > 0
              ? <><CircleAlert aria-hidden="true" />{failed} failed</>
              : durationMs !== undefined ? formatDuration(durationMs) : null}
        </span>
      </div>
      <div className="flex flex-col px-1 pb-1">
        {group.calls.map((call) => (
          <ToolCallRow key={call.callId} call={call} waiting={waiting} loadFullText={loadFullText} />
        ))}
      </div>
    </div>
  );
}

const BATCH_NOUNS: Record<string, string> = {
  Read: "files", Write: "files", Edit: "files", Patch: "files", List: "directories",
  Search: "searches", Find: "searches", Fetch: "pages", Run: "commands",
  Delegate: "agents", Spawn: "agents", Emit: "events", Send: "messages",
};

/// "Read 3 files", "Run 2 commands", or "3 tool calls" for a mixed batch.
export function batchTitle(calls: TranscriptToolCall[]): string {
  const verbs = new Set(calls.map((call) => call.display?.verb ?? call.toolName));
  if (verbs.size === 1) {
    const verb = [...verbs][0]!;
    const noun = BATCH_NOUNS[verb];
    if (noun) return `${verb} ${calls.length} ${noun}`;
    return `${verb} × ${calls.length}`;
  }
  return `${calls.length} tool calls`;
}

function ToolCallRow({
  call,
  waiting,
  loadFullText,
}: {
  call: TranscriptToolCall;
  waiting: boolean;
  loadFullText?: FullTextLoader;
}) {
  const links = useContext(TranscriptLinksContext);
  const display = call.display;
  const group = display?.group ?? "other";
  const style = groupStyle(group);
  const failed = isFailedToolCall(call);
  const cancelled = call.status === "cancelled";
  const terminal = isTerminalToolStatus(call.status);
  const running = !terminal && !waiting;
  const elapsed = useElapsed(call.startedAtMs, running);

  const Icon = running ? LoaderCircle : callIcon(call);
  const verb = call.continuation ? "Tool activity" : (display?.verb ?? call.toolName);
  const rawTarget = display?.target ?? toolTarget(call.argumentsJson);
  const target = rawTarget ? presentTarget(call, rawTarget, links) : null;
  const detail = call.continuation ? "started before the loaded history" : display?.detail;
  const childSession = group === "agent" ? subagentSessionId(call.output) : null;
  const childHref = childSession && links.sessionHref ? links.sessionHref(childSession) : null;

  let right: ReactNode = null;
  if (cancelled) {
    right = <><CircleMinus aria-label="Cancelled" />cancelled</>;
  } else if (failed) {
    right = <><CircleAlert aria-label="Failed" />failed{call.durationMs !== undefined ? ` · ${formatDuration(call.durationMs)}` : ""}</>;
  } else if (waiting && !terminal) {
    right = <><Clock3 aria-label="Waiting" />waiting</>;
  } else if (running) {
    right = elapsed !== undefined ? formatDuration(elapsed) : null;
  } else if (call.durationMs !== undefined) {
    right = formatDuration(call.durationMs);
  }

  const errorLine = failed ? firstLine(call.error ?? call.output) : null;

  return (
    <StepRow
      icon={<Icon className={cn(running && "motion-safe:animate-spin")} />}
      tone={running ? "text-amber-600 dark:text-amber-300" : style.text}
      verb={verb}
      target={target}
      targetTitle={rawTarget ?? undefined}
      detail={detail}
      right={right}
      failed={failed}
      cancelled={cancelled}
      ariaLabel={`${verb}${rawTarget ? ` ${rawTarget}` : ""}`}
      extra={childHref && (
        <a
          href={childHref}
          onClick={(event) => {
            event.stopPropagation();
            if (links.navigate) {
              event.preventDefault();
              links.navigate(childHref);
            }
          }}
          className="flex shrink-0 items-center gap-1 text-xs text-primary underline underline-offset-4 [&_svg]:size-3"
        >
          <ExternalLink aria-hidden="true" />open session
        </a>
      )}
      note={errorLine && (
        <p className="px-2 pb-1 pl-[1.875rem] font-mono text-[11px] leading-4 text-destructive [overflow-wrap:anywhere]">
          {errorLine}
        </p>
      )}
    >
      <ToolCallDetail call={call} loadFullText={loadFullText} />
    </StepRow>
  );
}

/// Emit rows name the peer bot; file rows shorten long paths.
function presentTarget(call: TranscriptToolCall, target: string, links: TranscriptLinks): string {
  const display = call.display;
  if (display?.group === "bot" && display.verb === "Emit" && target.startsWith("to ") && target !== "to self") {
    const botId = target.slice(3);
    return `to ${links.botName?.(botId) ?? botId}`;
  }
  if (display?.group === "explore" || display?.group === "edit") return shortenTarget(target);
  return target;
}

/// A sub-agent result envelope names the child session; the Delegate row
/// links to it.
export function subagentSessionId(output: string | null | undefined): string | null {
  if (!output) return null;
  try {
    const parsed: unknown = JSON.parse(output);
    if (!parsed || typeof parsed !== "object") return null;
    const record = parsed as Record<string, unknown>;
    const id = record.session_id ?? record.sessionId;
    return typeof id === "string" && id ? id : null;
  } catch {
    return null;
  }
}

function ToolCallDetail({
  call,
  loadFullText,
}: {
  call: TranscriptToolCall;
  loadFullText?: FullTextLoader;
}) {
  const input = call.argumentsJson ? prettyJson(call.argumentsJson) : null;
  const output = call.error || call.output || null;
  const effects = call.effects?.length ? JSON.stringify(call.effects, null, 2) : null;
  const defaultTab = output ? "output" : input ? "input" : "effects";
  const failed = isFailedToolCall(call);

  return (
    <div className="mb-1.5 min-w-0 max-w-full space-y-1.5 pl-[1.875rem] pr-2">
      {call.media?.length ? <MediaStrip items={call.media} className="pt-1" /> : null}
      {input || output || effects ? (
        <Tabs defaultValue={defaultTab} className="min-w-0 max-w-full gap-1.5">
          <TabsList variant="line" className="h-6 text-xs">
            {input && <TabsTrigger value="input" className="text-xs">Arguments</TabsTrigger>}
            {output && <TabsTrigger value="output" className="text-xs">{failed ? "Error" : "Result"}</TabsTrigger>}
            {effects && <TabsTrigger value="effects" className="text-xs">Effects</TabsTrigger>}
          </TabsList>
          {input && (
            <TabsContent value="input" className="min-w-0 max-w-full">
              <DetailBlock value={input} />
            </TabsContent>
          )}
          {output && (
            <TabsContent value="output" className="min-w-0 max-w-full">
              <ExpandableContent
                text={output}
                truncated={call.outputTruncated}
                blobRef={call.outputContentRef}
                loadFullText={loadFullText}
              >
                {(text) => <DetailBlock value={text} tone={failed ? "warning" : "default"} />}
              </ExpandableContent>
            </TabsContent>
          )}
          {effects && (
            <TabsContent value="effects" className="min-w-0 max-w-full">
              <DetailBlock value={effects} />
            </TabsContent>
          )}
        </Tabs>
      ) : (
        <p className="text-xs text-muted-foreground">Waiting for tool details…</p>
      )}
      <ToolCallMeta call={call} />
    </div>
  );
}

/// What a debugger needs and a reader can ignore: raw tool name, call id,
/// timing, output size.
function ToolCallMeta({ call }: { call: TranscriptToolCall }) {
  const items: string[] = [call.toolName];
  if (call.toolId && call.toolId !== call.toolName) items.push(call.toolId);
  items.push(call.callId);
  if (call.startedAtMs !== undefined) items.push(`started ${clockTime(call.startedAtMs)}`);
  if (call.durationMs !== undefined) items.push(formatDuration(call.durationMs));
  if (call.outputBytes !== undefined) items.push(formatBytes(call.outputBytes));
  if (call.outputTruncated) items.push("truncated");
  return (
    <p
      className="flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[10px] leading-4 text-muted-foreground [overflow-wrap:anywhere]"
      aria-label="Call details"
    >
      {items.map((item, index) => <span key={index}>{item}</span>)}
    </p>
  );
}

function clockTime(ms: number): string {
  const date = new Date(ms);
  const pad = (value: number, width = 2) => String(value).padStart(width, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function DetailBlock({ value, tone = "default" }: { value: string; tone?: "default" | "warning" }) {
  const [copied, setCopied] = useState(false);

  return (
    <div className="relative min-w-0 max-w-full">
      <pre
        className={cn(
          "max-h-96 w-full min-w-0 max-w-full overflow-auto rounded-md border bg-muted/30 p-3 pr-10 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-words",
          tone === "warning" && "border-destructive/20 bg-destructive/5",
        )}
      >
        {value}
      </pre>
      <Button
        variant="ghost"
        size="icon-xs"
        className="absolute right-2 top-2 bg-background/80"
        aria-label={copied ? "Copied" : "Copy details"}
        title={copied ? "Copied" : "Copy details"}
        onClick={() => {
          void navigator.clipboard.writeText(value).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1_500);
          }).catch(() => undefined);
        }}
      >
        {copied ? <Check /> : <Copy />}
      </Button>
    </div>
  );
}

function reasoningTitle(text: string): string {
  const first = text.split("\n", 1)[0]?.trim() ?? "Reasoning";
  const bold = /^\*\*(.+?)\*\*$/.exec(first)?.[1];
  const heading = /^#{1,6}\s+(.+)$/.exec(first)?.[1];
  const title = bold ?? heading ?? first.replace(/^[-*]\s+/, "");
  return title.length > 120 ? `${title.slice(0, 117)}…` : title;
}

function prettyJson(value: string): string {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
}
