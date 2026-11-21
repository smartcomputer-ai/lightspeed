import { sessionCollectionMeta } from "../model/collection-watch";
import { useEffect, type ReactNode } from "react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { api, type SessionListPage, type SessionOrigin, type SessionSummary, type SessionView } from "@/api";
import { sessionDotState } from "@/components/activity-dot";
import { relativeTime } from "@/lib/relative-time";
import {
  MetaDetails,
  MetaDetailsAction,
  MetaDetailsFooter,
  MetaDetailsHeading,
  MetaDetailsList,
  MetaDetailsRow,
} from "@/components/meta-details";
import { ManagedIcon, ParentAgentIcon, PrivateIcon, SessionCloseIcon, SharedIcon, ShowMoreIcon, SubAgentIcon } from "@/icons";
import type { MetaItem, MetaPlace, MetaTone } from "@/components/meta-toolbar";

/// The places of the page: its header and the strip under it.
const PAGE: MetaPlace[] = ["header", "strip"];

/// A session's meta items: one list for every place the session appears,
/// each item naming its places. A list row shows the essentials, its page
/// header more, the strip under the header everything.
///
///                list  header  strip
///   parent agent  -      x      x     leftmost; its state on click
///   activity      -      x      x     Stopped, Working, Waiting (a row has its dot)
///   closed        x      x      x
///   controller    x      x      x     the bot or integration that drives it
///   sub-agents    x      x      -     how many, the first by name; on the page it lists them on click
///   sharing      (x)     x      x     a row marks a shared root session only
///   environment   x      x      x
///   resources     -      x      x     workspaces, MCP servers, tools
///   each sub-agent -     -      x

/// What a session's meta items are built from: the summary any list has,
/// and what a session's page knows besides.
export interface SessionMetaInput {
  session: Pick<SessionSummary, "id" | "access" | "activity" | "lifecycleStatus" | "lifecycleControlled" | "closedAtMs" | "retention" | "origin">
    & Partial<Pick<SessionView, "executionFailure">>;
  /// Sub-agent sessions under it, when known; `more` when there are more
  /// than counted, `first` the first one's name.
  subAgents?: { count: number; more?: boolean; first?: string };
  /// Display information supplied by the controller's UI when available.
  controller?: { label: string; icon?: MetaItem["icon"]; iconColor?: string; href?: string };
  /// Its lineage, on its page: the parent, and each sub-agent by name.
  lineage?: SessionLineage;
  lineageHref?: (sessionId: string) => string;
  /// What it can reach, as items (see sessionResourceItems).
  resources?: MetaItem[];
}

/// The keys of the meta items callers single out.
export const SESSION_META = {
  activity: "activity",
  controller: "controller",
} as const;

const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;

/// A date and time in the viewer's locale, for tooltips.
function whenLabel(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/// When a closed session closed, and when retention deletes it.
function closedTip(session: SessionMetaInput["session"]): ReactNode {
  const deleteAt = session.retention.deleteAtMs;
  return (
    <>
      {session.closedAtMs != null ? `Closed ${whenLabel(session.closedAtMs)}` : "Closed"}
      {deleteAt != null && <><br />{`Deleted after ${whenLabel(deleteAt)}`}</>}
    </>
  );
}

/// A session's state as a tone, for the dot beside a lineage link.
function lineageTone(session: Pick<SessionSummary, "activity" | "lifecycleStatus">): { tone?: MetaTone; pulse?: boolean } {
  if (session.lifecycleStatus === "closed") return { tone: "muted" };
  if (session.activity === "waiting") return { tone: "warning" };
  if (session.activity === "working") return { tone: "success", pulse: true };
  return {};
}

export function compactSessionId(id: string, length = 18): string {
  return `${id.slice(0, length)}${id.length > length ? "…" : ""}`;
}

/// The session's meta items, most important first in each group.
export function sessionMetaItems({ session, subAgents, controller, lineage, lineageHref, resources = [] }: SessionMetaInput): MetaItem[] {
  const items: MetaItem[] = [];
  const closed = session.lifecycleStatus === "closed";

  // The agent that delegated to it, leftmost: where this session came from.
  if (lineage?.parent && lineageHref) {
    const parent = lineage.parent;
    items.push({
      key: "parent", label: `Parent agent: ${parent.label}`, icon: ParentAgentIcon, text: "Parent agent",
      tooltip: `Parent agent: ${parent.label}`,
      ...(parent.session ? lineageTone(parent.session) : parent.closed ? { tone: "muted" as const } : {}),
      priority: 9, places: PAGE,
      details: <ParentAgentDetails parent={parent} href={lineageHref} />,
    });
  }

  // State: a stopped workflow, a closed session, or what a run is doing.
  if (!closed && session.executionFailure) {
    items.push({
      key: SESSION_META.activity, label: "Stopped", value: "Stopped", tone: "danger", priority: 10, places: PAGE,
      tooltip: "Stopped: the session's workflow is no longer running",
    });
  } else if (closed) {
    items.push({ key: "closed", label: "Closed", icon: SessionCloseIcon, text: "Closed", tooltip: closedTip(session), priority: 10 });
  } else if (session.activity === "waiting") {
    items.push({
      key: SESSION_META.activity, label: "Waiting for approval", value: "Waiting for approval", tone: "warning", priority: 10, places: PAGE,
      tooltip: "Waiting for approval: a run is paused until someone decides on a tool call",
    });
  } else if (session.activity === "working") {
    items.push({
      key: SESSION_META.activity, label: "Working", value: "Working", tone: "success", pulse: true, priority: 10, places: PAGE,
      tooltip: "Working: a run is in progress",
    });
  }

  // Who drives it.
  if (session.lifecycleControlled) {
    const name = controller?.label;
    items.push({
      key: SESSION_META.controller,
      label: name ? `Managed by ${name}` : "Managed session",
      icon: controller?.icon ?? ManagedIcon,
      iconColor: controller?.iconColor,
      text: name ? `Managed by ${name}` : "Managed",
      tooltip: `${name ?? "An integration"} controls this session's lifecycle and chat input`,
      href: controller?.href,
      priority: 8,
    });
  }

  // How many it delegated to, the first by name. On its page a click lists
  // each, to open one; the strip names each instead.
  const count = subAgents?.count ?? lineage?.children.length ?? 0;
  if (count > 0) {
    const more = subAgents?.more ?? lineage?.more ?? false;
    const many = `${count}${more ? "+" : ""}`;
    const first = subAgents?.first ?? (lineage?.children[0] && sessionName(lineage.children[0]));
    items.push({
      key: "subagents", label: plural(count, "sub-agent"), icon: SubAgentIcon, value: many,
      text: `${many} sub-agent${count === 1 && !more ? "" : "s"}`,
      tooltip: subAgentsTip(first, count, more),
      priority: 6, places: ["list", "header"],
      ...(lineage && lineageHref && lineage.children.length > 0
        ? { details: <SubAgentList lineage={lineage} href={lineageHref} /> }
        : {}),
    });
  }

  // Who sees it. Sub-agents follow their root, so a list marks only a root,
  // and only when shared: private is the default.
  if (session.access) {
    const shared = session.access.visibility === "universe";
    items.push({
      key: "sharing", label: shared ? "Shared" : "Private", icon: shared ? SharedIcon : PrivateIcon, text: shared ? "Shared" : "Private",
      tooltip: shared ? "Shared: every member of the universe sees this" : "Private: only its creator and admins see this",
      priority: 4, ...(shared && !session.origin ? {} : { places: PAGE }),
    });
  }

  items.push(...resources);

  // Each sub-agent, in the strip.
  if (lineage && lineageHref) {
    for (const child of lineage.children) {
      const name = child.displayName?.trim();
      items.push({
        key: `subagent:${child.id}`, label: `Sub-agent: ${name || child.id}`, icon: SubAgentIcon, text: name || compactSessionId(child.id),
        tooltip: name ? `Sub-agent: ${name}` : `Sub-agent ${child.id}`, href: lineageHref(child.id),
        ...lineageTone(child), priority: 1, places: ["strip"],
      });
    }
    if (lineage.more && lineage.loadMore) {
      items.push({
        key: "subagents:more", label: "Load more sub-agents", text: lineage.loadingMore ? "Loading…" : "More sub-agents",
        onSelect: lineage.loadMore, priority: 0, places: ["strip"],
      });
    }
  }
  return items;
}

/// A session as a person reads it: its name, else its shortened id.
export function sessionName(session: Pick<SessionSummary, "id" | "displayName">): string {
  return session.displayName?.trim() || compactSessionId(session.id);
}

/// The sub-agent count's tooltip: the one sub-agent by name, or the first
/// and how many more. Without a name, how many.
export function subAgentsTip(first: string | undefined, count: number, more: boolean): string {
  if (!first) return `${count}${more ? "+" : ""} sub-agent${count === 1 && !more ? "" : "s"} delegated by this session`;
  if (count === 1 && !more) return `Sub-agent: ${first}`;
  return `Sub-agents: ${first} and ${count - 1}${more ? "+" : ""} more`;
}

/// A session's facts for a lineage row: its profile, its state and when it
/// last changed.
function lineageFacts(session: Pick<SessionSummary, "activity" | "lifecycleStatus" | "updatedAtMs" | "origin">
  & Partial<Pick<SessionSummary, "executionFailure" | "activityPhase" | "lastRun">>) {
  const state = sessionDotState(session);
  const closed = session.lifecycleStatus === "closed";
  return {
    tone: state.tone,
    closed,
    facts: [session.origin?.agent?.profileId, state.label ?? (closed ? "Closed" : "Idle"), ago(session.updatedAtMs)],
  };
}

/// The parent agent's popover: the session that delegated to this one, with
/// its state, opening it.
function ParentAgentDetails({ parent, href }: { parent: NonNullable<SessionLineage["parent"]>; href: (sessionId: string) => string }) {
  const facts = parent.session ? lineageFacts(parent.session) : undefined;
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Parent agent" description="The session that delegated work to this one." />
      <MetaDetailsList label="Parent agent">
        <MetaDetailsRow
          primary={parent.label}
          secondary={facts?.facts}
          tone={facts?.tone}
          muted={facts?.closed ?? parent.closed}
          href={href(parent.id)}
        />
      </MetaDetailsList>
    </MetaDetails>
  );
}

/// How long ago, as "5m ago" or "just now".
function ago(ms: number): string {
  const since = relativeTime(ms);
  return since === "now" ? "just now" : since && `${since} ago`;
}

/// The sub-agents a session delegated to, as the popover of its sub-agent
/// item: each with its state, profile and last change, opening its session.
function SubAgentList({ lineage, href }: { lineage: SessionLineage; href: (sessionId: string) => string }) {
  return (
    <SubAgentDetails
      sessions={lineage.children}
      href={href}
      description="Sessions this one delegated work to."
      {...(lineage.more && lineage.loadMore ? { loadMore: { onSelect: lineage.loadMore, loading: lineage.loadingMore } } : {})}
    />
  );
}

/// A popover listing sub-agent sessions: each with its state, profile (and
/// `parentOf`, who delegated to it) and last change, opening its session.
/// `loadMore` offers the next page.
export function SubAgentDetails({ sessions, href, description, parentOf, loadMore }: {
  sessions: SessionSummary[];
  href: (sessionId: string) => string;
  description: string;
  parentOf?: (session: SessionSummary) => string | undefined;
  loadMore?: { onSelect: () => void; loading: boolean };
}) {
  return (
    <MetaDetails>
      <MetaDetailsHeading title="Sub-agents" description={description} />
      <MetaDetailsList label="Sub-agents">
        {sessions.map((child) => {
          const name = child.displayName?.trim();
          const { tone, closed, facts } = lineageFacts(child);
          const parent = parentOf?.(child);
          return (
            <MetaDetailsRow
              key={child.id}
              primary={name || <span className="font-mono text-xs">{compactSessionId(child.id)}</span>}
              secondary={[facts[0], parent && `of ${parent}`, ...facts.slice(1)]}
              tone={tone}
              muted={closed}
              href={href(child.id)}
            />
          );
        })}
      </MetaDetailsList>
      {loadMore && (
        <MetaDetailsFooter>
          <MetaDetailsAction icon={ShowMoreIcon} onSelect={loadMore.onSelect} keepOpen busy={loadMore.loading}>
            {loadMore.loading ? "Loading…" : "Load more sub-agents"}
          </MetaDetailsAction>
        </MetaDetailsFooter>
      )}
    </MetaDetails>
  );
}

/// A session's place in a delegation tree: the session that delegated to it
/// and the sub-agents it delegated to.
export interface SessionLineage {
  /// The session that delegated to it; `session` once read.
  parent?: { id: string; label: string; closed: boolean; session?: SessionView };
  children: SessionSummary[];
  /// There are more sub-agents than loaded.
  more: boolean;
  loadingMore: boolean;
  loadMore?: () => void;
}

/// Reads a session's lineage. Run activity refreshes the sub-agents; their
/// data stays while a refresh is under way or fails, so the strip neither
/// collapses nor resizes the transcript, and a later empty result applies.
export function useSessionLineage({ universeId, sessionId, origin, runRevision }: {
  universeId: string;
  sessionId: string;
  origin: SessionOrigin | null | undefined;
  runRevision: number;
}): SessionLineage {
  const parentId = origin?.parentSessionId;
  const parent = useQuery({
    queryKey: ["session", universeId, parentId],
    queryFn: () => api<SessionView>("GET", `/api/ui/universes/${universeId}/sessions/${encodeURIComponent(parentId!)}`),
    enabled: Boolean(parentId),
  });
  const children = useInfiniteQuery({
    queryKey: ["session-children", universeId, sessionId],
    meta: sessionCollectionMeta(universeId, row => row.origin?.parentSessionId === sessionId),
    queryFn: ({ pageParam }) => {
      const params = new URLSearchParams({ limit: "50" });
      if (pageParam) params.set("cursor", pageParam);
      return api<SessionListPage>("GET", `/api/ui/universes/${universeId}/sessions/${encodeURIComponent(sessionId)}/children?${params.toString()}`);
    },
    initialPageParam: "",
    getNextPageParam: (last) => last.nextCursor ?? undefined,
  });
  const refetchChildren = children.refetch;
  useEffect(() => {
    if (runRevision > 0) void refetchChildren();
  }, [runRevision, refetchChildren]);
  const parentName = parent.data?.displayName?.trim();
  return {
    ...(parentId
      ? { parent: { id: parentId, label: parentName || compactSessionId(parentId), closed: parent.data?.lifecycleStatus === "closed", ...(parent.data ? { session: parent.data } : {}) } }
      : {}),
    children: children.data?.pages.flatMap((page) => page.sessions) ?? [],
    more: Boolean(children.hasNextPage),
    loadingMore: children.isFetchingNextPage,
    loadMore: () => void children.fetchNextPage(),
  };
}

/// Whether a lineage has anything to show.
export const hasLineage = (lineage: SessionLineage) => Boolean(lineage.parent) || lineage.children.length > 0;
