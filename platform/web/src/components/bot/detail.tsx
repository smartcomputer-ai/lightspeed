import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  ArrowUpRight,
  ChevronDown,
  ChevronRight,
  LoaderCircle,
  Pause,
  Play,
  RotateCcw,
  SlidersHorizontal,
} from "lucide-react";
import { NavLink, useNavigate, useSearchParams } from "react-router-dom";
import { api, botLabel, type BotControllerSnapshot, type BotStateView, type BotView, type SessionView } from "@/api";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { SessionMenuIdentity, SessionMenuMetadata } from "@/components/session/session-menu-details";
import { SessionMenuPreferences } from "@/components/session/session-menu-preferences";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { BotActivity } from "./activity";
import { BotChat } from "./chat";
import { BotEditorDialog } from "./editor-dialog";
import { BotAvatar } from "./face";
import { botInputOf, idIsRedundant } from "./identity";
import { BotSetup } from "./setup";
import { StatusDot, botStatus, relativeTime } from "./status";

export type BotTab = "chat" | "activity";

/** Threads shown as tabs before the rest fold into the +N menu. */
const INLINE_THREADS = 3;

export interface ConversationTab {
  id: string;
  label: string;
  hint: string;
  live: boolean;
  closed: boolean;
  kind: "main" | "thread" | "subagent";
  lastActiveMs?: number;
}

/**
 * A bot's conversations as tabs: Main, then the most recently active
 * threads inline, everything else — older threads and sub-agents under
 * their parent — behind +N. The selected conversation is always inline, so
 * a deep link never lands in the overflow.
 */
export function conversationTabs(
  state: BotStateView | undefined,
  selectedId: string | undefined,
): { inline: ConversationTab[]; overflow: ConversationTab[] } {
  const controller = state?.controller;
  if (!controller) return { inline: [], overflow: [] };
  const active = new Set((controller.activeDeliveries ?? []).map((delivery) => delivery.sessionId));
  const sessions = controller.sessions ?? [];
  const labelOf = new Map(sessions.map((session) => [session.sessionId, session.kind === "main" ? "Main" : session.label]));
  for (const child of state?.descendants ?? []) {
    labelOf.set(child.id, child.displayName?.trim() || child.id.slice(0, 14));
  }
  const ready = controller.setupStatus === "ready";
  const main: ConversationTab = {
    id: controller.mainSessionId,
    label: "Main",
    hint: ready ? "the bot's desk" : controller.setupStatus === "degraded" ? "needs attention" : "starting…",
    live: active.has(controller.mainSessionId),
    closed: false,
    kind: "main",
  };
  const threads: ConversationTab[] = sessions
    .filter((session) => session.kind !== "main")
    .sort((left, right) => (right.lastActiveAtMs ?? 0) - (left.lastActiveAtMs ?? 0))
    .map((session) => ({
      id: session.sessionId,
      label: session.label,
      hint: session.kind === "perKey" ? "thread" : "one-off",
      live: active.has(session.sessionId) || session.busy,
      closed: false,
      kind: "thread",
      ...(session.lastActiveAtMs == null ? {} : { lastActiveMs: session.lastActiveAtMs }),
    }));
  const subagents: ConversationTab[] = (state?.descendants ?? []).map((child) => {
    const parentId = child.origin?.parentSessionId;
    return {
      id: child.id,
      label: child.displayName ?? child.id.slice(0, 14),
      hint: `sub-agent of ${parentId ? (labelOf.get(parentId) ?? parentId.slice(0, 12)) : "the bot"}`,
      live: child.lifecycleStatus !== "closed",
      closed: child.lifecycleStatus === "closed",
      kind: "subagent" as const,
      lastActiveMs: child.updatedAtMs,
    };
  });
  const inline = [main, ...threads.slice(0, INLINE_THREADS)];
  const overflow = [...threads.slice(INLINE_THREADS), ...subagents];
  if (selectedId !== undefined && !inline.some((tab) => tab.id === selectedId)) {
    const index = overflow.findIndex((tab) => tab.id === selectedId);
    if (index >= 0) inline.push(...overflow.splice(index, 1));
    else inline.push({ id: selectedId, label: `${selectedId.slice(0, 14)}…`, hint: "conversation", live: false, closed: false, kind: "thread" });
  }
  return { inline, overflow };
}

/**
 * One bot: a header that answers "is it working?", then one row for its
 * conversations and Activity. Settings is a global bot action in the header.
 */
export function BotDetail({
  universeId,
  slug,
  bot,
  state,
  stateError,
  manage,
  view,
  sessionId,
}: {
  universeId: string;
  slug: string;
  bot: BotView;
  state?: BotStateView;
  stateError?: string;
  manage: boolean;
  view: BotTab;
  sessionId: string | undefined;
}) {
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const base = `/u/${slug}/bots/${bot.botId}`;
  const controller = state?.controller ?? undefined;
  const status = botStatus(bot, controller, stateError);
  const selected = view === "chat" ? (sessionId ?? controller?.mainSessionId) : undefined;
  const { inline, overflow } = conversationTabs(
    state,
    view === "chat" ? selected : undefined,
  );
  const sessionHref = (id: string) => (id === controller?.mainSessionId ? base : `${base}/chat/${encodeURIComponent(id)}`);
  const enabled = bot.enabled ?? true;
  const togglePause = useMutation({
    mutationFn: () =>
      api<{ bot: BotView }>("PUT", `/api/v1/universes/${universeId}/bots/${bot.botId}`, {
        bot: { ...botInputOf(bot), enabled: !enabled },
        expectedRevision: bot.revision,
      }),
    onSuccess: async ({ bot: updated }) => {
      queryClient.setQueryData(["bot", universeId, bot.botId], { bot: updated });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["bots", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] }),
      ]);
    },
  });
  const pending = controller?.pendingDeliveries ?? 0;
  const settingsOpen = searchParams.get("settings") === "open";
  const setSettingsOpen = (open: boolean) => {
    const next = new URLSearchParams(searchParams);
    if (open) next.set("settings", "open");
    else next.delete("settings");
    setSearchParams(next, { replace: !open });
  };

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex h-12 shrink-0 items-center gap-2.5 border-b px-4">
        <NavLink to={`/u/${slug}/bots`} className="md:hidden" aria-label="Back to bots">
          <ChevronRight className="size-4 rotate-180" />
        </NavLink>
        <BotAvatar botId={bot.botId} size={26} />
        <span className="min-w-0 truncate text-sm font-semibold">{botLabel(bot)}</span>
        {!idIsRedundant(bot.displayName, bot.botId) && (
          <code className="hidden truncate text-xs text-muted-foreground sm:inline">{bot.botId}</code>
        )}
        <span
          className={cn(
            "flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground",
            status.tone === "attention" && "text-destructive",
          )}
          title={stateError ?? controller?.lastError ?? undefined}
        >
          <StatusDot tone={status.tone} />
          <span className="truncate">{status.label}</span>
        </span>
        <div className="ml-auto flex items-center gap-1">
          {manage && bot.closedAtMs == null && (
            <Button
              variant="outline"
              size="xs"
              disabled={togglePause.isPending}
              onClick={() => togglePause.mutate()}
              title={enabled ? "Pause: schedules stop and events wait; nothing is lost." : "Resume schedules and delivery."}
            >
              {togglePause.isPending ? (
                <LoaderCircle data-icon="inline-start" className="animate-spin" />
              ) : enabled ? (
                <Pause data-icon="inline-start" />
              ) : (
                <Play data-icon="inline-start" />
              )}
              {enabled ? "Pause" : "Resume"}
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => setSettingsOpen(true)}
            title="Bot settings"
            aria-label="Bot settings"
          >
            <SlidersHorizontal />
          </Button>
        </div>
      </div>
      {togglePause.error && (
        <p className="border-b bg-destructive/10 px-4 py-1.5 text-xs text-destructive">{togglePause.error.message}</p>
      )}
      <nav className="flex h-10 shrink-0 items-stretch gap-0.5 overflow-x-auto overflow-y-hidden overscroll-y-none touch-pan-x touch-pinch-zoom border-b px-2" aria-label="Bot conversations and activity">
        {controller ? (
          inline.map((tab) => {
            const active = view === "chat" && selected === tab.id;
            return (
              <span
                key={tab.id}
                className={cn(
                  "flex shrink-0 items-stretch border-b-2",
                  active ? "border-primary" : "border-transparent",
                )}
              >
                <NavLink
                  to={sessionHref(tab.id)}
                  end
                  title={tab.hint}
                  className={cn(
                    "flex max-w-48 items-center gap-1.5 px-2.5 py-2 text-sm whitespace-nowrap",
                    active ? "pr-1 font-medium text-foreground" : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  <StatusDot tone={tab.closed ? "closed" : tab.live ? "live" : "idle"} />
                  <span className={cn("truncate", tab.kind === "subagent" && "text-muted-foreground")}>
                    {tab.kind === "subagent" ? `↳ ${tab.label}` : tab.label}
                  </span>
                  {tab.kind === "main" && controller.setupStatus !== "ready" && (
                    <span className={cn("text-[11px]", controller.setupStatus === "degraded" ? "text-destructive" : "text-muted-foreground")}>
                      {controller.setupStatus === "degraded" ? "needs attention" : "starting…"}
                    </span>
                  )}
                </NavLink>
                {active && (
                  <ConversationMenu
                    universeId={universeId}
                    slug={slug}
                    bot={bot}
                    controller={controller}
                    sessionId={tab.id}
                    tab={tab}
                    manage={manage}
                  />
                )}
              </span>
            );
          })
        ) : (
          <span className="self-center px-2 text-xs text-muted-foreground">
            {stateError ? "Controller unavailable" : "Starting…"}
          </span>
        )}
        {overflow.length > 0 && <OverflowTabs tabs={overflow} sessionHref={sessionHref} />}
        <span className="my-2.5 mx-1.5 w-px shrink-0 bg-border" aria-hidden />
        <TabLink to={`${base}/activity`} active={view === "activity"}>
          <Activity className="size-4" />
          Activity
          {pending > 0 && (
            <span className="rounded-full bg-primary px-1.5 text-[10px] font-semibold text-primary-foreground">{pending}</span>
          )}
        </TabLink>
      </nav>
      {view === "chat" ? (
        <BotChat universeId={universeId} slug={slug} bot={bot} state={state} stateError={stateError} sessionId={sessionId} />
      ) : (
        <BotActivity universeId={universeId} slug={slug} bot={bot} state={state} stateError={stateError} manage={manage} />
      )}
      <BotEditorDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        title={`Bot Settings (${botLabel(bot)})`}
        description="Identity, job, triggers, session profile, collaborators, guardrails, and lifecycle."
        contentClassName="sm:max-w-4xl"
      >
        <BotSetup universeId={universeId} slug={slug} bot={bot} state={state} manage={manage} />
      </BotEditorDialog>
    </div>
  );
}

function TabLink({
  to,
  active,
  title,
  children,
}: {
  to: string;
  active: boolean;
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <NavLink
      to={to}
      end
      title={title}
      className={cn(
        "flex max-w-48 shrink-0 items-center gap-1.5 border-b-2 px-2.5 py-2 text-sm whitespace-nowrap",
        active
          ? "border-primary font-medium text-foreground"
          : "border-transparent text-muted-foreground hover:text-foreground",
      )}
    >
      {children}
    </NavLink>
  );
}

function OverflowTabs({ tabs, sessionHref }: { tabs: ConversationTab[]; sessionHref: (id: string) => string }) {
  const navigate = useNavigate();
  const threads = tabs.filter((tab) => tab.kind !== "subagent");
  const subagents = tabs.filter((tab) => tab.kind === "subagent");
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <button
            type="button"
            className="flex shrink-0 items-center gap-1 border-b-2 border-transparent px-2 py-2 text-sm text-muted-foreground hover:text-foreground"
          />
        }
      >
        +{tabs.length}
        <ChevronDown className="size-3.5" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="max-h-96 min-w-64 overflow-y-auto">
        {threads.length > 0 && (
          <DropdownMenuGroup>
            <DropdownMenuLabel>Threads</DropdownMenuLabel>
            {threads.map((tab) => (
              <OverflowItem key={tab.id} tab={tab} onSelect={() => navigate(sessionHref(tab.id))} />
            ))}
          </DropdownMenuGroup>
        )}
        {threads.length > 0 && subagents.length > 0 && <DropdownMenuSeparator />}
        {subagents.length > 0 && (
          <DropdownMenuGroup>
            <DropdownMenuLabel>Sub-agents</DropdownMenuLabel>
            {subagents.map((tab) => (
              <OverflowItem key={tab.id} tab={tab} onSelect={() => navigate(sessionHref(tab.id))} />
            ))}
          </DropdownMenuGroup>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function OverflowItem({ tab, onSelect }: { tab: ConversationTab; onSelect: () => void }) {
  return (
    <DropdownMenuItem onClick={onSelect} className="gap-2">
      <StatusDot tone={tab.closed ? "closed" : tab.live ? "live" : "idle"} />
      <span className="min-w-0 flex-1">
        <span className={cn("block truncate", tab.closed && "text-muted-foreground")}>{tab.label}</span>
        <span className="block truncate text-[11px] text-muted-foreground">{tab.hint}</span>
      </span>
      {tab.lastActiveMs !== undefined && (
        <span className="shrink-0 text-[11px] text-muted-foreground">{relativeTime(tab.lastActiveMs)}</span>
      )}
    </DropdownMenuItem>
  );
}

/**
 * What the session header used to carry, as a chevron on the active
 * conversation's tab — next to the thing it acts on: the id, the full-page
 * view, and (for the bot's own sessions) a reset. Configuration is not
 * here on purpose: a bot's sessions are configured through Setup (profile
 * and brief), and a per-session edit would drift from it unseen — the
 * Sessions page keeps that escape hatch under its "Managed by" framing.
 */
function ConversationMenu({
  universeId,
  slug,
  bot,
  controller,
  sessionId,
  tab,
  manage,
}: {
  universeId: string;
  slug: string;
  bot: BotView;
  controller: BotControllerSnapshot;
  sessionId: string;
  tab: ConversationTab | undefined;
  manage: boolean;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [resetOpen, setResetOpen] = useState(false);
  const managedHere = (controller.sessions ?? []).some((entry) => entry.sessionId === sessionId);
  const session = useQuery({
    queryKey: ["session", universeId, sessionId],
    queryFn: () =>
      api<SessionView>(
        "GET",
        `/api/v1/universes/${universeId}/sessions/${encodeURIComponent(sessionId)}`,
      ),
  });
  const reset = useMutation({
    mutationFn: () =>
      api(
        "POST",
        `/api/v1/universes/${universeId}/bots/${bot.botId}/sessions/${encodeURIComponent(sessionId)}/rotate`,
      ),
    onSuccess: () => {
      setResetOpen(false);
      return queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] });
    },
  });
  const label = tab?.kind === "main" ? "Main" : (tab?.label ?? "this conversation");
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <button
              type="button"
              aria-label="Conversation menu"
              title="Conversation menu"
              className="flex items-center rounded-sm pr-2 pl-0.5 text-muted-foreground hover:text-foreground"
            />
          }
        >
          {reset.isPending ? <LoaderCircle className="size-3.5 animate-spin" /> : <ChevronDown className="size-3.5" />}
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          className="max-h-[min(28rem,calc(100vh-1rem))] w-80 max-w-[calc(100vw-1rem)]"
        >
          <SessionMenuIdentity sessionId={sessionId} />
          <SessionMenuPreferences />
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuItem onClick={() => navigate(`/u/${slug}/sessions/${encodeURIComponent(sessionId)}`)}>
              <ArrowUpRight /> Open on the Sessions page
            </DropdownMenuItem>
          </DropdownMenuGroup>
          {manage && managedHere && bot.closedAtMs == null && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuItem disabled={reset.isPending} onClick={() => setResetOpen(true)}>
                  <RotateCcw /> Reset {label}…
                </DropdownMenuItem>
              </DropdownMenuGroup>
            </>
          )}
          <SessionMenuMetadata metadata={session.data?.metadata} />
        </DropdownMenuContent>
      </DropdownMenu>
      <AlertDialog
        open={resetOpen}
        onOpenChange={(open) => {
          setResetOpen(open);
          if (open) reset.reset();
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Reset {label}?</AlertDialogTitle>
            <AlertDialogDescription>
              The conversation and its open sub-agents close, and the bot continues in a fresh one
              with no prior history. Active work finishes first; events already admitted stay queued.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {reset.error && <p className="text-sm text-destructive">{reset.error.message}</p>}
          <AlertDialogFooter>
            <AlertDialogCancel>Keep</AlertDialogCancel>
            <AlertDialogAction disabled={reset.isPending} onClick={() => reset.mutate()}>
              {reset.isPending ? "Resetting…" : "Reset"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
