import { useMemo, useState } from "react";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, RotateCcw, Webhook } from "lucide-react";
import { Link } from "react-router-dom";
import {
  api,
  type BotActiveDeliverySnapshot,
  type BotControllerSnapshot,
  type BotEventListResponse,
  type BotEventOutcome,
  type BotEventView,
  type BotRecentDeliverySnapshot,
  type BotStateView,
  type BotView,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import { SendEventDialog } from "./send-event-dialog";

type OutcomeFilter = "all" | "pending" | "handled" | "ignored" | "deferred" | "failed" | "system";

const OUTCOME_FILTERS: Array<{ value: OutcomeFilter; label: string }> = [
  { value: "all", label: "All outcomes" },
  { value: "pending", label: "Pending" },
  { value: "handled", label: "Handled" },
  { value: "ignored", label: "Ignored" },
  { value: "deferred", label: "Deferred" },
  { value: "failed", label: "Failed or blocked" },
  { value: "system", label: "Steered, appended, archived" },
];

export function matchesOutcomeFilter(
  outcome: BotEventOutcome | null | undefined,
  filter: OutcomeFilter,
): boolean {
  switch (filter) {
    case "all":
      return true;
    case "pending":
      return outcome == null || outcome === "unresolved";
    case "failed":
      return outcome === "run_failed" || outcome === "blocked";
    case "system":
      return outcome === "steered" || outcome === "appended" || outcome === "archived";
    default:
      return outcome === filter;
  }
}

/**
 * What the bot did: one live timeline of numbered events with their outcome
 * and the bot's own one-line summary, under a strip that answers "now" and
 * "today". Replay and the test event live here, next to the history they
 * add to.
 */
export function BotActivity({
  universeId,
  slug,
  bot,
  state,
  stateError,
  manage,
  invoke,
}: {
  universeId: string;
  slug: string;
  bot: BotView;
  state?: BotStateView;
  stateError?: string;
  manage: boolean;
  invoke: boolean;
}) {
  const queryClient = useQueryClient();
  const [eventOpen, setEventOpen] = useState(false);
  const [outcomeFilter, setOutcomeFilter] = useState<OutcomeFilter>("all");
  const [search, setSearch] = useState("");
  const base = `/u/${slug}/bots/${bot.botId}`;
  const controller = state?.controller ?? undefined;
  const pages = useInfiniteQuery({
    queryKey: ["bot-events", universeId, bot.botId],
    queryFn: ({ pageParam }) =>
      api<BotEventListResponse>(
        "GET",
        `/api/v1/universes/${universeId}/bots/${bot.botId}/events?limit=50${
          pageParam ? `&cursor=${encodeURIComponent(pageParam)}` : ""
        }`,
      ),
    initialPageParam: "",
    getNextPageParam: (last) => last.nextCursor ?? undefined,
    refetchInterval: 5_000,
    refetchIntervalInBackground: false,
  });
  const replay = useMutation({
    mutationFn: (seq: number) =>
      api("POST", `/api/v1/universes/${universeId}/bots/${bot.botId}/events/replay`, { seq }),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-events", universeId, bot.botId] }),
      ]);
    },
  });
  const events = pages.data?.pages.flatMap((page) => page.events ?? []) ?? [];
  const needle = search.trim().toLowerCase();
  const visible = useMemo(
    () =>
      events.filter(
        (event) =>
          matchesOutcomeFilter(event.outcome, outcomeFilter) &&
          (needle === "" ||
            `${event.kind} ${event.triggerId ?? ""} ${event.outcomeDetail ?? ""} ${event.senderBotId ?? ""} ${event.session?.label ?? ""} ${event.summary}`
              .toLowerCase()
              .includes(needle)),
      ),
    [events, outcomeFilter, needle],
  );
  // Live controller state fills in rows whose stored outcome is still null
  // (a delivery in flight); the stored outcome wins once written.
  const activeBySeq = new Map<number, BotActiveDeliverySnapshot>();
  for (const delivery of controller?.activeDeliveries ?? []) {
    for (const seq of delivery.seqs) activeBySeq.set(seq, delivery);
  }
  const recentBySeq = new Map<number, BotRecentDeliverySnapshot>();
  for (const delivery of controller?.recentDeliveries ?? []) {
    for (const seq of delivery.seqs) recentBySeq.set(seq, delivery);
  }
  const sessionHref = (sessionId: string) =>
    sessionId === controller?.mainSessionId ? base : `${base}/chat/${encodeURIComponent(sessionId)}`;

  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-x-hidden overflow-y-auto">
      <div className="mx-auto grid w-full min-w-0 max-w-5xl gap-5 px-4 py-5 text-sm md:px-8">
        <NowStrip bot={bot} controller={controller} stateError={stateError} sessionHref={sessionHref} />
        <div className="flex flex-wrap items-center gap-2">
          <Select value={outcomeFilter} onValueChange={(value) => value && setOutcomeFilter(value as OutcomeFilter)}>
            <SelectTrigger size="sm" className="w-48">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {OUTCOME_FILTERS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Filter by kind, trigger, thread, or summary"
            className="h-8 w-64 text-xs"
          />
          <span className="text-xs text-muted-foreground">
            {visible.length}
            {pages.hasNextPage ? "+" : ""} of {events.length}
            {pages.hasNextPage ? "+" : ""} loaded
          </span>
          {invoke && bot.closedAtMs == null && (
            <Button variant="outline" size="xs" className="ml-auto" onClick={() => setEventOpen(true)}>
              <Webhook data-icon="inline-start" /> Send a test event
            </Button>
          )}
        </div>
        <div className="grid gap-1.5">
          {pages.isLoading && <p className="text-xs text-muted-foreground">Loading events…</p>}
          {pages.error && <p className="text-xs text-destructive">{pages.error.message}</p>}
          {replay.error && <p className="text-xs text-destructive">{replay.error.message}</p>}
          {visible.map((event) => (
            <EventRow
              key={event.seq}
              event={event}
              active={activeBySeq.get(event.seq)}
              recent={recentBySeq.get(event.seq)}
              sessionHref={sessionHref}
              onReplay={manage && bot.closedAtMs == null ? () => replay.mutate(event.seq) : undefined}
            />
          ))}
          {!pages.isLoading && !pages.error && events.length === 0 && (
            <p className="rounded-md border border-dashed p-4 text-center text-xs text-muted-foreground">
              Nothing has happened yet. Events arrive from the bot's triggers
              {invoke ? " — or send a test event to see it work." : "."}
            </p>
          )}
          {!pages.isLoading && events.length > 0 && visible.length === 0 && (
            <p className="text-xs text-muted-foreground">No loaded events match the filter.</p>
          )}
          {pages.hasNextPage && (
            <Button
              variant="outline"
              size="sm"
              className="w-full"
              disabled={pages.isFetchingNextPage}
              onClick={() => void pages.fetchNextPage()}
            >
              {pages.isFetchingNextPage ? "Loading…" : "Load older events"}
            </Button>
          )}
        </div>
      </div>
      {invoke && (
        <SendEventDialog universeId={universeId} botId={bot.botId} open={eventOpen} onOpenChange={setEventOpen} />
      )}
    </div>
  );
}

function NowStrip({
  bot,
  controller,
  stateError,
  sessionHref,
}: {
  bot: BotView;
  controller?: BotControllerSnapshot;
  stateError?: string;
  sessionHref: (sessionId: string) => string;
}) {
  if (stateError) {
    return (
      <p className="rounded-md bg-destructive/10 p-3 text-xs text-destructive">
        The controller is unavailable: {stateError}
      </p>
    );
  }
  if (!controller) return <p className="text-xs text-muted-foreground">Waiting for the controller…</p>;
  const activeDeliveries = controller.activeDeliveries ?? [];
  const buffers = controller.buffers ?? [];
  const pendingDeliveries = controller.pendingDeliveries ?? 0;
  const quiet = activeDeliveries.length === 0 && buffers.length === 0 && pendingDeliveries === 0;
  return (
    <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
      <Stat label="Now">
        {quiet ? (
          <span className="text-muted-foreground">Nothing in flight</span>
        ) : (
          <span className="grid gap-0.5">
            {activeDeliveries.map((delivery) => (
              <span key={delivery.deliveryId} className="truncate">
                Working on {delivery.seqs.length > 1 ? `${delivery.seqs.length} events` : "an event"}
                {" → "}
                <Link to={sessionHref(delivery.sessionId)} className="underline-offset-2 hover:underline">
                  {delivery.sessionId === controller.mainSessionId
                    ? "Main"
                    : ((controller.sessions ?? []).find((session) => session.sessionId === delivery.sessionId)?.label ?? "thread")}
                </Link>
              </span>
            ))}
            {pendingDeliveries > 0 && (
              <span className="text-muted-foreground">
                {pendingDeliveries} {pendingDeliveries === 1 ? "delivery" : "deliveries"} queued
              </span>
            )}
          </span>
        )}
      </Stat>
      <Stat label="Coalescing">
        {buffers.length === 0 ? (
          <span className="text-muted-foreground">No batches forming</span>
        ) : (
          <span className="grid gap-0.5">
            {buffers.map((buffer) => (
              <span key={buffer.key} className="truncate">
                {buffer.seqs.length} {buffer.seqs.length === 1 ? "event" : "events"} · flushes {flushLabel(buffer.flushAtMs)}
              </span>
            ))}
          </span>
        )}
      </Stat>
      <Stat label="Today">
        <span>
          {budgetLabel(bot.runsPerDay ?? null, controller)}
          <span className="block text-muted-foreground">{controller.eventsProcessed ?? 0} events processed in all</span>
        </span>
      </Stat>
      <Stat label="Errors" tone={controller.lastError ? "destructive" : undefined}>
        {controller.lastError ? (
          <span className="line-clamp-2 wrap-anywhere" title={controller.lastError}>
            {controller.lastError}
          </span>
        ) : (
          <span className="text-muted-foreground">None</span>
        )}
      </Stat>
    </div>
  );
}

function Stat({
  label,
  tone,
  children,
}: {
  label: string;
  tone?: "destructive";
  children: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "grid min-w-0 content-start gap-1 rounded-md border p-3 text-xs",
        tone === "destructive" && "border-destructive/40 text-destructive",
      )}
    >
      <span className="text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">{label}</span>
      {children}
    </div>
  );
}

function EventRow({
  event,
  active,
  recent,
  sessionHref,
  onReplay,
}: {
  event: BotEventView;
  /** The in-flight delivery carrying this event, from live controller state. */
  active?: BotActiveDeliverySnapshot;
  /** The finished delivery that carried it, while the stored outcome may lag. */
  recent?: BotRecentDeliverySnapshot;
  sessionHref: (sessionId: string) => string;
  onReplay?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const working = active !== undefined;
  const outcome = event.outcome ?? recent?.outcome ?? null;
  const detail = event.outcomeDetail ?? recent?.summary ?? event.summary;
  const runId = event.runId ?? recent?.runId ?? active?.runId;
  const batchSize = (active ?? recent)?.seqs.length ?? 1;
  const usage = recent?.usage;
  const hops = event.hops ?? 0;
  const chip = outcome ? outcome.replaceAll("_", " ") : working ? "working" : "pending";
  return (
    <div className={cn("rounded-md border text-xs", open && "bg-muted/30")}>
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        className="grid w-full grid-cols-[auto_minmax(0,1fr)_auto] items-baseline gap-x-2 px-3 py-2 text-left"
        aria-expanded={open}
      >
        <code className="w-12 shrink-0 text-muted-foreground" title={event.eventId}>
          #{event.seq}
        </code>
        <span className="min-w-0">
          <span className="block truncate">
            <span className="font-medium">{event.kind}</span>
            <span className="text-muted-foreground"> · {event.triggerId ?? "operator"}</span>
            {event.senderBotId && <span className="text-muted-foreground"> · from {event.senderBotId}</span>}
            {event.session && (
              <span className="text-muted-foreground">
                {" → "}
                <Link
                  to={sessionHref(event.session.sessionId)}
                  onClick={(click) => click.stopPropagation()}
                  className="underline-offset-2 hover:underline"
                >
                  {event.session.label}
                </Link>
              </span>
            )}
          </span>
          {detail && (
            <span className={cn("block truncate text-muted-foreground", outcome === "run_failed" && "text-destructive")}>
              {detail}
            </span>
          )}
        </span>
        <span className="flex shrink-0 items-center gap-1.5">
          {batchSize > 1 && (
            <Badge variant="outline" title={(active ?? recent)?.deliveryId}>
              batch of {batchSize}
            </Badge>
          )}
          {hops > 0 && <Badge variant="outline">{hops} hop{hops === 1 ? "" : "s"}</Badge>}
          <Badge
            variant={outcomeVariant(outcome, working)}
            title={event.resolvedAtMs != null ? `resolved ${timeLabel(event.resolvedAtMs)}` : undefined}
          >
            {chip}
          </Badge>
          <span className="w-16 text-right text-muted-foreground">{timeLabel(event.receivedAtMs)}</span>
          {open ? <ChevronDown className="size-3.5 text-muted-foreground" /> : <ChevronRight className="size-3.5 text-muted-foreground" />}
        </span>
      </button>
      {open && (
        <div className="grid gap-1.5 border-t px-3 py-2 text-muted-foreground">
          <div className="grid gap-x-4 gap-y-1 sm:grid-cols-2">
            <span>
              Event id <code className="wrap-anywhere">{event.eventId}</code>
            </span>
            <span>Received {new Date(event.receivedAtMs).toLocaleString()}</span>
            {event.resolvedAtMs != null && <span>Resolved {new Date(event.resolvedAtMs).toLocaleString()}</span>}
            {runId && (
              <span>
                Run <code>{runId}</code>
              </span>
            )}
            {event.inReplyTo && (
              <span>
                Reply to #{event.inReplyTo.seq} at {event.inReplyTo.bot}
              </span>
            )}
            {usage != null && (usage.inputTokens ?? 0) > 0 && (
              <span>
                {Math.round(((usage.cachedInputTokens ?? 0) / (usage.inputTokens ?? 1)) * 100)}% of{" "}
                {(usage.inputTokens ?? 0).toLocaleString()} prompt tokens served from cache
              </span>
            )}
          </div>
          {detail && <p className="wrap-anywhere text-foreground">{detail}</p>}
          {onReplay && (
            <div>
              <Button variant="outline" size="xs" onClick={onReplay}>
                <RotateCcw data-icon="inline-start" /> Replay this event
              </Button>
              <span className="ml-2">Creates a new delivery from the same payload.</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function outcomeVariant(
  outcome: BotEventOutcome | null,
  working: boolean,
): "destructive" | "secondary" | "outline" | "default" {
  if (outcome === "run_failed" || outcome === "blocked") return "destructive";
  if (outcome === "handled") return "secondary";
  if (outcome === null && working) return "default";
  return "outline";
}

function flushLabel(flushAtMs: number): string {
  const deltaSeconds = Math.round((flushAtMs - Date.now()) / 1000);
  if (deltaSeconds <= 0) return "now";
  if (deltaSeconds < 120) return `in ${deltaSeconds}s`;
  return `in ${Math.round(deltaSeconds / 60)}m`;
}

function timeLabel(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "";
  const today = new Date().toDateString() === date.toDateString();
  return today
    ? date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** `runsPerDay` is spent by bot runs and by the sub-agent sessions those runs delegate. */
export function budgetLabel(
  runsPerDay: number | null,
  controller: BotControllerSnapshot | null | undefined,
): string {
  const runs = controller?.runsToday ?? 0;
  const descendants = controller?.descendantsToday ?? 0;
  const used = runs + descendants;
  const limit = runsPerDay === null ? `${used} runs, no daily limit` : `${used} / ${runsPerDay} runs`;
  return descendants > 0 ? `${limit} (${runs} runs, ${descendants} sub-agents)` : limit;
}
