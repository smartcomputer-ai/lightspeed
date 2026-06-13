import { extractTriggeredText } from "./text.js";

export type ActivationPolicy = "dm" | "mention" | "always" | "silent";

export type GroupActivation = Exclude<ActivationPolicy, "dm">;

export type ControlCommand =
  | { kind: "activation"; mode: GroupActivation }
  | { kind: "status" };

export type Classification =
  | { kind: "drop"; reason: string }
  /// The sender is not on the channel's turn allowlist. `notify` is true when
  /// the bridge should reply with an authorization error (direct chats) and
  /// false when it should drop silently (group members).
  | { kind: "denied"; notify: boolean }
  | { kind: "control"; command: ControlCommand }
  | { kind: "userTurn"; text: string }
  | { kind: "roomEvent"; text: string };

export interface ClassifyInput {
  text: string;
  isDirect: boolean;
  isFromSelf: boolean;
  mentionedBot: boolean;
  isReplyToBot: boolean;
  /// Sender is allowed to run a turn at all (channel turn allowlist).
  turnAllowed: boolean;
  /// Sender is allowed to run control commands (control allowlist).
  controlAllowed: boolean;
}

export interface ClassifyOptions {
  activation: ActivationPolicy;
  triggerPrefixes: readonly string[];
  mentionNames: readonly string[];
  botUsername?: string | null;
}

export function parseControlCommand(text: string): ControlCommand | null {
  const trimmed = text.trim();
  const activation = /^\/activation(?:@[\w_]+)?(?:\s+(\w+))?$/i.exec(trimmed);
  if (activation) {
    const mode = (activation[1] ?? "").toLowerCase();
    if (mode === "mention" || mode === "always" || mode === "silent") {
      return { kind: "activation", mode };
    }
    // `/activation` without a valid mode reports usage via status.
    return { kind: "status" };
  }
  if (/^\/status(?:@[\w_]+)?$/i.test(trimmed)) {
    return { kind: "status" };
  }
  return null;
}

export function classifyInbound(
  message: ClassifyInput,
  options: ClassifyOptions,
): Classification {
  if (message.isFromSelf) {
    return { kind: "drop", reason: "self" };
  }
  const text = message.text.trim();
  if (!text) {
    return { kind: "drop", reason: "empty" };
  }

  const control = parseControlCommand(text);
  if (control && message.controlAllowed) {
    // Control senders may toggle activation/status even if they are not on the
    // turn allowlist.
    return { kind: "control", command: control };
  }

  // Turn gate: senders absent from the channel allowlist cannot chat or seed
  // room context. Direct chats get an explicit error; group members are
  // dropped silently to avoid replying to every outsider message.
  if (!message.turnAllowed) {
    return { kind: "denied", notify: message.isDirect };
  }

  // Explicit trigger prefixes always address the bot, in every activation
  // mode. This is the escape hatch for `silent` chats.
  const prefixed = extractTriggeredText(text, {
    botUsername: options.botUsername ?? null,
    mentionNames: options.mentionNames,
    prefixes: options.triggerPrefixes,
    requireTrigger: true,
  });
  if (prefixed !== null) {
    return prefixed
      ? { kind: "userTurn", text: prefixed }
      : { kind: "drop", reason: "empty-trigger" };
  }

  if (options.activation === "silent") {
    return { kind: "roomEvent", text };
  }

  if (message.isDirect) {
    return { kind: "userTurn", text };
  }

  if (options.activation === "always") {
    return { kind: "userTurn", text };
  }

  if (message.mentionedBot || message.isReplyToBot) {
    return { kind: "userTurn", text: stripBotMention(text, options) };
  }

  return { kind: "roomEvent", text };
}

function stripBotMention(text: string, options: ClassifyOptions): string {
  const names = [
    ...(options.botUsername ? [options.botUsername] : []),
    ...options.mentionNames,
  ]
    .map((name) => name.trim().replace(/^@/, ""))
    .filter(Boolean);
  let result = text;
  for (const name of names) {
    const pattern = new RegExp(`@${escapeRegExp(name)}\\b[:,]?\\s*`, "i");
    result = result.replace(pattern, " ");
  }
  result = result.replace(/\s+/g, " ").trim();
  return result || text.trim();
}

export type ReplyToMode = "off" | "first" | "all";

/// Decides whether an outbound chunk quotes the message it answers. Direct
/// chats never quote (no ambiguity there); groups quote per mode: `first`
/// anchors only the first chunk, `all` anchors every chunk.
export function shouldQuoteChunk(
  mode: ReplyToMode,
  isDirect: boolean,
  chunkIndex: number,
): boolean {
  if (isDirect || mode === "off") {
    return false;
  }
  return mode === "all" || chunkIndex === 0;
}

export interface EnvelopeInput {
  provider: string;
  chatLabel: string;
  isDirect: boolean;
  senderName: string;
  timestampMs: number;
  text: string;
  /// Short channel message id, exposed so the model can later target
  /// reactions/edits/replies (P71 G5).
  messageId?: string;
}

/// Renders the structured envelope used for room events and batched turns:
/// `[telegram:group Engineering #4123] Alice (2026-06-12 12:01Z): the deploy looks stuck`
export function formatEnvelope(input: EnvelopeInput): string {
  const scope = input.isDirect ? "dm" : `group ${input.chatLabel}`;
  const id = input.messageId ? ` #${input.messageId}` : "";
  const time = formatTimestamp(input.timestampMs);
  return `[${input.provider}:${scope}${id}] ${input.senderName} (${time}): ${input.text}`;
}

function formatTimestamp(timestampMs: number): string {
  const date = new Date(timestampMs);
  if (Number.isNaN(date.getTime())) {
    return "unknown time";
  }
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())} ` +
    `${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}Z`
  );
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
