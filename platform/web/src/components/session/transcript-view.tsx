import { useContext, useId, useLayoutEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Check, Inbox, Loader2, ShieldQuestion, TriangleAlert, X } from "lucide-react";
import type { PendingApprovalView } from "@lightspeed-ai/agent-client";
import { BotIcon } from "@/components/icons/bot";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
import { Button } from "@/components/ui/button";
import { Marker, MarkerContent, MarkerIcon } from "@/components/ui/marker";
import { Message, MessageContent } from "@/components/ui/message";
import { MarkdownContent } from "@/components/session/markdown-content";
import { RunOutcomeLine } from "@/components/session/run-stats";
import type { FullTextLoader } from "@/components/session/expandable-content";
import { ReasoningTrace, ToolGroupTrace } from "@/components/session/tool-trace";
import { TranscriptLinksContext } from "@/components/session/transcript-links";
import { type TranscriptEntry, type TranscriptMedia } from "@/lib/sessions/transcript";
import { MediaStrip } from "@/components/session/media";
import { cn } from "@/lib/utils";

/// Full-width transcript rows without avatars. User inputs use muted bands;
/// assistant output is plain rendered text, with compact tool and lifecycle markers.

export function TranscriptEntryView({
  entry,
  loadFullText,
  showRunStatistics = true,
}: {
  entry: TranscriptEntry;
  loadFullText?: FullTextLoader;
  showRunStatistics?: boolean;
}) {
  switch (entry.kind) {
    case "message":
      return entry.role === "user" ? (
        <UserBand
          text={entry.text}
          origin={entry.origin}
          steering={entry.steering === true}
          media={entry.media}
        />
      ) : (
        <Message>
          <MessageContent>
            <Bubble variant="ghost" className="max-w-full">
              <BubbleContent>
                <MarkdownContent>{entry.text}</MarkdownContent>
                {entry.citations?.length ? (
                  <div className="mt-3 flex flex-wrap gap-2 border-t pt-3 pb-1 text-xs text-muted-foreground">
                    <span className="font-medium">Sources</span>
                    {entry.citations.map((citation, index) => (
                      <a
                        key={`${citation.url}:${index}`}
                        href={citation.url}
                        target="_blank"
                        rel="noreferrer"
                        title={citation.citedText ?? undefined}
                        className="text-primary underline underline-offset-4"
                      >
                        {citation.title || citationHost(citation.url)}
                      </a>
                    ))}
                  </div>
                ) : null}
              </BubbleContent>
            </Bubble>
          </MessageContent>
        </Message>
      );
    case "system":
      return <SystemChips entries={[entry]} />;
    case "reasoning":
      return <ReasoningTrace text={entry.text} />;
    case "tool-group":
      return <ToolGroupTrace group={entry} loadFullText={loadFullText} />;
    case "run-summary":
      return <RunOutcomeLine summary={entry} showStatistics={showRunStatistics} />;
    case "marker":
      return entry.tone === "error" ? (
        <Marker className="text-destructive">
          <MarkerIcon>
            <TriangleAlert />
          </MarkerIcon>
          <MarkerContent>{entry.text}</MarkerContent>
        </Marker>
      ) : (
        <Marker variant="separator">
          <MarkerContent>{entry.text}</MarkerContent>
        </Marker>
      );
  }
}

/// Context entries that are not conversation: instructions and catalog
/// versions, as small chips on one row. A superseded version stays dimmed.
export function SystemChips({ entries }: { entries: Extract<TranscriptEntry, { kind: "system" }>[] }) {
  return (
    <div className="flex min-w-0 flex-wrap gap-1.5 px-2 py-0.5" aria-label="Context updates">
      {entries.map((entry) => (
        <span
          key={entry.key}
          className={cn(
            "max-w-full truncate rounded-full border border-dashed px-2 text-[11px] leading-4 text-muted-foreground",
            entry.superseded && "opacity-50",
          )}
          title={entry.superseded ? `${entry.text} — superseded by a newer version` : entry.text}
        >
          {entry.text}
        </span>
      ))}
    </div>
  );
}

/// The delivered form of a bot event opens with one header line:
/// `── event #N · kind · source · time`. Split it off so the band can show
/// the sender and kind as a header instead of a rule of text.
export function parseEventPrompt(text: string): {
  seq: string | null;
  kind: string | null;
  source: string | null;
  time: string | null;
  body: string;
} | null {
  const newline = text.indexOf("\n");
  const first = (newline === -1 ? text : text.slice(0, newline)).trim();
  if (!first.startsWith("── event #")) return null;
  const parts = first.slice(2).trim().split(" · ");
  return {
    seq: parts[0]?.replace(/^event #/, "") || null,
    kind: parts[1] ?? null,
    source: parts[2] ?? null,
    time: parts[3] ?? null,
    body: newline === -1 ? "" : text.slice(newline + 1).replace(/^\n+/, ""),
  };
}

function citationHost(url: string): string {
  try {
    return new URL(url).hostname || url;
  } catch {
    return url;
  }
}

// 160px of text plus padding and the expansion control keeps a collapsed band
// around 200px tall. Measure natural content so wrapping and font changes count.
const COLLAPSED_TEXT_HEIGHT = 160;

export function UserBand({
  text,
  origin,
  pending = false,
  steering = false,
  media,
}: {
  text: string;
  /// Application-supplied origin of the input; `event` marks a delivered bot
  /// event, which gets a sender header instead of a plain band.
  origin?: string;
  pending?: boolean;
  /// A message injected into a running run rather than its initial input.
  steering?: boolean;
  /// Images and documents sent with the input.
  media?: TranscriptMedia[];
}) {
  const contentId = useId();
  const contentRef = useRef<HTMLDivElement>(null);
  const [overflowing, setOverflowing] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const links = useContext(TranscriptLinksContext);
  const event = origin === "event" ? parseEventPrompt(text) : null;
  const body = event ? event.body : text;

  useLayoutEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    const measure = () => setOverflowing(content.scrollHeight > COLLAPSED_TEXT_HEIGHT);
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(content);
    return () => observer.disconnect();
  }, [body, steering]);

  return (
    <Message>
      <MessageContent>
        <Bubble
          variant="muted"
          className={cn(
            "w-full max-w-full",
            pending && "opacity-60",
            event && "*:data-[slot=bubble-content]:border-l-2 *:data-[slot=bubble-content]:border-l-teal-600/60 *:data-[slot=bubble-content]:rounded-l-sm dark:*:data-[slot=bubble-content]:border-l-teal-300/50",
          )}
        >
          <BubbleContent className="w-full">
            {event && (
              <div className="mb-1.5 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
                {event.source?.startsWith("bot:")
                  ? <BotIcon size={14} className="shrink-0 text-teal-700 dark:text-teal-300" />
                  : <Inbox className="size-3.5 shrink-0 text-teal-700 dark:text-teal-300" />}
                <span className="font-medium text-foreground">{eventSourceLabel(event.source, links.botName)}</span>
                {event.kind && <><span aria-hidden="true">·</span><span className="font-mono text-[11px]">{event.kind}</span></>}
                {event.seq && <><span aria-hidden="true">·</span><span>#{event.seq}</span></>}
                {event.time && <span className="ml-auto tabular-nums">{event.time}</span>}
              </div>
            )}
            <div
              id={contentId}
              className="overflow-hidden"
              style={{
                maxHeight: expanded ? undefined : COLLAPSED_TEXT_HEIGHT,
                maskImage: overflowing && !expanded
                  ? "linear-gradient(to bottom, black calc(100% - 48px), transparent)"
                  : undefined,
              }}
            >
              <div ref={contentRef} className="whitespace-pre-wrap [overflow-wrap:anywhere]">
                {steering && (
                  <span
                    className="mr-2 rounded-sm border border-current/25 px-1 py-px align-middle text-[10px] uppercase tracking-wide opacity-70"
                    title="Sent into the running run; the agent saw it at its next turn"
                  >
                    steer
                  </span>
                )}
                {body}
              </div>
            </div>
            {media?.length ? <MediaStrip items={media} className={cn(body ? "mt-2" : "")} /> : null}
            {overflowing && (
              <div className="mt-1 flex justify-center">
                <button
                  type="button"
                  data-message-expansion
                  aria-expanded={expanded}
                  aria-controls={contentId}
                  onClick={() => setExpanded((value) => !value)}
                  className={cn(
                    "inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs font-medium opacity-80 transition-colors hover:opacity-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-current",
                    "hover:bg-foreground/5",
                  )}
                >
                  {expanded ? "Show less" : "Show more"}
                  {expanded ? <ChevronUp className="size-3.5" /> : <ChevronDown className="size-3.5" />}
                </button>
              </div>
            )}
          </BubbleContent>
        </Bubble>
      </MessageContent>
    </Message>
  );
}

/// "Support Triage" for `bot:support-triage`, "github webhook" for
/// `webhook:github`; anything else stays as delivered.
function eventSourceLabel(
  source: string | null,
  botName: ((botId: string) => string | undefined) | undefined,
): string {
  if (!source) return "Event";
  const colon = source.indexOf(":");
  if (colon === -1) return source;
  const family = source.slice(0, colon);
  const name = source.slice(colon + 1);
  if (family === "bot") return botName?.(name) ?? name;
  if (["webhook", "schedule", "poll", "chat"].includes(family)) return `${name} ${family}`;
  return source;
}

export interface QueuedRunItem {
  /// Stable identity across the optimistic → confirmed transition (the
  /// client submission id when known), so the row is updated, not remounted.
  key: string;
  runId: string | null;
  text: string;
  /// Still being submitted or awaiting the engine's acknowledgement.
  pending?: boolean;
  /// A cancel is in flight for this queued run.
  cancelling?: boolean;
}

/// Messages queued behind the active run, in start order, each with a
/// cancel control. Sits between the transcript and the composer.
export function QueuedRunsBar({
  items,
  onCancel,
}: {
  items: QueuedRunItem[];
  onCancel: (runId: string) => void;
}) {
  if (items.length === 0) {
    return null;
  }
  return (
    <div className="shrink-0 border-t bg-muted/40" aria-label="Queued messages">
      <div className="mx-auto w-full max-w-5xl px-4 py-2 md:px-8">
        <p className="pb-1 text-[10px] uppercase tracking-wide text-muted-foreground">
          Queued — starts after the current run
        </p>
        <ul className="flex flex-col gap-1">
        {items.map((item, index) => (
          <li
            key={item.key}
            className={cn(
              "flex items-center gap-2 text-sm",
              (item.pending || item.cancelling) && "opacity-60",
            )}
          >
            <span className="w-5 shrink-0 text-right text-xs text-muted-foreground">
              {index + 1}.
            </span>
            <span className="min-w-0 flex-1 truncate whitespace-pre-wrap" title={item.text}>
              {item.text}
            </span>
            {item.cancelling ? (
              <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" />
            ) : (
              <Button
                variant="ghost"
                size="icon-xs"
                disabled={!item.runId}
                aria-label="Cancel queued message"
                title={item.runId ? "Remove from the queue" : "Waiting for the engine to accept this message"}
                onClick={() => item.runId && onCancel(item.runId)}
              >
                <X />
              </Button>
            )}
          </li>
        ))}
        </ul>
      </div>
    </div>
  );
}

export function ApprovalCards({
  approvals,
  deciding,
  error,
  onDecide,
}: {
  approvals: PendingApprovalView[];
  deciding: { approvalId: string; decision: "approve" | "reject" } | null;
  error: { approvalId: string; message: string } | null;
  onDecide: (approvalId: string, decision: "approve" | "reject") => void;
}) {
  if (approvals.length === 0) return null;
  return (
    <div className="flex flex-col gap-3" aria-label="Pending approvals">
      {approvals.map((approval) => {
        const subject = approval.subject;
        const busy = deciding?.approvalId === approval.approvalId;
        return (
          <section
            key={approval.approvalId}
            className="rounded-lg border border-amber-500/35 bg-amber-500/5 p-4"
          >
            <div className="flex items-start gap-3">
              <ShieldQuestion className="mt-0.5 size-4 shrink-0 text-amber-600" />
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium">Approve MCP tool call?</p>
                <p className="mt-1 text-xs text-muted-foreground">
                  <span className="font-medium text-foreground">{subject.toolName}</span>
                  {` on ${subject.serverLabel}`}
                </p>
                <pre className="mt-3 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md bg-muted/70 p-3 font-mono text-xs">
                  {subject.argumentsPreview}
                </pre>
                <div className="mt-3 flex items-center gap-2">
                  <Button
                    size="sm"
                    disabled={deciding !== null}
                    onClick={() => onDecide(approval.approvalId, "approve")}
                  >
                    {busy && deciding?.decision === "approve" ? (
                      <Loader2 className="animate-spin" />
                    ) : (
                      <Check />
                    )}
                    Approve
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={deciding !== null}
                    onClick={() => onDecide(approval.approvalId, "reject")}
                  >
                    {busy && deciding?.decision === "reject" ? (
                      <Loader2 className="animate-spin" />
                    ) : (
                      <X />
                    )}
                    Reject
                  </Button>
                  <span className="ml-auto font-mono text-[10px] text-muted-foreground">
                    {approval.approvalId}
                  </span>
                </div>
                {error?.approvalId === approval.approvalId && (
                  <p className="mt-2 text-xs text-destructive">{error.message}</p>
                )}
              </div>
            </div>
          </section>
        );
      })}
    </div>
  );
}
