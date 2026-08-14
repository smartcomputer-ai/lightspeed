import type { NormalizedInboundV1 } from "../../contracts/channel.js";
import type { ChannelInboundMediaV1 } from "../../contracts/media.js";

export interface WhatsAppInboundMessage {
  messageId: string;
  remoteJid: string;
  participantJid?: string;
  pushName?: string;
  timestampMs: number;
  text: string;
  mentionedJids?: string[];
  quotedParticipantJid?: string;
  fromMe?: boolean;
  media?: ChannelInboundMediaV1[];
}

export interface WhatsAppIngressContext {
  accountId: string;
  ownJids: ReadonlySet<string>;
}

export function normalizeWhatsAppInbound(
  context: WhatsAppIngressContext,
  message: WhatsAppInboundMessage,
): NormalizedInboundV1 | null {
  const suppliedText = message.text.trim();
  const media = message.media ?? [];
  if (message.fromMe === true || (suppliedText.length === 0 && media.length === 0)) {
    return null;
  }
  const mentionedBot = (message.mentionedJids ?? []).some((jid) =>
    matchesAnyJid(jid, context.ownJids),
  );
  const text =
    (mentionedBot
      ? stripOwnMentions(suppliedText, message.mentionedJids ?? [], context.ownJids)
      : suppliedText) || mediaPlaceholder(media[0]);
  const isDirect = !message.remoteJid.endsWith("@g.us");
  const senderId = message.participantJid ?? message.remoteJid;
  return {
    version: 1,
    messageId: message.messageId,
    route: {
      provider: "whatsapp",
      accountId: context.accountId,
      chatId: message.remoteJid,
    },
    senderId,
    senderName: message.pushName?.trim() || senderId.split("@")[0] || senderId,
    timestampMs: message.timestampMs,
    text,
    ...(media.length === 0 ? {} : { media }),
    isDirect,
    mentionedBot,
    isReplyToBot:
      message.quotedParticipantJid !== undefined &&
      matchesAnyJid(message.quotedParticipantJid, context.ownJids),
  };
}

export function stripOwnMentions(
  text: string,
  mentionedJids: readonly string[],
  ownJids: ReadonlySet<string>,
): string {
  let result = text;
  for (const mentionedJid of mentionedJids) {
    if (!matchesAnyJid(mentionedJid, ownJids)) continue;
    const user = mentionedJid.split("@")[0]?.split(":")[0];
    if (!user) continue;
    result = result.replace(new RegExp(`@${escapeRegExp(user)}\\b[:,]?\\s*`, "g"), " ");
  }
  const stripped = result.replace(/\s+/g, " ").trim();
  return stripped || text.trim();
}

function mediaPlaceholder(media: ChannelInboundMediaV1 | undefined): string {
  if (media === undefined) return "";
  if (media.kind === "image") return "(sent an image)";
  if (media.kind === "audio") {
    return media.name === "voice.ogg" ? "(sent a voice note)" : `(sent audio: ${media.name ?? "audio"})`;
  }
  return `(sent a file: ${media.name ?? "document"})`;
}

export function matchesAnyJid(candidate: string, ownJids: ReadonlySet<string>): boolean {
  const normalized = normalizeJid(candidate);
  for (const own of ownJids) {
    if (normalizeJid(own) === normalized) {
      return true;
    }
  }
  return false;
}

function normalizeJid(value: string): string {
  const [user = value, server = ""] = value.toLowerCase().split("@");
  return `${user.split(":")[0]}@${server}`;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
