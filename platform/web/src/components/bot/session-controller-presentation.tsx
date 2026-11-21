import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, botLabel, type BotListResponse } from "@/api";
import { BotIcon } from "@/icons";
import { botColor } from "./face";
import { useFeature } from "@/lib/universes";
import { useActionPermissions } from "@/lib/permissions";
import { controlledSessionBotId, controlledSessionOwnerLabel } from "@/lib/sessions/lifecycle-controller";
import type { ControllerPresentationProps } from "@session-ui/controller-presentation";

/** Adds bot attribution to managed sessions wherever the shared session view is rendered. */
export function BotSessionControllerPresentation({ universeId, handle, session, children }: ControllerPresentationProps) {
  const botId = controlledSessionBotId(session?.integration, session?.metadata);
  const botsEnabled = useFeature("bots");
  const mayRead = useActionPermissions(universeId).can("read");
  const showBot = botId !== null && botsEnabled && mayRead;
  const directory = useQuery({
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", `/api/ui/universes/${universeId}/bots`),
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
