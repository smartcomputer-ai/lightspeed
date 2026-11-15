import { useSessionCompaction } from "@/lib/sessions/compaction";
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DropdownCaretIcon, GoToIcon, ResetIcon } from "@/icons";
import { NavLink, useNavigate, useSearchParams } from "react-router-dom";
import { DetailHeader } from "@/components/detail-header";
import { MetaToolbar } from "@/components/meta-toolbar";
import { botMetaItems } from "./bot-meta";
import { compactSessionId, sessionName } from "@/components/session/session-meta";
import { api, botLabel, type BotControllerSnapshot, type BotEventListResponse, type BotStateView, type BotView, type SessionView } from "@/api";
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
import { SessionActionsMenu } from "@/components/session/session-actions-menu";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { activityTone, foldActivity } from "@/components/activity-dot";
import type { ResourceAccessSummary, SessionActivity } from "@lightspeed-ai/sdk";
import { cn } from "@/lib/utils";
import { useDocumentTitle } from "@/lib/document-title";
import { useActionPermissions, usePermissionIdentity } from "@/lib/permissions";
import { BotActivity } from "./activity";
import { BotActionsMenu } from "./bot-actions-menu";
import { BotChat } from "./chat";
import { BotEditorDialog } from "./editor-dialog";
import { botInputOf } from "./identity";
import { BotSetup } from "./setup";
import { StatusDot, botStatus, relativeTime } from "./status";

export type BotTab = "chat" | "activity";

/** Threads shown as tabs before the rest fold into the +N menu. */
const INLINE_THREADS = 3;

export interface ConversationTab {
  id: string;
  label: string;
  hint: string;
  activity: SessionActivity;
  closed: boolean;
  kind: "main" | "thread" | "subagent";
  lastActiveMs?: number;
}

/**
 * A bot's conversations as tabs: Main, then the most recently active
 * threads inline, older threads behind +N. Sub-agents are not tabs: the
 * header's sub-agent item lists them. The selected conversation is always
 * inline, a sub-agent too, so a deep link never lands in the overflow.
 */
export function conversationTabs(
  state: BotStateView | undefined,
  selectedId: string | undefined,
): { inline: ConversationTab[]; overflow: ConversationTab[] } {
  const controller = state?.controller;
  if (!controller) return { inline: [], overflow: [] };
  // What each session is doing comes from the sessions themselves, so a run
  // someone started by chatting counts like one the bot started.
  const activityOf = new Map((state?.sessions ?? []).map((session) => [session.id, session.activity]));
  const activity = (id: string): SessionActivity => activityOf.get(id) ?? "idle";
  const sessions = controller.sessions ?? [];
  const labelOf = new Map(sessions.map((session) => [session.sessionId, session.kind === "main" ? "Main" : session.label]));
  const descendants = (state?.sessions ?? []).filter((session) => session.origin);
  for (const child of descendants) {
    labelOf.set(child.id, child.displayName?.trim() || child.id.slice(0, 14));
  }
  const ready = controller.setupStatus === "ready";
  const main: ConversationTab = {
    id: controller.mainSessionId,
    label: "Main",
    hint: ready ? "the bot's desk" : controller.setupStatus === "degraded" ? "needs attention" : "starting…",
    activity: activity(controller.mainSessionId),
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
      activity: activity(session.sessionId),
      closed: false,
      kind: "thread",
      ...(session.lastActiveAtMs == null ? {} : { lastActiveMs: session.lastActiveAtMs }),
    }));
  const subagents: ConversationTab[] = descendants.map((child) => {
    const parentId = child.origin?.parentSessionId;
    return {
      id: child.id,
      label: child.displayName ?? child.id.slice(0, 14),
      hint: `sub-agent of ${parentId ? (labelOf.get(parentId) ?? parentId.slice(0, 12)) : "the bot"}`,
      activity: child.activity,
      closed: child.lifecycleStatus === "closed",
      kind: "subagent" as const,
      lastActiveMs: child.updatedAtMs,
    };
  });
  const inline = [main, ...threads.slice(0, INLINE_THREADS)];
  const overflow = threads.slice(INLINE_THREADS);
  if (selectedId !== undefined && !inline.some((tab) => tab.id === selectedId)) {
    const index = overflow.findIndex((tab) => tab.id === selectedId);
    const subagent = subagents.find((tab) => tab.id === selectedId);
    if (index >= 0) inline.push(...overflow.splice(index, 1));
    else if (subagent) inline.push(subagent);
    else inline.push({ id: selectedId, label: `${selectedId.slice(0, 14)}…`, hint: "conversation", activity: activity(selectedId), closed: false, kind: "thread" });
  }
  return { inline, overflow };
}

/**
 * One bot: a header that answers "is it working?", with its sub-agents and
 * its activity among the meta items, then one row for its conversations.
 * Activity and Settings open in dialogs over the conversation, each kept in
 * the address (?activity=open, ?settings=open) so a link or reload reopens it.
 */
export function BotDetail({
  universeId,
  handle,
  bot,
  access,
  state,
  stateError,
  view,
  sessionId,
}: {
  universeId: string;
  handle: string;
  bot: BotView;
  /** Who sees the bot and who created it, for its sharing mark. */
  access?: ResourceAccessSummary;
  state?: BotStateView;
  stateError?: string;
  view: BotTab;
  sessionId: string | undefined;
}) {
  const permissions = useActionPermissions(universeId);
  const viewerId = usePermissionIdentity();
  const manage = permissions.can("manageBot");
  const invoke = permissions.can("invokeBot");
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const base = `/u/${handle}/bots/${bot.botId}`;
  const controller = state?.controller ?? undefined;
  const status = botStatus(bot, controller, stateError, foldActivity((state?.sessions ?? []).map((session) => session.activity)));
  const selected = sessionId ?? controller?.mainSessionId;
  const { inline, overflow } = conversationTabs(state, selected);
  const activityOpen = searchParams.get("activity") === "open";
  useDocumentTitle([
    activityOpen ? "Activity" : inline.find((tab) => tab.id === selected)?.label,
    botLabel(bot),
  ]);
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
  // Who delegated to a sub-agent: a conversation by its tab label, else
  // another sub-agent by name.
  const conversationLabel = new Map((controller?.sessions ?? []).map((session) => [session.sessionId, session.kind === "main" ? "Main" : session.label]));
  const sessionsById = new Map((state?.sessions ?? []).map((session) => [session.id, session]));
  const parentOf = (session: { origin?: { parentSessionId: string } | null }) => {
    const parentId = session.origin?.parentSessionId;
    if (!parentId) return undefined;
    const parent = sessionsById.get(parentId);
    return conversationLabel.get(parentId) ?? (parent ? sessionName(parent) : compactSessionId(parentId, 12));
  };
  const settingsOpen = searchParams.get("settings") === "open";
  const setSettingsOpen = (open: boolean) => {
    const next = new URLSearchParams(searchParams);
    if (open) next.set("settings", "open");
    else next.delete("settings");
    setSearchParams(next, { replace: !open });
  };
  const setActivityOpen = (open: boolean) => {
    const next = new URLSearchParams(searchParams);
    if (open) next.set("activity", "open");
    else next.delete("activity");
    setSearchParams(next, { replace: !open });
  };
  // The former activity page's address opens the dialog over Main.
  const navigate = useNavigate();
  useEffect(() => {
    if (view === "activity") navigate(`${base}?activity=open`, { replace: true });
  }, [view, base, navigate]);
  // The newest event, for the activity item's tooltip; the dialog loads the rest.
  const latestEvent = useQuery({
    queryKey: ["bot-events", universeId, bot.botId, "latest"],
    queryFn: () => api<BotEventListResponse>("GET", `/api/v1/universes/${universeId}/bots/${bot.botId}/events?limit=1`),
    refetchInterval: 15_000,
    refetchIntervalInBackground: false,
  });

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <DetailHeader
        back={{ to: `/u/${handle}/bots`, label: "Back to bots" }}
        title={botLabel(bot)}
        tags={
          <MetaToolbar
            items={botMetaItems({
              status,
              error: stateError ?? controller?.lastError,
              pending,
              closed: bot.closedAtMs != null,
              activity: { open: () => setActivityOpen(true), latest: latestEvent.data?.events?.[0] ?? null },
              ...(access ? { access, viewerId } : {}),
              subAgents: {
                sessions: (state?.sessions ?? []).filter((session) => session.origin),
                href: sessionHref,
                parentOf,
              },
            })}
            place="header"
            full
            label="Bot details"
            className="grow"
          />
        }
        actions={[{
          kind: "custom",
          key: "bot",
          node: (
            <BotActionsMenu
              botId={bot.botId}
              onSettings={() => setSettingsOpen(true)}
              pause={manage && bot.closedAtMs == null
                ? { enabled, pending: togglePause.isPending, onToggle: () => togglePause.mutate() }
                : undefined}
            />
          ),
        }]}
      />
      {togglePause.error && (
        <p className="border-b bg-destructive/10 px-4 py-1.5 text-xs text-destructive">{togglePause.error.message}</p>
      )}
      <nav className="flex h-10 shrink-0 items-stretch gap-0.5 overflow-x-auto overflow-y-hidden overscroll-y-none touch-pan-x touch-pinch-zoom border-b px-2" aria-label="Bot conversations and activity">
        {controller ? (
          inline.map((tab) => {
            const active = selected === tab.id;
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
                  <StatusDot tone={activityTone(tab.activity, tab.closed)} />
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
                    handle={handle}
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
      </nav>
      <BotChat universeId={universeId} handle={handle} bot={bot} state={state} stateError={stateError} sessionId={sessionId} />
      <BotEditorDialog
        open={activityOpen}
        onOpenChange={setActivityOpen}
        title={`Activity (${botLabel(bot)})`}
        description="The events this bot received and how it handled them."
        contentClassName="sm:max-w-5xl"
      >
        <BotActivity universeId={universeId} handle={handle} bot={bot} state={state} stateError={stateError} manage={manage} invoke={invoke} />
      </BotEditorDialog>
      <BotEditorDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        title={`Bot Settings (${botLabel(bot)})`}
        description="Identity, job, triggers, session profile, collaborators, guardrails, and lifecycle."
        contentClassName="sm:max-w-4xl"
      >
        <BotSetup universeId={universeId} handle={handle} bot={bot} state={state} manage={manage} />
      </BotEditorDialog>
    </div>
  );
}

function OverflowTabs({ tabs, sessionHref }: { tabs: ConversationTab[]; sessionHref: (id: string) => string }) {
  const navigate = useNavigate();
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
        <DropdownCaretIcon className="size-3.5" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="max-h-96 min-w-64 overflow-y-auto">
        <DropdownMenuGroup>
          <DropdownMenuLabel>Threads</DropdownMenuLabel>
          {tabs.map((tab) => (
            <OverflowItem key={tab.id} tab={tab} onSelect={() => navigate(sessionHref(tab.id))} />
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function OverflowItem({ tab, onSelect }: { tab: ConversationTab; onSelect: () => void }) {
  return (
    <DropdownMenuItem onClick={onSelect} className="gap-2">
      <StatusDot tone={activityTone(tab.activity, tab.closed)} />
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
 * The shared session menu, as a chevron on the active conversation's tab —
 * next to the thing it acts on: the id, the full-page view, and (for the
 * bot's own sessions) a reset. Configuration is not
 * here on purpose: a bot's sessions are configured through Setup (profile
 * and brief), and a per-session edit would drift from it unseen — the
 * Sessions page keeps that escape hatch under its "Managed by" framing.
 */
function ConversationMenu({
  universeId,
  handle,
  bot,
  controller,
  sessionId,
  tab,
  manage,
}: {
  universeId: string;
  handle: string;
  bot: BotView;
  controller: BotControllerSnapshot;
  sessionId: string;
  tab: ConversationTab | undefined;
  manage: boolean;
}) {
  const queryClient = useQueryClient();
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
  const compaction = useSessionCompaction(universeId, sessionId, session.data);
  const canCompact = useActionPermissions(universeId).can("controlSession");
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
      <SessionActionsMenu
        variant="tab"
        sessionId={sessionId}
        metadata={session.data?.metadata}
        onCompact={canCompact && session.data && session.data.lifecycleStatus !== "closed" ? compaction.compact : undefined}
        compactionLabel={compaction.label}
        open={{
          label: "Open on the Sessions page",
          href: `/u/${handle}/sessions/${encodeURIComponent(sessionId)}`,
          icon: <GoToIcon />,
        }}
        lifecycle={manage && managedHere && bot.closedAtMs == null ? (
          <DropdownMenuItem disabled={reset.isPending} onClick={() => setResetOpen(true)}>
            <ResetIcon /> Reset {label}…
          </DropdownMenuItem>
        ) : undefined}
      />
      <AlertDialog
        open={resetOpen && manage}
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
            <AlertDialogAction disabled={!manage || reset.isPending} onClick={() => manage && reset.mutate()}>
              {reset.isPending ? "Resetting…" : "Reset"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
