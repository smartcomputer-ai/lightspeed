import { useWatchConnected } from "@/lib/universe-watch";
import type { BotReadResponse } from "@lightspeed-ai/sdk";
import { useQuery } from "@tanstack/react-query";
import { AddIcon, BotAvatarIcon } from "@/icons";
import { useNavigate, useParams } from "react-router-dom";
import {
  api,
  botLabel,
  type BotListItem,
  type BotListResponse,
  type BotStateReadResponse,
  type BotView,
} from "@/api";
import { BotDetail, type BotTab } from "@/components/bot/detail";
import { BotCreateDialog } from "@/pages/BotCreatePage";
import { BotAvatar } from "@/components/bot/face";
import { StatusDot, outcomeLabel, relativeTime, type BotTone } from "@/components/bot/status";
import { Button } from "@/components/ui/button";
import { DetailPrompt, LoadingNote, UniverseNotFound } from "@/components/page";
import { useCreateParam } from "@/lib/create-param";
import { useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { useActionPermissions } from "@/lib/permissions";
import {
  ListGroupLabel,
  ListPane,
  ListPaneBody,
  ListPaneHeader,
  ListPaneState,
  ListMark,
  ListRow,
  ListRowLine,
} from "@/components/list-pane";
import { requestRowMenu } from "@/lib/row-menu";
import { botMetaItems } from "@/components/bot/bot-meta";

const ROSTER_REFRESH_MS = 5_000;

/// Bots: a roster on the left and one bot's conversations or activity on the right.
export function BotsPage({ view = "chat" }: { admin: boolean; view?: BotTab }) {
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
const TONE_LABEL: Record<BotTone, string> = {
  live: "Working",
  waiting: "Waiting for approval",
  idle: "Idle",
  paused: "Paused",
  attention: "Needs attention: its last event failed",
  closed: "Closed",
};

export function rosterLine(bot: BotListItem): { text: string; tone: BotTone } {
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
  create,
  onCreate,
}: {
  universeId: string;
  handle: string;
  activeId: string | undefined;
  create: boolean;
  onCreate: () => void;
}) {
  const watchConnected = useWatchConnected(universeId);
  const bots = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/v1/universes/${universeId}/bots`),
    refetchInterval: watchConnected ? 60_000 : ROSTER_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const roster = bots.data?.bots ?? [];
  const groups = rosterGroups(roster);
  const navigate = useNavigate();

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
      <ListPaneBody>
        <ListPaneState
          loading={bots.isLoading}
          error={bots.error}
          hasData={Boolean(bots.data)}
          empty={roster.length === 0 && "No bots yet."}
        />
        {groups.map((group) => (
          <div key={group.title}>
            {groups.length > 1 && <ListGroupLabel>{group.title}</ListGroupLabel>}
            <ul>
              {group.bots.map((bot) => {
                const line = rosterLine(bot);
                return (
                  <ListRow
                    key={bot.botId}
                    to={`/u/${handle}/bots/${bot.botId}`}
                    active={bot.botId === activeId}
                    // Right-click: the bot (opened if it is not) and its
                    // page's ⋯ menu, at the pointer.
                    entryProps={{
                      onContextMenu: (event) => {
                        event.preventDefault();
                        if (bot.botId !== activeId) navigate(`/u/${handle}/bots/${bot.botId}`);
                        requestRowMenu("bot", bot.botId, event);
                      },
                    }}
                    leading={<BotAvatar botId={bot.botId} size={30} />}
                    indicator={<ListMark label={TONE_LABEL[line.tone]} tip={TONE_LABEL[line.tone]}><StatusDot tone={line.tone} /></ListMark>}
                    title={botLabel(bot)}
                    marks={botMetaItems({ status: { label: line.text, tone: line.tone }, pending: bot.pendingCount, closed: bot.closedAtMs != null })}
                    meta={bot.lastEvent && relativeTime(bot.lastEvent.receivedAtMs)}
                  >
                    <ListRowLine tone={line.tone === "attention" ? "destructive" : undefined} title={line.text}>
                      {line.text}
                    </ListRowLine>
                  </ListRow>
                );
              })}
            </ul>
          </div>
        ))}
      </ListPaneBody>
    </>
  );
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
    queryKey: ["bot", universeId, botId],
    queryFn: () => api<BotReadResponse>("GET", `/api/v1/universes/${universeId}/bots/${botId}`),
  });
  const state = useQuery({
    queryKey: ["bot-state", universeId, botId],
    queryFn: () => api<BotStateReadResponse>("GET", `/api/v1/universes/${universeId}/bots/${botId}/state`),
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
      {...(state.data?.state ? { state: state.data.state } : {})}
      {...(state.error ? { stateError: state.error.message } : {})}
      view={view}
      sessionId={sessionId}
    />
  );
}
