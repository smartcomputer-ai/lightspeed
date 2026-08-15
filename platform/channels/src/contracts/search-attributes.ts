import {
  SearchAttributeType,
  defineSearchAttributeKey,
  type SearchAttributePair,
} from "@temporalio/common";
import type { ChannelSessionStartV1 } from "./channel.js";

export const CHANNEL_UNIVERSE_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LightspeedUniverseId",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_PROVIDER_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LightspeedChannelProvider",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_ACCOUNT_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LightspeedChannelAccountId",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_BINDING_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LightspeedChannelBindingId",
  SearchAttributeType.KEYWORD,
);
export const CHANNEL_SESSION_SEARCH_ATTRIBUTE = defineSearchAttributeKey(
  "LightspeedManagedSessionId",
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
