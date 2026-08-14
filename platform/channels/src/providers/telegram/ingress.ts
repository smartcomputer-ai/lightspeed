import type { NormalizedInboundV1 } from "../../contracts/channel.js";
import type { ChannelInboundMediaV1 } from "../../contracts/media.js";
import {
  MAX_AUDIO_BYTES,
  MAX_IMAGE_BYTES,
  audioMime,
  documentMime,
  mediaByteLimit,
} from "../../media/validation.js";

export interface TelegramFileDescriptor {
  fileId: string;
  fileSize?: number;
}

export interface TelegramPhotoDescriptor extends TelegramFileDescriptor {
  width: number;
  height: number;
}

export interface TelegramNamedFileDescriptor extends TelegramFileDescriptor {
  fileName?: string;
  mimeType?: string;
}

export interface TelegramInboundMessage {
  messageId: number;
  chatId: number | string;
  chatType: "private" | "group" | "supergroup" | "channel";
  threadId?: number;
  senderId: number | string;
  senderUsername?: string;
  senderFirstName?: string;
  senderLastName?: string;
  timestampMs: number;
  text?: string;
  caption?: string;
  entities?: Array<{ type: string; offset: number; length: number }>;
  captionEntities?: Array<{ type: string; offset: number; length: number }>;
  replyToSenderId?: number | string;
  photos?: TelegramPhotoDescriptor[];
  document?: TelegramNamedFileDescriptor;
  voice?: TelegramFileDescriptor & { mimeType?: string };
  audio?: TelegramNamedFileDescriptor;
}

export interface TelegramIngressContext {
  accountId: string;
  botId: number | string;
  botUsername?: string;
}

export function normalizeTelegramInbound(
  context: TelegramIngressContext,
  message: TelegramInboundMessage,
): NormalizedInboundV1 | null {
  if (String(message.senderId) === String(context.botId)) {
    return null;
  }
  const media = normalizeTelegramMedia(message);
  const suppliedText = (message.text ?? message.caption ?? "").trim();
  if (suppliedText.length === 0 && media.length === 0) {
    return null;
  }
  const text = suppliedText || mediaPlaceholder(media[0]);
  const entities = message.text === undefined ? message.captionEntities : message.entities;
  const mentionedBot = mentionsUsername(text, entities ?? [], context.botUsername);
  const senderName = [message.senderFirstName, message.senderLastName]
    .filter((part): part is string => part !== undefined && part.length > 0)
    .join(" ") || message.senderUsername || String(message.senderId);
  return {
    version: 1,
    messageId: String(message.messageId),
    route: {
      provider: "telegram",
      accountId: context.accountId,
      chatId: String(message.chatId),
      ...(message.threadId === undefined ? {} : { threadId: String(message.threadId) }),
    },
    senderId: String(message.senderId),
    senderName,
    timestampMs: message.timestampMs,
    text,
    ...(media.length === 0 ? {} : { media }),
    isDirect: message.chatType === "private",
    mentionedBot,
    isReplyToBot:
      message.replyToSenderId !== undefined &&
      String(message.replyToSenderId) === String(context.botId),
  };
}

export function normalizeTelegramMedia(message: TelegramInboundMessage): ChannelInboundMediaV1[] {
  const photo = [...(message.photos ?? [])]
    .filter((candidate) => candidate.fileSize === undefined || candidate.fileSize <= MAX_IMAGE_BYTES)
    .sort((left, right) => right.width * right.height - left.width * left.height)[0];
  if (photo !== undefined) {
    return [telegramMedia(photo, "image", "image/jpeg", "photo.jpg")];
  }

  const document = message.document;
  if (document !== undefined) {
    const mime = documentMime(document.fileName, document.mimeType);
    if (
      mime !== null &&
      (document.fileSize === undefined || document.fileSize <= mediaByteLimit("document", mime))
    ) {
      return [telegramMedia(document, "document", mime, document.fileName ?? "document")];
    }
  }

  const voice = message.voice;
  if (voice !== undefined) {
    const mime = audioMime("voice.ogg", voice.mimeType ?? "audio/ogg");
    if (mime !== null && (voice.fileSize === undefined || voice.fileSize <= MAX_AUDIO_BYTES)) {
      return [telegramMedia(voice, "audio", mime, "voice.ogg")];
    }
  }

  const audio = message.audio;
  if (audio !== undefined) {
    const mime = audioMime(audio.fileName, audio.mimeType);
    if (mime !== null && (audio.fileSize === undefined || audio.fileSize <= MAX_AUDIO_BYTES)) {
      return [telegramMedia(audio, "audio", mime, audio.fileName ?? "audio")];
    }
  }
  return [];
}

function telegramMedia(
  file: TelegramFileDescriptor,
  kind: ChannelInboundMediaV1["kind"],
  mime: string,
  name: string,
): ChannelInboundMediaV1 {
  return {
    version: 1,
    provider: "telegram",
    fileId: file.fileId,
    kind,
    mime,
    name,
    ...(file.fileSize === undefined ? {} : { byteSize: file.fileSize }),
  };
}

function mediaPlaceholder(media: ChannelInboundMediaV1 | undefined): string {
  if (media === undefined) return "";
  if (media.kind === "image") return "(sent an image)";
  if (media.kind === "audio") {
    return media.name === "voice.ogg" ? "(sent a voice note)" : `(sent audio: ${media.name ?? "audio"})`;
  }
  return `(sent a file: ${media.name ?? "document"})`;
}

function mentionsUsername(
  text: string,
  entities: Array<{ type: string; offset: number; length: number }>,
  botUsername: string | undefined,
): boolean {
  if (botUsername === undefined) {
    return false;
  }
  const expected = `@${botUsername.replace(/^@/, "")}`.toLowerCase();
  return entities.some(
    (entity) =>
      entity.type === "mention" &&
      text.slice(entity.offset, entity.offset + entity.length).toLowerCase() === expected,
  );
}
