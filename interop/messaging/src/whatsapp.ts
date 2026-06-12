import {
  DisconnectReason,
  fetchLatestBaileysVersion,
  makeWASocket,
  normalizeMessageContent,
  useMultiFileAuthState,
  type WAMessage,
  type WASocket,
  type proto,
} from "baileys";
import qrcode from "qrcode-terminal";
import type { WhatsAppBridgeConfig } from "./config.js";
import { stableHash } from "./ids.js";
import type { MessagingBridgeRuntime } from "./runtime.js";
import { extractTriggeredText, splitMessageText } from "./text.js";

export interface RunningWhatsAppBridge {
  stop: () => Promise<void>;
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
        console.log(`whatsapp: connected as account ${config.accountId}`);
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
        await handleWhatsAppMessage(config, runtime, nextSock, allowedJids, message);
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
  };
}

async function handleWhatsAppMessage(
  config: WhatsAppBridgeConfig,
  runtime: MessagingBridgeRuntime,
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

  const rawText = extractWhatsAppText(message);
  if (!rawText) {
    return;
  }

  const isGroup = remoteJid.endsWith("@g.us");
  const text = extractTriggeredText(rawText, {
    mentionNames: config.mentionNames,
    prefixes: config.triggerPrefixes,
    requireTrigger: config.requireTrigger || isGroup,
  });
  if (text === null) {
    return;
  }
  if (!text) {
    await sock.sendMessage(remoteJid, { text: "Send text after the trigger." }, { quoted: message });
    return;
  }

  const participant = message.key.participant ?? "direct";
  const conversationParts = ["whatsapp", config.accountId, remoteJid];
  const conversationKey = `whatsapp:${stableHash(conversationParts)}`;
  const messageKey = `whatsapp:${stableHash([
    config.accountId,
    remoteJid,
    participant,
    messageId,
  ])}`;

  await runtime.handleInboundText(
    {
      accountId: config.accountId,
      conversationKey,
      conversationParts,
      messageId,
      messageKey,
      provider: "whatsapp",
      text,
    },
    {
      sendReply: async (replyText) => {
        for (const chunk of splitMessageText(replyText, 3_500)) {
          await sock.sendMessage(remoteJid, { text: chunk }, { quoted: message });
        }
      },
    },
  );
}

function extractWhatsAppText(message: WAMessage): string | null {
  const content = normalizeMessageContent(message.message ?? undefined);
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
