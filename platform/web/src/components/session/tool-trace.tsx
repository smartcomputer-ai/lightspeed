import { useMemo, useState } from "react";
import {
  Brain,
  Check,
  CheckCircle2,
  ChevronDown,
  Circle,
  CircleAlert,
  CircleMinus,
  Clock3,
  Copy,
  Loader2,
  PencilLine,
  Search,
  TerminalSquare,
  Wrench,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Separator } from "@/components/ui/separator";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { MarkdownContent } from "@/components/session/markdown-content";
import {
  isFailedToolCall,
  isTerminalToolStatus,
  toolTarget,
  type TranscriptToolCall,
  type TranscriptToolGroup,
  type TranscriptToolGroupStatus,
} from "@/lib/sessions/transcript";
import { cn } from "@/lib/utils";

export function ReasoningTrace({ text }: { text: string }) {
  const [open, setOpen] = useState(false);

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <div className="min-w-0 max-w-full overflow-hidden rounded-lg border border-dashed bg-muted/20">
        <CollapsibleTrigger className="flex w-full items-center gap-3 px-3 py-2.5 text-left outline-none transition-colors hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring/50">
          <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
            <Brain className="size-4" />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              Thinking
            </span>
            <span className="block truncate text-sm text-muted-foreground">
              {reasoningTitle(text)}
            </span>
          </span>
          <ChevronDown
            className={cn(
              "size-4 shrink-0 text-muted-foreground transition-transform",
              open && "rotate-180",
            )}
          />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <Separator />
          <div className="px-4 py-3 text-muted-foreground/80">
            <MarkdownContent className="text-xs italic">{text}</MarkdownContent>
          </div>
        </CollapsibleContent>
      </div>
    </Collapsible>
  );
}

export function ToolGroupTrace({ group }: { group: TranscriptToolGroup }) {
  const active = !isTerminalToolStatus(group.status);
  const failedCalls = group.calls.filter(isFailedToolCall).length;
  const [open, setOpen] = useState(false);

  return (
    <Card
      size="sm"
      className={cn(
        "min-w-0 max-w-full gap-0 py-0 shadow-none",
        active && "ring-amber-500/30",
      )}
    >
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleTrigger className="w-full px-3 py-3 text-left outline-none transition-colors hover:bg-muted/30 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/50">
          <span className="flex items-center gap-3">
            <ToolGroupIcon calls={group.calls} />
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium">
                {toolGroupTitle(group)}
              </span>
              {group.calls.length > 1 && (
                <span className="mt-1.5 flex flex-col gap-1">
                  {group.calls.map((call) => (
                    <ToolActivity key={call.callId} call={call} />
                  ))}
                </span>
              )}
            </span>
            <GroupStatusBadge group={group} failedCalls={failedCalls} />
            <ChevronDown
              className={cn(
                "size-4 shrink-0 text-muted-foreground transition-transform",
                open && "rotate-180",
              )}
            />
          </span>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <Separator />
          <div className="min-w-0 divide-y">
            {group.calls.map((call) => (
              <ToolCallDetails key={call.callId} call={call} />
            ))}
          </div>
        </CollapsibleContent>
      </Collapsible>
    </Card>
  );
}

function ToolActivity({ call }: { call: TranscriptToolCall }) {
  const display = call.display;
  const target = display?.target ?? toolTarget(call.argumentsJson);
  const verb = display?.verb ?? call.toolName;
  return (
    <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
      <ActivityIcon group={display?.group} className="size-3 shrink-0" />
      <span className={cn("shrink-0 font-medium", activityColor(display?.group))}>
        {verb}
      </span>
      {target && <span className="min-w-0 flex-1 truncate font-mono text-[11px]">{target}</span>}
      {display?.detail && <span className="min-w-0 truncate">{display.detail}</span>}
      <ToolCallStateIcon call={call} />
    </span>
  );
}

function ToolCallDetails({ call }: { call: TranscriptToolCall }) {
  const input = call.argumentsJson ? prettyJson(call.argumentsJson) : null;
  const output = call.error || call.output || null;
  const effects = call.effects?.length ? JSON.stringify(call.effects, null, 2) : null;
  const defaultTab = output ? "output" : input ? "input" : "effects";
  const failed = isFailedToolCall(call);

  return (
    <div className="min-w-0 max-w-full space-y-3 px-4 py-3">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <span className="max-w-full font-mono text-xs font-medium [overflow-wrap:anywhere]">
          {call.toolName}
        </span>
        <StatusBadge status={call.status} failed={failed} />
        <span className="ml-auto truncate font-mono text-[10px] text-muted-foreground">
          {call.callId}
        </span>
      </div>
      {call.display && (
        <p className="max-w-full text-xs text-muted-foreground [overflow-wrap:anywhere]">
          <span className={cn("font-medium", activityColor(call.display.group))}>
            {call.display.verb}
          </span>
          {call.display.target ? ` ${call.display.target}` : ""}
          {call.display.detail ? ` · ${call.display.detail}` : ""}
        </p>
      )}
      {input || output || effects ? (
        <Tabs defaultValue={defaultTab} className="min-w-0 max-w-full">
          <TabsList variant="line" className="h-7">
            {input && <TabsTrigger value="input">Arguments</TabsTrigger>}
            {output && <TabsTrigger value="output">{failed ? "Error" : "Result"}</TabsTrigger>}
            {effects && <TabsTrigger value="effects">Effects</TabsTrigger>}
          </TabsList>
          {input && (
            <TabsContent value="input" className="min-w-0 max-w-full">
              <DetailBlock value={input} />
            </TabsContent>
          )}
          {output && (
            <TabsContent value="output" className="min-w-0 max-w-full">
              <DetailBlock value={output} tone={failed ? "warning" : "default"} />
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
    </div>
  );
}

function DetailBlock({ value, tone = "default" }: { value: string; tone?: "default" | "warning" }) {
  const [copied, setCopied] = useState(false);

  return (
    <div className="relative min-w-0 max-w-full">
      <pre
        className={cn(
          "max-h-96 w-full min-w-0 max-w-full overflow-auto rounded-md border bg-muted/40 p-3 pr-10 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-words",
          tone === "warning" && "border-amber-500/20 bg-amber-500/5",
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

function ToolGroupIcon({ calls }: { calls: TranscriptToolCall[] }) {
  const groups = useMemo(() => new Set(calls.map((call) => call.display?.group)), [calls]);
  const group = groups.size === 1 ? calls[0]?.display?.group : "other";
  return (
    <span
      className={cn(
        "flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground",
        group === "explore" && "bg-cyan-500/10 text-cyan-600 dark:text-cyan-300",
        group === "edit" && "bg-amber-500/10 text-amber-600 dark:text-amber-300",
        group === "execute" && "bg-blue-500/10 text-blue-600 dark:text-blue-300",
      )}
    >
      <ActivityIcon group={group} className="size-4" />
    </span>
  );
}

function ToolCallStateIcon({ call }: { call: TranscriptToolCall }) {
  if (isFailedToolCall(call)) {
    return <CircleAlert aria-label="Failed" className="ml-auto size-3 shrink-0" />;
  }
  if (call.status === "cancelled") {
    return <CircleMinus aria-label="Cancelled" className="ml-auto size-3 shrink-0" />;
  }
  if (isTerminalToolStatus(call.status)) {
    return (
      <CheckCircle2
        aria-label="Completed"
        className="ml-auto size-3 shrink-0 text-emerald-600 dark:text-emerald-300"
      />
    );
  }
  return (
    <Circle
      aria-label="In progress"
      className="ml-auto size-2.5 shrink-0 fill-amber-500 text-amber-500"
    />
  );
}

function ActivityIcon({ group, className }: { group?: string | null; className?: string }) {
  if (group === "explore") return <Search className={className} />;
  if (group === "edit") return <PencilLine className={className} />;
  if (group === "execute") return <TerminalSquare className={className} />;
  return <Wrench className={className} />;
}

function StatusBadge({
  status,
  failed = false,
}: {
  status: TranscriptToolGroupStatus;
  failed?: boolean;
}) {
  if (failed) {
    return (
      <Badge variant="outline" className="text-muted-foreground">
        <CircleAlert />
        Failed
      </Badge>
    );
  }
  if (status === "cancelled") {
    return (
      <Badge variant="outline" className="text-muted-foreground">
        <CircleMinus />
        Cancelled
      </Badge>
    );
  }
  if (!isTerminalToolStatus(status)) {
    return (
      <Badge variant="outline" className="border-amber-500/30 text-amber-700 dark:text-amber-300">
        {status === "waiting" ? <Clock3 /> : <Circle className="fill-current" />}
        {statusLabel(status)}
      </Badge>
    );
  }
  return (
    <Badge variant="outline" className="border-emerald-500/30 text-emerald-700 dark:text-emerald-300">
      <CheckCircle2 />
      {statusLabel(status)}
    </Badge>
  );
}

function GroupStatusBadge({
  group,
  failedCalls,
}: {
  group: TranscriptToolGroup;
  failedCalls: number;
}) {
  if (!isTerminalToolStatus(group.status)) {
    const completed = group.calls.filter((call) => isTerminalToolStatus(call.status)).length;
    const waiting = group.status === "waiting";
    return (
      <Badge
        variant="outline"
        className="border-amber-500/30 text-amber-700 dark:text-amber-300"
        aria-label={waiting
          ? "Waiting for tool calls"
          : `${completed} of ${group.calls.length} tool calls complete`}
      >
        {waiting ? <Clock3 /> : <Loader2 className="motion-safe:animate-spin" />}
        {waiting
          ? "Waiting"
          : group.calls.length === 1
            ? "In progress"
            : `${completed}/${group.calls.length}`}
      </Badge>
    );
  }
  if (failedCalls > 0) {
    return (
      <Badge variant="outline" className="text-muted-foreground">
        <CircleAlert />
        {failedCalls} failed
      </Badge>
    );
  }
  if (group.status === "cancelled") {
    return (
      <Badge variant="outline" className="text-muted-foreground">
        <CircleMinus />
        Cancelled
      </Badge>
    );
  }
  return <StatusBadge status={group.status} />;
}

function toolGroupTitle(group: TranscriptToolGroup): string {
  if (group.calls.length !== 1) {
    return "Tool calls";
  }
  const call = group.calls[0];
  if (!call) {
    return "Tool call";
  }
  const display = call.display;
  const verb = display?.verb ?? call.toolName;
  const target = display?.target ?? toolTarget(call.argumentsJson);
  return [verb, target, display?.detail].filter(Boolean).join(" ");
}

function statusLabel(status: TranscriptToolGroupStatus): string {
  if (status === "succeeded" || status === "completedWithErrors") return "Done";
  if (status === "requested" || status === "running") return "In progress";
  if (status === "cancelled") return "Cancelled";
  return status.charAt(0).toUpperCase() + status.slice(1);
}

function activityColor(group: string | null | undefined): string {
  if (group === "explore") return "text-cyan-700 dark:text-cyan-300";
  if (group === "edit") return "text-amber-700 dark:text-amber-300";
  if (group === "execute") return "text-blue-700 dark:text-blue-300";
  return "text-foreground/75";
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
