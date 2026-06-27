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
import type { OutboundMessageView } from "@lightspeed/agent-client";
import { cleanChannelMessageId } from "./channel_id.js";
import { resolveInboundAccess, type WhatsAppBridgeConfig } from "./config.js";
import type { BridgeRouting } from "./telegram.js";
import { stableHash } from "./ids.js";
import { renderWhatsAppText } from "./markdown.js";
import { audioMime, documentByteLimit, documentMime, MAX_AUDIO_BYTES } from "./media.js";
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
  routing: BridgeRouting,
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
  if (config.allowFrom.length === 0) {
    console.warn(
      hasPairableBinding(routing, "whatsapp")
        ? "whatsapp: WHATSAPP_ALLOW_FROM is empty; matching pairable bindings require pairing"
        : "whatsapp: WHATSAPP_ALLOW_FROM is empty; any sender in an allowed chat can chat",
    );
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
        if (sock === nextSock) {
          sock = null;
        }
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
        await handleWhatsAppMessage(
          config,
          runtime,
          policy,
          routing,
          nextSock,
          allowedJids,
          message,
        );
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

function hasPairableBinding(routing: BridgeRouting, channel: "telegram" | "whatsapp"): boolean {
  return routing.bindings.some(
    (binding) =>
      binding.pairing &&
      (binding.match.channel === channel || binding.match.channel === "*"),
  );
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
          const sent = await sock.sendMessage(jid, { text: renderWhatsAppText(chunk) });
          lastId = sent?.key?.id ?? lastId;
        }
        return lastId !== undefined ? { channelMessageId: lastId } : {};
      }
      case "react": {
        const messageId = cleanChannelMessageId(payload.messageId);
        await sock.sendMessage(jid, {
          react: {
            text: payload.emoji,
            key: { remoteJid: jid, id: messageId, fromMe: false },
          },
        });
        return {};
      }
      case "edit": {
        const messageId = cleanChannelMessageId(payload.messageId);
        await sock.sendMessage(jid, {
          text: renderWhatsAppText(payload.text),
          edit: { remoteJid: jid, id: messageId, fromMe: true },
        });
        return { channelMessageId: messageId };
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
  routing: BridgeRouting,
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
  const audio = content?.audioMessage ?? null;
  const audioMimeType = audio
    ? audioMime(null, audio.mimetype ?? (audio.ptt ? "audio/ogg" : null))
    : null;
  const hasAudio =
    audioMimeType !== null && Number(audio?.fileLength ?? 0) <= MAX_AUDIO_BYTES;
  if (!rawText && !hasImage && !hasDocument && !hasAudio) {
    return;
  }

  const ownJid = sock.user?.id ? jidNormalizedUser(sock.user.id) : null;
  const contextInfo = extractContextInfo(content);
  const isGroup = remoteJid.endsWith("@g.us");
  const senderJid = message.key.participant
    ? jidNormalizedUser(message.key.participant)
    : jidNormalizedUser(remoteJid);
  const senderPhone = senderJid.split("@")[0];
  const senderHandles = senderPhone ? [senderJid, senderPhone] : [senderJid];
  const access = resolveInboundAccess(
    {
      channel: "whatsapp",
      handles: senderHandles,
      chatId: remoteJid,
      scope: isGroup ? "group" : "direct",
    },
    config,
    routing.bindings,
  );

  const conversationParts = ["whatsapp", config.accountId, remoteJid];
  const conversationKey = `whatsapp:${stableHash(conversationParts)}`;
  const pairingKey = `whatsapp:${stableHash(["whatsapp", config.accountId, remoteJid])}`;
  const messageKey = `whatsapp:${stableHash([
    config.accountId,
    remoteJid,
    message.key.participant ?? "direct",
    messageId,
  ])}`;
  const fetchMedia = hasImage
    ? () => downloadWhatsAppImage(message, image?.mimetype ?? null)
    : hasDocument
      ? () => downloadWhatsAppDocument(message, documentMimeType, document?.fileName ?? null)
      : hasAudio
        ? () =>
            downloadWhatsAppAudio(
              message,
              audioMimeType,
              audio?.ptt ? "voice.ogg" : "audio",
            )
        : undefined;

  const inbound: NormalizedInbound = {
    provider: "whatsapp",
    accountId: config.accountId,
    chatId: remoteJid,
    conversationKey,
    pairingKey,
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
        : hasAudio
          ? audio?.ptt
            ? "(sent a voice note)"
            : "(sent audio)"
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
    turnAllowed: access.turnAllowed,
    controlAllowed: access.controlAllowed,
    bindingCandidates: access.bindingCandidates,
    bindingId: access.bindingId,
    profile: access.profile,
    profileLabel: access.profileLabel,
    sessionKey: access.sessionKey,
    ...(fetchMedia ? { fetchMedia } : {}),
  };

  await runtime.handleInbound(inbound, policy, {
    sendReply: async (replyText) => {
      const chunks = splitMessageText(replyText, 3_500);
      for (const [index, chunk] of chunks.entries()) {
        const quote = shouldQuoteChunk(config.replyToMode, !isGroup, index);
        await sock.sendMessage(
          remoteJid,
          { text: renderWhatsAppText(chunk) },
          quote ? { quoted: message } : {},
        );
      }
    },
    setTyping: async () => {
      await sock.sendPresenceUpdate("composing", remoteJid);
    },
    clearTyping: async () => {
      await sock.sendPresenceUpdate("paused", remoteJid);
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

async function downloadWhatsAppAudio(
  message: WAMessage,
  mime: string,
  name: string,
): Promise<InboundMedia[]> {
  const buffer = (await downloadMediaMessage(message, "buffer", {})) as Buffer;
  if (buffer.byteLength > MAX_AUDIO_BYTES) {
    return [];
  }
  return [
    {
      base64: buffer.toString("base64"),
      mime,
      name,
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
    content.audioMessage?.contextInfo ??
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
