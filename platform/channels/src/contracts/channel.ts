import {
  MAX_CHANNEL_MEDIA_PER_MESSAGE,
  parseChannelInboundMediaV1,
  type ChannelInboundMediaV1,
} from "./media.js";

export const CHANNEL_SESSION_WORKFLOW = "channelSessionWorkflowV1";
export const CHANNELS_WORKFLOW_TASK_QUEUE = "lightspeed-channels-workflows-v1";
export const CHANNELS_ACTIVITY_TASK_QUEUE = "lightspeed-channels-activities-v1";
export const CHANNEL_INBOUND_SIGNAL = "channel_inbound_v1";
export const CHANNEL_STATE_QUERY = "channel_state_v1";

export type ChannelProvider = "telegram" | "whatsapp";

export interface ChannelRoute {
  provider: ChannelProvider;
  accountId: string;
  chatId: string;
  threadId?: string;
}

/** Secret-free durable input for one channel-owned managed session. */
export interface ChannelSessionStartV1 {
  version: 1;
  universeId: string;
  bindingId: string;
  displayName?: string;
  profileId?: string;
  sessionId: string;
  sessionKey: string;
  scope: "direct" | "group";
  activation: import("../policy/activation.js").ChannelActivationSettings;
  access: import("../policy/access.js").ChannelAccessSettings;
  initialRoute: ChannelRoute;
  deliveryTaskQueue: string;
}

export interface NormalizedInboundV1 {
  version: 1;
  messageId: string;
  route: ChannelRoute;
  senderId: string;
  senderName: string;
  timestampMs: number;
  text: string;
  media?: ChannelInboundMediaV1[];
  isDirect: boolean;
  mentionedBot: boolean;
  isReplyToBot: boolean;
}

export interface AdmittedChannelInboundV1 extends NormalizedInboundV1 {
  authorization: import("../policy/access.js").ChannelAuthorization;
}

export function parseNormalizedInboundV1(value: unknown): NormalizedInboundV1 {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("channel inbound payload must be an object");
  }
  const input = value as Record<string, unknown>;
  if (input.version !== 1) {
    throw new TypeError("channel inbound payload version must be 1");
  }
  const route = parseChannelRoute(input.route);
  for (const key of ["messageId", "senderId", "senderName"] as const) {
    requireString(input[key], key);
  }
  if (typeof input.text !== "string") {
    throw new TypeError("text must be a string");
  }
  if (!Number.isSafeInteger(input.timestampMs) || (input.timestampMs as number) < 0) {
    throw new TypeError("timestampMs must be a non-negative safe integer");
  }
  for (const key of ["isDirect", "mentionedBot", "isReplyToBot"] as const) {
    if (typeof input[key] !== "boolean") {
      throw new TypeError(`${key} must be a boolean`);
    }
  }
  let media: ChannelInboundMediaV1[] | undefined;
  if (input.media !== undefined) {
    if (!Array.isArray(input.media)) {
      throw new TypeError("channel inbound media must be an array");
    }
    if (input.media.length === 0 || input.media.length > MAX_CHANNEL_MEDIA_PER_MESSAGE) {
      throw new TypeError(`channel inbound media must contain 1-${MAX_CHANNEL_MEDIA_PER_MESSAGE} items`);
    }
    media = input.media.map(parseChannelInboundMediaV1);
    if (media.some((entry) => entry.provider !== route.provider)) {
      throw new TypeError("channel inbound media provider must match the route provider");
    }
  }
  if (input.text.length === 0 && media === undefined) {
    throw new TypeError("channel inbound payload must contain text or media");
  }
  return { ...input, route, ...(media === undefined ? {} : { media }) } as NormalizedInboundV1;
}

export function parseAdmittedChannelInboundV1(value: unknown): AdmittedChannelInboundV1 {
  const inbound = parseNormalizedInboundV1(value);
  const raw = value as Record<string, unknown>;
  if (
    typeof raw.authorization !== "object" ||
    raw.authorization === null ||
    Array.isArray(raw.authorization)
  ) {
    throw new TypeError("channel inbound authorization must be an object");
  }
  const authorization = raw.authorization as Record<string, unknown>;
  if (
    typeof authorization.turnAllowed !== "boolean" ||
    typeof authorization.controlAllowed !== "boolean"
  ) {
    throw new TypeError("channel inbound authorization flags must be booleans");
  }
  if (
    authorization.memberRole !== null &&
    authorization.memberRole !== "member" &&
    authorization.memberRole !== "admin" &&
    authorization.memberRole !== "owner"
  ) {
    throw new TypeError("channel inbound memberRole is invalid");
  }
  return {
    ...inbound,
    authorization: authorization as unknown as AdmittedChannelInboundV1["authorization"],
  };
}

function parseChannelRoute(value: unknown): ChannelRoute {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("channel route must be an object");
  }
  const route = value as Record<string, unknown>;
  if (route.provider !== "telegram" && route.provider !== "whatsapp") {
    throw new TypeError("channel route provider must be telegram or whatsapp");
  }
  requireString(route.accountId, "route.accountId");
  requireString(route.chatId, "route.chatId");
  if (route.threadId !== undefined) {
    requireString(route.threadId, "route.threadId");
  }
  return route as unknown as ChannelRoute;
}

function requireString(value: unknown, name: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
}
