import { describe, expect, it } from "vitest";
import type { ChannelSessionStartV1 } from "../src/contracts/channel.js";
import {
  CHANNEL_SEARCH_ATTRIBUTE_NAMES,
  channelSessionSearchAttributes,
} from "../src/contracts/search-attributes.js";

describe("channel workflow search attributes", () => {
  it("indexes every operational routing identity without message data", () => {
    const start = {
      universeId: "universe-1",
      bindingId: "binding-1",
      sessionId: "channel:v1:telegram:session-1",
      initialRoute: { provider: "telegram", accountId: "primary", chatId: "secret-chat" },
    } as ChannelSessionStartV1;

    expect(channelSessionSearchAttributes(start).map(({ key, value }) => [key.name, value])).toEqual([
      ["LsbotUniverseId", "universe-1"],
      ["LsbotChannelProvider", "telegram"],
      ["LsbotChannelAccountId", "primary"],
      ["LsbotChannelBindingId", "binding-1"],
      ["LsbotManagedSessionId", "channel:v1:telegram:session-1"],
    ]);
    expect(CHANNEL_SEARCH_ATTRIBUTE_NAMES).not.toContain("secret-chat");
  });
});
