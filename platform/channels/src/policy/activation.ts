import type { NormalizedInboundV1 } from "../contracts/channel.js";

export type GroupActivation = "mention" | "always" | "silent";
export type ActivationPolicy = "dm" | GroupActivation;

export interface ChannelBatchingSettings {
  debounceMs: number;
  maxWaitMs: number;
  maxMessages: number;
}

export interface ChannelActivationSettings {
  mode: ActivationPolicy;
  triggerPrefixes: string[];
  mentionNames: string[];
  batching: ChannelBatchingSettings;
  roomContextLimit: number;
}

export interface StoredChannelActivationSettings {
  group?: GroupActivation;
  triggerPrefixes?: string[];
  mentionNames?: string[];
  batching?: Partial<ChannelBatchingSettings>;
  roomContextLimit?: number;
}

export type InboundClassification =
  | { kind: "drop"; reason: "empty" | "empty-trigger" }
  | { kind: "userTurn"; text: string }
  | { kind: "roomEvent"; text: string };

export const DEFAULT_CHANNEL_BATCHING: Readonly<ChannelBatchingSettings> = {
  debounceMs: 400,
  maxWaitMs: 1_500,
  maxMessages: 8,
};

const DEFAULT_TRIGGER_PREFIXES = ["/ask", "/lightspeed"];

export function resolveActivationSettings(
  scope: "direct" | "group",
  value: unknown,
): ChannelActivationSettings {
  const stored = parseStoredActivationSettings(value);
  return {
    mode: scope === "direct" ? "dm" : (stored.group ?? "mention"),
    triggerPrefixes: uniqueNonEmpty(stored.triggerPrefixes ?? DEFAULT_TRIGGER_PREFIXES),
    mentionNames: uniqueNonEmpty(stored.mentionNames ?? []),
    batching: {
      debounceMs: boundedInteger(
        stored.batching?.debounceMs,
        DEFAULT_CHANNEL_BATCHING.debounceMs,
        0,
        5_000,
      ),
      maxWaitMs: boundedInteger(
        stored.batching?.maxWaitMs,
        DEFAULT_CHANNEL_BATCHING.maxWaitMs,
        1,
        30_000,
      ),
      maxMessages: boundedInteger(
        stored.batching?.maxMessages,
        DEFAULT_CHANNEL_BATCHING.maxMessages,
        1,
        32,
      ),
    },
    roomContextLimit: boundedInteger(stored.roomContextLimit, 200, 0, 1_000),
  };
}

export function classifyInbound(
  inbound: NormalizedInboundV1,
  settings: ChannelActivationSettings,
): InboundClassification {
  const text = inbound.text.trim();
  if (text.length === 0) {
    return { kind: "drop", reason: "empty" };
  }

  const triggered = extractTriggeredText(text, settings);
  if (triggered !== null) {
    return triggered.length === 0
      ? { kind: "drop", reason: "empty-trigger" }
      : { kind: "userTurn", text: triggered };
  }
  if (settings.mode === "silent") {
    return { kind: "roomEvent", text };
  }
  if (inbound.isDirect || settings.mode === "dm" || settings.mode === "always") {
    return { kind: "userTurn", text };
  }
  if (inbound.mentionedBot || inbound.isReplyToBot) {
    return { kind: "userTurn", text: stripNamedMention(text, settings.mentionNames) };
  }
  return { kind: "roomEvent", text };
}

export function formatInboundEnvelope(
  inbound: NormalizedInboundV1,
  text = inbound.text,
): string {
  const scope = inbound.isDirect ? "dm" : `group ${inbound.route.chatId}`;
  const thread = inbound.route.threadId === undefined ? "" : ` thread ${inbound.route.threadId}`;
  return `[${inbound.route.provider}:${scope}${thread} #${inbound.messageId}] ${inbound.senderName} (${formatTimestamp(inbound.timestampMs)}): ${text}`;
}

function parseStoredActivationSettings(value: unknown): StoredChannelActivationSettings {
  if (value === null || value === undefined) {
    return {};
  }
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("binding activation must be an object");
  }
  const input = value as Record<string, unknown>;
  if (
    input.group !== undefined &&
    input.group !== "mention" &&
    input.group !== "always" &&
    input.group !== "silent"
  ) {
    throw new TypeError("binding activation.group must be mention, always, or silent");
  }
  for (const key of ["triggerPrefixes", "mentionNames"] as const) {
    if (
      input[key] !== undefined &&
      (!Array.isArray(input[key]) || input[key].some((entry) => typeof entry !== "string"))
    ) {
      throw new TypeError(`binding activation.${key} must be a string array`);
    }
  }
  if (input.roomContextLimit !== undefined && !Number.isSafeInteger(input.roomContextLimit)) {
    throw new TypeError("binding activation.roomContextLimit must be an integer");
  }
  if (
    input.batching !== undefined &&
    (typeof input.batching !== "object" || input.batching === null || Array.isArray(input.batching))
  ) {
    throw new TypeError("binding activation.batching must be an object");
  }
  const batching = input.batching as Record<string, unknown> | undefined;
  for (const key of ["debounceMs", "maxWaitMs", "maxMessages"] as const) {
    if (batching?.[key] !== undefined && !Number.isSafeInteger(batching[key])) {
      throw new TypeError(`binding activation.batching.${key} must be an integer`);
    }
  }
  return input as StoredChannelActivationSettings;
}

function extractTriggeredText(text: string, settings: ChannelActivationSettings): string | null {
  for (const prefix of settings.triggerPrefixes) {
    const slashMatch = new RegExp(`^${escapeRegExp(prefix)}(?:@[\\w_]+)?(?:\\s+|$)`, "i");
    if (slashMatch.test(text)) {
      return text.replace(slashMatch, "").trim();
    }
  }
  for (const name of settings.mentionNames) {
    const mention = name.replace(/^@/, "");
    const pattern = new RegExp(`^@${escapeRegExp(mention)}(?:[:,]?\\s+|$)`, "i");
    if (pattern.test(text)) {
      return text.replace(pattern, "").trim();
    }
  }
  return null;
}

function stripNamedMention(text: string, names: readonly string[]): string {
  let result = text;
  for (const value of names) {
    const name = value.replace(/^@/, "");
    result = result.replace(new RegExp(`@${escapeRegExp(name)}\\b[:,]?\\s*`, "i"), " ");
  }
  return result.replace(/\s+/g, " ").trim() || text;
}

function uniqueNonEmpty(values: readonly string[]): string[] {
  return [...new Set(values.map((value) => value.trim()).filter(Boolean))];
}

function boundedInteger(value: number | undefined, fallback: number, min: number, max: number): number {
  if (value === undefined) {
    return fallback;
  }
  if (value < min || value > max) {
    throw new RangeError(`activation batching value must be between ${min} and ${max}`);
  }
  return value;
}

function formatTimestamp(timestampMs: number): string {
  const date = new Date(timestampMs);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())} ${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}Z`;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
