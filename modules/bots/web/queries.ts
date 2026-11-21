import type { AgentNotification, BotListResponse, BotReadResponse } from "@lightspeed-ai/sdk";
import { universeWatchMeta } from "@lightspeed-ai/platform-web/modules";
import { api } from "@lightspeed-ai/platform-web/modules/client";
import { bots } from "../module";

export function botsApiPath(universeId: string, botId?: string) {
  const collection = `/api/ui/universes/${encodeURIComponent(universeId)}/modules/${bots.id}`;
  return botId === undefined ? collection : `${collection}/${encodeURIComponent(botId)}`;
}

function botChange(event: AgentNotification) {
  if (event.method === "bots/changed") return { id: event.params.bot.botId, row: event.params.bot };
  if (event.method === "bots/removed") return { id: event.params.botId };
}

export function botListQuery(universeId: string) {
  return {
    queryKey: ["bots", universeId],
    queryFn: () => api<BotListResponse>("GET", botsApiPath(universeId)),
    meta: universeWatchMeta<BotListResponse>(universeId, event => {
      const change = botChange(event);
      if (!change) return;
      return {
        key: change.id,
        apply: current => {
          const bots = (current.bots ?? []).filter(bot => bot.botId !== change.id);
          if (change.row) bots.push(change.row);
          return { ...current, bots };
        },
      };
    }),
  };
}

export function botDetailQuery(universeId: string, botId: string) {
  return {
    queryKey: ["bot", universeId, botId],
    queryFn: () => api<BotReadResponse>("GET", botsApiPath(universeId, botId)),
    meta: universeWatchMeta<BotReadResponse>(universeId, event => {
      const change = botChange(event);
      if (!change || change.id !== botId) return;
      const row = change.row;
      if (!row) return { key: botId, refetch: true };
      return { key: botId, apply: current => ({ ...current, bot: row, access: row.access, status: row.status }) };
    }),
  };
}
