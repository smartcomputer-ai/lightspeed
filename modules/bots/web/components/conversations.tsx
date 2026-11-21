import { useState } from "react";
import { DropdownCaretIcon, SessionMainThreadIcon, SessionSubThreadIcon, SubAgentIcon, type IconComponent } from "@lightspeed-ai/platform-web/modules/ui/icons";
import type { BotEventOutcome, BotStateView, SessionActivity } from "@lightspeed-ai/sdk";
import { activityLabel, activityTone, sessionDotState, type StatusTone } from "@lightspeed-ai/platform-web/modules/ui/status";
import { outcomeLabel } from "./status";
import { relativeTime } from "@lightspeed-ai/platform-web/modules/ui";
import {
  MetaDetails,
  MetaDetailsHeading,
  MetaDetailsList,
  MetaDetailsRow,
  type MetaItem,
} from "@lightspeed-ai/platform-web/modules/ui/meta";
import { sessionName } from "@lightspeed-ai/platform-web/modules/conversations";
import { Popover, PopoverContent, PopoverTrigger } from "@lightspeed-ai/platform-web/modules/ui/popover";

/// A bot's conversations, as its row in the bots list nests them and as
/// its page's conversation switcher lists them: Main, its threads, and the
/// sub-agents under the conversation that delegated to them. Everything
/// comes from the bot's state, already read for its page or its open row.

/// One sub-row under a selected bot: a thread, or a sub-agent under the
/// conversation that delegated to it.
export interface BotSubRow {
  id: string;
  title: string;
  /** The title's tooltip: what kind of conversation it is. */
  hint?: string;
  /** Its second line: a thread's latest work (as the bot's own row says it), or a sub-agent's profile and state. */
  line: string;
  /** The line reports a failure: drawn in the danger color. */
  failed?: boolean;
  tone: StatusTone;
  /** A closed sub-agent: drawn faded. */
  closed: boolean;
  /** What it is, as the mark in its meta toolbar: a thread (or one-off) of the bot, or a sub-agent. */
  kind: { thread: true; label: string } | { thread: false; profile?: string };
  /** When it last changed. */
  atMs?: number;
  children: BotSubRow[];
}

/// A bot's conversations as sub-rows of its row, which stands for Main:
/// its threads, most recently active first, and each sub-agent under the
/// conversation that delegated to it (one of Main's under the bot itself).
/// Everything comes from the bot's state, already read: nothing more is.
export function botSubRows(state: BotStateView | undefined): BotSubRow[] {
  const controller = state?.controller;
  if (!controller) return [];
  const sessions = state?.sessions ?? [];
  const summaryOf = new Map(sessions.map((session) => [session.id, session]));
  // Each thread's delivery in flight, and its latest finished one.
  const activeOf = new Map((controller.activeDeliveries ?? []).map((delivery) => [delivery.sessionId, delivery]));
  const recentOf = new Map<string, NonNullable<typeof controller.recentDeliveries>[number]>();
  for (const delivery of controller.recentDeliveries ?? []) {
    const known = recentOf.get(delivery.sessionId);
    if (!known || delivery.finishedAtMs > known.finishedAtMs) recentOf.set(delivery.sessionId, delivery);
  }
  const threads: BotSubRow[] = (controller.sessions ?? [])
    .filter((session) => session.kind !== "main")
    .sort((left, right) => (right.lastActiveAtMs ?? 0) - (left.lastActiveAtMs ?? 0))
    .map((session) => {
      const summary = summaryOf.get(session.sessionId);
      const kind = session.kind === "perKey" ? "Thread" : "One-off";
      const { line, tone, failed } = threadLine({
        activity: summary?.activity,
        active: activeOf.get(session.sessionId),
        recent: recentOf.get(session.sessionId),
        preview: summary?.lastMessagePreview ?? undefined,
        kind,
      });
      return {
        id: session.sessionId,
        title: session.label,
        hint: `${kind}: ${session.label}`,
        kind: { thread: true, label: kind },
        line,
        ...(failed ? { failed } : {}),
        tone,
        closed: false,
        ...(session.lastActiveAtMs == null ? {} : { atMs: session.lastActiveAtMs }),
        children: [],
      };
    });
  const byId = new Map<string, BotSubRow>(threads.map((row) => [row.id, row]));
  const subagents = sessions.filter((session) => session.origin);
  for (const session of subagents) {
    const state = sessionDotState(session);
    const closed = session.lifecycleStatus === "closed";
    byId.set(session.id, {
      id: session.id,
      title: sessionName(session),
      kind: { thread: false, ...(session.origin?.agent?.profileId ? { profile: session.origin.agent.profileId } : {}) },
      line: [session.origin?.agent?.profileId, state.label ?? (closed ? "Closed" : "Idle")].filter(Boolean).join(" · "),
      tone: state.tone,
      closed,
      atMs: session.updatedAtMs,
      children: [],
    });
  }
  // Hang each sub-agent off its parent; Main's (and any whose parent is not
  // listed) off the bot.
  const rows = [...threads];
  for (const session of subagents) {
    const row = byId.get(session.id)!;
    const parent = byId.get(session.origin!.parentSessionId);
    if (parent && parent !== row) parent.children.push(row);
    else rows.push(row);
  }
  return rows;
}

/// A sub-row's kind as a mark in its meta toolbar: the sub-thread icon on a
/// thread or one-off, the sub-agent icon on a sub-agent.
export function subRowKindMark(row: Pick<BotSubRow, "kind">): MetaItem {
  return row.kind.thread
    ? { key: "kind", label: row.kind.label, icon: SessionSubThreadIcon, tooltip: `${row.kind.label}: a conversation of the bot beside Main` }
    : {
      key: "kind",
      label: "Sub-agent",
      icon: SubAgentIcon,
      tooltip: row.kind.profile ? `Sub-agent, running ${row.kind.profile}` : "Sub-agent",
    };
}

/// A thread's second line, as the bot's own row words its latest work: what
/// waits on it, the event it is working on, how its latest delivery ended
/// and the bot's summary of it, else its latest message, else its kind.
export function threadLine({ activity, active, recent, preview, kind }: {
  activity: SessionActivity | undefined;
  active: { seqs: number[] } | undefined;
  recent: { seqs: number[]; outcome: BotEventOutcome; summary?: string | null } | undefined;
  preview: string | undefined;
  kind: string;
}): { line: string; tone: StatusTone; failed: boolean } {
  const seq = (seqs: number[]) => (seqs.length ? Math.max(...seqs) : undefined);
  if (activity === "waiting") return { line: activityLabel("waiting")!, tone: "waiting", failed: false };
  if (active) {
    const on = seq(active.seqs);
    return { line: on === undefined ? "Working" : `Working on #${on}`, tone: "live", failed: false };
  }
  if (activity === "working") return { line: activityLabel("working")!, tone: "live", failed: false };
  if (recent) {
    const failed = recent.outcome === "runFailed" || recent.outcome === "blocked";
    const on = seq(recent.seqs);
    const detail = recent.summary?.trim() || preview?.trim() || kind;
    return {
      line: `${on === undefined ? "" : `#${on} `}${outcomeLabel(recent.outcome)} · ${detail}`,
      tone: failed ? "attention" : "idle",
      failed,
    };
  }
  return { line: preview?.trim() || kind, tone: activityTone(activity), failed: false };
}

/// A conversation of the bot as its page names it: Main, a thread, or a
/// sub-agent, with its title and second line.
export interface BotConversation {
  id: string;
  kind: "main" | "thread" | "subagent";
  title: string;
  line: string;
  tone: StatusTone;
  failed?: boolean;
  closed: boolean;
  atMs?: number;
  /// Who delegated to a sub-agent: a conversation's title.
  parent?: string;
}

/// Every conversation of the bot, Main first, then each thread followed by
/// the sub-agents under it (Main's after the threads), depth first.
export function botConversations(state: BotStateView | undefined): BotConversation[] {
  const controller = state?.controller;
  if (!controller) return [];
  const mainId = controller.mainSessionId;
  const summary = (state?.sessions ?? []).find((session) => session.id === mainId);
  const recent = (controller.recentDeliveries ?? [])
    .filter((delivery) => delivery.sessionId === mainId)
    .reduce<NonNullable<typeof controller.recentDeliveries>[number] | undefined>(
      (latest, delivery) => (!latest || delivery.finishedAtMs > latest.finishedAtMs ? delivery : latest),
      undefined,
    );
  const mainLine = threadLine({
    activity: summary?.activity,
    active: (controller.activeDeliveries ?? []).find((delivery) => delivery.sessionId === mainId),
    recent,
    preview: summary?.lastMessagePreview ?? undefined,
    kind: "The bot's own conversation",
  });
  const lastActive = (controller.sessions ?? []).find((session) => session.sessionId === mainId)?.lastActiveAtMs;
  const main: BotConversation = {
    id: mainId,
    kind: "main",
    title: "Main",
    line: mainLine.line,
    tone: mainLine.tone,
    ...(mainLine.failed ? { failed: true } : {}),
    closed: false,
    ...(lastActive == null ? {} : { atMs: lastActive }),
  };
  const conversations: BotConversation[] = [main];
  const walk = (rows: BotSubRow[], parent: string | undefined) => {
    for (const row of rows) {
      conversations.push({
        id: row.id,
        kind: row.kind.thread ? "thread" : "subagent",
        title: row.title,
        line: row.line,
        tone: row.tone,
        ...(row.failed ? { failed: true } : {}),
        closed: row.closed,
        ...(row.atMs === undefined ? {} : { atMs: row.atMs }),
        ...(row.kind.thread ? {} : { parent: parent ?? "Main" }),
      });
      walk(row.children, row.title);
    }
  };
  walk(botSubRows(state), undefined);
  return conversations;
}

const KIND_ICON: Record<BotConversation["kind"], IconComponent> = {
  main: SessionMainThreadIcon,
  thread: SessionSubThreadIcon,
  subagent: SubAgentIcon,
};

const KIND_LABEL: Record<BotConversation["kind"], string> = { main: "Main", thread: "Thread", subagent: "Sub-agent" };

/// How long ago, as "5m ago" or "just now".
function ago(ms: number): string {
  const since = relativeTime(ms);
  return since === "now" ? "just now" : `${since} ago`;
}

/// The bot's conversations as a popover's content: Main and its threads
/// (sub-agents are reached from their thread), each with its state and
/// latest work, opening it; the one shown is marked.
export function ConversationDetails({ conversations, currentId, href, bot }: {
  conversations: BotConversation[];
  currentId: string | undefined;
  href: (sessionId: string) => string;
  /// The bot's name, for the heading.
  bot: string;
}) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Conversations" description={`${bot}'s main conversation and its threads.`} />
      <MetaDetailsList label="Conversations">
        {conversations.filter((conversation) => conversation.kind !== "subagent").map((conversation) => (
          <MetaDetailsRow
            key={conversation.id}
            primary={conversation.title}
            secondary={[
              // What it is, where its title does not say: Main's does.
              conversation.kind === "subagent" ? `Sub-agent of ${conversation.parent}` : conversation.kind === "thread" && KIND_LABEL.thread,
              conversation.line,
              conversation.atMs !== undefined && ago(conversation.atMs),
            ]}
            tone={conversation.tone}
            muted={conversation.closed}
            current={conversation.id === currentId}
            href={href(conversation.id)}
          />
        ))}
      </MetaDetailsList>
    </MetaDetails>
  );
}

/// The open conversation's name in the page's conversation header, with its
/// kind's icon and a caret: a popover (pointing back at it) lists Main and
/// the threads to switch to. An open sub-agent is named too.
export function ConversationSwitcher({ state, currentId, href, bot }: {
  state: BotStateView | undefined;
  currentId: string | undefined;
  href: (sessionId: string) => string;
  bot: string;
}) {
  const [open, setOpen] = useState(false);
  const conversations = botConversations(state);
  const current = conversations.find((conversation) => conversation.id === currentId);
  const Icon = KIND_ICON[current?.kind ?? "main"];
  const title = current?.title ?? (currentId ? `${currentId.slice(0, 14)}…` : "Main");
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        render={
          <button
            type="button"
            aria-label={`${current ? KIND_LABEL[current.kind] : "Conversation"}: ${title}. Switch conversation`}
            className="-ml-1.5 flex h-7 min-w-0 cursor-action items-center gap-1.5 rounded-md px-1.5 text-sm font-semibold outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring/50 data-popup-open:bg-muted"
          />
        }
      >
        <Icon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <span className="min-w-0 truncate">{title}</span>
        <DropdownCaretIcon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
      </PopoverTrigger>
      <PopoverContent align="start" className="w-96 p-1.5">
        <ConversationDetails conversations={conversations} currentId={currentId} href={href} bot={bot} />
      </PopoverContent>
    </Popover>
  );
}
