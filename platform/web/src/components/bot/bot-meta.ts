import { createElement, Fragment, type ReactNode } from "react";
import { ActivityIcon, InboxIcon, PrivateIcon, SharedIcon, SubAgentIcon } from "@/icons";
import type { Attribution, ResourceAccessSummary } from "@lightspeed-ai/sdk";
import type { BotEventView, SessionSummary } from "@/api";
import type { MetaItem, MetaTone } from "@/components/meta-toolbar";
import { SubAgentDetails, sessionName, subAgentsTip } from "@/components/session/session-meta";
import type { BotTone } from "./status";

/// A bot's meta items: one list for its list row and its page header.
///
///   status      what it is doing, in words with a dot (Working, Paused, …);
///               not in a list row, whose dot and second line show it
///   pending     how many events wait for it; in a list row (the page's
///               activity item counts them there)
///   sub-agents  on its page: how many its conversations delegated to, the
///               first by name; lists them on click
///   activity    on its page: opens its activity in a dialog, with how many
///               events wait; its tooltip names the latest event
///   sharing     who sees it (a bot's audience is the universe) and who
///               created it

export const BOT_META = { status: "status", pending: "pending", subAgents: "subagents", activity: "activity", sharing: "sharing" } as const;

const PAGE = ["header", "strip"] as const;

const TONE: Record<BotTone, { tone: MetaTone; pulse?: boolean }> = {
  live: { tone: "success", pulse: true },
  waiting: { tone: "warning" },
  idle: { tone: "muted" },
  paused: { tone: "muted" },
  attention: { tone: "danger" },
  closed: { tone: "muted" },
};

export function botMetaItems({ status, error, pending, closed, activity, subAgents, access, viewerId }: {
  status: { label: string; tone: BotTone };
  /// Why it needs attention, for the status's tooltip.
  error?: string | null;
  /// Events waiting to be handled.
  pending: number;
  closed: boolean;
  /// On its page: opens its activity, where the waiting events are, and
  /// the latest event, for the item's tooltip.
  activity?: { open: () => void; latest?: BotEventView | null };
  /// Who sees it and who created it; `viewerId` (the signed-in user) names
  /// a creator who is the viewer "you".
  access?: ResourceAccessSummary;
  viewerId?: string | null;
  /// On its page: the sub-agents its conversations delegated to, each
  /// opening its conversation; `parentOf` names who delegated to one.
  subAgents?: {
    sessions: SessionSummary[];
    href: (sessionId: string) => string;
    parentOf?: (session: SessionSummary) => string | undefined;
  };
}): MetaItem[] {
  const items: MetaItem[] = [
    { key: BOT_META.status, label: status.label, value: status.label, ...TONE[status.tone], tooltip: error ?? null, priority: 10, places: ["header", "strip"] },
  ];
  const waiting = pending > 0 && !closed;
  const waitingTip = `${pending} event${pending === 1 ? "" : "s"} waiting to be handled`;
  if (waiting) {
    items.push({
      key: BOT_META.pending,
      label: `${pending} waiting`,
      icon: InboxIcon,
      value: pending,
      text: `${pending} waiting`,
      tone: "info",
      tooltip: waitingTip,
      priority: 8,
      // On its page the activity item counts them.
      ...(activity ? { places: ["list"] as const } : {}),
    });
  }
  const children = subAgents?.sessions ?? [];
  if (subAgents && children.length > 0) {
    items.push({
      key: BOT_META.subAgents,
      label: `${children.length} sub-agent${children.length === 1 ? "" : "s"}`,
      icon: SubAgentIcon,
      value: children.length,
      text: `${children.length} sub-agent${children.length === 1 ? "" : "s"}`,
      tooltip: subAgentsTip(sessionName(children[0]!), children.length, false),
      priority: 6,
      places: PAGE,
      details: createElement(SubAgentDetails, {
        sessions: children,
        href: subAgents.href,
        description: "Sessions this bot's conversations delegated work to.",
        ...(subAgents.parentOf ? { parentOf: subAgents.parentOf } : {}),
      }),
    });
  }
  if (access) {
    const shared = access.visibility === "universe";
    const creator = access.createdBy ? creatorLabel(access.createdBy, viewerId) : null;
    items.push({
      key: BOT_META.sharing,
      label: shared ? "Shared" : "Private",
      icon: shared ? SharedIcon : PrivateIcon,
      text: shared ? "Shared" : "Private",
      tooltip: lines([
        shared ? "Shared: every member of the universe sees this bot" : "Private: only its creator and admins see this bot",
        creator && `Created by ${creator}`,
      ].filter((line): line is string => Boolean(line))),
      priority: 4,
    });
  }
  if (activity) {
    items.push({
      key: BOT_META.activity,
      label: waiting ? `Activity, ${pending} waiting` : "Activity",
      icon: ActivityIcon,
      ...(waiting ? { value: pending, tone: "info" as const } : {}),
      text: waiting ? `Activity · ${pending} waiting` : "Activity",
      tooltip: lines(activityTip(activity.latest, waiting ? waitingTip : null)),
      onSelect: activity.open,
      priority: 7,
      places: PAGE,
    });
  }
  return items;
}

/// How long ago, as "5m ago" or "just now".
function ago(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h ago`;
  return `${Math.floor(delta / 86_400_000)}d ago`;
}

/// The activity item's tooltip, line by line: the latest event (its number,
/// kind, how it went and when, then what came of it as the activity list
/// says it: the outcome's note, else the event's summary), then what waits.
export function activityTip(latest: BotEventView | null | undefined, waiting: string | null): string[] {
  if (!latest) return [waiting ? `Activity: ${waiting}` : "Activity: no events yet"];
  const outcome = latest.outcome ? (latest.outcome === "runFailed" ? "run failed" : latest.outcome) : "pending";
  const head = `Latest: #${latest.seq} ${latest.kind} · ${outcome} · ${ago(latest.receivedAtMs)}`;
  const summary = (latest.outcomeDetail ?? latest.summary).trim();
  const short = summary.length > 120 ? `${summary.slice(0, 119).trimEnd()}…` : summary;
  return [head, short, waiting].filter((line): line is string => Boolean(line));
}

/// Lines as one tooltip, a break between each.
function lines(texts: string[]): ReactNode {
  return createElement(Fragment, null, ...texts.flatMap((text, index) => (index > 0 ? [createElement("br"), text] : [text])));
}

/// Who created something, as a person reads it: you, another member, an API
/// key, or the system. Members are not named: that would need the members
/// list.
export function creatorLabel(createdBy: Attribution, viewerId: string | null | undefined): string {
  switch (createdBy.kind) {
    case "actor":
      return viewerId && createdBy.id === viewerId ? "you" : "a member";
    case "token":
      return createdBy.name;
    case "local":
      return "a local operator";
    default:
      return "the system";
  }
}
