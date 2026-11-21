import { botsApiPath, botListQuery, botDetailQuery } from "./queries";
import { Fragment, useState, type ReactNode } from "react";
import {
  useWatchConnected,
  useCreateParam,
  useActiveUniverse,
  useActionPermissions,
  usePermissionIdentity,
} from "@lightspeed-ai/platform-web/modules";
import { useQueries, useQuery } from "@tanstack/react-query";
import { AddIcon, BotAvatarIcon } from "@lightspeed-ai/platform-web/modules/ui/icons";
import { useNavigate, useParams } from "react-router-dom";
import { api } from "@lightspeed-ai/platform-web/modules/client";
import { botLabel } from "./model";
import type { BotListItem, BotStateReadResponse, BotStateView, BotView } from "@lightspeed-ai/sdk";
import { botSubRows, subRowKindMark, type BotSubRow } from "./components/conversations";
import { BotDetail, type BotTab } from "./components/detail";
import { BotCreateDialog } from "./BotCreatePage";
import { BotAvatar } from "./components/face";
import { StatusDot, type ActivityTone } from "@lightspeed-ai/platform-web/modules/ui/status";
import { outcomeLabel } from "./components/status";
import { relativeTime, DetailPrompt, LoadingNote, UniverseNotFound, cn } from "@lightspeed-ai/platform-web/modules/ui";
import { Button } from "@lightspeed-ai/platform-web/modules/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@lightspeed-ai/platform-web/modules/ui/tooltip";
import {
  ListGroupLabel,
  ListPane,
  ListPaneBody,
  ListPaneHeader,
  ListPaneState,
  ListMark,
  ListRow,
  ListRowLine,
  type ListRowTree,
  requestRowMenu,
} from "@lightspeed-ai/platform-web/modules/ui/lists";
import { botMetaItems } from "./components/bot-meta";

const ROSTER_REFRESH_MS = 5_000;

export function BotActivityPage() {
  return <BotsPage view="activity" />;
}

/// Bots: a roster on the left and one bot's conversations or activity on the right.
export function BotsPage({ view = "chat" }: { view?: BotTab }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const { botId, sessionId } = useParams<{ botId?: string; sessionId?: string }>();
  const permissions = useActionPermissions(universe?.id);
  const [createOpen, setCreateOpen] = useCreateParam("bot");

  if (isLoading) return <LoadingNote />;
  if (!universe) {
    return (
      <div className="p-6">
        <UniverseNotFound handle={handle} />
      </div>
    );
  }

  const create = permissions.can("createBot");
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <ListPane detailOpen={Boolean(botId)}>
        <BotsPane
          universeId={universe.id}
          handle={handle!}
          activeId={botId}
          activeSessionId={sessionId}
          create={create}
          onCreate={() => setCreateOpen(true)}
        />
      </ListPane>
      <section className={cn("min-w-0 flex-1 flex-col", botId ? "flex" : "hidden md:flex")}>
        {botId ? (
          <BotWorkspace
            key={botId}
            universeId={universe.id}
            handle={handle!}
            botId={botId}
            view={view}
            sessionId={sessionId}
          />
        ) : (
          <DetailPrompt
            icon={<BotAvatarIcon size={40} className="text-muted-foreground/60" />}
            create={create ? { label: "New bot", onClick: () => setCreateOpen(true) } : undefined}
          >
            Pick a bot{create ? ", or create one" : ""}.
          </DetailPrompt>
        )}
      </section>
      {create && createOpen && (
        <BotCreateDialog
          universeId={universe.id}
          handle={handle!}
          open={createOpen}
          onOpenChange={setCreateOpen}
        />
      )}
    </div>
  );
}

/// One line of "what it is doing", from the event log alone: the newest
/// event and whether anything is still unresolved.
/// What a bot's status dot means, for its tooltip.
const TONE_LABEL: Record<ActivityTone, string> = {
  live: "Working",
  waiting: "Waiting for approval",
  idle: "Idle",
  paused: "Paused",
  attention: "Needs attention: its last event failed",
  closed: "Closed",
};

export function rosterLine(bot: BotListItem): { text: string; tone: ActivityTone } {
  if (bot.closedAtMs != null) return { text: "Closed", tone: "closed" };
  if (bot.enabled === false) {
    return {
      text: bot.pendingCount > 0 ? `Paused · ${bot.pendingCount} waiting` : "Paused",
      tone: "paused",
    };
  }
  if (bot.status === "attention") return { text: "Needs attention", tone: "attention" };
  if (bot.status === "budget") return { text: "Budget exhausted", tone: "attention" };
  const last = bot.lastEvent;
  if (bot.activity === "waiting") return { text: "Waiting for approval", tone: "waiting" };
  if (bot.pendingCount > 0) {
    const on = last && last.outcome == null ? ` on #${last.seq}` : "";
    return { text: `Working${on} · ${last?.kind ?? "event"}`, tone: "live" };
  }
  if (bot.activity === "working") return { text: "Working", tone: "live" };
  if (!last) return { text: "Waiting for its first event", tone: "idle" };
  const failed = last.outcome === "runFailed" || last.outcome === "blocked";
  const detail = last.outcomeDetail?.trim() || last.kind;
  return {
    text: `#${last.seq} ${last.outcome ? outcomeLabel(last.outcome) : "pending"} · ${detail}`,
    tone: failed ? "attention" : "idle",
  };
}

function rosterGroups(bots: BotListItem[]) {
  const byActivity = (left: BotListItem, right: BotListItem) => {
    const l = left.lastEvent?.receivedAtMs ?? 0;
    const r = right.lastEvent?.receivedAtMs ?? 0;
    return r - l || botLabel(left).localeCompare(botLabel(right));
  };
  const open = (bot: BotListItem) => bot.closedAtMs == null;
  return [
    { title: "Active", bots: bots.filter((bot) => open(bot) && bot.enabled !== false).sort(byActivity) },
    { title: "Paused", bots: bots.filter((bot) => open(bot) && bot.enabled === false).sort(byActivity) },
    { title: "Closed", bots: bots.filter((bot) => !open(bot)).sort(byActivity) },
  ].filter((group) => group.bots.length > 0);
}

function BotsPane({
  universeId,
  handle,
  activeId,
  activeSessionId,
  create,
  onCreate,
}: {
  universeId: string;
  handle: string;
  activeId: string | undefined;
  /** The conversation open in the selected bot; none for Main. */
  activeSessionId: string | undefined;
  create: boolean;
  onCreate: () => void;
}) {
  const watchConnected = useWatchConnected(universeId);
  const bots = useQuery({
    ...botListQuery(universeId),
    refetchInterval: watchConnected ? 60_000 : ROSTER_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const roster = bots.data?.bots ?? [];
  const groups = rosterGroups(roster);
  const navigate = useNavigate();
  const viewerId = usePermissionIdentity();
  const { isOpen, toggle, openBots } = useOpenRows(activeId);
  // Each open bot's conversations, from its state: read only while its row
  // is open. The selected bot's page reads the same query and keeps it
  // fresh; another open bot's refreshes slowly, and stops once closed.
  const states = useQueries({
    queries: openBots.map((botId) => ({
      queryKey: ["bot-state", universeId, botId],
      queryFn: () => api<BotStateReadResponse>("GET", `${botsApiPath(universeId, botId)}/state`),
      refetchInterval: botId === activeId ? (false as const) : OPEN_BOT_REFRESH_MS,
      refetchIntervalInBackground: false,
    })),
  });
  const stateOf = new Map(openBots.map((botId, index) => [botId, states[index]?.data?.state]));
  const activeState = activeId ? stateOf.get(activeId) : undefined;
  const mainSessionId = activeState?.controller?.mainSessionId;
  const onMain = activeSessionId === undefined || activeSessionId === mainSessionId;
  // The selected conversation stays in sight: the rows above it open, once.
  useOpenPath(activeId, onMain ? undefined : activeSessionId, activeState, toggle.openAll);

  return (
    <>
      <ListPaneHeader
        title="Bots"
        count={bots.data ? roster.length : undefined}
        actions={create && (
          <Button variant="ghost" size="icon-sm" onClick={onCreate} aria-label="New bot">
            <AddIcon />
          </Button>
        )}
      />
      <ListPaneBody expand="title">
        <ListPaneState
          loading={bots.isLoading}
          error={bots.error}
          hasData={Boolean(bots.data)}
          empty={roster.length === 0 && "No bots yet."}
        />
        {groups.map((group) => {
          // Whether a bot has conversations to open: from its state once
          // read, else from the list's count of its open sessions (Main is
          // one of them). Without either it is not known, and gets no caret.
          const subRowsOf = (bot: BotListItem) => {
            const state = stateOf.get(bot.botId);
            return state ? botSubRows(state) : undefined;
          };
          const opens = (bot: BotListItem) => {
            const rows = subRowsOf(bot);
            return rows ? rows.length > 0 : (bot.openSessionCount ?? 0) > 1;
          };
          const caretRoom = group.bots.some(opens);
          return (
            <div key={group.title}>
              {groups.length > 1 && <ListGroupLabel>{group.title}</ListGroupLabel>}
              <ul>
                {group.bots.map((bot) => {
                  const line = rosterLine(bot);
                  const selected = bot.botId === activeId;
                  const base = `/u/${handle}/bots/${bot.botId}`;
                  const key = botKey(bot.botId);
                  const expandable = opens(bot);
                  const open = expandable && isOpen(key);
                  const children = open ? subRowsOf(bot) ?? [] : [];
                  return (
                    <Fragment key={bot.botId}>
                      <ListRow
                        to={base}
                        // The bot's row stands for its Main conversation.
                        active={selected && onMain}
                        tree={{ depth: 0, children: children.length > 0 }}
                        {...(expandable ? {
                          expander: {
                            open,
                            onToggle: () => toggle(key),
                            label: `${open ? "Hide" : "Show"} ${botLabel(bot)}'s conversations`,
                          },
                        } : {})}
                        caretRoom={caretRoom}
                        // Right-click: the bot (opened if it is not) and its
                        // page's ⋯ menu, at the pointer.
                        entryProps={{
                          onContextMenu: (event) => {
                            event.preventDefault();
                            if (bot.botId !== activeId) navigate(`/u/${handle}/bots/${bot.botId}`);
                            requestRowMenu("bot", bot.botId, event);
                          },
                        }}
                        leading={<BotAbout bot={bot}><BotAvatar botId={bot.botId} size={30} /></BotAbout>}
                        indicator={<ListMark label={TONE_LABEL[line.tone]} tip={TONE_LABEL[line.tone]}><StatusDot tone={line.tone} /></ListMark>}
                        title={botLabel(bot)}
                        marks={botMetaItems({
                          status: { label: line.text, tone: line.tone },
                          pending: bot.pendingCount,
                          closed: bot.closedAtMs != null,
                          access: bot.access,
                          viewerId,
                        })}
                        meta={bot.lastEvent && relativeTime(bot.lastEvent.receivedAtMs)}
                      >
                        <ListRowLine tone={line.tone === "attention" ? "destructive" : undefined} title={line.text}>
                          {line.text}
                        </ListRowLine>
                      </ListRow>
                      {children.length > 0 && (
                        <BotSubRows
                          rows={children}
                          depth={1}
                          lines={[]}
                          activeSessionId={selected && !onMain ? activeSessionId : undefined}
                          href={(sessionId) => `${base}/chat/${encodeURIComponent(sessionId)}`}
                          isOpen={isOpen}
                          toggle={toggle}
                        />
                      )}
                    </Fragment>
                  );
                })}
              </ul>
            </div>
          );
        })}
      </ListPaneBody>
    </>
  );
}

/// A bot's avatar with what the bot is for on hover: its name, description
/// and brief. Without either there is nothing to add to its row.
function BotAbout({ bot, children }: { bot: BotListItem; children: ReactNode }) {
  const description = bot.description?.trim();
  const brief = bot.brief?.trim();
  if (!description && !brief) return children;
  return (
    <Tooltip>
      <TooltipTrigger render={<span className="flex" />}>{children}</TooltipTrigger>
      <TooltipContent className="grid max-w-80 gap-1 text-left">
        <span className="font-medium">{botLabel(bot)}</span>
        {description && <span>{description}</span>}
        {brief && <span className="line-clamp-6 whitespace-pre-line opacity-80">{brief}</span>}
      </TooltipContent>
    </Tooltip>
  );
}

/// How often an open bot that is not selected re-reads its conversations.
const OPEN_BOT_REFRESH_MS = 15_000;

/// Keys of the open rows: a bot's and a conversation's, apart.
const botKey = (botId: string) => `bot:${botId}`;
const sessionKey = (sessionId: string) => `session:${sessionId}`;

/// Which rows of the bots list are open. Selecting a bot opens it, one level
/// down; anything opened or closed by hand stays so while the list lives.
function useOpenRows(activeId: string | undefined) {
  const [open, setOpen] = useState<Set<string>>(() => new Set(activeId ? [botKey(activeId)] : []));
  const [opened, setOpened] = useState(activeId);
  if (activeId !== opened) {
    // Opened on selection, during render: the bot's state loads with it.
    setOpened(activeId);
    if (activeId && !open.has(botKey(activeId))) setOpen(new Set([...open, botKey(activeId)]));
  }
  const toggle = Object.assign(
    (key: string) =>
      setOpen((current) => {
        const next = new Set(current);
        if (next.has(key)) next.delete(key);
        else next.add(key);
        return next;
      }),
    {
      openAll: (keys: string[]) =>
        setOpen((current) => (keys.every((key) => current.has(key)) ? current : new Set([...current, ...keys]))),
    },
  );
  const openBots = [...open].filter((key) => key.startsWith("bot:")).map((key) => key.slice("bot:".length));
  return { isOpen: (key: string) => open.has(key), toggle, openBots };
}

/// Opens the rows above the selected conversation, once per selection, so a
/// deep link or a pick from the header lands on a visible row; closing them
/// by hand afterwards sticks.
function useOpenPath(
  activeId: string | undefined,
  sessionId: string | undefined,
  state: BotStateView | undefined,
  openAll: (keys: string[]) => void,
) {
  const [done, setDone] = useState<string | null>(null);
  const target = activeId && sessionId ? `${activeId}/${sessionId}` : null;
  if (target && target !== done && state) {
    setDone(target);
    const path = pathTo(botSubRows(state), sessionId!);
    if (path) openAll([botKey(activeId!), ...path.map(sessionKey)]);
  }
}

/// The ids of the rows above the one with `id`, outermost first; null when
/// it is not among them.
function pathTo(rows: BotSubRow[], id: string, above: string[] = []): string[] | null {
  for (const row of rows) {
    if (row.id === id) return above;
    const found = pathTo(row.children, id, [...above, row.id]);
    if (found) return found;
  }
  return null;
}

/// A level of an open bot's sub-rows, and below each open one the next
/// level. A row with children gets a caret; when any of the level has one,
/// the rest keep its room, so their titles line up.
function BotSubRows({ rows, depth, lines, activeSessionId, href, isOpen, toggle }: {
  rows: BotSubRow[];
  depth: number;
  /** Per ancestor level below the bot: whether that ancestor has a later sibling. */
  lines: boolean[];
  activeSessionId: string | undefined;
  href: (sessionId: string) => string;
  isOpen: (key: string) => boolean;
  toggle: (key: string) => void;
}) {
  const caretRoom = rows.some((row) => row.children.length > 0);
  return rows.map((row, index) => {
    const last = index === rows.length - 1;
    const key = sessionKey(row.id);
    const open = row.children.length > 0 && isOpen(key);
    const tree: ListRowTree = { depth, last, lines, children: open };
    return (
      <Fragment key={row.id}>
        <ListRow
          to={href(row.id)}
          active={row.id === activeSessionId}
          tree={tree}
          {...(row.children.length > 0 ? {
            expander: { open, onToggle: () => toggle(key), label: `${open ? "Hide" : "Show"} the sub-agents of ${row.title}` },
          } : {})}
          caretRoom={caretRoom}
          indicator={<StatusDot tone={row.tone} />}
          title={row.title}
          {...(row.hint ? { titleHint: row.hint } : {})}
          marks={[subRowKindMark(row)]}
          muted={row.closed}
          meta={row.atMs !== undefined && relativeTime(row.atMs)}
        >
          <ListRowLine tone={row.failed ? "destructive" : undefined} title={row.line}>{row.line}</ListRowLine>
        </ListRow>
        {open && (
          <BotSubRows
            rows={row.children}
            depth={depth + 1}
            lines={[...lines, !last]}
            activeSessionId={activeSessionId}
            href={href}
            isOpen={isOpen}
            toggle={toggle}
          />
        )}
      </Fragment>
    );
  });
}

function BotWorkspace({
  universeId,
  handle,
  botId,
  view,
  sessionId,
}: {
  universeId: string;
  handle: string;
  botId: string;
  view: BotTab;
  sessionId: string | undefined;
}) {
  const bot = useQuery({
    ...botDetailQuery(universeId, botId),
  });
  const state = useQuery({
    queryKey: ["bot-state", universeId, botId],
    queryFn: () => api<BotStateReadResponse>("GET", `${botsApiPath(universeId, botId)}/state`),
    refetchInterval: 3_000,
    retry: true,
  });

  if (bot.isLoading) {
    return <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">Loading…</div>;
  }
  if (bot.error || !bot.data) {
    return (
      <div className="flex flex-1 items-center justify-center p-6 text-sm text-destructive">
        {bot.error?.message ?? "Bot not found"}
      </div>
    );
  }

  return (
    <BotDetail
      universeId={universeId}
      handle={handle}
      bot={bot.data.bot}
      access={bot.data.access}
      {...(state.data?.state ? { state: state.data.state } : {})}
      {...(state.error ? { stateError: state.error.message } : {})}
      view={view}
      sessionId={sessionId}
    />
  );
}
