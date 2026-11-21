import { QueryClient } from "@tanstack/react-query";
import type { BotListItem, BotListResponse, BotReadResponse } from "@lightspeed-ai/sdk";
import { expect, it } from "vitest";
import { WatchCache } from "@/lib/watch-cache";
import { botDetailQuery, botListQuery, botsApiPath } from "./queries";

const bot: BotListItem = {
  botId: "support", profileId: "support", revision: 1, eventSeq: 0,
  createdAtMs: 1, updatedAtMs: 1, activity: "idle", pendingCount: 0, triggerCount: 0,
  access: { visibility: "universe" },
};

it("mounts bot requests inside the universe's private module API", () => {
  expect(botsApiPath("org/acme", "my bot")).toBe("/api/ui/universes/org%2Facme/modules/bots/my%20bot");
});

it("applies events to opted-in bot queries and replays them across an overlapping roster read", async () => {
  const client = new QueryClient();
  const cache = new WatchCache(client, "acme");
  const roster = botListQuery("acme");
  const detail = botDetailQuery("acme", bot.botId);
  const other = botListQuery("other");
  try {
    await client.fetchQuery({ ...roster, queryFn: () => ({ bots: [] }) });
    await client.fetchQuery({ ...other, queryFn: () => ({ bots: [] }) });
    await client.fetchQuery({ ...detail, queryFn: () => ({ bot, access: bot.access }) });
    let resolve!: (data: BotListResponse) => void;
    const request = client.fetchQuery({ ...roster, queryFn: () => new Promise<BotListResponse>(yes => { resolve = yes; }) });
    cache.receive({ method: "bots/changed", params: { bot: { ...bot, displayName: "Triage" } } });
    resolve({ bots: [] });
    await request;
    expect(client.getQueryData<BotListResponse>(roster.queryKey)?.bots?.[0]?.displayName).toBe("Triage");
    expect(client.getQueryData<BotReadResponse>(detail.queryKey)?.bot.displayName).toBe("Triage");
    expect(client.getQueryData(other.queryKey)).toEqual({ bots: [] });

    cache.receive({ method: "bots/removed", params: { botId: bot.botId } });
    expect(client.getQueryData(roster.queryKey)).toEqual({ bots: [] });
    expect(client.getQueryState(detail.queryKey)?.isInvalidated).toBe(true);
  } finally {
    cache.dispose();
    client.clear();
  }
});
