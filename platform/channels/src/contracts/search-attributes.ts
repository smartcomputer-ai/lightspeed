import {
  SearchAttributeType,
  defineSearchAttributeKey,
  type SearchAttributePair,
} from "@temporalio/common";
import type { ChannelSessionStartV1 } from "./channel.js";

export const CHANNEL_UNIVERSE_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LsbotUniverseId",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_PROVIDER_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LsbotChannelProvider",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_ACCOUNT_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LsbotChannelAccountId",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_BINDING_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LsbotChannelBindingId",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_SESSION_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LsbotManagedSessionId",
  SearchAttributeType.KEYWORD,
);

export const CHANNEL_SEARCH_ATTRIBUTE_NAMES = [
  CHANNEL_UNIVERSE_SEARCH_ATTRIBUTE.name,
  CHANNEL_PROVIDER_SEARCH_ATTRIBUTE.name,
  CHANNEL_ACCOUNT_SEARCH_ATTRIBUTE.name,
  CHANNEL_BINDING_SEARCH_ATTRIBUTE.name,
  CHANNEL_SESSION_SEARCH_ATTRIBUTE.name,
] as const;

export function channelSessionSearchAttributes(
  start: ChannelSessionStartV1,
): SearchAttributePair[] {
  return [
    { key: CHANNEL_UNIVERSE_SEARCH_ATTRIBUTE, value: start.universeId },
    { key: CHANNEL_PROVIDER_SEARCH_ATTRIBUTE, value: start.initialRoute.provider },
    { key: CHANNEL_ACCOUNT_SEARCH_ATTRIBUTE, value: start.initialRoute.accountId },
    { key: CHANNEL_BINDING_SEARCH_ATTRIBUTE, value: start.bindingId },
    { key: CHANNEL_SESSION_SEARCH_ATTRIBUTE, value: start.sessionId },
  ];
}
