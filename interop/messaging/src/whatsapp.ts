import {
  DisconnectReason,
  downloadMediaMessage,
  fetchLatestBaileysVersion,
  jidNormalizedUser,
  makeWASocket,
  normalizeMessageContent,
  useMultiFileAuthState,
  type WAMessage,
  type WASocket,
  type proto,
} from "baileys";
import qrcode from "qrcode-terminal";
import type { OutboundMessageView } from "@forge/agent-client";
import type { WhatsAppBridgeConfig } from "./config.js";
import { stableHash } from "./ids.js";
import { documentByteLimit, documentMime } from "./media.js";
import { DeliveryError, type ChannelDeliverer, type DeliveryResult } from "./outbox.js";
import { shouldQuoteChunk } from "./policy.js";
import type {
  ChannelPolicy,
  InboundMedia,
  MessagingBridgeRuntime,
  NormalizedInbound,
} from "./runtime.js";
import type { BindingState } from "./store.js";
import { splitMessageText } from "./text.js";

const MAX_WHATSAPP_IMAGE_BYTES = 10 * 1024 * 1024;

export interface RunningWhatsAppBridge {
  stop: () => Promise<void>;
  deliverer: ChannelDeliverer;
}

export async function startWhatsAppBridge(
  config: WhatsAppBridgeConfig,
  runtime: MessagingBridgeRuntime,
): Promise<RunningWhatsAppBridge> {
  const { state, saveCreds } = await useMultiFileAuthState(config.authDir);
  const { version } = await fetchLatestBaileysVersion();
  const allowedJids = new Set(config.allowedJids);
  let sock: WASocket | null = null;
  let stopped = false;
  let reconnectTimer: NodeJS.Timeout | null = null;

  if (allowedJids.size === 0) {
    console.warn("whatsapp: WHATSAPP_ALLOWED_JIDS is empty; all chats can trigger the bridge");
  }

  const policy: ChannelPolicy = {
    triggerPrefixes: config.triggerPrefixes,
    mentionNames: config.mentionNames,
    groupActivation: config.groupActivation,
  };

  const connect = () => {
    if (stopped) {
      return;
    }
    const nextSock = makeWASocket({
      auth: state,
      markOnlineOnConnect: false,
      printQRInTerminal: false,
      syncFullHistory: false,
      version,
    });
    sock = nextSock;

    nextSock.ev.on("creds.update", saveCreds);
    nextSock.ev.on("connection.update", (update) => {
      if (update.qr && config.printQr) {
        console.log("whatsapp: scan this QR code with the spare WhatsApp account");
        qrcode.generate(update.qr, { small: true });
      }
      if (update.connection === "open") {
        console.log(
          `whatsapp: connected as account ${config.accountId} (group activation: ${config.groupActivation})`,
        );
      }
      if (update.connection === "close") {
        const statusCode = (update.lastDisconnect?.error as { output?: { statusCode?: number } })
          ?.output?.statusCode;
        if (statusCode === DisconnectReason.loggedOut) {
          console.error("whatsapp: logged out; remove the auth dir and pair again");
          return;
        }
        console.warn(`whatsapp: connection closed${statusCode ? ` (${statusCode})` : ""}`);
        if (!stopped) {
          reconnectTimer = setTimeout(connect, 3_000);
        }
      }
    });

    nextSock.ev.on("messages.upsert", async (upsert) => {
      if (upsert.type !== "notify") {
        return;
      }
      for (const message of upsert.messages) {
        await handleWhatsAppMessage(config, runtime, policy, nextSock, allowedJids, message);
      }
    });
  };

  connect();

  return {
    stop: async () => {
      stopped = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
      }
      sock?.end(undefined);
    },
    deliverer: {
      channel: "whatsapp",
      accountId: config.accountId,
      deliver: async (binding, payload) => {
        const socket = sock;
        if (!socket) {
          throw new DeliveryError("whatsapp socket is not connected", true);
        }
        return deliverWhatsAppPayload(socket, binding, payload);
      },
    },
  };
}

async function deliverWhatsAppPayload(
  sock: WASocket,
  binding: BindingState,
  payload: OutboundMessageView["payload"],
): Promise<DeliveryResult> {
  const jid = binding.chatId;
  try {
    switch (payload.type) {
      case "send": {
        let lastId: string | undefined;
        for (const chunk of splitMessageText(payload.text, 3_500)) {
          const sent = await sock.sendMessage(jid, { text: chunk });
          lastId = sent?.key?.id ?? lastId;
        }
        return lastId !== undefined ? { channelMessageId: lastId } : {};
      }
      case "react": {
        await sock.sendMessage(jid, {
          react: {
            text: payload.emoji,
            key: { remoteJid: jid, id: payload.messageId, fromMe: false },
          },
        });
        return {};
      }
      case "edit": {
        await sock.sendMessage(jid, {
          text: payload.text,
          edit: { remoteJid: jid, id: payload.messageId, fromMe: true },
        });
        return { channelMessageId: payload.messageId };
      }
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new DeliveryError(`whatsapp delivery failed: ${message}`, true);
  }
}

async function handleWhatsAppMessage(
  config: WhatsAppBridgeConfig,
  runtime: MessagingBridgeRuntime,
  policy: ChannelPolicy,
  sock: WASocket,
  allowedJids: ReadonlySet<string>,
  message: WAMessage,
): Promise<void> {
  const remoteJid = message.key.remoteJid;
  const messageId = message.key.id;
  if (!remoteJid || !messageId || message.key.fromMe) {
    return;
  }
  if (remoteJid === "status@broadcast" || remoteJid.endsWith("@broadcast")) {
    return;
  }
  if (allowedJids.size > 0 && !allowedJids.has(remoteJid)) {
    return;
  }

  const content = normalizeMessageContent(message.message ?? undefined);
  const rawText = extractWhatsAppText(content);
  const image = content?.imageMessage ?? null;
  const imageSize = Number(image?.fileLength ?? 0);
  const hasImage = Boolean(image) && imageSize <= MAX_WHATSAPP_IMAGE_BYTES;
  const document = content?.documentMessage ?? null;
  const documentMimeType = document
    ? documentMime(document.fileName, document.mimetype)
    : null;
  const hasDocument =
    documentMimeType !== null &&
    Number(document?.fileLength ?? 0) <= documentByteLimit(documentMimeType);
  if (!rawText && !hasImage && !hasDocument) {
    return;
  }

  const ownJid = sock.user?.id ? jidNormalizedUser(sock.user.id) : null;
  const contextInfo = extractContextInfo(content);
  const isGroup = remoteJid.endsWith("@g.us");
  const senderJid = message.key.participant
    ? jidNormalizedUser(message.key.participant)
    : jidNormalizedUser(remoteJid);
  const allowFrom = new Set(config.allowFrom);

  const conversationParts = ["whatsapp", config.accountId, remoteJid];
  const conversationKey = `whatsapp:${stableHash(conversationParts)}`;
  const messageKey = `whatsapp:${stableHash([
    config.accountId,
    remoteJid,
    message.key.participant ?? "direct",
    messageId,
  ])}`;

  const inbound: NormalizedInbound = {
    provider: "whatsapp",
    accountId: config.accountId,
    chatId: remoteJid,
    conversationKey,
    conversationParts,
    messageId,
    messageKey,
    senderId: senderJid,
    senderName: message.pushName?.trim() || senderJid.split("@")[0] || senderJid,
    timestampMs: Number(message.messageTimestamp ?? 0) * 1000,
    text:
      rawText ||
      (hasDocument
        ? `(sent a file: ${document?.fileName ?? "document"})`
        : "(sent an image)"),
    isDirect: !isGroup,
    chatLabel: isGroup ? remoteJid.split("@")[0] || remoteJid : "dm",
    mentionedBot: Boolean(
      ownJid && contextInfo?.mentionedJid?.some((jid) => jidNormalizedUser(jid) === ownJid),
    ),
    isReplyToBot: Boolean(
      contextInfo?.quotedMessage &&
        ownJid &&
        contextInfo.participant &&
        jidNormalizedUser(contextInfo.participant) === ownJid,
    ),
    isFromSelf: Boolean(message.key.fromMe),
    senderAllowed: allowFrom.size > 0 ? allowFrom.has(senderJid) : !isGroup,
    ...(hasImage
      ? { fetchMedia: () => downloadWhatsAppImage(message, image?.mimetype ?? null) }
      : hasDocument
        ? {
            fetchMedia: () =>
              downloadWhatsAppDocument(message, documentMimeType, document?.fileName ?? null),
          }
        : {}),
  };

  await runtime.handleInbound(inbound, policy, {
    sendReply: async (replyText) => {
      const chunks = splitMessageText(replyText, 3_500);
      for (const [index, chunk] of chunks.entries()) {
        const quote = shouldQuoteChunk(config.replyToMode, !isGroup, index);
        await sock.sendMessage(
          remoteJid,
          { text: chunk },
          quote ? { quoted: message } : {},
        );
      }
    },
    setTyping: async () => {
      await sock.sendPresenceUpdate("composing", remoteJid);
    },
  });
}

async function downloadWhatsAppImage(
  message: WAMessage,
  mimetype: string | null,
): Promise<InboundMedia[]> {
  const buffer = (await downloadMediaMessage(message, "buffer", {})) as Buffer;
  if (buffer.byteLength > MAX_WHATSAPP_IMAGE_BYTES) {
    return [];
  }
  const mime = (mimetype ?? "image/jpeg").split(";")[0]?.trim() || "image/jpeg";
  return [
    {
      base64: buffer.toString("base64"),
      mime,
    },
  ];
}

async function downloadWhatsAppDocument(
  message: WAMessage,
  mime: string,
  fileName: string | null,
): Promise<InboundMedia[]> {
  const buffer = (await downloadMediaMessage(message, "buffer", {})) as Buffer;
  if (buffer.byteLength > documentByteLimit(mime)) {
    return [];
  }
  return [
    {
      base64: buffer.toString("base64"),
      mime,
      ...(fileName !== null ? { name: fileName } : {}),
    },
  ];
}

function extractContextInfo(content: proto.IMessage | undefined): proto.IContextInfo | null {
  if (!content) {
    return null;
  }
  return (
    content.extendedTextMessage?.contextInfo ??
    content.imageMessage?.contextInfo ??
    content.videoMessage?.contextInfo ??
    content.documentMessage?.contextInfo ??
    null
  );
}

function extractWhatsAppText(content: proto.IMessage | undefined): string | null {
  if (!content) {
    return null;
  }

  if (content.conversation) {
    return content.conversation;
  }
  if (content.extendedTextMessage?.text) {
    return content.extendedTextMessage.text;
  }
  if (content.imageMessage?.caption) {
    return content.imageMessage.caption;
  }
  if (content.videoMessage?.caption) {
    return content.videoMessage.caption;
  }
  if (content.documentMessage?.caption) {
    return content.documentMessage.caption;
  }
  if (content.buttonsResponseMessage?.selectedDisplayText) {
    return content.buttonsResponseMessage.selectedDisplayText;
  }
  if (content.listResponseMessage?.title) {
    return content.listResponseMessage.title;
  }
  return null;
}

export type WhatsAppProto = proto.IMessage;
