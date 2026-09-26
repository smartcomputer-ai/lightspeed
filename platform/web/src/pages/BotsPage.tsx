import type { BotReadResponse } from "@lightspeed-ai/agent-client";
import { ReadError } from "@/components/read-error";
import { useQuery } from "@tanstack/react-query";
import { Plus } from "lucide-react";
import { NavLink, useParams } from "react-router-dom";
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
import { StatusDot, relativeTime, type BotTone } from "@/components/bot/status";
import { BotFace } from "@/components/icons/bot";
import { Button } from "@/components/ui/button";
import { DetailPrompt, ListNote, LoadingNote, UniverseNotFound } from "@/components/page";
import { useCreateParam } from "@/lib/create-param";
import { useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { useActionPermissions } from "@/lib/permissions";

const ROSTER_REFRESH_MS = 5_000;

/// Bots: a roster on the left and one bot's conversations or activity on the right.
export function BotsPage({ view = "chat" }: { admin: boolean; view?: BotTab }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { botId, sessionId } = useParams<{ botId?: string; sessionId?: string }>();
  const permissions = useActionPermissions(universe?.id);
  const [createOpen, setCreateOpen] = useCreateParam("bot");

  if (isLoading) return <LoadingNote />;
  if (!universe) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  const create = permissions.can("create_bot");
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <aside
        className={cn(
          "w-full shrink-0 flex-col border-r md:flex md:w-72",
          botId ? "hidden" : "flex",
        )}
      >
        <BotsPane
          universeId={universe.id}
          slug={slug!}
          activeId={botId}
          create={create}
          onCreate={() => setCreateOpen(true)}
        />
      </aside>
      <section className={cn("min-w-0 flex-1 flex-col", botId ? "flex" : "hidden md:flex")}>
        {botId ? (
          <BotWorkspace
            key={botId}
            universeId={universe.id}
            slug={slug!}
            botId={botId}
            view={view}
            sessionId={sessionId}
          />
        ) : (
          <DetailPrompt
            icon={<BotFace size={40} className="text-muted-foreground/60" />}
            create={create ? { label: "New bot", onClick: () => setCreateOpen(true) } : undefined}
          >
            Pick a bot{create ? ", or create one" : ""}.
          </DetailPrompt>
        )}
      </section>
      {create && createOpen && (
        <BotCreateDialog
          universeId={universe.id}
          slug={slug!}
          open={createOpen}
          onOpenChange={setCreateOpen}
        />
      )}
    </div>
  );
}

/// One line of "what it is doing", from the event log alone: the newest
/// event and whether anything is still unresolved.
export function rosterLine(bot: BotListItem): { text: string; tone: BotTone } {
  if (bot.closedAtMs != null) return { text: "Closed", tone: "closed" };
  if (bot.enabled === false) {
    return {
      text: bot.pendingCount > 0 ? `Paused · ${bot.pendingCount} waiting` : "Paused",
      tone: "paused",
    };
  }
  const last = bot.lastEvent;
  if (bot.pendingCount > 0) {
    const on = last && last.outcome == null ? ` on #${last.seq}` : "";
    return { text: `Working${on} · ${last?.kind ?? "event"}`, tone: "live" };
  }
  if (!last) return { text: "Waiting for its first event", tone: "idle" };
  const failed = last.outcome === "run_failed" || last.outcome === "blocked";
  const detail = last.outcomeDetail?.trim() || last.kind;
  return {
    text: `#${last.seq} ${(last.outcome ?? "pending").replaceAll("_", " ")} · ${detail}`,
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
  slug,
  activeId,
  create,
  onCreate,
}: {
  universeId: string;
  slug: string;
  activeId: string | undefined;
  create: boolean;
  onCreate: () => void;
}) {
  const bots = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/v1/universes/${universeId}/bots`),
    refetchInterval: ROSTER_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const roster = bots.data?.bots ?? [];
  const groups = rosterGroups(roster);

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <h1 className="text-sm font-semibold">Bots</h1>
        {bots.data && <span className="text-xs text-muted-foreground">{roster.length}</span>}
        {create && (
          <Button
            variant="ghost"
            size="icon-sm"
            className="ml-auto"
            onClick={onCreate}
            aria-label="New bot"
          >
            <Plus />
          </Button>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {bots.isLoading && <p className="p-4 text-sm text-muted-foreground">Loading…</p>}
        {bots.error && <ReadError error={bots.error} loading={!bots.data} className="p-4" />}
        {bots.data && roster.length === 0 && (
          <ListNote>No bots yet.</ListNote>
        )}
        {groups.map((group) => (
          <div key={group.title}>
            {groups.length > 1 && (
              <div className="px-4 pt-3 pb-1 text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
                {group.title}
              </div>
            )}
            <ul>
              {group.bots.map((bot) => {
                const line = rosterLine(bot);
                return (
                  <li key={bot.botId}>
                    <NavLink
                      to={`/u/${slug}/bots/${bot.botId}`}
                      className={cn(
                        "grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-2.5 border-b px-4 py-2.5 text-sm hover:bg-muted/50",
                        bot.botId === activeId && "bg-muted",
                      )}
                    >
                      <BotAvatar botId={bot.botId} size={28} className="row-span-2" />
                      <span className="flex min-w-0 items-center gap-1.5">
                        <StatusDot tone={line.tone} />
                        <span className="min-w-0 truncate font-medium">{botLabel(bot)}</span>
                      </span>
                      <span className="text-right text-[11px] text-muted-foreground">
                        {bot.pendingCount > 0 && bot.closedAtMs == null ? (
                          <span className="inline-block min-w-4 rounded-full bg-primary px-1.5 text-center font-semibold text-primary-foreground">
                            {bot.pendingCount}
                          </span>
                        ) : (
                          bot.lastEvent && relativeTime(bot.lastEvent.receivedAtMs)
                        )}
                      </span>
                      <span
                        className={cn(
                          "col-span-2 min-w-0 truncate text-xs text-muted-foreground",
                          line.tone === "attention" && "text-destructive",
                        )}
                        title={line.text}
                      >
                        {line.text}
                      </span>
                    </NavLink>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </div>
    </>
  );
}

function BotWorkspace({
  universeId,
  slug,
  botId,
  view,
  sessionId,
}: {
  universeId: string;
  slug: string;
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
      slug={slug}
      bot={bot.data.bot}
      {...(state.data?.state ? { state: state.data.state } : {})}
      {...(state.error ? { stateError: state.error.message } : {})}
      view={view}
      sessionId={sessionId}
    />
  );
}
