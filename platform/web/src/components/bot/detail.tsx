import { useSessionCompaction } from "@session-ui/model/compaction";
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfigureIcon, GoToIcon, LoadingIcon, PauseIcon, ResetIcon, ResumeIcon } from "@/icons";
import { useNavigate, useSearchParams } from "react-router-dom";
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
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { foldActivity } from "@/components/activity-dot";
import type { ResourceAccessSummary } from "@lightspeed-ai/sdk";
import { useDocumentTitle } from "@/lib/document-title";
import { useSessionPageHref } from "@/lib/web-modules";
import { useActionPermissions, usePermissionIdentity } from "@/lib/permissions";
import { BotActivity } from "./activity";
import { BotActionsMenu } from "./bot-actions-menu";
import { BotChat } from "./chat";
import { BotEditorDialog } from "./editor-dialog";
import { botInputOf } from "./identity";
import { BotSetup } from "./setup";
import { botStatus } from "./status";
import { botConversations, ConversationSwitcher } from "./conversations";

export type BotTab = "chat" | "activity";

/**
 * One bot: a header that answers "is it working?", with its sub-agents and
 * its activity among the meta items, then the open conversation with a
 * header of its own: a switcher naming it (and listing the others), its
 * state and what it can reach, and its ⋯ menu. Activity and Settings open
 * in dialogs over the conversation, each kept in the address
 * (?activity=open, ?settings=open) so a link or reload reopens it.
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
  const current = botConversations(state).find((conversation) => conversation.id === selected);
  const activityOpen = searchParams.get("activity") === "open";
  useDocumentTitle([activityOpen ? "Activity" : current?.title, botLabel(bot)]);
  const sessionHref = (id: string) => (id === controller?.mainSessionId ? base : `${base}/chat/${encodeURIComponent(id)}`);
  const enabled = bot.enabled ?? true;
  const togglePause = useMutation({
    mutationFn: () =>
      api<{ bot: BotView }>("PUT", `/api/ui/universes/${universeId}/bots/${bot.botId}`, {
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
    queryFn: () => api<BotEventListResponse>("GET", `/api/ui/universes/${universeId}/bots/${bot.botId}/events?limit=1`),
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
              // The sub-agents item is left out of the header for now; the
              // bot's conversation list shows its sub-agents.
              // subAgents: {
              //   sessions: (state?.sessions ?? []).filter((session) => session.origin),
              //   href: sessionHref,
              //   parentOf,
              // },
            })}
            place="header"
            full
            label="Bot details"
            className="grow"
          />
        }
        tools={[
          ...(manage && bot.closedAtMs == null
            ? [{
                key: "pause",
                label: enabled ? "Pause bot" : "Resume bot",
                hint: enabled
                  ? "Pause bot: schedules stop and events wait; nothing is lost."
                  : "Resume bot: schedules and delivery resume.",
                icon: togglePause.isPending
                  ? <LoadingIcon className="motion-safe:animate-spin" />
                  : enabled ? <PauseIcon /> : <ResumeIcon />,
                disabled: togglePause.isPending,
                onSelect: () => togglePause.mutate(),
              }]
            : []),
          { key: "settings", label: "Bot settings", icon: <ConfigureIcon />, onSelect: () => setSettingsOpen(true) },
        ]}
      />
      {/* The same actions as the list row's right-click menu, which has no
          button here: the header shows them as tools. */}
      <BotActionsMenu
        trigger={false}
        botId={bot.botId}
        onSettings={() => setSettingsOpen(true)}
        pause={manage && bot.closedAtMs == null
          ? { enabled, pending: togglePause.isPending, onToggle: () => togglePause.mutate() }
          : undefined}
      />
      {togglePause.error && (
        <p className="border-b bg-destructive/10 px-4 py-1.5 text-xs text-destructive">{togglePause.error.message}</p>
      )}
      <BotChat
        universeId={universeId}
        handle={handle}
        bot={bot}
        state={state}
        stateError={stateError}
        sessionId={sessionId}
        {...(controller && selected ? {
          header: {
            title: <ConversationSwitcher state={state} currentId={selected} href={sessionHref} bot={botLabel(bot)} />,
            actions: (
              <ConversationMenu
                universeId={universeId}
                handle={handle}
                bot={bot}
                controller={controller}
                sessionId={selected}
                label={current?.title ?? "this conversation"}
                manage={manage}
              />
            ),
          },
        } : {})}
      />
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

/**
 * The shared session menu, as the ⋯ of the open conversation's header —
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
  label,
  manage,
}: {
  universeId: string;
  handle: string;
  bot: BotView;
  controller: BotControllerSnapshot;
  sessionId: string;
  /// What the conversation is called, e.g. "Main".
  label: string;
  manage: boolean;
}) {
  const queryClient = useQueryClient();
  const [resetOpen, setResetOpen] = useState(false);
  const sessionHref = useSessionPageHref(handle, sessionId);
  const managedHere = (controller.sessions ?? []).some((entry) => entry.sessionId === sessionId);
  const session = useQuery({
    queryKey: ["session", universeId, sessionId],
    queryFn: () =>
      api<SessionView>(
        "GET",
        `/api/ui/universes/${universeId}/sessions/${encodeURIComponent(sessionId)}`,
      ),
  });
  const compaction = useSessionCompaction(universeId, sessionId, session.data);
  const canCompact = useActionPermissions(universeId).can("controlSession");
  const reset = useMutation({
    mutationFn: () =>
      api(
        "POST",
        `/api/ui/universes/${universeId}/bots/${bot.botId}/sessions/${encodeURIComponent(sessionId)}/rotate`,
      ),
    onSuccess: () => {
      setResetOpen(false);
      return queryClient.invalidateQueries({ queryKey: ["bot-state", universeId, bot.botId] });
    },
  });
  return (
    <>
      <SessionActionsMenu
        label="Conversation actions"
        lifecyclePlacement="first"
        sessionId={sessionId}
        metadata={session.data?.metadata}
        onCompact={canCompact && session.data && session.data.lifecycleStatus !== "closed" ? compaction.compact : undefined}
        compactionLabel={compaction.label}
        open={sessionHref ? {
          label: "Open on the Sessions page",
          href: sessionHref,
          icon: <GoToIcon />,
        } : undefined}
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
