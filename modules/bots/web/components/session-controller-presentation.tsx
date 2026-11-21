import { botListQuery } from "../queries";
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { botLabel } from "../model";
import { BotIcon } from "@lightspeed-ai/platform-web/modules/ui/icons";
import { botColor } from "./face";
import { useFeature, useActionPermissions, type ControllerPresentationProps } from "@lightspeed-ai/platform-web/modules";
import { controlledSessionBotId, controlledSessionOwnerLabel } from "../lifecycle-controller";


/** Adds bot attribution to managed sessions wherever the shared session view is rendered. */
export function BotSessionControllerPresentation({ universeId, handle, session, children }: ControllerPresentationProps) {
  const botId = controlledSessionBotId(session?.integration, session?.metadata);
  const botsEnabled = useFeature("bots");
  const mayRead = useActionPermissions(universeId).can("read");
  const showBot = botId !== null && botsEnabled && mayRead;
  const directory = useQuery({
    ...botListQuery(universeId),
    enabled: showBot,
    staleTime: 60_000,
    retry: false,
  });
  const names = useMemo(() => new Map(
    (directory.data?.bots ?? []).map(bot => [bot.botId, botLabel(bot)]),
  ), [directory.data]);

  if (!botId || !session) return children({ label: controlledSessionOwnerLabel(session?.integration) });
  const href = showBot && names.has(botId)
    ? `/u/${encodeURIComponent(handle)}/bots/${encodeURIComponent(botId)}/chat/${encodeURIComponent(session.id)}`
    : undefined;
  return children({
    label: "a bot",
    attribution: { icon: BotIcon, iconColor: botColor(botId), label: (showBot ? names.get(botId) : undefined) ?? botId, href },
    open: href ? { label: "Open in bot", href, icon: <BotIcon /> } : undefined,
    participantName: showBot ? (id) => names.get(id) : undefined,
  });
}
