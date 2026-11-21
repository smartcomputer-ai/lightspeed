import { useEffect, useRef, type ReactNode } from "react";
import { LoadingIcon } from "@lightspeed-ai/platform-web/modules/ui/icons";
import { useLocation, useNavigate } from "react-router-dom";
import { api } from "@lightspeed-ai/platform-web/modules/client";
import { botLabel } from "../model";
import type { BotControllerSnapshot, BotStateView, BotView } from "@lightspeed-ai/sdk";
import { useActionPermissions } from "@lightspeed-ai/platform-web/modules";
import { SessionDetail, sessionDraftKey } from "@lightspeed-ai/platform-web/modules/conversations";


/** The first thing a freshly created bot hears; its answer is the smoke test. */
export const INTRODUCTION_PROMPT =
  "You were just created. Introduce yourself in two sentences and confirm your setup: the triggers that wake you, the tools and environment you can use. Ask about anything that is unclear or missing.";

const introduced = new Set<string>();

/**
 * One of the bot's conversations, full width: the transcript and composer.
 * The composer sends a plain message to that session — an ordinary client
 * run queued behind whatever the bot is doing. Which conversation is shown
 * is the page's business: `header` names it and holds its menu.
 */
export function BotChat({
  universeId,
  handle,
  bot,
  state,
  stateError,
  sessionId,
  header,
}: {
  universeId: string;
  handle: string;
  bot: BotView;
  state?: BotStateView;
  stateError?: string;
  sessionId: string | undefined;
  /// The conversation's header: what is open (a switcher) and its ⋯ menu.
  header?: { title: ReactNode; actions?: ReactNode };
}) {
  const base = `/u/${handle}/bots/${bot.botId}`;
  const controller = state?.controller ?? undefined;
  const main = controller?.mainSessionId;
  const selected = sessionId ?? main;
  const isMain = selected !== undefined && selected === main;
  const sessionHref = (id: string) => (id === main ? base : `${base}/chat/${encodeURIComponent(id)}`);
  const ready = controller?.setupStatus === "ready";
  const permissions = useActionPermissions(universeId);
  const controlMain = !!main && permissions.can("controlSession");

  useIntroduction(universeId, bot, controller, isMain, controlMain);

  if (selected === undefined) {
    return (
      <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
        {stateError ? `The bot's controller is unavailable: ${stateError}` : "Starting the bot…"}
      </div>
    );
  }
  if (isMain && !ready) {
    // Not ready is either "still starting" or "could not start": say which,
    // and say why. A degraded controller keeps retrying on its own.
    const degraded = controller?.setupStatus === "degraded" || Boolean(controller?.lastError);
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center text-sm text-muted-foreground">
        {bot.closedAtMs != null || degraded ? null : <LoadingIcon className="size-4 animate-spin" />}
        {bot.closedAtMs != null ? (
          "This bot is closed; its conversations were released."
        ) : degraded ? (
          <>
            <span className="font-medium text-destructive">The main conversation could not be set up.</span>
            <span className="max-w-xl wrap-anywhere">{controller?.lastError ?? "The controller reported a problem."}</span>
            <span>The bot keeps trying on its own, and its other conversations keep working. If the cause is in Bot settings, fix it and save to try again now.</span>
          </>
        ) : (
          `Starting ${botLabel(bot)}'s main conversation…`
        )}
      </div>
    );
  }
  return (
    <SessionDetail
      key={sessionDraftKey(universeId, selected)}
      universeId={universeId}
      handle={handle}
      sessionId={selected}
      backTo={base}
      embedded
      {...(header ? { embeddedHeader: header } : {})}
      sessionHref={sessionHref}
    />
  );
}

/// Right after creation the wizard lands here with `introduce` in the
/// router state; once the main session is ready, say hello exactly once.
function useIntroduction(
  universeId: string,
  bot: BotView,
  controller: BotControllerSnapshot | undefined,
  isMain: boolean,
  controlMain: boolean,
) {
  const location = useLocation();
  const navigate = useNavigate();
  const requested = (location.state as { introduce?: boolean } | null)?.introduce === true;
  const sent = useRef(false);
  const ready = controller?.setupStatus === "ready";
  const mainSessionId = controller?.mainSessionId;
  const introductionKey = `${universeId}:${bot.botId}`;
  useEffect(() => {
    if (!controlMain || !requested || !isMain || !ready || !mainSessionId || sent.current || introduced.has(introductionKey)) return;
    sent.current = true;
    introduced.add(introductionKey);
    void api("POST", `/api/ui/universes/${universeId}/sessions/${mainSessionId}/messages`, {
      text: INTRODUCTION_PROMPT,
      submissionId: crypto.randomUUID(),
    }).catch(() => undefined);
    navigate(location.pathname, { replace: true, state: null });
  }, [controlMain, requested, isMain, ready, mainSessionId, introductionKey, universeId, navigate, location.pathname]);
}
